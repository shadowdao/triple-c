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
 * The scan is therefore a `scan()` the Scan button calls and nothing else, and
 * the result lives in this hook rather than in the component so that reopening
 * the section shows the last result instead of paying again.
 *
 * ## The generation guard
 *
 * A user who hits Scan twice can have two `df()` calls in flight, and they can
 * land out of order — the second one is not necessarily slower. Every async
 * write checks it is still the newest before it lands, the same pattern
 * `useContainerMigration` uses.
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
      setReport(next);
      // Planning is cheap and always wanted: the classification is what makes
      // the numbers actionable, and it reuses the report rather than scanning
      // again.
      const nextPlan = await commands.listReclaimable(next);
      if (generation.current !== mine) return;
      setPlan(nextPlan);
    } catch (e) {
      if (generation.current !== mine) return;
      setError(String(e));
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
      // Deliberately no automatic re-scan. It costs another `df()`, and the
      // outcome already reports measured bytes for every target — a user who
      // wants the new totals asks for them.
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
    } catch (e) {
      setError(String(e));
    } finally {
      setWorking(false);
    }
  }, []);

  const clearOutcome = useCallback(() => setOutcome(null), []);

  return { report, plan, scanning, working, error, outcome, scan, runReclaim, destroy, clearOutcome };
}
