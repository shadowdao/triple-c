import { describe, it, expect, vi, beforeEach } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import SharedAuthSettings from "./SharedAuthSettings";
import { useAppState } from "../../store/appState";
import type { ClearTokenOutcome, Project } from "../../lib/types";

const hasClaudeToken = vi.fn();
const clearClaudeToken = vi.fn();

vi.mock("../../lib/tauri-commands", () => ({
  hasClaudeToken: () => hasClaudeToken(),
  clearClaudeToken: () => clearClaudeToken(),
  acquireClaudeToken: vi.fn(),
  submitClaudeTokenCode: vi.fn(),
}));

vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(async () => vi.fn()) }));
vi.mock("@tauri-apps/plugin-opener", () => ({ openUrl: vi.fn() }));

let projects: Project[] = [];
vi.mock("../../hooks/useProjects", () => ({
  useProjects: () => ({ projects }),
}));

const baseProject: Project = {
  id: "p1",
  name: "api-server",
  paths: [{ host_path: "/src/api", mount_name: "api" }],
  container_id: null,
  status: "stopped",
  backend: "anthropic",
  bedrock_config: null,
  ollama_config: null,
  openai_compatible_config: null,
  allow_docker_access: false,
  sandbox_mode_enabled: true,
  mission_control_enabled: false,
  auth_bridge_enabled: false,
  use_shared_auth_token: true,
  full_permissions: false,
  permission_mode: null,
  ssh_key_path: null,
  git_token: null,
  git_user_name: null,
  git_user_email: null,
  custom_env_vars: [],
  port_mappings: [],
  claude_instructions: null,
  claude_code_settings: null,
  renamed_session_names: {},
  created_at: "2026-01-01T00:00:00Z",
  updated_at: "2026-01-01T00:00:00Z",
};

const running = (over: Partial<Project> = {}): Project => ({
  ...baseProject,
  status: "running",
  container_id: "container-1",
  ...over,
});

