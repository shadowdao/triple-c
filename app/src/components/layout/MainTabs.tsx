import { Fragment, useEffect, useRef, useState } from "react";
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
 *
 * Tabs are draggable. The drag is HTML5's, not a pointer-event
 * reimplementation, so the OS supplies the drag image and the Escape-to-cancel
 * behaviour for free; the only thing tracked here is where the drop would land.
 * `Ctrl+Shift+←/→` does the same thing without a mouse.
 */
export default function MainTabs() {
  const { sessions, close } = useTerminal();
  const { projects, update } = useProjects();
  const { tabOrder, activeTabKey, setActiveTabKey, closeHomeTab, moveTab } = useAppState(
    useShallow((s) => ({
      tabOrder: s.tabOrder,
      activeTabKey: s.activeTabKey,
      setActiveTabKey: s.setActiveTabKey,
      closeHomeTab: s.closeHomeTab,
      moveTab: s.moveTab,
    })),
  );
  const [menu, setMenu] = useState<ContextMenuState | null>(null);
  const [renamingId, setRenamingId] = useState<string | null>(null);
  const [renameDraft, setRenameDraft] = useState("");
  const renameInputRef = useRef<HTMLInputElement>(null);
  /** The tab being dragged, and the slot it would drop into. */
  const [dragKey, setDragKey] = useState<string | null>(null);
  const [dropIndex, setDropIndex] = useState<number | null>(null);

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

  const tabClass = (active: boolean, dragging: boolean) =>
    `flex items-center gap-1.5 pl-3 pr-1.5 h-full text-xs cursor-pointer border-r border-[var(--border-color)] transition-colors ${
      active
        ? "bg-[var(--bg-primary)] text-[var(--text-primary)]"
        : "text-[var(--text-secondary)] hover:text-[var(--text-primary)]"
    }${dragging ? " opacity-40" : ""}`;

  const endDrag = () => {
    setDragKey(null);
    setDropIndex(null);
  };

  /**
   * Drag props shared by both tab kinds.
   *
   * A tab in rename mode is not draggable: a `draggable` ancestor swallows the
   * mouse-drag that selects text inside the input, which would make the rename
   * field impossible to select in.
   */
  const dragProps = (key: string, index: number, renaming: boolean) => ({
    draggable: !renaming,
    onDragStart: (e: React.DragEvent<HTMLDivElement>) => {
      e.dataTransfer.effectAllowed = "move";
      // Some platforms refuse to start a drag with an empty payload.
      e.dataTransfer.setData("text/plain", key);
      setDragKey(key);
      setDropIndex(index);
    },
    onDragOver: (e: React.DragEvent<HTMLDivElement>) => {
      if (!dragKey) return; // not our drag — a file dropped on the strip isn't one
      e.preventDefault();
      e.dataTransfer.dropEffect = "move";
      const rect = e.currentTarget.getBoundingClientRect();
      const after = e.clientX > rect.left + rect.width / 2;
      setDropIndex(index + (after ? 1 : 0));
    },
    onDragEnd: endDrag,
  });

  /** Drop lands wherever the marker is showing, and only there. */
  const onDrop = (e: React.DragEvent<HTMLDivElement>) => {
    const key = dragKey ?? e.dataTransfer.getData("text/plain");
    if (!key || dropIndex === null) return endDrag();
    e.preventDefault();
    const from = tabOrder.indexOf(key);
    // `dropIndex` is a slot in the strip as it looks *now*; `moveTab` places the
    // tab after pulling it out, so every slot past the tab's own shifts down one.
    moveTab(key, dropIndex > from ? dropIndex - 1 : dropIndex);
    endDrag();
  };

  const dropMarker = (
    <div
      aria-hidden="true"
      data-testid="tab-drop-marker"
      className="w-0.5 -mx-px h-full bg-[var(--accent)] flex-shrink-0"
    />
  );

  const renderTab = (key: string, index: number) => {
    const active = activeTabKey === key;

    if (isHomeTab(key)) {
      const projectId = tabKeyId(key);
      const project = projects.find((p) => p.id === projectId);
      if (!project) return null;
      return (
        <div
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
          {...dragProps(key, index, false)}
          className={tabClass(active, dragKey === key)}
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
        {...dragProps(key, index, isRenaming)}
        className={tabClass(active, dragKey === key)}
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
  };

  return (
    <div
      className="flex items-center h-full"
      role="tablist"
      aria-label="Open tabs"
      onDrop={onDrop}
      onDragLeave={(e) => {
        // Leaving the strip entirely (not crossing between tabs) parks the
        // marker, so a drag aborted outside doesn't leave one behind.
        if (!e.currentTarget.contains(e.relatedTarget as Node | null)) {
          setDropIndex(null);
        }
      }}
    >
      {tabOrder.map((key, index) => {
        const tab = renderTab(key, index);
        if (!tab) return null;
        return (
          <Fragment key={key}>
            {dragKey !== null && dropIndex === index && dropMarker}
            {tab}
          </Fragment>
        );
      })}

      {/* The empty run after the last tab is a drop target too — it is where
          the hand naturally goes to say "put it at the end". */}
      <div
        className="flex-1 self-stretch"
        onDragOver={(e) => {
          if (!dragKey) return;
          e.preventDefault();
          e.dataTransfer.dropEffect = "move";
          setDropIndex(tabOrder.length);
        }}
      >
        {dragKey !== null && dropIndex === tabOrder.length && dropMarker}
      </div>

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
