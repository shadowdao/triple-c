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
    expect(j).toEqual({ text: "hello src/a.ts", firstRow: 0, rowStarts: [0], rowIndents: [0] });
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

describe("joinWrappedRows on a long chain", () => {
  const cols = 40;
  /** 1000 hard-wrapped rows: row 0 at column 0, every later row indented 2. */
  const chainRows = (): Array<[string, boolean]> => {
    const rows: Array<[string, boolean]> = [["/" + "a".repeat(cols - 1), false]];
    for (let i = 1; i < 1000; i++) rows.push(["  " + String(i).padStart(4, "0") + "b".repeat(cols - 6), false]);
    return rows;
  };
  const counting = (rows: Array<[string, boolean]>) => {
    const b = buffer(rows);
    let reads = 0;
    return {
      reads: () => reads,
      source: { getLine: (y: number) => (reads++, b.getLine(y)) } as RowSource,
    };
  };

  it("always includes the hovered row, deep in a chain longer than the budget", () => {
    const rows = chainRows();
    for (const row of [45, 300, 999]) {
      const j = joinWrappedRows(buffer(rows), row, { cols });
      expect(j.firstRow).toBeLessThanOrEqual(row);
      expect(j.firstRow + j.rowStarts.length).toBeGreaterThan(row);
      expect(j.text).toContain(String(row).padStart(4, "0"));
      expect(j.text.length).toBeLessThanOrEqual(MAX_JOINED_LENGTH);
    }
  });

  it("drops a continuation's indent when the window starts mid-chain", () => {
    const j = joinWrappedRows(buffer(chainRows()), 500, { cols });
    expect(j.firstRow).toBeGreaterThan(0);
    expect(j.rowIndents[0]).toBe(2);
    expect(j.text.startsWith("  ")).toBe(false);
  });

  it("reads a bounded number of rows, not the whole chain above", () => {
    const c = counting(chainRows());
    joinWrappedRows(c.source, 999, { cols });
    // The window holds at most MAX_JOINED_LENGTH / (cols - 2) rows; each is
    // read a few times by the walk. Reading all 1000 rows would be far more.
    expect(c.reads()).toBeLessThan(1000);
  });
});

/**
 * Claude Code wraps its own output: a word longer than the line is broken
 * mid-word and the rest is drawn on the next row with cursor moves, so xterm
 * never marks that row `isWrapped`. Captured from Claude Code at 70 columns
 * (bytes: `...9329-e\r\x1b[2C\x1b[1B039e2a...`).
 */
const CLAUDE_ROW_1 = "\u25cf The file is at /tmp/claude-1000/-workspace/5e4ab145-915e-4902-9329-e";
const CLAUDE_ROW_2 = "  039e2a07562/scratchpad/notes.md ok";
const SCRATCH = "/tmp/claude-1000/-workspace/5e4ab145-915e-4902-9329-e039e2a07562/scratchpad/notes.md";

describe("joinWrappedRows across Claude Code's hard wraps", () => {
  it("the captured row really does end at the right edge", () => {
    expect(CLAUDE_ROW_1.length).toBe(70);
  });

  it("joins a path broken mid-word onto the next row, dropping the indent", () => {
    const b = buffer([["before", false], [CLAUDE_ROW_1, false], [CLAUDE_ROW_2, false], ["after", false]]);
    const j = joinWrappedRows(b, 1, { cols: 70 });
    expect(j.text).toBe(`\u25cf The file is at ${SCRATCH} ok`);
    expect(j.firstRow).toBe(1);
    expect(j.rowStarts).toEqual([0, 70]);
    expect(j.rowIndents).toEqual([0, 2]);
    // Asked from the continuation row, the same line comes back.
    expect(joinWrappedRows(b, 2, { cols: 70 })).toEqual(j);
    // The first character of the continuation is drawn at column 3.
    expect(offsetToCell(j, 70)).toEqual({ x: 3, y: 3 });
  });

  it("joins a path broken over three rows", () => {
    const cols = 30;
    const path = "/tmp/" + "a".repeat(60) + "/notes.md"; // 74 chars
    const rows: Array<[string, boolean]> = [
      [path.slice(0, 30), false],
      ["  " + path.slice(30, 58), false],
      ["  " + path.slice(58), false],
    ];
    const j = joinWrappedRows(buffer(rows), 2, { cols });
    expect(j.text).toBe(path);
    expect(j.rowIndents).toEqual([0, 2, 2]);
  });

  it("does not glue an ordinary word at the edge to a short word below it", () => {
    const cols = 40;
    const row1 = "Run the tests and then go and look at it"; // ends at the edge
    expect(row1.length).toBe(cols);
    const b = buffer([[row1, false], ["  src/main.rs for details", false]]);
    expect(joinWrappedRows(b, 0, { cols }).text).toBe(row1);
    expect(joinWrappedRows(b, 1, { cols }).text).toBe("  src/main.rs for details");
  });

  it("does not glue two column-0 lines together (continuations are indented)", () => {
    const cols = 40;
    const row0 = "/var/log/some/very/long/dir/file-aaa.txt";
    expect(row0.length).toBe(cols);
    const b = buffer([[row0, false], ["src/main.rs", false]]);
    expect(joinWrappedRows(b, 0, { cols }).text).toBe(row0);
    expect(joinWrappedRows(b, 1, { cols }).text).toBe("src/main.rs");
  });

  it("does not bridge a row that stops short of the edge", () => {
    const b = buffer([["/tmp/claude-1000/-workspace/abc", false], ["def/notes.md", false]]);
    expect(joinWrappedRows(b, 1, { cols: 70 }).text).toBe("def/notes.md");
  });

  it("does not bridge without knowing the width", () => {
    const b = buffer([[CLAUDE_ROW_1, false], [CLAUDE_ROW_2, false]]);
    expect(joinWrappedRows(b, 0).text).toBe(CLAUDE_ROW_1);
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
