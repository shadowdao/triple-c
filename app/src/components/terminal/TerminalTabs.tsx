import { useTerminal } from "../../hooks/useTerminal";

export default function TerminalTabs() {
  const { sessions, activeSessionId, setActiveSession, close } = useTerminal();

  if (sessions.length === 0) {
    return (
      <div className="px-3 text-xs text-[var(--text-secondary)] leading-10">
        No active terminals
      </div>
    );
  }

  return (
    <div className="flex items-center h-full">
      {sessions.map((session) => (
        <div
          key={session.id}
          onClick={() => setActiveSession(session.id)}
          className={`flex items-center gap-2 px-3 h-full text-xs cursor-pointer border-r border-[var(--border-color)] transition-colors ${
            activeSessionId === session.id
              ? "bg-[var(--bg-primary)] text-[var(--text-primary)]"
              : "text-[var(--text-secondary)] hover:text-[var(--text-primary)]"
          }`}
        >
          <span className="truncate max-w-[120px]">
            {session.projectName}{session.sessionType === "bash" ? " (bash)" : ""}
          </span>
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
      ))}
    </div>
  );
}
