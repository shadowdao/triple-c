import type { ReactNode } from "react";
import { useShallow } from "zustand/react/shallow";
import { useAppState } from "../../store/appState";
import ProjectList from "../projects/ProjectList";
import SettingsPanel from "../settings/SettingsPanel";

type SidebarView = "projects" | "settings";

const RAIL_ICONS: { view: SidebarView; label: string; icon: ReactNode }[] = [
  {
    view: "projects",
    label: "Projects",
    icon: (
      <svg className="w-5 h-5" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
        <path d="M3 7a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v9a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V7z" />
      </svg>
    ),
  },
  {
    view: "settings",
    label: "Settings",
    icon: (
      <svg className="w-5 h-5" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
        <circle cx="12" cy="12" r="3" />
        <path d="M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 1 1-2.83 2.83l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 0 1-4 0v-.09a1.65 1.65 0 0 0-1-1.51 1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 1 1-2.83-2.83l.06-.06a1.65 1.65 0 0 0 .33-1.82 1.65 1.65 0 0 0-1.51-1H3a2 2 0 0 1 0-4h.09a1.65 1.65 0 0 0 1.51-1 1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 1 1 2.83-2.83l.06.06a1.65 1.65 0 0 0 1.82.33H9a1.65 1.65 0 0 0 1-1.51V3a2 2 0 0 1 4 0v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 1 1 2.83 2.83l-.06.06a1.65 1.65 0 0 0-.33 1.82V9a1.65 1.65 0 0 0 1.51 1H21a2 2 0 0 1 0 4h-.09a1.65 1.65 0 0 0-1.51 1z" />
      </svg>
    ),
  },
];

export default function Sidebar() {
  const { sidebarView, setSidebarView, sidebarCollapsed, setSidebarCollapsed, toggleSidebarCollapsed } = useAppState(
    useShallow(s => ({
      sidebarView: s.sidebarView,
      setSidebarView: s.setSidebarView,
      sidebarCollapsed: s.sidebarCollapsed,
      setSidebarCollapsed: s.setSidebarCollapsed,
      toggleSidebarCollapsed: s.toggleSidebarCollapsed,
    }))
  );

  if (sidebarCollapsed) {
    const railBtn = (view: SidebarView, label: string, icon: ReactNode) => {
      const active = sidebarView === view;
      return (
        <button
          key={view}
          onClick={() => {
            setSidebarView(view);
            setSidebarCollapsed(false);
          }}
          title={label}
          aria-label={label}
          className={`flex items-center justify-center h-10 w-full transition-colors ${
            active
              ? "text-[var(--accent)]"
              : "text-[var(--text-secondary)] hover:text-[var(--text-primary)]"
          }`}
        >
          {icon}
        </button>
      );
    };

    return (
      <div className="flex flex-col h-full w-12 bg-[var(--bg-secondary)] border border-[var(--border-color)] rounded-lg overflow-hidden">
        <button
          onClick={toggleSidebarCollapsed}
          title="Expand sidebar"
          aria-label="Expand sidebar"
          className="flex items-center justify-center h-10 border-b border-[var(--border-color)] text-[var(--text-secondary)] hover:text-[var(--text-primary)] transition-colors"
        >
          <svg className="w-4 h-4" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
            <polyline points="9 18 15 12 9 6" />
          </svg>
        </button>
        <div className="flex flex-col py-1">
          {RAIL_ICONS.map(({ view, label, icon }) => railBtn(view, label, icon))}
        </div>
      </div>
    );
  }

  const tabCls = (view: SidebarView) =>
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
        <button onClick={() => setSidebarView("settings")} className={tabCls("settings")}>
          Settings
        </button>
        <button
          onClick={toggleSidebarCollapsed}
          title="Collapse sidebar"
          aria-label="Collapse sidebar"
          className="px-2 text-[var(--text-secondary)] hover:text-[var(--text-primary)] transition-colors"
        >
          <svg className="w-4 h-4" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
            <polyline points="15 18 9 12 15 6" />
          </svg>
        </button>
      </div>

      {/* Content */}
      <div className="flex-1 overflow-y-auto overflow-x-hidden p-1 min-w-0">
        {sidebarView === "projects" ? <ProjectList /> : <SettingsPanel />}
      </div>
    </div>
  );
}
