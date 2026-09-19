import { useCallback, useEffect, useRef, useState } from "react";
import { Terminal, type ILinkHandler } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import { WebglAddon } from "@xterm/addon-webgl";
import { WebLinksAddon } from "@xterm/addon-web-links";
import "@xterm/xterm/css/xterm.css";
import { useTerminal } from "../../hooks/useTerminal";
import { useAppState } from "../../store/appState";
import { CLAUDE_SOFT_NEWLINE } from "../../lib/claudeInput";
import {
  awsSsoRefresh,
  openPageInContainerBrowser,
  openUrlExternal,
  uploadHostFileToTerminal,
} from "../../lib/tauri-commands";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { UrlDetector, type UrlSource } from "../../lib/urlDetector";
import {
  RelayRateLimiter,
  URL_RELAY_OSC,
  extendsUrl,
  parseUrlRelayOsc,
  sanitizeRelayUrl,
  urlOrigin,
} from "../../lib/urlRelay";
import { classifyDrop, DROP_BLOCKED_TOAST } from "../../lib/dropTarget";
import { useSignInOpenTarget } from "../../hooks/useSignInOpenTarget";
import UrlToast, {
  URL_TOAST_PRIMARY_SELECTOR,
  URL_TOAST_SELECTOR,
  URL_TOAST_SHORTCUT,
} from "./UrlToast";
import { trimSelection } from "./trimSelection";
import { resolveTerminalGpuRendering } from "../../lib/terminalRenderer";
import TerminalContextMenu from "./TerminalContextMenu";

interface Props {
  sessionId: string;
  active: boolean;
}

/**
 * Where a prompted URL came from.
 *
 * `relay` is the container asking explicitly, over OSC 7777, with the URL
 * base64-encoded — exact by construction. `osc8` is lifted verbatim out of a
 * hyperlink parameter — also exact, but nobody asked for it. `heuristic` was
 * reassembled from painted text and is the only one that can be a *truncated
 * guess* at the link it is showing.
 */
export type PromptSource = "relay" | UrlSource;

/**
 * What the shared prompt slot holds.
 *
 * `seq` is identity: the slot is one long-lived place that several prompts pass
 * through, so "is this still the prompt I acted on?" cannot be answered by the
 * URL (the same link can legitimately be relayed twice) and must not be
 * answered by "is anything there?". It keys the toast for remounting *and*
 * guards the deferred dismissal — see `dismissUrlPromptIfCurrent`.
 */
interface UrlPrompt {
  url: string;
  label: string;
  source: PromptSource;
  seq: number;
}

/** Higher wins. Provenance, not recency. */
const SOURCE_RANK: Record<PromptSource, number> = {
  heuristic: 0,
  osc8: 1,
  relay: 2,
};

/**
 * Whether `next` may take over the prompt slot from `current`.
 *
 * The bug this exists for: `claude login` relays its OAuth URL over OSC 7777,
 * base64-encoded and therefore complete; the screen-scraper's 300 ms debounce
 * then fires, finds the same link cut into terminal-width pieces, and — under
 * the old last-writer-wins slot — replaced the good URL with a truncated one
 * that still parses, still points at the right host, and cannot authorise
 * anything. The user is the one who has to notice.
 *
 * Two rules, in order:
 *
 *  - Better provenance always wins, worse provenance never does. A scraped
 *    guess cannot displace an exact copy.
 *  - Between equals, only an *extension* of what is showing may replace it.
 *    That is {@link extendsUrl}, the same rule and the same reasoning as
 *    `pickSignInUrl` in `hooks/useClaudeAuth.ts`: a repaint can land a
 *    truncated copy before the complete one, and a longer string sharing a
 *    prefix cannot move the origin. The relay is exempt because each OSC 7777
 *    is a fresh deliberate request rather than another view of the last one —
 *    a second `gh auth login` must be able to replace the first.
 */
export function supersedes(
  next: { url: string; source: PromptSource },
  current: { url: string; source: PromptSource } | null,
): boolean {
  if (!current) return true;
  if (SOURCE_RANK[next.source] !== SOURCE_RANK[current.source]) {
    return SOURCE_RANK[next.source] > SOURCE_RANK[current.source];
  }
  if (next.source === "relay") return true;
  return extendsUrl(next.url, current.url);
}

/**
 * Marks the hover card, for xterm's stylesheet and for the tests.
 *
 * It does *not* make xterm route pointer events around the card. xterm only
 * consults this class inside `Linkifier._handleMouseMove`, which is registered
 * on `screenElement`; the card is appended to `Terminal.element`, a *sibling*
 * of that node, so the check never sees it. What keeps the card out of the way
 * is `pointerEvents: "none"` on the card itself — see `hover` below for what
 * goes wrong without it.
 */
export const OSC8_HOVER_CLASS = "xterm-hover";

/**
 * Report a failed handoff to the host's browser.
 *
 * One sink, one card. See the long note on `handleOpenUrl` for what this catch
 * does *not* catch on Linux; a click that appears to do nothing is the
 * complaint either way, so every route that opens a URL says the same thing in
 * the same place.
 */
function reportOpenFailure(e: unknown) {
  useAppState.getState().pushToast({
    kind: "error",
    message: "Could not open that link in your browser",
    detail: String(e),
    // A dead opener fails for every link in the buffer. One card.
    dedupeKey: "host-open-failed",
  });
}

/**
 * `ILinkHandler`, plus the one thing xterm never asks for.
 *
 * `leave` is only ever reached through `Linkifier._clearCurrentLink`, i.e. a
 * pointer that moved. Switching tabs from the keyboard moves no pointer and
 * the Linkifier's dispose path does not clear either, so the card outlives the
 * pane and is still there when the user comes back. {@link dismiss} is how the
 * view says "this pane is gone" without pretending to be a mouse event.
 */
export type Osc8LinkHandler = ILinkHandler & { dismiss(): void };

/**
 * Is a program holding the mouse?
 *
 * One expression, two readers that must never disagree: the status-bar badge
 * (`syncMouseCapture`) and the gate on opening a link ({@link opensOnClick}).
 * A gate that thought tracking was off while the badge said it was on would be
 * the whole security hole back again.
 */
function terminalTracksMouse(term: Terminal): boolean {
  return term.modes.mouseTrackingMode !== "none";
}

/**
 * Everything the gate asks the terminal, sampled at the moment of the click.
 *
 * A struct rather than three getters because the three are read together and
 * must describe one instant: `hasSelection` is only meaningful against the
 * `mouseTracking` that decided which gestures could have produced it.
 */
export interface ClickContext {
  /** {@link terminalTracksMouse} — the container's to change, at any time. */
  mouseTracking: boolean;
  /** Does the terminal hold a selection *right now*? See {@link opensOnClick}. */
  hasSelection: boolean;
  /** xterm's `macOptionClickForcesSelection`, read rather than assumed. */
  macOptionClickForcesSelection: boolean;
}

function readClickContext(term: Terminal): ClickContext {
  return {
    mouseTracking: terminalTracksMouse(term),
    hasSelection: term.hasSelection(),
    macOptionClickForcesSelection:
      term.options.macOptionClickForcesSelection ?? false,
  };
}

/** xterm's `isMac` verbatim (`common/Platform.ts`), so we split where it does. */
function isMacPlatform(): boolean {
  const platform = typeof navigator === "undefined" ? "" : navigator.platform;
  return ["Macintosh", "MacIntel", "MacPPC", "Mac68K"].includes(platform);
}

/**
 * xterm's `SelectionService.shouldForceSelection`, mirrored.
 *
 * The modifier is not our choice and it must not drift: while a program holds
 * the mouse, this is the one gesture the user already has for "this click is
 * for the terminal, not for the program", so it is the gesture that may open a
 * link. xterm's rule is
 * `isMac ? e.altKey && rawOptions.macOptionClickForcesSelection : e.shiftKey`,
 * and the option is read from the terminal rather than assumed: this view sets
 * it true today, so the two agreed, but xterm's default is false and nothing
 * would have reported the day that line went. A hardcoded `altKey` would then
 * accept a modifier xterm no longer treats as force-select.
 *
 * The gate and the hint below both call this. A hint that names a key the gate
 * does not accept is worse than no hint — the user concludes the link is
 * broken — and that is a bug this branch has already shipped once, so the two
 * are not allowed separate answers.
 */
