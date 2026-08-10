import { useEffect, useState } from "react";
import type { ClaudeSession, Project, ScheduledTask } from "../../../lib/types";
import {
  listClaudeSessions,
  listScheduledTasks,
  getSchedulerNotifications,
  resumeSessionCommand,
} from "../../../lib/tauri-commands";
import type { useProjectActions } from "../../../hooks/useProjectActions";
import type { SaveState } from "../../../hooks/useSaveState";
import PermissionModeControl, {
  permissionModePatch,
} from "../PermissionModeControl";
import CapabilityTiles from "./CapabilityTiles";
import ContainerMigrationBanner from "./ContainerMigrationBanner";
import type { ContainerMigration } from "../../../hooks/useContainerMigration";
import SaveIndicator from "../../ui/SaveIndicator";
import Button from "../../ui/Button";
import { formatAge } from "./format";
import type { ProjectHomeTabId } from "./ProjectHome";

const BACKEND_LABEL: Record<Project["backend"], string> = {
  anthropic: "Anthropic",
  bedrock: "AWS Bedrock",
  ollama: "Ollama",
  llama_cpp: "llama.cpp",
  open_ai_compatible: "OpenAI Compatible",
};

interface Props {
  project: Project;
  save: (patch: Partial<Project>) => Promise<boolean>;
  saveState: SaveState;
  actions: ReturnType<typeof useProjectActions>;
  onOpenTab: (tab: ProjectHomeTabId) => void;
  /** Base-image staleness, run state and report. Owned by `ProjectHome`. */
  migration: ContainerMigration;
  /** Migration mirrors Reset's gate: only offered on a stopped container. */
  canMigrate: boolean;
  onOpenMigration: () => void;
}

