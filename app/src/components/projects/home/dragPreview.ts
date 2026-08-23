/**
 * The image the OS shows under the cursor during a drag-out.
 *
 * `startDrag` requires one — the plugin's `image` argument is not optional, and
 * it only accepts a `data:image/png;base64,` URL — so this is drawn rather than
 * shipped as an asset. Drawing it is also what keeps the colours honest: the
 * palette lives in CSS custom properties, and reading them off the document is
 * the only way a raw-pixel preview can still come from the design tokens rather
 * than from hard-coded hexes.
 */

/**
 * A 1x1 transparent PNG, used when no 2D canvas is available — jsdom has none,
 * and a webview can refuse a context under memory pressure. `startDrag` needs
 * *an* image, and a drag with an invisible preview is much better than no drag.
 */
const TRANSPARENT_PNG =
  "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAAC0lEQVR4nGNgAAIAAAUAAXpeqz8AAAAASUVORK5CYII=";

/** Longest filename drawn in full; past this the middle is elided. */
const MAX_LABEL = 28;

function cssVar(name: string, fallback: string): string {
  const value = getComputedStyle(document.documentElement).getPropertyValue(name).trim();
  return value || fallback;
}

/** Keep both ends of a long name — the extension is the informative half. */
function elide(label: string): string {
  if (label.length <= MAX_LABEL) return label;
  const head = label.slice(0, MAX_LABEL - 12);
  const tail = label.slice(-9);
  return `${head}…${tail}`;
}

export function dragPreviewIcon(label: string): string {
  try {
    const text = elide(label);
    // Cap the scale: the OS draws this at logical size, so a 3x buffer is only
    // bytes over IPC.
    const scale = Math.min(window.devicePixelRatio || 1, 2);
    const height = 24;
    const padding = 8;
    const canvas = document.createElement("canvas");

    // Measuring needs a context, and sizing the canvas resets it — so measure
    // on a throwaway pass, then size, then draw.
    const probe = canvas.getContext("2d");
    if (!probe) return TRANSPARENT_PNG;
    const font = "12px ui-monospace, SFMono-Regular, Menlo, monospace";
    probe.font = font;
    const width = Math.ceil(probe.measureText(text).width) + padding * 2;

    canvas.width = Math.round(width * scale);
    canvas.height = Math.round(height * scale);
    const ctx = canvas.getContext("2d");
    if (!ctx) return TRANSPARENT_PNG;
    ctx.scale(scale, scale);

    ctx.fillStyle = cssVar("--bg-tertiary", "#2a2a2a");
    ctx.strokeStyle = cssVar("--accent", "#6aa8ff");
    ctx.lineWidth = 1;
    if (typeof ctx.roundRect === "function") {
      ctx.beginPath();
      ctx.roundRect(0.5, 0.5, width - 1, height - 1, 4);
      ctx.fill();
      ctx.stroke();
    } else {
      ctx.fillRect(0.5, 0.5, width - 1, height - 1);
      ctx.strokeRect(0.5, 0.5, width - 1, height - 1);
    }

    ctx.font = font;
    ctx.fillStyle = cssVar("--text-primary", "#e6e6e6");
    ctx.textBaseline = "middle";
    ctx.fillText(text, padding, height / 2);

    const url = canvas.toDataURL("image/png");
    // jsdom (and a canvas that failed to encode) answers `data:,` — which the
    // Rust side rejects outright, taking the whole drag with it.
    return url.startsWith("data:image/png;base64,") ? url : TRANSPARENT_PNG;
  } catch {
    return TRANSPARENT_PNG;
  }
}
