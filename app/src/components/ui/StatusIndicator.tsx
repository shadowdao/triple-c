import type { ProjectStatus } from "../../lib/types";

/**
 * Status is never encoded by hue alone: every tone carries a distinct glyph
 * shape, and (unless `iconOnly`) a word.
 */
export type StatusTone =
  | "running"
  | "stopped"
  | "busy"
  | "error"
  | "unknown"
  | "ok"
  | "off";

interface ToneStyle {
  glyph: string;
  color: string;
  pulse?: boolean;
}

const TONES: Record<StatusTone, ToneStyle> = {
  running: { glyph: "●", color: "var(--success)" },
  ok: { glyph: "●", color: "var(--success)" },
  stopped: { glyph: "○", color: "var(--text-secondary)" },
  off: { glyph: "○", color: "var(--text-secondary)" },
  busy: { glyph: "◐", color: "var(--warning)", pulse: true },
  error: { glyph: "▲", color: "var(--error)" },
  // Still being checked — distinct from "unavailable", and it pulses.
  unknown: { glyph: "◌", color: "var(--text-disabled)", pulse: true },
};

export const PROJECT_STATUS_TONE: Record<ProjectStatus, StatusTone> = {
  running: "running",
  stopped: "stopped",
  starting: "busy",
  stopping: "busy",
  error: "error",
};

export const PROJECT_STATUS_LABEL: Record<ProjectStatus, string> = {
  running: "Running",
  stopped: "Stopped",
  starting: "Starting",
  stopping: "Stopping",
  error: "Error",
};

interface Props {
  tone: StatusTone;
  label: string;
  /** Render the glyph only; `label` still ships as accessible text. */
  iconOnly?: boolean;
  className?: string;
  title?: string;
  /** Let a long label (a failure summary) wrap instead of overflowing its container. */
  wrap?: boolean;
}

export default function StatusIndicator({
  tone,
  label,
  iconOnly = false,
  className = "",
  title,
  wrap = false,
}: Props) {
  const style = TONES[tone];
  return (
    <span
      className={`inline-flex gap-1 ${wrap ? "items-baseline whitespace-normal break-words min-w-0 max-w-full" : "items-center whitespace-nowrap"} ${className}`}
      title={title ?? label}
    >
      <span
        aria-hidden="true"
        className={`leading-none text-[10px] ${style.pulse ? "animate-status-pulse" : ""}`}
        style={{ color: style.color }}
      >
        {style.glyph}
      </span>
      {iconOnly ? (
        <span className="sr-only">{label}</span>
      ) : (
        <span style={{ color: style.color }}>{label}</span>
      )}
    </span>
  );
}

/** Status pill for a project, derived from `ProjectStatus`. */
export function ProjectStatusIndicator({
  status,
  iconOnly,
  className,
}: {
  status: ProjectStatus;
  iconOnly?: boolean;
  className?: string;
}) {
  return (
    <StatusIndicator
      tone={PROJECT_STATUS_TONE[status]}
      label={PROJECT_STATUS_LABEL[status]}
      iconOnly={iconOnly}
      className={className}
    />
  );
}
