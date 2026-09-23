import { describe, expect, it } from "vitest";
import { EditorState, Text } from "@codemirror/state";
import { highlightExtension, highlightLineField, lineRangeToPositions, setHighlight } from "./highlightLine";

describe("lineRangeToPositions", () => {
  const doc = Text.of(["one", "two", "three"]);
  it("maps 1-based inclusive lines to document offsets", () => {
    expect(lineRangeToPositions(doc, 2, 2)).toEqual({ from: 4, to: 4 });
    expect(lineRangeToPositions(doc, 1, 3)).toEqual({ from: 0, to: 8 });
  });
  it("clamps past the end and refuses nonsense", () => {
    expect(lineRangeToPositions(doc, 2, 99)).toEqual({ from: 4, to: 8 });
    expect(lineRangeToPositions(doc, 99, 100)).toEqual({ from: 8, to: 8 });
    expect(lineRangeToPositions(doc, 0, 1)).toEqual({ from: 0, to: 0 });
    expect(lineRangeToPositions(doc, 3, 1)).toEqual({ from: 8, to: 8 });
  });
});

describe("highlightLineField", () => {
  it("decorates every line in the range and clears on null", () => {
    let state = EditorState.create({ doc: "a\nb\nc\nd", extensions: [highlightExtension()] });
    state = state.update({ effects: setHighlight.of({ from: 2, to: 3 }) }).state;
    let count = 0;
    state.field(highlightLineField).between(0, state.doc.length, () => { count++; });
    expect(count).toBe(2);
    state = state.update({ effects: setHighlight.of(null) }).state;
    count = 0;
    state.field(highlightLineField).between(0, state.doc.length, () => { count++; });
    expect(count).toBe(0);
  });
});