describe("SharedAuthSettings", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    projects = [];
    hasClaudeToken.mockResolvedValue(false);
    useAppState.setState({ toasts: [] });
  });

  /** Open the confirmation and go through with it. */
  async function revoke(outcome: Partial<ClearTokenOutcome>) {
    projects = [running()];
    hasClaudeToken.mockResolvedValue(true);
    clearClaudeToken.mockResolvedValue({
      snapshots_scrubbed: [],
      snapshots_failed: [],
      snapshots_superseded: [],
      docker_unavailable: null,
      ...outcome,
    });
    render(<SharedAuthSettings />);
    fireEvent.click(await screen.findByRole("button", { name: "Revoke" }));
    fireEvent.click(await screen.findByRole("button", { name: "Revoke token" }));
    await waitFor(() =>
      expect(useAppState.getState().toasts.length).toBeGreaterThan(0),
    );
    return useAppState.getState().toasts[0];
  }

  it("disables Authenticate and says why when nothing is running", async () => {
    projects = [baseProject];
    render(<SharedAuthSettings />);

    expect(screen.getByRole("button", { name: "Authenticate" })).toBeDisabled();
    expect(screen.getByTestId("shared-auth-no-container")).toHaveTextContent(
      /start a project first/i,
    );
    await waitFor(() => expect(hasClaudeToken).toHaveBeenCalled());
  });

  it("treats a running project with no container id as unusable", async () => {
    projects = [running({ container_id: null })];
    render(<SharedAuthSettings />);
    expect(screen.getByRole("button", { name: "Authenticate" })).toBeDisabled();
    await waitFor(() => expect(hasClaudeToken).toHaveBeenCalled());
  });

  it("enables Authenticate once a container is running", async () => {
    projects = [running()];
    render(<SharedAuthSettings />);

    expect(screen.getByRole("button", { name: "Authenticate" })).toBeEnabled();
    expect(screen.queryByTestId("shared-auth-no-container")).not.toBeInTheDocument();
    await waitFor(() => expect(hasClaudeToken).toHaveBeenCalled());
  });

  it("offers a host picker only when more than one project is running", async () => {
    projects = [running()];
    const { rerender } = render(<SharedAuthSettings />);
    expect(screen.queryByLabelText("Run the sign-in in")).not.toBeInTheDocument();

    projects = [running(), running({ id: "p2", name: "web" })];
    rerender(<SharedAuthSettings />);
    expect(screen.getByLabelText("Run the sign-in in")).toBeInTheDocument();
    await waitFor(() => expect(hasClaudeToken).toHaveBeenCalled());
  });

  it("shows Revoke only when a token is stored", async () => {
    projects = [running()];
    hasClaudeToken.mockResolvedValue(true);
    render(<SharedAuthSettings />);

    await screen.findByRole("button", { name: "Revoke" });
    expect(screen.getByRole("button", { name: "Re-authenticate" })).toBeEnabled();
    expect(screen.getByTestId("shared-auth-detail")).toHaveTextContent(
      /A shared token is stored/,
    );
  });

  it("reports a keychain read failure instead of claiming there is no token", async () => {
    projects = [running()];
    hasClaudeToken.mockRejectedValue("keyring backend unavailable");
    render(<SharedAuthSettings />);

    await screen.findByText("keyring backend unavailable");
    expect(screen.queryByRole("button", { name: "Revoke" })).not.toBeInTheDocument();
  });

  // ── Revoking has to tell the truth ──────────────────────────────────────
  // Deleting the keychain entry is only part of it. `docker commit` copies the
  // token into each project's snapshot image, and an image outlives every
  // container built from it — so a "removed" message while a snapshot still
  // holds a live ~1-year credential is the wrong thing to say.

  it("says so plainly when snapshot images were cleared too", async () => {
    const toast = await revoke({
      snapshots_scrubbed: ["triple-c-snapshot-p1:latest"],
    });
    expect(toast.kind).toBe("success");
    expect(toast.message).toMatch(/1 snapshot image/);
  });

  it("reports an error, not success, when an image still holds the token", async () => {
    const toast = await revoke({
      snapshots_failed: ["triple-c-snapshot-p1:latest: image has child images"],
    });
    expect(toast.kind).toBe("error");
    expect(toast.message).toMatch(/still in some images/i);
    expect(toast.detail).toMatch(/triple-c-snapshot-p1/);
  });

  it("does not claim the images are clean when Docker could not be reached", async () => {
    const toast = await revoke({ docker_unavailable: "Docker is not running" });
    expect(toast.kind).toBe("error");
    expect(toast.detail).toMatch(/Docker could not be reached/);
  });

  it("mentions a retained image layer without calling the revoke a failure", async () => {
    const toast = await revoke({
      snapshots_scrubbed: ["triple-c-snapshot-p1:latest"],
      snapshots_superseded: ["triple-c-snapshot-p1:latest"],
    });
    expect(toast.kind).toBe("success");
    expect(toast.detail).toMatch(/still on disk because a container is running/);
  });

  it("still succeeds plainly when there was nothing to scrub", async () => {
    const toast = await revoke({});
    expect(toast.kind).toBe("success");
    expect(toast.message).toBe("Shared Claude token removed from the keychain.");
  });

  // ── A skipped scrub is not a success, and must stay retryable ────────────
  // `scrub_secrets_from_snapshots` refuses a project another operation holds
  // rather than racing its `:latest` tag. That leaves a live ~1-year token in
  // the image, so it can be neither folded into the success message nor
  // described as a permanent failure whose remedy is Reset.

  it("reports a skipped snapshot as an incomplete revocation, not a success", async () => {
    const toast = await revoke({
      snapshots_skipped: [
        "triple-c-snapshot-p1:latest: This project's container is being started or recreated. Wait for it to finish before removing a credential from its snapshot.",
      ],
    });
    expect(toast.kind).toBe("error");
    expect(toast.message).toMatch(/still in 1 snapshot image/i);
    expect(toast.detail).toMatch(/run the\s+cleanup again/i);
    // The wrong advice for a transient refusal.
    expect(toast.detail).not.toMatch(/Reset/i);
  });

  it("keeps a retry available after the revoke has cleared the keychain", async () => {
    projects = [running()];
    // Stored when the panel mounts, gone after the revoke — which is exactly
    // the state that used to remove the only button able to finish the job.
    hasClaudeToken.mockResolvedValueOnce(true).mockResolvedValue(false);
    clearClaudeToken.mockResolvedValue({
      snapshots_scrubbed: [],
      snapshots_failed: [],
      snapshots_skipped: ["triple-c-snapshot-p1:latest: busy"],
      snapshots_superseded: [],
      docker_unavailable: null,
    });
    render(<SharedAuthSettings />);

    fireEvent.click(await screen.findByRole("button", { name: "Revoke" }));
    fireEvent.click(await screen.findByRole("button", { name: "Revoke token" }));

    const retry = await screen.findByTestId("shared-auth-retry");
    await waitFor(() =>
      expect(screen.queryByRole("button", { name: "Revoke" })).not.toBeInTheDocument(),
    );
    expect(screen.getByTestId("shared-auth-leftover")).toHaveTextContent(
      /still readable/i,
    );
    expect(retry).toBeEnabled();
  });

  it("clears the warning when a retry finally finishes the job", async () => {
    projects = [running()];
    hasClaudeToken.mockResolvedValueOnce(true).mockResolvedValue(false);
    clearClaudeToken.mockResolvedValueOnce({
      snapshots_scrubbed: [],
      snapshots_failed: [],
      snapshots_skipped: ["triple-c-snapshot-p1:latest: busy"],
      snapshots_superseded: [],
      docker_unavailable: null,
    });
    render(<SharedAuthSettings />);
    fireEvent.click(await screen.findByRole("button", { name: "Revoke" }));
    fireEvent.click(await screen.findByRole("button", { name: "Revoke token" }));
    const retry = await screen.findByTestId("shared-auth-retry");

    clearClaudeToken.mockResolvedValueOnce({
      snapshots_scrubbed: ["triple-c-snapshot-p1:latest"],
      snapshots_failed: [],
      snapshots_skipped: [],
      snapshots_superseded: [],
      docker_unavailable: null,
    });
    fireEvent.click(retry);

    await waitFor(() =>
      expect(screen.queryByTestId("shared-auth-leftover")).not.toBeInTheDocument(),
    );
    expect(clearClaudeToken).toHaveBeenCalledTimes(2);
    const toast = useAppState.getState().toasts.at(-1)!;
    expect(toast.kind).toBe("success");
    expect(toast.message).toMatch(/cleared from 1 snapshot image/i);
  });

  it("offers a snapshot sweep even when no token is stored", async () => {
    // Snapshots committed by an older build carry the token whether or not
    // anything is in the keychain today, so the sweep cannot be gated on it.
    projects = [running()];
    hasClaudeToken.mockResolvedValue(false);
    clearClaudeToken.mockResolvedValue({
      snapshots_scrubbed: [],
      snapshots_failed: [],
      snapshots_skipped: [],
      snapshots_superseded: [],
      docker_unavailable: null,
    });
    render(<SharedAuthSettings />);

    const sweep = await screen.findByTestId("shared-auth-sweep");
    expect(screen.queryByRole("button", { name: "Revoke" })).not.toBeInTheDocument();
    fireEvent.click(sweep);

    await waitFor(() => expect(clearClaudeToken).toHaveBeenCalled());
    const toast = useAppState.getState().toasts.at(-1)!;
    expect(toast.kind).toBe("success");
    expect(toast.message).toBe("No snapshot image is holding the token.");
  });

  it("tolerates a backend that does not report skipped snapshots", async () => {
    // `snapshots_skipped` is newer than the rest of the payload; its absence
    // must read as "none", never as undefined reaching the UI.
    const toast = await revoke({ snapshots_scrubbed: ["triple-c-snapshot-p1:latest"] });
    expect(toast.kind).toBe("success");
    expect(screen.queryByTestId("shared-auth-leftover")).not.toBeInTheDocument();
  });
});
