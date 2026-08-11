#!/usr/bin/env python3
"""Render every packaged icon from the SVG sources in this directory.

    pip install cairosvg pillow
    python3 branding/build-icons.py

The SVGs are the source of truth; everything under app/src-tauri/icons/ and the
favicon are generated. Two sources, not one, on purpose:

    triple-c-icon.svg        used for 48 px and up
    triple-c-icon-small.svg  used for 32 px and down

At 16-32 px the cursor bar closes up against the chevron and the frame's 12 px
stroke resamples to a grey smear, so the small source drops the cursor, widens
the mouth and draws the mark larger in the tile. Windows picks the 16 and 24 px
entries out of the .ico for the taskbar and window corner, which is exactly
where the old single-size .ico was falling over.
"""

import io
import struct
from pathlib import Path

import cairosvg
from PIL import Image

BRANDING = Path(__file__).resolve().parent
REPO = BRANDING.parent
ICONS = REPO / "app" / "src-tauri" / "icons"
PUBLIC = REPO / "app" / "public"

FULL_SRC = BRANDING / "triple-c-icon.svg"
SMALL_SRC = BRANDING / "triple-c-icon-small.svg"

# Below this, render from the small source.
SMALL_MAX = 32

# .ico entries. Windows uses 16 in the window corner and 24/32 in the taskbar,
# 48 in list views and 256 in the "extra large icons" view.
ICO_SIZES = [16, 24, 32, 48, 64, 128, 256]

# .icns entries: (chunk type, pixel size). PNG-backed chunks, macOS 10.7+.
ICNS_ENTRIES = [
    (b"ic11", 32),    # 16pt @2x
    (b"ic12", 64),    # 32pt @2x
    (b"ic07", 128),   # 128pt
    (b"ic13", 256),   # 128pt @2x
    (b"ic08", 256),   # 256pt
    (b"ic14", 512),   # 256pt @2x
    (b"ic09", 512),   # 512pt
    (b"ic10", 1024),  # 512pt @2x
]


def render(size: int) -> Image.Image:
    """Rasterise the right source at `size` px square."""
    src = SMALL_SRC if size <= SMALL_MAX else FULL_SRC
    png = cairosvg.svg2png(url=str(src), output_width=size, output_height=size)
    return Image.open(io.BytesIO(png)).convert("RGBA")


def write_png(size: int, path: Path) -> None:
    render(size).save(path, "PNG", optimize=True)
    print(f"  {path.relative_to(REPO)}  {size}x{size}")


def write_ico(path: Path) -> None:
    """Hand-assemble the .ico so each entry can come from its own source.

    Pillow's save(sizes=...) downsamples one image, which would put the
    12 px-stroke artwork into the 16 px entry — the thing this avoids.
    """
    images = [render(s) for s in ICO_SIZES]
    payloads = []
    for img in images:
        buf = io.BytesIO()
        img.save(buf, "PNG", optimize=True)  # PNG-compressed entries, Vista+
        payloads.append(buf.getvalue())

    offset = 6 + 16 * len(images)
    header = struct.pack("<HHH", 0, 1, len(images))
    entries, blob = b"", b""
    for img, data in zip(images, payloads):
        w = 0 if img.width >= 256 else img.width
        h = 0 if img.height >= 256 else img.height
        entries += struct.pack("<BBBBHHII", w, h, 0, 0, 1, 32, len(data), offset)
        blob += data
        offset += len(data)
    path.write_bytes(header + entries + blob)
    print(f"  {path.relative_to(REPO)}  {', '.join(str(s) for s in ICO_SIZES)}")


def write_icns(path: Path) -> None:
    chunks = b""
    for kind, size in ICNS_ENTRIES:
        buf = io.BytesIO()
        render(size).save(buf, "PNG", optimize=True)
        data = buf.getvalue()
        chunks += kind + struct.pack(">I", len(data) + 8) + data
    path.write_bytes(b"icns" + struct.pack(">I", len(chunks) + 8) + chunks)
    print(f"  {path.relative_to(REPO)}  {', '.join(str(s) for _, s in ICNS_ENTRIES)}")


def main() -> None:
    print("branding/")
    write_png(1024, BRANDING / "triple-c-icon-1024.png")

    print("app/src-tauri/icons/")
    write_png(32, ICONS / "32x32.png")
    write_png(128, ICONS / "128x128.png")
    write_png(256, ICONS / "128x128@2x.png")
    write_png(512, ICONS / "icon.png")
    write_ico(ICONS / "icon.ico")
    write_icns(ICONS / "icon.icns")

    print("app/public/")
    (PUBLIC / "favicon.svg").write_text(SMALL_SRC.read_text())
    print(f"  {(PUBLIC / 'favicon.svg').relative_to(REPO)}  (copy of {SMALL_SRC.name})")


if __name__ == "__main__":
    main()
