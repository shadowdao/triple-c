import { useEffect, useMemo, useState } from "react";
import { useShallow } from "zustand/react/shallow";
import { projectRemovalIsClean } from "../../../lib/types";
import { useAppState } from "../../../store/appState";
import { useProjectActions } from "../../../hooks/useProjectActions";
import { useProjects } from "../../../hooks/useProjects";
import { useProjectSave } from "../../../hooks/useSaveState";
import { useContainerMigration } from "../../../hooks/useContainerMigration";
import { ProjectStatusIndicator } from "../../ui/StatusIndicator";
import Button from "../../ui/Button";
import OverflowMenu from "../../ui/OverflowMenu";
import ConfirmRemoveModal from "../ConfirmRemoveModal";
import ConfirmResetModal from "../ConfirmResetModal";
import MigrateContainerModal from "../MigrateContainerModal";
import OverviewTab from "./OverviewTab";
import SessionsTab from "./SessionsTab";
import AutomationTab from "./AutomationTab";
import ConfigTab from "./ConfigTab";
import FilesTab from "./FilesTab";
import BrowserTab from "./BrowserTab";
import { formatUptime } from "./format";
import { describeLeftovers, leftoverVerb } from "./removalReport";

const TABS = [
  { id: "overview", label: "Overview" },
  { id: "sessions", label: "Sessions" },
  { id: "automation", label: "Automation" },
  { id: "config", label: "Config" },
  { id: "files", label: "Files" },
  { id: "browser", label: "Browser" },
] as const;

export type ProjectHomeTabId = (typeof TABS)[number]["id"];

interface Props {
  projectId: string;
  active: boolean;
}

/**
 * The project promoted from a sidebar card to a first-class main-area view.
 * Everything that used to spray out of `ProjectCard` as a modal lives here.
 */
