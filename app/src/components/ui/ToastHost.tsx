import { useEffect, useState } from "react";
import { useShallow } from "zustand/react/shallow";
import { useAppState, type Toast } from "../../store/appState";

const TONE: Record<Toast["kind"], { border: string; bg: string; fg: string; glyph: string }> = {
  error: {
    border: "var(--error)",
    bg: "var(--error-muted)",
    fg: "var(--error)",
    glyph: "▲",
  },
  success: {
    border: "var(--success)",
    bg: "var(--success-muted)",
    fg: "var(--success)",
    glyph: "✓",
  },
  info: {
    border: "var(--border-color)",
    bg: "var(--accent-muted)",
    fg: "var(--accent)",
    glyph: "●",
  },
};

function ToastCard({ toast, onDismiss }: { toast: Toast; onDismiss: () => void }) {
  const [expanded, setExpanded] = useState(false);
  const tone = TONE[toast.kind];

  // Errors stay until dismissed; transient confirmations time out.
  useEffect(() => {
    if (toast.kind === "error") return;
    const timer = setTimeout(onDismiss, 6000);
    return () => clearTimeout(timer);
  }, [toast.kind, onDismiss]);

  return (
    <div
      className="animate-toast-in flex items-start gap-2 w-[24rem] max-w-[calc(100vw-2rem)] px-3 py-2 rounded-[var(--radius-panel)] border text-xs"
      style={{
        borderColor: tone.border,
        background: `color-mix(in srgb, var(--bg-overlay) 88%, ${tone.bg})`,
        boxShadow: "var(--shadow-overlay)",
      }}
    >
      <span aria-hidden="true" className="mt-[1px] leading-none" style={{ color: tone.fg }}>
        {tone.glyph}
      </span>
      <div className="flex-1 min-w-0">
        <div className="text-[var(--text-primary)] break-words">{toast.message}</div>
        {toast.detail && (
          <>
            <button
              type="button"
              onClick={() => setExpanded((e) => !e)}
              aria-expanded={expanded}
              className="mt-1 text-[var(--accent)] hover:text-[var(--accent-hover)] transition-colors"
            >
              {expanded ? "Hide details" : "Details"}
            </button>
            {expanded && (
              <pre className="mt-1 max-h-40 overflow-auto whitespace-pre-wrap break-words font-mono text-[11px] text-[var(--text-secondary)] bg-[var(--bg-primary)] border border-[var(--border-color)] rounded-[var(--radius-control)] p-2">
                {toast.detail}
              </pre>
            )}
          </>
        )}
      </div>
      <button
        type="button"
        onClick={onDismiss}
        aria-label="Dismiss notification"
        className="flex-shrink-0 w-5 h-5 flex items-center justify-center rounded-[var(--radius-control)] text-[var(--text-secondary)] hover:text-[var(--text-primary)] hover:bg-[var(--bg-tertiary)] transition-colors"
      >
        <span aria-hidden="true">✕</span>
      </button>
    </div>
  );
}

/** Bottom-right stack. Errors get a home here instead of a 12px card line. */
export default function ToastHost() {
  const { toasts, dismissToast } = useAppState(
    useShallow((s) => ({ toasts: s.toasts, dismissToast: s.dismissToast })),
  );

  if (toasts.length === 0) return null;

  return (
    <div
      className="fixed bottom-4 right-4 z-[60] flex flex-col gap-2 items-end"
      role="region"
      aria-label="Notifications"
      aria-live="polite"
    >
      {toasts.map((toast) => (
        <ToastCard
          key={toast.id}
          toast={toast}
          onDismiss={() => dismissToast(toast.id)}
        />
      ))}
    </div>
  );
}
