import { describe, it, expect, vi, beforeEach } from "vitest";
import { fireEvent, render, screen } from "@testing-library/react";
import MainTabs from "./MainTabs";
import { useAppState, homeTabKey, terminalTabKey } from "../../store/appState";
import type { Project, TerminalSession } from "../../lib/types";

const close = vi.fn();

const sessions: TerminalSession[] = [
  {
    id: "s1",
    projectId: "p1",
    projectName: "api-server",
    sessionName: "claude",
    sessionType: "claude",
  },
  {
    id: "s2",
    projectId: "p1",
    projectName: "api-server",
    sessionName: "shell",
    sessionType: "bash",
  },
] as unknown as TerminalSession[];

const projects: Project[] = [
  {
    id: "p1",
    name: "api-server",
    status: "running",
    permission_mode: "bypass",
    renamed_session_names: {},
  },
] as unknown as Project[];

vi.mock("../../hooks/useTerminal", () => ({
  useTerminal: () => ({ sessions, close }),
}));
vi.mock("../../hooks/useProjects", () => ({
  useProjects: () => ({ projects, update: vi.fn() }),
}));

const HOME = homeTabKey("p1");
const S1 = terminalTabKey("s1");
const S2 = terminalTabKey("s2");

/**
 * A stand-in for the DataTransfer jsdom doesn't implement. It only has to
 * carry the tab key, which is what a drop falls back to reading.
 */
function dataTransfer() {
  const store: Record<string, string> = {};
  return {
    effectAllowed: "",
    dropEffect: "",
    setData: (format: string, value: string) => {
      store[format] = value;
    },
    getData: (format: string) => store[format] ?? "",
  };
}

/**
 * A dragover carrying a real `clientX`.
 *
 * jsdom has no `DragEvent`, so Testing Library's synthesized one is a plain
 * `Event` with no pointer coordinates — and the coordinate is the whole point
 * here, since it decides which side of a tab the drop lands on. A `MouseEvent`
 * has one, and React reads it the same way.
 */
function dragOverAt(el: Element, clientX: number, dt: ReturnType<typeof dataTransfer>) {
  const event = new MouseEvent("dragover", { bubbles: true, cancelable: true, clientX });
  Object.defineProperty(event, "dataTransfer", { value: dt });
  fireEvent(el, event);
}

/** Pin a tab's geometry so "past the midpoint" means something in jsdom. */
function place(el: Element, left: number, width = 100) {
  el.getBoundingClientRect = () =>
    ({ left, width, right: left + width, top: 0, bottom: 30, height: 30, x: left, y: 0 }) as DOMRect;
}

const order = () => useAppState.getState().tabOrder;

beforeEach(() => {
  vi.clearAllMocks();
  useAppState.setState({
    tabOrder: [HOME, S1, S2],
    activeTabKey: HOME,
    activeSessionId: null,
    projects,
  });
});

describe("MainTabs reordering", () => {
  it("drags a tab to the front", () => {
    render(<MainTabs />);
    const tabs = screen.getAllByRole("tab");
    tabs.forEach((tab, i) => place(tab, i * 100));

    const dt = dataTransfer();
    fireEvent.dragStart(tabs[2], { dataTransfer: dt });
    // Left half of the first tab — the marker sits before it.
    dragOverAt(tabs[0], 10, dt);
    expect(screen.getByTestId("tab-drop-marker")).toBeInTheDocument();
    fireEvent.drop(tabs[0], { dataTransfer: dt });

    expect(order()).toEqual([S2, HOME, S1]);
  });

  it("drops after the tab when the pointer is past its midpoint", () => {
    render(<MainTabs />);
    const tabs = screen.getAllByRole("tab");
    tabs.forEach((tab, i) => place(tab, i * 100));

    const dt = dataTransfer();
    fireEvent.dragStart(tabs[0], { dataTransfer: dt });
    dragOverAt(tabs[1], 190, dt);
    fireEvent.drop(tabs[1], { dataTransfer: dt });

    expect(order()).toEqual([S1, HOME, S2]);
  });

  it("dragging does not steal the selection", () => {
    render(<MainTabs />);
    const tabs = screen.getAllByRole("tab");
    tabs.forEach((tab, i) => place(tab, i * 100));

    const dt = dataTransfer();
    fireEvent.dragStart(tabs[1], { dataTransfer: dt });
    dragOverAt(tabs[2], 290, dt);
    fireEvent.drop(tabs[2], { dataTransfer: dt });

    expect(order()).toEqual([HOME, S2, S1]);
    expect(useAppState.getState().activeTabKey).toBe(HOME);
  });

  it("shows no drop marker until a drag is under way", () => {
    render(<MainTabs />);
    expect(screen.queryByTestId("tab-drop-marker")).toBeNull();
  });

  it("clears the marker when the drag ends without a drop", () => {
    render(<MainTabs />);
    const tabs = screen.getAllByRole("tab");
    tabs.forEach((tab, i) => place(tab, i * 100));

    const dt = dataTransfer();
    fireEvent.dragStart(tabs[2], { dataTransfer: dt });
    dragOverAt(tabs[0], 10, dt);
    fireEvent.dragEnd(tabs[2], { dataTransfer: dt });

    expect(screen.queryByTestId("tab-drop-marker")).toBeNull();
    expect(order()).toEqual([HOME, S1, S2]);
  });

  it("leaves a tab being renamed undraggable, so its text stays selectable", () => {
    render(<MainTabs />);
    const tabs = screen.getAllByRole("tab");
    expect(tabs[1]).toHaveAttribute("draggable", "true");

    fireEvent.doubleClick(tabs[1]);

    expect(screen.getByLabelText("Rename tab")).toBeInTheDocument();
    expect(screen.getAllByRole("tab")[1]).toHaveAttribute("draggable", "false");
  });
});
