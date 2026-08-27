import { useCallback, useState } from "react";
import { save } from "@tauri-apps/plugin-dialog";
import type { Project } from "../lib/types";
import * as commands from "../lib/tauri-commands";
import { formatBytes } from "../lib/formatBytes";
import { useAppState } from "../store/appState";
import { useProjects } from "./useProjects";
import { useTerminal } from "./useTerminal";

/**
 * Lifecycle + terminal actions for one project, shared by the sidebar row and
 * Project Home so both surfaces behave identically. Failures raise a toast
 * (with expandable detail) rather than a blocking modal or a 12px card line.
 */
export function useProjectActions(project: Project) {
  const { start, stop, rebuild } = useProjects();
  const { open: openTerminal, sendInput } = useTerminal();
  const pushToast = useAppState((s) => s.pushToast);
  const setContainerProgress = useAppState((s) => s.setContainerProgress);
  const [busy, setBusy] = useState(false);
  const [backingUp, setBackingUp] = useState(false);

  const fail = useCallback(
    (message: string, error: unknown) => {
      pushToast({ kind: "error", message, detail: String(error) });
    },
    [pushToast],
  );

  const run = useCallback(
    async <T,>(label: string, fn: () => Promise<T>): Promise<T | undefined> => {
      setBusy(true);
      setContainerProgress(project.id, null);
      try {
        return await fn();
      } catch (e) {
        fail(`${label} failed for “${project.name}”`, e);
        return undefined;
      } finally {
        setContainerProgress(project.id, null);
        setBusy(false);
      }
    },
    [fail, project.id, project.name, setContainerProgress],
  );

  const handleStart = useCallback(
    () => run("Start", () => start(project.id)),
    [run, start, project.id],
  );

  const handleStop = useCallback(
    () => run("Stop", () => stop(project.id)),
    [run, stop, project.id],
  );

  const handleReset = useCallback(
    () =>
      run("Reset", async () => {
        const outcome = await rebuild(project.id);
        if (outcome.leftover_volumes.length > 0) {
          const n = outcome.leftover_volumes.length;
          pushToast({
            kind: "error",
            message: `Reset for “${project.name}” could not fully clean up`,
            detail: `${n === 1 ? "A volume" : `${n} volumes`} could not be removed, so the new \
container may still contain data from before the reset. You may need to remove ${n === 1 ? "it" : "them"} \
manually with \`docker volume rm\`.`,
          });
        }
        return outcome;
      }),
    [run, rebuild, project.id, project.name, pushToast],
  );

  const openClaudeTerminal = useCallback(async () => {
    try {
      return await openTerminal(project.id, project.name);
    } catch (e) {
      fail(`Could not open a Claude terminal for “${project.name}”`, e);
      return null;
    }
  }, [openTerminal, project.id, project.name, fail]);

  const openShell = useCallback(async () => {
    try {
      return await openTerminal(project.id, project.name, "bash");
    } catch (e) {
      fail(`Could not open a shell for “${project.name}”`, e);
      return null;
    }
  }, [openTerminal, project.id, project.name, fail]);

  /**
   * Open a shell tab and type `command` into it. Used for [Resume] and for the
   * capability drawer's "Manage in terminal" — the backend has no
   * run-a-command entry point, so we drive the shell we just opened.
   */
  const openTerminalWithCommand = useCallback(
    async (command: string, sessionLabel?: string) => {
      try {
        const sessionId = await openTerminal(
          project.id,
          project.name,
          "bash",
          sessionLabel,
        );
        // Give the login shell a moment to draw its prompt before typing.
        setTimeout(() => {
          sendInput(sessionId, `${command}\n`).catch((e) =>
            fail("Could not send the command to the terminal", e),
          );
        }, 700);
        return sessionId;
      } catch (e) {
        fail(`Could not open a terminal for “${project.name}”`, e);
        return null;
      }
    },
    [openTerminal, sendInput, project.id, project.name, fail],
  );

  const handleBackup = useCallback(async () => {
    if (!project.container_id) {
      pushToast({
        kind: "error",
        message: "Start the project at least once before backing up.",
      });
      return;
    }
    const stamp = new Date().toISOString().slice(0, 19).replace(/[:T]/g, "-");
    const safeName = project.name.replace(/[^a-zA-Z0-9_-]+/g, "_");
    try {
      const hostPath = await save({
        defaultPath: `${safeName}-backup-${stamp}.tar.gz`,
        filters: [{ name: "Gzipped tarball", extensions: ["tar.gz"] }],
      });
      if (!hostPath) return;
      setBackingUp(true);
      const bytes = await commands.downloadContainerBackup(project.id, hostPath);
      // `binary` matches what the host's file browser will say about the
      // tarball this just wrote. The unit is part of the formatted string, so
      // there is no separate " MB" to append — and unlike the inline
      // `toFixed(1)` this replaced, a multi-gigabyte backup no longer reports
      // itself as a five-digit number of megabytes.
      const size = formatBytes(bytes, { binary: true });
      pushToast({
        kind: "success",
        message: `Backup saved (${size}).`,
        detail:
          "Includes Claude config — may contain API keys. Keep the archive private.",
      });
    } catch (e) {
      fail("Backup failed", e);
    } finally {
      setBackingUp(false);
    }
  }, [project.container_id, project.id, project.name, pushToast, fail]);

  return {
    busy,
    backingUp,
    handleStart,
    handleStop,
    handleReset,
    handleBackup,
    openClaudeTerminal,
    openShell,
    openTerminalWithCommand,
  };
}
