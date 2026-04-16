import { useState, useEffect, useRef, useCallback } from "react";
import type { ClaudeCodeSettings } from "../../lib/types";

interface Props {
  settings: ClaudeCodeSettings | null;
  disabled: boolean;
  onSave: (settings: ClaudeCodeSettings | null) => Promise<void>;
  onClose: () => void;
}

const DEFAULTS: ClaudeCodeSettings = {
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

export default function ClaudeCodeSettingsModal({ settings, disabled, onSave, onClose }: Props) {
  const [local, setLocal] = useState<ClaudeCodeSettings>(settings ?? { ...DEFAULTS });
  const overlayRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const handleKeyDown = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    document.addEventListener("keydown", handleKeyDown);
    return () => document.removeEventListener("keydown", handleKeyDown);
  }, [onClose]);

  const handleOverlayClick = useCallback(
    (e: React.MouseEvent<HTMLDivElement>) => {
      if (e.target === overlayRef.current) onClose();
    },
    [onClose],
  );

  const update = async (patch: Partial<ClaudeCodeSettings>) => {
    const next = { ...local, ...patch };
    setLocal(next);
    try {
      await onSave(isAllDefaults(next) ? null : next);
    } catch (err) {
      console.error("Failed to save Claude Code settings:", err);
    }
  };

  const toggleButton = (label: string, description: string, value: boolean, onChange: (v: boolean) => void) => (
    <div className="flex items-center justify-between gap-4">
      <div className="min-w-0">
        <div className="text-sm font-medium text-[var(--text-primary)]">{label}</div>
        <div className="text-xs text-[var(--text-secondary)]">{description}</div>
      </div>
      <button
        onClick={() => onChange(!value)}
        disabled={disabled}
        className={`px-2 py-0.5 text-xs rounded transition-colors disabled:opacity-50 shrink-0 ${
          value
            ? "bg-[var(--success)] text-white"
            : "bg-[var(--bg-primary)] border border-[var(--border-color)] text-[var(--text-secondary)]"
        }`}
      >
        {value ? "ON" : "OFF"}
      </button>
    </div>
  );

  return (
    <div
      ref={overlayRef}
      onClick={handleOverlayClick}
      className="fixed inset-0 bg-black/50 flex items-center justify-center z-50"
    >
      <div className="bg-[var(--bg-secondary)] border border-[var(--border-color)] rounded-lg p-6 w-[32rem] shadow-xl max-h-[80vh] overflow-y-auto">
        <h2 className="text-lg font-semibold mb-4">Claude Code Settings</h2>

        {disabled && (
          <div className="px-2 py-1.5 mb-3 bg-[var(--warning)]/15 border border-[var(--warning)]/30 rounded text-xs text-[var(--warning)]">
            Container must be stopped to change Claude Code settings.
          </div>
        )}

        <div className="space-y-4 mb-6">
          {/* TUI Mode */}
          <div className="flex items-center justify-between gap-4">
            <div className="min-w-0">
              <div className="text-sm font-medium text-[var(--text-primary)]">TUI Mode</div>
              <div className="text-xs text-[var(--text-secondary)]">Enables flicker-free alt-screen rendering</div>
            </div>
            <select
              value={local.tui_mode ?? ""}
              onChange={(e) => update({ tui_mode: e.target.value || null })}
              disabled={disabled}
              className="px-2 py-1 bg-[var(--bg-primary)] border border-[var(--border-color)] rounded text-sm text-[var(--text-primary)] focus:outline-none focus:border-[var(--accent)] disabled:opacity-50 shrink-0"
            >
              <option value="">Default</option>
              <option value="fullscreen">Fullscreen</option>
            </select>
          </div>

          {/* Effort Level */}
          <div className="flex items-center justify-between gap-4">
            <div className="min-w-0">
              <div className="text-sm font-medium text-[var(--text-primary)]">Effort Level</div>
              <div className="text-xs text-[var(--text-secondary)]">Controls how much reasoning Claude applies</div>
            </div>
            <select
              value={local.effort ?? ""}
              onChange={(e) => update({ effort: e.target.value || null })}
              disabled={disabled}
              className="px-2 py-1 bg-[var(--bg-primary)] border border-[var(--border-color)] rounded text-sm text-[var(--text-primary)] focus:outline-none focus:border-[var(--accent)] disabled:opacity-50 shrink-0"
            >
              <option value="">Default</option>
              <option value="low">Low</option>
              <option value="medium">Medium</option>
              <option value="high">High</option>
            </select>
          </div>

          {/* Boolean toggles */}
          {toggleButton(
            "Focus Mode",
            "Collapses tool output to one-line summaries",
            local.focus_mode,
            (v) => update({ focus_mode: v }),
          )}

          {toggleButton(
            "Thinking Summaries",
            "Shows thinking process as summaries",
            local.show_thinking_summaries,
            (v) => update({ show_thinking_summaries: v }),
          )}

          {toggleButton(
            "Session Recap",
            "Provides context when returning to a session",
            local.enable_session_recap,
            (v) => update({ enable_session_recap: v }),
          )}

          {toggleButton(
            "Auto-Scroll Disabled",
            "Disables auto-scroll when in fullscreen TUI mode",
            local.auto_scroll_disabled,
            (v) => update({ auto_scroll_disabled: v }),
          )}

          {toggleButton(
            "Env Scrub",
            "Strips credentials from subprocess environments for security",
            local.env_scrub,
            (v) => update({ env_scrub: v }),
          )}

          {toggleButton(
            "Prompt Caching (1h)",
            "Enables 1-hour prompt cache TTL instead of 5 minutes",
            local.prompt_caching_1h,
            (v) => update({ prompt_caching_1h: v }),
          )}
        </div>

        <div className="flex justify-end">
          <button
            onClick={onClose}
            className="px-4 py-2 text-sm text-[var(--text-secondary)] hover:text-[var(--text-primary)] transition-colors"
          >
            Close
          </button>
        </div>
      </div>
    </div>
  );
}
