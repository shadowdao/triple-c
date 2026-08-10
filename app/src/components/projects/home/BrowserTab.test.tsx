import { describe, it, expect, vi, beforeEach } from "vitest";
import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import BrowserTab from "./BrowserTab";
import type {
  BrowserSetupOutcome,
  BrowserViewStatus,
  PlaywrightDetection,
  Project,
} from "../../../lib/types";

const getBrowserViewStatus = vi.fn<() => Promise<BrowserViewStatus>>();
const setBrowserViewEnabled = vi.fn<() => Promise<BrowserViewStatus>>();
const checkBrowserViewSupport = vi.fn<() => Promise<PlaywrightDetection>>();
const installBrowserViewSupport = vi.fn<() => Promise<BrowserSetupOutcome>>();
const installBrowserViewBrowser = vi.fn<(id: string, b: string) => Promise<BrowserSetupOutcome>>();
const pushToast = vi.fn();
const setContainerProgress = vi.fn();

vi.mock("../../../lib/tauri-commands", () => ({
  getBrowserViewStatus: () => getBrowserViewStatus(),
  setBrowserViewEnabled: () => setBrowserViewEnabled(),
  checkBrowserViewSupport: () => checkBrowserViewSupport(),
  installBrowserViewSupport: () => installBrowserViewSupport(),
  installBrowserViewBrowser: (id: string, b: string) => installBrowserViewBrowser(id, b),
}));

vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async () => () => {}),
}));

const storeState = {
  pushToast,
  setContainerProgress,
  containerProgress: {} as Record<string, string>,
};

