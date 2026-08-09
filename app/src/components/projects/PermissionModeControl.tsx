import type { PermissionMode, Project } from "../../lib/types";
import SegmentedControl, { type Segment } from "../ui/SegmentedControl";

export const PERMISSION_MODES: Segment<PermissionMode>[] = [
  { value: "plan", label: "Plan", hint: "Claude proposes a plan and makes no changes." },
  { value: "default", label: "Default", hint: "Claude asks before each tool call." },
  {
    value: "acceptEdits",
    label: "Accept Edits",
    hint: "File edits are auto-approved; other tools still prompt.",
  },
  {
    value: "bypass",
    label: "Bypass",
    hint: "Every tool call is auto-approved (--dangerously-skip-permissions).",
  },
];

/**
 * `permission_mode` is nullable for projects saved before it existed; fall back
 * to the legacy boolean.
 */
export function effectivePermissionMode(project: Project): PermissionMode {
  return project.permission_mode ?? (project.full_permissions ? "bypass" : "default");
}

/**
 * The patch to apply when the user picks a mode. `full_permissions` is kept in
 * sync so anything still reading the legacy field cannot drift.
 */
export function permissionModePatch(mode: PermissionMode): Partial<Project> {
  return { permission_mode: mode, full_permissions: mode === "bypass" };
}

interface Props {
  project: Project;
  onChange: (mode: PermissionMode) => void;
  disabled?: boolean;
  /** Explanation of why the control is disabled, shown beneath it. */
  disabledReason?: string;
}

/**
 * The hero control. Per §B3.3: Bypass is only painted as caution when the
 * sandbox is OFF — with the sandbox ON, bypassing prompts is contained.
 */
export default function PermissionModeControl({
  project,
  onChange,
  disabled = false,
  disabledReason,
}: Props) {
  const mode = effectivePermissionMode(project);
  const sandboxOn = project.sandbox_mode_enabled;
  const uncontainedBypass = mode === "bypass" && !sandboxOn;

  const segments = PERMISSION_MODES.map((segment) =>
    segment.value === "bypass" ? { ...segment, caution: !sandboxOn } : segment,
  );

  const active = PERMISSION_MODES.find((s) => s.value === mode);

  return (
    <div className="space-y-2">
      <div className="flex flex-wrap items-center gap-x-4 gap-y-2">
        <span className="text-[11px] font-semibold uppercase tracking-wide text-[var(--text-secondary)]">
          Permission mode
        </span>
        <SegmentedControl
          label="Permission mode"
          segments={segments}
          value={mode}
          onChange={onChange}
          disabled={disabled}
        />
        <span
          className="text-xs text-[var(--text-secondary)]"
          data-testid="sandbox-state"
        >
          Sandbox{" "}
          <span
            className={
              sandboxOn ? "text-[var(--success)] font-semibold" : "text-[var(--warning)] font-semibold"
            }
          >
            {sandboxOn ? "ON" : "OFF"}
          </span>
          {sandboxOn ? " — bubblewrap isolation" : " — no filesystem/network isolation"}
        </span>
      </div>

      <p
        className={`text-xs leading-snug ${
          uncontainedBypass ? "text-[var(--warning)]" : "text-[var(--text-secondary)]"
        }`}
        data-testid="permission-mode-hint"
      >
        {uncontainedBypass
          ? "Caution: every tool call is auto-approved and the sandbox is off, so nothing contains what Claude runs."
          : mode === "bypass"
            ? "Every tool call is auto-approved — contained by the sandbox."
            : (active?.hint ?? "")}
      </p>

      {project.status === "running" && (
        <p className="text-xs text-[var(--text-disabled)]">
          Applies to terminals opened from now on.
        </p>
      )}

      {disabled && disabledReason && (
        <p className="text-xs text-[var(--text-disabled)]">{disabledReason}</p>
      )}
    </div>
  );
}
