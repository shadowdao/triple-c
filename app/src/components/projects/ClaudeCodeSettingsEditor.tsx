import { useEffect, useState } from "react";
import type { ClaudeCodeSettings } from "../../lib/types";
import Toggle from "../ui/Toggle";
import { SwitchRow, selectClass } from "../ui/Field";

interface Props {
  settings: ClaudeCodeSettings | null;
  disabled: boolean;
  disabledReason?: string;
  onSave: (settings: ClaudeCodeSettings | null) => Promise<unknown>;
}

export const CLAUDE_CODE_DEFAULTS: ClaudeCodeSettings = {
  tui_mode: null,
  effort: null,
  auto_scroll_disabled: false,
  focus_mode: false,
  show_thinking_summaries: false,
  enable_session_recap: false,
  env_scrub: false,
  prompt_caching_1h: false,
};

function isAllDefaults(s: ClaudeCodeSettings): boolean {
  return (
    s.tui_mode === null &&
    s.effort === null &&
    s.auto_scroll_disabled === false &&
    s.focus_mode === false &&
    s.show_thinking_summaries === false &&
    s.enable_session_recap === false &&
    s.env_scrub === false &&
    s.prompt_caching_1h === false
  );
}

const BOOLEAN_FIELDS: {
  key: keyof Omit<ClaudeCodeSettings, "tui_mode" | "effort">;
  label: string;
  hint: string;
}[] = [
  { key: "focus_mode", label: "Focus mode", hint: "Collapses tool output to one-line summaries." },
  {
    key: "show_thinking_summaries",
    label: "Thinking summaries",
    hint: "Shows Claude's thinking process as summaries.",
  },
  {
    key: "enable_session_recap",
    label: "Session recap",
    hint: "Provides context when returning to a session.",
  },
  {
    key: "auto_scroll_disabled",
    label: "Auto-scroll disabled",
    hint: "Disables auto-scroll when in fullscreen TUI mode.",
  },
  {
    key: "env_scrub",
    label: "Env scrub",
    hint: "Strips credentials from subprocess environments.",
  },
  {
    key: "prompt_caching_1h",
    label: "Prompt caching (1h)",
    hint: "Uses a 1-hour prompt cache TTL instead of 5 minutes.",
  },
];

export default function ClaudeCodeSettingsEditor({
  settings,
  disabled,
  disabledReason,
  onSave,
}: Props) {
  const [local, setLocal] = useState<ClaudeCodeSettings>(
    settings ?? { ...CLAUDE_CODE_DEFAULTS },
  );

  useEffect(() => {
    setLocal(settings ?? { ...CLAUDE_CODE_DEFAULTS });
  }, [settings]);

  const apply = (patch: Partial<ClaudeCodeSettings>) => {
    const next = { ...local, ...patch };
    setLocal(next);
    onSave(isAllDefaults(next) ? null : next);
  };

  return (
    <div className="space-y-4">
      {disabled && disabledReason && (
        <p className="px-2 py-1.5 bg-[var(--warning-muted)] border border-[var(--warning)]/30 rounded-[var(--radius-control)] text-xs text-[var(--warning)]">
          {disabledReason}
        </p>
      )}

      <SwitchRow
        label="TUI mode"
        hint="Enables flicker-free alt-screen rendering."
        control={
          <select
            value={local.tui_mode ?? ""}
            aria-label="TUI mode"
            onChange={(e) => apply({ tui_mode: e.target.value || null })}
            disabled={disabled}
            className={selectClass}
          >
            <option value="">Default</option>
            <option value="fullscreen">Fullscreen</option>
          </select>
        }
      />

      <SwitchRow
        label="Effort level"
        hint="Controls how much reasoning Claude applies."
        control={
          <select
            value={local.effort ?? ""}
            aria-label="Effort level"
            onChange={(e) => apply({ effort: e.target.value || null })}
            disabled={disabled}
            className={selectClass}
          >
            <option value="">Default</option>
            <option value="low">Low</option>
            <option value="medium">Medium</option>
            <option value="high">High</option>
          </select>
        }
      />

      {BOOLEAN_FIELDS.map(({ key, label, hint }) => (
        <SwitchRow
          key={key}
          label={label}
          hint={hint}
          control={
            <Toggle
              label={label}
              checked={local[key]}
              disabled={disabled}
              onChange={(v) => apply({ [key]: v } as Partial<ClaudeCodeSettings>)}
            />
          }
        />
      ))}
    </div>
  );
}
