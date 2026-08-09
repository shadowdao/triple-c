import { describe, it, expect, vi, beforeEach } from "vitest";
import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import BrowserTab from "./BrowserTab";
import type { BrowserViewStatus, Project } from "../../../lib/types";

const getBrowserViewStatus = vi.fn<() => Promise<BrowserViewStatus>>();
const setBrowserViewEnabled = vi.fn<() => Promise<BrowserViewStatus>>();
const pushToast = vi.fn();

vi.mock("../../../lib/tauri-commands", () => ({
  getBrowserViewStatus: () => getBrowserViewStatus(),
  setBrowserViewEnabled: () => setBrowserViewEnabled(),
}));

vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async () => () => {}),
}));

vi.mock("../../../store/appState", () => ({
  useAppState: (selector: (s: unknown) => unknown) => selector({ pushToast }),
}));

const OFF: BrowserViewStatus = {
  enabled: false,
  state: "off",
  url: null,
  host_port: null,
  container_port: null,
  started_at: null,
  detection: null,
  message: null,
};

const project: Project = {
  id: "p1",
  name: "api-server",
  paths: [{ host_path: "/home/user/api", mount_name: "api" }],
  container_id: "c1",
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
  permission_mode: "bypass",
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
} as unknown as Project;

beforeEach(() => {
  vi.clearAllMocks();
  getBrowserViewStatus.mockResolvedValue(OFF);
});

describe("BrowserTab", () => {
  it("does not offer to start anything while the container is stopped", async () => {
    render(<BrowserTab project={{ ...project, status: "stopped" }} active />);
    expect(await screen.findByText(/container isn’t running/i)).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /start browser view/i })).toBeNull();
    expect(getBrowserViewStatus).not.toHaveBeenCalled();
  });

  it("starts off, and never starts a view without being asked", async () => {
    render(<BrowserTab project={project} active />);
    await waitFor(() => expect(getBrowserViewStatus).toHaveBeenCalled());
    expect(screen.getByText("Off")).toBeInTheDocument();
    expect(screen.queryByTitle(/browser view for/i)).toBeNull();
    expect(setBrowserViewEnabled).not.toHaveBeenCalled();
  });

  it("shows the live pane, pointed at loopback with a token, once started", async () => {
    setBrowserViewEnabled.mockResolvedValue({
      ...OFF,
      enabled: true,
      state: "running",
      url: "http://127.0.0.1:47820/index.html?ws=abc&token=SEKRIT",
      host_port: 47820,
      container_port: 39321,
      started_at: "2026-08-09T10:00:00Z",
    });

    render(<BrowserTab project={project} active />);
    await waitFor(() => expect(getBrowserViewStatus).toHaveBeenCalled());
    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: /start browser view/i }));
    });

    const frame = await screen.findByTitle("Playwright browser view for api-server");
    expect(frame).toHaveAttribute(
      "src",
      "http://127.0.0.1:47820/index.html?ws=abc&token=SEKRIT",
    );
    expect(screen.getByText("Live")).toBeInTheDocument();
    expect(screen.getByText(/127\.0\.0\.1:47820 → container :39321/)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Stop" })).toBeInTheDocument();
  });

  it("explains precisely what is missing instead of spinning", async () => {
    getBrowserViewStatus.mockResolvedValue({
      ...OFF,
      enabled: true,
      state: "unavailable",
      message:
        "Playwright isn't installed in this container. Install it with `npm i -D playwright`.",
      detection: {
        node_version: "22.11.0",
        playwright_version: null,
        playwright_path: null,
        has_bind: false,
        cli_version: null,
        cli_entry: null,
        searched: ["/workspace", "/usr/lib/node_modules"],
      },
    });

    render(<BrowserTab project={project} active />);

    expect(await screen.findByText(/npm i -D playwright/)).toBeInTheDocument();
    expect(screen.getByText("Unavailable")).toBeInTheDocument();
    // The probe's findings are shown, so the user can see why.
    expect(screen.getByText("22.11.0")).toBeInTheDocument();
    expect(screen.getByText("not in this build")).toBeInTheDocument();
    expect(screen.getByText(/usr\/lib\/node_modules/)).toBeInTheDocument();
    expect(screen.queryByTitle(/browser view for/i)).toBeNull();
  });

  it("surfaces a start failure rather than leaving the pane blank", async () => {
    setBrowserViewEnabled.mockRejectedValue("container went away");

    render(<BrowserTab project={project} active />);
    await waitFor(() => expect(getBrowserViewStatus).toHaveBeenCalled());
    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: /start browser view/i }));
    });

    expect(await screen.findByText(/didn’t start/i)).toBeInTheDocument();
    expect(screen.getByText(/container went away/)).toBeInTheDocument();
    expect(pushToast).toHaveBeenCalledWith(
      expect.objectContaining({ kind: "error" }),
    );
  });

  it("stops the view when asked", async () => {
    getBrowserViewStatus.mockResolvedValue({
      ...OFF,
      enabled: true,
      state: "running",
      url: "http://127.0.0.1:47821/?token=T",
      host_port: 47821,
      container_port: 39321,
    });
    setBrowserViewEnabled.mockResolvedValue(OFF);

    render(<BrowserTab project={project} active />);
    const stop = await screen.findByRole("button", { name: "Stop" });
    await act(async () => {
      fireEvent.click(stop);
    });

    await waitFor(() => expect(setBrowserViewEnabled).toHaveBeenCalled());
    expect(await screen.findByText("Off")).toBeInTheDocument();
    expect(screen.queryByTitle(/browser view for/i)).toBeNull();
  });
});
