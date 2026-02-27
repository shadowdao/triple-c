import { useEffect, useRef } from "react";
import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import { WebglAddon } from "@xterm/addon-webgl";
import { WebLinksAddon } from "@xterm/addon-web-links";
import { openUrl } from "@tauri-apps/plugin-opener";
import "@xterm/xterm/css/xterm.css";
import { useTerminal } from "../../hooks/useTerminal";

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

    // Web links addon — opens URLs in host browser via Tauri
    const webLinksAddon = new WebLinksAddon((_event, uri) => {
      openUrl(uri).catch((e) => console.error("Failed to open URL:", e));
    });
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

    // Handle backend output -> terminal
    let unlistenOutput: (() => void) | null = null;
    let unlistenExit: (() => void) | null = null;

    onOutput(sessionId, (data) => {
      term.write(data);
    }).then((unlisten) => {
      unlistenOutput = unlisten;
    });

    onExit(sessionId, () => {
      term.write("\r\n\x1b[33m[Session ended]\x1b[0m\r\n");
    }).then((unlisten) => {
      unlistenExit = unlisten;
    });

    // Handle resize
    const resizeObserver = new ResizeObserver(() => {
      fitAddon.fit();
      resize(sessionId, term.cols, term.rows);
    });
    resizeObserver.observe(containerRef.current);

    return () => {
      inputDisposable.dispose();
      unlistenOutput?.();
      unlistenExit?.();
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
      style={{ padding: "4px" }}
    />
  );
}
