import { useCallback, useRef, useState } from "react";
import * as commands from "../lib/tauri-commands";
import type {
  DestructiveTarget,
  DiskUsageReport,
  ReclaimOutcome,
  ReclaimPlan,
  ReclaimTarget,
} from "../lib/types";

/**
 * State for the Disk section.
 *
 * ## Why nothing here runs on mount
 *
 * A scan is `GET /system/df`, which walks every image, container and volume on
 * the daemon and computes shared-layer sizes. On a 100 GB store that is
 * seconds. `AccordionSection` unmounts its body when collapsed, so a
 * `useEffect` scan would re-run every single time the user opened the section.
 * The scan is therefore only ever what the Scan button calls.
 *
 * Note what that does *not* buy: this hook lives inside `DiskSettings`, which
 * the accordion unmounts on collapse, so its state goes with it and reopening
 * the section shows an unscanned panel again. That is the honest behaviour —
 * a stale total is worse than an absent one — but it means collapsing and
 * reopening discards a scan the user paid for. Lifting the report into
 * `appState` would fix that and is deliberately not done here: it would put a
 * multi-megabyte, rapidly-stale blob into the app-wide store for one panel.
 *
 * ## The generation guard
 *
 * A user who hits Scan twice can have two `df()` calls in flight, and they can
 * land out of order — the second one is not necessarily slower. Every async
 * write in `scan` checks it is still the newest before it lands, the same
 * pattern `useContainerMigration` uses.
 *
 * The race that actually bites, though, is not scan-versus-scan: it is
 * scan-versus-**mutation**. A scan takes seconds and does not set `working`, so
 * nothing stopped a reclaim starting on top of one. The reclaim correctly drops
 * the plan — and then the still-running scan landed, passed its own generation
 * check, and repainted a pre-reclaim report *plus a fresh, clickable plan
 * listing objects that had just been deleted*. So every mutation bumps the
 * counter as well: whatever a scan is holding was measured before the mutation
 * and is now a lie, and throwing it away is the only honest thing to do with
 * it. (The Scan button is disabled while `working` for the mirror-image case,
 * so a scan can never start *during* a mutation.)
 *
 * That is also why `scanning` is not cleared against the same counter: a
 * mutation bumping it mid-scan would strand the flag at true and leave the
 * button reading "Scanning…" forever. `latestScan` records the generation the
 * newest *scan* owns — only a newer scan may take the flag away — and that is
 * what the `finally` compares against.
 */
export interface DiskUsageState {
  report: DiskUsageReport | null;
  plan: ReclaimPlan | null;
  /** A scan is in flight. */
  scanning: boolean;
  /** A reclaim or a destroy is in flight. */
  working: boolean;
  error: string | null;
  /** The outcome of the last reclaim, kept on screen until the next scan. */
  outcome: ReclaimOutcome | null;
  scan: () => Promise<void>;
  /**
   * Resolves `true` when the call came back, `false` when it threw and the
   * failure went into `error`. Callers that dismiss UI on completion — the
   * confirmation dialogs — must only dismiss on `true`, or the failure is left
   * with nowhere on screen the user is looking.
   */
  runReclaim: (targets: ReclaimTarget[]) => Promise<boolean>;
  /** Same contract as `runReclaim`: `false` means it failed and `error` says how. */
  destroy: (target: DestructiveTarget, confirmation: string) => Promise<boolean>;
  /** Run the orphaned-snapshot sweep and report what it found *and refused*. */
  runSweep: () => Promise<void>;
  clearOutcome: () => void;
}

