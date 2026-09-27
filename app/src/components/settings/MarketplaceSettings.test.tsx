import { describe, it, expect, vi, beforeEach } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import MarketplaceSettings from "./MarketplaceSettings";
import { useAppState, MARKETPLACE_TAB_KEY } from "../../store/appState";
import type { AppSettings } from "../../lib/types";

const listMarketplaceUpdates = vi.fn();
vi.mock("../../lib/tauri-commands", () => ({
  listMarketplaceUpdates: () => listMarketplaceUpdates(),
}));

describe("MarketplaceSettings", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    useAppState.setState({
      tabOrder: [],
      activeTabKey: null,
      appSettings: {
        marketplaces: [{ id: "m1", name: "Starter", url: "https://x/y.git", branch: null, account_id: null }],
        global_marketplace_installs: [
          { marketplace_id: "m1", kind: "agent", key: "a", commit: "a".repeat(40) },
          { marketplace_id: "m1", kind: "hook", key: "h", commit: "a".repeat(40) },
        ],
        marketplace_accounts: [],
      } as unknown as AppSettings,
    });
    listMarketplaceUpdates.mockResolvedValue([
      {
        item: { marketplace_id: "m1", kind: "agent", key: "a" },
        pinned: "a".repeat(40),
        head: "b".repeat(40),
        invalid_at_head: null,
      },
      // Not applicable (re-review round 2): not counted as available.
      {
        item: { marketplace_id: "m1", kind: "hook", key: "h" },
        pinned: "a".repeat(40),
        head: "b".repeat(40),
        invalid_at_head: 'unknown hook event "PreFoo"',
      },
    ]);
  });

  it("summarises and opens the Marketplace tab", async () => {
    render(<MarketplaceSettings />);
    expect(screen.getByTestId("marketplace-summary")).toHaveTextContent("1 marketplace");
    expect(screen.getByTestId("marketplace-summary")).toHaveTextContent("2 installed for all projects");
    await waitFor(() =>
      expect(screen.getByTestId("marketplace-summary")).toHaveTextContent("1 update available"),
    );
    fireEvent.click(screen.getByRole("button", { name: "Open Marketplace" }));
    expect(useAppState.getState().activeTabKey).toBe(MARKETPLACE_TAB_KEY);
  });
});
