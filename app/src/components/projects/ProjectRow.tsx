import { useShallow } from "zustand/react/shallow";
import type { Project } from "../../lib/types";
import { useAppState, homeTabKey } from "../../store/appState";
import { useProjectActions } from "../../hooks/useProjectActions";
import { ProjectStatusIndicator } from "../ui/StatusIndicator";

interface Props {
  project: Project;
}

/**
 * Sidebar rows are select-only: name, paths, status, and hover controls.
 * Clicking a row opens (or focuses) that project's Project Home tab — the
 * settings form no longer lives in a 280px accordion.
 */
export default function ProjectRow({ project }: Props) {
  const { activeTabKey, selectedProjectId, openProjectHome, progress } = useAppState(
    useShallow((s) => ({
      activeTabKey: s.activeTabKey,
      selectedProjectId: s.selectedProjectId,
      openProjectHome: s.openProjectHome,
      progress: s.containerProgress[project.id],
    })),
  );
  const { busy, handleStart, handleStop, openClaudeTerminal } =
    useProjectActions(project);

  const isSelected =
    activeTabKey === homeTabKey(project.id) || selectedProjectId === project.id;
  const isRunning = project.status === "running";
  const isTransitioning =
    project.status === "starting" || project.status === "stopping";

  return (
    <div
      className={`group relative px-2 py-1.5 rounded-[var(--radius-control)] transition-colors min-w-0 overflow-hidden ${
        isSelected
          ? "bg-[var(--bg-tertiary)]"
          : "hover:bg-[var(--bg-tertiary)]"
      }`}
    >
      <button
        type="button"
        onClick={() => openProjectHome(project.id)}
        aria-current={isSelected ? "true" : undefined}
        className="w-full text-left min-w-0"
      >
        <div className="flex items-center gap-2 min-w-0">
          <ProjectStatusIndicator status={project.status} iconOnly />
          <span className="text-[13px] font-medium truncate flex-1 text-[var(--text-primary)]">
            {project.name}
          </span>
          {/* Space reserved for the hover controls so the name never jumps. */}
          <span className="w-[3.75rem] flex-shrink-0" aria-hidden="true" />
        </div>
        <div className="mt-0.5 ml-4 space-y-0.5 min-w-0">
          {project.paths.map((pp, i) => (
            <div
              key={i}
              className="text-xs text-[var(--text-secondary)] truncate font-mono"
            >
              /workspace/{pp.mount_name}
            </div>
          ))}
          <div className="text-xs">
            {isTransitioning ? (
              <span className="text-[var(--warning)] truncate block">
                {progress ?? `${project.status}…`}
              </span>
            ) : (
              <ProjectStatusIndicator
                status={project.status}
                className="text-xs"
              />
            )}
          </div>
        </div>
      </button>

      {/* Hover / focus-within controls */}
      <div className="absolute top-1.5 right-2 flex items-center gap-0.5 opacity-0 group-hover:opacity-100 group-focus-within:opacity-100 transition-opacity">
        <button
          type="button"
          disabled={busy}
          // While a container is mid-transition this stays live so it can act
          // as the force-stop that the old progress modal used to offer.
          onClick={() => (isRunning || isTransitioning ? handleStop() : handleStart())}
          title={
            isTransitioning
              ? `Force stop ${project.name}`
              : isRunning
                ? `Stop ${project.name}`
                : `Start ${project.name}`
          }
          aria-label={
            isTransitioning
              ? `Force stop ${project.name}`
              : isRunning
                ? `Stop ${project.name}`
                : `Start ${project.name}`
          }
          className="w-6 h-6 flex items-center justify-center rounded-[var(--radius-control)] text-[var(--text-secondary)] hover:text-[var(--text-primary)] hover:bg-[var(--bg-primary)] disabled:text-[var(--text-disabled)] transition-colors"
        >
          {isRunning || isTransitioning ? (
            <svg className="w-3.5 h-3.5" viewBox="0 0 24 24" fill="currentColor" aria-hidden="true">
              <rect x="6" y="6" width="12" height="12" rx="1.5" />
            </svg>
          ) : (
            <svg className="w-3.5 h-3.5" viewBox="0 0 24 24" fill="currentColor" aria-hidden="true">
              <path d="M8 5.5v13l11-6.5z" />
            </svg>
          )}
        </button>
        <button
          type="button"
          disabled={!isRunning}
          onClick={() => openClaudeTerminal()}
          title={`Open a Claude terminal for ${project.name}`}
          aria-label={`Open a Claude terminal for ${project.name}`}
          className="w-6 h-6 flex items-center justify-center rounded-[var(--radius-control)] text-[var(--text-secondary)] hover:text-[var(--text-primary)] hover:bg-[var(--bg-primary)] disabled:text-[var(--text-disabled)] transition-colors"
        >
          <svg
            className="w-3.5 h-3.5"
            viewBox="0 0 24 24"
            fill="none"
            stroke="currentColor"
            strokeWidth="2"
            strokeLinecap="round"
            strokeLinejoin="round"
            aria-hidden="true"
          >
            <rect x="3" y="4" width="18" height="16" rx="2" />
            <polyline points="7 9 10 12 7 15" />
            <line x1="13" y1="15" x2="17" y2="15" />
          </svg>
        </button>
      </div>
    </div>
  );
}
