import type { Project } from "../../../lib/types";
import type { SaveState } from "../../../hooks/useSaveState";
import SaveIndicator from "../../ui/SaveIndicator";
import WorkspaceSection from "./config/WorkspaceSection";
import ModelSection from "./config/ModelSection";
import AccessSection from "./config/AccessSection";
import RuntimeSection from "./config/RuntimeSection";

interface Props {
  project: Project;
  save: (patch: Partial<Project>) => Promise<boolean>;
  saveState: SaveState;
}

const STOPPED_ONLY =
  "Container must be stopped to change this setting.";

/**
 * Everything the seven config modals used to hold, full-width and grouped.
 * Saves happen on blur; the indicator in the header reports the outcome.
 */
export default function ConfigTab({ project, save, saveState }: Props) {
  const isStopped = project.status === "stopped" || project.status === "error";
  const disabled = !isStopped;

  return (
    <div className="p-4 space-y-4 max-w-4xl">
      <div className="flex items-center justify-between gap-4 min-h-[1.5rem]">
        {disabled ? (
          <p className="px-2 py-1 text-xs text-[var(--warning)] bg-[var(--warning-muted)] border border-[var(--warning)]/30 rounded-[var(--radius-control)]">
            Container is {project.status} — stop it to change these settings.
          </p>
        ) : (
          <p className="text-xs text-[var(--text-secondary)]">
            Changes save when a field loses focus.
          </p>
        )}
        <SaveIndicator state={saveState} />
      </div>

      <WorkspaceSection project={project} save={save} disabled={disabled} />
      <ModelSection project={project} save={save} disabled={disabled} />
      <AccessSection
        project={project}
        save={save}
        disabled={disabled}
        disabledReason={STOPPED_ONLY}
      />
      <RuntimeSection
        project={project}
        save={save}
        disabled={disabled}
        disabledReason={STOPPED_ONLY}
      />
    </div>
  );
}
