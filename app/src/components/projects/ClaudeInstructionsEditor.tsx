import { useEffect, useState } from "react";

interface Props {
  instructions: string;
  disabled: boolean;
  disabledReason?: string;
  onSave: (instructions: string) => Promise<unknown>;
  rows?: number;
  autoFocus?: boolean;
}

export default function ClaudeInstructionsEditor({
  instructions: initial,
  disabled,
  disabledReason,
  onSave,
  rows = 10,
  autoFocus = false,
}: Props) {
  const [instructions, setInstructions] = useState(initial);

  useEffect(() => {
    setInstructions(initial);
  }, [initial]);

  return (
    <div className="space-y-2">
      {disabled && disabledReason && (
        <p className="px-2 py-1.5 bg-[var(--warning-muted)] border border-[var(--warning)]/30 rounded-[var(--radius-control)] text-xs text-[var(--warning)]">
          {disabledReason}
        </p>
      )}
      <textarea
        autoFocus={autoFocus}
        value={instructions}
        onChange={(e) => setInstructions(e.target.value)}
        onBlur={() => onSave(instructions)}
        placeholder="Enter instructions for Claude Code in this project's container..."
        aria-label="Claude instructions"
        disabled={disabled}
        rows={rows}
        className="w-full px-3 py-2 bg-[var(--bg-primary)] border border-[var(--border-color)] rounded-[var(--radius-control)] text-[13px] text-[var(--text-primary)] focus:border-[var(--accent)] disabled:text-[var(--text-disabled)] disabled:bg-[var(--bg-secondary)] resize-y font-mono transition-colors"
      />
    </div>
  );
}
