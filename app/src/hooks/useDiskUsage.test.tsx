import { describe, it, expect, vi, beforeEach } from "vitest";
import { act, renderHook, waitFor } from "@testing-library/react";
import { useDiskUsage, type DiskUsageState } from "./useDiskUsage";
import type { DiskUsageReport } from "../lib/types";

const getDockerDiskUsage = vi.fn();
const listReclaimable = vi.fn();
const reclaim = vi.fn();
const destroyProjectDiskObject = vi.fn();

vi.mock("../lib/tauri-commands", () => ({
  getDockerDiskUsage: () => getDockerDiskUsage(),
  listReclaimable: (report: DiskUsageReport) => listReclaimable(report),
  reclaim: (targets: unknown) => reclaim(targets),
  destroyProjectDiskObject: (target: unknown, confirmation: string) =>
    destroyProjectDiskObject(target, confirmation),
  sweepOrphanedSnapshots: () => sweepOrphanedSnapshots(),
}));

const sweepOrphanedSnapshots = vi.fn();

const report = (scanned_at: string): DiskUsageReport =>
  ({ scanned_at, projects: [] }) as unknown as DiskUsageReport;

const plan = { items: [], destructive: [], store_error: null };

beforeEach(() => {
  vi.clearAllMocks();
  listReclaimable.mockResolvedValue(plan);
  reclaim.mockResolvedValue({ results: [], total_freed_bytes: 0 });
});

