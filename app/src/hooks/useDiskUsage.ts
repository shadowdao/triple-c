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
 * pattern `useContainerMigration` uses. `runReclaim` and `destroy` do not need
 * it: the UI disables their buttons while `working` is set, so there is never
 * a second one to race.
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
  runReclaim: (targets: ReclaimTarget[]) => Promise<void>;
  destroy: (target: DestructiveTarget, confirmation: string) => Promise<void>;
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

  const scan = useCallback(async () => {
    const mine = ++generation.current;
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
      if (generation.current === mine) setScanning(false);
    }
  }, []);

  const runReclaim = useCallback(async (targets: ReclaimTarget[]) => {
    if (targets.length === 0) return;
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
    } catch (e) {
      setError(String(e));
    } finally {
      setWorking(false);
    }
  }, []);

  const destroy = useCallback(async (target: DestructiveTarget, confirmation: string) => {
    setWorking(true);
    setError(null);
    try {
      const result = await commands.destroyProjectDiskObject(target, confirmation);
      setOutcome({ results: [result], total_freed_bytes: result.freed_bytes });
      // Same reasoning as `runReclaim`: the destructive list named an object
      // that is now gone.
      setPlan(null);
    } catch (e) {
      setError(String(e));
    } finally {
      setWorking(false);
    }
  }, []);

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
  }, []);

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
