import { describe, it, expect, vi, beforeEach } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { useAppState } from "../../store/appState";
import type { Project } from "../../lib/types";

const marketplaceGhHostAvailable = vi.fn();
const addMarketplaceGhHostAccount = vi.fn();
const addMarketplaceTokenAccount = vi.fn();
const getSettings = vi.fn();
vi.mock("../../lib/tauri-commands", () => ({
  marketplaceGhHostAvailable: () => marketplaceGhHostAvailable(),
  addMarketplaceGhHostAccount: (...a: unknown[]) => addMarketplaceGhHostAccount(...a),
  addMarketplaceTokenAccount: (...a: unknown[]) => addMarketplaceTokenAccount(...a),
  getSettings: () => getSettings(),
}));
vi.mock("./GhContainerLoginModal", () => ({
  default: ({ projectId }: { projectId: string }) => <div>container login for {projectId}</div>,
}));

import AddAccountModal from "./AddAccountModal";

const running = { id: "p1", name: "api", status: "running", container_id: "c1" } as unknown as Project;

describe("AddAccountModal", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    getSettings.mockResolvedValue({ marketplace_accounts: [] });
    useAppState.setState({ projects: [running], toasts: [] });
  });

  it("uses host gh when available", async () => {
    marketplaceGhHostAvailable.mockResolvedValue(true);
    addMarketplaceGhHostAccount.mockResolvedValue({ id: "a1" });
    const onClose = vi.fn();
    render(<AddAccountModal onClose={onClose} />);
    expect(await screen.findByText(/gh is installed on this computer/)).toBeInTheDocument();
    fireEvent.change(screen.getByLabelText("Label"), { target: { value: "Personal" } });
    fireEvent.click(screen.getByRole("button", { name: "Add account" }));
    await waitFor(() => expect(addMarketplaceGhHostAccount).toHaveBeenCalledWith("Personal", "github.com"));
    await waitFor(() => expect(onClose).toHaveBeenCalled());
  });

  it("falls back to gh in a running container", async () => {
    marketplaceGhHostAvailable.mockResolvedValue(false);
    render(<AddAccountModal onClose={vi.fn()} />);
    expect(await screen.findByLabelText("Run gh in")).toBeInTheDocument();
    fireEvent.change(screen.getByLabelText("Label"), { target: { value: "Work" } });
    fireEvent.click(screen.getByRole("button", { name: "Sign in" }));
    expect(screen.getByText("container login for p1")).toBeInTheDocument();
  });

  it("adds a token account for any host", async () => {
    marketplaceGhHostAvailable.mockResolvedValue(false);
    addMarketplaceTokenAccount.mockResolvedValue({ id: "a2" });
    render(<AddAccountModal onClose={vi.fn()} />);
    fireEvent.click(await screen.findByRole("radio", { name: "Access token" }));
    fireEvent.change(screen.getByLabelText("Label"), { target: { value: "Gitea" } });
    fireEvent.change(screen.getByLabelText("Host"), { target: { value: "repo.anhonesthost.net" } });
    fireEvent.change(screen.getByLabelText("Token"), { target: { value: "test-token-not-real" } });
    fireEvent.click(screen.getByRole("button", { name: "Add account" }));
    await waitFor(() =>
      expect(addMarketplaceTokenAccount).toHaveBeenCalledWith("Gitea", "repo.anhonesthost.net", "test-token-not-real"),
    );
  });

  it("shows a validation error from the backend", async () => {
    marketplaceGhHostAvailable.mockResolvedValue(false);
    addMarketplaceTokenAccount.mockRejectedValue("The token was rejected by repo.anhonesthost.net (HTTP 401)");
    render(<AddAccountModal onClose={vi.fn()} />);
    fireEvent.click(await screen.findByRole("radio", { name: "Access token" }));
    fireEvent.change(screen.getByLabelText("Label"), { target: { value: "G" } });
    fireEvent.change(screen.getByLabelText("Host"), { target: { value: "repo.anhonesthost.net" } });
    fireEvent.change(screen.getByLabelText("Token"), { target: { value: "test-token-not-real" } });
    fireEvent.click(screen.getByRole("button", { name: "Add account" }));
    expect(await screen.findByText(/HTTP 401/)).toBeInTheDocument();
  });
});
