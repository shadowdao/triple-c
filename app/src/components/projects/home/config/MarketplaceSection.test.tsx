import { describe, it, expect, vi, beforeEach } from "vitest";
import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { useAppState, MARKETPLACE_TAB_KEY } from "../../../../store/appState";
import type { AppSettings, Project } from "../../../../lib/types";

const setGlobalItemDisabled = vi.fn();
const getMarketplaceSyncReport = vi.fn();
vi.mock("../../../../lib/tauri-commands", () => ({
  setGlobalItemDisabled: (...a: unknown[]) => setGlobalItemDisabled(...a),
  getMarketplaceSyncReport: (id: string) => getMarketplaceSyncReport(id),
}));

let syncFinishedHandler: ((event: { payload: { project_id: string; report: unknown } }) => void) | null = null;
const listenMock = vi.fn(async (_name: string, cb: (event: { payload: { project_id: string; report: unknown } }) => void) => {
  syncFinishedHandler = cb;
  return vi.fn();
});
vi.mock("@tauri-apps/api/event", () => ({
  listen: (...a: Parameters<typeof listenMock>) => listenMock(...a),
}));

import MarketplaceSection from "./MarketplaceSection";

const A = "a".repeat(40);
const project = {
  id: "p1",
  name: "api",
  status: "running",
  marketplace_installs: [{ marketplace_id: "m1", kind: "command", key: "cmd", commit: A }],
  marketplace_disabled: [{ marketplace_id: "m1", kind: "hook", key: "noisy" }],
} as unknown as Project;

describe("MarketplaceSection", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    syncFinishedHandler = null;
    useAppState.setState({
      tabOrder: [],
      activeTabKey: null,
      projects: [project],
      toasts: [],
      appSettings: {
        marketplaces: [{ id: "m1", name: "Starter", url: "https://x/y.git", branch: null, account_id: null }],
        marketplace_accounts: [],
        global_marketplace_installs: [
          { marketplace_id: "m1", kind: "agent", key: "rev", commit: A },
          { marketplace_id: "m1", kind: "hook", key: "noisy", commit: A },
        ],
      } as unknown as AppSettings,
    });
    getMarketplaceSyncReport.mockResolvedValue({
      installed: ["agent:rev"],
      updated: [],
      removed: [],
      skipped: [{ item: "command:cmd", reason: "a file you created has the same name" }],
      errors: [],
      finished_at: "2026-09-27T12:00:00Z",
    });
  });

  it("shows effective items with their source and the opted-out global item", async () => {
    render(<MarketplaceSection project={project} />);
    const rev = screen.getByTestId("mp-global-agent-rev");
    expect(within(rev).getByRole("switch")).toBeChecked();
    const noisy = screen.getByTestId("mp-global-hook-noisy");
    expect(within(noisy).getByRole("switch")).not.toBeChecked();
    expect(screen.getByTestId("mp-project-command-cmd")).toHaveTextContent("This project only");
    expect(await screen.findByText(/a file you created has the same name/)).toBeInTheDocument();
  });

  it("opts out of a global item", async () => {
    setGlobalItemDisabled.mockResolvedValue({ ...project, marketplace_disabled: [] });
    render(<MarketplaceSection project={project} />);
    fireEvent.click(within(screen.getByTestId("mp-global-agent-rev")).getByRole("switch"));
    await waitFor(() =>
      expect(setGlobalItemDisabled).toHaveBeenCalledWith("p1", { marketplace_id: "m1", kind: "agent", key: "rev" }, true),
    );
  });

  it("opens the Marketplace filtered to this project", async () => {
    render(<MarketplaceSection project={project} />);
    await screen.findByText(/a file you created has the same name/);
    fireEvent.click(screen.getByRole("button", { name: "Open in Marketplace" }));
    expect(useAppState.getState().activeTabKey).toBe(MARKETPLACE_TAB_KEY);
    expect(useAppState.getState().marketplaceFilterProjectId).toBe("p1");
  });

  it("refetches the sync report when marketplace-sync-finished fires for this project", async () => {
    render(<MarketplaceSection project={project} />);
    await screen.findByText(/a file you created has the same name/);
    expect(getMarketplaceSyncReport).toHaveBeenCalledTimes(1);

    getMarketplaceSyncReport.mockResolvedValue({
      installed: [],
      updated: [],
      removed: [],
      skipped: [],
      errors: ["boom"],
      finished_at: "2026-09-27T13:00:00Z",
    });

    expect(syncFinishedHandler).not.toBeNull();
    syncFinishedHandler?.({ payload: { project_id: "p1", report: {} } });

    await waitFor(() => expect(getMarketplaceSyncReport).toHaveBeenCalledTimes(2));
    expect(await screen.findByText("boom")).toBeInTheDocument();
  });

  it("ignores marketplace-sync-finished events for other projects", async () => {
    render(<MarketplaceSection project={project} />);
    await screen.findByText(/a file you created has the same name/);
    expect(getMarketplaceSyncReport).toHaveBeenCalledTimes(1);

    expect(syncFinishedHandler).not.toBeNull();
    syncFinishedHandler?.({ payload: { project_id: "p2", report: {} } });

    await new Promise((r) => setTimeout(r, 0));
    expect(getMarketplaceSyncReport).toHaveBeenCalledTimes(1);
  });
});
