import { useShallow } from "zustand/react/shallow";
import {
  useAppState,
  isHomeTab,
  isTerminalTab,
  tabKeyId,
  NOTES_DOCK_MIN_WIDTH,
  NOTES_DOCK_MAX_WIDTH,
} from "../../store/appState";
import NotesDockPanel from "../notes/NotesDockPanel";
import Button from "../ui/Button";

/**
 * Notes beside whatever is on screen.
 *
 * Project Home and Terminal are sibling top-level tabs, so notes living only
 * in a sub-tab would be hidden exactly when the agent is running — which is
 * when a note is worth sending. The dock is the answer to that.
 *
 * **It takes space from inside the window and never resizes it.** Growing the
 * OS window was tried and rejected on evidence: honoured under XWayland,
 * silently corrupting under native Wayland, where `outer_position()` returns a
 * confident `Ok(0,0)` for a window that is somewhere else. See the design doc,
 * §6.1. Narrowing the terminal instead costs nothing — `TerminalView`'s
 * ResizeObserver already reflows xterm and resizes the container PTY.
 */
export default function NotesDock() {
  const {
    notesDockOpen,
    setNotesDockOpen,
    notesDockWidth,
    setNotesDockWidth,
    activeTabKey,
    sessions,
  } = useAppState(
    useShallow((s) => ({
      notesDockOpen: s.notesDockOpen,
      setNotesDockOpen: s.setNotesDockOpen,
      notesDockWidth: s.notesDockWidth,
      setNotesDockWidth: s.setNotesDockWidth,
      activeTabKey: s.activeTabKey,
      sessions: s.sessions,
    })),
  );

  // Dragging the separator. Pointer capture rather than window listeners, so
  // the drag survives the pointer crossing the terminal — which swallows
  // events — and ends correctly if the button is released outside the window.
  const onPointerDown = (e: React.PointerEvent<HTMLDivElement>) => {
    e.preventDefault();
    const handle = e.currentTarget;
    handle.setPointerCapture(e.pointerId);
    const startX = e.clientX;
    const startWidth = notesDockWidth;
    // The dock is on the right, so dragging left widens it.
    const onMove = (move: PointerEvent) =>
      setNotesDockWidth(startWidth + (startX - move.clientX));
    const onUp = () => {
      handle.releasePointerCapture(e.pointerId);
      handle.removeEventListener("pointermove", onMove);
      handle.removeEventListener("pointerup", onUp);
    };
    handle.addEventListener("pointermove", onMove);
    handle.addEventListener("pointerup", onUp);
  };

  const onHandleKeyDown = (e: React.KeyboardEvent<HTMLDivElement>) => {
    const step = e.shiftKey ? 64 : 16;
    if (e.key === "ArrowLeft") {
      e.preventDefault();
      setNotesDockWidth(notesDockWidth + step);
    } else if (e.key === "ArrowRight") {
      e.preventDefault();
      setNotesDockWidth(notesDockWidth - step);
    }
  };

  if (!notesDockOpen) return null;

  // Follow whatever is in front: a home tab is its own project, a terminal tab
  // is the project it belongs to.
  let projectId: string | null = null;
  if (activeTabKey && isHomeTab(activeTabKey)) {
    projectId = tabKeyId(activeTabKey);
  } else if (activeTabKey && isTerminalTab(activeTabKey)) {
    projectId =
      sessions.find((s) => s.id === tabKeyId(activeTabKey))?.projectId ?? null;
  }

  return (
    <aside
      aria-label="Notes"
      style={{ width: `${notesDockWidth}px` }}
      className="relative flex-shrink-0 flex flex-col min-h-0 bg-[var(--bg-secondary)] border border-[var(--border-color)] rounded-[var(--radius-panel)] overflow-hidden"
    >
      {/* Separator, not decoration: it carries a role and arrow keys, because
          a resize that only answers to a drag is unavailable to anyone not
          using a mouse. */}
      <div
        role="separator"
        aria-label="Resize notes panel"
        aria-orientation="vertical"
        aria-valuenow={notesDockWidth}
        aria-valuemin={NOTES_DOCK_MIN_WIDTH}
        aria-valuemax={NOTES_DOCK_MAX_WIDTH}
        tabIndex={0}
        onPointerDown={onPointerDown}
        onKeyDown={onHandleKeyDown}
        className="absolute left-0 top-0 h-full w-1.5 cursor-col-resize hover:bg-[var(--accent-muted)] transition-colors"
      />
      <div className="flex items-center justify-between gap-2 px-3 h-9 flex-shrink-0 border-b border-[var(--border-color)]">
        <h2 className="text-[13px] font-semibold text-[var(--text-primary)]">Notes</h2>
        <Button variant="ghost" onClick={() => setNotesDockOpen(false)} aria-label="Close notes">
          Close
        </Button>
      </div>
      <div className="flex-1 min-h-0">
        {projectId ? (
          <NotesDockPanel projectId={projectId} />
        ) : (
          <p className="p-4 text-[13px] text-[var(--text-secondary)]">
            Open a project or a terminal to see its notes.
          </p>
        )}
      </div>
    </aside>
  );
}
