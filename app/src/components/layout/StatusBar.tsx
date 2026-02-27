import { useAppState } from "../../store/appState";

export default function StatusBar() {
  const { projects, sessions } = useAppState();
  const running = projects.filter((p) => p.status === "running").length;

  return (
    <div className="flex items-center h-6 px-3 bg-[var(--bg-tertiary)] border border-[var(--border-color)] rounded-lg text-xs text-[var(--text-secondary)]">
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
    </div>
  );
}
