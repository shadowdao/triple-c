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

/** Pixels of horizontal travel before a press becomes a drag rather than a click. */
const DRAG_THRESHOLD = 4;

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
 * Tabs are draggable, on pointer events rather than HTML5 drag-and-drop — see
 * `pointerProps` for why neither of the two obvious alternatives works.
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
  /** Where the dragged tab is drawn, and how it looked when the drag started. */
  const [ghost, setGhost] = useState<{ x: number; y: number; label: string; icon: string } | null>(
    null,
  );
  const stripRef = useRef<HTMLDivElement>(null);
  /** A press that has not yet moved far enough to be a drag. */
  const pending = useRef<{
    key: string;
    startX: number;
    dragging: boolean;
    offsetX: number;
    width: number;
    height: number;
    top: number;
  } | null>(null);
  const suppressClick = useRef(false);

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

  // Escape abandons a drag — the one affordance a pointer-event drag has to
  // supply for itself, since the OS is not running this one.
  useEffect(() => {
    if (!dragKey) return;
    const onKeyDown = (e: KeyboardEvent) => {
      if (e.key !== "Escape") return;
      pending.current = null;
      setDragKey(null);
      setDropIndex(null);
      setGhost(null);
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [dragKey]);

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

  /**
   * What a tab reads as, for the dragged copy. Same sources the tab itself
   * uses — a ghost showing a different name from the tab it came from would be
   * worse than no ghost.
   */
  const tabLabel = (key: string): string => {
    if (isHomeTab(key)) {
      return projects.find((p) => p.id === tabKeyId(key))?.name ?? "";
    }
    const session = sessions.find((s) => s.id === tabKeyId(key));
    if (!session) return "";
    const custom = getCustomName(session.projectId, session.id);
    return custom
      ? `${session.projectName}: ${custom}`
      : (session.sessionName ?? session.projectName) +
          (session.sessionType === "bash" ? " (bash)" : "");
  };

  const endDrag = () => {
    pending.current = null;
    setDragKey(null);
    setDropIndex(null);
    setGhost(null);
  };

  /**
   * Which slot the pointer is currently over, as an insertion index into
   * `tabOrder`.
   *
   * Measured from the tabs actually on screen rather than from the event's
   * target, so the answer is the same whatever the pointer happens to be over —
   * including the drop marker itself, and including a `tabOrder` entry whose
   * session has already gone and which therefore renders nothing.
   */
  const dropIndexAt = (clientX: number): number => {
    const strip = stripRef.current;
    if (!strip) return tabOrder.length;
    for (const el of strip.querySelectorAll<HTMLElement>("[data-tab-index]")) {
      const rect = el.getBoundingClientRect();
      if (clientX < rect.left + rect.width / 2) return Number(el.dataset.tabIndex);
    }
    return tabOrder.length;
  };

  /**
   * Dragging is done with pointer events, not HTML5 drag-and-drop.
   *
   * Two reasons, both load-bearing. Tauri's `dragDropEnabled` — which the
   * terminal needs left on, because only the native drag-drop event carries
   * dropped *file paths* — blocks HTML5 drag inside the webview on Windows, so
   * an HTML5 implementation is simply dead there. And an HTML5 drag carries a
   * `DataTransfer`: released over any text field in the app, the default
   * handler types the payload into it.
   */
  const pointerProps = (key: string, renaming: boolean) => ({
    onPointerDown: (e: React.PointerEvent<HTMLDivElement>) => {
      // Left button only, never from the close button, and never while the
      // rename input is up — that drag is a text selection.
      if (e.button !== 0 || renaming) return;
      if ((e.target as HTMLElement).closest("button, input")) return;
      const rect = e.currentTarget.getBoundingClientRect();
      pending.current = {
        key,
        startX: e.clientX,
        dragging: false,
        // Where inside the tab the pointer grabbed it, so the ghost sits under
        // the cursor exactly where the real tab was — the thing that makes a
        // drag feel like moving an object rather than nudging a setting.
        offsetX: e.clientX - rect.left,
        width: rect.width,
        height: rect.height,
        top: rect.top,
      };
      e.currentTarget.setPointerCapture?.(e.pointerId);
    },
    onPointerMove: (e: React.PointerEvent<HTMLDivElement>) => {
      const drag = pending.current;
      if (!drag) return;
      // A few pixels of slop, so a click that trembles stays a click.
      if (!drag.dragging && Math.abs(e.clientX - drag.startX) < DRAG_THRESHOLD) return;
      drag.dragging = true;
      setDragKey(drag.key);
      setDropIndex(dropIndexAt(e.clientX));
      setGhost({
        x: e.clientX - drag.offsetX,
        y: drag.top,
        label: tabLabel(drag.key),
        icon: isHomeTab(drag.key) ? "⌂" : "▣",
      });
    },
    onPointerUp: (e: React.PointerEvent<HTMLDivElement>) => {
      const drag = pending.current;
      e.currentTarget.releasePointerCapture?.(e.pointerId);
      if (!drag?.dragging) {
        pending.current = null;
        return; // a plain click: leave it to `onClick` to select the tab
      }
      const to = dropIndexAt(e.clientX);
      const from = tabOrder.indexOf(drag.key);
      // `to` is a slot in the strip as it looks *now*; `moveTab` places the tab
      // after pulling it out, so every slot past its own shifts down one.
      if (from !== -1) moveTab(drag.key, to > from ? to - 1 : to);
      // The click that follows this pointerup is the drag's, not a selection.
      suppressClick.current = true;
      endDrag();
    },
    onPointerCancel: endDrag,
  });

  /** A drag in progress swallows the click it ends with. */
  const activateTab = (key: string) => {
    if (suppressClick.current) {
      suppressClick.current = false;
      return;
    }
    setActiveTabKey(key);
  };

  const dropMarker = (
    <div
      aria-hidden="true"
      data-testid="tab-drop-marker"
      className="w-0.5 -mx-px h-full bg-[var(--accent)] flex-shrink-0 pointer-events-none"
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
          data-tab-index={index}
          onClick={() => activateTab(key)}
          onKeyDown={(e) => {
            if (e.key === "Enter" || e.key === " ") {
              e.preventDefault();
              setActiveTabKey(key);
            }
          }}
          {...pointerProps(key, false)}
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
        data-tab-index={index}
        onClick={() => activateTab(terminalTabKey(session.id))}
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
        {...pointerProps(key, isRenaming)}
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

  // The marker goes before the first tab that is *actually on screen* at or
  // past the drop slot. Addressing it by raw index would lose it whenever a
  // `tabOrder` entry renders nothing — the window between a session ending and
  // the store dropping its key — leaving the drag with no visible target.
  let markerPending = dragKey !== null && dropIndex !== null;

  return (
    <div ref={stripRef} className="flex items-center h-full" role="tablist" aria-label="Open tabs">
      {tabOrder.map((key, index) => {
        const tab = renderTab(key, index);
        if (!tab) return null;
        const marker = markerPending && index >= (dropIndex ?? 0);
        if (marker) markerPending = false;
        return (
          <Fragment key={key}>
            {marker && dropMarker}
            {tab}
          </Fragment>
        );
      })}

      {/* The empty run after the last tab is a drop target too — it is where
          the hand naturally goes to say "put it at the end". */}
      <div className="flex-1 self-stretch">{markerPending && dropMarker}</div>

      {ghost && (
        // A copy of the tab, following the pointer. Without it the only
        // feedback is a dimmed source and a thin line, which reads as "some
        // setting changed" rather than "I am holding this tab".
        <div
          aria-hidden="true"
          data-testid="tab-drag-ghost"
          className="fixed z-50 flex items-center gap-1.5 px-3 h-8 text-xs rounded-[var(--radius-control)] bg-[var(--bg-primary)] text-[var(--text-primary)] border border-[var(--accent)] pointer-events-none"
          style={{
            left: ghost.x,
            top: ghost.y,
            boxShadow: "var(--shadow-overlay)",
            opacity: 0.9,
          }}
        >
          <span className="text-[var(--text-secondary)]">{ghost.icon}</span>
          <span className="truncate max-w-[180px]">{ghost.label}</span>
        </div>
      )}

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
