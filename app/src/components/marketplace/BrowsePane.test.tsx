import { describe, it, expect, vi, beforeEach } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { useAppState } from "../../store/appState";
import type { AppSettings, CatalogItem, MarketplaceSnapshot } from "../../lib/types";
import type { MarketplaceApi } from "../../hooks/useMarketplace";

vi.mock("./InstallControls", () => ({
  default: ({ headCommit }: { headCommit: string | null }) => <div>install controls at {headCommit}</div>,
}));
vi.mock("./AddMarketplaceModal", () => ({ default: () => <div>add modal</div> }));
const updateMarketplace = vi.fn();
vi.mock("../../lib/tauri-commands", () => ({
  updateMarketplace: (m: unknown) => updateMarketplace(m),
}));

import BrowsePane from "./BrowsePane";

const it_ = (kind: CatalogItem["kind"], key: string, patch: Partial<CatalogItem> = {}): CatalogItem => ({
  kind,
  key,
  name: key,
  description: `${key} description`,
  path: key,
  invalid: null,
  hook_commands: [],
  plugin_components: [],
  preview: `${key} preview body`,
  ...patch,
});

const snapshot: MarketplaceSnapshot = {
  marketplace_id: "m1",
  head_commit: "a".repeat(40),
  fetched_at: "2026-09-27T12:00:00Z",
  fetch_error: "network unreachable",
  items: [it_("agent", "code-reviewer"), it_("hook", "notify-on-stop"), it_("skill", "broken", { invalid: "SKILL.md missing" })],
};

function api(patch: Partial<MarketplaceApi> = {}): MarketplaceApi {
  return {
    snapshots: [snapshot],
    updates: [],
    loading: false,
    refreshing: [],
    load: vi.fn(),
    refresh: vi.fn(),
    reloadState: vi.fn(),
    install: vi.fn(),
    uninstall: vi.fn(),
    setDisabled: vi.fn(),
    update: vi.fn(),
    forget: vi.fn(),
    remove: vi.fn(async () => true),
    ...patch,
  };
}

describe("BrowsePane", () => {
  beforeEach(() => {
    useAppState.setState({
      appSettings: {
        marketplaces: [{ id: "m1", name: "Starter", url: "https://github.com/s/m.git", branch: null, account_id: null }],
        marketplace_accounts: [],
        global_marketplace_installs: [],
      } as unknown as AppSettings,
      projects: [],
      marketplaceFilterProjectId: null,
    });
  });

  it("lists items, filters by kind and search, and shows detail", () => {
    render(<BrowsePane mp={api()} />);
    expect(screen.getByText("network unreachable")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /code-reviewer/ })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /notify-on-stop/ })).toBeInTheDocument();

    fireEvent.click(screen.getByRole("radio", { name: "Hooks" }));
    expect(screen.queryByRole("button", { name: /code-reviewer/ })).not.toBeInTheDocument();

    fireEvent.click(screen.getByRole("radio", { name: "All" }));
    fireEvent.change(screen.getByLabelText("Search items"), { target: { value: "review" } });
    expect(screen.queryByRole("button", { name: /notify-on-stop/ })).not.toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: /code-reviewer/ }));
    expect(screen.getByText("code-reviewer preview body")).toBeInTheDocument();
    expect(screen.getByText(`install controls at ${"a".repeat(40)}`)).toBeInTheDocument();
  });

  it("I2: installs pin the head the shown item was read at, not a later one", () => {
    const mp = api();
    const { rerender } = render(<BrowsePane mp={mp} />);
    fireEvent.click(screen.getByRole("button", { name: /code-reviewer/ }));
    rerender(<BrowsePane mp={{ ...mp, snapshots: [{ ...snapshot, head_commit: "b".repeat(40) }] }} />);
    expect(screen.getByText(`install controls at ${"a".repeat(40)}`)).toBeInTheDocument();
  });

  it("lets the kind filter wrap instead of running out of its column", () => {
    // All + five kinds are wider than the fixed-width item column.
    render(<BrowsePane mp={api()} />);
    expect(screen.getByRole("radiogroup", { name: "Item kind" })).toHaveClass("flex-wrap");
  });

  it("shows why an item is invalid", () => {
    render(<BrowsePane mp={api()} />);
    fireEvent.click(screen.getByRole("button", { name: /broken/ }));
    expect(screen.getByText("SKILL.md missing")).toBeInTheDocument();
  });

  describe("PR review #7: changing a marketplace's account", () => {
    const withAccount = () =>
      useAppState.setState({
        toasts: [],
        appSettings: {
          marketplaces: [{ id: "m1", name: "Starter", url: "https://github.com/s/m.git", branch: null, account_id: null }],
          marketplace_accounts: [{ id: "a1", label: "Work", host: "gitlab.com", method: "token", username: null }],
          global_marketplace_installs: [],
        } as unknown as AppSettings,
      });

    it("toasts a refused change instead of leaving it unhandled", async () => {
      withAccount();
      updateMarketplace.mockRejectedValueOnce("The account \"Work\" is for gitlab.com, but this marketplace is on github.com.");
      const mp = api();
      render(<BrowsePane mp={mp} />);
      fireEvent.change(screen.getByLabelText("Account for Starter"), { target: { value: "a1" } });
      await waitFor(() => expect(useAppState.getState().toasts).toHaveLength(1));
      const toast = useAppState.getState().toasts[0];
      expect(toast.kind).toBe("error");
      expect(toast.detail).toContain("is for gitlab.com");
      expect(mp.refresh).not.toHaveBeenCalled();
    });

    it("refreshes the marketplace after a successful change", async () => {
      withAccount();
      updateMarketplace.mockResolvedValueOnce({});
      const mp = api();
      render(<BrowsePane mp={mp} />);
      fireEvent.change(screen.getByLabelText("Account for Starter"), { target: { value: "a1" } });
      await waitFor(() => expect(mp.refresh).toHaveBeenCalledWith("m1"));
      expect(updateMarketplace).toHaveBeenCalledWith(expect.objectContaining({ id: "m1", account_id: "a1" }));
      expect(mp.reloadState).toHaveBeenCalled();
      expect(useAppState.getState().toasts).toHaveLength(0);
    });
  });

  it("refreshes one marketplace", () => {
    const mp = api();
    render(<BrowsePane mp={mp} />);
    fireEvent.click(screen.getByRole("button", { name: "Refresh Starter" }));
    expect(mp.refresh).toHaveBeenCalledWith("m1");
  });

  it("offers Add when there are no marketplaces", () => {
    useAppState.setState({
      appSettings: { marketplaces: [], marketplace_accounts: [], global_marketplace_installs: [] } as unknown as AppSettings,
    });
    render(<BrowsePane mp={api({ snapshots: [] })} />);
    fireEvent.click(screen.getByRole("button", { name: "Add marketplace" }));
    expect(screen.getByText("add modal")).toBeInTheDocument();
  });

  it("confirms before removing a marketplace (F6)", () => {
    const mp = api();
    render(<BrowsePane mp={mp} />);
    fireEvent.click(screen.getByRole("button", { name: "Remove Starter" }));
    expect(screen.getByText(/Source removed/)).toBeInTheDocument();
    expect(screen.getByText(/Forget/)).toBeInTheDocument();
    expect(mp.remove).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("button", { name: "Remove marketplace" }));
    expect(mp.remove).toHaveBeenCalledWith("m1");
  });
});
