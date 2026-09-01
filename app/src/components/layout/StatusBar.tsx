import { useShallow } from "zustand/react/shallow";
import { useAppState } from "../../store/appState";
import SttButton from "../terminal/SttButton";
import type { useSTT } from "../../hooks/useSTT";

interface Props {
  stt: ReturnType<typeof useSTT>;
}

export default function StatusBar({ stt }: Props) {
  const {
    projects, sessions, terminalHasSelection, activeSessionId, sttEnabled,
    terminalAtBottom, scrollActiveToBottom, notesDockOpen, toggleNotesDock,
  } = useAppState(
    useShallow(s => ({
      projects: s.projects,
      sessions: s.sessions,
      terminalHasSelection: s.terminalHasSelection,
      activeSessionId: s.activeSessionId,
      sttEnabled: s.appSettings?.stt?.enabled,
      terminalAtBottom: s.terminalAtBottom,
      scrollActiveToBottom: s.scrollActiveToBottom,
      notesDockOpen: s.notesDockOpen,
      toggleNotesDock: s.toggleNotesDock,
    }))
  );
  const running = projects.filter((p) => p.status === "running").length;
  // Only in a Claude tab: the chord is bound there and nowhere else, and a hint
  // for a key that does nothing is worse than no hint.
  const inClaudeSession = sessions.some(
    (s) => s.id === activeSessionId && s.sessionType === "claude",
  );

  return (
    <div className="flex items-center h-6 px-4 bg-[var(--bg-tertiary)] border border-[var(--border-color)] rounded-[var(--radius-panel)] text-xs text-[var(--text-secondary)]">
      <span>
        {projects.length} project{projects.length !== 1 ? "s" : ""}
      </span>
      <span className="mx-2">|</span>
      <span>
        {running} running
      </span>
      <span className="mx-2">|</span>
      <span>
        {sessions.length} terminal{sessions.length !== 1 ? "s" : ""}
      </span>
      {terminalHasSelection && (
        <>
          <span className="mx-2">|</span>
          <span className="text-[var(--accent)]">
            Ctrl+Shift+C: copy trimmed &middot; Ctrl+Shift+Alt+C: copy raw
          </span>
        </>
      )}
      {!terminalHasSelection && inClaudeSession && (
        <>
          <span className="mx-2">|</span>
          <span title="Sends ESC+CR — the sequence Claude Code's own /terminal-setup installs. Alt+Enter does the same.">
            Shift+Enter: newline
          </span>
        </>
      )}
      {/* Right-aligned controls: Jump to Current + STT mic */}
      <div className="ml-auto flex items-center gap-3 pl-2">
        {activeSessionId && !terminalAtBottom && (
          <button
            onClick={() => scrollActiveToBottom()}
            className="text-[var(--accent)] hover:text-[var(--accent-hover)] cursor-pointer"
            title="Scroll the terminal to the latest output"
          >
            Jump to Current ↓
          </button>
        )}
        <button
          onClick={toggleNotesDock}
          aria-pressed={notesDockOpen}
          className="text-[var(--accent)] hover:text-[var(--accent-hover)] cursor-pointer"
          title="Show or hide the notes panel beside the current tab"
        >
          Notes
        </button>
        {sttEnabled && activeSessionId && (
          <SttButton
            state={stt.state}
            error={stt.error}
            onToggle={stt.toggle}
            onCancel={stt.cancelRecording}
          />
        )}
      </div>
    </div>
  );
}
