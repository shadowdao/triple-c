/**
 * Joins an xterm buffer row with its wrapped continuations.
 *
 * Two kinds: xterm's own soft wraps (`isWrapped`), and — when the caller
 * passes the terminal width — the mid-word breaks Claude Code makes itself,
 * which xterm cannot see (see `hardWrapIndent`).
 *
 * `WebLinksAddon` does the same in its private `LinkComputer`, which the
 * built package does not export — so the walk is repeated here, with the same
 * 2048-character budget. Rows are read with `translateToString(true)`, which
 * trims the right edge; a wrap never ends in trailing spaces xterm would keep,
 * so the join is exact for the text a path can occur in.
 *
 * Wide characters (CJK, emoji) occupy two cells but one string index, so a
 * column computed from a string offset drifts right of the glyph on such rows.
 * The addon corrects this with `getCell`; v1 accepts the drift (underline
 * lands a cell early; the click still resolves the same link).
 */

import { PATH_SEGMENT_CLASS } from "./filePathLinks";

export const MAX_JOINED_LENGTH = 2048;

/** Minimal slice of xterm's IBuffer this needs. */
export interface RowSource {
  getLine(y: number): { isWrapped: boolean; translateToString(trimRight?: boolean): string } | undefined;
}

export interface JoinOptions {
  /**
   * The terminal's width. Given, rows that Claude Code broke mid-word are
   * joined too (see `hardWrapIndent`); omitted, only xterm's soft wraps are.
   */
  cols?: number;
}

export interface JoinedLine {
  text: string;
  /** 0-based index of the first buffer row that contributed. */
  firstRow: number;
  /** For each contributed row (in order), the string offset at which it starts. */
  rowStarts: number[];
  /**
   * For each contributed row, how many leading cells were left out of `text`:
   * the indent of a hard-wrapped continuation, 0 for every other row.
   */
  rowIndents: number[];
}

/** The characters `findFilePathLinks`' token is made of: segment characters and `/`. */
const PATH_CLASS = `[${PATH_SEGMENT_CLASS}/]`;
const PATH_CHAR = new RegExp(PATH_CLASS);
const TRAILING_PATH_RUN = new RegExp(`${PATH_CLASS}+$`);
const LEADING_PATH_RUN = new RegExp(`^${PATH_CLASS}+`);
/** Cells a full row may stop short of the right edge (Ink boxes pad by one or two). */
const EDGE_SLACK = 2;
/** Claude Code draws its transcript (and so every continuation row) at least this far in. */
const MIN_CONTINUATION_INDENT = 2;

/**
 * Is `next` the rest of a word that Claude Code broke at the end of `prev`?
 * Returns the continuation's indent (cells to skip), or `null`.
 *
 * Claude Code wraps its own output (Ink / wrap-ansi) and draws continuation
 * rows with cursor moves, so xterm never marks them `isWrapped` — captured at
 * 70 columns: `…/5e4ab145-915e-4902-9329-e\r\x1b[2C\x1b[1B039e2a…/notes.md`.
 * It breaks *inside* a word only when the word is longer than the line, which
 * is exactly what a long `/tmp/claude-<uid>/…` path is. So bridge only when
 * `prev` runs to (within {@link EDGE_SLACK} of) the right edge and ends in a
 * path character, `next` starts (after an indent of at least
 * {@link MIN_CONTINUATION_INDENT} cells — so two column-0 lines of shell
 * output are never glued) with one, and the two runs
 * together would not have fit on one line — i.e. word wrap could not have
 * moved the word down whole. That last test is what keeps ordinary prose (a
 * word that happens to end at the edge above `src/main.rs`) from being glued.
 */
function hardWrapIndent(prev: string, next: string, cols: number): number | null {
  if (prev.length < cols - EDGE_SLACK || prev.length > cols) return null;
  if (!PATH_CHAR.test(prev[prev.length - 1])) return null;
  const indent = next.search(/\S/);
  if (indent < MIN_CONTINUATION_INDENT || !PATH_CHAR.test(next[indent])) return null;
  const tail = TRAILING_PATH_RUN.exec(prev)?.[0].length ?? 0;
  const head = LEADING_PATH_RUN.exec(next.slice(indent))?.[0].length ?? 0;
  if (tail + head <= cols - indent - EDGE_SLACK) return null;
  return indent;
}

/**
 * Cells to skip if row `y` continues row `y - 1`: 0 for an xterm soft wrap,
 * the indent for a Claude Code hard wrap, `null` if `y` starts a new line.
 */
function continuationIndent(buffer: RowSource, y: number, cols: number | undefined): number | null {
  if (y <= 0) return null;
  const line = buffer.getLine(y);
  if (!line) return null;
  if (line.isWrapped) return 0;
  if (!cols) return null;
  const prev = buffer.getLine(y - 1);
  if (!prev) return null;
  return hardWrapIndent(prev.translateToString(true), line.translateToString(true), cols);
}

/**
 * Joins `row` with the rows it wraps from and into. A chain longer than
 * {@link MAX_JOINED_LENGTH} is windowed around `row`: the walk up stops after
 * half the budget, the walk down after the whole of it, and `row` itself is
 * always included — so a hover deep in a huge wrapped blob reads a bounded
 * number of rows and still finds the text under the pointer.
 */
export function joinWrappedRows(buffer: RowSource, row: number, options: JoinOptions = {}): JoinedLine {
  const { cols } = options;
  let top = row;
  let above = 0;
  while (top > 0 && continuationIndent(buffer, top, cols) !== null) {
    // Every row counts at least 1, so a run of blank soft-wrapped rows is bounded too.
    const len = Math.max(1, buffer.getLine(top - 1)?.translateToString(true).length ?? 0);
    if (above + len > MAX_JOINED_LENGTH / 2) break;
    above += len;
    top--;
  }

  const parts: string[] = [];
  const rowIndents: number[] = [];
  let length = 0;
  let y = top;
  for (;;) {
    const line = buffer.getLine(y);
    if (!line) break;
    let indent = 0;
    const c = continuationIndent(buffer, y, cols);
    if (y !== top) {
      if (c === null) break;
      indent = c;
    } else if (c !== null) {
      indent = c; // the window starts mid-chain: drop this row's indent as well
    }
    const text = line.translateToString(true).slice(indent);
    if (length + text.length > MAX_JOINED_LENGTH && y > row) break;
    parts.push(text);
    rowIndents.push(indent);
    length += text.length;
    y++;
  }

  const rowStarts: number[] = [];
  let offset = 0;
  for (const p of parts) {
    rowStarts.push(offset);
    offset += p.length;
  }
  return { text: parts.join(""), firstRow: top, rowStarts, rowIndents };
}

/** String offset → 1-based {x, y} cell (y is the buffer row + 1). */
export function offsetToCell(
  joined: Pick<JoinedLine, "firstRow" | "rowStarts"> & Partial<Pick<JoinedLine, "rowIndents">>,
  offset: number,
): { x: number; y: number } {
  let rowIdx = 0;
  for (let i = 0; i < joined.rowStarts.length; i++) {
    if (joined.rowStarts[i] <= offset) rowIdx = i;
  }
  const indent = joined.rowIndents?.[rowIdx] ?? 0;
  return { x: offset - joined.rowStarts[rowIdx] + indent + 1, y: joined.firstRow + rowIdx + 1 };
}
