import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, fireEvent } from "@testing-library/react";
import ProjectRow from "./ProjectRow";
import type { Project } from "../../lib/types";

const mockStart = vi.fn();
const mockStop = vi.fn();
const mockOpenClaudeTerminal = vi.fn();

vi.mock("../../hooks/useProjectActions", () => ({
  useProjectActions: () => ({
    busy: false,
    backingUp: false,
    handleStart: mockStart,
    handleStop: mockStop,
    handleReset: vi.fn(),
    handleBackup: vi.fn(),
    openClaudeTerminal: mockOpenClaudeTerminal,
    openShell: vi.fn(),
    openTerminalWithCommand: vi.fn(),
  }),
}));

const mockOpenProjectHome = vi.fn();
let storeState: Record<string, unknown> = {};

vi.mock("../../store/appState", async () => {
  const actual = await vi.importActual<typeof import("../../store/appState")>(
    "../../store/appState",
  );
  return {
    ...actual,
    useAppState: vi.fn((selector: (s: unknown) => unknown) => selector(storeState)),
  };
});

const baseProject: Project = {
  id: "test-1",
  name: "Test Project",
  paths: [{ host_path: "/home/user/project", mount_name: "project" }],
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

function setStore(overrides: Record<string, unknown> = {}) {
  storeState = {
    activeTabKey: null,
    selectedProjectId: null,
    openProjectHome: mockOpenProjectHome,
    containerProgress: {},
    ...overrides,
  };
}

describe("ProjectRow", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    setStore();
  });

  it("renders project name and mount path", () => {
    render(<ProjectRow project={baseProject} />);
    expect(screen.getByText("Test Project")).toBeInTheDocument();
    expect(screen.getByText("/workspace/project")).toBeInTheDocument();
  });

  it("row root has min-w-0 and overflow-hidden to contain content", () => {
    const { container } = render(<ProjectRow project={baseProject} />);
    const row = container.firstElementChild;
    expect(row).not.toBeNull();
    expect(row!.className).toContain("min-w-0");
    expect(row!.className).toContain("overflow-hidden");
  });

  it("communicates status with a word, not colour alone", () => {
    render(<ProjectRow project={baseProject} />);
    expect(screen.getAllByText("Stopped").length).toBeGreaterThan(0);

    render(<ProjectRow project={{ ...baseProject, status: "error" }} />);
    expect(screen.getAllByText("Error").length).toBeGreaterThan(0);
  });

  it("selecting the row opens that project's home tab instead of expanding in place", () => {
    render(<ProjectRow project={baseProject} />);
    fireEvent.click(screen.getByText("Test Project"));
    expect(mockOpenProjectHome).toHaveBeenCalledWith("test-1");
    // No config form is rendered in the sidebar any more.
    expect(screen.queryByPlaceholderText("/path/to/folder")).toBeNull();
  });

  it("offers start when stopped and stop when running", () => {
    const { unmount } = render(<ProjectRow project={baseProject} />);
    fireEvent.click(screen.getByRole("button", { name: "Start Test Project" }));
    expect(mockStart).toHaveBeenCalled();
    unmount();

    render(<ProjectRow project={{ ...baseProject, status: "running" }} />);
    fireEvent.click(screen.getByRole("button", { name: "Stop Test Project" }));
    expect(mockStop).toHaveBeenCalled();
  });

  it("only allows opening a terminal while the container runs", () => {
    const { unmount } = render(<ProjectRow project={baseProject} />);
    expect(
      screen.getByRole("button", {
        name: "Open a Claude terminal for Test Project",
      }),
    ).toBeDisabled();
    unmount();

    render(<ProjectRow project={{ ...baseProject, status: "running" }} />);
    fireEvent.click(
      screen.getByRole("button", {
        name: "Open a Claude terminal for Test Project",
      }),
    );
    expect(mockOpenClaudeTerminal).toHaveBeenCalled();
  });

  it("shows container progress inline rather than in a blocking modal", () => {
    setStore({ containerProgress: { "test-1": "Pulling image…" } });
    render(<ProjectRow project={{ ...baseProject, status: "starting" }} />);
    expect(screen.getByText("Pulling image…")).toBeInTheDocument();
    expect(screen.queryByRole("dialog")).toBeNull();
  });
});
