import { useEffect } from "react";
import Sidebar from "./components/layout/Sidebar";
import TopBar from "./components/layout/TopBar";
import StatusBar from "./components/layout/StatusBar";
import TerminalView from "./components/terminal/TerminalView";
import { useDocker } from "./hooks/useDocker";
import { useSettings } from "./hooks/useSettings";
import { useProjects } from "./hooks/useProjects";
import { useAppState } from "./store/appState";

export default function App() {
  const { checkDocker, checkImage } = useDocker();
  const { checkApiKey } = useSettings();
  const { refresh } = useProjects();
  const { sessions, activeSessionId } = useAppState();

  // Initialize on mount
  useEffect(() => {
    checkDocker();
    checkImage();
    checkApiKey();
    refresh();
  }, []); // eslint-disable-line react-hooks/exhaustive-deps

  return (
    <div className="flex flex-col h-screen p-6 gap-4 bg-[var(--bg-primary)]">
      <TopBar />
      <div className="flex flex-1 min-h-0 gap-4">
        <Sidebar />
        <main className="flex-1 bg-[var(--bg-secondary)] border border-[var(--border-color)] rounded-lg min-w-0 overflow-hidden">
          {sessions.length === 0 ? (
            <WelcomeScreen />
          ) : (
            <div className="w-full h-full">
              {sessions.map((session) => (
                <TerminalView
                  key={session.id}
                  sessionId={session.id}
                  active={session.id === activeSessionId}
                />
              ))}
            </div>
          )}
        </main>
      </div>
      <StatusBar />
    </div>
  );
}

function WelcomeScreen() {
  return (
    <div className="flex items-center justify-center h-full text-[var(--text-secondary)]">
      <div className="text-center">
        <h1 className="text-3xl font-bold mb-2 text-[var(--text-primary)]">
          Triple-C
        </h1>
        <p className="text-sm mb-4">Claude Code Container</p>
        <p className="text-xs max-w-md">
          Add a project from the sidebar, start its container, then open a
          terminal to begin using Claude Code in a sandboxed environment.
        </p>
      </div>
    </div>
  );
}
