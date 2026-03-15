import { useCallback, useEffect, useRef, useState } from "react";
import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import { WebglAddon } from "@xterm/addon-webgl";
import { WebLinksAddon } from "@xterm/addon-web-links";
import { openUrl } from "@tauri-apps/plugin-opener";
import "@xterm/xterm/css/xterm.css";
import { useTerminal } from "../../hooks/useTerminal";
import { useAppState } from "../../store/appState";
import { awsSsoRefresh } from "../../lib/tauri-commands";
import { UrlDetector } from "../../lib/urlDetector";
import UrlToast from "./UrlToast";

interface Props {
  sessionId: string;
  active: boolean;
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

  const ssoBufferRef = useRef("");
  const ssoTriggeredRef = useRef(false);
  const projectId = useAppState(
    (s) => s.sessions.find((sess) => sess.id === sessionId)?.projectId
  );

  const [detectedUrl, setDetectedUrl] = useState<string | null>(null);
  const [imagePasteMsg, setImagePasteMsg] = useState<string | null>(null);
  const [isAtBottom, setIsAtBottom] = useState(true);
  const [isAutoFollow, setIsAutoFollow] = useState(true);
  const isAtBottomRef = useRef(true);
  // Tracks user intent to follow output — only set to false by explicit user
  // actions (mouse wheel up), not by xterm scroll events during writes.
  const autoFollowRef = useRef(true);
  const lastUserScrollTimeRef = useRef(0);

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
    const urlRegex = /https?:\/\/[^\s'"\x07]+/;
    const webLinksAddon = new WebLinksAddon((_event, uri) => {
      openUrl(uri).catch((e) => console.error("Failed to open URL:", e));
    }, { urlRegex });
    term.loadAddon(webLinksAddon);

    term.open(containerRef.current);

    // Ctrl+Shift+C copies selected terminal text to clipboard.
    // This prevents the keystroke from reaching the container (where
    // Ctrl+C would send SIGINT and cancel running work).
    term.attachCustomKeyEventHandler((event) => {
      if (event.type === "keydown" && event.ctrlKey && event.shiftKey && event.key === "C") {
        const sel = term.getSelection();
        if (sel) {
          navigator.clipboard.writeText(sel).catch((e) =>
            console.error("Ctrl+Shift+C clipboard write failed:", e),
          );
        }
        return false; // prevent xterm from processing this key
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

    const detector = new UrlDetector((url) => setDetectedUrl(url));
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

  // Auto-dismiss toast after 30 seconds
  useEffect(() => {
    if (!detectedUrl) return;
    const timer = setTimeout(() => setDetectedUrl(null), 30_000);
    return () => clearTimeout(timer);
  }, [detectedUrl]);

  // Auto-dismiss image paste message after 3 seconds
  useEffect(() => {
    if (!imagePasteMsg) return;
    const timer = setTimeout(() => setImagePasteMsg(null), 3_000);
    return () => clearTimeout(timer);
  }, [imagePasteMsg]);

  const handleOpenUrl = useCallback(() => {
    if (detectedUrl) {
      openUrl(detectedUrl).catch((e) =>
        console.error("Failed to open URL:", e),
      );
      setDetectedUrl(null);
    }
  }, [detectedUrl]);

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
      {detectedUrl && (
        <UrlToast
          url={detectedUrl}
          onOpen={handleOpenUrl}
          onDismiss={() => setDetectedUrl(null)}
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
      {/* Jump to Current - bottom right, when scrolled up */}
      {!isAtBottom && (
        <button
          onClick={handleScrollToBottom}
          className="absolute bottom-4 right-4 z-50 px-3 py-1.5 rounded-md text-xs font-medium bg-[#1f2937] text-[#58a6ff] border border-[#30363d] shadow-lg hover:bg-[#2d3748] transition-colors cursor-pointer"
        >
          Jump to Current ↓
        </button>
      )}
      <div
        ref={containerRef}
        className="w-full h-full"
        style={{ padding: "8px" }}
      />
    </div>
  );
}