vi.mock("../../../store/appState", () => ({
  useAppState: (selector: (s: unknown) => unknown) => selector(storeState),
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

const NOTHING: PlaywrightDetection = {
  node_version: "22.11.0",
  playwright_version: null,
  playwright_path: null,
  playwright_cli: null,
  has_bind: false,
  cli_version: null,
  cli_entry: null,
  browsers: [],
  chrome_channel: null,
  searched: [
    "/workspace",
    "/usr/lib/node_modules",
    "/home/claude/.npm/_npx/9f3a/node_modules",
  ],
};

const READY: PlaywrightDetection = {
  ...NOTHING,
  playwright_version: "1.62.1",
  playwright_path: "/workspace/node_modules/playwright-core/package.json",
  playwright_cli: "/workspace/node_modules/playwright-core/cli.js",
  has_bind: true,
  cli_version: "0.1.18",
  cli_entry: "/workspace/node_modules/@playwright/cli/playwright-cli.js",
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
  storeState.containerProgress = {};
  getBrowserViewStatus.mockResolvedValue(OFF);
  checkBrowserViewSupport.mockResolvedValue(READY);
});

describe("BrowserTab", () => {
  it("does not offer to start anything while the container is stopped", async () => {
    render(<BrowserTab project={{ ...project, status: "stopped" }} active />);
    expect(await screen.findByText(/container isn’t running/i)).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /start browser view/i })).toBeNull();
    expect(getBrowserViewStatus).not.toHaveBeenCalled();
    expect(checkBrowserViewSupport).not.toHaveBeenCalled();
  });

  it("starts off, and never starts a view or installs anything without being asked", async () => {
    checkBrowserViewSupport.mockResolvedValue({ ...READY, browsers: ["chromium-1200"] });
    render(<BrowserTab project={project} active />);
    await waitFor(() => expect(getBrowserViewStatus).toHaveBeenCalled());
    expect(screen.getByText("Off")).toBeInTheDocument();
    expect(screen.queryByTitle(/browser view for/i)).toBeNull();
    expect(setBrowserViewEnabled).not.toHaveBeenCalled();
    // Probing is read-only and expected; installing is a mutation and is not.
    await waitFor(() => expect(checkBrowserViewSupport).toHaveBeenCalled());
    expect(installBrowserViewSupport).not.toHaveBeenCalled();
    expect(installBrowserViewBrowser).not.toHaveBeenCalled();
  });

  it("shows the live pane, pointed at loopback with a token, once started", async () => {
    checkBrowserViewSupport.mockResolvedValue({ ...READY, browsers: ["chromium-1200"] });
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

  it("offers setup before the user hits a wall, naming what is missing", async () => {
    checkBrowserViewSupport.mockResolvedValue(NOTHING);

    render(<BrowserTab project={project} active />);

    // No Start attempt was needed to learn this.
    expect(await screen.findByRole("button", { name: /set up playwright/i })).toBeInTheDocument();
    expect(screen.getByText(/Missing: playwright, @playwright\/cli/)).toBeInTheDocument();
    // The npx cache is shown among the searched roots — that is where an
    // MCP-installed Playwright actually lives.
    expect(screen.getByText(/_npx\/9f3a\/node_modules/)).toBeInTheDocument();
    // A browser can't be installed before Playwright is.
    expect(screen.getByRole("button", { name: /install chromium/i })).toBeDisabled();
  });

  it("installs Playwright on request and updates itself from the fresh probe", async () => {
    checkBrowserViewSupport.mockResolvedValue(NOTHING);
    installBrowserViewSupport.mockResolvedValue({
      detection: READY,
      log: "added 5 packages in 3s",
      browser_launched: null,
      warning: "Playwright is installed, but this container has no browser to drive yet.",
    });

    render(<BrowserTab project={project} active />);
    const button = await screen.findByRole("button", { name: /set up playwright/i });
    await act(async () => {
      fireEvent.click(button);
    });

    await waitFor(() => expect(installBrowserViewSupport).toHaveBeenCalled());
    // The pane re-rendered from the returned probe — no reopening the tab.
    expect(await screen.findByText("1.62.1")).toBeInTheDocument();
    // Stated in the warning box, and again in the pane's own summary line.
    expect(screen.getAllByText(/no browser to drive yet/).length).toBeGreaterThan(0);
    // And the browser buttons are now live.
    expect(screen.getByRole("button", { name: /install chromium/i })).toBeEnabled();
    expect(screen.getByRole("button", { name: /install chrome channel/i })).toBeEnabled();
    // The progress line is always cleared, whatever happened.
    expect(setContainerProgress).toHaveBeenCalledWith("p1", null);
  });

  it("says which browser is for which caller, and states the size first", async () => {
    checkBrowserViewSupport.mockResolvedValue(READY);
    render(<BrowserTab project={project} active />);

    expect(await screen.findByText(/several hundred mb/i)).toBeInTheDocument();
    // The copy is broken across a <code> element, so match the container.
    expect(
      screen.getByText((_, el) =>
        (el?.textContent ?? "").includes("@playwright/mcp") &&
        (el?.textContent ?? "").includes("asks for") &&
        el?.tagName.toLowerCase() === "li",
      ),
    ).toBeInTheDocument();
    expect(screen.getByText(/roughly 150 mb/i)).toBeInTheDocument();
  });

  it("installs the chrome channel when that is the one asked for", async () => {
    checkBrowserViewSupport.mockResolvedValue(READY);
    installBrowserViewBrowser.mockResolvedValue({
      detection: { ...READY, chrome_channel: "/usr/bin/google-chrome-stable" },
      log: "Installing google-chrome-stable",
      browser_launched: true,
      warning: null,
    });

    render(<BrowserTab project={project} active />);
    const button = await screen.findByRole("button", { name: /install chrome channel/i });
    await act(async () => {
      fireEvent.click(button);
    });

    await waitFor(() =>
      expect(installBrowserViewBrowser).toHaveBeenCalledWith("p1", "chrome"),
    );
    // Shown as the step's "done" line and again in the diagnostics table.
    await waitFor(() =>
      expect(screen.getAllByText(/google-chrome-stable/).length).toBeGreaterThan(0),
    );
  });

  it("reports an install failure with the real command output", async () => {
    checkBrowserViewSupport.mockResolvedValue(NOTHING);
    installBrowserViewSupport.mockRejectedValue(
      "npm couldn't install Playwright in this container (exit 1).\n\nnpm said:\nEACCES: permission denied",
    );

    render(<BrowserTab project={project} active />);
    const button = await screen.findByRole("button", { name: /set up playwright/i });
    await act(async () => {
      fireEvent.click(button);
    });

    expect(await screen.findByText(/EACCES: permission denied/)).toBeInTheDocument();
    expect(pushToast).toHaveBeenCalledWith(
      expect.objectContaining({ kind: "error" }),
    );
    expect(setContainerProgress).toHaveBeenCalledWith("p1", null);
  });

  it("explains precisely what is missing instead of spinning", async () => {
    checkBrowserViewSupport.mockRejectedValue("container busy");
    getBrowserViewStatus.mockResolvedValue({
      ...OFF,
      enabled: true,
      state: "unavailable",
      message:
        "Playwright isn't installed in this container. Two packages are needed: `playwright` and `@playwright/cli`.",
      detection: NOTHING,
    });

    render(<BrowserTab project={project} active />);

    expect(await screen.findByText(/Two packages are needed/)).toBeInTheDocument();
    expect(screen.getByText("Unavailable")).toBeInTheDocument();
    // The probe's findings are shown, so the user can see why.
    expect(screen.getByText("22.11.0")).toBeInTheDocument();
    expect(screen.getByText("not in this build")).toBeInTheDocument();
    expect(screen.getByText(/usr\/lib\/node_modules/)).toBeInTheDocument();
    expect(screen.queryByTitle(/browser view for/i)).toBeNull();
  });

  it("surfaces a start failure rather than leaving the pane blank", async () => {
    checkBrowserViewSupport.mockResolvedValue({ ...READY, browsers: ["chromium-1200"] });
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
    checkBrowserViewSupport.mockResolvedValue({ ...READY, browsers: ["chromium-1200"] });
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
