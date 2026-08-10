import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, waitFor } from "@testing-library/react";
import SharedAuthSettings from "./SharedAuthSettings";
import type { Project } from "../../lib/types";

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
  });

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
});
