import { useEffect, useRef, useState } from "react";
import { useTerminal } from "../../hooks/useTerminal";
import { useProjects } from "../../hooks/useProjects";

interface ContextMenuState {
  sessionId: string;
  x: number;
  y: number;
}

export default function TerminalTabs() {
  const { sessions, activeSessionId, setActiveSession, close } = useTerminal();
  const { projects, update } = useProjects();
  const [menu, setMenu] = useState<ContextMenuState | null>(null);
  const [renamingId, setRenamingId] = useState<string | null>(null);
  const [renameDraft, setRenameDraft] = useState("");
  const renameInputRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    if (!menu) return;
    const dismiss = () => setMenu(null);
    window.addEventListener("click", dismiss);
    window.addEventListener("scroll", dismiss, true);
    return () => {
      window.removeEventListener("click", dismiss);
      window.removeEventListener("scroll", dismiss, true);
    };
  }, [menu]);

  useEffect(() => {
    if (renamingId) {
      renameInputRef.current?.focus();
      renameInputRef.current?.select();
    }
  }, [renamingId]);

  if (sessions.length === 0) {
    return (
      <div className="px-3 text-xs text-[var(--text-secondary)] leading-10">
        No active terminals
      </div>
    );
  }

  const getCustomName = (projectId: string, sessionId: string): string | null => {
    const project = projects.find((p) => p.id === projectId);
    return project?.renamed_session_names?.[sessionId] ?? null;
  };

  const startRename = (sessionId: string) => {
    const session = sessions.find((s) => s.id === sessionId);
    if (!session) return;
    const current = getCustomName(session.projectId, sessionId) ?? session.sessionName ?? session.projectName;
    setRenameDraft(current);
    setRenamingId(sessionId);
    setMenu(null);
  };

  const commitRename = async (sessionId: string) => {
    const session = sessions.find((s) => s.id === sessionId);
    if (!session) {
      setRenamingId(null);
      return;
    }
    const project = projects.find((p) => p.id === session.projectId);
    if (!project) {
      setRenamingId(null);
      return;
    }
    const trimmed = renameDraft.trim();
    const map = { ...(project.renamed_session_names ?? {}) };
    if (trimmed) {
      map[sessionId] = trimmed;
    } else {
      delete map[sessionId];
    }
    try {
      await update({ ...project, renamed_session_names: map });
    } catch (err) {
      console.error("Failed to rename terminal tab:", err);
    } finally {
      setRenamingId(null);
    }
  };

  const clearCustomName = async (sessionId: string) => {
    const session = sessions.find((s) => s.id === sessionId);
    if (!session) return;
    const project = projects.find((p) => p.id === session.projectId);
    if (!project) return;
    const map = { ...(project.renamed_session_names ?? {}) };
    if (!(sessionId in map)) {
      setMenu(null);
      return;
    }
    delete map[sessionId];
    try {
      await update({ ...project, renamed_session_names: map });
    } catch (err) {
      console.error("Failed to reset terminal tab name:", err);
    } finally {
      setMenu(null);
    }
  };

  return (
    <div className="flex items-center h-full">
      {sessions.map((session) => {
        const customName = getCustomName(session.projectId, session.id);
        const baseLabel =
          (session.sessionName ?? session.projectName) +
          (session.sessionType === "bash" ? " (bash)" : "");
        const displayLabel = customName
          ? `${session.projectName}: ${customName}`
          : baseLabel;
        const isRenaming = renamingId === session.id;
        return (
          <div
            key={session.id}
            onClick={() => setActiveSession(session.id)}
            onContextMenu={(e) => {
              e.preventDefault();
              setMenu({ sessionId: session.id, x: e.clientX, y: e.clientY });
            }}
            onDoubleClick={() => startRename(session.id)}
            className={`flex items-center gap-2 px-3 h-full text-xs cursor-pointer border-r border-[var(--border-color)] transition-colors ${
              activeSessionId === session.id
                ? "bg-[var(--bg-primary)] text-[var(--text-primary)]"
                : "text-[var(--text-secondary)] hover:text-[var(--text-primary)]"
            }`}
          >
            {isRenaming ? (
              <input
                ref={renameInputRef}
                value={renameDraft}
                onChange={(e) => setRenameDraft(e.target.value)}
                onClick={(e) => e.stopPropagation()}
                onBlur={() => commitRename(session.id)}
                onKeyDown={(e) => {
                  if (e.key === "Enter") (e.target as HTMLInputElement).blur();
                  if (e.key === "Escape") setRenamingId(null);
                }}
                className="max-w-[180px] px-1 py-0 bg-[var(--bg-primary)] border border-[var(--accent)] rounded text-xs text-[var(--text-primary)] focus:outline-none"
              />
            ) : (
              <span className="truncate max-w-[200px]" title={displayLabel}>
                {displayLabel}
              </span>
            )}
            <button
              onClick={(e) => {
                e.stopPropagation();
                close(session.id);
              }}
              className="text-[var(--text-secondary)] hover:text-[var(--error)] transition-colors"
              title="Close terminal"
            >
              ×
            </button>
          </div>
        );
      })}
      {menu && (() => {
        const session = sessions.find((s) => s.id === menu.sessionId);
        const hasCustom = session ? !!getCustomName(session.projectId, menu.sessionId) : false;
        return (
          <div
            className="fixed z-50 min-w-[160px] py-1 bg-[var(--bg-secondary)] border border-[var(--border-color)] rounded shadow-lg text-xs"
            style={{ top: menu.y, left: menu.x }}
            onClick={(e) => e.stopPropagation()}
          >
            <button
              className="w-full text-left px-3 py-1.5 text-[var(--text-primary)] hover:bg-[var(--bg-primary)] transition-colors"
              onClick={() => startRename(menu.sessionId)}
            >
              Rename tab
            </button>
            {hasCustom && (
              <button
                className="w-full text-left px-3 py-1.5 text-[var(--text-secondary)] hover:bg-[var(--bg-primary)] transition-colors"
                onClick={() => clearCustomName(menu.sessionId)}
              >
                Reset name
              </button>
            )}
            <div className="border-t border-[var(--border-color)] my-1" />
            <button
              className="w-full text-left px-3 py-1.5 text-[var(--error)] hover:bg-[var(--bg-primary)] transition-colors"
              onClick={() => {
                close(menu.sessionId);
                setMenu(null);
              }}
            >
              Close tab
            </button>
          </div>
        );
      })()}
    </div>
  );
}
