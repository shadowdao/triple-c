import { useRef } from "react";

export interface Segment<T extends string> {
  value: T;
  label: string;
  /** Visible helper text under the control when this segment is selected. */
  hint?: string;
  /** Paint this segment with the caution treatment when selected. */
  caution?: boolean;
}

interface Props<T extends string> {
  /** Accessible group name. */
  label: string;
  segments: Segment<T>[];
  value: T;
  onChange: (value: T) => void;
  disabled?: boolean;
  className?: string;
}

/**
 * Roving-tabindex radio group rendered as a segmented control.
 * Arrow keys move between segments; only the selected one is tabbable.
 */
export default function SegmentedControl<T extends string>({
  label,
  segments,
  value,
  onChange,
  disabled = false,
  className = "",
}: Props<T>) {
  const groupRef = useRef<HTMLDivElement>(null);

  const move = (delta: number) => {
    const index = segments.findIndex((s) => s.value === value);
    const next = segments[(index + delta + segments.length) % segments.length];
    if (!next) return;
    onChange(next.value);
    requestAnimationFrame(() => {
      groupRef.current
        ?.querySelector<HTMLElement>(`[data-segment="${next.value}"]`)
        ?.focus();
    });
  };

  return (
    <div
      ref={groupRef}
      role="radiogroup"
      aria-label={label}
      className={`inline-flex p-0.5 gap-0.5 bg-[var(--bg-primary)] border border-[var(--border-color)] rounded-[var(--radius-control)] ${className}`}
      onKeyDown={(e) => {
        if (disabled) return;
        if (e.key === "ArrowRight" || e.key === "ArrowDown") {
          e.preventDefault();
          move(1);
        } else if (e.key === "ArrowLeft" || e.key === "ArrowUp") {
          e.preventDefault();
          move(-1);
        }
      }}
    >
      {segments.map((segment) => {
        const selected = segment.value === value;
        let cls =
          "text-[var(--text-secondary)] hover:text-[var(--text-primary)] hover:bg-[var(--bg-tertiary)]";
        if (selected) {
          cls = segment.caution
            ? "bg-[var(--warning-emphasis)] text-white"
            : "bg-[var(--accent-emphasis)] text-white";
        }
        return (
          <button
            key={segment.value}
            type="button"
            role="radio"
            data-segment={segment.value}
            aria-checked={selected}
            tabIndex={selected ? 0 : -1}
            disabled={disabled}
            onClick={() => onChange(segment.value)}
            className={`h-6 px-2.5 text-xs font-medium rounded-[4px] transition-colors disabled:text-[var(--text-disabled)] disabled:hover:bg-transparent ${cls}`}
          >
            {segment.label}
          </button>
        );
      })}
    </div>
  );
}