function forcesSelection(
  event: { altKey: boolean; shiftKey: boolean },
  macOptionClickForcesSelection: boolean,
): boolean {
  return isMacPlatform()
    ? event.altKey && macOptionClickForcesSelection
    : event.shiftKey;
}

/**
 * What the card tells the user to do, for the state the terminal is in *now*.
 *
 * Conditional because the gesture is: with no program tracking the mouse a
 * plain click opens the link, and naming a modifier then would send the user
 * hunting for a key that changes nothing. The last branch is the same rule
 * once more: on a Mac with `macOptionClickForcesSelection` off there *is* no
 * force-selection modifier, so {@link opensOnClick} can never pass while a
 * program holds the mouse, and naming Option would be naming a dead key.
 */
function openHintLabel(ctx: ClickContext): string {
  if (!ctx.mouseTracking) return "Click to open";
  if (!isMacPlatform()) return "Shift+click to open";
  if (!ctx.macOptionClickForcesSelection) {
    return "Not clickable while a program holds the mouse";
  }
  return "Option+click to open";
}

/**
 * Whether this mouseup is a request to leave the app for the host browser.
 *
 * xterm asks none of this. `Linkifier._handleMouseUp` activates whenever the
 * mouseup lands on the same link the mousedown did — no button check, no mode
 * check, no `detail`, no drag threshold, no timestamp (`SelectionService` has
 * a `_mouseDownTimeStamp`; the Linkifier has nothing). Four refusals, for four
 * different mistakes:
 *
 *  - **Anything but the primary button.** Without this a *right*-click
 *    activates the link as well as opening this pane's context menu, and a
 *    middle-click paste opens it too.
 *  - **A mouseup that ended a selection.** This is the load-bearing one, and
 *    the reason is that a drag is a single press and a single release, so its
 *    click count is 1 and nothing else distinguishes it from a click. Both
 *    gestures a user makes to *copy* a string end here: drag across a few
 *    characters, or double-click a word (xterm selects it on the second
 *    mousedown, so the selection is already in the model by the time this
 *    runs). Worse, while a program holds the mouse Shift/Option+drag is the
 *    *only* way to select at all — byte-identical to the modifier below — so
 *    without this check a container that wraps each output row in an OSC 8
 *    turns every legitimate copy into a browser open. The selection check is
 *    also cheap to be wrong about in the safe direction: xterm's
 *    `_handleSingleClick` clears the model on the mousedown of a plain click,
 *    so an old selection elsewhere in the buffer is already gone by the time a
 *    real click on a link arrives here.
 *
 *    The limit of this check, stated because the paragraph above reads
 *    absolute: it sees a drag only once the drag has spanned a *cell*. A press
 *    and release inside one character cell, or a drag walked back to where it
 *    started, leaves `finalSelectionEnd === finalSelectionStart`, so
 *    `hasSelection()` is false and the link opens. Nothing reached the
 *    clipboard in that case and the card showed the real origin first, so the
 *    cost is small — but it is the gap a mousedown/mouseup distance check
 *    would have closed, and it is the price of not keeping that second source
 *    of truth.
 *  - **A repeat click**, `detail > 1`. Belt to the above's braces: it holds
 *    even when the selection came out empty (a double-click on trailing
 *    whitespace selects nothing) and it does not depend on xterm having
 *    updated the selection model before the Linkifier's listener runs. It is
 *    `!== 1`, not `> 1`: a mouseup derived from a real click always carries
 *    `detail >= 1`, so `> 1` would have waved through anything synthesised
 *    with `detail` 0. Nothing in the container can dispatch a DOM event, so
 *    that is hardening rather than a hole being closed.
 *    Comparing mousedown and mouseup *coordinates* would be a third signal,
 *    but xterm hands this handler only the mouseup — the mousedown is not
 *    ours to see without binding our own listener to the host element, which
 *    is a second source of truth about the same gesture.
 *  - **A plain click while a modifier is required.** OSC 8 lets the container
 *    wrap any clickable TUI widget — a menu row, a "1. Yes", a file chip — in
 *    a link to anywhere, and because the mouse report still reaches the
 *    program the widget also responds, so nothing looks wrong. Requiring the
 *    force-selection modifier there makes the two intents distinguishable.
 *
 * `modifierPromised` is that last requirement made sticky, and it is about the
 * card rather than the click: the hint is rendered once, at hover, from a mode
 * the container may change before the user's finger comes down. The gate
 * honours the stricter of what the card promised and what is true now, so a
 * card reading "Shift+click to open" cannot be on screen while a bare click
 * opens the link.
 *
 * **What this does not close.** `mouseTracking` is a permission the attacker
 * grants itself — see `activate`.
 *
 * With nothing tracking the mouse and nothing promised, a bare click is
 * correct and expected: it is what `WebLinksAddon` does for the plain-text
 * URLs in the same buffer, which is why that handler applies this same gate.
 */
function opensOnClick(
  event: MouseEvent,
  ctx: ClickContext,
  modifierPromised = false,
): boolean {
  if (event.button !== 0) return false;
  if (event.detail !== 1) return false;
  if (ctx.hasSelection) return false;
  if (!ctx.mouseTracking && !modifierPromised) return true;
  return forcesSelection(event, ctx.macOptionClickForcesSelection);
}

/**
 * Makes OSC 8 hyperlinks clickable, and shows where they actually go.
 *
 * ## Why xterm's own link matching is not enough
 *
 * `WebLinksAddon` matches *rendered text*, row by row. Claude Code prints its
 * links as OSC 8 hyperlinks whose visible text is hard-wrapped into
 * terminal-width pieces — measured against 2.1.226, a 346-character sign-in
 * URL arrives as five emissions, each carrying the whole URL in its OSC 8
 * parameter and about 80 characters of it on screen (see `lib/urlDetector.ts`,
 * which had to grow the same second branch). So the addon matches a fragment
 * or nothing at all, which is the entire reason the URL toast exists. xterm
 * hands `linkHandler` the complete parameter instead, however the label was
 * sliced, so this covers exactly the case the addon cannot — and the addon
 * stays, because it covers the plain-text URLs in ordinary shell output that
 * carry no OSC 8 at all.
 *
 * ## xterm applies no gate of its own, so this one does
 *
 * There is a tempting story in which xterm's mouse-reporting mousedown cancels
 * the event before the link layer sees it, leaving only the force-selection
 * modifier a way through. It is false in both halves. That branch calls
 * `cancel(e)`, which is a no-op unless `cancelEvents` is set and it defaults to
 * false; and the mouse-reporting listeners are bound on `Terminal.element`
 * while the Linkifier is bound on `screenElement`, a descendant, so bubbling
 * reaches the link first no matter what. `Linkifier._handleMouseUp` then
 * activates the link with no check on the button, the modifier or the mouse
 * mode.
 *
 * So the gate is {@link opensOnClick}, applied in `activate`, and everything it
 * asks about is read from the terminal at the moment of the click rather than
 * captured — the container changes the mouse mode whenever it likes, and the
 * selection is whatever the gesture that ended in this mouseup left behind.
 *
 * ## What the mouse mode is worth, honestly
 *
 * Reading it fresh makes it *current*; it does not make it *trustworthy*. The
 * mode is set by the container, with a DECSET, and `?1002l` takes effect as
 * soon as xterm *parses* it (on its queued write task, not synchronously with
 * the container's output) — so a hostile container can drop tracking for
 * a few hundred milliseconds at a time and a plain click that lands in one of
 * those windows passes the mode half of the gate. It cannot time the user's
 * click, but it does not need to: a fraction of clicks is enough, and the only
 * tell is the status-bar badge flickering. This is a **known residual**, not
 * something this gate closes, and the freshness of the read must not be read
 * as an answer to it.
 *
 * Two things narrow it, neither of which depends on the mode. The selection
 * and click-count checks hold in either tracking state, so the gestures a user
 * makes to copy text are refused whatever the container has the mode set to —
 * which removes the "wrap every row in an OSC 8 and harvest the shift-drags"
 * version entirely. And the hover card's promise is sticky (see
 * `modifierPromised`): the flicker now has to cover the *hover* as well as the
 * click, because a card drawn while tracking was on goes on demanding the
 * modifier after the container drops it. What remains is a container that
 * drops tracking before the pointer arrives and holds it off until the click —
 * at which point the card also says "Click to open", so the user is at least
 * not being told one thing and given another. The real fix is a signal the
 * container cannot write, and there is none in this pane today.
 *
 * ## The hover card is the security half, not a nicety
 *
 * OSC 8 fully decouples the visible text from the target: the container can
 * print `https://claude.ai` and link it anywhere. That is strictly worse than
 * the userinfo spoofing `sanitizeRelayUrl` already rejects, because here
 * nothing in the painted row is even *derived* from the destination. So the
 * origin of the real target is shown before the user commits, the same way the
 * URL toast shows it and for the same reason ({@link urlOrigin}'s note): the
 * origin decides where the user's credentials end up, so it is rendered in
 * full and the *remainder* is the only part an ellipsis may eat.
 *
 * The card sits at the bottom of the pane rather than beside the pointer —
 * where a browser puts it, and never underneath the cursor, so it cannot
 * flicker the link out from under the hover that summoned it.
 *
 * @param getHost returns `Terminal.element`, which does not exist until
 *        `term.open()` has run — hence a getter rather than the element.
 * @param readState samples {@link ClickContext} — a getter for the same
 *        reason, and the *only* reason: every one of those answers changes
 *        under us, between the hover and the click that follows it.
 */
