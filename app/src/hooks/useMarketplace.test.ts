import { describe, it, expect, vi, beforeEach } from "vitest";
import { act, renderHook, waitFor } from "@testing-library/react";
import { useAppState } from "../store/appState";
import type { AppSettings, MarketplaceSnapshot } from "../lib/types";

const listMarketplaceSnapshots = vi.fn();
const refreshMarketplaces = vi.fn();
const listMarketplaceUpdates = vi.fn();
const getSettings = vi.fn();
const listProjects = vi.fn();
const installMarketplaceItem = vi.fn();
const updateMarketplaceItem = vi.fn();

vi.mock("../lib/tauri-commands", () => ({
  listMarketplaceSnapshots: () => listMarketplaceSnapshots(),
  refreshMarketplaces: (id?: string) => refreshMarketplaces(id),
  listMarketplaceUpdates: () => listMarketplaceUpdates(),
  getSettings: () => getSettings(),
  listProjects: () => listProjects(),
  installMarketplaceItem: (...a: unknown[]) => installMarketplaceItem(...a),
  updateMarketplaceItem: (...a: unknown[]) => updateMarketplaceItem(...a),
}));

let syncHandler: ((e: { payload: unknown }) => void) | null = null;
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async (_name: string, cb: (e: { payload: unknown }) => void) => {
    syncHandler = cb;
    return vi.fn();
  }),
}));

import { useMarketplace, useMarketplaceSyncToasts } from "./useMarketplace";

const snap = (id: string, fetched_at: string | null): MarketplaceSnapshot => ({
  marketplace_id: id,
  head_commit: null,
  fetched_at,
  fetch_error: null,
  items: [],
});

describe("useMarketplace", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    useAppState.setState({ toasts: [], appSettings: { marketplaces: [] } as unknown as AppSettings });
    listMarketplaceUpdates.mockResolvedValue([]);
    getSettings.mockResolvedValue({ marketplaces: [] });
    listProjects.mockResolvedValue([]);
  });

  it("loads snapshots and refreshes only stale ones", async () => {
    const fresh = snap("m1", new Date().toISOString());
    const stale = snap("m2", null);
    listMarketplaceSnapshots.mockResolvedValue([fresh, stale]);
    refreshMarketplaces.mockResolvedValue([{ ...stale, fetched_at: new Date().toISOString() }]);

    const { result } = renderHook(() => useMarketplace());
    await act(() => result.current.load({ refreshStale: true }));

    expect(refreshMarketplaces).toHaveBeenCalledTimes(1);
    expect(refreshMarketplaces).toHaveBeenCalledWith("m2");
    expect(result.current.snapshots.map((s) => s.marketplace_id)).toEqual(["m1", "m2"]);
    expect(result.current.snapshots[1].fetched_at).not.toBeNull();
  });

  it("toasts and reloads after a failed mutation", async () => {
    listMarketplaceSnapshots.mockResolvedValue([]);
    installMarketplaceItem.mockRejectedValue("boom");
    const { result } = renderHook(() => useMarketplace());
    const ok = await act(() =>
      result.current.install({ marketplace_id: "m1", kind: "agent", key: "a" }, { type: "global" }, "c".repeat(40)),
    );
    expect(ok).toBe(false);
    expect(useAppState.getState().toasts[0]).toMatchObject({ kind: "error", detail: "boom" });
  });

  it("I2: passes the reviewed commit to install and update", async () => {
    listMarketplaceSnapshots.mockResolvedValue([]);
    installMarketplaceItem.mockResolvedValue({});
    updateMarketplaceItem.mockResolvedValue(undefined);
    const item = { marketplace_id: "m1", kind: "hook" as const, key: "h" };
    const { result } = renderHook(() => useMarketplace());
    await act(() => result.current.install(item, { type: "global" }, "c".repeat(40)));
    expect(installMarketplaceItem).toHaveBeenCalledWith(item, { type: "global" }, "c".repeat(40));
    await act(() => result.current.update(item, { type: "project", project_id: "p1" }, "d".repeat(40)));
    expect(updateMarketplaceItem).toHaveBeenCalledWith(item, { type: "project", project_id: "p1" }, "d".repeat(40));
  });
});

describe("useMarketplaceSyncToasts", () => {
  beforeEach(() => {
    syncHandler = null;
    useAppState.setState({
      toasts: [],
      projects: [{ id: "p1", name: "api" }] as never,
    });
  });

  it("toasts a sync with errors and stays quiet on a clean one", async () => {
    renderHook(() => useMarketplaceSyncToasts());
    await waitFor(() => expect(syncHandler).not.toBeNull());

    act(() =>
      syncHandler!({
        payload: {
          project_id: "p1",
          report: { installed: ["agent:a"], updated: [], removed: [], skipped: [], errors: [], finished_at: "" },
        },
      }),
    );
    expect(useAppState.getState().toasts).toHaveLength(0);

    act(() =>
      syncHandler!({
        payload: {
          project_id: "p1",
          report: {
            installed: [],
            updated: [],
            removed: [],
            skipped: [{ item: "agent:a", reason: "a file you created has the same name" }],
            errors: ["claude plugin install failed"],
            finished_at: "",
          },
        },
      }),
    );
    const toast = useAppState.getState().toasts[0];
    expect(toast.kind).toBe("error");
    expect(toast.message).toContain("api");
    expect(toast.detail).toContain("claude plugin install failed");
    expect(toast.detail).toContain("agent:a");
  });
});
