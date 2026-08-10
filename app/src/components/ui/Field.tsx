import { useId, type ReactNode } from "react";

/**
 * Shared control styling. Full-width forms mean the helper text that used to
 * hide inside 27 hover-only tooltips can just be visible.
 */
export const inputClass =
  "w-full px-2.5 py-1.5 bg-[var(--bg-primary)] border border-[var(--border-color)] rounded-[var(--radius-control)] text-[13px] text-[var(--text-primary)] focus:border-[var(--accent)] disabled:text-[var(--text-disabled)] disabled:bg-[var(--bg-secondary)] transition-colors";

export const monoInputClass = `${inputClass} font-mono`;

export const selectClass =
  "px-2.5 py-1.5 bg-[var(--bg-primary)] border border-[var(--border-color)] rounded-[var(--radius-control)] text-[13px] text-[var(--text-primary)] focus:border-[var(--accent)] disabled:text-[var(--text-disabled)] disabled:bg-[var(--bg-secondary)] transition-colors";

interface FieldProps {
  label: string;
  /** Visible helper text — the replacement for hover-only tooltips. */
  hint?: ReactNode;
  children: (id: string) => ReactNode;
  className?: string;
}

export default function Field({ label, hint, children, className = "" }: FieldProps) {
  const id = useId();
  return (
    <div className={className}>
      <label
        htmlFor={id}
        className="block text-[13px] font-medium text-[var(--text-primary)]"
      >
        {label}
      </label>
      {hint && (
        <p className="mt-0.5 mb-1 text-xs text-[var(--text-secondary)] leading-snug">
          {hint}
        </p>
      )}
      <div className={hint ? "" : "mt-1"}>{children(id)}</div>
    </div>
  );
}

/** Label + helper text on the left, a control (usually a Toggle) on the right. */
export function SwitchRow({
  label,
  hint,
  control,
}: {
  label: string;
  hint?: ReactNode;
  control: ReactNode;
}) {
  return (
    <div className="flex items-start justify-between gap-4">
      <div className="min-w-0">
        <div className="text-[13px] font-medium text-[var(--text-primary)]">{label}</div>
        {hint && (
          <p className="mt-0.5 text-xs text-[var(--text-secondary)] leading-snug">{hint}</p>
        )}
      </div>
      <div className="flex-shrink-0 pt-0.5">{control}</div>
    </div>
  );
}

/** Grouping card used by the Config tab (Workspace / Model / Access / Runtime). */
export function ConfigGroup({
  title,
  description,
  children,
}: {
  title: string;
  description?: string;
  children: ReactNode;
}) {
  return (
    <section className="border border-[var(--border-color)] rounded-[var(--radius-panel)] bg-[var(--bg-secondary)]">
      <header className="px-4 py-2.5 border-b border-[var(--border-color)]">
        <h3 className="text-[11px] font-semibold uppercase tracking-wide text-[var(--text-secondary)]">
          {title}
        </h3>
        {description && (
          <p className="mt-0.5 text-xs text-[var(--text-secondary)]">{description}</p>
        )}
      </header>
      <div className="px-4 py-4 space-y-4">{children}</div>
    </section>
  );
}
