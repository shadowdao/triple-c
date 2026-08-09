import { useCallback, useEffect, useState } from "react";
import type { Project, ScheduledTask, SchedulerNotification } from "../../../lib/types";
import {
  clearSchedulerNotifications,
  getScheduledTaskLog,
  getSchedulerNotifications,
  listScheduledTasks,
  removeScheduledTask,
  runScheduledTaskNow,
  setScheduledTaskEnabled,
} from "../../../lib/tauri-commands";
import { useAppState } from "../../../store/appState";
import Button from "../../ui/Button";
import Toggle from "../../ui/Toggle";
import Modal from "../../ui/Modal";
import StatusIndicator from "../../ui/StatusIndicator";
import { formatAge } from "./format";

interface Props {
  project: Project;
}

/**
 * UI for `triple-c-scheduler`, which ships in every container and until now
 * had no interface beyond a CLAUDE.md paragraph.
 */
export default function AutomationTab({ project }: Props) {
  const [tasks, setTasks] = useState<ScheduledTask[]>([]);
  const [notifications, setNotifications] = useState<SchedulerNotification[]>([]);
  const [loading, setLoading] = useState(false);
  const [busyTaskId, setBusyTaskId] = useState<string | null>(null);
  const [log, setLog] = useState<{ task: ScheduledTask; text: string } | null>(null);
  const [confirmRemoveId, setConfirmRemoveId] = useState<string | null>(null);
  const pushToast = useAppState((s) => s.pushToast);
  const running = project.status === "running";

  const load = useCallback(() => {
    if (!running) {
      setTasks([]);
      setNotifications([]);
      return;
    }
    setLoading(true);
    Promise.all([
      listScheduledTasks(project.id).catch(() => [] as ScheduledTask[]),
      getSchedulerNotifications(project.id).catch(
        () => [] as SchedulerNotification[],
      ),
    ])
      .then(([t, n]) => {
        setTasks(t);
        setNotifications(n);
      })
      .finally(() => setLoading(false));
  }, [project.id, running]);

  useEffect(load, [load]);

  const withTask = async (taskId: string, label: string, fn: () => Promise<unknown>) => {
    setBusyTaskId(taskId);
    try {
      await fn();
      load();
    } catch (e) {
      pushToast({ kind: "error", message: `${label} failed`, detail: String(e) });
    } finally {
      setBusyTaskId(null);
    }
  };

  const openLog = async (task: ScheduledTask) => {
    setBusyTaskId(task.id);
    try {
      const text = await getScheduledTaskLog(project.id, task.id, 200);
      setLog({ task, text });
    } catch (e) {
      pushToast({
        kind: "error",
        message: `Could not read the log for “${task.name}”`,
        detail: String(e),
      });
    } finally {
      setBusyTaskId(null);
    }
  };

  const removing = tasks.find((t) => t.id === confirmRemoveId) ?? null;

  return (
    <div className="p-4 space-y-6 max-w-4xl">
      {/* Notifications */}
      {notifications.length > 0 && (
        <section className="border border-[var(--accent)]/40 bg-[var(--accent-muted)] rounded-[var(--radius-panel)]">
          <header className="flex items-center justify-between px-3 py-2 border-b border-[var(--border-color)]">
            <h2 className="text-[11px] font-semibold uppercase tracking-wide text-[var(--accent)]">
              {notifications.length} notification
              {notifications.length === 1 ? "" : "s"}
            </h2>
            <Button
              onClick={async () => {
                try {
                  await clearSchedulerNotifications(project.id);
                  setNotifications([]);
                } catch (e) {
                  pushToast({
                    kind: "error",
                    message: "Could not clear notifications",
                    detail: String(e),
                  });
                }
              }}
            >
              Clear all
            </Button>
          </header>
          <ul className="divide-y divide-[var(--border-color)]">
            {notifications.map((n, i) => (
              <li key={`${n.task_id}-${i}`} className="px-3 py-2">
                <div className="flex items-center gap-2 text-xs">
                  <span className="font-medium text-[var(--text-primary)]">
                    {n.task_name ?? n.task_id}
                  </span>
                  {n.status && (
                    <StatusIndicator
                      tone={n.status.toLowerCase() === "success" ? "ok" : "error"}
                      label={n.status}
                    />
                  )}
                  <span className="text-[var(--text-secondary)] ml-auto">
                    {formatAge(n.created_at) ?? n.time ?? ""}
                  </span>
                </div>
                <p className="mt-0.5 text-xs text-[var(--text-secondary)] whitespace-pre-wrap break-words">
                  {n.summary ?? n.body}
                </p>
              </li>
            ))}
          </ul>
        </section>
      )}

      <section>
        <div className="flex items-center justify-between mb-3">
          <p className="text-xs text-[var(--text-secondary)]">
            Recurring Claude Code runs managed by{" "}
            <code className="font-mono text-[var(--text-primary)]">
              triple-c-scheduler
            </code>{" "}
            inside the container.
          </p>
          <Button onClick={load} disabled={!running || loading}>
            {loading ? "Refreshing…" : "Refresh"}
          </Button>
        </div>

        {!running ? (
          <p className="text-[13px] text-[var(--text-secondary)]">
            Start the container to list its scheduled tasks.
          </p>
        ) : tasks.length === 0 && !loading ? (
          <p className="text-[13px] text-[var(--text-secondary)]">
            No scheduled tasks. Ask Claude to add one with{" "}
            <code className="font-mono">triple-c-scheduler add</code>.
          </p>
        ) : (
          <ul className="space-y-1">
            {tasks.map((task) => (
              <li
                key={task.id}
                className="flex items-center gap-3 px-3 py-2 bg-[var(--bg-secondary)] border border-[var(--border-color)] rounded-[var(--radius-control)]"
              >
                <div className="flex-1 min-w-0">
                  <div className="flex items-center gap-2">
                    <span className="text-[13px] font-medium text-[var(--text-primary)] truncate">
                      {task.name}
                    </span>
                    <span className="text-[10px] uppercase tracking-wide px-1.5 py-0.5 rounded-[var(--radius-control)] bg-[var(--bg-tertiary)] text-[var(--text-secondary)]">
                      {task.task_type}
                    </span>
                  </div>
                  <div className="text-xs text-[var(--text-secondary)] font-mono truncate">
                    {task.at ?? task.schedule}
                    {task.last_run ? ` · last run ${formatAge(task.last_run) ?? task.last_run}` : ""}
                  </div>
                </div>
                <Toggle
                  label={`${task.name} enabled`}
                  checked={task.enabled}
                  disabled={busyTaskId === task.id}
                  onChange={(v) =>
                    withTask(task.id, "Toggle task", () =>
                      setScheduledTaskEnabled(project.id, task.id, v),
                    )
                  }
                />
                <Button
                  disabled={busyTaskId === task.id}
                  onClick={() =>
                    withTask(task.id, "Run now", () =>
                      runScheduledTaskNow(project.id, task.id),
                    )
                  }
                >
                  Run now
                </Button>
                <Button disabled={busyTaskId === task.id} onClick={() => openLog(task)}>
                  Log
                </Button>
                <Button
                  variant="danger"
                  disabled={busyTaskId === task.id}
                  onClick={() => setConfirmRemoveId(task.id)}
                >
                  Remove
                </Button>
              </li>
            ))}
          </ul>
        )}
      </section>

      {log && (
        <Modal
          title={`Log — ${log.task.name}`}
          onClose={() => setLog(null)}
          widthClassName="w-[46rem]"
          footer={<Button onClick={() => setLog(null)}>Close</Button>}
        >
          <pre className="whitespace-pre-wrap break-words font-mono text-xs text-[var(--text-secondary)]">
            {log.text.trim() || "(empty log)"}
          </pre>
        </Modal>
      )}

      {removing && (
        <Modal
          title="Remove scheduled task"
          onClose={() => setConfirmRemoveId(null)}
          widthClassName="w-[26rem]"
          footer={
            <>
              <Button variant="ghost" onClick={() => setConfirmRemoveId(null)}>
                Cancel
              </Button>
              <Button
                className="bg-[var(--error-emphasis)] text-white border border-transparent hover:opacity-90"
                onClick={() => {
                  setConfirmRemoveId(null);
                  withTask(removing.id, "Remove task", () =>
                    removeScheduledTask(project.id, removing.id),
                  );
                }}
              >
                Remove
              </Button>
            </>
          }
        >
          <p className="text-[13px] text-[var(--text-secondary)]">
            Remove <strong className="text-[var(--text-primary)]">{removing.name}</strong>{" "}
            from this container&rsquo;s scheduler?
          </p>
        </Modal>
      )}
    </div>
  );
}
