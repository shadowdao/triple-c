/**
 * Routing for Tauri's *native* drag-drop event.
 *
 * The listener is window-wide — every pane that wants dropped file paths gets
 * the same event — so each one decides for itself whether the drop was meant
 * for it. That decision used to be purely geometric: is the payload position
 * inside my rect? A rect is not what the user sees, though. An open `Modal` is
 * a `fixed inset-0` portal at `z-50` painted *over* the whole window, and the
 * pane underneath still had its rect, so releasing a drag onto a dialog
 * uploaded the file into the directory the dialog was covering. Same for the
 * shutdown overlay, which is on screen precisely while nothing should be
 * accepting work at all.
 *
 * So the hit test is now: nothing is covering the window, **and** the point is
 * inside my rect, **and** whatever is actually painted at that point is mine.
 */

export interface DropPoint {
  x: number;
  y: number;
}

/**
 * Anything that swallows a drop wherever it lands.
 *
 * `[aria-modal="true"]` is every dialog in the app for free — `ui/Modal` is
 * the only way one is built, and it sets that attribute. `data-blocks-drop`
 * is for full-window overlays that are not dialogs (the shutdown overlay).
 */
const BLOCKING_SELECTOR = '[aria-modal="true"],[data-blocks-drop="true"]';

/** True while a modal or a blocking overlay is on screen. */
export function dropIsBlocked(doc: Document = document): boolean {
  return doc.querySelector(BLOCKING_SELECTOR) !== null;
}

export interface DropTargetOptions {
  doc?: Document;
  /** Override the ratio used to convert physical pixels to CSS pixels. */
  devicePixelRatio?: number;
}

/**
 * Whether a native drop at `pos` (physical pixels) belongs to `el`.
 *
 * A hidden pane is `display:none` and therefore has a zero-size rect, which is
 * what stops two panes both claiming the same drop.
 */
export function isDropTarget(
  el: HTMLElement | null | undefined,
  pos: DropPoint,
  options: DropTargetOptions = {},
): boolean {
  const doc = options.doc ?? el?.ownerDocument ?? document;
  if (dropIsBlocked(doc)) return false;

  const rect = el?.getBoundingClientRect();
  if (!el || !rect || rect.width === 0 || rect.height === 0) return false;

  const dpr =
    options.devicePixelRatio ??
    (doc.defaultView?.devicePixelRatio || 1);
  const x = pos.x / dpr;
  const y = pos.y / dpr;
  if (x < rect.left || x > rect.right || y < rect.top || y > rect.bottom) {
    return false;
  }

  // Z-order, where the environment can answer it. `elementFromPoint` skips
  // `pointer-events: none`, so the pane's own decorative drop hint does not
  // count as something covering it. jsdom has no layout and returns null,
  // which is treated as "no opinion" rather than "not mine".
  if (typeof doc.elementFromPoint === "function") {
    const top = doc.elementFromPoint(x, y);
    if (top && top !== doc.body && top !== doc.documentElement && !el.contains(top)) {
      return false;
    }
  }

  return true;
}
