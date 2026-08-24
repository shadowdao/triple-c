import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, fireEvent, act } from "@testing-library/react";
import WorkspaceSection from "./WorkspaceSection";
import type { Project } from "../../../../lib/types";

// The Browse button is the OS folder picker.
const open = vi.fn();
vi.mock("@tauri-apps/plugin-dialog", () => ({
  open: (...args: unknown[]) => open(...args),
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

const save = vi.fn().mockResolvedValue(true);

function renderSection(over: Partial<Project> = {}, disabled = false) {
  return render(
    <WorkspaceSection
      project={{ ...baseProject, ...over }}
      save={save}
      disabled={disabled}
    />,
  );
}

/** Every folder list this component has sent to `update_project`. */
function savedLists() {
  return save.mock.calls
    .filter(([patch]) => "paths" in patch)
    .map(([patch]) => patch.paths);
}

describe("WorkspaceSection — the blank row is never stored", () => {
  beforeEach(() => vi.clearAllMocks());

  /**
   * The bug this file exists for. `create_container` mounts every stored row
   * unfiltered, so a persisted `{host_path: "", mount_name: ""}` becomes
   * `{"Target": "/workspace/", "Source": ""}` and the daemon refuses the whole
   * container with `field Source must not be empty` — the project can never be
   * started or recreated again. Click "+ Add folder", blur a field, and it is
   * bricked.
   */
  it("drops the placeholder row when a real edit is saved", () => {
    renderSection();
    fireEvent.click(screen.getByRole("button", { name: "+ Add folder" }));

    const hostPath = screen.getByLabelText("Folder 1 host path");
    fireEvent.change(hostPath, { target: { value: "/src/api-v2" } });
    fireEvent.blur(hostPath);

    expect(save).toHaveBeenCalledTimes(1);
    expect(savedLists()[0]).toEqual([{ host_path: "/src/api-v2", mount_name: "api" }]);
  });

  it("drops it when Browse fills a different row in", async () => {
    open.mockResolvedValueOnce("/src/api-v2");
    renderSection();
    fireEvent.click(screen.getByRole("button", { name: "+ Add folder" }));

    // The picker is awaited inside the handler, so the state update that
    // follows it lands outside the click.
    await act(async () => {
      fireEvent.click(screen.getAllByRole("button", { name: "Browse" })[0]);
    });

    expect(savedLists()[0]).toEqual([{ host_path: "/src/api-v2", mount_name: "api" }]);
  });

  it("drops it when a row is removed", () => {
    renderSection({
      paths: [
        { host_path: "/src/api", mount_name: "api" },
        { host_path: "/src/web", mount_name: "web" },
      ],
    });
    fireEvent.click(screen.getByRole("button", { name: "+ Add folder" }));
    fireEvent.click(screen.getByRole("button", { name: "Remove folder 2" }));

    expect(savedLists()[0]).toEqual([{ host_path: "/src/api", mount_name: "api" }]);
  });

  it("never sends a row with an empty host path, whatever the route", () => {
    renderSection();
    fireEvent.click(screen.getByRole("button", { name: "+ Add folder" }));
    const hostPath = screen.getByLabelText("Folder 1 host path");
    fireEvent.change(hostPath, { target: { value: "/src/api-v2" } });
    fireEvent.blur(hostPath);

    for (const list of savedLists()) {
      for (const row of list) {
        expect(row.host_path).not.toBe("");
        expect(row.mount_name).not.toBe("");
      }
    }
  });
});

describe("WorkspaceSection — what a blur is allowed to save", () => {
  beforeEach(() => vi.clearAllMocks());

  /**
   * Both inputs save on blur, so tabbing from the host path to the mount name
   * fires a save with the name still empty — which `update_project` refuses,
   * turning an ordinary keystroke into an error toast.
   */
  it("holds a half-filled row back until it is complete", () => {
    renderSection();
    fireEvent.click(screen.getByRole("button", { name: "+ Add folder" }));

    const newHostPath = screen.getByLabelText("Folder 2 host path");
    fireEvent.change(newHostPath, { target: { value: "/src/web" } });
    fireEvent.blur(newHostPath);
    expect(save).not.toHaveBeenCalled();

    const newMountName = screen.getByLabelText("Folder 2 mount name");
    fireEvent.change(newMountName, { target: { value: "web" } });
    fireEvent.blur(newMountName);
    expect(savedLists()[0]).toEqual([
      { host_path: "/src/api", mount_name: "api" },
      { host_path: "/src/web", mount_name: "web" },
    ]);
  });

  /**
   * Blurring out of an untouched field is not an edit. Saving anyway would
   * round-trip the filtered list through `project` and take the empty row away
   * while the user was still filling it in.
   */
  it("saves nothing when the blur changed nothing", () => {
    renderSection();
    fireEvent.click(screen.getByRole("button", { name: "+ Add folder" }));
    fireEvent.blur(screen.getByLabelText("Folder 1 mount name"));
    expect(save).not.toHaveBeenCalled();
    expect(screen.getByLabelText("Folder 2 host path")).toBeTruthy();
  });

  it("still saves a rename, which does not go through the folder list", () => {
    renderSection();
    const name = screen.getByDisplayValue("api-server");
    fireEvent.change(name, { target: { value: "api-v2" } });
    fireEvent.blur(name);
    expect(save).toHaveBeenCalledWith({ name: "api-v2" });
  });
});
