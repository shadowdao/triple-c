import { useShallow } from "zustand/react/shallow";
import { useAppState } from "../../store/appState";
import ProjectList from "../projects/ProjectList";
import McpPanel from "../mcp/McpPanel";
import SettingsPanel from "../settings/SettingsPanel";

export default function Sidebar() {
  const { sidebarView, setSidebarView } = useAppState(
    useShallow(s => ({ sidebarView: s.sidebarView, setSidebarView: s.setSidebarView }))
  );

  const tabCls = (view: typeof sidebarView) =>
    `flex-1 px-3 py-2 text-sm font-medium transition-colors ${
      sidebarView === view
        ? "text-[var(--accent)] border-b-2 border-[var(--accent)]"
        : "text-[var(--text-secondary)] hover:text-[var(--text-primary)]"
    }`;

  return (
    <div className="flex flex-col h-full w-[25%] min-w-56 max-w-80 bg-[var(--bg-secondary)] border border-[var(--border-color)] rounded-lg overflow-hidden">
      {/* Nav tabs */}
      <div className="flex border-b border-[var(--border-color)]">
        <button onClick={() => setSidebarView("projects")} className={tabCls("projects")}>
          Projects
        </button>
        <button onClick={() => setSidebarView("mcp")} className={tabCls("mcp")}>
          MCP <span className="text-[0.6rem] px-1 py-0.5 rounded bg-yellow-500/20 text-yellow-400 ml-0.5">Beta</span>
        </button>
        <button onClick={() => setSidebarView("settings")} className={tabCls("settings")}>
          Settings
        </button>
      </div>

      {/* Content */}
      <div className="flex-1 overflow-y-auto overflow-x-hidden p-1 min-w-0">
        {sidebarView === "projects" ? (
          <ProjectList />
        ) : sidebarView === "mcp" ? (
          <McpPanel />
        ) : (
          <SettingsPanel />
        )}
      </div>
    </div>
  );
}
