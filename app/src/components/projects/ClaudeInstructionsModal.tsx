import { useState, useEffect, useRef, useCallback } from "react";

interface Props {
  instructions: string;
  disabled: boolean;
  onSave: (instructions: string) => Promise<void>;
  onClose: () => void;
}

export default function ClaudeInstructionsModal({ instructions: initial, disabled, onSave, onClose }: Props) {
  const [instructions, setInstructions] = useState(initial);
  const overlayRef = useRef<HTMLDivElement>(null);
  const textareaRef = useRef<HTMLTextAreaElement>(null);

  useEffect(() => {
    textareaRef.current?.focus();
  }, []);

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

  const handleBlur = async () => {
    try { await onSave(instructions); } catch (err) {
      console.error("Failed to update Claude instructions:", err);
    }
  };

  return (
    <div
      ref={overlayRef}
      onClick={handleOverlayClick}
      className="fixed inset-0 bg-black/50 flex items-center justify-center z-50"
    >
      <div className="bg-[var(--bg-secondary)] border border-[var(--border-color)] rounded-lg p-6 w-[40rem] shadow-xl max-h-[80vh] flex flex-col">
        <h2 className="text-lg font-semibold mb-1">Claude Instructions</h2>
        <p className="text-xs text-[var(--text-secondary)] mb-4">
          Per-project instructions for Claude Code (written to ~/.claude/CLAUDE.md in container)
        </p>

        {disabled && (
          <div className="px-2 py-1.5 mb-3 bg-[var(--warning)]/15 border border-[var(--warning)]/30 rounded text-xs text-[var(--warning)]">
            Container must be stopped to change Claude instructions.
          </div>
        )}

        <textarea
          ref={textareaRef}
          value={instructions}
          onChange={(e) => setInstructions(e.target.value)}
          onBlur={handleBlur}
          placeholder="Enter instructions for Claude Code in this project's container..."
          disabled={disabled}
          rows={14}
          className="w-full flex-1 px-3 py-2 bg-[var(--bg-primary)] border border-[var(--border-color)] rounded text-sm text-[var(--text-primary)] focus:outline-none focus:border-[var(--accent)] disabled:opacity-50 resize-y font-mono"
        />

        <div className="flex justify-end mt-4">
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
