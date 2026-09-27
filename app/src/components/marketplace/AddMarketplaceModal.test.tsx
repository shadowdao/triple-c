import { describe, it, expect, vi, beforeEach } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { useAppState } from "../../store/appState";
import type { AppSettings } from "../../lib/types";

const addMarketplace = vi.fn();
vi.mock("../../lib/tauri-commands", () => ({
  addMarketplace: (...a: unknown[]) => addMarketplace(...a),
}));

import AddMarketplaceModal from "./AddMarketplaceModal";

describe("AddMarketplaceModal", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    useAppState.setState({
      appSettings: {
        marketplace_accounts: [{ id: "acc1", label: "Work", host: "github.com", method: "token", username: "me" }],
        marketplaces: [],
        global_marketplace_installs: [],
      } as unknown as AppSettings,
    });
  });

  it("submits name, url, branch and account", async () => {
    const onAdded = vi.fn();
    addMarketplace.mockResolvedValue({ marketplace_id: "m1", head_commit: null, fetched_at: null, fetch_error: null, items: [] });
    render(<AddMarketplaceModal onClose={vi.fn()} onAdded={onAdded} />);
    fireEvent.change(screen.getByLabelText("Name"), { target: { value: "Starter" } });
    fireEvent.change(screen.getByLabelText("Repository URL"), { target: { value: "https://github.com/shadowdao/triple-c-marketplace.git" } });
    fireEvent.change(screen.getByLabelText("Branch"), { target: { value: "" } });
    fireEvent.change(screen.getByLabelText("Account"), { target: { value: "acc1" } });
    fireEvent.click(screen.getByRole("button", { name: "Add marketplace" }));
    await waitFor(() => expect(onAdded).toHaveBeenCalled());
    expect(addMarketplace).toHaveBeenCalledWith("Starter", "https://github.com/shadowdao/triple-c-marketplace.git", null, "acc1");
  });

  it("rejects non-https URLs before calling the backend", () => {
    render(<AddMarketplaceModal onClose={vi.fn()} onAdded={vi.fn()} />);
    fireEvent.change(screen.getByLabelText("Name"), { target: { value: "x" } });
    fireEvent.change(screen.getByLabelText("Repository URL"), { target: { value: "git@github.com:a/b.git" } });
    expect(screen.getByRole("button", { name: "Add marketplace" })).toBeDisabled();
    expect(screen.getByText(/must start with https:\/\//)).toBeInTheDocument();
  });

  it("shows the backend error and stays open", async () => {
    addMarketplace.mockRejectedValue("Work cannot read this repository (HTTP 404)");
    render(<AddMarketplaceModal onClose={vi.fn()} onAdded={vi.fn()} />);
    fireEvent.change(screen.getByLabelText("Name"), { target: { value: "x" } });
    fireEvent.change(screen.getByLabelText("Repository URL"), { target: { value: "https://github.com/a/b.git" } });
    fireEvent.click(screen.getByRole("button", { name: "Add marketplace" }));
    expect(await screen.findByText(/HTTP 404/)).toBeInTheDocument();
  });
});
