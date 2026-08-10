import { useCallback, useEffect, useRef, useState } from "react";
import type {
  ContainerStaleness,
  MigrationOptions,
  MigrationReport,
  MigrationState,
  Project,
} from "../lib/types";
import {
  MIGRATION_PHASE_AWAITING_CONFIRMATION,
  MIGRATION_PHASE_IN_PROGRESS,
  MIGRATION_PHASE_INTERRUPTED,
} from "../lib/types";
import * as commands from "../lib/tauri-commands";
import { useAppState } from "../store/appState";

/**
 * Unsettled phases from `MigrationState.phase` (hyphenated, unlike the
 * outcome phases on `MigrationReport`). Compared as strings on purpose: the
 * backend types this loosely so an unrecognised value from a future build
 * cannot crash the UI, and neither can it here — an unknown phase simply
 * surfaces nothing rather than throwing.
 */
const IN_PROGRESS = MIGRATION_PHASE_IN_PROGRESS;
const INTERRUPTED = MIGRATION_PHASE_INTERRUPTED;
const AWAITING = MIGRATION_PHASE_AWAITING_CONFIRMATION;

export interface ContainerMigration {
  /** Null until the first probe returns, or when the container has never been created. */
  staleness: ContainerStaleness | null;
  probing: boolean;
  /** True while a migration is running — whether we started it or found it. */
  running: boolean;
  /** True when the run in progress was recovered from disk, not started here. */
  recovered: boolean;
  /**
   * A migration the app died in the middle of. It is not running and it has no
   * report: the container is mid-swap until someone resumes or rolls it back.
   */
  interrupted: MigrationState | null;
  /** Re-enter an interrupted migration. The backend continues the same run. */
  resume: () => Promise<void>;
  /** The settled report, kept until the user keeps, rolls back or dismisses it. */
  report: MigrationReport | null;
  /** Progress lines from `container-progress`, oldest first. */
  log: string[];
  /** The most recent progress line, or null before the first one arrives. */
  phaseMessage: string | null;
  /** True while confirm/rollback is in flight. */
  busy: boolean;
  start: (options: MigrationOptions) => Promise<void>;
  keep: () => Promise<void>;
  rollback: () => Promise<void>;
  /** Clear a report we cannot act on (failed / rolled back). Local only. */
  dismiss: () => void;
  refresh: () => Promise<void>;
}

/**
 * Container base-image migration for one project.
 *
 * Three things have to survive a closed modal: the run itself, the progress
 * log, and the report. A migration takes minutes, so the modal is a *view* onto
 * this hook rather than the thing that owns the work — closing it must not
 * cancel anything. The hook lives in `ProjectHome`, above both the modal and
 * the Overview banner, so either surface can be showing at any point.
 *
 * A migration the app died in the middle of is picked up from
 * `getMigrationState` on mount — as `interrupted`, which is offered for resume,
 * or as `awaiting-confirmation`, whose report is put back on screen. Without
 * that, a half-migrated container would look identical to a healthy one, which
 * is the exact failure mode this whole feature exists to fix.
 */
