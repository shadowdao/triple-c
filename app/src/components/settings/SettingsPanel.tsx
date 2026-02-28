import { useState, useEffect } from "react";
import ApiKeyInput from "./ApiKeyInput";
import DockerSettings from "./DockerSettings";
import AwsSettings from "./AwsSettings";
import { useSettings } from "../../hooks/useSettings";
import { useUpdates } from "../../hooks/useUpdates";

export default function SettingsPanel() {
  const { appSettings, saveSettings } = useSettings();
  const { appVersion, checkForUpdates } = useUpdates();
  const [globalInstructions, setGlobalInstructions] = useState(appSettings?.global_claude_instructions ?? "");
  const [checkingUpdates, setCheckingUpdates] = useState(false);

  // Sync local state when appSettings change
  useEffect(() => {
    setGlobalInstructions(appSettings?.global_claude_instructions ?? "");
  }, [appSettings?.global_claude_instructions]);

  const handleInstructionsBlur = async () => {
    if (!appSettings) return;
    await saveSettings({ ...appSettings, global_claude_instructions: globalInstructions || null });
  };

  const handleCheckNow = async () => {
    setCheckingUpdates(true);
    try {
      await checkForUpdates();
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
      <ApiKeyInput />
      <DockerSettings />
      <AwsSettings />
      <div>
        <label className="block text-sm font-medium mb-2">Claude Instructions</label>
        <p className="text-xs text-[var(--text-secondary)] mb-1.5">
          Global instructions applied to all projects (written to ~/.claude/CLAUDE.md in containers)
        </p>
        <textarea
          value={globalInstructions}
          onChange={(e) => setGlobalInstructions(e.target.value)}
          onBlur={handleInstructionsBlur}
          placeholder="Instructions for Claude Code in all project containers..."
          rows={4}
          className="w-full px-2 py-1.5 text-xs bg-[var(--bg-primary)] border border-[var(--border-color)] rounded focus:outline-none focus:border-[var(--accent)] resize-y font-mono"
        />
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
        </div>
      </div>
    </div>
  );
}
