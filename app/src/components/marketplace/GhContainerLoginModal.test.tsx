import { describe, it, expect, vi, beforeEach } from "vitest";
import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";

const startMarketplaceGhContainerLogin = vi.fn();
const cancelMarketplaceGhLogin = vi.fn();
const openUrlExternal = vi.fn();
vi.mock("../../lib/tauri-commands", () => ({
  startMarketplaceGhContainerLogin: (...a: unknown[]) => startMarketplaceGhContainerLogin(...a),
  cancelMarketplaceGhLogin: () => cancelMarketplaceGhLogin(),
  openUrlExternal: (u: string) => openUrlExternal(u),
}));

const handlers = new Map<string, (e: { payload: unknown }) => void>();
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async (name: string, cb: (e: { payload: unknown }) => void) => {
    handlers.set(name, cb);
    return vi.fn();
  }),
}));

import GhContainerLoginModal from "./GhContainerLoginModal";

describe("GhContainerLoginModal", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    handlers.clear();
  });

  it("shows the device code, opens the URL, and finishes", async () => {
    let resolve!: (v: unknown) => void;
    startMarketplaceGhContainerLogin.mockReturnValue(new Promise((r) => (resolve = r)));
    const onDone = vi.fn();
    render(
      <GhContainerLoginModal label="Work" host="github.com" projectId="p1" projectName="api" onClose={vi.fn()} onDone={onDone} />,
    );
    await waitFor(() => expect(handlers.has("marketplace-gh-login-code")).toBe(true));
    await waitFor(() => expect(startMarketplaceGhContainerLogin).toHaveBeenCalledWith("Work", "github.com", "p1"));

    act(() =>
      handlers.get("marketplace-gh-login-code")!({
        payload: { account_id: "unknown-yet", code: "ABCD-1234", url: "https://github.com/login/device" },
      }),
    );
    expect(screen.getByText("ABCD-1234")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Open GitHub" }));
    expect(openUrlExternal).toHaveBeenCalledWith("https://github.com/login/device");

    await act(async () => resolve({ id: "acc9", label: "Work", host: "github.com", method: "gh_container", username: "me" }));
    await waitFor(() => expect(onDone).toHaveBeenCalled());
  });

  it("refuses to open a non-GitHub URL from the container", async () => {
    startMarketplaceGhContainerLogin.mockReturnValue(new Promise(() => {}));
    render(<GhContainerLoginModal label="W" host="github.com" projectId="p1" projectName="api" onClose={vi.fn()} onDone={vi.fn()} />);
    await waitFor(() => expect(handlers.has("marketplace-gh-login-code")).toBe(true));
    act(() =>
      handlers.get("marketplace-gh-login-code")!({
        payload: { account_id: "x", code: "ABCD-1234", url: "https://evil.example/login" },
      }),
    );
    expect(screen.queryByRole("button", { name: "Open GitHub" })).not.toBeInTheDocument();
  });

  it("cancels", async () => {
    startMarketplaceGhContainerLogin.mockReturnValue(new Promise(() => {}));
    const onClose = vi.fn();
    render(<GhContainerLoginModal label="W" host="github.com" projectId="p1" projectName="api" onClose={onClose} onDone={vi.fn()} />);
    fireEvent.click(await screen.findByRole("button", { name: "Cancel sign-in" }));
    expect(cancelMarketplaceGhLogin).toHaveBeenCalled();
    expect(onClose).toHaveBeenCalled();
  });
});
