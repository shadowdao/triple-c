import { useCallback } from "react";
import { useShallow } from "zustand/react/shallow";
import { listen } from "@tauri-apps/api/event";
import { useAppState } from "../store/appState";
import * as commands from "../lib/tauri-commands";

export function useTerminal() {
  const { sessions, activeSessionId, addSession, removeSession, setActiveSession } =
    useAppState(
      useShallow(s => ({
        sessions: s.sessions,
        activeSessionId: s.activeSessionId,
        addSession: s.addSession,
        removeSession: s.removeSession,
        setActiveSession: s.setActiveSession,
      }))
    );

  const open = useCallback(
    async (projectId: string, projectName: string, sessionType: "claude" | "bash" = "claude", sessionName?: string) => {
      const sessionId = crypto.randomUUID();
      await commands.openTerminalSession(projectId, sessionId, sessionType, sessionName);
      addSession({ id: sessionId, projectId, projectName, sessionType, sessionName: sessionName ?? null });
      return sessionId;
    },
    [addSession],
  );

  const close = useCallback(
    async (sessionId: string) => {
      // Capture session/project info before we drop it from local state.
      const { sessions: currentSessions, projects } = useAppState.getState();
      const session = currentSessions.find((s) => s.id === sessionId);
      const project = session ? projects.find((p) => p.id === session.projectId) : undefined;

      await commands.closeTerminalSession(sessionId);
      removeSession(sessionId);

      // Drop any persisted custom name for this session.
      if (project && project.renamed_session_names && sessionId in project.renamed_session_names) {
        const map = { ...project.renamed_session_names };
        delete map[sessionId];
        try {
          const updated = await commands.updateProject({ ...project, renamed_session_names: map });
          useAppState.getState().updateProjectInList(updated);
        } catch (err) {
          console.error("Failed to clear renamed tab name on close:", err);
        }
      }
    },
    [removeSession],
  );

  const sendInput = useCallback(
    async (sessionId: string, data: string) => {
      const bytes = Array.from(new TextEncoder().encode(data));
      await commands.terminalInput(sessionId, bytes);
    },
    [],
  );

  const resize = useCallback(
    async (sessionId: string, cols: number, rows: number) => {
      await commands.terminalResize(sessionId, cols, rows);
    },
    [],
  );

  const pasteImage = useCallback(
    async (sessionId: string, imageData: Uint8Array) => {
      const bytes = Array.from(imageData);
      return commands.pasteImageToTerminal(sessionId, bytes);
    },
    [],
  );

  const onOutput = useCallback(
    (sessionId: string, callback: (data: Uint8Array) => void) => {
      const eventName = `terminal-output-${sessionId}`;
      return listen<number[]>(eventName, (event) => {
        callback(new Uint8Array(event.payload));
      });
    },
    [],
  );

  const onExit = useCallback(
    (sessionId: string, callback: () => void) => {
      const eventName = `terminal-exit-${sessionId}`;
      return listen<void>(eventName, () => {
        callback();
      });
    },
    [],
  );

  return {
    sessions,
    activeSessionId,
    setActiveSession,
    open,
    close,
    sendInput,
    pasteImage,
    resize,
    onOutput,
    onExit,
  };
}
