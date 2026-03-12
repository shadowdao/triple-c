import { useState, useEffect } from "react";
import DockerSettings from "./DockerSettings";
import AwsSettings from "./AwsSettings";
import { useSettings } from "../../hooks/useSettings";
import { useUpdates } from "../../hooks/useUpdates";
import ClaudeInstructionsModal from "../projects/ClaudeInstructionsModal";
import EnvVarsModal from "../projects/EnvVarsModal";
import { detectHostTimezone } from "../../lib/tauri-commands";
import type { EnvVar } from "../../lib/types";

export default function SettingsPanel() {
  const { appSettings, saveSettings } = useSettings();
  const { appVersion, imageUpdateInfo, checkForUpdates, checkImageUpdate } = useUpdates();
  const [globalInstructions, setGlobalInstructions] = useState(appSettings?.global_claude_instructions ?? "");
  const [globalEnvVars, setGlobalEnvVars] = useState<EnvVar[]>(appSettings?.global_custom_env_vars ?? []);
  const [checkingUpdates, setCheckingUpdates] = useState(false);
  const [timezone, setTimezone] = useState(appSettings?.timezone ?? "");
  const [showInstructionsModal, setShowInstructionsModal] = useState(false);
  const [showEnvVarsModal, setShowEnvVarsModal] = useState(false);

  // Sync local state when appSettings change
  useEffect(() => {
    setGlobalInstructions(appSettings?.global_claude_instructions ?? "");
    setGlobalEnvVars(appSettings?.global_custom_env_vars ?? []);
    setTimezone(appSettings?.timezone ?? "");
  }, [appSettings?.global_claude_instructions, appSettings?.global_custom_env_vars, appSettings?.timezone]);

  // Auto-detect timezone on first load if not yet set
  useEffect(() => {
    if (appSettings && !appSettings.timezone) {
      detectHostTimezone().then((tz) => {
        setTimezone(tz);
        saveSettings({ ...appSettings, timezone: tz });
      }).catch(() => {});
    }
  }, [appSettings?.timezone]);

  const handleCheckNow = async () => {
    setCheckingUpdates(true);
    try {
      await Promise.all([checkForUpdates(), checkImageUpdate()]);
    } finally {
      setCheckingUpdates(false);
    }
  };

  const handleAutoCheckToggle = async () => {
    if (!appSettings) return;
    await saveSettings({ ...appSettings, auto_check_updates: !appSettings.auto_check_updates });
  };

  return (
    <div className="p-4 space-y-6">
      <h2 className="text-xs font-semibold uppercase text-[var(--text-secondary)]">
        Settings
      </h2>
      <DockerSettings />
      <AwsSettings />

      {/* Container Timezone */}
      <div>
        <label className="block text-sm font-medium mb-1">Container Timezone</label>
        <p className="text-xs text-[var(--text-secondary)] mb-1.5">
          Timezone for containers — affects scheduled task timing (IANA format, e.g. America/New_York)
        </p>
        <input
          type="text"
          value={timezone}
          onChange={(e) => setTimezone(e.target.value)}
          onBlur={async () => {
            if (appSettings) {
              await saveSettings({ ...appSettings, timezone: timezone || null });
            }
          }}
          placeholder="UTC"
          className="w-full px-2 py-1 text-sm bg-[var(--bg-primary)] border border-[var(--border-color)] rounded focus:outline-none focus:border-[var(--accent)]"
        />
      </div>

      {/* Global Claude Instructions */}
      <div>
        <label className="block text-sm font-medium mb-1">Claude Instructions</label>
        <p className="text-xs text-[var(--text-secondary)] mb-1.5">
          Global instructions applied to all projects (written to ~/.claude/CLAUDE.md in containers)
        </p>
        <div className="flex items-center justify-between">
          <span className="text-xs text-[var(--text-secondary)]">
            {globalInstructions ? "Configured" : "Not set"}
          </span>
          <button
            onClick={() => setShowInstructionsModal(true)}
            className="text-xs px-2 py-0.5 text-[var(--accent)] hover:text-[var(--accent-hover)] hover:bg-[var(--bg-primary)] rounded transition-colors"
          >
            Edit
          </button>
        </div>
      </div>

      {/* Global Environment Variables */}
      <div>
        <label className="block text-sm font-medium mb-1">Global Environment Variables</label>
        <p className="text-xs text-[var(--text-secondary)] mb-1.5">
          Applied to all project containers. Per-project variables override global ones with the same key.
        </p>
        <div className="flex items-center justify-between">
          <span className="text-xs text-[var(--text-secondary)]">
            {globalEnvVars.length > 0 ? `${globalEnvVars.length} variable${globalEnvVars.length === 1 ? "" : "s"}` : "None"}
          </span>
          <button
            onClick={() => setShowEnvVarsModal(true)}
            className="text-xs px-2 py-0.5 text-[var(--accent)] hover:text-[var(--accent-hover)] hover:bg-[var(--bg-primary)] rounded transition-colors"
          >
            Edit
          </button>
        </div>
      </div>

      {/* Updates section */}
      <div>
        <label className="block text-sm font-medium mb-2">Updates</label>
        <div className="space-y-2">
          {appVersion && (
            <p className="text-xs text-[var(--text-secondary)]">
              Current version: <span className="text-[var(--text-primary)] font-mono">{appVersion}</span>
            </p>
          )}
          <div className="flex items-center gap-2">
            <label className="text-xs text-[var(--text-secondary)]">Auto-check for updates</label>
            <button
              onClick={handleAutoCheckToggle}
              className={`px-2 py-0.5 text-xs rounded transition-colors ${
                appSettings?.auto_check_updates !== false
                  ? "bg-[var(--success)] text-white"
                  : "bg-[var(--bg-primary)] border border-[var(--border-color)] text-[var(--text-secondary)]"
              }`}
            >
              {appSettings?.auto_check_updates !== false ? "ON" : "OFF"}
            </button>
          </div>
          <button
            onClick={handleCheckNow}
            disabled={checkingUpdates}
            className="px-3 py-1.5 text-xs bg-[var(--bg-primary)] border border-[var(--border-color)] rounded hover:bg-[var(--border-color)] disabled:opacity-50 transition-colors"
          >
            {checkingUpdates ? "Checking..." : "Check now"}
          </button>
          {imageUpdateInfo && (
            <div className="flex items-center gap-2 px-3 py-2 text-xs bg-[var(--bg-primary)] border border-[var(--warning,#f59e0b)] rounded">
              <span className="inline-block w-2 h-2 rounded-full bg-[var(--warning,#f59e0b)]" />
              <span>A newer container image is available. Re-pull the image in Docker settings above to update.</span>
            </div>
          )}
        </div>
      </div>

      {showInstructionsModal && (
        <ClaudeInstructionsModal
          instructions={globalInstructions}
          disabled={false}
          onSave={async (instructions) => {
            setGlobalInstructions(instructions);
            if (appSettings) {
              await saveSettings({ ...appSettings, global_claude_instructions: instructions || null });
            }
          }}
          onClose={() => setShowInstructionsModal(false)}
        />
      )}

      {showEnvVarsModal && (
        <EnvVarsModal
          envVars={globalEnvVars}
          disabled={false}
          onSave={async (vars) => {
            setGlobalEnvVars(vars);
            if (appSettings) {
              await saveSettings({ ...appSettings, global_custom_env_vars: vars });
            }
          }}
          onClose={() => setShowEnvVarsModal(false)}
        />
      )}
    </div>
  );
}
