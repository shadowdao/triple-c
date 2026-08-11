import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { renderHook } from "@testing-library/react";
import { useKeyboardShortcuts } from "./useKeyboardShortcuts";
import { useAppState, homeTabKey, terminalTabKey } from "../store/appState";

vi.mock("./useTerminal", () => ({
  useTerminal: () => ({ open: vi.fn(), close: vi.fn() }),
}));

const HOME = homeTabKey("p1");
const S1 = terminalTabKey("s1");
const S2 = terminalTabKey("s2");

const order = () => useAppState.getState().tabOrder;

/** Press a chord, from whatever is focused. */
function press(key: string, { shift = false } = {}) {
  document.dispatchEvent(
    new KeyboardEvent("keydown", { key, ctrlKey: true, shiftKey: shift, bubbles: true }),
  );
}

/** Focus a real element of the given kind, inside `parent` if given. */
function focus(tag: "input" | "textarea", parentClass?: string): HTMLElement {
  const el = document.createElement(tag);
  if (parentClass) {
    const parent = document.createElement("div");
    parent.className = parentClass;
    parent.appendChild(el);
    document.body.appendChild(parent);
  } else {
    document.body.appendChild(el);
  }
  el.focus();
  return el;
}

beforeEach(() => {
  useAppState.setState({ tabOrder: [HOME, S1, S2], activeTabKey: S1, activeSessionId: "s1" });
});

afterEach(() => {
  document.body.innerHTML = "";
});

describe("Ctrl+Shift+←/→", () => {
  it("moves the active tab along the strip", () => {
    renderHook(() => useKeyboardShortcuts());

    press("ArrowLeft", { shift: true });
    expect(order()).toEqual([S1, HOME, S2]);

    press("ArrowRight", { shift: true });
    expect(order()).toEqual([HOME, S1, S2]);
  });

  it("leaves word-wise selection alone in a text field", () => {
    // Ctrl+Shift+←/→ already means "extend the selection by a word" in every
    // input in the app — the rename field, Config fields, Settings fields.
    // Taking it there would break selection *and* silently reorder tabs.
    renderHook(() => useKeyboardShortcuts());
    focus("input");

    press("ArrowLeft", { shift: true });

    expect(order()).toEqual([HOME, S1, S2]);
  });

  it("still moves tabs from the terminal, whose focus lives in a textarea", () => {
    // xterm keeps a hidden textarea focused as its input-method shim. It is
    // not a field anyone edits word-wise, and the terminal is where these
    // shortcuts matter most, so it is not treated as one.
    renderHook(() => useKeyboardShortcuts());
    focus("textarea", "xterm xterm-helper-textarea-host");

    press("ArrowLeft", { shift: true });

    expect(order()).toEqual([S1, HOME, S2]);
  });

  it("does nothing without Shift — that chord is readline's word motion", () => {
    renderHook(() => useKeyboardShortcuts());

    press("ArrowLeft");

    expect(order()).toEqual([HOME, S1, S2]);
  });
});
