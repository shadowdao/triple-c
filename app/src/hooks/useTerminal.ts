import { useCallback } from "react";
import { useShallow } from "zustand/react/shallow";
import { listen } from "@tauri-apps/api/event";
import { useAppState } from "../store/appState";
import * as commands from "../lib/tauri-commands";

/**
 * Per-session ordered write queue.
 *
 * Every keystroke used to be its own `invoke("terminal_input")`, and because
 * that command is `async` on the Rust side Tauri spawns each one as an
 * independent task. Those tasks then race for the session mutex in
 * `ExecSessionManager::send_input`, so nothing preserved the order the bytes
 * were typed in — the visible symptom was a backspace landing *after* the
 * characters typed behind it. The serial writer task downstream cannot help,
 * because the order is already lost by the time anything reaches the channel.
 *
 * The queue restores ordering the same way the web terminal gets it for free:
 * one write in flight at a time, the next only after the previous resolves.
 * Anything typed while a write is in flight coalesces into the next chunk,
 * which also collapses a burst of typing into a couple of IPC round trips
 * rather than one per key. Concatenating the byte arrays is safe — a PTY
 * cannot tell one write of "ab" from writes of "a" then "b" — and each
 * caller's promise still settles only when its own bytes have gone, so
 * `await sendInput(...)` keeps the meaning it had.
 *
 * Module scope, not hook scope, because `useTerminal()` is called from several
 * components (App for speech-to-text, TerminalView for typing and image paste,
 * useProjectActions for tile commands). A per-hook queue would give each caller
 * its own ordering and leave them racing against each other.
 */
type PendingWrite = {
  bytes: number[];
  resolve: () => void;
  reject: (reason: unknown) => void;
};

const inputQueues = new Map<string, { pending: PendingWrite[]; draining: boolean }>();

async function drainInputQueue(sessionId: string): Promise<void> {
  const q = inputQueues.get(sessionId);
  if (!q || q.draining) return;

  q.draining = true;
  try {
    while (q.pending.length > 0) {
      // Take everything queued so far as one batch, preserving order.
      const batch = q.pending.splice(0, q.pending.length);
      const bytes = batch.flatMap((w) => w.bytes);
      try {
        await commands.terminalInput(sessionId, bytes);
        batch.forEach((w) => w.resolve());
      } catch (err) {
        // Reject only the writes in this batch. Anything queued while it was
        // in flight is still pending and gets its own attempt on the next lap.
        batch.forEach((w) => w.reject(err));
      }
    }
  } finally {
    q.draining = false;
    // Drop the entry once idle so closed sessions do not accumulate.
    if (q.pending.length === 0) inputQueues.delete(sessionId);
  }
}

function enqueueInput(sessionId: string, bytes: number[]): Promise<void> {
  return new Promise<void>((resolve, reject) => {
    let q = inputQueues.get(sessionId);
    if (!q) {
      q = { pending: [], draining: false };
      inputQueues.set(sessionId, q);
    }
    q.pending.push({ bytes, resolve, reject });
    void drainInputQueue(sessionId);
  });
}

/** Drop any queued input for a session that is going away. */
function discardInputQueue(sessionId: string): void {
  const q = inputQueues.get(sessionId);
  if (!q) return;
  const dropped = q.pending.splice(0, q.pending.length);
  dropped.forEach((w) => w.reject(new Error(`Session ${sessionId} closed`)));
  if (!q.draining) inputQueues.delete(sessionId);
}

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

      discardInputQueue(sessionId);
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
      await enqueueInput(sessionId, bytes);
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
