import { describe, it, expect, vi, beforeEach } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";

const load = vi.fn(async () => {});
vi.mock("../../hooks/useMarketplace", () => ({
  useMarketplace: () => ({
    snapshots: [],
    updates: [],
    loading: false,
    refreshing: [],
    load,
    refresh: vi.fn(),
    reloadState: vi.fn(),
    install: vi.fn(),
    uninstall: vi.fn(),
    setDisabled: vi.fn(),
    update: vi.fn(),
    forget: vi.fn(),
    remove: vi.fn(),
  }),
}));
vi.mock("./BrowsePane", () => ({ default: () => <div>browse pane</div> }));
vi.mock("./InstalledPane", () => ({ default: () => <div>installed pane</div> }));
vi.mock("./AccountsPane", () => ({ default: () => <div>accounts pane</div> }));

import MarketplaceView from "./MarketplaceView";

describe("MarketplaceView", () => {
  beforeEach(() => vi.clearAllMocks());

  it("loads with stale refresh when first shown and switches sub-tabs", async () => {
    render(<MarketplaceView active />);
    await waitFor(() => expect(load).toHaveBeenCalledWith({ refreshStale: true }));
    expect(screen.getByText("browse pane")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("tab", { name: "Installed" }));
    expect(screen.getByText("installed pane")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("tab", { name: "Accounts" }));
    expect(screen.getByText("accounts pane")).toBeInTheDocument();
  });

  it("does not load while hidden", () => {
    render(<MarketplaceView active={false} />);
    expect(load).not.toHaveBeenCalled();
  });
});
