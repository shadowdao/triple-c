import { describe, it, expect, vi, beforeEach } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { useAppState } from "../../store/appState";
import type { AppSettings } from "../../lib/types";
import type { MarketplaceApi } from "../../hooks/useMarketplace";

const testMarketplaceAccount = vi.fn();
const removeMarketplaceAccount = vi.fn();
vi.mock("../../lib/tauri-commands", () => ({
  testMarketplaceAccount: (id: string) => testMarketplaceAccount(id),
  removeMarketplaceAccount: (id: string) => removeMarketplaceAccount(id),
}));
vi.mock("./AddAccountModal", () => ({ default: () => <div>add account modal</div> }));

import AccountsPane from "./AccountsPane";

const settings = {
  marketplace_accounts: [
    { id: "a1", label: "Personal", host: "github.com", method: "gh_host", username: "me" },
    { id: "a2", label: "Gitea", host: "repo.example.com", method: "token", username: "jk" },
  ],
  marketplaces: [{ id: "m1", name: "Team", url: "https://repo.example.com/t/m.git", branch: null, account_id: "a2" }],
  global_marketplace_installs: [],
} as unknown as AppSettings;

describe("AccountsPane", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    useAppState.setState({ appSettings: settings, toasts: [] });
  });

  it("lists accounts with their method and usage", () => {
    render(<AccountsPane mp={{} as MarketplaceApi} />);
    expect(screen.getByText("Personal")).toBeInTheDocument();
    expect(screen.getByText(/gh on this computer/)).toBeInTheDocument();
    expect(screen.getByText(/Used by Team$/)).toBeInTheDocument();
  });

  it("tests an account", async () => {
    testMarketplaceAccount.mockResolvedValue("me");
    render(<AccountsPane mp={{} as MarketplaceApi} />);
    fireEvent.click(screen.getByRole("button", { name: "Test Personal" }));
    await waitFor(() => expect(useAppState.getState().toasts[0]).toMatchObject({ kind: "success" }));
    expect(useAppState.getState().toasts[0].message).toContain("me");
  });

  // F5: the backend refuses to remove an account a marketplace uses, so the
  // UI must not promise otherwise with a confirm modal — Remove is disabled
  // with a hint instead, and there is no confirm step to click through.
  it("disables Remove for an account in use, with a hint", () => {
    render(<AccountsPane mp={{} as MarketplaceApi} />);
    const removeGitea = screen.getByRole("button", { name: "Remove Gitea" });
    expect(removeGitea).toHaveAttribute("aria-disabled", "true");
    expect(screen.getByText(/Used by Team.*change or remove that marketplace first/)).toBeInTheDocument();
    fireEvent.click(removeGitea);
    expect(removeMarketplaceAccount).not.toHaveBeenCalled();
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
  });

  it("removes an unused account", async () => {
    removeMarketplaceAccount.mockResolvedValue({ ...settings, marketplace_accounts: [settings.marketplace_accounts[1]] });
    render(<AccountsPane mp={{} as MarketplaceApi} />);
    const removePersonal = screen.getByRole("button", { name: "Remove Personal" });
    expect(removePersonal).not.toHaveAttribute("aria-disabled");
    fireEvent.click(removePersonal);
    await waitFor(() => expect(removeMarketplaceAccount).toHaveBeenCalledWith("a1"));
    await waitFor(() => expect(useAppState.getState().appSettings!.marketplace_accounts).toHaveLength(1));
  });

  it("opens the add dialog", () => {
    render(<AccountsPane mp={{} as MarketplaceApi} />);
    fireEvent.click(screen.getByRole("button", { name: "Add account" }));
    expect(screen.getByText("add account modal")).toBeInTheDocument();
  });
});
