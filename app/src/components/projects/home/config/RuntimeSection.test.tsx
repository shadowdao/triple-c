import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, fireEvent, waitFor } from "@testing-library/react";
import RuntimeSection from "./RuntimeSection";
import type { AuthBridgeStatus, Project } from "../../../../lib/types";

// The auth-bridge row owns its own IPC — see `AuthBridgeRow.tsx` for why it
// does not go through `save`.
const OFF_BRIDGE: AuthBridgeStatus = { enabled: false, active_ports: [], conflicts: [] };
const setAuthBridgeEnabled = vi.fn(async () => ({ ...OFF_BRIDGE, enabled: true }));

vi.mock("../../../../lib/tauri-commands", () => ({
  getAuthBridgeStatus: vi.fn(async () => OFF_BRIDGE),
  setAuthBridgeEnabled: (id: string, on: boolean) => setAuthBridgeEnabled(id, on),
}));

vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async () => () => {}),
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

/**
 * `scope="project"` on the settings editor is one prop with no visible owner,
 * and deleting it fails silently in the worst possible direction: the editor
 * falls back to `"global"`, every three-state control collapses to an on/off
 * switch, and a field the project is *inheriting* as on renders flat Off. The
 * user then reads a lie and, worse, flipping that switch writes a deliberate
 * `false` that overrides the global On they thought they were looking at.
 *
 * Nothing asserted the prop was passed, so these go through what is rendered
 * rather than through props — a switch where a select belongs is exactly the
 * regression, and it is visible from the outside.
 */
describe("RuntimeSection — Claude Code settings are edited at project scope", () => {
  beforeEach(() => vi.clearAllMocks());

  it("gives every setting the third Global state a project can inherit through", () => {
    renderSection();
    const focus = screen.getByLabelText("Focus mode") as HTMLSelectElement;
    expect(
      Array.from(focus.querySelectorAll("option")).map((o) => o.getAttribute("value")),
    ).toEqual(["global", "off", "on"]);
  });

  it("renders an untouched setting as inheriting, not as Off", () => {
    // `claude_code_settings: null` means "this project has no opinion", which
    // is not the same instruction as off. At global scope the same field is a
    // plain unchecked switch — indistinguishable from a user who turned it
    // off, and the reason the missing prop would never be noticed.
    renderSection({ claude_code_settings: null });
    expect((screen.getByLabelText("Focus mode") as HTMLSelectElement).value).toBe(
      "global",
    );
    expect(screen.queryByRole("switch", { name: "Focus mode" })).not.toBeInTheDocument();
  });

  it("keeps a stored project override visible over the inherited value", () => {
    renderSection({
      claude_code_settings: {
        tui_mode: null,
        effort: null,
        auto_scroll_disabled: null,
        focus_mode: true,
        show_thinking_summaries: null,
        session_recap_disabled: null,
        env_scrub: null,
        prompt_caching_1h: null,
      },
    });
    expect((screen.getByLabelText("Focus mode") as HTMLSelectElement).value).toBe("on");
  });
});

describe("RuntimeSection — auth bridge toggle", () => {
  beforeEach(() => vi.clearAllMocks());

  it("is reachable while the container is running", async () => {
    // The rest of the tab is gated on a stopped container because those
    // settings are baked in at creation. This one is host-side and has its own
    // command, and the moment a user needs it is the moment a login is hanging
    // in a *running* container — so the tab's `disabled` must not reach it.
    renderSection({ status: "running" }, true);

    const toggle = screen.getByRole("switch", { name: "Auth bridge" });
    await waitFor(() => expect(toggle).not.toBeDisabled());

    fireEvent.click(toggle);
    await waitFor(() => expect(setAuthBridgeEnabled).toHaveBeenCalledWith("p1", true));
    // And never through the generic project save, which would drop it on the
    // floor while the container runs.
    expect(save).not.toHaveBeenCalled();
  });
});
