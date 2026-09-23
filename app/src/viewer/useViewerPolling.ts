import { useEffect, useRef } from "react";

/** A visibility-gated interval that never overlaps its own ticks (spec §5). */
export function useViewerPolling(intervalMs: number, tick: () => Promise<void>, enabled: boolean): void {
  const tickRef = useRef(tick);
  tickRef.current = tick;

  useEffect(() => {
    if (!enabled) return;
    let disposed = false;
    let inFlight = false;
    let timer: ReturnType<typeof setInterval> | null = null;

    const run = async () => {
      if (disposed || inFlight || document.visibilityState !== "visible") return;
      inFlight = true;
      try { await tickRef.current(); } finally { inFlight = false; }
    };
    const start = () => { if (timer === null) timer = setInterval(run, intervalMs); };
    const stop = () => { if (timer !== null) { clearInterval(timer); timer = null; } };
    const onVisibility = () => {
      if (document.visibilityState === "visible") { void run(); start(); } else { stop(); }
    };

    document.addEventListener("visibilitychange", onVisibility);
    onVisibility();
    return () => {
      disposed = true;
      stop();
      document.removeEventListener("visibilitychange", onVisibility);
    };
  }, [intervalMs, enabled]);
}
