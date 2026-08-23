import { useCallback, useEffect, useRef, useState } from "react";
import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import { WebglAddon } from "@xterm/addon-webgl";
import { WebLinksAddon } from "@xterm/addon-web-links";
import { openUrl } from "@tauri-apps/plugin-opener";
import "@xterm/xterm/css/xterm.css";
import { useTerminal } from "../../hooks/useTerminal";
import { useAppState } from "../../store/appState";
import {
  awsSsoRefresh,
  openPageInContainerBrowser,
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
} from "../../lib/urlRelay";
import { isDropTarget } from "../../lib/dropTarget";
import UrlToast, {
  URL_TOAST_PRIMARY_SELECTOR,
  URL_TOAST_SELECTOR,
  URL_TOAST_SHORTCUT,
} from "./UrlToast";
import { trimSelection } from "./trimSelection";
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

export default function TerminalView({ sessionId, active }: Props) {
  const containerRef = useRef<HTMLDivElement>(null);
  const terminalContainerRef = useRef<HTMLDivElement>(null);
  const termRef = useRef<Terminal | null>(null);
  const fitRef = useRef<FitAddon | null>(null);
  const webglRef = useRef<WebglAddon | null>(null);
  const detectorRef = useRef<UrlDetector | null>(null);
  const { sendInput, pasteImage, resize, onOutput, onExit } = useTerminal();
  const setTerminalHasSelection = useAppState(s => s.setTerminalHasSelection);
  const setTerminalAtBottom = useAppState(s => s.setTerminalAtBottom);
  const setScrollActiveToBottom = useAppState(s => s.setScrollActiveToBottom);

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
  const [urlPrompt, setUrlPrompt] = useState<{
    url: string;
    label: string;
    source: PromptSource;
    seq: number;
  } | null>(null);
  const promptSeqRef = useRef(0);
  const relayLimiterRef = useRef(new RelayRateLimiter());
  // Read by the long-lived keyboard listener below, which is registered once
  // and would otherwise close over the prompt as it was at mount.
  const urlPromptRef = useRef<{ url: string } | null>(null);

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
    setUrlPrompt(null);
    if (wasInside) termRef.current?.focus();
  }, []);

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
      setUrlPrompt((current) => {
        if (!supersedes({ url, source }, current)) return current;
        promptSeqRef.current += 1;
        return { url, label, source, seq: promptSeqRef.current };
      });
    },
    [],
  );
  useEffect(() => {
    urlPromptRef.current = urlPrompt;
  }, [urlPrompt]);

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
  const [isAtBottom, setIsAtBottom] = useState(true);
  const [isAutoFollow, setIsAutoFollow] = useState(true);
  const [contextMenu, setContextMenu] = useState<{ x: number; y: number } | null>(null);
  const isAtBottomRef = useRef(true);
  // Tracks user intent to follow output — only set to false by explicit user
  // actions (mouse wheel up), not by xterm scroll events during writes.
  const autoFollowRef = useRef(true);
  const lastUserScrollTimeRef = useRef(0);

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
  // drop was meant for it. `isDropTarget` is that decision, shared with the
  // Files pane: the physical-pixel position ÷ `devicePixelRatio` against this
  // pane's rect (a hidden pane is `display:none`, so its zero-size rect is what
  // stops two panes both claiming the drop), plus z-order — which a rect alone
  // cannot see. An open `Modal` is a `fixed inset-0` portal painted *over* the
  // window and the pane underneath still has its rect, so the geometric test
  // that used to live here uploaded files into the directory a dialog was
  // covering. Same for the shutdown overlay, which is on screen precisely while
  // nothing should be accepting work at all.
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
        if (!isDropTarget(containerRef.current, event.payload.position)) return;

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

  useEffect(() => {
    if (!containerRef.current) return;

    const term = new Terminal({
      cursorBlink: true,
      fontSize: 14,
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
    const webLinksAddon = new WebLinksAddon((_event, uri) => {
      // Same sink, same rule: what xterm matched came off the container's
      // output, so it is validated before it reaches the OS opener. A click
      // here is a deliberate act on visible text, but "visible" is exactly
      // what a userinfo-spoofed URL subverts.
      const safe = sanitizeRelayUrl(uri);
      if (!safe) {
        console.warn("Refusing to open a link that failed validation");
        return;
      }
      openUrl(safe).catch((e) => console.error("Failed to open URL:", e));
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
        sendInput(sessionId, "\x1b\r");
        return false; // xterm must not also send a bare CR, which submits
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
      sendInput(sessionId, data);
    });

    // Detect user-initiated scroll-up (mouse wheel) to pause auto-follow.
    // Captured during capture phase so it fires before xterm's own handler.
    const handleWheel = (e: WheelEvent) => {
      lastUserScrollTimeRef.current = Date.now();
      if (e.deltaY < 0) {
        autoFollowRef.current = false;
        setIsAutoFollow(false);
        isAtBottomRef.current = false;
        setIsAtBottom(false);
      }
    };
    containerRef.current.addEventListener("wheel", handleWheel, { capture: true, passive: true });

    // Track scroll position to show "Jump to Current" button.
    // Debounce state updates via rAF to avoid excessive re-renders during rapid output.
    let scrollStateRafId: number | null = null;
    const scrollDisposable = term.onScroll(() => {
      const buf = term.buffer.active;
      const atBottom = buf.viewportY >= buf.baseY;
      isAtBottomRef.current = atBottom;

      // Re-enable auto-follow only when USER scrolls to bottom (not write-triggered)
      const isUserScroll = (Date.now() - lastUserScrollTimeRef.current) < 300;
      if (atBottom && isUserScroll && !autoFollowRef.current) {
        autoFollowRef.current = true;
        setIsAutoFollow(true);
      }

      if (scrollStateRafId === null) {
        scrollStateRafId = requestAnimationFrame(() => {
          scrollStateRafId = null;
          setIsAtBottom(isAtBottomRef.current);
        });
      }
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
      term.write(data, () => {
        if (autoFollowRef.current) {
          term.scrollToBottom();
          if (!isAtBottomRef.current) {
            isAtBottomRef.current = true;
            setIsAtBottom(true);
          }
        }
      });
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
        fitAddon.fit();
        resize(sessionId, term.cols, term.rows);
        if (autoFollowRef.current) {
          term.scrollToBottom();
        }
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
      scrollDisposable.dispose();
      selectionDisposable.dispose();
      setTerminalHasSelection(false);
      containerRef.current?.removeEventListener("wheel", handleWheel, { capture: true });
      containerRef.current?.removeEventListener("paste", handlePaste, { capture: true });
      outputPromise.then((fn) => fn?.());
      exitPromise.then((fn) => fn?.());
      if (scrollStateRafId !== null) cancelAnimationFrame(scrollStateRafId);
      if (resizeRafId !== null) cancelAnimationFrame(resizeRafId);
      resizeObserver.disconnect();
      try { webglRef.current?.dispose(); } catch { /* may already be disposed */ }
      webglRef.current = null;
      term.dispose();
      termRef.current = null;
    };
  }, [sessionId]); // eslint-disable-line react-hooks/exhaustive-deps

  // Manage WebGL lifecycle and re-fit when tab becomes active.
  // Only the active terminal holds a WebGL context to avoid exhausting
  // the browser's limited pool (~8-16 contexts).
  useEffect(() => {
    const term = termRef.current;
    if (!term) return;

    if (active) {
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
      fitRef.current?.fit();
      if (autoFollowRef.current) {
        term.scrollToBottom();
      }
      term.focus();
    } else {
      // Release WebGL context for inactive terminals
      if (webglRef.current) {
        try { webglRef.current.dispose(); } catch { /* ignore */ }
        webglRef.current = null;
      }
    }
  }, [active]);

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

  const handleOpenUrl = useCallback(() => {
    if (!urlPrompt) return;
    // Validated again at the sink. `promptUrl` is the only writer and already
    // sanitizes, so this can only fail if that invariant is broken — which is
    // precisely when it matters that the last thing before `openUrl` checks.
    const safe = sanitizeRelayUrl(urlPrompt.url);
    dismissUrlPrompt();
    if (!safe) {
      console.warn("Refusing to open a URL that failed validation");
      return;
    }
    openUrl(safe).catch((e) => console.error("Failed to open URL:", e));
  }, [urlPrompt, dismissUrlPrompt]);

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

  const handleScrollToBottom = useCallback(() => {
    const term = termRef.current;
    if (term) {
      autoFollowRef.current = true;
      setIsAutoFollow(true);
      fitRef.current?.fit();
      term.scrollToBottom();
      isAtBottomRef.current = true;
      setIsAtBottom(true);
    }
  }, []);

  // Surface this terminal's scroll state to the status bar's "Jump to Current"
  // control, but only while it's the active (visible) terminal.
  useEffect(() => {
    if (!active) return;
    setTerminalAtBottom(isAtBottom);
    setScrollActiveToBottom(handleScrollToBottom);
  }, [active, isAtBottom, handleScrollToBottom, setTerminalAtBottom, setScrollActiveToBottom]);

  // On unmount, if this was the active terminal, clear the status-bar scroll
  // state so it doesn't point at a disposed terminal. (Tab switches don't
  // unmount — the deactivating terminal stays mounted but hidden — so this
  // only fires when the active session is actually closed.)
  useEffect(() => {
    return () => {
      if (activeRef.current) {
        setTerminalAtBottom(true);
        setScrollActiveToBottom(() => {});
      }
    };
  }, [setTerminalAtBottom, setScrollActiveToBottom]);

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

  const handleToggleAutoFollow = useCallback(() => {
    const next = !autoFollowRef.current;
    autoFollowRef.current = next;
    setIsAutoFollow(next);
    if (next) {
      const term = termRef.current;
      if (term) {
        fitRef.current?.fit();
        term.scrollToBottom();
        isAtBottomRef.current = true;
        setIsAtBottom(true);
      }
    }
  }, []);

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
      {/* Auto-follow toggle - top right */}
      <button
        onClick={handleToggleAutoFollow}
        className={`absolute top-2 right-4 z-50 px-2 py-1 rounded text-[10px] font-medium border shadow-sm transition-colors cursor-pointer ${
          isAutoFollow
            ? "bg-[#1a2332] text-[#3fb950] border-[#238636] hover:bg-[#1f2d3d]"
            : "bg-[#1f2937] text-[#8b949e] border-[#30363d] hover:bg-[#2d3748]"
        }`}
        title={isAutoFollow ? "Auto-scrolling to latest output (click to pause)" : "Auto-scroll paused (click to resume)"}
      >
        {isAutoFollow ? "▼ Following" : "▽ Paused"}
      </button>
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
