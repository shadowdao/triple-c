# Branding

The mark is a container with its right wall opened, so the enclosure itself is the letter **C**,
holding a `>_` prompt. Container plus shell, in one closed shape — no type inside the icon, so
nothing goes illegible when the app is 16 px tall in a taskbar.

## Files

| File | What it is |
|---|---|
| `triple-c-mark.svg` | The mark alone, transparent. Use on any ground. |
| `triple-c-mark-small.svg` | Optical variant for 32 px and below. |
| `triple-c-icon.svg` | The app icon: mark at 70% on a `#0D1117` tile, 22% corner radius. |
| `triple-c-icon-small.svg` | The app icon at small sizes — small mark, drawn at 82%. |
| `triple-c-lockup-dark.svg` | Horizontal lockup for dark backgrounds. Wordmark is outlined, so no font is needed to render it. |
| `triple-c-lockup-light.svg` | The same for light backgrounds. |
| `triple-c-icon-1024.png` | Master raster, generated. |
| `build-icons.py` | Regenerates every packaged icon from the SVGs. |

## Palette

| Role | Dark ground | Light ground |
|---|---|---|
| Frame | `#58A6FF` (`--accent`) | `#1F6FEB` (`--accent-emphasis`) |
| Prompt | `#F0821E` | `#C4610F` |
| Tile | `#0D1117` (`--bg-primary`) | — |
| Wordmark | `#E6EDF3` (`--text-primary`) | `#131C25` |

The frame and prompt colours are the app's own accent tokens from `app/src/index.css`, which is why
the icon sits on the app's chrome instead of fighting it. Orange is the only equity carried over
from the previous marks, and it is now an accent rather than a background.

## Two sources, not one

`build-icons.py` renders sizes ≥ 48 px from `triple-c-icon.svg` and sizes ≤ 32 px from
`triple-c-icon-small.svg`. At small sizes the cursor bar closes up against the chevron and a 12 px
frame stroke resamples to a grey smear, so the small variant drops the cursor, widens the mouth,
thickens the strokes and draws the mark larger inside the tile.

This matters most for `icon.ico`: Windows picks the 16 px entry for the window corner and 24/32 px
for the taskbar. The previous `.ico` contained a *single* 16 px image, which Windows then upscaled
everywhere else — the likely cause of `screenshot_for_fix/task_bar_icon_not_correct.png`. The
current one carries 16, 24, 32, 48, 64, 128 and 256, each rendered from vector rather than
downsampled from one bitmap.

## Regenerating

```bash
pip install cairosvg pillow
python3 branding/build-icons.py
```

Writes `app/src-tauri/icons/{32x32,128x128,128x128@2x,icon}.png`, `icon.ico`, `icon.icns`,
`app/public/favicon.svg` and `branding/triple-c-icon-1024.png`. Do not hand-edit those — edit the
SVG and re-run.

The lockups are committed sources, not generated: their wordmark is Liberation Mono Bold converted
to outlines (`-1.6` tracking, 44 px cap height), so re-cutting it needs the font and `fonttools`.
