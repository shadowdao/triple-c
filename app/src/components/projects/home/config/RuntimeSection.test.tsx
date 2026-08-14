import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, fireEvent } from "@testing-library/react";
import RuntimeSection from "./RuntimeSection";
import type { Project } from "../../../../lib/types";

const baseProject: Project = {
  id: "p1",
  name: "api-server",
  paths: [{ host_path: "/src/api", mount_name: "api" }],
  container_id: null,
  status: "stopped",
  backend: "anthropic",
  bedrock_config: null,
  ollama_config: null,
  llamacpp_config: null,
  openai_compatible_config: null,
  allow_docker_access: false,
  sandbox_mode_enabled: true,
  mission_control_enabled: false,
  auth_bridge_enabled: false,
  browser_view_enabled: false,
  vpn_support_enabled: false,
  use_shared_auth_token: true,
  full_permissions: false,
  permission_mode: null,
  ssh_key_path: null,
  ca_cert_path: null,
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

const VPN = "VPN support";

const save = vi.fn().mockResolvedValue(true);

function renderSection(over: Partial<Project> = {}, disabled = false) {
  return render(
    <RuntimeSection
      project={{ ...baseProject, ...over }}
      save={save}
      disabled={disabled}
      disabledReason="Container must be stopped to change this setting."
    />,
  );
}

describe("RuntimeSection — VPN support toggle", () => {
  beforeEach(() => vi.clearAllMocks());

  it("saves only the VPN flag when switched on", () => {
    renderSection();
    fireEvent.click(screen.getByRole("switch", { name: VPN }));
    expect(save).toHaveBeenCalledWith({ vpn_support_enabled: true });
  });

  it("saves the flag off again, rather than dropping the key", () => {
    // Off has to be written explicitly: the container carries a
    // `triple-c.vpn-support` label either way, and an absent value would leave
    // the capability granted.
    renderSection({ vpn_support_enabled: true });
    fireEvent.click(screen.getByRole("switch", { name: VPN }));
    expect(save).toHaveBeenCalledWith({ vpn_support_enabled: false });
  });

  it("reflects the project's current state", () => {
    renderSection({ vpn_support_enabled: true });
    expect(screen.getByRole("switch", { name: VPN })).toBeChecked();
  });

  it("cannot be changed while the container is running", () => {
    // Capabilities and devices are fixed at creation, so this setting is gated
    // on the container being stopped along with the rest of the tab.
    renderSection({}, true);
    const toggle = screen.getByRole("switch", { name: VPN });
    expect(toggle).toBeDisabled();
    fireEvent.click(toggle);
    expect(save).not.toHaveBeenCalled();
  });

  it("warns that the change recreates the container", () => {
    renderSection();
    expect(
      screen.getByText(/recreates the container on its next start/i),
    ).toBeInTheDocument();
  });
});