export function useDiskUsage(): DiskUsageState {
  const [report, setReport] = useState<DiskUsageReport | null>(null);
  const [plan, setPlan] = useState<ReclaimPlan | null>(null);
  const [scanning, setScanning] = useState(false);
  const [working, setWorking] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [outcome, setOutcome] = useState<ReclaimOutcome | null>(null);
  const generation = useRef(0);
  /** The generation belonging to the most recently *started* scan. */
  const latestScan = useRef(0);

  /**
   * Retire every in-flight scan. Called at the top of each mutation, because
   * the moment we start deleting things, a measurement taken before that is no
   * longer describing the daemon the user is looking at.
   */
  const invalidateScans = useCallback(() => {
    generation.current += 1;
  }, []);

  const scan = useCallback(async () => {
    const mine = ++generation.current;
    latestScan.current = mine;
    setScanning(true);
    setError(null);
    // The previous outcome describes a state that no longer holds once a new
    // scan starts, so it goes rather than sitting beside fresh numbers.
    setOutcome(null);
    try {
      const next = await commands.getDockerDiskUsage();
      if (generation.current !== mine) return;
      // Planning is cheap and always wanted: the classification is what makes
      // the numbers actionable, and it reuses the report rather than scanning
      // again.
      const nextPlan = await commands.listReclaimable(next);
      if (generation.current !== mine) return;
      // Both land together, or neither does. Setting the report before
      // awaiting the plan would render this scan's totals above the *previous*
      // scan's still-clickable tick list if the plan call failed.
      setReport(next);
      setPlan(nextPlan);
    } catch (e) {
      if (generation.current !== mine) return;
      setError(String(e));
      // The old report is left on screen deliberately — it is still an
      // accurate measurement of an earlier moment, and the error says the
      // refresh failed. What must not survive is a plan describing a scan the
      // user can no longer see the totals for, but that cannot happen: the two
      // only ever move together.
    } finally {
      // Deliberately `latestScan`, not `generation`: a mutation that retired
      // this scan did not start another one, so this scan is still the last
      // word on whether a scan is running.
      if (latestScan.current === mine) setScanning(false);
    }
  }, []);

  const runReclaim = useCallback(
    async (targets: ReclaimTarget[]): Promise<boolean> => {
      // Nothing was asked for, so nothing failed — a caller gating a dialog on
      // this must not be left staring at an error that has no cause.
      if (targets.length === 0) return true;
      invalidateScans();
      setWorking(true);
      setError(null);
      try {
        const result = await commands.reclaim(targets);
        setOutcome(result);
        // **The plan is now stale and must not stay clickable.** Its rows
        // describe objects this call just removed, so leaving them ticked lets
        // the user fire the same reclaim again against nothing. Dropping the plan
        // (not the report) leaves the totals on screen, marked as measured before
        // the reclaim, with the tick list gone.
        //
        // Deliberately no automatic re-scan: it costs another `df()`, and the
        // outcome already reports measured bytes for every target — a user who
        // wants the new totals asks for them.
        setPlan(null);
        return true;
      } catch (e) {
        setError(String(e));
        return false;
      } finally {
        setWorking(false);
      }
    },
    [invalidateScans],
  );

  const destroy = useCallback(
    async (target: DestructiveTarget, confirmation: string): Promise<boolean> => {
      invalidateScans();
      setWorking(true);
      setError(null);
      try {
        const result = await commands.destroyProjectDiskObject(target, confirmation);
        setOutcome({ results: [result], total_freed_bytes: result.freed_bytes });
        // Same reasoning as `runReclaim`: the destructive list named an object
        // that is now gone.
        setPlan(null);
        return true;
      } catch (e) {
        setError(String(e));
        return false;
      } finally {
        setWorking(false);
      }
    },
    [invalidateScans],
  );

  /**
   * The startup sweep, on demand.
   *
   * Not the same as ticking "superseded snapshot layers", even though both end
   * up removing the same images: this reports `in_use` — the orphans Docker
   * *refused* to delete because a stopped project's container still needs
   * them. That refusal is the sweep's third safety net and it is invisible
   * everywhere else in the app, because every existing caller throws the
   * report away.
   */
  const runSweep = useCallback(async () => {
    invalidateScans();
    setWorking(true);
    setError(null);
    try {
      const sweep = await commands.sweepOrphanedSnapshots();
      if (sweep.unavailable) {
        setError(sweep.unavailable);
        return;
      }
      const refused =
        sweep.in_use > 0
          ? ` ${sweep.in_use} were left alone because a container is still built from them — start and stop, or recreate, that project and a later sweep gets them.`
          : "";
      setOutcome({
        results: [
          {
            target: { kind: "dangling_snapshots" },
            destroyed: null,
            ok: sweep.failed.length === 0,
            freed_bytes: sweep.reclaimed_bytes,
            projected_bytes: null,
            message: `Swept ${sweep.removed.length} superseded image(s).${refused}`,
          },
        ],
        total_freed_bytes: sweep.reclaimed_bytes,
      });
      setPlan(null);
    } catch (e) {
      setError(String(e));
    } finally {
      setWorking(false);
    }
  }, [invalidateScans]);

  const clearOutcome = useCallback(() => setOutcome(null), []);

  return {
    report,
    plan,
    scanning,
    working,
    error,
    outcome,
    scan,
    runReclaim,
    destroy,
    runSweep,
    clearOutcome,
  };
}
