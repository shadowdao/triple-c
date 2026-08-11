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
 * A pointer event carrying a real `clientX`.
 *
 * jsdom implements no `PointerEvent`, so Testing Library's synthesized one has
 * no coordinates — and the coordinate is the whole point here, since it decides
 * which slot the drop lands in. `MouseEvent` has one, and React dispatches on
 * the event's type name either way.
 */
function pointer(el: Element, type: string, clientX: number) {
  fireEvent(el, new MouseEvent(type, { bubbles: true, cancelable: true, clientX, button: 0 }));
}

/** Press, move past the drag threshold, and release over `endX`. */
function dragTab(el: Element, fromX: number, endX: number) {
  pointer(el, "pointerdown", fromX);
  pointer(el, "pointermove", endX);
  pointer(el, "pointerup", endX);
}

/** Pin a tab's geometry so "past the midpoint" means something in jsdom. */
function place(el: Element, left: number, width = 100) {
  el.getBoundingClientRect = () =>
    ({ left, width, right: left + width, top: 0, bottom: 30, height: 30, x: left, y: 0 }) as DOMRect;
}

/** Lay the strip out as three 100px tabs starting at x=0. */
function laidOut() {
  const tabs = screen.getAllByRole("tab");
  tabs.forEach((tab, i) => place(tab, i * 100));
  return tabs;
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
    const tabs = laidOut();

    // Left half of the first tab — the tab lands before it.
    dragTab(tabs[2], 250, 10);

    expect(order()).toEqual([S2, HOME, S1]);
  });

  it("drops after the tab when the pointer is past its midpoint", () => {
    render(<MainTabs />);
    const tabs = laidOut();

    dragTab(tabs[0], 50, 190);

    expect(order()).toEqual([S1, HOME, S2]);
  });

  it("drops at the end when released past the last tab", () => {
    render(<MainTabs />);
    const tabs = laidOut();

    dragTab(tabs[0], 50, 800);

    expect(order()).toEqual([S1, S2, HOME]);
  });

  it("dragging does not steal the selection", () => {
    render(<MainTabs />);
    const tabs = laidOut();

    dragTab(tabs[1], 150, 290);

    expect(order()).toEqual([HOME, S2, S1]);
    expect(useAppState.getState().activeTabKey).toBe(HOME);
  });

  it("does not let a drag select the tab's text", () => {
    // A pointer-driven drag is still a mouse drag as far as the browser is
    // concerned, so without this the label highlights blue while you move it.
    // The rename field is exempt — selecting there is the whole point.
    render(<MainTabs />);
    for (const tab of screen.getAllByRole("tab")) {
      expect(tab.className).toContain("select-none");
    }

    fireEvent.doubleClick(screen.getAllByRole("tab")[1]);
    expect(screen.getByLabelText("Rename tab").className).toContain("select-text");
  });

  it("shows the tab itself under the cursor while dragging", () => {
    // A dimmed source tab and a thin line do not read as "I am holding this
    // tab" — the dragged copy is what makes the gesture legible.
    render(<MainTabs />);
    const tabs = laidOut();
    expect(screen.queryByTestId("tab-drag-ghost")).toBeNull();

    pointer(tabs[2], "pointerdown", 250);
    pointer(tabs[2], "pointermove", 120);

    const ghost = screen.getByTestId("tab-drag-ghost");
    expect(ghost).toHaveTextContent("shell (bash)");
    expect(ghost).toHaveTextContent("▣");

    pointer(tabs[2], "pointerup", 120);
    expect(screen.queryByTestId("tab-drag-ghost")).toBeNull();
  });

  it("carries the project name when a home tab is dragged", () => {
    render(<MainTabs />);
    const tabs = laidOut();

    pointer(tabs[0], "pointerdown", 50);
    pointer(tabs[0], "pointermove", 250);

    expect(screen.getByTestId("tab-drag-ghost")).toHaveTextContent("api-server");
    expect(screen.getByTestId("tab-drag-ghost")).toHaveTextContent("⌂");
  });

  it("drops the dragged copy when the drag is abandoned", () => {
    render(<MainTabs />);
    const tabs = laidOut();

    pointer(tabs[2], "pointerdown", 250);
    pointer(tabs[2], "pointermove", 10);
    fireEvent.keyDown(window, { key: "Escape" });

    expect(screen.queryByTestId("tab-drag-ghost")).toBeNull();
  });

  it("shows the drop marker only while a drag is under way", () => {
    render(<MainTabs />);
    const tabs = laidOut();
    expect(screen.queryByTestId("tab-drop-marker")).toBeNull();

    pointer(tabs[2], "pointerdown", 250);
    pointer(tabs[2], "pointermove", 10);
    expect(screen.getByTestId("tab-drop-marker")).toBeInTheDocument();

    pointer(tabs[2], "pointerup", 10);
    expect(screen.queryByTestId("tab-drop-marker")).toBeNull();
  });

  it("abandons the drag on Escape, leaving the order alone", () => {
    render(<MainTabs />);
    const tabs = laidOut();

    pointer(tabs[2], "pointerdown", 250);
    pointer(tabs[2], "pointermove", 10);
    fireEvent.keyDown(window, { key: "Escape" });

    expect(screen.queryByTestId("tab-drop-marker")).toBeNull();
    pointer(tabs[2], "pointerup", 10);
    expect(order()).toEqual([HOME, S1, S2]);
  });

  it("treats a press that barely moves as a click, not a drag", () => {
    render(<MainTabs />);
    const tabs = laidOut();

    // Two pixels of tremble, under the threshold.
    pointer(tabs[2], "pointerdown", 250);
    pointer(tabs[2], "pointermove", 252);
    pointer(tabs[2], "pointerup", 252);
    fireEvent.click(tabs[2]);

    expect(order()).toEqual([HOME, S1, S2]);
    expect(useAppState.getState().activeTabKey).toBe(S2);
  });

  it("does not select the tab it just dropped", () => {
    render(<MainTabs />);
    const tabs = laidOut();

    dragTab(tabs[2], 250, 10);
    // The browser fires a click after the pointerup that ended the drag.
    fireEvent.click(tabs[2]);

    expect(order()).toEqual([S2, HOME, S1]);
    expect(useAppState.getState().activeTabKey).toBe(HOME);
  });

  it("ignores a press that starts on the close button", () => {
    render(<MainTabs />);
    const tabs = laidOut();
    const close = screen.getByRole("button", { name: "Close shell (bash)" });

    fireEvent(close, new MouseEvent("pointerdown", { bubbles: true, clientX: 290, button: 0 }));
    pointer(tabs[2], "pointermove", 10);

    expect(screen.queryByTestId("tab-drop-marker")).toBeNull();
    expect(order()).toEqual([HOME, S1, S2]);
  });

  it("does not drag a tab that is being renamed — that drag selects text", () => {
    render(<MainTabs />);
    const tabs = laidOut();
    fireEvent.doubleClick(tabs[1]);
    expect(screen.getByLabelText("Rename tab")).toBeInTheDocument();

    dragTab(screen.getAllByRole("tab")[1], 150, 10);

    expect(order()).toEqual([HOME, S1, S2]);
  });

  it("carries no drag payload that another element could receive", () => {
    // An HTML5 drag would put the tab key in a DataTransfer, and releasing over
    // any text field in the app would type `term:…` into it. Pointer events
    // have nothing to hand over, and the tabs are not draggable at all.
    render(<MainTabs />);
    for (const tab of screen.getAllByRole("tab")) {
      expect(tab).not.toHaveAttribute("draggable", "true");
    }
  });
});
