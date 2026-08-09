import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, fireEvent } from "@testing-library/react";
import PermissionModeControl, {
  effectivePermissionMode,
  permissionModePatch,
} from "./PermissionModeControl";
import type { Project } from "../../lib/types";

const baseProject: Project = {
  id: "p1",
  name: "api-server",
  paths: [{ host_path: "/src/api", mount_name: "api" }],
  container_id: null,
  status: "running",
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

describe("effectivePermissionMode", () => {
  it("falls back to the legacy boolean when permission_mode is null", () => {
    expect(effectivePermissionMode(baseProject)).toBe("default");
    expect(
      effectivePermissionMode({ ...baseProject, full_permissions: true }),
    ).toBe("bypass");
  });

  it("prefers permission_mode when it is set", () => {
    expect(
      effectivePermissionMode({
        ...baseProject,
        permission_mode: "plan",
        full_permissions: true,
      }),
    ).toBe("plan");
  });
});

describe("permissionModePatch", () => {
  it("keeps the legacy full_permissions flag in sync", () => {
    expect(permissionModePatch("bypass")).toEqual({
      permission_mode: "bypass",
      full_permissions: true,
    });
    expect(permissionModePatch("acceptEdits")).toEqual({
      permission_mode: "acceptEdits",
      full_permissions: false,
    });
  });
});

describe("PermissionModeControl", () => {
  const onChange = vi.fn();

  beforeEach(() => {
    vi.clearAllMocks();
  });

  it("renders all four modes as a radio group with the effective one checked", () => {
    render(<PermissionModeControl project={baseProject} onChange={onChange} />);
    const group = screen.getByRole("radiogroup", { name: "Permission mode" });
    expect(group).toBeInTheDocument();
    expect(screen.getAllByRole("radio")).toHaveLength(4);
    expect(screen.getByRole("radio", { name: "Default" })).toHaveAttribute(
      "aria-checked",
      "true",
    );
  });

  it("reports the picked mode", () => {
    render(<PermissionModeControl project={baseProject} onChange={onChange} />);
    fireEvent.click(screen.getByRole("radio", { name: "Accept Edits" }));
    expect(onChange).toHaveBeenCalledWith("acceptEdits");
  });

  it("moves selection with the arrow keys", () => {
    render(<PermissionModeControl project={baseProject} onChange={onChange} />);
    fireEvent.keyDown(screen.getByRole("radiogroup", { name: "Permission mode" }), {
      key: "ArrowRight",
    });
    expect(onChange).toHaveBeenCalledWith("acceptEdits");
  });

  it("shows sandbox state beside the control", () => {
    render(<PermissionModeControl project={baseProject} onChange={onChange} />);
    expect(screen.getByTestId("sandbox-state")).toHaveTextContent(
      /Sandbox\s*ON/,
    );
  });

  it("does not paint Bypass as dangerous while the sandbox contains it", () => {
    render(
      <PermissionModeControl
        project={{ ...baseProject, permission_mode: "bypass" }}
        onChange={onChange}
      />,
    );
    const bypass = screen.getByRole("radio", { name: "Bypass" });
    expect(bypass.className).toContain("--accent-emphasis");
    expect(bypass.className).not.toContain("--warning-emphasis");
    expect(screen.getByTestId("permission-mode-hint")).toHaveTextContent(
      /contained by the sandbox/i,
    );
  });

  it("uses caution colour only when Bypass runs with the sandbox off", () => {
    render(
      <PermissionModeControl
        project={{
          ...baseProject,
          permission_mode: "bypass",
          sandbox_mode_enabled: false,
        }}
        onChange={onChange}
      />,
    );
    const bypass = screen.getByRole("radio", { name: "Bypass" });
    expect(bypass.className).toContain("--warning-emphasis");
    expect(screen.getByTestId("permission-mode-hint")).toHaveTextContent(
      /Caution/i,
    );
  });
});
