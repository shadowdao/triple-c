import { create } from "zustand";
import type { Project, TerminalSession, AppSettings, UpdateInfo } from "../lib/types";

interface AppState {
  // Projects
  projects: Project[];
  selectedProjectId: string | null;
  setProjects: (projects: Project[]) => void;
  setSelectedProject: (id: string | null) => void;
  updateProjectInList: (project: Project) => void;
  removeProjectFromList: (id: string) => void;

  // Terminal sessions
  sessions: TerminalSession[];
  activeSessionId: string | null;
  addSession: (session: TerminalSession) => void;
  removeSession: (id: string) => void;
  setActiveSession: (id: string | null) => void;

  // UI state
  sidebarView: "projects" | "settings";
  setSidebarView: (view: "projects" | "settings") => void;
  dockerAvailable: boolean | null;
  setDockerAvailable: (available: boolean | null) => void;
  imageExists: boolean | null;
  setImageExists: (exists: boolean | null) => void;
  hasKey: boolean | null;
  setHasKey: (has: boolean | null) => void;

  // App settings
  appSettings: AppSettings | null;
  setAppSettings: (settings: AppSettings) => void;

  // Update info
  updateInfo: UpdateInfo | null;
  setUpdateInfo: (info: UpdateInfo | null) => void;
  appVersion: string;
  setAppVersion: (version: string) => void;
}

export const useAppState = create<AppState>((set) => ({
  // Projects
  projects: [],
  selectedProjectId: null,
  setProjects: (projects) => set({ projects }),
  setSelectedProject: (id) => set({ selectedProjectId: id }),
  updateProjectInList: (project) =>
    set((state) => ({
      projects: state.projects.map((p) =>
        p.id === project.id ? project : p,
      ),
    })),
  removeProjectFromList: (id) =>
    set((state) => ({
      projects: state.projects.filter((p) => p.id !== id),
      selectedProjectId:
        state.selectedProjectId === id ? null : state.selectedProjectId,
    })),

  // Terminal sessions
  sessions: [],
  activeSessionId: null,
  addSession: (session) =>
    set((state) => ({
      sessions: [...state.sessions, session],
      activeSessionId: session.id,
    })),
  removeSession: (id) =>
    set((state) => {
      const sessions = state.sessions.filter((s) => s.id !== id);
      return {
        sessions,
        activeSessionId:
          state.activeSessionId === id
            ? (sessions[sessions.length - 1]?.id ?? null)
            : state.activeSessionId,
      };
    }),
  setActiveSession: (id) => set({ activeSessionId: id }),

  // UI state
  sidebarView: "projects",
  setSidebarView: (view) => set({ sidebarView: view }),
  dockerAvailable: null,
  setDockerAvailable: (available) => set({ dockerAvailable: available }),
  imageExists: null,
  setImageExists: (exists) => set({ imageExists: exists }),
  hasKey: null,
  setHasKey: (has) => set({ hasKey: has }),

  // App settings
  appSettings: null,
  setAppSettings: (settings) => set({ appSettings: settings }),

  // Update info
  updateInfo: null,
  setUpdateInfo: (info) => set({ updateInfo: info }),
  appVersion: "",
  setAppVersion: (version) => set({ appVersion: version }),
}));
