import { useEffect, useRef, useCallback } from "react";

interface Props {
  projectName: string;
  operation: "starting" | "stopping" | "resetting";
  progressMsg: string | null;
  error: string | null;
  completed: boolean;
  onForceStop: () => void;
  onClose: () => void;
}

const operationLabels: Record<string, string> = {
  starting: "Starting",
  stopping: "Stopping",
  resetting: "Resetting",
};

export default function ContainerProgressModal({
  projectName,
  operation,
  progressMsg,
  error,
  completed,
  onForceStop,
  onClose,
}: Props) {
  const overlayRef = useRef<HTMLDivElement>(null);

  // Auto-close on success after 800ms
  useEffect(() => {
    if (completed && !error) {
      const timer = setTimeout(onClose, 800);
      return () => clearTimeout(timer);
    }
  }, [completed, error, onClose]);

  // Escape to close (only when completed or error)
  useEffect(() => {
    const handleKeyDown = (e: KeyboardEvent) => {
      if (e.key === "Escape" && (completed || error)) onClose();
    };
    document.addEventListener("keydown", handleKeyDown);
    return () => document.removeEventListener("keydown", handleKeyDown);
  }, [completed, error, onClose]);

  const handleOverlayClick = useCallback(
    (e: React.MouseEvent<HTMLDivElement>) => {
      if (e.target === overlayRef.current && (completed || error)) onClose();
    },
    [completed, error, onClose],
  );

  const inProgress = !completed && !error;

  return (
    <div
      ref={overlayRef}
      onClick={handleOverlayClick}
      className="fixed inset-0 bg-black/50 flex items-center justify-center z-50"
    >
      <div className="bg-[var(--bg-secondary)] border border-[var(--border-color)] rounded-lg p-6 w-80 shadow-xl text-center">
        <h3 className="text-sm font-semibold mb-4">
          {operationLabels[operation]} &ldquo;{projectName}&rdquo;
        </h3>

        {/* Spinner / checkmark / error icon */}
        <div className="flex justify-center mb-3">
          {error ? (
            <span className="text-3xl text-[var(--error)]">✕</span>
          ) : completed ? (
            <span className="text-3xl text-[var(--success)]">✓</span>
          ) : (
            <div className="w-8 h-8 border-2 border-[var(--accent)] border-t-transparent rounded-full animate-spin" />
          )}
        </div>

        {/* Progress message */}
        <p className="text-xs text-[var(--text-secondary)] min-h-[1.25rem] mb-4">
          {error
            ? <span className="text-[var(--error)]">{error}</span>
            : completed
              ? "Done!"
              : progressMsg ?? `${operationLabels[operation]}...`}
        </p>

        {/* Buttons */}
        <div className="flex justify-center gap-2">
          {inProgress && (
            <button
              onClick={(e) => { e.stopPropagation(); onForceStop(); }}
              className="px-3 py-1.5 text-xs text-[var(--error)] border border-[var(--error)]/30 rounded hover:bg-[var(--error)]/10 transition-colors"
            >
              Force Stop
            </button>
          )}
          {(completed || error) && (
            <button
              onClick={(e) => { e.stopPropagation(); onClose(); }}
              className="px-3 py-1.5 text-xs text-[var(--text-secondary)] hover:text-[var(--text-primary)] border border-[var(--border-color)] rounded transition-colors"
            >
              Close
            </button>
          )}
        </div>
      </div>
    </div>
  );
}
