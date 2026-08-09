import type { Project } from "../../../../lib/types";
import Toggle from "../../../ui/Toggle";
import { ConfigGroup, SwitchRow } from "../../../ui/Field";
import PermissionModeControl, { permissionModePatch } from "../../PermissionModeControl";
import ClaudeInstructionsEditor from "../../ClaudeInstructionsEditor";
import ClaudeCodeSettingsEditor from "../../ClaudeCodeSettingsEditor";

interface Props {
  project: Project;
  save: (patch: Partial<Project>) => Promise<boolean>;
  disabled: boolean;
  disabledReason?: string;
}

export default function RuntimeSection({
  project,
  save,
  disabled,
  disabledReason,
}: Props) {
  return (
    <>
      <ConfigGroup
        title="Runtime"
        description="How much the sandbox lets Claude do, and what contains it."
      >
        <div className="pb-2 border-b border-[var(--border-color)]">
          <PermissionModeControl
            project={project}
            onChange={(mode) => save(permissionModePatch(mode))}
          />
        </div>

        <SwitchRow
          label="Sandbox mode"
          hint="Claude Code's bash sandbox (bubblewrap filesystem and network isolation). Triple-C is the source of truth: toggling this overrides any manual /sandbox configuration in the container's settings.json on next start."
          control={
            <Toggle
              label="Sandbox mode"
              checked={project.sandbox_mode_enabled}
              disabled={disabled}
              onChange={(v) => save({ sandbox_mode_enabled: v })}
            />
          }
        />

        <SwitchRow
          label="Allow container spawning"
          hint="Mounts the Docker socket so Claude can build and run Docker containers from inside the sandbox."
          control={
            <Toggle
              label="Allow container spawning"
              checked={project.allow_docker_access}
              disabled={disabled}
              onChange={(v) => save({ allow_docker_access: v })}
            />
          }
        />

        <SwitchRow
          label="Mission Control"
          hint="A web dashboard for monitoring and managing Claude sessions remotely."
          control={
            <Toggle
              label="Mission Control"
              checked={project.mission_control_enabled}
              disabled={disabled}
              onChange={(v) => save({ mission_control_enabled: v })}
            />
          }
        />

        {disabled && disabledReason && (
          <p className="text-xs text-[var(--text-disabled)]">{disabledReason}</p>
        )}
      </ConfigGroup>

      <ConfigGroup
        title="Claude instructions"
        description="Written to ~/.claude/CLAUDE.md inside this project's container."
      >
        <ClaudeInstructionsEditor
          instructions={project.claude_instructions ?? ""}
          disabled={disabled}
          disabledReason={disabledReason}
          onSave={(value) => save({ claude_instructions: value || null })}
        />
      </ConfigGroup>

      <ConfigGroup
        title="Claude Code settings"
        description="Per-project CLI behaviour. These override the global defaults in Settings."
      >
        <ClaudeCodeSettingsEditor
          settings={project.claude_code_settings}
          disabled={disabled}
          disabledReason={disabledReason}
          onSave={(settings) => save({ claude_code_settings: settings })}
        />
      </ConfigGroup>
    </>
  );
}
