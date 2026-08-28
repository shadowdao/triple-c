#!/bin/sh
# Register a Triple-C AppImage with the desktop, so it appears in the app
# launcher with its icon instead of only being runnable from a file manager.
#
# An AppImage is a single executable file and nothing more: it ships a
# `.desktop` entry and icons *inside* itself, but nothing on the host ever
# reads them, because nothing installed it. This script does what a package
# manager's install hooks would — copies the icons into the user's icon theme
# and writes a `.desktop` entry pointing at wherever the AppImage actually
# lives.
#
#     ./scripts/install-appimage.sh ~/Apps/Triple-C_0.4.17_amd64.AppImage
#     ./scripts/install-appimage.sh --uninstall
#
# Everything goes under ~/.local/share, so there is no sudo and no root-owned
# file to clean up later. The AppImage itself is never copied or moved — the
# launcher entry points at the path you give here, so keep the file somewhere
# stable (`~/Apps` or `~/.local/bin`, not `~/Downloads`) or re-run this after
# moving it.
#
# Extraction uses `--appimage-extract`, which unpacks the payload directly and
# needs no FUSE. So this script works even on a system where *running* the
# AppImage would need `fuse2` installed first.

set -eu

APP_ID="triple-c"
DESKTOP_DIR="${XDG_DATA_HOME:-$HOME/.local/share}/applications"
ICON_DIR="${XDG_DATA_HOME:-$HOME/.local/share}/icons/hicolor"
DESKTOP_FILE="${DESKTOP_DIR}/${APP_ID}.desktop"

refresh_caches() {
    # Both are best-effort: a minimal desktop may ship neither, and neither
    # failing means the install did not work.
    if command -v update-desktop-database >/dev/null 2>&1; then
        update-desktop-database "${DESKTOP_DIR}" 2>/dev/null || true
    fi
    if command -v gtk-update-icon-cache >/dev/null 2>&1; then
        gtk-update-icon-cache -f -t "${ICON_DIR}" 2>/dev/null || true
    fi
}

uninstall() {
    rm -f "${DESKTOP_FILE}"
    find "${ICON_DIR}" -name "${APP_ID}.png" -delete 2>/dev/null || true
    refresh_caches
    echo "Removed the Triple-C launcher entry and icons."
    echo "The AppImage itself was not touched."
}

if [ "${1:-}" = "--uninstall" ]; then
    uninstall
    exit 0
fi

APPIMAGE="${1:-}"
if [ -z "${APPIMAGE}" ]; then
    echo "usage: $0 [--uninstall] /path/to/Triple-C_<version>_amd64.AppImage" >&2
    exit 2
fi
if [ ! -f "${APPIMAGE}" ]; then
    echo "No such file: ${APPIMAGE}" >&2
    exit 1
fi

# An absolute path, because the .desktop Exec line is read from anywhere.
APPIMAGE=$(cd "$(dirname "${APPIMAGE}")" && printf '%s/%s' "$(pwd)" "$(basename "${APPIMAGE}")")

if [ ! -x "${APPIMAGE}" ]; then
    echo "Making ${APPIMAGE} executable"
    chmod +x "${APPIMAGE}"
fi

WORK=$(mktemp -d)
# shellcheck disable=SC2064  # WORK is expanded now on purpose.
trap "rm -rf '${WORK}'" EXIT INT TERM

echo "Extracting bundled icons from $(basename "${APPIMAGE}")..."
( cd "${WORK}" && "${APPIMAGE}" --appimage-extract >/dev/null )

SRC="${WORK}/squashfs-root"
if [ ! -d "${SRC}/usr/share/icons/hicolor" ]; then
    echo "That AppImage has no bundled icons — is it really Triple-C?" >&2
    exit 1
fi

# Copy every size the bundle ships, keeping the theme's directory layout.
COUNT=0
while IFS= read -r icon; do
    [ -n "${icon}" ] || continue
    rel=${icon#"${SRC}/usr/share/icons/hicolor/"}
    install -Dm644 "${icon}" "${ICON_DIR}/${rel}"
    COUNT=$((COUNT + 1))
done <<EOF
$(find "${SRC}/usr/share/icons/hicolor" -name "${APP_ID}.png")
EOF
echo "Installed ${COUNT} icon size(s) into ${ICON_DIR}"

# Written rather than copied from the bundle. The bundled entry has
# `Exec=triple-c`, which resolves only inside the running AppImage's own mount
# — from the host it names a binary that is not on PATH, so the launcher entry
# would appear and then fail to start anything. `Categories` is empty in the
# bundle too, which leaves the entry to fall into "Other" in most menus.
# `StartupWMClass` is kept exactly as the bundle sets it: it is what lets the
# shell match the running window to this entry, so the taskbar shows the real
# icon instead of a generic placeholder.
mkdir -p "${DESKTOP_DIR}"
cat > "${DESKTOP_FILE}" <<DESKTOP
[Desktop Entry]
Type=Application
Name=Triple-C
Comment=Run Claude Code sandboxed in Docker containers
Exec=${APPIMAGE} %U
Icon=${APP_ID}
Terminal=false
Categories=Development;
StartupWMClass=${APP_ID}
DESKTOP
chmod 644 "${DESKTOP_FILE}"
echo "Wrote ${DESKTOP_FILE}"

refresh_caches

echo
echo "Done. Triple-C should now be in your app launcher."
echo "If the icon is generic or the entry is missing, log out and back in —"
echo "see \"App Icon Missing After Installing (Linux)\" in HOW-TO-USE.md."
