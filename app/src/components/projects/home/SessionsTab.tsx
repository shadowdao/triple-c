import { useCallback, useEffect, useState } from "react";
import { useShallow } from "zustand/react/shallow";
import type { ClaudeSession, Project, WorkspaceTerminal } from "../../../lib/types";
import {
  endWorkspaceTerminal,
  listClaudeSessions,
  listWorkspaceTerminals,
} from "../../../lib/tauri-commands";
import type { useProjectActions } from "../../../hooks/useProjectActions";
import { useTerminal } from "../../../hooks/useTerminal";
import { useAppState } from "../../../store/appState";
import { isAwsProject } from "../../../lib/awsWorkspace";
import { reattachTab, terminalLabel } from "../../../lib/persistentTerminals";
import Button from "../../ui/Button";
import { formatAge, formatBytes, formatElapsed } from "./format";

interface Props {
  project: Project;
  actions: ReturnType<typeof useProjectActions>;
}

/**
 * The stop/start container model buries "which conversation was I in?" in the
 * config volume. This lists it and makes [Resume] one click.
 */
export default function SessionsTab({ project, actions }: Props) {
  const [sessions, setSessions] = useState<ClaudeSession[]>([]);
  const [loading, setLoading] = useState(false);
  const running = project.status === "running";

  const load = useCallback(() => {
    if (!running) {
      setSessions([]);
      return;
    }
    setLoading(true);
    listClaudeSessions(project.id)
      .then(setSessions)
      .catch(() => setSessions([]))
      .finally(() => setLoading(false));
  }, [project.id, running]);

  useEffect(load, [load]);

  const resume = (session: ClaudeSession) =>
    actions.resumeClaudeSession(session.id, session.name ?? "resume");

  return (
    <div className="p-4 max-w-4xl">
      {isAwsProject(project) && running && <WorkspaceTerminals project={project} />}
      <div className="flex items-center justify-between mb-3">
        <p className="text-xs text-[var(--text-secondary)]">
          Conversations stored on this project&rsquo;s config volume. Resume opens a
          terminal running the resume command.
        </p>
        <Button onClick={load} disabled={!running || loading}>
          {loading ? "Refreshing…" : "Refresh"}
        </Button>
      </div>

      {!running ? (
        <p className="text-[13px] text-[var(--text-secondary)]">
          Start the container to read its saved sessions.
        </p>
      ) : sessions.length === 0 && !loading ? (
        <p className="text-[13px] text-[var(--text-secondary)]">
          No sessions recorded yet. Open a Claude terminal to start one.
        </p>
      ) : (
        <ul className="space-y-1">
          {sessions.map((session) => (
            <li
              key={session.id}
              className="flex items-center gap-3 px-3 py-2 bg-[var(--bg-secondary)] border border-[var(--border-color)] rounded-[var(--radius-control)]"
            >
              <div className="flex-1 min-w-0">
                <div className="text-[13px] text-[var(--text-primary)] truncate">
                  {session.name ?? session.summary ?? "(untitled session)"}
                </div>
                <div className="text-xs text-[var(--text-secondary)] truncate font-mono">
                  {session.id}
                  {session.cwd ? ` · ${session.cwd}` : ""}
                </div>
              </div>
              <div className="flex-shrink-0 text-right text-xs text-[var(--text-secondary)] tabular-nums">
                <div>{formatAge(session.last_modified) ?? "—"}</div>
                <div>
                  {formatBytes(session.size_bytes)} · {session.message_count} msg
                </div>
              </div>
              <Button variant="primary" onClick={() => resume(session)}>
                Resume
              </Button>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}

/**
 * AWS: the terminals running in the workspace — the ones open here, the ones
 * this app left detached, and any another machine or the web terminal
 * opened. Reattach opens one in a tab; End stops it (and closes its tab).
 */
function WorkspaceTerminals({ project }: { project: Project }) {
  const [terminals, setTerminals] = useState<WorkspaceTerminal[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [confirmEnd, setConfirmEnd] = useState<string | null>(null);
  const openIds = useAppState(useShallow((s) => s.sessions.map((x) => x.id)));
  const { close } = useTerminal();
  const pushToast = useAppState((s) => s.pushToast);

  const load = useCallback(() => {
    setLoading(true);
    setError(null);
    listWorkspaceTerminals(project.id)
      .then(setTerminals)
      .catch((e) => {
        setTerminals([]);
        setError(String(e));
      })
      .finally(() => setLoading(false));
  }, [project.id]);

  useEffect(load, [load]);

  const isOpen = (id: string) => openIds.includes(id);

  const reattach = (t: WorkspaceTerminal) => {
    useAppState.getState().addSession(reattachTab(project.id, project.name, t));
  };

  const end = async (t: WorkspaceTerminal) => {
    setConfirmEnd(null);
    try {
      if (isOpen(t.session_id)) await close(t.session_id);
      else await endWorkspaceTerminal(project.id, t.session_id);
    } catch (e) {
      pushToast({ kind: "error", message: "Could not end that terminal", detail: String(e) });
    }
    load();
  };

  return (
    <section className="mb-6" aria-labelledby="workspace-terminals-heading">
      <div className="flex items-center justify-between mb-2">
        <div>
          <h3 id="workspace-terminals-heading" className="text-[13px] font-medium text-[var(--text-primary)]">
            Terminals in the workspace
          </h3>
          <p className="text-xs text-[var(--text-secondary)]">
            Terminals keep running in the workspace when Triple-C closes or loses its
            connection. Closing a tab ends its terminal.
          </p>
        </div>
        <Button onClick={load} disabled={loading}>
          {loading ? "Refreshing…" : "Refresh"}
        </Button>
      </div>
      {error ? (
        <p className="text-[13px] text-[var(--text-secondary)]">{error}</p>
      ) : terminals.length === 0 && !loading ? (
        <p className="text-[13px] text-[var(--text-secondary)]">No terminals are running in the workspace.</p>
      ) : (
        <ul className="space-y-1" aria-label="Terminals in the workspace">
          {terminals.map((t) => {
            const open = isOpen(t.session_id);
            const state = open ? "Open here" : t.attached ? "Attached elsewhere" : "Detached";
            return (
              <li
                key={t.session_id}
                className="flex items-center gap-3 px-3 py-2 bg-[var(--bg-secondary)] border border-[var(--border-color)] rounded-[var(--radius-control)]"
              >
                <div className="flex-1 min-w-0">
                  <div className="text-[13px] text-[var(--text-primary)] truncate">{terminalLabel(t)}</div>
                  <div className="text-xs text-[var(--text-secondary)] truncate">
                    {t.kind === "claude" ? "Claude Code" : "Shell"} · {state} · started{" "}
                    {formatElapsed(Date.now() - t.created_unix * 1000)}
                    {t.known ? "" : " · opened elsewhere"}
                  </div>
                </div>
                {!open && (
                  <Button variant="primary" onClick={() => reattach(t)}>
                    Reattach
                  </Button>
                )}
                {confirmEnd === t.session_id ? (
                  <>
                    <Button variant="danger" onClick={() => void end(t)}>Confirm end</Button>
                    <Button onClick={() => setConfirmEnd(null)}>Cancel</Button>
                  </>
                ) : (
                  <Button onClick={() => setConfirmEnd(t.session_id)}>End</Button>
                )}
              </li>
            );
          })}
        </ul>
      )}
    </section>
  );
}
