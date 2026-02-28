import { useShallow } from "zustand/react/shallow";
import { useAppState } from "../../store/appState";
import ProjectList from "../projects/ProjectList";
import SettingsPanel from "../settings/SettingsPanel";

export default function Sidebar() {
  const { sidebarView, setSidebarView } = useAppState(
    useShallow(s => ({ sidebarView: s.sidebarView, setSidebarView: s.setSidebarView }))
  );

  return (
    <div className="flex flex-col h-full w-[25%] min-w-56 max-w-80 bg-[var(--bg-secondary)] border border-[var(--border-color)] rounded-lg overflow-hidden">
      {/* Nav tabs */}
      <div className="flex border-b border-[var(--border-color)]">
        <button
          onClick={() => setSidebarView("projects")}
          className={`flex-1 px-3 py-2 text-sm font-medium transition-colors ${
            sidebarView === "projects"
              ? "text-[var(--accent)] border-b-2 border-[var(--accent)]"
              : "text-[var(--text-secondary)] hover:text-[var(--text-primary)]"
          }`}
        >
          Projects
        </button>
        <button
          onClick={() => setSidebarView("settings")}
          className={`flex-1 px-3 py-2 text-sm font-medium transition-colors ${
            sidebarView === "settings"
              ? "text-[var(--accent)] border-b-2 border-[var(--accent)]"
              : "text-[var(--text-secondary)] hover:text-[var(--text-primary)]"
          }`}
        >
          Settings
        </button>
      </div>

      {/* Content */}
      <div className="flex-1 overflow-y-auto p-1">
        {sidebarView === "projects" ? <ProjectList /> : <SettingsPanel />}
      </div>
    </div>
  );
}
