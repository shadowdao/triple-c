import { useCallback, useEffect, useState } from "react";
import { useShallow } from "zustand/react/shallow";
import { listen } from "@tauri-apps/api/event";
import Sidebar from "./components/layout/Sidebar";
import TopBar from "./components/layout/TopBar";
import StatusBar from "./components/layout/StatusBar";
import TerminalView from "./components/terminal/TerminalView";
import DockerInstallDialog from "./components/DockerInstallDialog";
import ProjectHome from "./components/projects/home/ProjectHome";
import AddProjectDialog from "./components/projects/AddProjectDialog";
import ToastHost from "./components/ui/ToastHost";
import { PaneVisibilityProvider } from "./components/ui/PaneVisibility";
import StatusIndicator from "./components/ui/StatusIndicator";
import Button from "./components/ui/Button";
import { useDocker } from "./hooks/useDocker";
import { useSettings } from "./hooks/useSettings";
import { useProjects } from "./hooks/useProjects";
import { useUpdates } from "./hooks/useUpdates";
import { useTerminal } from "./hooks/useTerminal";
import { useSTT } from "./hooks/useSTT";
import { useContainerProgress } from "./hooks/useContainerProgress";
import { useKeyboardShortcuts } from "./hooks/useKeyboardShortcuts";
import { useAppState, isHomeTab, tabKeyId, homeTabKey } from "./store/appState";
import { reconcileProjectStatuses } from "./lib/tauri-commands";

export default function App() {
  const { checkDocker, checkImage, startDockerPolling } = useDocker();
  const { loadSettings } = useSettings();
  const { refresh } = useProjects();
  const { loadVersion, checkForUpdates, checkImageUpdate, startPeriodicCheck } = useUpdates();
  const { sessions, activeSessionId, tabOrder, activeTabKey, setProjects, setSttToggle } =
    useAppState(
      useShallow(s => ({
        sessions: s.sessions,
        activeSessionId: s.activeSessionId,
        tabOrder: s.tabOrder,
        activeTabKey: s.activeTabKey,
        setProjects: s.setProjects,
        setSttToggle: s.setSttToggle,
      }))
    );
  const [showInstallDialog, setShowInstallDialog] = useState(false);
  const [shuttingDown, setShuttingDown] = useState(false);

  /**
   * Everything that can only be done once Docker answers. Called from the
   * startup check *and* from the poller when the daemon shows up later — a
   * session that launched before Docker was ready otherwise never reconciles
   * container state or recovers an interrupted migration.
   */
  const onDockerReady = useCallback(async () => {
    checkImage();
    // Reconcile project statuses against actual Docker container state,
    // then refresh the project list so the UI reflects reality.
    try {
      setProjects(await reconcileProjectStatuses());
    } catch {
      // If reconciliation fails (e.g. Docker hiccup), just load from store
      refresh();
    }
  }, [checkImage, setProjects, refresh]);

  // Single STT instance bound to the active session. The mic lives in the
  // StatusBar; the terminal's Ctrl+Shift+M shortcut calls stt.toggle via the
  // store (registered below).
  const { sendInput } = useTerminal();
  const stt = useSTT(activeSessionId ?? "", sendInput);
  useEffect(() => {
    setSttToggle(stt.toggle);
  }, [stt.toggle, setSttToggle]);

  useContainerProgress();
  useKeyboardShortcuts();

  // Initialize on mount
  useEffect(() => {
    loadSettings();
    let stopPolling: (() => void) | undefined;
    checkDocker().then((available) => {
      if (available) {
        onDockerReady();
      } else {
        setShowInstallDialog(true);
        stopPolling = startDockerPolling(onDockerReady);
      }
    });
    refresh();

    // Update detection
    loadVersion();
    const updateTimer = setTimeout(() => {
      checkForUpdates();
      checkImageUpdate();
    }, 3000);
    const cleanup = startPeriodicCheck();
    return () => {
      clearTimeout(updateTimer);
      cleanup?.();
      stopPolling?.();
    };
  }, []); // eslint-disable-line react-hooks/exhaustive-deps

  // The backend prevents the window closing so it can stop containers first,
  // which freezes the UI for several seconds. This says why.
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    let cancelled = false;
    listen("app-shutting-down", () => setShuttingDown(true))
      .then((fn) => {
        if (cancelled) fn();
        else unlisten = fn;
      })
      .catch((e) => console.error("Failed to listen for shutdown:", e));
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, []);

  const homeProjectIds = tabOrder.filter(isHomeTab).map(tabKeyId);

  return (
    <div className="flex flex-col h-screen p-3 gap-3 bg-[var(--bg-primary)]">
      <TopBar />
      <div className="flex flex-1 min-h-0 gap-3">
        <Sidebar />
        <main className="flex-1 bg-[var(--bg-secondary)] border border-[var(--border-color)] rounded-[var(--radius-panel)] min-w-0 overflow-hidden">
          {tabOrder.length === 0 ? (
            <WelcomeScreen />
          ) : (
            <div className="w-full h-full">
              {/* Every tab stays mounted and the inactive ones are merely
                  `hidden`, which a dialog's portal to `document.body` does not
                  inherit: a confirmation opened in one project stayed painted
                  over whatever tab the user switched to, kept its focus trap,
                  and — being a blocking overlay — refused every native file
                  drop in the window. `PaneVisibilityProvider` is how a `Modal`
                  inside a pane finds out the pane stepped aside. */}
              {homeProjectIds.map((projectId) => (
                <PaneVisibilityProvider
                  key={projectId}
                  visible={activeTabKey === homeTabKey(projectId)}
                >
                  <ProjectHome
                    projectId={projectId}
                    active={activeTabKey === homeTabKey(projectId)}
                  />
                </PaneVisibilityProvider>
              ))}
              {sessions.map((session) => (
                <PaneVisibilityProvider
                  key={session.id}
                  visible={session.id === activeSessionId}
                >
                  <TerminalView
                    sessionId={session.id}
                    active={session.id === activeSessionId}
                  />
                </PaneVisibilityProvider>
              ))}
            </div>
          )}
        </main>
      </div>
      <StatusBar stt={stt} />
      <ToastHost />
      {showInstallDialog && (
        <DockerInstallDialog onClose={() => setShowInstallDialog(false)} />
      )}
      {shuttingDown && (
        <div
          className="fixed inset-0 z-50 flex items-center justify-center bg-[var(--bg-primary)]/95 backdrop-blur-sm"
          role="status"
          aria-live="polite"
          /* Covers the whole window, so no pane underneath may accept a
             native file drop while it is up — see `lib/dropTarget.ts`. */
          data-blocks-drop="true"
          data-testid="shutdown-overlay"
        >
          <div className="flex flex-col items-center gap-2 px-6 text-center">
            <StatusIndicator tone="busy" label="Shutting down" className="text-sm" />
            <p className="text-[13px] text-[var(--text-secondary)]">
              Stopping containers before quitting. This window will close on its own.
            </p>
          </div>
        </div>
      )}
    </div>
  );
}

