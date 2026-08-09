import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, fireEvent } from "@testing-library/react";
import ModelSection from "./ModelSection";
import type { Backend, Project } from "../../../../lib/types";

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

const TOGGLE = "Use the shared Claude token";

const save = vi.fn().mockResolvedValue(true);

function renderSection(over: Partial<Project> = {}, disabled = false) {
  return render(
    <ModelSection
      project={{ ...baseProject, ...over }}
      save={save}
      disabled={disabled}
    />,
  );
}

describe("ModelSection — shared auth token toggle", () => {
  beforeEach(() => vi.clearAllMocks());

  it("renders for the Anthropic backend", () => {
    renderSection();
    expect(screen.getByRole("switch", { name: TOGGLE })).toBeInTheDocument();
  });

  it.each<Backend>(["bedrock", "ollama", "open_ai_compatible"])(
    "is hidden for the %s backend",
    (backend) => {
      renderSection({ backend });
      expect(screen.queryByRole("switch", { name: TOGGLE })).not.toBeInTheDocument();
    },
  );

  it("defaults to on, including for data written before the field existed", () => {
    renderSection();
    expect(screen.getByRole("switch", { name: TOGGLE })).toHaveAttribute(
      "aria-checked",
      "true",
    );

    const legacy = { ...baseProject } as Partial<Project>;
    delete legacy.use_shared_auth_token;
    renderSection(legacy);
    expect(screen.getAllByRole("switch", { name: TOGGLE })[1]).toHaveAttribute(
      "aria-checked",
      "true",
    );
  });

  it("saves the opt-out and explains the consequence", () => {
    renderSection();
    fireEvent.click(screen.getByRole("switch", { name: TOGGLE }));
    expect(save).toHaveBeenCalledWith({ use_shared_auth_token: false });

    renderSection({ use_shared_auth_token: false });
    expect(screen.getByText(/needs its own `claude login`/)).toBeInTheDocument();
  });

  it("follows the container-stopped rule like the rest of the group", () => {
    renderSection({}, true);
    expect(screen.getByRole("switch", { name: TOGGLE })).toBeDisabled();
  });
});
