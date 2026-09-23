import { describe, expect, it } from "vitest";
import { joinWrappedRows, MAX_JOINED_LENGTH, offsetToCell, type RowSource } from "./xtermLineJoin";

/** rows[i] = [text, isWrapped] */
const buffer = (rows: Array<[string, boolean]>): RowSource => ({
  getLine: (y) =>
    rows[y] ? { isWrapped: rows[y][1], translateToString: (trim?: boolean) => (trim ? rows[y][0].trimEnd() : rows[y][0]) } : undefined,
});

describe("joinWrappedRows", () => {
  it("returns a single unwrapped row as-is", () => {
    const j = joinWrappedRows(buffer([["hello src/a.ts", false]]), 0);
    expect(j).toEqual({ text: "hello src/a.ts", firstRow: 0, rowStarts: [0] });
  });

  it("walks up to the row that started the wrap and down through continuations", () => {
    const b = buffer([
      ["unrelated", false],
      ["/workspace/very/long/pa", false],
      ["th/to/file.ts:12 and mo", true],
      ["re text", true],
      ["next line", false],
    ]);
    const fromMiddle = joinWrappedRows(b, 2);
    expect(fromMiddle.text).toBe("/workspace/very/long/path/to/file.ts:12 and more text");
    expect(fromMiddle.firstRow).toBe(1);
    expect(fromMiddle.rowStarts).toEqual([0, 23, 46]);
    expect(joinWrappedRows(b, 1)).toEqual(fromMiddle);
    expect(joinWrappedRows(b, 3)).toEqual(fromMiddle);
  });

  it("stops at the length budget", () => {
    const rows: Array<[string, boolean]> = [["a".repeat(1000), false]];
    for (let i = 0; i < 5; i++) rows.push(["b".repeat(1000), true]);
    const j = joinWrappedRows(buffer(rows), 0);
    expect(j.text.length).toBeLessThanOrEqual(MAX_JOINED_LENGTH);
    expect(j.rowStarts.length).toBe(2);
  });
});

describe("offsetToCell", () => {
  it("maps offsets to 1-based cells on the right row", () => {
    const j = { text: "abcdefgh", firstRow: 4, rowStarts: [0, 3, 6] };
    expect(offsetToCell(j, 0)).toEqual({ x: 1, y: 5 });
    expect(offsetToCell(j, 2)).toEqual({ x: 3, y: 5 });
    expect(offsetToCell(j, 3)).toEqual({ x: 1, y: 6 });
    expect(offsetToCell(j, 7)).toEqual({ x: 2, y: 7 });
  });
});
