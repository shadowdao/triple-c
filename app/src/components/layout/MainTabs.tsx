import { useEffect, useRef, useState } from "react";
import { useShallow } from "zustand/react/shallow";
import { useTerminal } from "../../hooks/useTerminal";
import { useProjects } from "../../hooks/useProjects";
import {
  useAppState,
  isHomeTab,
  tabKeyId,
  terminalTabKey,
} from "../../store/appState";
import { effectivePermissionMode } from "../projects/PermissionModeControl";
import { ProjectStatusIndicator } from "../ui/StatusIndicator";
import type { PermissionMode } from "../../lib/types";

interface ContextMenuState {
  sessionId: string;
  x: number;
  y: number;
}

const MODE_BADGE: Record<PermissionMode, { text: string; className: string }> = {
  plan: { text: "plan", className: "bg-[var(--bg-tertiary)] text-[var(--text-secondary)]" },
  default: { text: "ask", className: "bg-[var(--bg-tertiary)] text-[var(--text-secondary)]" },
  acceptEdits: { text: "edits", className: "bg-[var(--accent-muted)] text-[var(--accent)]" },
  bypass: { text: "bypass", className: "bg-[var(--warning-muted)] text-[var(--warning)]" },
};

/**
 * One strip for both main-area tab kinds: Project Home views (⌂) and
 * terminals (▣).
 */