export function createOsc8LinkHandler(
  getHost: () => HTMLElement | null,
  readState: () => ClickContext,
): Osc8LinkHandler {
  let card: HTMLDivElement | null = null;
  /**
   * Did the card the user is looking at name a modifier?
   *
   * Written whenever a card is drawn, and cleared with it — `hover()` clears
   * and returns early when there is no host element, which leaves this false,
   * the stricter of the two directions. xterm only activates a link
   * it is currently hovering (`Linkifier._currentLink`), so there is always a
   * fresh hover behind a click — which is what makes this the promise the user
   * actually read, rather than a stale one. See `opensOnClick`.
   */
  let modifierPromised = false;

  const clear = () => {
    card?.remove();
    card = null;
    modifierPromised = false;
  };

  const span = (text: string, style: Partial<CSSStyleDeclaration>) => {
    const el = document.createElement("span");
    el.textContent = text;
    Object.assign(el.style, style);
    return el;
  };

  return {
    activate(event, text) {
      // Opening the host browser is the one thing in this pane the container
      // may not provoke on its own *and* the one thing no selection gesture
      // may provoke by accident. See `opensOnClick` — including the residual
      // it does not close.
      if (!opensOnClick(event, readState(), modifierPromised)) return;
      // Same sink and same rule as the WebLinksAddon branch: this came off the
      // container's output, so it is validated before it reaches the OS
      // opener. One implementation — `sanitizeRelayUrl` — on purpose.
      const safe = sanitizeRelayUrl(text);
      if (!safe) {
        console.warn("Refusing to open a link that failed validation");
        return;
      }
      openUrlExternal(safe).catch(reportOpenFailure);
    },

    hover(_event, text) {
      clear();
      const host = getHost();
      if (!host) return;
      const ctx = readState();
      // Sampled here and held, because this is what the card is about to tell
      // the user — and the gate has to honour it even if the container has
      // moved on by the time they click.
      modifierPromised = ctx.mouseTracking;

      card = document.createElement("div");
      card.className = OSC8_HOVER_CLASS;
      card.dataset.testid = "osc8-hover";
      Object.assign(card.style, {
        position: "absolute",
        left: "8px",
        bottom: "8px",
        maxWidth: "calc(100% - 16px)",
        boxSizing: "border-box",
        zIndex: "30",
        // The card lands under the pointer for a link in the bottom rows, and
        // it is not a sibling the Linkifier hit-tests around (see
        // `OSC8_HOVER_CLASS`). Without this, `screenElement` gets `mouseleave`
        // the moment the card appears — card removed, pointer back on the
        // link, card back: a flicker loop — and worse, the `mouseup` that
        // activates the link lands on the card, so the link cannot be opened
        // at all. Nothing here is interactive, so nothing is lost.
        pointerEvents: "none",
        display: "flex",
        alignItems: "baseline",
        gap: "6px",
        padding: "3px 8px",
        fontSize: "12px",
        fontFamily: "monospace",
        background: "var(--bg-secondary)",
        border: "1px solid var(--border-color)",
        // The origin wraps, so the card grows downward rather than sideways;
        // this is the backstop for anything that still cannot fit.
        overflow: "hidden",
        borderRadius: "6px",
        boxShadow: "var(--shadow-overlay)",
        color: "var(--text-primary)",
      } as Partial<CSSStyleDeclaration>);

      const safe = sanitizeRelayUrl(text);
      const origin = safe && urlOrigin(safe);
      if (!safe || !origin) {
        // Nothing of the rejected target is echoed into the DOM — it is
        // untrusted text, and the only useful thing to say is that the click
        // will not do anything. Deliberately not "it is not a web address":
        // `https://claude.ai@evil.tld/` and an over-length URL both are one,
        // and a card that explains a refusal wrongly teaches the user to
        // distrust the card.
        card.appendChild(
          span("This link will not be opened — it failed the URL safety check", {
            color: "var(--text-secondary)",
          }),
        );
      } else {
        const rest = safe.startsWith(origin) ? safe.slice(origin.length) : safe;
        const originEl = span(origin, {
          fontWeight: "700",
          // The part that decides where the credentials go, so all of it is
          // shown: truncating it *is* the spoof, and so is pushing its tail
          // off the right edge of the pane. The attacker picks the length —
          // `https://claude.ai.<300 chars>.evil.tld` parses and passes every
          // `sanitizeRelayUrl` rule — so "do not shrink" is not enough:
          // `flex-shrink: 0` pins a flex item at its max-content width and the
          // text never wraps, it just overflows. It wraps instead, onto as
          // many lines as it needs, and the truncatable remainder below is the
          // thing that gives way.
          flexShrink: "1",
          minWidth: "0",
          overflowWrap: "anywhere",
          whiteSpace: "normal",
        });
        originEl.dataset.testid = "osc8-hover-origin";
        card.appendChild(originEl);

        const restEl = span(rest, {
          color: "var(--text-secondary)",
          overflow: "hidden",
          textOverflow: "ellipsis",
          whiteSpace: "nowrap",
          minWidth: "0",
        });
        restEl.dataset.testid = "osc8-hover-rest";
        card.appendChild(restEl);

        const hint = span(openHintLabel(ctx), {
          color: "var(--text-secondary)",
          flexShrink: "0",
          marginLeft: "4px",
        });
        card.appendChild(hint);
      }

      host.appendChild(card);
    },

    leave: clear,

    dismiss: clear,
  };
}

