import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, fireEvent } from "@testing-library/react";
import NotesDock from "./NotesDock";
import type { Project, TerminalSession } from "../../lib/types";

vi.mock("../notes/NotesPanel", () => ({
  default: ({ projectId }: { projectId: string }) => (
    <div data-testid="panel">{`panel:${projectId}`}</div>
  ),
}));

let state: Record<string, unknown> = {};
vi.mock("../../store/appState", () => ({
  useAppState: Object.assign(
    (selector: (s: unknown) => unknown) => selector(state),
    { getState: () => state },
  ),
  isHomeTab: (k: string) => k.startsWith("home:"),
  isTerminalTab: (k: string) => k.startsWith("term:"),
  tabKeyId: (k: string) => k.slice(k.indexOf(":") + 1),
  // The mocked store module still needs to supply the width constants the
  // dock imports from it for the separator's aria-value attributes.
  NOTES_DOCK_MIN_WIDTH: 260,
  NOTES_DOCK_MAX_WIDTH: 720,
}));

const session: TerminalSession = {
  id: "s1",
  projectId: "p9",
  projectName: "api",
  sessionType: "claude",
  sessionName: null,
};

beforeEach(() => {
  state = {
    notesDockOpen: true,
    setNotesDockOpen: vi.fn(),
    toggleNotesDock: vi.fn(),
    notesDockWidth: 352,
    setNotesDockWidth: vi.fn(),
    activeTabKey: null,
    sessions: [session],
    projects: [{ id: "p9", name: "api" } as unknown as Project],
  };
});

describe("NotesDock", () => {
  it("renders nothing when closed", () => {
    state.notesDockOpen = false;
    const { container } = render(<NotesDock />);
    expect(container).toBeEmptyDOMElement();
  });

  it("follows a project home tab", () => {
    state.activeTabKey = "home:p1";
    render(<NotesDock />);
    expect(screen.getByTestId("panel")).toHaveTextContent("panel:p1");
  });

  it("follows the project of the active terminal tab", () => {
    // The dock exists to be visible while the agent runs, so a terminal tab
    // must resolve to its project, not to nothing.
    state.activeTabKey = "term:s1";
    render(<NotesDock />);
    expect(screen.getByTestId("panel")).toHaveTextContent("panel:p9");
  });

  it("explains itself when no project is active", () => {
    state.activeTabKey = null;
    render(<NotesDock />);
    expect(screen.queryByTestId("panel")).not.toBeInTheDocument();
    expect(screen.getByText(/open a project/i)).toBeInTheDocument();
  });

  it("shows nothing for a terminal whose session has gone", () => {
    state.activeTabKey = "term:vanished";
    render(<NotesDock />);
    expect(screen.queryByTestId("panel")).not.toBeInTheDocument();
  });

  it("renders at the stored width", () => {
    state.activeTabKey = "home:p1";
    state.notesDockWidth = 420;
    render(<NotesDock />);
    expect(screen.getByLabelText("Notes")).toHaveStyle({ width: "420px" });
  });

  it("has a keyboard-reachable resize handle", () => {
    // Drag is a mouse gesture; a separator that only responds to pointer
    // events is unusable without one.
    state.activeTabKey = "home:p1";
    render(<NotesDock />);
    const handle = screen.getByRole("separator", { name: /resize notes/i });
    fireEvent.keyDown(handle, { key: "ArrowLeft" });
    expect(state.setNotesDockWidth).toHaveBeenCalled();
  });
});
