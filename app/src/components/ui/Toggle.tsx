interface Props {
  checked: boolean;
  onChange: (value: boolean) => void;
  disabled?: boolean;
  /** Accessible name — required, since the visual label lives outside. */
  label: string;
  /** Paint the "on" state as caution rather than success. */
  tone?: "success" | "caution";
}

/**
 * ON/OFF switch. The old version put white text on `--success` (~2.1:1, the
 * worst contrast in the app); the on-state now uses a tinted background with
 * the token colour as *foreground*.
 */
export default function Toggle({
  checked,
  onChange,
  disabled = false,
  label,
  tone = "success",
}: Props) {
  const onStyle =
    tone === "caution"
      ? "bg-[var(--warning-muted)] border-[var(--warning)] text-[var(--warning)]"
      : "bg-[var(--success-muted)] border-[var(--success)] text-[var(--success)]";

  return (
    <button
      type="button"
      role="switch"
      aria-checked={checked}
      aria-label={label}
      disabled={disabled}
      onClick={() => onChange(!checked)}
      className={`inline-flex items-center justify-center h-6 min-w-[3rem] px-2 text-xs font-semibold rounded-[var(--radius-control)] border transition-colors disabled:cursor-not-allowed disabled:text-[var(--text-disabled)] disabled:border-[var(--border-color)] disabled:bg-[var(--bg-primary)] ${
        checked
          ? onStyle
          : "bg-[var(--bg-primary)] border-[var(--border-color)] text-[var(--text-secondary)]"
      }`}
    >
      {checked ? "ON" : "OFF"}
    </button>
  );
}
