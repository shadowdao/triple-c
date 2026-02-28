import { useEffect, useRef } from "react";
import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import { WebglAddon } from "@xterm/addon-webgl";
import { WebLinksAddon } from "@xterm/addon-web-links";
import { openUrl } from "@tauri-apps/plugin-opener";
import "@xterm/xterm/css/xterm.css";
import { useTerminal } from "../../hooks/useTerminal";

/** Strip ANSI escape sequences from a string. */
function stripAnsi(s: string): string {
  return s.replace(/\x1b\[[0-9;]*[A-Za-z]|\x1b\][^\x07]*\x07/g, "");
}

interface Props {
  sessionId: string;
  active: boolean;
}

export default function TerminalView({ sessionId, active }: Props) {
  const containerRef = useRef<HTMLDivElement>(null);
  const termRef = useRef<Terminal | null>(null);
  const fitRef = useRef<FitAddon | null>(null);
  const { sendInput, resize, onOutput, onExit } = useTerminal();

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

    // Try WebGL renderer, fall back silently
    try {
      const webglAddon = new WebglAddon();
      term.loadAddon(webglAddon);
    } catch {
      // WebGL not available, canvas renderer is fine
    }

    fitAddon.fit();
    termRef.current = term;
    fitRef.current = fitAddon;

    // Send initial size
    resize(sessionId, term.cols, term.rows);

    // Handle user input -> backend
    const inputDisposable = term.onData((data) => {
      sendInput(sessionId, data);
    });

    // ── URL accumulator ──────────────────────────────────────────────
    // Claude Code login emits a long OAuth URL that gets split across
    // hard newlines (\n / \r\n).  The WebLinksAddon only joins
    // soft-wrapped lines (the `isWrapped` flag), so the URL match is
    // truncated and the link fails when clicked.
    //
    // Fix: buffer recent output, strip ANSI codes, and after a short
    // debounce check for a URL that spans multiple lines.  When found,
    // write a single clean clickable copy to the terminal.
    const textDecoder = new TextDecoder();
    let outputBuffer = "";
    let debounceTimer: ReturnType<typeof setTimeout> | null = null;

    const flushUrlBuffer = () => {
      const plain = stripAnsi(outputBuffer);
      // Reassemble: strip hard newlines and carriage returns to join
      // fragments that were split across terminal lines.
      const joined = plain.replace(/[\r\n]+/g, "");
      // Look for a long OAuth/auth URL (Claude login URLs contain
      // "oauth" or "console.anthropic.com" or "/authorize").
      const match = joined.match(/https?:\/\/[^\s'"\x07]{80,}/);
      if (match) {
        const url = match[0];
        term.write("\r\n\x1b[36m🔗 Clickable login URL:\x1b[0m\r\n");
        term.write(`\x1b[4;34m${url}\x1b[0m\r\n`);
      }
      outputBuffer = "";
    };

    // Handle backend output -> terminal
    let aborted = false;

    const outputPromise = onOutput(sessionId, (data) => {
      if (aborted) return;
      term.write(data);

      // Accumulate for URL detection (data is a Uint8Array, so decode it)
      outputBuffer += textDecoder.decode(data);
      // Cap buffer size to avoid memory growth
      if (outputBuffer.length > 8192) {
        outputBuffer = outputBuffer.slice(-4096);
      }
      if (debounceTimer) clearTimeout(debounceTimer);
      debounceTimer = setTimeout(flushUrlBuffer, 150);
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

    // Handle resize (throttled via requestAnimationFrame to avoid excessive calls)
    let resizeRafId: number | null = null;
    const resizeObserver = new ResizeObserver(() => {
      if (resizeRafId !== null) return;
      resizeRafId = requestAnimationFrame(() => {
        resizeRafId = null;
        fitAddon.fit();
        resize(sessionId, term.cols, term.rows);
      });
    });
    resizeObserver.observe(containerRef.current);

    return () => {
      aborted = true;
      if (debounceTimer) clearTimeout(debounceTimer);
      inputDisposable.dispose();
      outputPromise.then((fn) => fn?.());
      exitPromise.then((fn) => fn?.());
      if (resizeRafId !== null) cancelAnimationFrame(resizeRafId);
      resizeObserver.disconnect();
      term.dispose();
    };
  }, [sessionId]); // eslint-disable-line react-hooks/exhaustive-deps

  // Re-fit when tab becomes active
  useEffect(() => {
    if (active && fitRef.current && termRef.current) {
      fitRef.current.fit();
      termRef.current.focus();
    }
  }, [active]);

  return (
    <div
      ref={containerRef}
      className={`w-full h-full ${active ? "" : "hidden"}`}
      style={{ padding: "8px" }}
    />
  );
}
