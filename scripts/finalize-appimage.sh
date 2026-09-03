#!/usr/bin/env bash
#
# Post-process a built AppImage: make it start on modern Mesa, and make it
# adoptable and updatable by an AppImage manager.
#
# Tauri hands off to linuxdeploy, which offers no hook between building the
# AppDir and packing it, so both jobs are done by unpacking the finished image
# and repacking it. That is also why the update information is embedded here
# rather than passed to the bundler.
#
# ---------------------------------------------------------------------------
# 1. The bundled Wayland client
# ---------------------------------------------------------------------------
#
# linuxdeploy-plugin-gtk bundles libwayland-client.so.0 as a dependency of
# GTK, and `AppRun.wrapped` puts the bundled lib directory ahead of the host's
# on the loader path. The host's Mesa then resolves its Wayland EGL platform
# against *our* copy instead of the system one it was built against, and when
# ours is older than Mesa needs, EGL initialisation fails outright:
#
#     Could not create default EGL display: EGL_BAD_PARAMETER. Aborting...
#
# WebKitGTK prints that from its own C code and kills the webview, so the
# window comes up blank. Measured on CachyOS with wayland 1.26 / Mesa 26.2.1
# against an AppImage built on Ubuntu 22.04 (wayland 1.20): eleven symbols
# Mesa can ask for are missing from the bundled copy, `wl_proxy_get_display`,
# `wl_proxy_get_queue`, `wl_display_create_queue_with_name` and
# `wl_fixes_interface` among them. Removing this one file from the AppDir
# fixes it; removing libwayland-egl or libepoxy does not.
#
# **Building on a newer runner would not fix this.** libwayland-client is a
# host-coupled library in the same way libGL, libEGL and libdrm are: it has to
# match the compositor and Mesa actually running, not the ones the build
# machine had. Any pinned version is wrong on a system newer than the builder,
# so the only correct version is the host's. That is what AppImage excludelists
# are for; this library simply is not on linuxdeploy's.
#
# Bundling a *newer* wayland instead would not fix this either, only defer it.
# The version floor is set by the host's Mesa: `libEGL_mesa.so.0` — the driver
# libglvnd's `libEGL.so.1` dlopens — carries a hard DT_NEEDED on
# libwayland-client.so.0. If those symbols will not resolve, the driver never
# loads, glvnd is left with none, and `eglGetDisplay` reports no display. That
# is why forcing GDK_BACKEND=x11 does not dodge it, and why the symptom is a
# bad-parameter error rather than a link failure. Their Mesa updates independently of our releases, so any version
# we pick is one wayland release away from being too old again.
#
# So the copy is not deleted, it is demoted. It moves to a directory that is
# not on the loader path, and a hook puts that directory on the path only when
# the host has no libwayland-client of its own. Hosts with one — which is
# every host with a graphical desktop, since Mesa itself depends on it — get
# theirs, matching their Mesa. A host without one still gets a working app.
#
# The ordering works because `AppRun.wrapped` appends the inherited
# LD_LIBRARY_PATH after its own AppDir entries, so anything the hook exports
# lands last: a fallback, never an override.
#
# ---------------------------------------------------------------------------
# 2. Metadata an AppImage manager needs
# ---------------------------------------------------------------------------
#
# Two things, neither of which the bundler produces:
#
#   * AppStream metadata, so a manager can show what the app is rather than a
#     bare filename. appimagetool warns about its absence on every build.
#   * Update information embedded in the image — the string that tells a
#     manager where to look for a newer build. Without it the app can be
#     adopted but never updated, which is the whole point.
#
# The update URL is a **fixed** tag on the GitHub mirror, which is where
# updates are pulled from, rather than `releases/latest`. `latest` follows
# whatever release is newest, and the Gitea-to-GitHub backfill creates one
# GitHub release per Gitea tag — including the `-win` and `-mac` tags, which
# carry no AppImage. A fixed tag cannot be pointed at a release that has none,
# and is equally immune to a release marked prerelease.
#
# The output is named for the fixed tag too. zsync records the filename it was
# generated for and a client resolves it relative to the .zsync URL, so a
# versioned name would send every client looking for the version it already
# has. The versioned copy is written afterwards for the normal release.
#
# It also fills in `Categories=`, which linuxdeploy leaves empty — that is what
# a desktop menu and most managers use to file the application.
#
# Usage: finalize-appimage.sh <directory holding the .AppImage>

set -euo pipefail

LIB="libwayland-client.so.0"
FALLBACK_DIR="usr/lib/wayland-fallback"
HOOK="apprun-hooks/triple-c-wayland-fallback.sh"
APPIMAGE_TOOL_URL="https://github.com/AppImage/appimagetool/releases/download/continuous/appimagetool-x86_64.AppImage"

