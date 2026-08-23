import { useEffect, useState } from "react";
import type { ClaudeCodeSettings } from "../../lib/types";
import Toggle from "../ui/Toggle";
import { SwitchRow, selectClass } from "../ui/Field";

interface Props {
  settings: ClaudeCodeSettings | null;
  disabled: boolean;
  disabledReason?: string;
  onSave: (settings: ClaudeCodeSettings | null) => Promise<unknown>;
  /**
   * `"project"` adds a third "Global" state to every switch, because a project
   * has somewhere to inherit *from*. The global editor has no such fallback —
   * unset there just means Claude Code's own default — so it stays a plain
   * on/off and never renders the extra choice.
   */
  scope?: "global" | "project";
}

export const CLAUDE_CODE_DEFAULTS: ClaudeCodeSettings = {
  tui_mode: null,
  effort: null,
  auto_scroll_disabled: null,
  focus_mode: null,
  show_thinking_summaries: null,
  session_recap_disabled: null,
  env_scrub: null,
  prompt_caching_1h: null,
};

/**
 * "Nothing is set at this level", which is saved as `null` rather than as a
 * struct of nulls so that a project with no opinion is indistinguishable from
 * one that never opened this editor.
 *
 * Note `false` is *not* a default any more: it is a deliberate off that
 * overrides a global on, so a settings object holding one has to be persisted.
 */
function isAllDefaults(s: ClaudeCodeSettings): boolean {
  return (
    s.tui_mode === null &&
    s.effort === null &&
    s.auto_scroll_disabled === null &&
    s.focus_mode === null &&
    s.show_thinking_summaries === null &&
    s.session_recap_disabled === null &&
    s.env_scrub === null &&
    s.prompt_caching_1h === null
  );
}

/**
 * Two of Claude Code's settings are **on by default**, so the field behind them
 * stores the *disabled* sense (`auto_scroll_disabled`, `session_recap_disabled`)
 * — that is what makes an untouched project mean "leave Claude Code alone"
 * rather than "the user turned this off". `invert` is what lets those still
 * read as an ordinary on/off switch here: the toggle shows the feature's state,
 * the field stores the deviation from the default.
 */
const BOOLEAN_FIELDS: {
  key: keyof Omit<ClaudeCodeSettings, "tui_mode" | "effort">;
  label: string;
  hint: string;
  invert?: boolean;
}[] = [
  { key: "focus_mode", label: "Focus mode", hint: "Collapses tool output to one-line summaries." },
  {
    key: "show_thinking_summaries",
    label: "Thinking summaries",
    hint: "Shows Claude's thinking process as summaries.",
  },
  {
    key: "session_recap_disabled",
    label: "Session recap",
    hint: "Shows a one-line recap when you return to the terminal after a few minutes away.",
    invert: true,
  },
  {
    key: "auto_scroll_disabled",
    label: "Auto-scroll",
    hint: "Follows new output to the bottom in fullscreen rendering.",
    invert: true,
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
  scope = "global",
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

      {/*
        Three states, not two. Leaving `tui` unset is what lets Claude Code pick
        the renderer for itself, which is not the same as pinning the classic
        one — and the key is now always written (or explicitly deleted), so
        "Automatic" has to be selectable rather than merely being what you get
        when nothing is emitted.
      */}
      <SwitchRow
        label="TUI mode"
        hint="Classic renders in your terminal's scrollback; fullscreen is the flicker-free alt-screen."
        control={
          <select
            value={local.tui_mode ?? ""}
            aria-label="TUI mode"
            onChange={(e) => apply({ tui_mode: e.target.value || null })}
            disabled={disabled}
            className={selectClass}
          >
            <option value="">Automatic</option>
            <option value="default">Classic</option>
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
            <option value="xhigh">Extra high</option>
          </select>
        }
      />

      {BOOLEAN_FIELDS.map(({ key, label, hint, invert }) => {
        const stored = local[key];

        if (scope === "global") {
          // No level above this one to inherit from, so "unset" and "off" are
          // the same instruction here and a plain switch is the honest control.
          // Unset therefore has to *display* as Claude Code's own default —
          // which for the two inverted fields is on, not off.
          const checked = invert ? stored !== true : stored === true;
          return (
            <SwitchRow
              key={key}
              label={label}
              hint={hint}
              control={
                <Toggle
                  label={label}
                  checked={checked}
                  disabled={disabled}
                  onChange={(v) => {
                    // Collapse back to null at the default rather than storing
                    // a redundant `false`, so an untouched global stays
                    // indistinguishable from one that was never opened.
                    const atDefault = invert ? v : !v;
                    apply({
                      [key]: atDefault ? null : invert ? !v : v,
                    } as Partial<ClaudeCodeSettings>);
                  }}
                />
              }
            />
          );
        }

        // `stored` holds the deviation from Claude Code's default, so an
        // inverted field reads back the other way round — see BOOLEAN_FIELDS.
        const selected =
          stored === null ? "global" : (invert ? !stored : stored) ? "on" : "off";

        return (
          <SwitchRow
            key={key}
            label={label}
            hint={hint}
            control={
              <select
                value={selected}
                aria-label={label}
                disabled={disabled}
                onChange={(e) => {
                  const choice = e.target.value;
                  const next =
                    choice === "global" ? null : invert ? choice === "off" : choice === "on";
                  apply({ [key]: next } as Partial<ClaudeCodeSettings>);
                }}
                className={selectClass}
              >
                <option value="global">Global</option>
                <option value="off">Off</option>
                <option value="on">On</option>
              </select>
            }
          />
        );
      })}
    </div>
  );
}
