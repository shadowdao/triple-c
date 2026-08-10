import type { SaveState } from "../../hooks/useSaveState";

/**
 * Visible outcome for save-on-blur. Config writes used to fail silently into
 * `console.error`, which is silent data loss.
 */
export default function SaveIndicator({ state }: { state: SaveState }) {
  if (state.status === "idle") return null;

  const map = {
    saving: { text: "Saving…", color: "var(--text-secondary)" },
    saved: { text: "Saved ✓", color: "var(--success)" },
    failed: { text: "Save failed ✕", color: "var(--error)" },
  } as const;
  const tone = map[state.status];

  return (
    <span
      role="status"
      aria-live="polite"
      className="text-xs font-medium"
      style={{ color: tone.color }}
      title={state.error ?? undefined}
    >
      {tone.text}
    </span>
  );
}
