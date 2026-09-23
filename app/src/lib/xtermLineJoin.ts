/**
 * Joins an xterm buffer row with its wrapped continuations.
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

export const MAX_JOINED_LENGTH = 2048;

/** Minimal slice of xterm's IBuffer this needs. */
export interface RowSource {
  getLine(y: number): { isWrapped: boolean; translateToString(trimRight?: boolean): string } | undefined;
}

export interface JoinedLine {
  text: string;
  /** 0-based index of the first buffer row that contributed. */
  firstRow: number;
  /** For each contributed row (in order), the string offset at which it starts. */
  rowStarts: number[];
}

export function joinWrappedRows(buffer: RowSource, row: number): JoinedLine {
  let top = row;
  while (top > 0 && buffer.getLine(top)?.isWrapped) top--;

  const parts: string[] = [];
  let length = 0;
  let y = top;
  for (;;) {
    const line = buffer.getLine(y);
    if (!line) break;
    if (y !== top && !line.isWrapped) break;
    const text = line.translateToString(true);
    if (length + text.length > MAX_JOINED_LENGTH && parts.length > 0) break;
    parts.push(text);
    length += text.length;
    y++;
  }

  const rowStarts: number[] = [];
  let offset = 0;
  for (const p of parts) {
    rowStarts.push(offset);
    offset += p.length;
  }
  return { text: parts.join(""), firstRow: top, rowStarts };
}

/** String offset → 1-based {x, y} cell (y is the buffer row + 1). */
export function offsetToCell(joined: JoinedLine, offset: number): { x: number; y: number } {
  let rowIdx = 0;
  for (let i = 0; i < joined.rowStarts.length; i++) {
    if (joined.rowStarts[i] <= offset) rowIdx = i;
  }
  return { x: offset - joined.rowStarts[rowIdx] + 1, y: joined.firstRow + rowIdx + 1 };
}
