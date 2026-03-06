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
    async (projectId: string, projectName: string, sessionType: "claude" | "bash" = "claude") => {
      const sessionId = crypto.randomUUID();
      await commands.openTerminalSession(projectId, sessionId, sessionType);
      addSession({ id: sessionId, projectId, projectName, sessionType });
      return sessionId;
    },
    [addSession],
  );

  const close = useCallback(
    async (sessionId: string) => {
      await commands.closeTerminalSession(sessionId);
      removeSession(sessionId);
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
