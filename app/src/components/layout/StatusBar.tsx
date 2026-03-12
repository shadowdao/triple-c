import { useShallow } from "zustand/react/shallow";
import { useAppState } from "../../store/appState";

export default function StatusBar() {
  const { projects, sessions, terminalHasSelection } = useAppState(
    useShallow(s => ({ projects: s.projects, sessions: s.sessions, terminalHasSelection: s.terminalHasSelection }))
  );
  const running = projects.filter((p) => p.status === "running").length;

  return (
    <div className="flex items-center h-6 px-4 bg-[var(--bg-tertiary)] border border-[var(--border-color)] rounded-lg text-xs text-[var(--text-secondary)]">
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
          <span className="text-[var(--accent)]">Ctrl+Shift+C to copy</span>
        </>
      )}
    </div>
  );
}
