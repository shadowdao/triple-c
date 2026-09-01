import { describe, it, expect } from "vitest";
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