export function useContainerMigration(project: Project): ContainerMigration {
  const projectId = project.id;
  const [staleness, setStaleness] = useState<ContainerStaleness | null>(null);
  const [probing, setProbing] = useState(false);
  const [running, setRunning] = useState(false);
  const [recovered, setRecovered] = useState(false);
  const [interrupted, setInterrupted] = useState<MigrationState | null>(null);
  const [report, setReport] = useState<MigrationReport | null>(null);
  const [log, setLog] = useState<string[]>([]);
  const [busy, setBusy] = useState(false);
  const pushToast = useAppState((s) => s.pushToast);
  const progress = useAppState((s) => s.containerProgress[projectId]);

  // Guards a late response from an earlier project overwriting a newer one.
  const generation = useRef(0);

  const refresh = useCallback(async () => {
    const gen = ++generation.current;
    if (!project.container_id) {
      setStaleness(null);
      return;
    }
    setProbing(true);
    try {
      const next = await commands.getContainerStaleness(projectId);
      if (gen === generation.current) setStaleness(next);
    } catch {
      // A probe that cannot reach the container is "we do not know", which is
      // an absent banner rather than an error one — the same call is retried
      // whenever the container's status changes.
      if (gen === generation.current) setStaleness(null);
    } finally {
      if (gen === generation.current) setProbing(false);
    }
  }, [projectId, project.container_id]);

  // Probe staleness when the container settles into a new state. The probe runs
  // two filesystem walks and is explicitly not for polling, so it is skipped
  // mid-transition and mid-run — a reading taken while the container is being
  // swapped describes neither the old system layer nor the new one.
  const settled = project.status !== "starting" && project.status !== "stopping";
  useEffect(() => {
    if (running || !settled) return;
    void refresh();
  }, [refresh, settled, running]);

  // Crash recovery: adopt whatever the backend still has on record.
  useEffect(() => {
    let cancelled = false;
    commands
      .getMigrationState(projectId)
      .then((state) => {
        if (cancelled || !state) return;
        if (state.phase === IN_PROGRESS) {
          // Something is still driving it; watch rather than restart.
          setRunning(true);
          setRecovered(true);
        } else if (state.phase === INTERRUPTED) {
          // Nothing is driving it. The container is mid-swap and will stay that
          // way until someone resumes — so this must be visible, not silent.
          setInterrupted(state);
        } else if (state.phase === AWAITING && state.report) {
          setReport(state.report);
        }
      })
      .catch(() => {
        /* No recorded state is the normal case. */
      });
    return () => {
      cancelled = true;
    };
  }, [projectId]);

  // A recovered run has no promise to await, so poll it to completion.
  useEffect(() => {
    if (!running || !recovered) return;
    let cancelled = false;
    const timer = setInterval(() => {
      commands
        .getMigrationState(projectId)
        .then((state: MigrationState | null) => {
          if (cancelled || state?.phase === IN_PROGRESS) return;
          setRunning(false);
          setRecovered(false);
          // A cleared record means it was confirmed or rolled back elsewhere.
          if (!state) {
            void refresh();
            return;
          }
          if (state.phase === INTERRUPTED) {
            setInterrupted(state);
            return;
          }
          if (state.report) setReport(state.report);
          void refresh();
        })
        .catch(() => {
          /* Keep polling; a transient IPC failure is not an outcome. */
        });
    }, 2500);
    return () => {
      cancelled = true;
      clearInterval(timer);
    };
  }, [running, recovered, projectId, refresh]);

  // Accumulate the shared progress line into a scrollback the modal can show.
  // The store collapses repeats, so identical consecutive apt lines appear once.
  useEffect(() => {
    if (!running || !progress) return;
    setLog((prev) =>
      prev[prev.length - 1] === progress ? prev : [...prev, progress],
    );
  }, [progress, running]);

  const start = useCallback(
    async (options: MigrationOptions) => {
      setLog([]);
      setReport(null);
      setRecovered(false);
      setInterrupted(null);
      setRunning(true);
      try {
        const result = await commands.migrateProjectToBase(projectId, options);
        setReport(result);
      } catch (e) {
        // A rejected call means the backend never produced a report. Synthesise
        // the failed shape so the report surface — not a toast that scrolls
        // away — is still what tells the user.
        setReport({
          phase: "failed",
          packages_requested: [],
          packages_installed: [],
          packages_failed: [],
          paths_copied: [],
          features_restored: [],
          rollback_available: false,
          message: String(e),
        });
      } finally {
        setRunning(false);
        useAppState.getState().setContainerProgress(projectId, null);
        void refresh();
      }
    },
    [projectId, refresh],
  );

  /**
   * Re-enter an interrupted migration. The backend continues that run rather
   * than starting a new one, and the recorded options are replayed as-is — the
   * deltas cannot be recomputed once the container has already been swapped.
   */
  const resume = useCallback(async () => {
    const pending = interrupted;
    if (!pending) return;
    await start(pending.options);
  }, [interrupted, start]);

  const keep = useCallback(async () => {
    setBusy(true);
    try {
      await commands.confirmMigration(projectId);
      setReport(null);
      await refresh();
    } catch (e) {
      pushToast({
        kind: "error",
        message: `Could not discard the rollback image for “${project.name}”`,
        detail: String(e),
      });
    } finally {
      setBusy(false);
    }
  }, [projectId, project.name, refresh, pushToast]);

  const rollback = useCallback(async () => {
    setBusy(true);
    try {
      await commands.rollbackMigration(projectId);
      setReport(null);
      setInterrupted(null);
      pushToast({
        kind: "success",
        message: `“${project.name}” is back on its previous system layer.`,
        detail:
          "Volumes were not touched, so anything written to your home directory or workspace during the update is still there.",
      });
      await refresh();
    } catch (e) {
      pushToast({
        kind: "error",
        message: `Rollback failed for “${project.name}”`,
        detail: String(e),
      });
    } finally {
      setBusy(false);
    }
  }, [projectId, project.name, refresh, pushToast]);

  const dismiss = useCallback(() => setReport(null), []);

  return {
    staleness,
    probing,
    running,
    recovered,
    interrupted,
    report,
    log,
    phaseMessage: log.length > 0 ? log[log.length - 1] : null,
    busy,
    start,
    resume,
    keep,
    rollback,
    dismiss,
    refresh,
  };
}
