import { useCallback, useEffect, useState } from "react";
import type { ClaudeSession, Project } from "../../../lib/types";
import { listClaudeSessions, resumeSessionCommand } from "../../../lib/tauri-commands";
import type { useProjectActions } from "../../../hooks/useProjectActions";
import { useAppState } from "../../../store/appState";
import Button from "../../ui/Button";
import { formatAge, formatBytes } from "./format";

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
  const pushToast = useAppState((s) => s.pushToast);
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

  const resume = async (session: ClaudeSession) => {
    try {
      const command = await resumeSessionCommand(project.id, session.id);
      await actions.openTerminalWithCommand(command, session.name ?? "resume");
    } catch (e) {
      pushToast({
        kind: "error",
        message: "Could not resume that session",
        detail: String(e),
      });
    }
  };

  return (
    <div className="p-4 max-w-4xl">
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
