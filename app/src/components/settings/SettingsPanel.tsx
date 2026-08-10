import { useState, useEffect } from "react";
import DockerSettings from "./DockerSettings";
import AwsSettings from "./AwsSettings";
import OllamaSettings from "./OllamaSettings";
import OpenAiCompatibleSettings from "./OpenAiCompatibleSettings";
import { useSettings } from "../../hooks/useSettings";
import { useUpdates } from "../../hooks/useUpdates";
import ClaudeInstructionsModal from "../projects/ClaudeInstructionsModal";
import ClaudeCodeSettingsModal from "../projects/ClaudeCodeSettingsModal";
import EnvVarsModal from "../projects/EnvVarsModal";
import { detectHostTimezone } from "../../lib/tauri-commands";
import type { EnvVar } from "../../lib/types";
import Tooltip from "../ui/Tooltip";
import AccordionSection from "../ui/AccordionSection";
import Toggle from "../ui/Toggle";
import WebTerminalSettings from "./WebTerminalSettings";
import SttSettings from "./SttSettings";
import SharedAuthSettings from "./SharedAuthSettings";

export default function SettingsPanel() {
  const { appSettings, saveSettings } = useSettings();
  const { appVersion, imageUpdateInfo, checkForUpdates, checkImageUpdate } = useUpdates();
  const [globalInstructions, setGlobalInstructions] = useState(appSettings?.global_claude_instructions ?? "");
  const [globalEnvVars, setGlobalEnvVars] = useState<EnvVar[]>(appSettings?.global_custom_env_vars ?? []);
  const [checkingUpdates, setCheckingUpdates] = useState(false);
  const [timezone, setTimezone] = useState(appSettings?.timezone ?? "");
  const [sshKeyPath, setSshKeyPath] = useState(appSettings?.default_ssh_key_path ?? "");
  const [gitName, setGitName] = useState(appSettings?.default_git_user_name ?? "");
  const [gitEmail, setGitEmail] = useState(appSettings?.default_git_user_email ?? "");
  const [showInstructionsModal, setShowInstructionsModal] = useState(false);
  const [showEnvVarsModal, setShowEnvVarsModal] = useState(false);
  const [showClaudeCodeSettingsModal, setShowClaudeCodeSettingsModal] = useState(false);

  // Sync local state when appSettings change
  useEffect(() => {
    setGlobalInstructions(appSettings?.global_claude_instructions ?? "");
    setGlobalEnvVars(appSettings?.global_custom_env_vars ?? []);
    setTimezone(appSettings?.timezone ?? "");
    setSshKeyPath(appSettings?.default_ssh_key_path ?? "");
    setGitName(appSettings?.default_git_user_name ?? "");
    setGitEmail(appSettings?.default_git_user_email ?? "");
  }, [appSettings?.global_claude_instructions, appSettings?.global_custom_env_vars, appSettings?.timezone, appSettings?.default_ssh_key_path, appSettings?.default_git_user_name, appSettings?.default_git_user_email]);

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
    <div className="p-4 space-y-3">
      <h2 className="text-xs font-semibold uppercase text-[var(--text-secondary)]">
        Settings
      </h2>

      <AccordionSection id="general" title="General">
        {/* Container Timezone */}
        <div>
          <label className="block text-sm font-medium mb-1">Container Timezone<Tooltip text="Sets the timezone inside containers. Affects scheduled task timing and log timestamps." /></label>
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
            className="w-full px-2 py-1 text-sm bg-[var(--bg-primary)] border border-[var(--border-color)] rounded focus:border-[var(--accent)]"
          />
        </div>

        {/* Global Claude Instructions */}
        <div>
          <label className="block text-sm font-medium mb-1">Claude Instructions<Tooltip text="Global instructions applied to all projects. Written to ~/.claude/CLAUDE.md in every container." /></label>
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
          <label className="block text-sm font-medium mb-1">Global Environment Variables<Tooltip text="Env vars injected into all containers. Per-project vars with the same key take precedence." /></label>
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

        {/* Global Claude Code Settings */}
        <div>
          <label className="block text-sm font-medium mb-1">Claude Code Settings<Tooltip text="Global defaults for Claude Code CLI behavior (TUI mode, effort, focus mode, caching, etc.). Per-project settings override these." /></label>
          <p className="text-xs text-[var(--text-secondary)] mb-1.5">
            Default Claude Code CLI settings applied to all projects. Per-project settings take precedence.
          </p>
          <div className="flex items-center justify-between">
            <span className="text-xs text-[var(--text-secondary)]">
              {appSettings?.global_claude_code_settings ? "Configured" : "Using defaults"}
            </span>
            <button
              onClick={() => setShowClaudeCodeSettingsModal(true)}
              className="text-xs px-2 py-0.5 text-[var(--accent)] hover:text-[var(--accent-hover)] hover:bg-[var(--bg-primary)] rounded transition-colors"
            >
              Edit
            </button>
          </div>
        </div>
      </AccordionSection>

      <AccordionSection id="claude-auth" title="Claude Authentication" defaultOpen={false}>
        <SharedAuthSettings />
      </AccordionSection>

      <AccordionSection id="backends" title="Backends" defaultOpen={false}>
        <AwsSettings />
        <div className="pt-3 border-t border-[var(--border-color)]" />
        <OllamaSettings />
        <div className="pt-3 border-t border-[var(--border-color)]" />
        <OpenAiCompatibleSettings />
      </AccordionSection>

      <AccordionSection id="container" title="Container" defaultOpen={false}>
        <DockerSettings />
      </AccordionSection>

      <AccordionSection id="git-ssh" title="Git / SSH" defaultOpen={false}>
        {/* Default SSH Key Directory */}
        <div>
          <label className="block text-sm font-medium mb-1">Default SSH Key Directory<Tooltip text="Global default SSH key directory. Mounted into containers that don't have a per-project SSH path set." /></label>
          <p className="text-xs text-[var(--text-secondary)] mb-1.5">
            Mounted into all containers unless overridden by a per-project setting.
          </p>
          <input
            type="text"
            value={sshKeyPath}
            onChange={(e) => setSshKeyPath(e.target.value)}
            onBlur={async () => {
              if (appSettings) {
                await saveSettings({ ...appSettings, default_ssh_key_path: sshKeyPath || null });
              }
            }}
            placeholder="~/.ssh"
            className="w-full px-2 py-1 text-sm bg-[var(--bg-primary)] border border-[var(--border-color)] rounded focus:border-[var(--accent)]"
          />
        </div>

        {/* Default Git Name */}
        <div>
          <label className="block text-sm font-medium mb-1">Default Git Name<Tooltip text="Sets git user.name inside containers. Per-project Git Name takes precedence." /></label>
          <input
            type="text"
            value={gitName}
            onChange={(e) => setGitName(e.target.value)}
            onBlur={async () => {
              if (appSettings) {
                await saveSettings({ ...appSettings, default_git_user_name: gitName || null });
              }
            }}
            placeholder="Your Name"
            className="w-full px-2 py-1 text-sm bg-[var(--bg-primary)] border border-[var(--border-color)] rounded focus:border-[var(--accent)]"
          />
        </div>

        {/* Default Git Email */}
        <div>
          <label className="block text-sm font-medium mb-1">Default Git Email<Tooltip text="Sets git user.email inside containers. Per-project Git Email takes precedence." /></label>
          <input
            type="text"
            value={gitEmail}
            onChange={(e) => setGitEmail(e.target.value)}
            onBlur={async () => {
              if (appSettings) {
                await saveSettings({ ...appSettings, default_git_user_email: gitEmail || null });
              }
            }}
            placeholder="you@example.com"
            className="w-full px-2 py-1 text-sm bg-[var(--bg-primary)] border border-[var(--border-color)] rounded focus:border-[var(--accent)]"
          />
        </div>
      </AccordionSection>

      <AccordionSection id="tools" title="Tools" defaultOpen={false}>
        <WebTerminalSettings />
        <SttSettings />
      </AccordionSection>

      <AccordionSection id="updates" title="Updates" defaultOpen={false}>
        <div className="space-y-2">
          {appVersion && (
            <p className="text-xs text-[var(--text-secondary)]">
              Current version: <span className="text-[var(--text-primary)] font-mono">{appVersion}</span>
            </p>
          )}
          <div className="flex items-center gap-2">
            <label className="text-xs text-[var(--text-secondary)]">Auto-check for updates</label>
            <Toggle
              label="Auto-check for updates"
              checked={appSettings?.auto_check_updates !== false}
              onChange={handleAutoCheckToggle}
            />
          </div>
          <button
            onClick={handleCheckNow}
            disabled={checkingUpdates}
            className="px-3 py-1.5 text-xs bg-[var(--bg-primary)] border border-[var(--border-color)] rounded hover:bg-[var(--border-color)] disabled:text-[var(--text-disabled)] transition-colors"
          >
            {checkingUpdates ? "Checking..." : "Check now"}
          </button>
          {imageUpdateInfo && (
            <div className="flex items-center gap-2 px-3 py-2 text-xs bg-[var(--bg-primary)] border border-[var(--warning)] rounded">
              <span className="inline-block w-2 h-2 rounded-full bg-[var(--warning)]" />
              <span>A newer container image is available. Re-pull the image in Container settings above to update.</span>
            </div>
          )}
        </div>
      </AccordionSection>

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

      {showClaudeCodeSettingsModal && (
        <ClaudeCodeSettingsModal
          settings={appSettings?.global_claude_code_settings ?? null}
          disabled={false}
          onSave={async (ccSettings) => {
            if (appSettings) {
              await saveSettings({ ...appSettings, global_claude_code_settings: ccSettings });
            }
          }}
          onClose={() => setShowClaudeCodeSettingsModal(false)}
        />
      )}
    </div>
  );
}
