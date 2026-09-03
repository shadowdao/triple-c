#!/usr/bin/env bash
#
# Drop the bundled libwayland-client.so.0 out of a built AppImage.
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
# Usage: unbundle-wayland-client.sh <directory holding the .AppImage>

set -euo pipefail

LIB="libwayland-client.so.0"
FALLBACK_DIR="usr/lib/wayland-fallback"
HOOK="apprun-hooks/triple-c-wayland-fallback.sh"
APPIMAGE_TOOL_URL="https://github.com/AppImage/appimagetool/releases/download/continuous/appimagetool-x86_64.AppImage"

dir="${1:?usage: unbundle-wayland-client.sh <bundle/appimage directory>}"
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

if [ ! -e "$root/usr/lib/$LIB" ]; then
  # Not a failure: linuxdeploy may have stopped bundling it, which is the
  # outcome this script exists to produce.
  echo "$LIB is not bundled — leaving $appimage alone."
  exit 0
fi

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

echo "Demoted $LIB to $FALLBACK_DIR; repacking."

tool="$work/appimagetool"
curl -fsSL -o "$tool" "$APPIMAGE_TOOL_URL"
chmod +x "$tool"

# --appimage-extract-and-run: CI runners generally have no FUSE.
ARCH=x86_64 "$tool" --appimage-extract-and-run "$root" "$appimage" >/dev/null
chmod +x "$appimage"

# The guards are the test. Each one is a way the repack could look like it
# worked while shipping the original bug.
( cd "$check" && "$here/$appimage" --appimage-extract >/dev/null )
out="$check/squashfs-root"

fail() { echo "FAILED: $1" >&2; exit 1; }

[ -e "$out/usr/lib/$LIB" ] && fail "$LIB is still on the loader path."
[ -e "$out/$FALLBACK_DIR/$LIB" ] || fail "the fallback copy of $LIB is missing."
[ -e "$out/$HOOK" ] || fail "the fallback hook is missing."
grep -q "triple-c-wayland-fallback" "$out/AppRun" || fail "AppRun does not source the hook."
[ -x "$out/usr/bin/triple-c" ] || fail "no executable usr/bin/triple-c."

echo "OK: $appimage now prefers the host $LIB, with a bundled fallback."
