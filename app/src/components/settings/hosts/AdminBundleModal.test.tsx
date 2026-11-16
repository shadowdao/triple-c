import { describe, it, expect, vi, beforeEach } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import AdminBundleModal from "./AdminBundleModal";

const invoke = vi.fn();
vi.mock("@tauri-apps/api/core", () => ({
  invoke: (cmd: string, args?: unknown) => invoke(cmd, args),
}));

describe("AdminBundleModal", () => {
  beforeEach(() => vi.clearAllMocks());

  it("saves the bundle and shows the folder and the two commands, with Copy", async () => {
    invoke.mockResolvedValue("/home/me/triple-c-onboard-dev");
    const writeText = vi.fn().mockResolvedValue(undefined);
    Object.assign(navigator, { clipboard: { writeText } });
    render(
      <AdminBundleModal connectionId="h1" region="eu-west-1" kind="user_onboard" environment="dev" ownerId="AIDAEXAMPLE" onClose={vi.fn()} />,
    );
    expect(screen.getByRole("dialog", { name: "Onboarding bundle" })).toBeInTheDocument();
    expect(screen.getByText(/environment admin/)).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Save bundle to folder…" }));
    expect(await screen.findByText("/home/me/triple-c-onboard-dev")).toBeInTheDocument();
    expect(invoke).toHaveBeenCalledWith("save_aws_admin_bundle", {
      connectionId: "h1",
      region: "eu-west-1",
      kind: "user_onboard",
      environment: "dev",
      ownerId: "AIDAEXAMPLE",
      network: null,
    });
    const commands = "tofu init -backend-config=backend.hcl\ntofu apply";
    expect(screen.getByText((_, el) => el?.tagName === "PRE" && el.textContent === commands)).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Copy commands" }));
    await waitFor(() => expect(writeText).toHaveBeenCalledWith(commands));
  });

  it("shows nothing new when the folder picker is dismissed", async () => {
    invoke.mockResolvedValue(null);
    render(<AdminBundleModal connectionId="h1" region="eu-west-1" kind="permissions_boundary" onClose={vi.fn()} />);
    fireEvent.click(screen.getByRole("button", { name: "Save bundle to folder…" }));
    await waitFor(() => expect(invoke).toHaveBeenCalled());
    expect(screen.queryByRole("button", { name: "Copy commands" })).toBeNull();
  });

  it("an environment bundle asks for the name and network", async () => {
    invoke.mockResolvedValue("/tmp/env");
    render(<AdminBundleModal connectionId="h1" region="eu-west-1" kind="environment" onClose={vi.fn()} />);
    fireEvent.change(screen.getByLabelText("Name"), { target: { value: "team" } });
    fireEvent.click(screen.getByRole("radio", { name: "Private + NAT" }));
    fireEvent.click(screen.getByRole("button", { name: "Save bundle to folder…" }));
    await waitFor(() =>
      expect(invoke).toHaveBeenCalledWith("save_aws_admin_bundle", {
        connectionId: "h1",
        region: "eu-west-1",
        kind: "environment",
        environment: "team",
        ownerId: null,
        network: { mode: "private-nat" },
        maxWorkspaceHours: 0,
      }),
    );
  });

  it("an environment bundle carries the lifetime cap", async () => {
    invoke.mockResolvedValue("/tmp/env");
    render(<AdminBundleModal connectionId="h1" region="eu-west-1" kind="environment" onClose={vi.fn()} />);
    fireEvent.change(screen.getByLabelText("Maximum workspace lifetime (hours, 0 = none)"), { target: { value: "24" } });
    fireEvent.click(screen.getByRole("button", { name: "Save bundle to folder…" }));
    await waitFor(() =>
      expect(invoke).toHaveBeenCalledWith("save_aws_admin_bundle", expect.objectContaining({ maxWorkspaceHours: 24 })),
    );
  });

  it("shows a save failure through the AWS error copy", async () => {
    invoke.mockRejectedValue({ kind: "sso_expired", profile: "prod-sso" });
    render(<AdminBundleModal connectionId="h1" region="eu-west-1" kind="permissions_boundary" onClose={vi.fn()} />);
    fireEvent.click(screen.getByRole("button", { name: "Save bundle to folder…" }));
    expect(await screen.findByText("Your SSO session for profile prod-sso has expired.")).toBeInTheDocument();
  });
});