/**
 * First run is a checklist, not a paragraph: it reuses state the app already
 * tracks and ends in a real button.
 */
function WelcomeScreen() {
  const { dockerAvailable, imageExists, projects, openProjectHome } = useAppState(
    useShallow((s) => ({
      dockerAvailable: s.dockerAvailable,
      imageExists: s.imageExists,
      projects: s.projects,
      openProjectHome: s.openProjectHome,
    })),
  );
  const [showAdd, setShowAdd] = useState(false);

  const steps: {
    label: string;
    state: boolean | null;
    pendingLabel: string;
    failLabel: string;
  }[] = [
    {
      label: "Docker detected",
      state: dockerAvailable,
      pendingLabel: "Checking for Docker…",
      failLabel: "Docker not available",
    },
    {
      label: "Container image ready",
      state: imageExists,
      pendingLabel: "Checking for the image…",
      failLabel: "Image not pulled yet — see Settings › Container",
    },
    {
      label: `${projects.length} project${projects.length === 1 ? "" : "s"} configured`,
      state: projects.length > 0 ? true : false,
      pendingLabel: "",
      failLabel: "No projects yet",
    },
  ];

  return (
    <div className="flex items-center justify-center h-full p-6">
      <div className="w-full max-w-md">
        <h1 className="text-xl font-semibold text-[var(--text-primary)]">Triple-C</h1>
        <p className="text-[13px] text-[var(--text-secondary)] mb-5">
          Claude Code, sandboxed in a container.
        </p>

        <ol className="space-y-2 mb-5">
          {steps.map((step) => (
            <li
              key={step.label}
              className="flex items-center gap-2 px-3 py-2 bg-[var(--bg-primary)] border border-[var(--border-color)] rounded-[var(--radius-control)]"
            >
              <StatusIndicator
                tone={step.state === true ? "ok" : step.state === false ? "error" : "unknown"}
                label={
                  step.state === true
                    ? step.label
                    : step.state === false
                      ? step.failLabel
                      : step.pendingLabel
                }
                className="text-[13px]"
              />
            </li>
          ))}
        </ol>

        <div className="flex items-center gap-2">
          <Button size="md" variant="primary" onClick={() => setShowAdd(true)}>
            {projects.length === 0 ? "Add your first project" : "Add a project"}
          </Button>
          {projects.length > 0 && (
            <Button size="md" onClick={() => openProjectHome(projects[0].id)}>
              Open {projects[0].name}
            </Button>
          )}
        </div>

        <p className="mt-4 text-xs text-[var(--text-secondary)]">
          Then start its container and press{" "}
          <kbd className="px-1 py-0.5 font-mono bg-[var(--bg-tertiary)] border border-[var(--border-color)] rounded-[4px]">
            Ctrl+T
          </kbd>{" "}
          to open a Claude terminal.
        </p>

        {showAdd && <AddProjectDialog onClose={() => setShowAdd(false)} />}
      </div>
    </div>
  );
}
