import { describe, it, expect, afterEach, vi } from "vitest";
import {
  clampDockWidth,
  NOTES_DOCK_MIN_WIDTH,
  NOTES_DOCK_MAX_WIDTH,
  NOTES_DOCK_DEFAULT_WIDTH,
} from "./appState";

describe("clampDockWidth", () => {
  it("keeps a sensible width", () => {
    expect(clampDockWidth(400)).toBe(400);
  });

  it("refuses to squeeze the dock into uselessness", () => {
    expect(clampDockWidth(10)).toBe(NOTES_DOCK_MIN_WIDTH);
  });

  it("refuses to squeeze the terminal into uselessness", () => {
    expect(clampDockWidth(5000)).toBe(NOTES_DOCK_MAX_WIDTH);
  });

  it("falls back for a stored value that is not a number", () => {
    // localStorage holds strings and can carry anything a previous version,
    // a hand edit, or a different screen left behind.
    expect(clampDockWidth(Number("banana"))).toBe(NOTES_DOCK_DEFAULT_WIDTH);
  });

  it("rounds, because a fractional px width blurs the border", () => {
    expect(clampDockWidth(400.6)).toBe(401);
  });
});

// The pure function above is only half the contract: the brief calls out that
// the clamp must guard the *read* path too, because localStorage can carry
// anything a previous version, a hand edit, or a different screen left
// behind. These tests exercise the real store initialization — seeding
// localStorage, then re-importing the module fresh so its top-level
// `loadNotesDockWidth()` call runs against the seeded value — rather than a
// function pulled out just to make this testable. A future refactor that
// dropped the clamp from the load path while keeping it on the write path
// would fail these.
describe("notesDockWidth store initialization", () => {
  const WIDTH_KEY = "triple-c.notes.dock.width";

  afterEach(() => {
    localStorage.removeItem(WIDTH_KEY);
  });

  it("clamps an out-of-range stored value on load", async () => {
    localStorage.setItem(WIDTH_KEY, "99999");
    vi.resetModules();
    const { useAppState } = await import("./appState");
    expect(useAppState.getState().notesDockWidth).toBe(NOTES_DOCK_MAX_WIDTH);
  });

  it("falls back to the default for a non-numeric stored value on load", async () => {
    localStorage.setItem(WIDTH_KEY, "banana");
    vi.resetModules();
    const { useAppState } = await import("./appState");
    expect(useAppState.getState().notesDockWidth).toBe(NOTES_DOCK_DEFAULT_WIDTH);
  });
});
