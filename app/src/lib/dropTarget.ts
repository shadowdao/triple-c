/**
 * Routing for Tauri's *native* drag-drop event.
 *
 * The listener is window-wide — every pane that wants dropped file paths gets
 * the same event — so the module answers two separate questions, and keeping
 * them separate is the whole design:
 *
 * 1. **Which pane is this drop for?** Geometry, and nothing else: is the
 *    payload position inside my rect? A hidden pane is `display:none` and so
 *    has a zero-size rect, which is what stops two panes both claiming the
 *    same drop. `TerminalView` is the only pane that takes dropped files
 *    today — the Files pane is container-side only — but the routing is what
 *    keeps it honest when a second one appears.
 * 2. **Should the app accept a drop at all right now?** `dropIsBlocked` —
 *    document-wide, no geometry, no z-order. While a modal or a blocking
 *    overlay is on screen anywhere, every drop is refused.
 *
 * ## Why there is no z-order test here, and must not be one
 *
 * A drop that lands underneath a dialog and silently uploads into the
 * container behind it is the failure mode that matters: it is
 * invisible, it writes to the container, and the user did not ask for it.
 * Every attempt to be *precise* about which points a dialog covers has gone
 * wrong, twice, in opposite directions:
 *
 * - Asking `el.contains(document.elementFromPoint(x, y))` — "is the thing
 *   painted here mine?" — refused drops onto anything painted *over* a pane
 *   that is not part of it: `TerminalView`'s always-rendered "▼ Following"
 *   toggle (a sibling of the xterm host), the URL toast, `ToastHost`'s stack.
 *   Permanent dead zones no user action could clear.
 * - Replacing that with "is a *blocking overlay* painted here?" removed the
 *   dead zones and opened a hole instead. `elementFromPoint` returns the
 *   topmost painted element, and plenty of things paint above a `z-50` modal
 *   backdrop in the same stacking context: `ToastHost` is `z-[60]`, so is
 *   `TerminalContextMenu`. A refused drop pushed a toast; the toast then sat
 *   over the dialog; the next drop released on that toast was reported as
 *   "clear" and landed in the covered directory. The gate armed its own hole.
 *
 * Both bugs are the same mistake: trusting a per-point answer to decide
 * whether the app should be accepting work at all. The document-wide question
 * has no z-index in it, so no future overlay can become a drop hole by being
 * painted high enough, and no chrome can become a dead zone by being painted
 * at all.
 *
 * What it costs: while any dialog is open, drops are refused *everywhere*,
 * including on parts of a pane the dialog does not cover. That is a state the
 * user put the app in deliberately and can leave in one keystroke, the
 * refusal is announced, and nothing is written. It is strictly the better
 * failure.
 *
 * ## jsdom
 *
 * Note for future changes: jsdom implements no layout and has no
 * `elementFromPoint`, which is how the first of those two bugs shipped green
 * through 81 drop tests — the branch was never entered in any of them. This
 * module no longer calls it (`dropTarget.test.ts` asserts that it does not),
 * so the gap can no longer hide a bug here. Anything that reintroduces a
 * geometric z-order test reintroduces the gap as well.
 */

export interface DropPoint {
  x: number;
  y: number;
}

/**
 * Anything that swallows drops while it is on screen.
 *
 * `[aria-modal="true"]` is every dialog in the app for free — `ui/Modal` is
 * the only way one is built, and it sets that attribute. `data-blocks-drop`
 * is for full-window overlays that are not dialogs (the shutdown overlay), and
 * `ui/Modal` puts it on its backdrop as well.
 */
const BLOCKING_SELECTOR = '[aria-modal="true"],[data-blocks-drop="true"]';

/**
 * A blocker inside this is in the DOM but not on screen — `ui/Modal` marks
 * itself `hidden` when the pane that owns it is not the visible one, so a
 * dialog left open in project A stops refusing drops in project B the moment
 * the tab changes.
 *
 * `[hidden]` only, deliberately. `aria-hidden="true"` used to count too, and
 * it is not a visibility statement: it is routinely put on *visible*
 * decorative content (`ui/Modal`'s own ✕ glyph, every `StatusIndicator`
 * dot). An overlay that happened to sit inside such a wrapper would have
 * silently stopped blocking — the exact class of hole this gate exists to
 * close. `ui/Modal` sets `hidden`, the `hidden` attribute, and inline
 * `display:none` together, so nothing in the app depended on the aria half.
 */
const OFFSCREEN_SELECTOR = "[hidden]";

/** A blocker that is actually painted, rather than merely mounted. */
function isOnScreen(el: Element): boolean {
  return el.closest(OFFSCREEN_SELECTOR) === null;
}

/** True while a modal or a blocking overlay is on screen *anywhere*. */
export function dropIsBlocked(doc: Document = document): boolean {
  return Array.from(doc.querySelectorAll(BLOCKING_SELECTOR)).some(isOnScreen);
}

/**
 * What both listeners say when they refuse a drop.
 *
 * `kind: "info"`, so it times out on its own: a drop refused because the user
 * has a dialog open is expected behaviour, not an error, and an error card
 * would sit on screen until dismissed. `dedupeKey` means three refused drops
 * leave one notice rather than a stack of three.
 */
export const DROP_BLOCKED_TOAST = {
  kind: "info",
  message: "File drop ignored",
  detail:
    "A dialog or full-window overlay is open, so nothing accepts dropped files. Close it and drop again.",
  dedupeKey: "drop-blocked",
} as const;

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
 * position on a HiDPI Mac or Linux box, aiming the hit test at a point the
 * user never touched.
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
 * - `accept` — it is ours, and the app is in a state to take it.
 * - `blocked` — it was aimed at us, but a modal or a blocking overlay is on
 *   screen. Worth *saying* to the user: the drop visibly did nothing.
 * - `elsewhere` — not our drop. Silence is the right response; some other
 *   pane's listener may be about to accept it.
 *
 * Geometry is asked **first**, so exactly one pane can ever answer `blocked`
 * for a given drop and the refusal is announced once rather than once per
 * listener.
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

  // Whose drop it is has been settled. Whether the app should be taking drops
  // at all is a separate, document-wide question — see the header.
  return dropIsBlocked(doc) ? "blocked" : "accept";
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
