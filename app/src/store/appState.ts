import { create } from "zustand";
import type { Project, TerminalSession, AppSettings, UpdateInfo, ImageUpdateInfo } from "../lib/types";

const SIDEBAR_COLLAPSED_KEY = "triple-c.sidebar.collapsed";

function loadSidebarCollapsed(): boolean {
  try {
    return localStorage.getItem(SIDEBAR_COLLAPSED_KEY) === "1";
  } catch {
    return false;
  }
}

function persistSidebarCollapsed(value: boolean) {
  try {
    localStorage.setItem(SIDEBAR_COLLAPSED_KEY, value ? "1" : "0");
  } catch {
    // ignore — storage may be unavailable
  }
}

/**
 * The main area hosts two tab kinds — terminals and Project Home views — in a
 * single ordered strip. Tabs are addressed by a string key so one array can
 * hold both kinds.
 */
export const terminalTabKey = (sessionId: string) => `term:${sessionId}`;
export const homeTabKey = (projectId: string) => `home:${projectId}`;
export const isTerminalTab = (key: string) => key.startsWith("term:");
export const isHomeTab = (key: string) => key.startsWith("home:");
export const tabKeyId = (key: string) => key.slice(key.indexOf(":") + 1);

/** activeSessionId is derived from the active tab so exactly one thing is "current". */
function activation(activeTabKey: string | null) {
  return {
    activeTabKey,
    activeSessionId:
      activeTabKey && isTerminalTab(activeTabKey) ? tabKeyId(activeTabKey) : null,
  };
}

export type ToastKind = "error" | "success" | "info";

export interface Toast {
  id: string;
  kind: ToastKind;
  message: string;
  /** Long text (e.g. a bollard error) shown behind a "Details" disclosure. */
  detail?: string;
}

let toastCounter = 0;

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

  // Main-area tab strip (terminals + Project Home views)
  tabOrder: string[];
  activeTabKey: string | null;
  openProjectHome: (projectId: string) => void;
  closeHomeTab: (projectId: string) => void;
  setActiveTabKey: (key: string) => void;
  cycleTab: (delta: number) => void;
  focusTabIndex: (index: number) => void;

  // Inline container progress, replacing the blocking progress modal.
  containerProgress: Record<string, string>;
  setContainerProgress: (projectId: string, message: string | null) => void;
  /** Wall-clock ms when a project was observed transitioning to "running". */
  runningSince: Record<string, number>;

  // Toasts
  toasts: Toast[];
  pushToast: (toast: Omit<Toast, "id">) => string;
  dismissToast: (id: string) => void;

  // UI state
  terminalHasSelection: boolean;
  setTerminalHasSelection: (has: boolean) => void;
  // STT toggle for the active session, registered by App so the terminal's
  // Ctrl+Shift+M shortcut can trigger the single status-bar mic instance.
  sttToggle: () => void;
  setSttToggle: (fn: () => void) => void;
  // Active terminal scroll state, surfaced so the status bar can host the
  // "Jump to Current" control. Only the active TerminalView writes these.
  terminalAtBottom: boolean;
  setTerminalAtBottom: (v: boolean) => void;
  scrollActiveToBottom: () => void;
  setScrollActiveToBottom: (fn: () => void) => void;
  sidebarView: "projects" | "settings";
  setSidebarView: (view: "projects" | "settings") => void;
  sidebarCollapsed: boolean;
  setSidebarCollapsed: (collapsed: boolean) => void;
  toggleSidebarCollapsed: () => void;
  dockerAvailable: boolean | null;
  setDockerAvailable: (available: boolean | null) => void;
  imageExists: boolean | null;
  setImageExists: (exists: boolean | null) => void;
  // App settings
  appSettings: AppSettings | null;
  setAppSettings: (settings: AppSettings) => void;

  // Update info
  updateInfo: UpdateInfo | null;
  setUpdateInfo: (info: UpdateInfo | null) => void;
  appVersion: string;
  setAppVersion: (version: string) => void;

  // Image update info
  imageUpdateInfo: ImageUpdateInfo | null;
  setImageUpdateInfo: (info: ImageUpdateInfo | null) => void;
}

/** Track running-since transitions so Overview can show an uptime. */
function trackRunning(
  previous: Project[],
  next: Project[],
  runningSince: Record<string, number>,
): Record<string, number> {
  let changed = false;
  const result = { ...runningSince };
  const now = Date.now();
  for (const project of next) {
    const before = previous.find((p) => p.id === project.id);
    if (project.status === "running") {
      if (result[project.id] === undefined) {
        result[project.id] = now;
        changed = true;
      }
    } else if (before?.status === "running" || result[project.id] !== undefined) {
      delete result[project.id];
      changed = true;
    }
  }
  return changed ? result : runningSince;
}

