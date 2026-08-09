import type { ButtonHTMLAttributes, ReactNode } from "react";

export type ButtonVariant = "primary" | "secondary" | "danger" | "ghost";
export type ButtonSize = "sm" | "md";

interface Props extends ButtonHTMLAttributes<HTMLButtonElement> {
  variant?: ButtonVariant;
  size?: ButtonSize;
  children: ReactNode;
}

/**
 * Real buttons with visible bounds and a ≥24px hit target.
 * Filled variants use the *-emphasis tokens so white text clears WCAG AA;
 * `--accent` stays reserved for foreground/link use.
 */
const VARIANTS: Record<ButtonVariant, string> = {
  primary:
    "bg-[var(--accent-emphasis)] text-white border border-transparent hover:bg-[var(--accent-emphasis-hover)] disabled:bg-[var(--bg-tertiary)] disabled:text-[var(--text-disabled)] disabled:border-[var(--border-color)]",
  secondary:
    "bg-[var(--bg-tertiary)] text-[var(--text-primary)] border border-[var(--border-color)] hover:bg-[var(--border-color)] disabled:text-[var(--text-disabled)] disabled:hover:bg-[var(--bg-tertiary)]",
  danger:
    "bg-transparent text-[var(--error)] border border-[var(--error)]/40 hover:bg-[var(--error-muted)] disabled:text-[var(--text-disabled)] disabled:border-[var(--border-color)] disabled:hover:bg-transparent",
  ghost:
    "bg-transparent text-[var(--text-secondary)] border border-transparent hover:text-[var(--text-primary)] hover:bg-[var(--bg-tertiary)] disabled:text-[var(--text-disabled)] disabled:hover:bg-transparent",
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
  children,
  ...rest
}: Props) {
  return (
    <button
      type={type}
      {...rest}
      className={`inline-flex items-center justify-center whitespace-nowrap rounded-[var(--radius-control)] font-medium transition-colors disabled:cursor-not-allowed ${SIZES[size]} ${VARIANTS[variant]} ${className}`}
    >
      {children}
    </button>
  );
}