APP_ID="com.triple-c.desktop"
# The channel pair lives in its own directory. Left beside the versioned image
# they are picked up by the release job's `*.AppImage` glob, and every release
# then carries an eighty-megabyte byte-identical duplicate under a second name
# — which is exactly as confusing on a downloads page as it sounds.
CHANNEL_DIR="update-channel"
STABLE_NAME="Triple-C_x86_64.AppImage"
UPDATE_TAG="linux-latest"
UPDATE_INFO="zsync|https://github.com/shadowdao/triple-c/releases/download/${UPDATE_TAG}/${STABLE_NAME}.zsync"
CATEGORIES="Development;Utility;"

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
appdata_src="$repo_root/packaging/appimage/$APP_ID.appdata.xml"
# appimagetool looks for `<desktop basename>.appdata.xml` and warns the
# metadata is missing under any other name — while the script cheerfully
# reported it present. The AppStream id inside the file is unchanged and is
# what actually identifies the component; only the filename follows the tool.
appdata_installed_as="Triple-C.appdata.xml"

dir="${1:?usage: finalize-appimage.sh <bundle/appimage directory>}"
cd "$dir"

shopt -s nullglob
images=(*.AppImage)
shopt -u nullglob
if [ ${#images[@]} -eq 0 ]; then
  echo "No .AppImage in $dir — nothing to do." >&2
  exit 0
fi
appimage="${images[0]}"
here="$PWD"

work="$(mktemp -d)"
check="$(mktemp -d)"
trap 'rm -rf "$work" "$check"' EXIT

echo "Inspecting $appimage"
( cd "$work" && "$here/$appimage" --appimage-extract >/dev/null )
root="$work/squashfs-root"

# The demotion and the metadata are independent jobs, and an absent library
# must not skip the second. An early exit here also left `update-channel/`
# uncreated, which killed the publish step on a missing directory and took the
# tag and mirror jobs down with it — a half-published release.
demoted=false
if [ -e "$root/usr/lib/$LIB" ]; then

  mkdir -p "$root/$FALLBACK_DIR"
  mv "$root/usr/lib/$LIB" "$root/$FALLBACK_DIR/$LIB"

cat > "$root/$HOOK" <<'HOOK_EOF'
#! /usr/bin/env bash
# Fall back to the bundled libwayland-client only when the host has none.
#
# The host's copy is the correct one whenever it exists: its Mesa was built
# against it, and `libEGL.so.1` needs symbols from it before it will load.
# Ours is here so a host without any libwayland-client still starts.
#
# This runs before AppRun.wrapped, which appends the inherited
# LD_LIBRARY_PATH after its own entries — so this is always a fallback.
_tc_host_has_wayland_client() {
    if command -v ldconfig >/dev/null 2>&1 &&
       ldconfig -p 2>/dev/null | grep -q "libwayland-client\.so\.0"; then
        return 0
    fi
    local d
    for d in /usr/lib /usr/lib64 /usr/lib/x86_64-linux-gnu \
             /lib /lib64 /lib/x86_64-linux-gnu; do
        [ -e "$d/libwayland-client.so.0" ] && return 0
    done
    return 1
}

if ! _tc_host_has_wayland_client; then
    _TC_APPDIR="${APPDIR:-"$(dirname "$(readlink -f "$0")")/.."}"
    export LD_LIBRARY_PATH="${_TC_APPDIR}/usr/lib/wayland-fallback${LD_LIBRARY_PATH:+:${LD_LIBRARY_PATH}}"
fi
unset -f _tc_host_has_wayland_client
HOOK_EOF
chmod +x "$root/$HOOK"

# AppRun sources each hook by name rather than globbing the directory, so a
# new hook file is inert until AppRun is told about it.
if ! grep -q "triple-c-wayland-fallback" "$root/AppRun"; then
  python3 - "$root/AppRun" <<'PATCH_EOF'
import sys
path = sys.argv[1]
src = open(path).read()
exec_line = 'exec "$this_dir"/AppRun.wrapped "$@"'
if exec_line not in src:
    raise SystemExit("AppRun does not have the exec line this patch expects")
src = src.replace(
    exec_line,
    'source "$this_dir"/apprun-hooks/"triple-c-wayland-fallback.sh"\n' + exec_line,
)
open(path, "w").write(src)
PATCH_EOF
  fi
  demoted=true
  echo "Demoted $LIB to $FALLBACK_DIR."
else
  echo "$LIB is not bundled — nothing to demote."
fi

# --- metadata -------------------------------------------------------------

# Version comes from the artifact rather than a second source that could drift.
version="$(printf '%s' "$appimage" | sed -n 's/.*_\([0-9][0-9.]*\)_.*/\1/p')"
[ -n "$version" ] || { echo "Could not read a version out of $appimage" >&2; exit 1; }

if [ -f "$appdata_src" ]; then
  mkdir -p "$root/usr/share/metainfo"
  sed -e "s/@VERSION@/$version/" -e "s/@DATE@/$(date -u +%Y-%m-%d)/" \
    "$appdata_src" > "$root/usr/share/metainfo/$appdata_installed_as"
  echo "Added AppStream metadata for $version."
else
  echo "No AppStream source at $appdata_src — skipping." >&2
fi

# linuxdeploy emits `Categories=` empty, which files the app nowhere.
for desktop in "$root"/*.desktop; do
  [ -e "$desktop" ] || continue
  if grep -q "^Categories=$" "$desktop"; then
    sed -i "s/^Categories=$/Categories=$CATEGORIES/" "$desktop"
    echo "Filled in Categories for $(basename "$desktop")."
  fi
done

echo "Repacking."

tool="$work/appimagetool"
curl -fsSL -o "$tool" "$APPIMAGE_TOOL_URL"
chmod +x "$tool"

# --appimage-extract-and-run: CI runners generally have no FUSE.
# -u embeds the update string and writes "$STABLE_NAME.zsync" beside the image.
rm -rf "$CHANNEL_DIR"
mkdir -p "$CHANNEL_DIR"
ARCH=x86_64 "$tool" --appimage-extract-and-run \
  -u "$UPDATE_INFO" "$root" "$CHANNEL_DIR/$STABLE_NAME" >/dev/null
chmod +x "$CHANNEL_DIR/$STABLE_NAME"

# The versioned name is what the per-version release publishes; the stable one
# and its .zsync go to the rolling tag. Same bytes, two names, two places.
# zsyncmake writes the .zsync into the working directory, not beside the image
# it describes, so it has to be collected rather than assumed in place.
[ -e "$STABLE_NAME.zsync" ] && mv "$STABLE_NAME.zsync" "$CHANNEL_DIR/"

cp "$CHANNEL_DIR/$STABLE_NAME" "$appimage"
chmod +x "$appimage"

# The guards are the test. Each one is a way the repack could look like it
# worked while shipping the original bug.
( cd "$check" && "$here/$appimage" --appimage-extract >/dev/null )
out="$check/squashfs-root"

fail() { echo "FAILED: $1" >&2; exit 1; }

if [ "$demoted" = true ]; then
  [ -e "$out/usr/lib/$LIB" ] && fail "$LIB is still on the loader path."
  [ -e "$out/$FALLBACK_DIR/$LIB" ] || fail "the fallback copy of $LIB is missing."
  [ -e "$out/$HOOK" ] || fail "the fallback hook is missing."
  grep -q "triple-c-wayland-fallback" "$out/AppRun" || fail "AppRun does not source the hook."
fi
[ -x "$out/usr/bin/triple-c" ] || fail "no executable usr/bin/triple-c."

# An empty Categories or missing metadata ships an image a manager cannot file
# or describe, and both fail silently at runtime rather than at build time.
! grep -q "^Categories=$" "$out"/*.desktop || fail "a desktop file still has an empty Categories."
[ -f "$appdata_src" ] && { [ -e "$out/usr/share/metainfo/$appdata_installed_as" ] \
  || fail "AppStream metadata did not make it into the image."; }

# The update string is the difference between adoptable and updatable. It
# lives in the image's own `.upd_info` ELF section, not in the .zsync — the
# .zsync only records a *relative* filename, which a client resolves against
# the URL it fetched the .zsync from. That is exactly why the output is named
# for the fixed tag: a versioned name here resolves to the build the client
# already has.
[ -e "$CHANNEL_DIR/$STABLE_NAME" ] || fail "the stable-named image is missing."
[ -e "$CHANNEL_DIR/$STABLE_NAME.zsync" ] || fail "appimagetool wrote no .zsync."

readelf -p .upd_info "$CHANNEL_DIR/$STABLE_NAME" 2>/dev/null | grep -qF "$UPDATE_INFO" \
  || fail "the image does not carry exactly the expected update information."
grep -aq "^Filename: $STABLE_NAME$" "$CHANNEL_DIR/$STABLE_NAME.zsync" \
  || fail "the .zsync names something other than $STABLE_NAME."

# The versioned release must carry one AppImage, not two. This is the guard
# for the duplicate that shipped in 0.4.20 and 0.4.21.
shopt -s nullglob
beside=(*.AppImage)
shopt -u nullglob
[ "${#beside[@]}" -eq 1 ] \
  || fail "expected 1 AppImage beside the release, found ${#beside[@]}."

echo "OK: $appimage prefers the host $LIB (fallback kept) and carries AppStream"
echo "    metadata. Channel pair in $CHANNEL_DIR/, updating from the $UPDATE_TAG tag."
