import type { ButtonHTMLAttributes, ReactNode } from "react";
import { useUnavailable } from "./unavailable";

export type ButtonVariant = "primary" | "secondary" | "danger" | "ghost";
export type ButtonSize = "sm" | "md";

interface Props extends ButtonHTMLAttributes<HTMLButtonElement> {
  variant?: ButtonVariant;
  size?: ButtonSize;
  children: ReactNode;
  /**
   * Unavailable, but still announced. Renders `aria-disabled` and wires
   * `unavailableReason` to `aria-describedby` instead of using the native
   * `disabled` attribute, which would take the button out of the tab order and
   * out of the accessibility tree — reason and all. Clicks and Enter/Space are
   * guarded for you. Prefer this over `disabled` whenever there is a reason
   * worth telling the user.
   */
  unavailable?: boolean;
  /** Why the button cannot be used. Required for `unavailable` to say anything. */
  unavailableReason?: string;
}

/**
 * Real buttons with visible bounds and a ≥24px hit target.
 * Filled variants use the *-emphasis tokens so white text clears WCAG AA;
 * `--accent` stays reserved for foreground/link use.
 *
 * The `aria-disabled:` class mirrors below exist because Tailwind's
 * `disabled:` variant only matches the native attribute, which `unavailable`
 * deliberately does not set. Keep the two lists in step.
 */
const VARIANTS: Record<ButtonVariant, string> = {
  primary:
    "bg-[var(--accent-emphasis)] text-white border border-transparent hover:bg-[var(--accent-emphasis-hover)] disabled:bg-[var(--bg-tertiary)] disabled:text-[var(--text-disabled)] disabled:border-[var(--border-color)] aria-disabled:bg-[var(--bg-tertiary)] aria-disabled:text-[var(--text-disabled)] aria-disabled:border-[var(--border-color)] aria-disabled:hover:bg-[var(--bg-tertiary)]",
  secondary:
    "bg-[var(--bg-tertiary)] text-[var(--text-primary)] border border-[var(--border-color)] hover:bg-[var(--border-color)] disabled:text-[var(--text-disabled)] disabled:hover:bg-[var(--bg-tertiary)] aria-disabled:text-[var(--text-disabled)] aria-disabled:hover:bg-[var(--bg-tertiary)]",
  danger:
    "bg-transparent text-[var(--error)] border border-[var(--error)]/40 hover:bg-[var(--error-muted)] disabled:text-[var(--text-disabled)] disabled:border-[var(--border-color)] disabled:hover:bg-transparent aria-disabled:text-[var(--text-disabled)] aria-disabled:border-[var(--border-color)] aria-disabled:hover:bg-transparent",
  ghost:
    "bg-transparent text-[var(--text-secondary)] border border-transparent hover:text-[var(--text-primary)] hover:bg-[var(--bg-tertiary)] disabled:text-[var(--text-disabled)] disabled:hover:bg-transparent aria-disabled:text-[var(--text-disabled)] aria-disabled:hover:text-[var(--text-disabled)] aria-disabled:hover:bg-transparent",
};

const SIZES: Record<ButtonSize, string> = {
  sm: "h-6 px-2 text-xs gap-1",
  md: "h-8 px-3 text-[13px] gap-1.5",
};

export default function Button({
  variant = "secondary",
  size = "sm",
  className = "",
  type = "button",
  unavailable = false,
  unavailableReason = "",
  children,
  ...rest
}: Props) {
  const { controlProps, reasonNode } = useUnavailable({
    unavailable,
    reason: unavailableReason,
    onClick: rest.onClick,
    onKeyDown: rest.onKeyDown,
  });

  return (
    <>
      <button
        type={type}
        {...rest}
        {...controlProps}
        className={`inline-flex items-center justify-center whitespace-nowrap rounded-[var(--radius-control)] font-medium transition-colors disabled:cursor-not-allowed aria-disabled:cursor-not-allowed ${SIZES[size]} ${VARIANTS[variant]} ${className}`}
      >
        {children}
      </button>
      {/* Outside the button: inside, the reason would join its accessible name. */}
      {reasonNode}
    </>
  );
}
