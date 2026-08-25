import { describe, it, expect, beforeEach } from "vitest";
import { useAppState, homeTabKey, terminalTabKey } from "./appState";

const A = homeTabKey("a");
const B = terminalTabKey("b");
const C = terminalTabKey("c");

function seed(tabOrder: string[], activeTabKey: string | null = null) {
  useAppState.setState({
    tabOrder,
    activeTabKey,
    activeSessionId: null,
    selectedProjectId: null,
  });
}

const order = () => useAppState.getState().tabOrder;

describe("tab reordering", () => {
  beforeEach(() => seed([A, B, C]));

  it("moves a tab to an earlier slot", () => {
    useAppState.getState().moveTab(C, 0);
    expect(order()).toEqual([C, A, B]);
  });

  it("moves a tab to a later slot", () => {
    useAppState.getState().moveTab(A, 2);
    expect(order()).toEqual([B, C, A]);
  });

  it("clamps a destination past the ends rather than dropping the tab", () => {
    useAppState.getState().moveTab(A, 99);
    expect(order()).toEqual([B, C, A]);
    useAppState.getState().moveTab(A, -5);
    expect(order()).toEqual([A, B, C]);
  });

  it("ignores a tab that isn't in the strip", () => {
    useAppState.getState().moveTab("term:gone", 0);
    expect(order()).toEqual([A, B, C]);
  });

  it("does not change what's active — dragging a tab is not selecting it", () => {
    seed([A, B, C], B);
    useAppState.getState().moveTab(C, 0);
    const state = useAppState.getState();
    expect(state.tabOrder).toEqual([C, A, B]);
    expect(state.activeTabKey).toBe(B);
  });

  it("nudges the active tab with the keyboard, in both directions", () => {
    seed([A, B, C], B);
    useAppState.getState().moveActiveTab(-1);
    expect(order()).toEqual([B, A, C]);
    useAppState.getState().moveActiveTab(1);
    expect(order()).toEqual([A, B, C]);
  });

  it("stops the active tab at the ends instead of wrapping it around", () => {
    seed([A, B, C], A);
    useAppState.getState().moveActiveTab(-1);
    // A held-down key must not teleport the tab to the far end.
    expect(order()).toEqual([A, B, C]);
  });

  it("does nothing when no tab is active", () => {
    seed([A, B, C], null);
    useAppState.getState().moveActiveTab(1);
    expect(order()).toEqual([A, B, C]);
  });

  it("keeps Ctrl+1..9 addressing the strip as reordered", () => {
    seed([A, B, C], A);
    useAppState.getState().moveTab(C, 0);
    useAppState.getState().focusTabIndex(0);
    expect(useAppState.getState().activeTabKey).toBe(C);
  });
});

describe("toasts", () => {
  beforeEach(() => useAppState.setState({ toasts: [] }));

  const toasts = () => useAppState.getState().toasts;

  it("stacks unkeyed toasts, which are one-off reports", () => {
    const { pushToast } = useAppState.getState();
    pushToast({ kind: "error", message: "one" });
    pushToast({ kind: "error", message: "two" });
    expect(toasts().map((t) => t.message)).toEqual(["one", "two"]);
  });

  it("lets a keyed toast supersede the one already on screen", () => {
    // A recurring notice — "File drop ignored" is the one that exists — must
    // not build a wall of identical cards when the user tries three times.
    const { pushToast } = useAppState.getState();
    const first = pushToast({ kind: "info", message: "ignored", dedupeKey: "drop-blocked" });
    pushToast({ kind: "error", message: "unrelated" });
    const second = pushToast({ kind: "info", message: "ignored", dedupeKey: "drop-blocked" });

    expect(toasts().map((t) => t.message)).toEqual(["unrelated", "ignored"]);
    // A fresh id, so the card re-mounts and its dismissal timer restarts
    // rather than the replacement inheriting the first one's remaining time.
    expect(second).not.toBe(first);
    expect(toasts().some((t) => t.id === first)).toBe(false);
  });

  it("keeps toasts with different keys apart", () => {
    const { pushToast } = useAppState.getState();
    pushToast({ kind: "info", message: "a", dedupeKey: "x" });
    pushToast({ kind: "info", message: "b", dedupeKey: "y" });
    expect(toasts()).toHaveLength(2);
  });
});
