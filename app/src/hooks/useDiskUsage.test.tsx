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
  sweepOrphanedSnapshots: vi.fn(),
}));

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

  it("surfaces a failure rather than leaving a stale report on screen", async () => {
    getDockerDiskUsage.mockRejectedValue("daemon unreachable");
    const { result } = renderHook(() => useDiskUsage());
    await act(async () => {
      await result.current.scan();
    });
    await waitFor(() => expect(result.current.error).toMatch(/daemon unreachable/));
    expect(result.current.scanning).toBe(false);
  });
});