export default function OverviewTab({
  project,
  save,
  saveState,
  actions,
  onOpenTab,
  migration,
  canMigrate,
  onOpenMigration,
}: Props) {
  const [sessions, setSessions] = useState<ClaudeSession[]>([]);
  const [tasks, setTasks] = useState<ScheduledTask[]>([]);
  const [notificationCount, setNotificationCount] = useState(0);
  const running = project.status === "running";

  useEffect(() => {
    if (!running) {
      setSessions([]);
      setTasks([]);
      setNotificationCount(0);
      return;
    }
    let cancelled = false;
    // All three degrade to empty when the container is unreachable.
    listClaudeSessions(project.id)
      .then((s) => !cancelled && setSessions(s.slice(0, 4)))
      .catch(() => !cancelled && setSessions([]));
    listScheduledTasks(project.id)
      .then((t) => !cancelled && setTasks(t))
      .catch(() => !cancelled && setTasks([]));
    getSchedulerNotifications(project.id)
      .then((n) => !cancelled && setNotificationCount(n.length))
      .catch(() => !cancelled && setNotificationCount(0));
    return () => {
      cancelled = true;
    };
  }, [project.id, running, project.container_id]);

  const handleResume = async (session: ClaudeSession) => {
    try {
      const command = await resumeSessionCommand(project.id, session.id);
      await actions.openTerminalWithCommand(command, session.name ?? "resume");
    } catch (e) {
      console.error("Failed to build the resume command:", e);
    }
  };

  return (
    <div className="p-4 space-y-6 max-w-4xl">
      {/* Permission mode — the hero control */}
      <section className="p-3 border border-[var(--border-color)] rounded-[var(--radius-panel)] bg-[var(--bg-secondary)]">
        <div className="flex items-start justify-between gap-4">
          <div className="flex-1 min-w-0">
            <PermissionModeControl
              project={project}
              disabled={!running && project.status !== "stopped" && project.status !== "error"}
              onChange={(mode) => save(permissionModePatch(mode))}
            />
          </div>
          <SaveIndicator state={saveState} />
        </div>
        <div className="mt-3 pt-3 border-t border-[var(--border-color)] flex flex-wrap gap-x-6 gap-y-1 text-xs">
          <span className="text-[var(--text-secondary)]">
            Backend{" "}
            <span className="text-[var(--text-primary)] font-medium">
              {BACKEND_LABEL[project.backend]}
            </span>
          </span>
          <span className="text-[var(--text-secondary)]">
            Docker access{" "}
            <span className="text-[var(--text-primary)] font-medium">
              {project.allow_docker_access ? "ON" : "OFF"}
            </span>
          </span>
          <span className="text-[var(--text-secondary)]">
            Mission Control{" "}
            <span className="text-[var(--text-primary)] font-medium">
              {project.mission_control_enabled ? "ON" : "OFF"}
            </span>
          </span>
          <button
            type="button"
            onClick={() => onOpenTab("config")}
            className="text-[var(--accent)] hover:text-[var(--accent-hover)] transition-colors"
          >
            Edit configuration →
          </button>
        </div>
      </section>

      {/* A container missing socat and bwrap is a capability statement, so the
          out-of-date warning sits directly above the capability inventory. */}
      <ContainerMigrationBanner
        migration={migration}
        canMigrate={canMigrate}
        onOpen={onOpenMigration}
      />

      <CapabilityTiles
        project={project}
        onManageInTerminal={(command) => actions.openTerminalWithCommand(command)}
      />

      <div className="grid gap-6 md:grid-cols-2">
        {/* Recent sessions */}
        <section>
          <div className="flex items-baseline justify-between mb-2">
            <h2 className="text-[11px] font-semibold uppercase tracking-wide text-[var(--text-secondary)]">
              Recent sessions
            </h2>
            <button
              type="button"
              onClick={() => onOpenTab("sessions")}
              className="text-xs text-[var(--accent)] hover:text-[var(--accent-hover)] transition-colors"
            >
              All sessions →
            </button>
          </div>
          {!running ? (
            <p className="text-xs text-[var(--text-secondary)]">
              Start the container to list saved conversations.
            </p>
          ) : sessions.length === 0 ? (
            <p className="text-xs text-[var(--text-secondary)]">No sessions yet.</p>
          ) : (
            <ul className="space-y-1">
              {sessions.map((session) => (
                <li
                  key={session.id}
                  className="flex items-center gap-2 px-2 py-1.5 bg-[var(--bg-secondary)] border border-[var(--border-color)] rounded-[var(--radius-control)]"
                >
                  <span className="flex-1 min-w-0 text-xs text-[var(--text-primary)] truncate">
                    {session.name ?? session.summary ?? session.id}
                  </span>
                  <span className="text-xs text-[var(--text-secondary)] flex-shrink-0">
                    {formatAge(session.last_modified) ?? ""}
                  </span>
                  <Button onClick={() => handleResume(session)}>Resume</Button>
                </li>
              ))}
            </ul>
          )}
        </section>

        {/* Scheduled tasks */}
        <section>
          <div className="flex items-baseline justify-between mb-2">
            <h2 className="text-[11px] font-semibold uppercase tracking-wide text-[var(--text-secondary)]">
              Scheduled tasks
            </h2>
            <button
              type="button"
              onClick={() => onOpenTab("automation")}
              className="text-xs text-[var(--accent)] hover:text-[var(--accent-hover)] transition-colors"
            >
              Automation →
            </button>
          </div>
          {!running ? (
            <p className="text-xs text-[var(--text-secondary)]">
              Start the container to list scheduled tasks.
            </p>
          ) : tasks.length === 0 ? (
            <p className="text-xs text-[var(--text-secondary)]">No scheduled tasks.</p>
          ) : (
            <ul className="space-y-1">
              {tasks.slice(0, 4).map((task) => (
                <li
                  key={task.id}
                  className="flex items-center gap-2 px-2 py-1.5 bg-[var(--bg-secondary)] border border-[var(--border-color)] rounded-[var(--radius-control)]"
                >
                  <span className="flex-1 min-w-0 text-xs text-[var(--text-primary)] truncate">
                    {task.name}
                  </span>
                  <span className="text-xs text-[var(--text-secondary)] font-mono flex-shrink-0">
                    {task.at ?? task.schedule}
                  </span>
                </li>
              ))}
            </ul>
          )}
          {notificationCount > 0 && (
            <button
              type="button"
              onClick={() => onOpenTab("automation")}
              className="mt-2 inline-flex items-center gap-1.5 px-2 py-1 text-xs rounded-[var(--radius-control)] bg-[var(--accent-muted)] text-[var(--accent)] hover:bg-[var(--bg-tertiary)] transition-colors"
            >
              {notificationCount} notification{notificationCount === 1 ? "" : "s"}
            </button>
          )}
        </section>
      </div>
    </div>
  );
}
