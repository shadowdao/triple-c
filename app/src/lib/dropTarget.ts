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
 * So the hit test is: the point is inside my rect, **and** nothing that
 * *swallows* drops is painted at that point.
 *
 * ## The question the z-order test asks — and the one it must not ask
 *
 * The first version of this asked `el.contains(document.elementFromPoint(x,y))`
 * — "is the thing painted here mine?" That is the wrong question, and it
 * created permanent dead zones. Panes have chrome painted *over* them that is
 * not part of the element handed to this function and does not swallow
 * anything: `TerminalView`'s always-present "▼ Following" button, the URL
 * toast, and `ToastHost`'s bottom-right stack — which is `fixed` at `z-[60]`
 * over *every* pane and whose error cards stay until dismissed. Under the
 * containment rule a drop onto any of them was silently refused, forever.
 *
 * The question that matches the intent is "is a *blocking overlay* painted
 * here?". A button, a toast or a tooltip over the pane is not one; a modal
 * backdrop is. Anything else painted at the point belongs to the pane's own
 * subtree or is chrome that is happy for the drop to fall through to it.
 *
 * That rule is also what *scopes* the blocking question. `dropIsBlocked` is
 * document-wide, and `ui/Modal` portals to `document.body`, so any dialog
 * anywhere used to refuse every drop in the window. Asking per-point means a
 * dialog only refuses the points it actually covers.
 *
 * ## jsdom
 *
 * jsdom implements no layout and has no `elementFromPoint`, so the z-order
 * branch cannot be exercised by simply rendering — which is exactly how the
 * containment bug shipped green through 81 drop tests. Every test that cares
 * about z-order therefore stubs `elementFromPoint` (see `dropTarget.test.ts`),
 * and the fallback below — when there is no such API, or it cannot resolve the
 * point — is the conservative document-wide question this used to ask.
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
 * is for full-window overlays that are not dialogs (the shutdown overlay), and
 * `ui/Modal` puts it on its backdrop as well: the backdrop is what
 * `elementFromPoint` returns for a point outside the dialog panel, and it is
 * the element that is really covering the pane.
 */
const BLOCKING_SELECTOR = '[aria-modal="true"],[data-blocks-drop="true"]';

/**
 * A blocker inside one of these is in the DOM but not on screen — `ui/Modal`
 * marks itself this way when the pane that owns it is not the visible one, so
 * a dialog left open in project A stops covering project B the moment the tab
 * changes.
 */
const OFFSCREEN_SELECTOR = '[hidden],[aria-hidden="true"]';

/** A blocker that is actually painted, rather than merely mounted. */
function isOnScreen(el: Element): boolean {
  return el.closest(OFFSCREEN_SELECTOR) === null;
}

/** True while a modal or a blocking overlay is on screen *anywhere*. */
export function dropIsBlocked(doc: Document = document): boolean {
  return Array.from(doc.querySelectorAll(BLOCKING_SELECTOR)).some(isOnScreen);
}

export interface DropTargetOptions {
  doc?: Document;
  /** Override the ratio used to convert physical pixels to CSS pixels. */
  devicePixelRatio?: number;
  /**
   * Override the platform question "does this payload arrive in physical
   * pixels?". Tests use it; nothing in the app passes it.
   */
  physicalPixelPayload?: boolean;
}

/**
 * Whether the drop payload's coordinates are physical device pixels.
 *
 * **Only Windows delivers physical pixels.** `wry`'s WebView2 drag-drop
 * handler reads the point from the OS in device pixels and passes it through
 * (`wry/src/webview2/drag_drop.rs`), while the macOS and GTK backends deliver
 * logical points and `tauri-runtime-wry`'s forwarding does not rescale them.
 * Dividing by `devicePixelRatio` unconditionally therefore halved every drop
 * position on a HiDPI Mac or Linux box — which used to be a silent
 * mis-aimed-but-usually-still-inside-the-pane error and, with a z-order test
 * in place, becomes a drop refused because the *halved* point lands on
 * something else.
 *
 * Verified by reading the wry/tauri sources named above. **Not** verified on a
 * real HiDPI macOS or GTK machine — neither is available here — which is why
 * the divisor is platform-conditional rather than simply deleted: the Windows
 * behaviour is the one covered by tests and by the shipped code path.
 */
