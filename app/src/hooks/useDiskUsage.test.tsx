import { describe, it, expect, vi, beforeEach } from "vitest";
import { act, renderHook, waitFor } from "@testing-library/react";
import { useDiskUsage } from "./useDiskUsage";
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
