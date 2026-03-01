import { useState, useEffect, useRef, useCallback } from "react";
import type { EnvVar } from "../../lib/types";

interface Props {
  envVars: EnvVar[];
  disabled: boolean;
  onSave: (vars: EnvVar[]) => Promise<void>;
  onClose: () => void;
}

export default function EnvVarsModal({ envVars: initial, disabled, onSave, onClose }: Props) {
  const [vars, setVars] = useState<EnvVar[]>(initial);
  const overlayRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const handleKeyDown = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    document.addEventListener("keydown", handleKeyDown);
    return () => document.removeEventListener("keydown", handleKeyDown);
  }, [onClose]);

  const handleOverlayClick = useCallback(
    (e: React.MouseEvent<HTMLDivElement>) => {
      if (e.target === overlayRef.current) onClose();
    },
    [onClose],
  );

  const updateVar = (index: number, field: keyof EnvVar, value: string) => {
    const updated = [...vars];
    updated[index] = { ...updated[index], [field]: value };
    setVars(updated);
  };

  const removeVar = async (index: number) => {
    const updated = vars.filter((_, i) => i !== index);
    setVars(updated);
    try { await onSave(updated); } catch (err) {
      console.error("Failed to remove environment variable:", err);
    }
  };

  const addVar = async () => {
    const updated = [...vars, { key: "", value: "" }];
    setVars(updated);
    try { await onSave(updated); } catch (err) {
      console.error("Failed to add environment variable:", err);
    }
  };

  const handleBlur = async () => {
    try { await onSave(vars); } catch (err) {
      console.error("Failed to update environment variables:", err);
    }
  };

  return (
    <div
      ref={overlayRef}
      onClick={handleOverlayClick}
      className="fixed inset-0 bg-black/50 flex items-center justify-center z-50"
    >
      <div className="bg-[var(--bg-secondary)] border border-[var(--border-color)] rounded-lg p-6 w-[36rem] shadow-xl max-h-[80vh] overflow-y-auto">
        <h2 className="text-lg font-semibold mb-4">Environment Variables</h2>

        {disabled && (
          <div className="px-2 py-1.5 mb-3 bg-[var(--warning)]/15 border border-[var(--warning)]/30 rounded text-xs text-[var(--warning)]">
            Container must be stopped to change environment variables.
          </div>
        )}

        <div className="space-y-2 mb-4">
          {vars.length === 0 && (
            <p className="text-xs text-[var(--text-secondary)]">No environment variables configured.</p>
          )}
          {vars.map((ev, i) => (
            <div key={i} className="flex gap-2 items-center">
              <input
                value={ev.key}
                onChange={(e) => updateVar(i, "key", e.target.value)}
                onBlur={handleBlur}
                placeholder="KEY"
                disabled={disabled}
                className="w-2/5 px-2 py-1.5 bg-[var(--bg-primary)] border border-[var(--border-color)] rounded text-sm text-[var(--text-primary)] focus:outline-none focus:border-[var(--accent)] disabled:opacity-50 font-mono"
              />
              <input
                value={ev.value}
                onChange={(e) => updateVar(i, "value", e.target.value)}
                onBlur={handleBlur}
                placeholder="value"
                disabled={disabled}
                className="flex-1 px-2 py-1.5 bg-[var(--bg-primary)] border border-[var(--border-color)] rounded text-sm text-[var(--text-primary)] focus:outline-none focus:border-[var(--accent)] disabled:opacity-50 font-mono"
              />
              <button
                onClick={() => removeVar(i)}
                disabled={disabled}
                className="px-2 py-1.5 text-sm text-[var(--error)] hover:bg-[var(--bg-primary)] rounded disabled:opacity-50 transition-colors"
              >
                x
              </button>
            </div>
          ))}
        </div>

        <div className="flex justify-between items-center">
          <button
            onClick={addVar}
            disabled={disabled}
            className="text-sm text-[var(--accent)] hover:text-[var(--accent-hover)] disabled:opacity-50 transition-colors"
          >
            + Add variable
          </button>
          <button
            onClick={onClose}
            className="px-4 py-2 text-sm text-[var(--text-secondary)] hover:text-[var(--text-primary)] transition-colors"
          >
            Close
          </button>
        </div>
      </div>
    </div>
  );
}