export default function TerminalView({ sessionId, active }: Props) {
  const containerRef = useRef<HTMLDivElement>(null);
  const terminalContainerRef = useRef<HTMLDivElement>(null);
  const termRef = useRef<Terminal | null>(null);
  const fitRef = useRef<FitAddon | null>(null);
  const webglRef = useRef<WebglAddon | null>(null);
  // Held only so the hover card can be taken down when this pane leaves the
  // screen — see `Osc8LinkHandler.dismiss`.
  const osc8LinkHandlerRef = useRef<Osc8LinkHandler | null>(null);
  const detectorRef = useRef<UrlDetector | null>(null);
  const { sendInput, pasteImage, resize, onOutput, onExit } = useTerminal();
  const gpuRenderingSetting = useAppState(s => s.appSettings?.terminal_gpu_rendering ?? null);
  const setTerminalHasSelection = useAppState(s => s.setTerminalHasSelection);
  const setTerminalMouseCaptured = useAppState(s => s.setTerminalMouseCaptured);
  const setReleaseActiveMouse = useAppState(s => s.setReleaseActiveMouse);

  const ssoBufferRef = useRef("");
  const ssoTriggeredRef = useRef(false);
  const projectId = useAppState(
    (s) => s.sessions.find((sess) => sess.id === sessionId)?.projectId
  );

  // Which program is on the other end of the PTY. Read through a ref because
  // the key handler is registered once, in the mount effect keyed on
  // `sessionId`, and a value captured there would go stale if the session
  // record arrived after the first render.
  const sessionType = useAppState(
    (s) => s.sessions.find((sess) => sess.id === sessionId)?.sessionType
  );
  const sessionTypeRef = useRef(sessionType);
  sessionTypeRef.current = sessionType;

  // One toast slot, three producers: the container's explicit "open this in the
  // host browser" relay (OSC 7777), OSC 8 hyperlink targets, and the heuristic
  // long-URL detector. Sharing the slot keeps them from stacking on top of each
  // other.
  //
  // All three read the container's PTY output, so all three are untrusted, and
  // all three must go through `sanitizeRelayUrl` before anything is stored here
  // — see `promptUrl` below, which is the only writer.
  //
  // `seq` exists because the slot is shared and long-lived: a second prompt
  // replacing a first would otherwise mutate the toast in place, swapping the
  // text under a user who is mid-read and mid-click. Keying the toast on it
  // remounts the component, so a new URL is unmistakably a new prompt.
  const [urlPrompt, setUrlPrompt] = useState<UrlPrompt | null>(null);
  const promptSeqRef = useRef(0);
  const relayLimiterRef = useRef(new RelayRateLimiter());
  /**
   * A mirror of the prompt slot, written *eagerly* by the two functions that
   * change it.
   *
   * Read by the long-lived keyboard listener below, which is registered once
   * and would otherwise close over the prompt as it was at mount — and by
   * {@link dismissUrlPromptIfCurrent}, which is the reason it is written on the
   * spot rather than from an effect. An effect-synced mirror lags the state it
   * mirrors by a commit, and the whole question that identity check answers is
   * "did a new prompt land while I was awaiting?" — a mirror that has not
   * caught up yet answers it wrong in exactly the window that matters.
   */
  const urlPromptRef = useRef<UrlPrompt | null>(null);

  /**
   * Empty the prompt slot, and put focus somewhere real if it was inside the
   * toast.
   *
   * The toast never *takes* focus — see the note in `UrlToast` — but a keyboard
   * user who jumped into it with {@link URL_TOAST_SHORTCUT} is standing on a
   * node that is about to unmount, and React does not rehome focus: it lands on
   * `document.body`, where the terminal receives nothing and the next keystroke
   * goes nowhere. Every route out of the toast goes through here for that
   * reason — Open, In container, ✕, Escape and the auto-dismiss alike.
   */
  const dismissUrlPrompt = useCallback(() => {
    const wasInside = !!document.activeElement?.closest(URL_TOAST_SELECTOR);
    urlPromptRef.current = null;
    setUrlPrompt(null);
    if (wasInside) termRef.current?.focus();
  }, []);

  /**
   * Dismiss, but only if the slot is still holding the prompt the caller
   * acted on.
   *
   * For anything that dismisses *after* awaiting. `openUrlExternal` takes at
   * least `OPENER_GRACE` (400 ms, doubled when `xdg-open` fails and `gio` is
   * tried) on Linux by construction, and the container can relay a second,
   * superseding URL inside that window — at which point the slot has been
   * remounted with prompt B and an unconditional `setUrlPrompt(null)` blanks
   * it. The user never sees B, and B exists nowhere but the container's
   * transcript, which is the exact failure "dismiss on success only" was
   * introduced to prevent.
   *
   * This is a sibling of {@link dismissUrlPrompt} rather than an optional
   * `expectedSeq` parameter on it, because `dismissUrlPrompt` is handed
   * straight to `onClick`/`onDismiss`: React would call it with a `MouseEvent`
   * as its first argument, that event would land in `expectedSeq`, and the ✕
   * button would silently stop dismissing anything. A parameter that is only
   * ever correct when nobody passes it by reference is not a safe signature
   * here.
   */
  const dismissUrlPromptIfCurrent = useCallback(
    (seq: number) => {
      if (urlPromptRef.current?.seq !== seq) return;
      dismissUrlPrompt();
    },
    [dismissUrlPrompt],
  );

  /**
   * The only writer of the prompt slot. Re-validates whatever the caller
   * found: the OSC relay branch has already been through `parseUrlRelayOsc`,
   * but the heuristic detector branch has been through nothing at all, and a
   * raw regex match is exactly the input `sanitizeRelayUrl` exists to refuse.
   *
   * Last-writer-wins is what this used to be, and it lost the OAuth URL every
   * time: the relay delivers the link base64-encoded and therefore exact, and
   * ~300 ms later the screen-scraper's debounce fired and overwrote it with a
   * truncated guess at the same link. `supersedes` is the fix — see there.
   */
  const promptUrl = useCallback(
    (raw: string, label: string, source: PromptSource) => {
      const url = sanitizeRelayUrl(raw);
      if (!url) {
        console.warn("Refusing to prompt for a URL that failed validation");
        return;
      }
      // Read and written through the ref rather than a functional update, so
      // the mirror is current the instant this returns. Two prompts arriving in
      // one tick still see each other — that is what the ref being the eager
      // copy buys — and the seq counter no longer advances inside a state
      // updater, which React is free to run twice.
      if (!supersedes({ url, source }, urlPromptRef.current)) return;
      promptSeqRef.current += 1;
      const next: UrlPrompt = { url, label, source, seq: promptSeqRef.current };
      urlPromptRef.current = next;
      setUrlPrompt(next);
    },
    [],
  );

  /**
   * The keyboard route into the toast.
   *
   * Registered on `document` in the capture phase for the same reason
   * `useKeyboardShortcuts` does it there: xterm would otherwise forward the
   * chord to the shell. It is *not* added to that hook because the target is
   * this pane's own toast — the hook has no way to name it, and only one pane
   * is on screen at a time, which is what `activeRef` checks.
   *
   * Nothing is swallowed unless there is a prompt to jump to, so Ctrl+Shift+O
   * reaches the terminal untouched the rest of the time.
   */
  useEffect(() => {
    const onKeyDown = (e: KeyboardEvent) => {
      if (!e.ctrlKey || !e.shiftKey || e.altKey || e.metaKey) return;
      if (e.key !== "o" && e.key !== "O") return;
      if (!activeRef.current || !urlPromptRef.current) return;
      const primary = terminalContainerRef.current?.querySelector<HTMLElement>(
        `${URL_TOAST_SELECTOR} ${URL_TOAST_PRIMARY_SELECTOR}`,
      );
      if (!primary) return;
      e.preventDefault();
      e.stopPropagation();
      primary.focus();
    };
    document.addEventListener("keydown", onKeyDown, true);
    return () => document.removeEventListener("keydown", onKeyDown, true);
  }, []);
  const [imagePasteMsg, setImagePasteMsg] = useState<string | null>(null);
  const [contextMenu, setContextMenu] = useState<{ x: number; y: number } | null>(null);
  // True while the program in the container holds mouse reporting open (any of
  // the DECSET ?1000/?1002/?1003 tracking modes). See `syncMouseCapture`.
  const [mouseCaptured, setMouseCaptured] = useState(false);
  const mouseCapturedRef = useRef(false);

  // Keep latest `active` readable inside long-lived listeners (drag-drop below,
  // and the unmount-cleanup effect further down).
  const activeRef = useRef(active);
  activeRef.current = active;

  // File drag-and-drop: dropped files are copied into the container and their
  // in-container paths typed into the prompt so Claude Code can read them.
  // Tauri intercepts OS file drops at the webview level, so we use
  // onDragDropEvent (HTML5 ondrop on the element wouldn't expose file paths).
  //
  // The listener is window-wide, so every pane decides for itself whether a
  // drop was meant for it. `classifyDrop` is that decision, shared with the
  // Files pane, and it asks two things in order: is the payload position
  // inside this pane's rect (a hidden pane is `display:none`, so its zero-size
  // rect is what stops two panes both claiming the drop), and — document-wide,
  // with no geometry — is a modal or blocking overlay on screen at all? An
  // open `Modal` is a `fixed inset-0` portal painted *over* the window and the
  // pane underneath still has its rect, so a rect alone uploaded files into
  // the directory a dialog was covering. See `lib/dropTarget.ts` for why the
  // blocking half is deliberately not a per-point z-order test.
  //
  // The rect asked about is the **pane wrapper**, not the xterm host inside it:
  // the pane is what the user sees as "the terminal", gutter included, and the
  // chrome painted over it (the mouse-release badge, the URL toast) is a
  // sibling of the host rather than a child. Nothing painted over the pane
  // refuses a drop on its own account — asking "is this element mine?" once
  // turned every pixel under that chrome into a permanent dead zone.
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    let cancelled = false;

    // Always single-quote: a dropped filename can contain shell metacharacters
    // ($(), &&, ', spaces) even with no whitespace, and this path is typed into
    // a live shell. Single-quoting with '\'' escaping neutralizes all of them.
    const quote = (p: string) => `'${p.replace(/'/g, "'\\''")}'`;

    (async () => {
      const un = await getCurrentWebview().onDragDropEvent(async (event) => {
        if (event.payload.type !== "drop") return;
        const verdict = classifyDrop(
          terminalContainerRef.current,
          event.payload.position,
        );
        // A refused drop is invisible — the file simply does not arrive — so
        // the one case where the user aimed at us and we said no gets both a
        // log line and something on screen. The toast, not `imagePasteMsg`:
        // whatever refused this is painted over the terminal, and `ToastHost`
        // sits above it.
        if (verdict === "blocked") {
          console.warn(
            "[drop] refused: a dialog or overlay is open",
            event.payload.position,
          );
          useAppState.getState().pushToast(DROP_BLOCKED_TOAST);
          return;
        }
        if (verdict !== "accept") return;

        const paths = event.payload.paths ?? [];
        if (paths.length === 0) return;

        setImagePasteMsg(`Adding ${paths.length} file${paths.length > 1 ? "s" : ""}…`);
        const containerPaths: string[] = [];
        for (const p of paths) {
          try {
            containerPaths.push(await uploadHostFileToTerminal(sessionId, p));
          } catch (err) {
            console.error("File drop upload failed for", p, err);
          }
        }
        if (containerPaths.length === 0) {
          setImagePasteMsg("File drop failed");
          return;
        }
        sendInput(sessionId, containerPaths.map(quote).join(" ") + " ");
        setImagePasteMsg(`Added ${containerPaths.length} file path${containerPaths.length > 1 ? "s" : ""}`);
      });
      if (cancelled) un();
      else unlisten = un;
    })();

    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, [sessionId, sendInput]);

  /**
   * Reconcile the badge with xterm's live mouse-tracking mode.
   *
   * There is no event for this, but there does not need to be a poll either:
   * the mode only ever changes because the container printed a DECSET/DECRST
   * sequence, so checking once per write covers every transition, exactly when
   * it happens. The ref gate keeps the common case (mode unchanged, thousands
   * of writes a second) down to one string comparison and no re-render.
   */
  const syncMouseCapture = useCallback(() => {
    const term = termRef.current;
    if (!term) return;
    const captured = terminalTracksMouse(term);
    if (captured === mouseCapturedRef.current) return;
    mouseCapturedRef.current = captured;
    setMouseCaptured(captured);
  }, []);

  /**
   * Take the mouse back from a program that grabbed it and never let go.
   *
   * A TUI that dies mid-menu (or is killed, or detaches) leaves its mouse
   * tracking modes set. xterm goes on routing clicks, drags and — under
   * `?1003` — every pointer *move* to the PTY, which kills text selection and
   * floods the prompt with escape bytes. The result reads as a frozen
   * terminal, and until now the only exit was closing the tab.
   *
   * The reset is `term.write`, deliberately, not `sendInput`: it goes into
   * xterm's own parser and never onto the wire. The program that asked for
   * tracking is usually already gone; if it is not, telling it the user pulled
   * the mouse back would only invite it to grab again on its next repaint.
   */
  const releaseMouse = useCallback(() => {
    const term = termRef.current;
    if (!term) return;
    // The three tracking modes, then the two encodings they report in. All
    // five, because a program is free to have set any combination and a
    // leftover encoding mode outlives the tracking mode that motivated it.
    term.write("\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1006l\x1b[?1015l", syncMouseCapture);
  }, [syncMouseCapture]);

  useEffect(() => {
    if (!containerRef.current) return;

    // Annotated because `linkHandler` below refers to `term` (for the element
    // it must anchor its hover card to, which does not exist until
    // `term.open()`), and TypeScript cannot infer a type it is already using.
    const term: Terminal = new Terminal({
      cursorBlink: true,
      fontSize: 14,
      // Let the user select text even while a program holds the mouse.
      // xterm's force-selection modifier is Shift everywhere *except* macOS,
      // where it is Option and is gated behind this option, which defaults to
      // false — so without this line Mac users have no force-select at all and
      // the only way to copy from a mouse-driven TUI is to take the mouse back
      // first. `SelectionService.shouldForceSelection`.
      macOptionClickForcesSelection: true,
      // OSC 8 hyperlinks — the form Claude Code prints its links in, and the
      // one `WebLinksAddon` structurally cannot match. See
      // `createOsc8LinkHandler`, including why opening one while a program
      // holds the mouse needs the same Shift/Option the line above is about.
      // Both arguments are getters because neither answer exists yet: the
      // element arrives with `term.open()`, and the mouse mode changes
      // whenever the container prints a DECSET.
      linkHandler: (osc8LinkHandlerRef.current = createOsc8LinkHandler(
        () => term.element ?? null,
        () => readClickContext(term),
      )),
      fontFamily: "'JetBrains Mono', 'Fira Code', 'Cascadia Code', Menlo, Monaco, monospace",
      theme: {
        background: "#0d1117",
        foreground: "#e6edf3",
        cursor: "#58a6ff",
        selectionBackground: "#264f78",
        black: "#484f58",
        red: "#ff7b72",
        green: "#3fb950",
        yellow: "#d29922",
        blue: "#58a6ff",
        magenta: "#bc8cff",
        cyan: "#39d353",
        white: "#b1bac4",
        brightBlack: "#6e7681",
        brightRed: "#ffa198",
        brightGreen: "#56d364",
        brightYellow: "#e3b341",
        brightBlue: "#79c0ff",
        brightMagenta: "#d2a8ff",
        brightCyan: "#56d364",
        brightWhite: "#f0f6fc",
      },
    });

    const fitAddon = new FitAddon();
    term.loadAddon(fitAddon);

    // Web links addon — opens URLs in host browser via Tauri, with a permissive regex
    // that matches URLs even if they lack trailing path segments (the default regex
    // misses OAuth URLs that end mid-line).
    // eslint-disable-next-line no-control-regex
    const urlRegex = /https?:\/\/[^\s'"`<>\x00-\x20\x7f]+/;
    const webLinksAddon = new WebLinksAddon((event, uri) => {
      // Same gate and same sink as `createOsc8LinkHandler`, because this
      // reaches the same `openUrlExternal` through the same
      // `Linkifier._handleMouseUp`, which checks nothing here either. Without
      // it a container that prints a plausible-looking `https://` row in a TUI
      // got a browser open on a plain click while it held the mouse, and a
      // double-click that merely selected a URL opened it.
      //
      // It is also the only gate on a real bypass of the OSC 8 one:
      // `OscLinkProvider` drops a hyperlink whose target is not http(s)
      // *before* `linkHandler` sees it (`allowNonHttpProtocols` is unset), so
      // an OSC 8 carrying a `javascript:` target and an `https://evil.tld/x`
      // label leaves the addon free to match the label. That click arrives
      // here and nowhere else.
      //
      // No `modifierPromised`: this path paints an underline rather than a
      // card, so it promises the user nothing to be held to.
      //
      // This branch is the one where the click really is an act on visible
      // text — the match *is* the painted characters — so the spoof it has to
      // survive is a userinfo-spoofed URL, which `sanitizeRelayUrl` rejects.
      // An OSC 8 link is not like that at all: its label and its target are
      // unrelated strings, which is why that handler shows the target's origin
      // on hover before a click can happen. Neither branch replaces the other:
      // this one covers plain-text URLs in ordinary shell output, which carry
      // no hyperlink parameter for xterm to hand over.
      if (!opensOnClick(event, readClickContext(term))) return;
      const safe = sanitizeRelayUrl(uri);
      if (!safe) {
        console.warn("Refusing to open a link that failed validation");
        return;
      }
      openUrlExternal(safe).catch(reportOpenFailure);
    }, { urlRegex });
    term.loadAddon(webLinksAddon);

    term.open(containerRef.current);

    // Ctrl+Shift+C copies the selection with whitespace trimmed (UI padding
    // stripped, internal indentation preserved). Ctrl+Shift+Alt+C copies raw.
    // Both prevent the keystroke from reaching the container (where Ctrl+C
    // would send SIGINT and cancel running work).
    term.attachCustomKeyEventHandler((event) => {
      if (event.type === "keydown" && event.ctrlKey && event.shiftKey && event.key === "C") {
        const sel = term.getSelection();
        if (sel) {
          const out = event.altKey ? sel : trimSelection(sel);
          navigator.clipboard.writeText(out).catch((e) =>
            console.error("Ctrl+Shift+C clipboard write failed:", e),
          );
        }
        return false; // prevent xterm from processing this key
      }
      // Ctrl+Shift+M toggles speech-to-text recording (mic lives in the status
      // bar, bound to the active session; trigger it via the store).
      if (event.type === "keydown" && event.ctrlKey && event.shiftKey && event.key === "M") {
        useAppState.getState().sttToggle();
        return false;
      }
      // Ctrl+Shift+X hands the mouse back. Same action as the badge, bound to
      // a key because the failure this recovers from is *the pointer not
      // working* — a control you have to click can be unreachable in exactly
      // the situation that calls for it.
      if (event.type === "keydown" && event.ctrlKey && event.shiftKey && event.key === "X") {
        releaseMouse();
        return false;
      }
      // Shift+Enter inserts a newline in Claude Code's prompt instead of
      // submitting it. xterm.js does not consult `shiftKey` for Enter
      // (`Keyboard.ts`, `case 13`), so without this branch Shift+Enter is
      // byte-identical to Enter and submits.
      //
      // `\x1b\r` — ESC then CR — is what Claude Code parses as `return` with
      // meta, and it is exactly what its own `/terminal-setup` writes into the
      // VS Code, Cursor, Alacritty and Zed keymaps. These are the in-band
      // bytes, not a guess, which is why this must NOT be "simplified" to
      // `\n`: Claude Code accepts `\n` too, but a shell would *run* the line,
      // so the two session types would quietly diverge.
      //
      // Scoped to Claude sessions for the same reason. A bash tab runs
      // `bash -l`, where readline has no binding for `\e\r` and answers with a
      // bell — harmless, but there is nothing to gain from sending it.
      if (
        event.type === "keydown" &&
        event.key === "Enter" &&
        event.shiftKey &&
        !event.ctrlKey &&
        !event.altKey &&
        !event.metaKey &&
        !event.isComposing &&
        sessionTypeRef.current === "claude"
      ) {
        sendInput(sessionId, CLAUDE_SOFT_NEWLINE);
        // **`preventDefault()` is what stops the submit, not the `return false`.**
        //
        // xterm's `_keyDown` returns the instant a custom handler says `false`
        // — *before* it sets `_keyDownHandled` and before it cancels the event.
        // `_keyPress` then checks that same flag, finds it still false, and
        // emits a bare CR for Enter's charCode 13. So returning `false` alone
        // sent ESC+CR *and* a submit: the newline was inserted and the
        // half-written prompt went to Claude with a stray blank line in it.
        // Cancelling the keydown is what stops the browser firing keypress at
        // all. Verified in Chromium; jsdom never synthesizes the follow-up
        // keypress, which is why the unit test could not see this.
        event.preventDefault();
        return false;
      }
      return true;
    });

    // WebGL addon is loaded/disposed dynamically in the active effect
    // to avoid exhausting the browser's limited WebGL context pool.

    fitAddon.fit();
    termRef.current = term;
    fitRef.current = fitAddon;

    // Send initial size
    resize(sessionId, term.cols, term.rows);

    // Handle OSC 52 clipboard write sequences from programs inside the container.
    // When a program (e.g. Claude Code) copies text via xclip/xsel/pbcopy, the
    // container's shim emits an OSC 52 escape sequence which xterm.js routes here.
    const osc52Disposable = term.parser.registerOscHandler(52, (data) => {
      const idx = data.indexOf(";");
      if (idx === -1) return false;
      const payload = data.substring(idx + 1);
      if (payload === "?") return false; // clipboard read request, not supported
      try {
        const decoded = atob(payload);
        navigator.clipboard.writeText(decoded).catch((e) =>
          console.error("OSC 52 clipboard write failed:", e),
        );
      } catch (e) {
        console.error("OSC 52 decode failed:", e);
      }
      return true;
    });

    // URL relay (OSC 7777) — a CLI inside the container asked for a URL to be
    // opened in a browser. The container has none; `triple-c-open` (installed
    // as xdg-open / $BROWSER / sensible-browser / ...) forwards the request
    // here instead.
    //
    // The container is untrusted, so this never opens anything by itself:
    // parseUrlRelayOsc enforces the http/https allowlist and the payload is
    // rate-limited, then the user gets the same confirmation toast the
    // long-URL detector uses. One click is a small price for not handing a
    // sandboxed agent a "make the host's logged-in browser fetch this"
    // primitive.
    const relayDisposable = term.parser.registerOscHandler(URL_RELAY_OSC, (data) => {
      const url = parseUrlRelayOsc(data);
      if (!url) {
        console.warn("URL relay: rejected request from container");
        return true; // consumed either way — never let it reach the screen
      }
      if (!relayLimiterRef.current.allow(url)) {
        console.warn("URL relay: rate-limited", url);
        return true;
      }
      // Exact by construction (base64 over OSC 7777), and the detector never
      // sees it — so tell it, or a truncated scrape of the same link could
      // still fill the slot once this prompt is dismissed.
      detectorRef.current?.noteExactUrl(url);
      promptUrl(url, "Container asked to open a URL", "relay");
      return true;
    });

    // Handle user input -> backend
    const inputDisposable = term.onData((data) => {
      // Ordered and coalesced by the queue in `useTerminal`; a rejection here
      // means the session is gone, which the exit listener already reports.
      sendInput(sessionId, data).catch((e) =>
        console.error("Failed to send terminal input:", e)
      );
    });

    // Track text selection to show copy hint in status bar
    const selectionDisposable = term.onSelectionChange(() => {
      setTerminalHasSelection(term.hasSelection());
    });

    // Handle image paste: intercept paste events with image data,
    // upload to the container, and inject the file path into terminal input.
    const handlePaste = (e: ClipboardEvent) => {
      const items = e.clipboardData?.items;
      if (!items) return;

      for (const item of Array.from(items)) {
        if (item.type.startsWith("image/")) {
          e.preventDefault();
          e.stopPropagation();

          const blob = item.getAsFile();
          if (!blob) return;

          blob.arrayBuffer().then(async (buf) => {
            try {
              setImagePasteMsg("Uploading image...");
              const data = new Uint8Array(buf);
              const filePath = await pasteImage(sessionId, data);
              // Inject the file path into terminal stdin
              sendInput(sessionId, filePath);
              setImagePasteMsg(`Image saved to ${filePath}`);
            } catch (err) {
              console.error("Image paste failed:", err);
              setImagePasteMsg("Image paste failed");
            }
          });
          return; // Only handle the first image
        }
      }
    };

    containerRef.current.addEventListener("paste", handlePaste, { capture: true });

    // Handle backend output -> terminal
    let aborted = false;

    // The detector samples this getter on every `feed`, so what it reassembles
    // with is the width the bytes were *printed* at — only a break the terminal
    // itself inserted may be deleted, and where that is moves with every
    // resize.
    const detector = new UrlDetector(
      (url, source) =>
        promptUrl(
          url,
          source === "osc8" ? "Link detected" : "Long URL detected",
          source,
        ),
      () => termRef.current?.cols ?? 0,
    );
    detectorRef.current = detector;

    const SSO_MARKER = "###TRIPLE_C_SSO_REFRESH###";
    const textDecoder = new TextDecoder();

    const outputPromise = onOutput(sessionId, (data) => {
      if (aborted) return;
      // Scrolling on new output is xterm's own job, and it already gets it
      // right: it follows the tail while the viewport is at the bottom and
      // holds position while you are reading further up. The manual
      // `scrollToBottom()` that used to live here fought that second half.
      term.write(data, syncMouseCapture);
      detector.feed(data);

      // Scan for SSO refresh marker in terminal output
      if (!ssoTriggeredRef.current && projectId) {
        const text = textDecoder.decode(data, { stream: true });
        // Combine with overlap from previous chunk to handle marker spanning chunks
        const combined = ssoBufferRef.current + text;
        if (combined.includes(SSO_MARKER)) {
          ssoTriggeredRef.current = true;
          ssoBufferRef.current = "";
          awsSsoRefresh(projectId).catch((e) =>
            console.error("AWS SSO refresh failed:", e)
          );
        } else {
          // Keep last N chars as overlap for next chunk
          ssoBufferRef.current = combined.slice(-SSO_MARKER.length);
        }
      }
    }).then((unlisten) => {
      if (aborted) unlisten();
      return unlisten;
    });

    const exitPromise = onExit(sessionId, () => {
      if (aborted) return;
      term.write("\r\n\x1b[33m[Session ended]\x1b[0m\r\n");
    }).then((unlisten) => {
      if (aborted) unlisten();
      return unlisten;
    });

    // Handle resize (throttled via requestAnimationFrame to avoid excessive calls).
    // Skip resize work for hidden terminals — containerRef will have 0 dimensions.
    let resizeRafId: number | null = null;
    const resizeObserver = new ResizeObserver(() => {
      if (resizeRafId !== null) return;
      const el = containerRef.current;
      if (!el || el.offsetWidth === 0 || el.offsetHeight === 0) return;
      resizeRafId = requestAnimationFrame(() => {
        resizeRafId = null;
        if (!containerRef.current || containerRef.current.offsetWidth === 0) return;
        // Whether the viewport was following the tail has to be sampled
        // *before* the fit: reflowing wrapped lines moves `baseY`, so asking
        // afterwards cannot tell "was at the bottom" from "was pushed off it".
        const wasAtBottom =
          term.buffer.active.viewportY >= term.buffer.active.baseY;
        fitAddon.fit();
        resize(sessionId, term.cols, term.rows);
        // Only re-anchor a viewport that was already on the tail. This
        // observer fires for any pane size change — opening the Notes dock,
        // dragging the sidebar, resizing the window — and none of those are a
        // reason to yank someone away from the scrollback they are reading.
        if (wasAtBottom) term.scrollToBottom();
      });
    });
    resizeObserver.observe(containerRef.current);

    return () => {
      aborted = true;
      detector.dispose();
      detectorRef.current = null;
      ssoTriggeredRef.current = false;
      ssoBufferRef.current = "";
      osc52Disposable.dispose();
      relayDisposable.dispose();
      inputDisposable.dispose();
      selectionDisposable.dispose();
      setTerminalHasSelection(false);
      containerRef.current?.removeEventListener("paste", handlePaste, { capture: true });
      outputPromise.then((fn) => fn?.());
      exitPromise.then((fn) => fn?.());
      if (resizeRafId !== null) cancelAnimationFrame(resizeRafId);
      resizeObserver.disconnect();
      try { webglRef.current?.dispose(); } catch { /* may already be disposed */ }
      webglRef.current = null;
      term.dispose();
      termRef.current = null;
      osc8LinkHandlerRef.current = null;
    };
  }, [sessionId]); // eslint-disable-line react-hooks/exhaustive-deps

  // A hover card only ever clears on a *pointer* leaving the link, so switching
  // tabs from the keyboard leaves one hanging over a pane nobody is looking at,
  // to be found still there on the way back. Hiding the wrapper does not fire
  // `mouseleave`, so nothing else would.
  useEffect(() => {
    if (active) return;
    osc8LinkHandlerRef.current?.dismiss();
  }, [active]);

  // Manage WebGL lifecycle and re-fit when tab becomes active.
  // Only the active terminal holds a WebGL context to avoid exhausting
  // the browser's limited pool (~8-16 contexts).
  useEffect(() => {
    const term = termRef.current;
    if (!term) return;

    // Auto on macOS/Windows, off on Linux, overridable either way — see
    // `resolveTerminalGpuRendering`. Loading the addon under a software-GL
    // WebKitGTK is slower than xterm's canvas renderer, not faster.
    const useGpu = resolveTerminalGpuRendering(gpuRenderingSetting, navigator.userAgent);

    // The renderer and the activation work are independent: a terminal with
    // GPU rendering switched off still has to fit and take focus when its tab
    // becomes active. Keeping these in one branch made "GPU off" silently mean
    // "never re-fit, never focus".
    if (active && useGpu) {
      // Attach WebGL renderer
      if (!webglRef.current) {
        try {
          const addon = new WebglAddon();
          addon.onContextLoss(() => {
            try { addon.dispose(); } catch { /* ignore */ }
            webglRef.current = null;
          });
          term.loadAddon(addon);
          webglRef.current = addon;
        } catch {
          // WebGL not available, canvas renderer is fine
        }
      }
    } else if (webglRef.current) {
      // Release the context — for inactive terminals, and when the setting
      // turns GPU rendering off while this terminal is on screen.
      try { webglRef.current.dispose(); } catch { /* ignore */ }
      webglRef.current = null;
    }

    if (active) {
      // Same rule as the resize observer: re-anchor only what was already
      // anchored, so a tab left scrolled up comes back where it was left.
      const wasAtBottom =
        term.buffer.active.viewportY >= term.buffer.active.baseY;
      fitRef.current?.fit();
      if (wasAtBottom) term.scrollToBottom();
      term.focus();
    }
  }, [active, gpuRenderingSetting]);

  // Focus on demand, for the caller that cannot rely on the effect above.
  // That one keys off `active`, so it covers switching *to* a terminal and
  // nothing else — and the notes dock sends to the terminal already on screen,
  // where `active` never changes. Consumed once and cleared, so asking twice
  // for the same terminal works.
  const pendingTerminalFocus = useAppState((s) => s.pendingTerminalFocus);
  const clearPendingTerminalFocus = useAppState(
    (s) => s.clearPendingTerminalFocus,
  );
  useEffect(() => {
    if (pendingTerminalFocus !== sessionId) return;
    termRef.current?.focus();
    clearPendingTerminalFocus();
  }, [pendingTerminalFocus, sessionId, clearPendingTerminalFocus]);

  // Auto-dismiss toast after 30 seconds — unless the user is standing in it.
  // A keyboard user who has just jumped into the toast is mid-decision, and
  // pulling it out from under them costs them the only route to finishing a
  // sign-in. It goes when they act on it, which is the same thing a mouse user
  // does by clicking.
  useEffect(() => {
    if (!urlPrompt) return;
    const timer = setTimeout(() => {
      if (document.activeElement?.closest(URL_TOAST_SELECTOR)) return;
      dismissUrlPrompt();
    }, 30_000);
    return () => clearTimeout(timer);
  }, [urlPrompt, dismissUrlPrompt]);

  // Auto-dismiss image paste message after 3 seconds
  useEffect(() => {
    if (!imagePasteMsg) return;
    const timer = setTimeout(() => setImagePasteMsg(null), 3_000);
    return () => clearTimeout(timer);
  }, [imagePasteMsg]);

  /**
   * Hand the prompted URL to the host's browser.
   *
   * Two things here are ordering, not decoration:
   *
   *  - **The toast is dismissed on success only, and only if it is still the
   *    same toast.** Dismissing first is what this replaced: a failed open left
   *    the user with an empty screen and no way back to a URL that only exists
   *    in the container's transcript. Now a failure keeps the prompt exactly
   *    where it was, which also leaves "In container" one click away — the
   *    fallback this failure is the argument for. Waiting to dismiss opens a
   *    second window, though: the open is awaited, the container can relay a
   *    superseding URL while it is in flight, and blanking the slot on success
   *    would then throw away a prompt the user has never seen. Hence the seq
   *    check in `dismissUrlPromptIfCurrent` rather than a bare dismissal.
   *  - **The failure is a toast, not a `console.error`.** Same `pushToast` the
   *    container-browser branch below uses, because from the user's side the
   *    two actions fail identically: nothing happens.
   *
   * What this does *not* cover, and must not be described as covering: on Linux
   * `xdg-open` routinely exits 0 having done nothing useful, so the most common
   * Linux failure resolves this promise and reports success. Stripping the
   * leaked AppImage environment before the browser is spawned is what addresses
   * that; this is the complement that catches everything which does report.
   */
  const handleOpenUrl = useCallback(() => {
    if (!urlPrompt) return;
    // Validated again at the sink. `promptUrl` is the only writer and already
    // sanitizes, so this can only fail if that invariant is broken — which is
    // precisely when it matters that the last thing before the opener checks.
    const safe = sanitizeRelayUrl(urlPrompt.url);
    if (!safe) {
      console.warn("Refusing to open a URL that failed validation");
      dismissUrlPrompt();
      return;
    }
    // The prompt this click was for. Captured before the await, because the
    // slot may be holding a different one by the time the opener answers.
    const openedSeq = urlPrompt.seq;
    openUrlExternal(safe)
      .then(() => dismissUrlPromptIfCurrent(openedSeq))
      .catch((e) =>
        useAppState.getState().pushToast({
          kind: "error",
          message: "Could not open it in your browser",
          detail: String(e),
          dedupeKey: "host-open-failed",
        }),
      );
  }, [urlPrompt, dismissUrlPrompt, dismissUrlPromptIfCurrent]);

  /**
   * Which action leads when the prompt is holding an Anthropic sign-in link.
   *
   * Resolved per project, not per URL — see `useSignInOpenTarget`. The toast
   * offers both regardless; this is only which one is filled in and reachable
   * with {@link URL_TOAST_SHORTCUT}.
   */
  const signInDefault = useSignInOpenTarget(projectId);

  /**
   * Open the prompted URL in the container's own browser instead of the host's.
   *
   * For a sign-in this is the shorter path: the callback listener the tool is
   * waiting on is inside the container, so a container-side browser closes the
   * loop with nothing crossing to the host. The page is published to the
   * project's Browser tab, which is where the user completes it by hand.
   */
  const handleOpenUrlInContainer = useCallback(() => {
    if (!urlPrompt) return;
    const safe = sanitizeRelayUrl(urlPrompt.url);
    // Unconditional, and it needs no seq guard, because it happens *before* the
    // first await: nothing else can have touched the slot between the click and
    // this line. The success and failure reports below are toasts rather than
    // this prompt coming back, so there is nothing here that has to survive the
    // round trip — which is what makes dismissing up front correct here and
    // wrong in `handleOpenUrl`. Anything that moves this dismissal after the
    // `openPageInContainerBrowser` call has to take the seq with it.
    dismissUrlPrompt();
    if (!safe) {
      console.warn("Refusing to open a URL that failed validation");
      return;
    }
    if (!projectId) return;
    // Land on the pane that will show it, before the work starts: opening takes
    // several seconds, and the progress line lives there.
    useAppState.getState().openProjectHomeTab(projectId, "browser");
    // A sign-in page is the one case where the *window* size matters least and
    // the layout matters most, so it gets the ordinary desktop viewport.
    // `true`: from a terminal there is no Browser pane on screen, so the page
    // needs a window of its own or it opens somewhere the user isn't looking.
    openPageInContainerBrowser(projectId, safe, 1280, 720, true)
      .then((result) => {
        const push = useAppState.getState().pushToast;
        if (result.error) {
          push({ kind: "error", message: "The page didn’t open", detail: result.error });
        } else {
          push({
            kind: "success",
            message: "Opened in the container’s browser",
          });
        }
      })
      .catch((e) =>
        useAppState.getState().pushToast({
          kind: "error",
          message: "Could not open it in the container’s browser",
          detail: String(e),
        }),
      );
  }, [urlPrompt, projectId, dismissUrlPrompt]);

  const writeSelection = useCallback((mode: "trimmed" | "raw") => {
    const term = termRef.current;
    if (!term) return;
    const sel = term.getSelection();
    if (!sel) return;
    const out = mode === "raw" ? sel : trimSelection(sel);
    navigator.clipboard.writeText(out).catch((e) =>
      console.error("Context menu clipboard write failed:", e),
    );
  }, []);

  const handleContextMenu = useCallback((e: React.MouseEvent) => {
    if (!termRef.current?.hasSelection()) return; // let default menu happen
    e.preventDefault();
    setContextMenu({ x: e.clientX, y: e.clientY });
  }, []);

  // Surface the capture state and its escape hatch to the status bar, but only
  // while this is the visible terminal.
  useEffect(() => {
    if (!active) return;
    setTerminalMouseCaptured(mouseCaptured);
    setReleaseActiveMouse(releaseMouse);
  }, [active, mouseCaptured, releaseMouse, setTerminalMouseCaptured, setReleaseActiveMouse]);

  // On unmount, if this was the active terminal, clear the status-bar state so
  // it does not point at a disposed terminal. (Tab switches do not unmount —
  // the deactivating terminal stays mounted but hidden — so this only fires
  // when the active session is actually closed.)
  useEffect(() => {
    return () => {
      if (activeRef.current) {
        setTerminalMouseCaptured(false);
        setReleaseActiveMouse(() => {});
      }
    };
  }, [setTerminalMouseCaptured, setReleaseActiveMouse]);

  return (
    <div
      ref={terminalContainerRef}
      className={`w-full h-full relative ${active ? "" : "hidden"}`}
    >
      {urlPrompt && (
        <UrlToast
          // A different URL is a different prompt, not an edit of this one.
          key={urlPrompt.seq}
          url={urlPrompt.url}
          label={urlPrompt.label}
          onOpen={handleOpenUrl}
          onOpenInContainer={handleOpenUrlInContainer}
          signInDefault={signInDefault}
          onDismiss={dismissUrlPrompt}
        />
      )}
      {imagePasteMsg && (
        <div
          className="absolute top-2 left-1/2 -translate-x-1/2 z-50 px-3 py-1.5 rounded-md text-xs font-medium bg-[#1f2937] text-[#e6edf3] border border-[#30363d] shadow-lg"
          onClick={() => setImagePasteMsg(null)}
        >
          {imagePasteMsg}
        </div>
      )}
      {/* Padding lives on this wrapper, NOT on the xterm host element. xterm's
          FitAddon measures the host element it's mounted into; padding there
          causes the grid to overhang and clip the rightmost column / bottom
          row. The host below fills this wrapper's content box with no padding.
          Kept to a tight, even gutter so the terminal claims as much area as
          possible while leaving a little breathing room beside the scrollbar. */}
      <div className="w-full h-full" style={{ padding: "4px 8px 4px 8px" }}>
        <div
          ref={containerRef}
          className="w-full h-full"
          onContextMenu={handleContextMenu}
        />
      </div>
      {contextMenu && (
        <TerminalContextMenu
          x={contextMenu.x}
          y={contextMenu.y}
          onCopyTrimmed={() => {
            writeSelection("trimmed");
            setContextMenu(null);
          }}
          onCopyRaw={() => {
            writeSelection("raw");
            setContextMenu(null);
          }}
          onDismiss={() => setContextMenu(null)}
        />
      )}
    </div>
  );
}