function payloadIsPhysical(
  view: (Window & typeof globalThis) | null,
  options: DropTargetOptions,
): boolean {
  if (options.physicalPixelPayload !== undefined) return options.physicalPixelPayload;
  return /windows/i.test(view?.navigator?.userAgent ?? "");
}

/**
 * Why a native drop at `pos` did or did not belong to `el`.
 *
 * - `accept` — it is ours.
 * - `blocked` — it landed on our rect, but a modal or a blocking overlay is
 *   painted there and swallowed it. Worth *saying* to the user: the drop
 *   visibly did nothing.
 * - `elsewhere` — not our drop. Silence is the right response; some other
 *   pane's listener is about to accept it.
 *
 * A hidden pane is `display:none` and therefore has a zero-size rect, which is
 * what stops two panes both claiming the same drop.
 */
export type DropVerdict = "accept" | "blocked" | "elsewhere";

export function classifyDrop(
  el: HTMLElement | null | undefined,
  pos: DropPoint,
  options: DropTargetOptions = {},
): DropVerdict {
  const doc = options.doc ?? el?.ownerDocument ?? document;

  const rect = el?.getBoundingClientRect();
  if (!el || !rect || rect.width === 0 || rect.height === 0) return "elsewhere";

  const view = doc.defaultView;
  const dpr =
    options.devicePixelRatio ??
    (payloadIsPhysical(view, options) ? view?.devicePixelRatio || 1 : 1);
  const x = pos.x / dpr;
  const y = pos.y / dpr;
  if (x < rect.left || x > rect.right || y < rect.top || y > rect.bottom) {
    return "elsewhere";
  }

  // Z-order, where the environment can answer it. `elementFromPoint` skips
  // `pointer-events: none`, so the pane's own decorative drop hint does not
  // count as something covering it.
  const top =
    typeof doc.elementFromPoint === "function" ? doc.elementFromPoint(x, y) : null;
  if (top && top !== doc.body && top !== doc.documentElement) {
    return coveredByBlocker(top, doc) ? "blocked" : "accept";
  }

  // No layout information — jsdom, or a point the view could not resolve. Fall
  // back to the document-wide question, which is the conservative answer: a
  // dialog somewhere refuses everything rather than risking a drop landing
  // underneath one.
  return dropIsBlocked(doc) ? "blocked" : "accept";
}

/**
 * Is the element painted at the drop point part of something that swallows
 * drops?
 *
 * Two directions, because a dialog is two elements: the panel carries
 * `aria-modal`, and the backdrop around it is what is painted over the pane.
 * `ui/Modal` marks its own backdrop, so `closest` covers both; the `contains`
 * half is the safety net for any overlay that wraps a dialog without marking
 * itself, and is deliberately not asked of `<body>`/`<html>` — those contain
 * every portal in the app and would make the answer "blocked" always.
 */
function coveredByBlocker(top: Element, doc: Document): boolean {
  const nearest = top.closest(BLOCKING_SELECTOR);
  if (nearest && isOnScreen(nearest)) return true;
  if (top === doc.body || top === doc.documentElement) return false;
  const inside = top.querySelector(BLOCKING_SELECTOR);
  return inside !== null && isOnScreen(inside);
}

/**
 * Whether a native drop at `pos` (physical pixels on Windows, logical
 * elsewhere) belongs to `el`.
 */
export function isDropTarget(
  el: HTMLElement | null | undefined,
  pos: DropPoint,
  options: DropTargetOptions = {},
): boolean {
  return classifyDrop(el, pos, options) === "accept";
}