describe("useDiskUsage", () => {
  it("holds no report until a scan is asked for", () => {
    const { result } = renderHook(() => useDiskUsage());
    expect(result.current.report).toBeNull();
    expect(result.current.plan).toBeNull();
    expect(getDockerDiskUsage).not.toHaveBeenCalled();
  });

  it("scans, then plans off the same report rather than scanning again", async () => {
    getDockerDiskUsage.mockResolvedValue(report("first"));
    const { result } = renderHook(() => useDiskUsage());
    await act(async () => {
      await result.current.scan();
    });
    expect(getDockerDiskUsage).toHaveBeenCalledTimes(1);
    expect(listReclaimable).toHaveBeenCalledWith(report("first"));
    expect(result.current.report?.scanned_at).toBe("first");
    expect(result.current.plan).toEqual(plan);
  });

  it("lets the newest scan win when two are in flight", async () => {
    // A user pressing Scan twice can have two `df()` calls outstanding, and
    // the second is not necessarily the slower one. A stale response must not
    // overwrite a fresher one.
    let resolveFirst: (value: DiskUsageReport) => void = () => {};
    getDockerDiskUsage
      .mockReturnValueOnce(
        new Promise<DiskUsageReport>((r) => {
          resolveFirst = r;
        }),
      )
      .mockResolvedValueOnce(report("second"));

    const { result } = renderHook(() => useDiskUsage());
    let firstScan: Promise<void> = Promise.resolve();
    act(() => {
      firstScan = result.current.scan();
    });
    await act(async () => {
      await result.current.scan();
    });
    expect(result.current.report?.scanned_at).toBe("second");

    // The slow first scan lands afterwards and is discarded.
    await act(async () => {
      resolveFirst(report("first"));
      await firstScan;
    });
    expect(result.current.report?.scanned_at).toBe("second");
    expect(result.current.scanning).toBe(false);
  });

  it("passes the ticked targets straight through", async () => {
    const { result } = renderHook(() => useDiskUsage());
    await act(async () => {
      await result.current.runReclaim([
        { kind: "dangling_snapshots" },
        { kind: "build_cache", all: false },
      ]);
    });
    expect(reclaim).toHaveBeenCalledWith([
      { kind: "dangling_snapshots" },
      { kind: "build_cache", all: false },
    ]);
  });

  it("does not call the backend for an empty selection", async () => {
    const { result } = renderHook(() => useDiskUsage());
    await act(async () => {
      await result.current.runReclaim([]);
    });
    expect(reclaim).not.toHaveBeenCalled();
  });

  it("does not re-scan after a reclaim", async () => {
    // Another `df()` costs seconds, and the outcome already carries measured
    // bytes for every target. A user who wants fresh totals asks for them.
    getDockerDiskUsage.mockResolvedValue(report("first"));
    const { result } = renderHook(() => useDiskUsage());
    await act(async () => {
      await result.current.scan();
    });
    await act(async () => {
      await result.current.runReclaim([{ kind: "dangling_snapshots" }]);
    });
    expect(getDockerDiskUsage).toHaveBeenCalledTimes(1);
  });

  it("clears the previous outcome when a new scan starts", async () => {
    getDockerDiskUsage.mockResolvedValue(report("first"));
    reclaim.mockResolvedValue({ results: [], total_freed_bytes: 42 });
    const { result } = renderHook(() => useDiskUsage());
    await act(async () => {
      await result.current.runReclaim([{ kind: "dangling_snapshots" }]);
    });
    expect(result.current.outcome?.total_freed_bytes).toBe(42);
    await act(async () => {
      await result.current.scan();
    });
    expect(result.current.outcome).toBeNull();
  });

  it("forwards the typed confirmation verbatim", async () => {
    destroyProjectDiskObject.mockResolvedValue({
      target: { kind: "dangling_snapshots" },
      ok: true,
      freed_bytes: 100,
      projected_bytes: null,
      message: "gone",
    });
    const { result } = renderHook(() => useDiskUsage());
    await act(async () => {
      await result.current.destroy({ kind: "config_volume", project_id: "p1" }, "whp");
    });
    expect(destroyProjectDiskObject).toHaveBeenCalledWith(
      { kind: "config_volume", project_id: "p1" },
      "whp",
    );
    expect(result.current.outcome?.total_freed_bytes).toBe(100);
  });

  it("reports a scan failure and keeps the last good measurement", async () => {
    // The old report is still an accurate measurement of an earlier moment,
    // and the error says the refresh failed. Blanking it would leave the panel
    // with nothing while telling the user nothing more.
    getDockerDiskUsage.mockResolvedValueOnce(report("first"));
    const { result } = renderHook(() => useDiskUsage());
    await act(async () => {
      await result.current.scan();
    });

    getDockerDiskUsage.mockRejectedValueOnce("daemon unreachable");
    await act(async () => {
      await result.current.scan();
    });
    await waitFor(() => expect(result.current.error).toMatch(/daemon unreachable/));
    expect(result.current.report?.scanned_at).toBe("first");
    expect(result.current.scanning).toBe(false);
  });

  it("never shows fresh totals beside a stale tick list", async () => {
    // `setReport` used to land before the plan call was awaited, so a plan
    // failure rendered this scan's numbers above the previous scan's rows.
    getDockerDiskUsage.mockResolvedValueOnce(report("first"));
    const { result } = renderHook(() => useDiskUsage());
    await act(async () => {
      await result.current.scan();
    });

    getDockerDiskUsage.mockResolvedValueOnce(report("second"));
    listReclaimable.mockRejectedValueOnce("planner exploded");
    await act(async () => {
      await result.current.scan();
    });
    expect(result.current.error).toMatch(/planner exploded/);
    expect(result.current.report?.scanned_at).toBe("first");
  });

  it("drops the plan after a reclaim so ticks cannot be re-fired at nothing", async () => {
    getDockerDiskUsage.mockResolvedValue(report("first"));
    const { result } = renderHook(() => useDiskUsage());
    await act(async () => {
      await result.current.scan();
    });
    expect(result.current.plan).toEqual(plan);

    await act(async () => {
      await result.current.runReclaim([{ kind: "dangling_snapshots" }]);
    });
    expect(result.current.plan).toBeNull();
    // The totals stay — they were measured before the reclaim and the outcome
    // says what changed.
    expect(result.current.report?.scanned_at).toBe("first");
  });

  it("runs the sweep through its own command and reports what it refused", async () => {
    // The sweep's `in_use` count — orphans Docker refused to delete because a
    // stopped project still needs them — is invisible everywhere else in the
    // app, because every other caller throws the report away.
    sweepOrphanedSnapshots.mockResolvedValue({
      removed: ["sha256:a", "sha256:b"],
      reclaimed_bytes: 11_900_000_000,
      in_use: 3,
      failed: [],
      unavailable: null,
    });
    const { result } = renderHook(() => useDiskUsage());
    await act(async () => {
      await result.current.runSweep();
    });
    expect(sweepOrphanedSnapshots).toHaveBeenCalled();
    expect(result.current.outcome?.total_freed_bytes).toBe(11_900_000_000);
    expect(result.current.outcome?.results[0].message).toMatch(/Swept 2 superseded image/);
    expect(result.current.outcome?.results[0].message).toMatch(/3 were left alone/);
  });

  // -------------------------------------------------------------------------
  // The scan-versus-mutation race
  // -------------------------------------------------------------------------

  it("throws away a scan that a reclaim overtook", async () => {
    // The live race the generation counter used to miss entirely. A scan takes
    // seconds and does not set `working`, so nothing stopped the user
    // reclaiming on top of one — and when the scan landed it repainted the
    // pre-reclaim report *and* a fresh, clickable plan listing objects the
    // reclaim had just deleted.
    let resolveScan: (value: DiskUsageReport) => void = () => {};
    getDockerDiskUsage.mockReturnValueOnce(
      new Promise<DiskUsageReport>((r) => {
        resolveScan = r;
      }),
    );

    const { result } = renderHook(() => useDiskUsage());
    let inFlight: Promise<void> = Promise.resolve();
    act(() => {
      inFlight = result.current.scan();
    });
    expect(result.current.scanning).toBe(true);

    await act(async () => {
      await result.current.runReclaim([{ kind: "dangling_snapshots" }]);
    });
    expect(result.current.plan).toBeNull();

    // The overtaken scan finishes last, and must land nothing at all.
    await act(async () => {
      resolveScan(report("measured before the reclaim"));
      await inFlight;
    });
    expect(result.current.report).toBeNull();
    expect(result.current.plan).toBeNull();
    // It does not even get as far as re-planning: a plan built from a report
    // this stale is the clickable half of the bug.
    expect(listReclaimable).not.toHaveBeenCalled();
  });

  it("does not strand `scanning` when a mutation retires the scan", async () => {
    // `scanning` is cleared against the newest *scan*, not the newest
    // generation — a mutation bumps the generation without starting a scan, so
    // guarding on that would leave the button reading "Scanning…" forever.
    let resolveScan: (value: DiskUsageReport) => void = () => {};
    getDockerDiskUsage.mockReturnValueOnce(
      new Promise<DiskUsageReport>((r) => {
        resolveScan = r;
      }),
    );

    const { result } = renderHook(() => useDiskUsage());
    let inFlight: Promise<void> = Promise.resolve();
    act(() => {
      inFlight = result.current.scan();
    });
    await act(async () => {
      await result.current.runReclaim([{ kind: "dangling_snapshots" }]);
    });
    await act(async () => {
      resolveScan(report("stale"));
      await inFlight;
    });
    expect(result.current.scanning).toBe(false);
  });

  it("retires an in-flight scan for a destroy and a sweep too", async () => {
    // Every mutation invalidates a measurement, not just the bulk one.
    destroyProjectDiskObject.mockResolvedValue({
      target: null,
      destroyed: { kind: "home_volume", project_id: "p1" },
      ok: true,
      freed_bytes: 1,
      projected_bytes: null,
      message: "gone",
    });
    sweepOrphanedSnapshots.mockResolvedValue({
      removed: [],
      reclaimed_bytes: 0,
      in_use: 0,
      failed: [],
      unavailable: null,
    });

    for (const mutate of [
      (r: DiskUsageState) => r.destroy({ kind: "home_volume", project_id: "p1" }, "whp"),
      (r: DiskUsageState) => r.runSweep(),
    ]) {
      let resolveScan: (value: DiskUsageReport) => void = () => {};
      getDockerDiskUsage.mockReturnValueOnce(
        new Promise<DiskUsageReport>((r) => {
          resolveScan = r;
        }),
      );
      const { result } = renderHook(() => useDiskUsage());
      let inFlight: Promise<void> = Promise.resolve();
      act(() => {
        inFlight = result.current.scan();
      });
      await act(async () => {
        await mutate(result.current);
      });
      await act(async () => {
        resolveScan(report("stale"));
        await inFlight;
      });
      expect(result.current.report).toBeNull();
      expect(result.current.plan).toBeNull();
      expect(result.current.scanning).toBe(false);
    }
  });

  // -------------------------------------------------------------------------
  // Reporting failure back to the caller
  // -------------------------------------------------------------------------

  it("tells the caller a reclaim failed instead of only swallowing it into `error`", async () => {
    // The confirmation dialogs close on completion. Without a return value
    // they closed on failure too, leaving the error at the top of a panel the
    // user had scrolled well past.
    reclaim.mockRejectedValueOnce("compaction failed: no space left on device");
    const { result } = renderHook(() => useDiskUsage());
    let ok: boolean | undefined;
    await act(async () => {
      ok = await result.current.runReclaim([{ kind: "compact_snapshot", project_id: "p1" }]);
    });
    expect(ok).toBe(false);
    expect(result.current.error).toMatch(/no space left on device/);
  });

  it("tells the caller a destroy failed", async () => {
    destroyProjectDiskObject.mockRejectedValueOnce("volume is in use by a running container");
    const { result } = renderHook(() => useDiskUsage());
    let ok: boolean | undefined;
    await act(async () => {
      ok = await result.current.destroy({ kind: "home_volume", project_id: "p1" }, "whp");
    });
    expect(ok).toBe(false);
    expect(result.current.error).toMatch(/in use by a running container/);
  });

  it("calls a refusal that came back inside `Ok` a failure, and keeps the plan", async () => {
    // `reclaim` reports per-target results, and a compaction the backend
    // declined is `ok: false` with a sentence saying why — not a thrown error.
    // Treating that as success closed the dialog that asked for it and took
    // the tick list away, even though every object it listed is still there.
    getDockerDiskUsage.mockResolvedValue(report("first"));
    reclaim.mockResolvedValue({
      results: [
        {
          target: { kind: "compact_snapshot", project_id: "p1" },
          destroyed: null,
          ok: false,
          freed_bytes: 0,
          projected_bytes: null,
          message: "Cannot compact p1: a terminal session is still attached.",
        },
      ],
      total_freed_bytes: 0,
    });
    const { result } = renderHook(() => useDiskUsage());
    await act(async () => {
      await result.current.scan();
    });
    let ok: boolean | undefined;
    await act(async () => {
      ok = await result.current.runReclaim([{ kind: "compact_snapshot", project_id: "p1" }]);
    });
    expect(ok).toBe(false);
    expect(result.current.plan).toEqual(plan);
    expect(result.current.outcome?.results[0].message).toMatch(/still attached/);
  });

  it("drops the plan when part of a batch did happen", async () => {
    getDockerDiskUsage.mockResolvedValue(report("first"));
    reclaim.mockResolvedValue({
      results: [
        { target: { kind: "dangling_snapshots" }, destroyed: null, ok: true, freed_bytes: 12, projected_bytes: null, message: "Removed 3 images" },
        { target: { kind: "compact_snapshot", project_id: "p1" }, destroyed: null, ok: false, freed_bytes: 0, projected_bytes: null, message: "Refused" },
      ],
      total_freed_bytes: 12,
    });
    const { result } = renderHook(() => useDiskUsage());
    await act(async () => {
      await result.current.scan();
    });
    let ok: boolean | undefined;
    await act(async () => {
      ok = await result.current.runReclaim([
        { kind: "dangling_snapshots" },
        { kind: "compact_snapshot", project_id: "p1" },
      ]);
    });
    expect(ok).toBe(false);
    expect(result.current.plan).toBeNull();
  });

  it("calls a refused destroy a failure and leaves its row in the plan", async () => {
    getDockerDiskUsage.mockResolvedValue(report("first"));
    destroyProjectDiskObject.mockResolvedValue({
      target: null,
      destroyed: { kind: "home_volume", project_id: "p1" },
      ok: false,
      freed_bytes: 0,
      projected_bytes: null,
      message: "The volume is still attached to a running container.",
    });
    const { result } = renderHook(() => useDiskUsage());
    await act(async () => {
      await result.current.scan();
    });
    let ok: boolean | undefined;
    await act(async () => {
      ok = await result.current.destroy({ kind: "home_volume", project_id: "p1" }, "whp");
    });
    expect(ok).toBe(false);
    expect(result.current.plan).toEqual(plan);
  });

  it("reports success when the call came back", async () => {
    const { result } = renderHook(() => useDiskUsage());
    let ok: boolean | undefined;
    await act(async () => {
      ok = await result.current.runReclaim([{ kind: "dangling_snapshots" }]);
    });
    expect(ok).toBe(true);
  });

  it("treats an unreachable daemon in the sweep report as an error", async () => {
    sweepOrphanedSnapshots.mockResolvedValue({
      removed: [],
      reclaimed_bytes: 0,
      in_use: 0,
      failed: [],
      unavailable: "Could not reach the Docker engine",
    });
    const { result } = renderHook(() => useDiskUsage());
    await act(async () => {
      await result.current.runSweep();
    });
    expect(result.current.error).toMatch(/Could not reach the Docker engine/);
    expect(result.current.outcome).toBeNull();
  });
});
