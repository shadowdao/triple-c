import { useCallback, useState } from "react";
import { save } from "@tauri-apps/plugin-dialog";
import type { Project, UnsavedWork, WipChoice } from "../lib/types";
import { isAwsProject, stopGuardNeeded } from "../lib/awsWorkspace";
import * as commands from "../lib/tauri-commands";
import { formatBytes } from "../lib/formatBytes";
import { describeResetLeftovers, resetLeftoverPronoun } from "../lib/resetOutcome";
import { useAppState } from "../store/appState";
import { useProjects } from "./useProjects";
import { useTerminal } from "./useTerminal";

/** How many task log lines a failed AWS start shows. */
const TASK_LOG_LINES = 30;

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
    async <T,>(
      label: string,
      fn: () => Promise<T>,
      explain?: (e: unknown) => Promise<string>,
    ): Promise<T | undefined> => {
      setBusy(true);
      setContainerProgress(project.id, null);
      try {
        return await fn();
      } catch (e) {
        fail(`${label} failed for “${project.name}”`, explain ? await explain(e) : e);
        return undefined;
      } finally {
        setContainerProgress(project.id, null);
        setBusy(false);
      }
    },
    [fail, project.id, project.name, setContainerProgress],
  );

  const aws = isAwsProject(project);

  /** An AWS start failure gets the task's last log lines, unless the error has them. */
  const withTaskLog = useCallback(
    async (e: unknown): Promise<string> => {
      const text = String(e);
      if (text.includes("Last log lines")) return text;
      try {
        const lines = await commands.awsTaskLog(project.id, TASK_LOG_LINES);
        if (lines.length > 0) return `${text}\n\nLast log lines:\n${lines.join("\n")}`;
      } catch {
        // The log is a courtesy; the start error is what matters.
      }
      return text;
    },
    [project.id],
  );

  const handleStart = useCallback(
    () => run("Start", () => start(project.id), aws ? withTaskLog : undefined),
    [run, start, project.id, aws, withTaskLog],
  );

  /**
   * The stop guard's question, set while it is asked: an AWS workspace with
   * work no remote has (or one that could not be checked) is not stopped
   * until the user picks Push, Stop anyway or Cancel.
   */
  const [stopGuard, setStopGuard] = useState<UnsavedWork | null>(null);

  const handleStop = useCallback(async () => {
    // A force stop of a transition skips the question: there is nothing
    // reliable to ask yet.
    if (aws && project.status === "running") {
      setBusy(true);
      let work: UnsavedWork;
      try {
        work = await commands.checkUnsavedWork(project.id);
      } catch (e) {
        work = { checked: false, repos: [], error: String(e) };
      } finally {
        setBusy(false);
      }
      if (stopGuardNeeded(work)) {
        setStopGuard(work);
        return undefined;
      }
    }
    return run("Stop", () => stop(project.id));
  }, [aws, project.status, project.id, run, stop]);

  const resolveStopGuard = useCallback(
    (choice: WipChoice | "unguarded" | null) => {
      setStopGuard(null);
      if (choice === null) return undefined;
      return run("Stop", async () => {
        // "unguarded": no choice is sent, so nothing is recorded in the
        // workspace and its own shutdown push runs as configured.
        const note = await stop(project.id, choice === "unguarded" ? undefined : choice);
        if (note) pushToast({ kind: "info", message: `“${project.name}” stopped`, detail: note });
        return note;
      });
    },
    [run, stop, project.id, project.name, pushToast],
  );

  const handleReset = useCallback(
    () =>
      run("Reset", async () => {
        const outcome = await rebuild(project.id);
        if (outcome.leftover_image || outcome.leftover_volumes.length > 0) {
          // Not "run `docker volume rm`" — by the time this renders, the new
          // container this same call just started already has the leftover
          // volume mounted, so that command would just hit the same 409
          // Reset did. Stopping the project first is what actually frees it.
          pushToast({
            kind: "error",
            message: `Reset for “${project.name}” did not fully clean up`,
            detail: `Triple-C could not remove ${describeResetLeftovers(outcome)} from before the reset, so \
the new container may still be built from, or contain, old data. Stop the project, then try \
Reset again, or remove ${resetLeftoverPronoun(outcome)} manually once stopped.`,
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
   * Open a Claude tab on a saved conversation. A real Claude session, not a
   * shell with `claude --resume` typed into it: the tab's session type is
   * what Shift+Enter, the status-bar hint and Send to agent key off, so a
   * resumed conversation in a "bash" tab submitted on Shift+Enter.
   */
  const resumeClaudeSession = useCallback(
    async (claudeSessionId: string, sessionLabel?: string) => {
      try {
        return await openTerminal(project.id, project.name, "claude", sessionLabel, claudeSessionId);
      } catch (e) {
        fail("Could not resume that session", e);
        return null;
      }
    },
    [openTerminal, project.id, project.name, fail],
  );

  /**
   * Open a shell tab and type `command` into it. Used for the capability
   * drawer's "Manage in terminal" — the backend has no run-a-command entry
   * point, so we drive the shell we just opened.
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
    stopGuard,
    resolveStopGuard,
    handleReset,
    handleBackup,
    openClaudeTerminal,
    openShell,
    resumeClaudeSession,
    openTerminalWithCommand,
  };
}