export default function MainTabs() {
  const { sessions, close } = useTerminal();
  const { projects, update } = useProjects();
  const { tabOrder, activeTabKey, setActiveTabKey, closeHomeTab } = useAppState(
    useShallow((s) => ({
      tabOrder: s.tabOrder,
      activeTabKey: s.activeTabKey,
      setActiveTabKey: s.setActiveTabKey,
      closeHomeTab: s.closeHomeTab,
    })),
  );
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

  if (tabOrder.length === 0) {
    return (
      <div className="px-3 text-xs text-[var(--text-secondary)] leading-10">
        No open tabs — select a project to open its home view.
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
    const current =
      getCustomName(session.projectId, sessionId) ??
      session.sessionName ??
      session.projectName;
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

  const tabClass = (active: boolean) =>
    `flex items-center gap-1.5 pl-3 pr-1.5 h-full text-xs cursor-pointer border-r border-[var(--border-color)] transition-colors ${
      active
        ? "bg-[var(--bg-primary)] text-[var(--text-primary)]"
        : "text-[var(--text-secondary)] hover:text-[var(--text-primary)]"
    }`;

  return (
    <div className="flex items-center h-full" role="tablist" aria-label="Open tabs">
      {tabOrder.map((key) => {
        const active = activeTabKey === key;

        if (isHomeTab(key)) {
          const projectId = tabKeyId(key);
          const project = projects.find((p) => p.id === projectId);
          if (!project) return null;
          return (
            <div
              key={key}
              role="tab"
              aria-selected={active}
              tabIndex={0}
              onClick={() => setActiveTabKey(key)}
              onKeyDown={(e) => {
                if (e.key === "Enter" || e.key === " ") {
                  e.preventDefault();
                  setActiveTabKey(key);
                }
              }}
              className={tabClass(active)}
            >
              <span aria-hidden="true" className="text-[var(--text-secondary)]">⌂</span>
              <span className="truncate max-w-[160px]" title={`${project.name} — project home`}>
                {project.name}
              </span>
              <ProjectStatusIndicator status={project.status} iconOnly />
              <button
                type="button"
                onClick={(e) => {
                  e.stopPropagation();
                  closeHomeTab(projectId);
                }}
                aria-label={`Close ${project.name} home tab`}
                title="Close tab"
                className="w-6 h-6 flex items-center justify-center rounded-[var(--radius-control)] text-[var(--text-secondary)] hover:text-[var(--error)] hover:bg-[var(--bg-tertiary)] transition-colors"
              >
                <span aria-hidden="true">×</span>
              </button>
            </div>
          );
        }

        const sessionId = tabKeyId(key);
        const session = sessions.find((s) => s.id === sessionId);
        if (!session) return null;
        const project = projects.find((p) => p.id === session.projectId);
        const customName = getCustomName(session.projectId, session.id);
        const baseLabel =
          (session.sessionName ?? session.projectName) +
          (session.sessionType === "bash" ? " (bash)" : "");
        const displayLabel = customName
          ? `${session.projectName}: ${customName}`
          : baseLabel;
        const isRenaming = renamingId === session.id;
        const badge = project ? MODE_BADGE[effectivePermissionMode(project)] : null;

        return (
          <div
            key={key}
            role="tab"
            aria-selected={active}
            tabIndex={0}
            onClick={() => setActiveTabKey(terminalTabKey(session.id))}
            onKeyDown={(e) => {
              if (e.key === "Enter" || e.key === " ") {
                e.preventDefault();
                setActiveTabKey(terminalTabKey(session.id));
              }
            }}
            onContextMenu={(e) => {
              e.preventDefault();
              setMenu({ sessionId: session.id, x: e.clientX, y: e.clientY });
            }}
            onDoubleClick={() => startRename(session.id)}
            className={tabClass(active)}
          >
            <span aria-hidden="true" className="text-[var(--text-secondary)]">▣</span>
            {isRenaming ? (
              <input
                ref={renameInputRef}
                value={renameDraft}
                aria-label="Rename tab"
                onChange={(e) => setRenameDraft(e.target.value)}
                onClick={(e) => e.stopPropagation()}
                onBlur={() => commitRename(session.id)}
                onKeyDown={(e) => {
                  if (e.key === "Enter") (e.target as HTMLInputElement).blur();
                  if (e.key === "Escape") setRenamingId(null);
                }}
                className="max-w-[180px] px-1 py-0 bg-[var(--bg-primary)] border border-[var(--accent)] rounded-[var(--radius-control)] text-xs text-[var(--text-primary)]"
              />
            ) : (
              <span className="truncate max-w-[180px]" title={displayLabel}>
                {displayLabel}
              </span>
            )}
            {badge && (
              <span
                className={`px-1 py-0.5 rounded-[4px] text-[10px] leading-none font-medium ${badge.className}`}
                title={`Permission mode: ${badge.text}`}
              >
                {badge.text}
              </span>
            )}
            <button
              type="button"
              onClick={(e) => {
                e.stopPropagation();
                close(session.id);
              }}
              aria-label={`Close ${displayLabel}`}
              title="Close terminal"
              className="w-6 h-6 flex items-center justify-center rounded-[var(--radius-control)] text-[var(--text-secondary)] hover:text-[var(--error)] hover:bg-[var(--bg-tertiary)] transition-colors"
            >
              <span aria-hidden="true">×</span>
            </button>
          </div>
        );
      })}

      {menu && (() => {
        const session = sessions.find((s) => s.id === menu.sessionId);
        const hasCustom = session
          ? !!getCustomName(session.projectId, menu.sessionId)
          : false;
        return (
          <div
            role="menu"
            className="fixed z-50 min-w-[160px] py-1 bg-[var(--bg-overlay)] border border-[var(--border-color)] rounded-[var(--radius-panel)] text-xs"
            style={{ top: menu.y, left: menu.x, boxShadow: "var(--shadow-overlay)" }}
            onClick={(e) => e.stopPropagation()}
          >
            <button
              type="button"
              role="menuitem"
              className="w-full text-left px-3 py-1.5 text-[var(--text-primary)] hover:bg-[var(--bg-tertiary)] transition-colors"
              onClick={() => startRename(menu.sessionId)}
            >
              Rename tab
            </button>
            {hasCustom && (
              <button
                type="button"
                role="menuitem"
                className="w-full text-left px-3 py-1.5 text-[var(--text-secondary)] hover:bg-[var(--bg-tertiary)] transition-colors"
                onClick={() => clearCustomName(menu.sessionId)}
              >
                Reset name
              </button>
            )}
            {session && (
              <button
                type="button"
                role="menuitem"
                className="w-full text-left px-3 py-1.5 text-[var(--text-primary)] hover:bg-[var(--bg-tertiary)] transition-colors"
                onClick={() => {
                  useAppState.getState().openProjectHome(session.projectId);
                  setMenu(null);
                }}
              >
                Open project home
              </button>
            )}
            <div className="border-t border-[var(--border-color)] my-1" />
            <button
              type="button"
              role="menuitem"
              className="w-full text-left px-3 py-1.5 text-[var(--error)] hover:bg-[var(--bg-tertiary)] transition-colors"
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
