import ApiKeyInput from "./ApiKeyInput";
import DockerSettings from "./DockerSettings";
import AwsSettings from "./AwsSettings";
import { useSettings } from "../../hooks/useSettings";

export default function SettingsPanel() {
  const { appSettings, saveSettings } = useSettings();

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
          value={appSettings?.global_claude_instructions ?? ""}
          onChange={async (e) => {
            if (!appSettings) return;
            await saveSettings({ ...appSettings, global_claude_instructions: e.target.value || null });
          }}
          placeholder="Instructions for Claude Code in all project containers..."
          rows={4}
          className="w-full px-2 py-1.5 text-xs bg-[var(--bg-primary)] border border-[var(--border-color)] rounded focus:outline-none focus:border-[var(--accent)] resize-y font-mono"
        />
      </div>
    </div>
  );
}
