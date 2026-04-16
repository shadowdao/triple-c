import { useCallback, useEffect, useRef, useState } from "react";
import type { SttState } from "../../hooks/useSTT";
import * as commands from "../../lib/tauri-commands";

interface Props {
  state: SttState;
  error: string | null;
  onToggle: () => Promise<void>;
  onCancel: () => Promise<void>;
}

export default function SttButton({ state, error, onToggle, onCancel }: Props) {
  const [elapsed, setElapsed] = useState(0);
  const [hovered, setHovered] = useState(false);
  const timerRef = useRef<ReturnType<typeof setInterval> | null>(null);

  // Track recording duration
  useEffect(() => {
    if (state === "recording") {
      setElapsed(0);
      timerRef.current = setInterval(() => setElapsed((e) => e + 1), 1000);
    } else {
      if (timerRef.current) {
        clearInterval(timerRef.current);
        timerRef.current = null;
      }
    }
    return () => {
      if (timerRef.current) clearInterval(timerRef.current);
    };
  }, [state]);

  const handleClick = useCallback(async () => {
    // Auto-start STT container if not running
    if (state === "idle") {
      try {
        const status = await commands.getSttStatus();
        if (!status.running) {
          await commands.startStt();
        }
      } catch {
        // Container start failed, toggle will still attempt transcription
      }
    }
    await onToggle();
  }, [state, onToggle]);

  const handleContextMenu = useCallback(
    (e: React.MouseEvent) => {
      e.preventDefault();
      if (state === "recording") {
        onCancel();
      }
    },
    [state, onCancel],
  );

  const formatTime = (seconds: number) => {
    const m = Math.floor(seconds / 60);
    const s = seconds % 60;
    return `${m}:${s.toString().padStart(2, "0")}`;
  };

  return (
    <div className="absolute bottom-2 left-2 z-50 flex items-center gap-2">
      <div className="relative">
        <button
          onClick={handleClick}
          onContextMenu={handleContextMenu}
          onMouseDown={(e) => e.preventDefault()} // prevent stealing focus from terminal
          onMouseEnter={() => setHovered(true)}
          onMouseLeave={() => setHovered(false)}
          disabled={state === "transcribing"}
          className={`w-8 h-8 rounded-full flex items-center justify-center transition-all cursor-pointer ${
          state === "recording"
            ? "bg-[#f85149] text-white shadow-lg animate-pulse"
            : state === "transcribing"
              ? "bg-[#1f2937] text-[#58a6ff] border border-[#30363d] opacity-80"
              : "bg-[#1f2937]/80 text-[#8b949e] border border-[#30363d] hover:text-[#e6edf3] hover:bg-[#2d3748]"
        }`}
        >
          {state === "transcribing" ? (
            <svg className="w-4 h-4 animate-spin" viewBox="0 0 24 24" fill="none">
              <circle cx="12" cy="12" r="10" stroke="currentColor" strokeWidth="2" opacity="0.25" />
              <path d="M12 2a10 10 0 0 1 10 10" stroke="currentColor" strokeWidth="2" strokeLinecap="round" />
            </svg>
          ) : (
            <svg className="w-4 h-4" viewBox="0 0 24 24" fill="currentColor">
              <path d="M12 14c1.66 0 3-1.34 3-3V5c0-1.66-1.34-3-3-3S9 3.34 9 5v6c0 1.66 1.34 3 3 3z" />
              <path d="M17 11c0 2.76-2.24 5-5 5s-5-2.24-5-5H5c0 3.53 2.61 6.43 6 6.92V21h2v-3.08c3.39-.49 6-3.39 6-6.92h-2z" />
            </svg>
          )}
        </button>
        {hovered && state !== "recording" && (
          <div className="absolute bottom-full left-0 mb-1.5 px-2 py-1 text-[11px] leading-snug text-[#e6edf3] bg-[#21262d] border border-[#30363d] rounded shadow-lg whitespace-nowrap pointer-events-none">
            {state === "transcribing" ? "Transcribing..." : (
              <>Speech to text <kbd className="ml-1 px-1 py-0.5 text-[10px] bg-[#0d1117] border border-[#30363d] rounded font-mono">Ctrl+Shift+M</kbd></>
            )}
          </div>
        )}
      </div>
      {state === "recording" && (
        <span className="text-xs text-[#f85149] font-mono bg-[#1f2937] px-2 py-0.5 rounded border border-[#30363d]">
          {formatTime(elapsed)}
        </span>
      )}
      {state === "error" && error && (
        <span className="text-xs text-[#f85149] bg-[#1f2937] px-2 py-0.5 rounded border border-[#30363d] max-w-[200px] truncate">
          {error}
        </span>
      )}
    </div>
  );
}