export const useAppState = create<AppState>((set) => ({
  // Projects
  projects: [],
  selectedProjectId: null,
  setProjects: (projects) =>
    set((state) => ({
      projects,
      runningSince: trackRunning(state.projects, projects, state.runningSince),
    })),
  setSelectedProject: (id) => set({ selectedProjectId: id }),
  updateProjectInList: (project) =>
    set((state) => {
      const projects = state.projects.map((p) =>
        p.id === project.id ? project : p,
      );
      return {
        projects,
        runningSince: trackRunning(state.projects, projects, state.runningSince),
      };
    }),
  removeProjectFromList: (id) =>
    set((state) => {
      const key = homeTabKey(id);
      const tabOrder = state.tabOrder.filter((k) => k !== key);
      const activeTabKey =
        state.activeTabKey === key
          ? (tabOrder[tabOrder.length - 1] ?? null)
          : state.activeTabKey;
      return {
        projects: state.projects.filter((p) => p.id !== id),
        selectedProjectId:
          state.selectedProjectId === id ? null : state.selectedProjectId,
        tabOrder,
        ...activation(activeTabKey),
      };
    }),

  // Terminal sessions
  sessions: [],
  activeSessionId: null,
  addSession: (session) =>
    set((state) => {
      const key = terminalTabKey(session.id);
      return {
        sessions: [...state.sessions, session],
        tabOrder: state.tabOrder.includes(key)
          ? state.tabOrder
          : [...state.tabOrder, key],
        ...activation(key),
      };
    }),
  removeSession: (id) =>
    set((state) => {
      const key = terminalTabKey(id);
      const index = state.tabOrder.indexOf(key);
      const tabOrder = state.tabOrder.filter((k) => k !== key);
      const activeTabKey =
        state.activeTabKey === key
          ? (tabOrder[Math.min(Math.max(index, 0), tabOrder.length - 1)] ?? null)
          : state.activeTabKey;
      return {
        sessions: state.sessions.filter((s) => s.id !== id),
        tabOrder,
        ...activation(activeTabKey),
      };
    }),
  setActiveSession: (id) =>
    set(() => activation(id === null ? null : terminalTabKey(id))),

  // Main-area tabs
  tabOrder: [],
  activeTabKey: null,
  openProjectHome: (projectId) =>
    set((state) => {
      const key = homeTabKey(projectId);
      return {
        selectedProjectId: projectId,
        tabOrder: state.tabOrder.includes(key)
          ? state.tabOrder
          : [...state.tabOrder, key],
        ...activation(key),
      };
    }),
  closeHomeTab: (projectId) =>
    set((state) => {
      const key = homeTabKey(projectId);
      const index = state.tabOrder.indexOf(key);
      if (index === -1) return {};
      const tabOrder = state.tabOrder.filter((k) => k !== key);
      const activeTabKey =
        state.activeTabKey === key
          ? (tabOrder[Math.min(index, tabOrder.length - 1)] ?? null)
          : state.activeTabKey;
      return { tabOrder, ...activation(activeTabKey) };
    }),
  setActiveTabKey: (key) =>
    set((state) => {
      if (!state.tabOrder.includes(key)) return {};
      const patch = activation(key);
      return isHomeTab(key)
        ? { ...patch, selectedProjectId: tabKeyId(key) }
        : patch;
    }),
  cycleTab: (delta) =>
    set((state) => {
      if (state.tabOrder.length === 0) return {};
      const current = state.activeTabKey
        ? state.tabOrder.indexOf(state.activeTabKey)
        : -1;
      const next =
        (current + delta + state.tabOrder.length) % state.tabOrder.length;
      const key = state.tabOrder[next];
      const patch = activation(key);
      return isHomeTab(key)
        ? { ...patch, selectedProjectId: tabKeyId(key) }
        : patch;
    }),
  focusTabIndex: (index) =>
    set((state) => {
      const key = state.tabOrder[index];
      if (!key) return {};
      const patch = activation(key);
      return isHomeTab(key)
        ? { ...patch, selectedProjectId: tabKeyId(key) }
        : patch;
    }),

  // Container progress
  containerProgress: {},
  setContainerProgress: (projectId, message) =>
    set((state) => {
      if (message === null) {
        if (state.containerProgress[projectId] === undefined) return {};
        const next = { ...state.containerProgress };
        delete next[projectId];
        return { containerProgress: next };
      }
      if (state.containerProgress[projectId] === message) return {};
      return {
        containerProgress: { ...state.containerProgress, [projectId]: message },
      };
    }),
  runningSince: {},

  // Toasts
  toasts: [],
  pushToast: (toast) => {
    const id = `toast-${++toastCounter}`;
    set((state) => ({ toasts: [...state.toasts, { ...toast, id }] }));
    return id;
  },
  dismissToast: (id) =>
    set((state) => ({ toasts: state.toasts.filter((t) => t.id !== id) })),

  // UI state
  terminalHasSelection: false,
  setTerminalHasSelection: (has) => set({ terminalHasSelection: has }),
  sttToggle: () => {},
  setSttToggle: (fn) => set({ sttToggle: fn }),
  terminalAtBottom: true,
  setTerminalAtBottom: (v) => set({ terminalAtBottom: v }),
  scrollActiveToBottom: () => {},
  setScrollActiveToBottom: (fn) => set({ scrollActiveToBottom: fn }),
  sidebarView: "projects",
  setSidebarView: (view) => set({ sidebarView: view }),
  sidebarCollapsed: loadSidebarCollapsed(),
  setSidebarCollapsed: (collapsed) => {
    persistSidebarCollapsed(collapsed);
    set({ sidebarCollapsed: collapsed });
  },
  toggleSidebarCollapsed: () =>
    set((state) => {
      const next = !state.sidebarCollapsed;
      persistSidebarCollapsed(next);
      return { sidebarCollapsed: next };
    }),
  dockerAvailable: null,
  setDockerAvailable: (available) => set({ dockerAvailable: available }),
  imageExists: null,
  setImageExists: (exists) => set({ imageExists: exists }),
  // App settings
  appSettings: null,
  setAppSettings: (settings) => set({ appSettings: settings }),

  // Update info
  updateInfo: null,
  setUpdateInfo: (info) => set({ updateInfo: info }),
  appVersion: "",
  setAppVersion: (version) => set({ appVersion: version }),

  // Image update info
  imageUpdateInfo: null,
  setImageUpdateInfo: (info) => set({ imageUpdateInfo: info }),
}));