export default function ProjectHome({ projectId, active }: Props) {
  const { projects, remove } = useProjects();
  const project = projects.find((p) => p.id === projectId);
  const [tab, setTab] = useState<ProjectHomeTabId>("overview");

  // Somewhere else asked for this project on a particular sub-tab — currently
  // "I opened a page in the container's browser, show me it". Consumed once, so
  // it cannot fight the user's own clicking afterwards.
  const pendingHomeTab = useAppState((s) => s.pendingHomeTab);
  useEffect(() => {
    if (pendingHomeTab?.projectId !== projectId) return;
    setTab(pendingHomeTab.tab as ProjectHomeTabId);
    useAppState.getState().clearPendingHomeTab();
  }, [pendingHomeTab, projectId]);
  const [confirmRemove, setConfirmRemove] = useState(false);
  const [confirmReset, setConfirmReset] = useState(false);
  const [showMigration, setShowMigration] = useState(false);
  const { runningSince, progress } = useAppState(
    useShallow((s) => ({
      runningSince: s.runningSince[projectId],
      progress: s.containerProgress[projectId],
    })),
  );

  // Re-render once a minute so the uptime line stays honest.
  const [, setTick] = useState(0);
  useEffect(() => {
    if (!active || runningSince === undefined) return;
    const timer = setInterval(() => setTick((t) => t + 1), 60_000);
    return () => clearInterval(timer);
  }, [active, runningSince]);

  const actions = useProjectActions(
    project ?? ({ id: projectId, name: "", container_id: null } as never),
  );
  const { save, saveState } = useProjectSave(
    project ?? ({ id: projectId, name: "" } as never),
  );
  // Owned here, not in the modal: the run outlives the dialog, and the Overview
  // banner has to keep showing progress and the report after it is dismissed.
  const migration = useContainerMigration(
    project ?? ({ id: projectId, name: "", container_id: null } as never),
  );

  const uptime = useMemo(() => formatUptime(runningSince), [runningSince]);

  if (!project) {
    return (
      <div className={`h-full flex items-center justify-center ${active ? "" : "hidden"}`}>
        <p className="text-[13px] text-[var(--text-secondary)]">
          This project is no longer available.
        </p>
      </div>
    );
  }

  const isRunning = project.status === "running";
  const isTransitioning =
    project.status === "starting" || project.status === "stopping";
  const isStopped = project.status === "stopped" || project.status === "error";
  // Rebuilding on a new base swaps the container out, so it gates exactly like
  // Reset does — with the extra condition that there is a container to migrate.
  // An interrupted migration is excluded too: its action is Resume, on the
  // Overview banner, not a fresh pre-flight.
  //
  // `probeSettled` is the fourth condition and it is not cosmetic. The probe
  // takes ~6 s, and until it lands every delta the pre-flight renders reads as
  // empty — so the dialog would tell the user there was nothing to copy while
  // the backend was told not to copy anything.
  const canMigrate =
    isStopped &&
    !actions.busy &&
    !migration.running &&
    !migration.interrupted &&
    migration.probeSettled &&
    !!project.container_id;

  return (
    <div className={`flex flex-col h-full min-h-0 ${active ? "" : "hidden"}`}>
      {/* Header */}
      <header className="flex-shrink-0 px-4 pt-3 pb-2 border-b border-[var(--border-color)]">
        <div className="flex items-start justify-between gap-4 flex-wrap">
          <div className="min-w-0">
            <h1 className="text-base font-semibold text-[var(--text-primary)] truncate">
              {project.name}
            </h1>
            <div className="mt-0.5 flex items-center gap-2 text-xs">
              <ProjectStatusIndicator status={project.status} />
              {isRunning && uptime && (
                <span className="text-[var(--text-secondary)]">· {uptime}</span>
              )}
              {isTransitioning && progress && (
                <span className="text-[var(--warning)] truncate">· {progress}</span>
              )}
            </div>
          </div>

          <div className="flex items-center gap-1.5 flex-wrap">
            {isRunning ? (
              <Button
                size="md"
                variant="primary"
                disabled={actions.busy}
                onClick={actions.openClaudeTerminal}
              >
                Open Claude Terminal
              </Button>
            ) : (
              <Button
                size="md"
                variant="primary"
                disabled={actions.busy || isTransitioning}
                onClick={actions.handleStart}
              >
                Start
              </Button>
            )}
            {isRunning && (
              <>
                <Button size="md" onClick={actions.openShell}>
                  Shell
                </Button>
                <Button size="md" onClick={() => setTab("files")}>
                  Files
                </Button>
                <Button size="md" disabled={actions.busy} onClick={actions.handleStop}>
                  Stop
                </Button>
              </>
            )}
            {isTransitioning && (
              <Button size="md" variant="danger" onClick={actions.handleStop}>
                Force stop
              </Button>
            )}
            <OverflowMenu
              items={[
                {
                  label: actions.backingUp ? "Backing up…" : "Back up container",
                  onSelect: actions.handleBackup,
                  disabled: actions.backingUp || !project.container_id,
                },
                {
                  label: "Update container base…",
                  onSelect: () => setShowMigration(true),
                  disabled: !canMigrate,
                },
                {
                  label: "Reset container…",
                  onSelect: () => setConfirmReset(true),
                  disabled: !isStopped || actions.busy,
                  danger: true,
                },
                {
                  label: "Remove project…",
                  onSelect: () => setConfirmRemove(true),
                  danger: true,
                },
              ]}
            />
          </div>
        </div>

        {/* Tabs */}
        <div role="tablist" aria-label="Project sections" className="flex gap-1 mt-3 -mb-2">
          {TABS.map((t) => (
            <button
              key={t.id}
              type="button"
              role="tab"
              id={`project-tab-${projectId}-${t.id}`}
              aria-selected={tab === t.id}
              aria-controls={`project-panel-${projectId}-${t.id}`}
              onClick={() => setTab(t.id)}
              className={`px-3 h-8 text-[13px] font-medium rounded-t-[var(--radius-control)] border-b-2 transition-colors ${
                tab === t.id
                  ? "text-[var(--text-primary)] border-[var(--accent)]"
                  : "text-[var(--text-secondary)] border-transparent hover:text-[var(--text-primary)]"
              }`}
            >
              {t.label}
            </button>
          ))}
        </div>
      </header>

      {/* Panel */}
      <div
        role="tabpanel"
        id={`project-panel-${projectId}-${tab}`}
        aria-labelledby={`project-tab-${projectId}-${tab}`}
        className="flex-1 min-h-0 overflow-y-auto"
      >
        {tab === "overview" && (
          <OverviewTab
            project={project}
            save={save}
            saveState={saveState}
            actions={actions}
            onOpenTab={setTab}
            migration={migration}
            canMigrate={canMigrate}
            onOpenMigration={() => setShowMigration(true)}
          />
        )}
        {tab === "sessions" && <SessionsTab project={project} actions={actions} />}
        {tab === "automation" && <AutomationTab project={project} />}
        {tab === "config" && (
          <ConfigTab project={project} save={save} saveState={saveState} />
        )}
        {tab === "files" && <FilesTab project={project} />}
        {tab === "browser" && (
          <BrowserTab project={project} active={active && tab === "browser"} />
        )}
      </div>

      {showMigration && (
        <MigrateContainerModal
          projectName={project.name}
          staleness={migration.staleness}
          migration={migration}
          // Closing is not cancelling — the run keeps going and the Overview
          // banner keeps reporting it.
          onClose={() => setShowMigration(false)}
        />
      )}
      {confirmReset && (
        <ConfirmResetModal
          projectName={project.name}
          onCancel={() => setConfirmReset(false)}
          onConfirm={() => {
            setConfirmReset(false);
            actions.handleReset();
          }}
        />
      )}
      {confirmRemove && (
        <ConfirmRemoveModal
          projectName={project.name}
          onCancel={() => setConfirmRemove(false)}
          onConfirm={async () => {
            setConfirmRemove(false);
            try {
              const report = await remove(project.id);
              if (!projectRemovalIsClean(report)) {
                const verb = leftoverVerb(report);
                if (report.retry_scheduled) {
                  useAppState.getState().pushToast({
                    kind: "info",
                    message: `“${project.name}” was removed, but Triple-C could not confirm all its Docker resources were removed`,
                    detail: `Triple-C could not confirm ${describeLeftovers(report)} ${verb} removed. It will check again the next time it starts.`,
                  });
                } else {
                  // The pending-cleanup record itself failed to save — no
                  // retry will happen, so this must not promise one.
                  useAppState.getState().pushToast({
                    kind: "error",
                    message: `“${project.name}” was removed, but Triple-C could not confirm its Docker resources were removed`,
                    detail: `Triple-C could not confirm ${describeLeftovers(report)} ${verb} removed, and could not record this for a retry. You may need to remove them manually (\`docker rm\` / \`docker rmi\` / \`docker volume rm\`).`,
                  });
                }
              }
            } catch (e) {
              useAppState.getState().pushToast({
                kind: "error",
                message: `Could not remove “${project.name}”`,
                detail: String(e),
              });
            }
          }}
        />
      )}
    </div>
  );
}
