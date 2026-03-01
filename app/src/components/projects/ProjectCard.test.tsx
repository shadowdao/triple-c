import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, fireEvent, act } from "@testing-library/react";
import ProjectCard from "./ProjectCard";
import type { Project } from "../../lib/types";

// Mock Tauri dialog plugin
vi.mock("@tauri-apps/plugin-dialog", () => ({
  open: vi.fn(),
}));

// Mock hooks
const mockUpdate = vi.fn();
const mockStart = vi.fn();
const mockStop = vi.fn();
const mockRebuild = vi.fn();
const mockRemove = vi.fn();

vi.mock("../../hooks/useProjects", () => ({
  useProjects: () => ({
    start: mockStart,
    stop: mockStop,
    rebuild: mockRebuild,
    remove: mockRemove,
    update: mockUpdate,
  }),
}));

vi.mock("../../hooks/useTerminal", () => ({
  useTerminal: () => ({
    open: vi.fn(),
  }),
}));

let mockSelectedProjectId: string | null = null;
vi.mock("../../store/appState", () => ({
  useAppState: vi.fn((selector) =>
    selector({
      selectedProjectId: mockSelectedProjectId,
      setSelectedProject: vi.fn(),
    })
  ),
}));

const mockProject: Project = {
  id: "test-1",
  name: "Test Project",
  paths: [{ host_path: "/home/user/project", mount_name: "project" }],
  container_id: null,
  status: "stopped",
  auth_mode: "anthropic",
  bedrock_config: null,
  allow_docker_access: false,
  ssh_key_path: null,
  git_token: null,
  git_user_name: null,
  git_user_email: null,
  custom_env_vars: [],
  claude_instructions: null,
  created_at: "2026-01-01T00:00:00Z",
  updated_at: "2026-01-01T00:00:00Z",
};

describe("ProjectCard", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    mockSelectedProjectId = null;
  });

  it("renders project name and path", () => {
    render(<ProjectCard project={mockProject} />);
    expect(screen.getByText("Test Project")).toBeInTheDocument();
    expect(screen.getByText("/workspace/project")).toBeInTheDocument();
  });

  it("card root has min-w-0 and overflow-hidden to contain content", () => {
    const { container } = render(<ProjectCard project={mockProject} />);
    const card = container.firstElementChild;
    expect(card).not.toBeNull();
    expect(card!.className).toContain("min-w-0");
    expect(card!.className).toContain("overflow-hidden");
  });

  describe("when selected and showing config", () => {
    beforeEach(() => {
      mockSelectedProjectId = "test-1";
    });

    it("expanded area has min-w-0 and overflow-hidden", () => {
      const { container } = render(<ProjectCard project={mockProject} />);
      // The expanded section (mt-2 ml-4) contains the auth/action/config controls
      const expandedSection = container.querySelector(".ml-4.mt-2");
      expect(expandedSection).not.toBeNull();
      expect(expandedSection!.className).toContain("min-w-0");
      expect(expandedSection!.className).toContain("overflow-hidden");
    });

    it("folder path inputs use min-w-0 to allow shrinking", async () => {
      const { container } = render(<ProjectCard project={mockProject} />);

      // Click Config button to show config panel
      await act(async () => {
        fireEvent.click(screen.getByText("Config"));
      });

      // After config is shown, check the folder host_path input has min-w-0
      const hostPathInputs = container.querySelectorAll('input[placeholder="/path/to/folder"]');
      expect(hostPathInputs.length).toBeGreaterThan(0);
      expect(hostPathInputs[0].className).toContain("min-w-0");
    });

    it("config panel container has overflow-hidden", async () => {
      const { container } = render(<ProjectCard project={mockProject} />);

      // Click Config button
      await act(async () => {
        fireEvent.click(screen.getByText("Config"));
      });

      // The config panel has border-t and overflow containment classes
      const allDivs = container.querySelectorAll("div");
      const configPanel = Array.from(allDivs).find(
        (div) => div.className.includes("border-t") && div.className.includes("min-w-0")
      );
      expect(configPanel).toBeDefined();
      expect(configPanel!.className).toContain("overflow-hidden");
    });
  });
});
