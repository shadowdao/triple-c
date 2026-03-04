import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen } from "@testing-library/react";
import Sidebar from "./Sidebar";

// Mock zustand store
vi.mock("../../store/appState", () => ({
  useAppState: vi.fn((selector) =>
    selector({
      sidebarView: "projects",
      setSidebarView: vi.fn(),
    })
  ),
}));

// Mock child components to isolate Sidebar layout testing
vi.mock("../projects/ProjectList", () => ({
  default: () => <div data-testid="project-list">ProjectList</div>,
}));
vi.mock("../settings/SettingsPanel", () => ({
  default: () => <div data-testid="settings-panel">SettingsPanel</div>,
}));
vi.mock("../mcp/McpPanel", () => ({
  default: () => <div data-testid="mcp-panel">McpPanel</div>,
}));

describe("Sidebar", () => {
  beforeEach(() => {
    vi.clearAllMocks();
  });

  it("renders the sidebar with content area", () => {
    render(<Sidebar />);
    expect(screen.getByText("Projects")).toBeInTheDocument();
    expect(screen.getByText("Settings")).toBeInTheDocument();
  });

  it("content area has min-w-0 to prevent flex overflow", () => {
    const { container } = render(<Sidebar />);
    const contentArea = container.querySelector(".overflow-y-auto");
    expect(contentArea).not.toBeNull();
    expect(contentArea!.className).toContain("min-w-0");
  });

  it("content area has overflow-x-hidden to prevent horizontal scroll", () => {
    const { container } = render(<Sidebar />);
    const contentArea = container.querySelector(".overflow-y-auto");
    expect(contentArea).not.toBeNull();
    expect(contentArea!.className).toContain("overflow-x-hidden");
  });

  it("sidebar outer container has overflow-hidden", () => {
    const { container } = render(<Sidebar />);
    const sidebar = container.firstElementChild;
    expect(sidebar).not.toBeNull();
    expect(sidebar!.className).toContain("overflow-hidden");
  });
});
