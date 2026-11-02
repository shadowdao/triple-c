import { trimSelection } from "./trimSelection";

/**
 * Clean-up applied to text copied out of a terminal, so that pasting it into
 * an editor, a shell or a chat works.
 *
 * Three separate problems, fixed by separate pure functions:
 *
 *  - **Invisible characters** (`sanitizeInvisibles`). Claude Code prints
 *    `❯` + U+00A0 for its prompt, and model output can carry NBSP / narrow
 *    NBSP / zero-width spaces; pasted into a shell they make `git status` an
 *    unknown command. xterm's `getSelection()` already maps U+00A0 (only that
 *    one) to a space; everything else survived until now.
 *  - **Claude's TUI chrome** (`stripClaudeGutter`): the `●` message bullet and
 *    `⎿` tool-result elbow in the left gutter, and `│ … │` dialog borders.
 *  - **Claude's hard wraps** (`rejoinHardWraps`). Claude Code wraps its own
 *    output to the terminal width and moves the cursor to the next row itself,
 *    so xterm never marks those rows `isWrapped` and a selection carries a real
 *    newline (plus the 2-column gutter indent) at every break — mid-sentence,
 *    and mid-URL when a long token was split at the edge.
 *
 * `cleanSelection` composes them with `trimSelection` for the trimmed copy
 * (Ctrl+Shift+C, context-menu "Copy"). OSC 52 content gets only
 * `sanitizeInvisibles`: the program chose that text, so it is not reflowed or
 * dedented — only made safe to paste. Raw copy (Ctrl+Shift+Alt+C) gets none.
 */

// ── invisible characters ────────────────────────────────────────────────────

/** NBSP, figure space, narrow NBSP, the U+2000–U+200A width spaces, medium
 *  math space. U+3000 (ideographic space) is deliberately absent: it is real
 *  CJK typography and occupies two cells. */
const ODD_SPACES = /[\u00A0\u2000-\u200A\u202F\u205F]/g;
/** Zero-width space, word joiner, BOM / zero-width no-break space. */
const ZERO_WIDTH = /[\u200B\u2060\uFEFF]/g;
/** ZWNJ / ZWJ touching ASCII (or a string edge). Between two non-ASCII
 *  characters they are meaningful — emoji ZWJ sequences, Persian ZWNJ,
 *  Indic conjunct control — so only this case is removed. */
const JOINER_NEAR_ASCII = /(^|[\x00-\x7F])[\u200C\u200D]+|[\u200C\u200D]+(?=[\x00-\x7F]|$)/g;

export function sanitizeInvisibles(text: string): string {
  if (!text) return text;
  return text
    .replace(/[\u2028\u2029]/g, "\n")
    .replace(ODD_SPACES, " ")
    .replace(ZERO_WIDTH, "")
    .replace(JOINER_NEAR_ASCII, "$1");
}

// ── cell width ──────────────────────────────────────────────────────────────

const WIDE_RANGES: ReadonlyArray<readonly [number, number]> = [
  [0x1100, 0x115f],
  [0x2e80, 0x303e],
  [0x3041, 0x33ff],
  [0x3400, 0x4dbf],
  [0x4e00, 0x9fff],
  [0xa000, 0xa4cf],
  [0xac00, 0xd7a3],
  [0xf900, 0xfaff],
  [0xfe30, 0xfe4f],
  [0xff00, 0xff60],
  [0xffe0, 0xffe6],
  [0x1f300, 0x1f64f],
  [0x1f900, 0x1f9ff],
  [0x20000, 0x3fffd],
];
const ZERO_CELL = /^[\p{M}\u200B-\u200D\u2060\uFE00-\uFE0F\uFEFF]$/u;

function charWidth(ch: string): number {
  if (ZERO_CELL.test(ch)) return 0;
  const cp = ch.codePointAt(0) ?? 0;
  for (const [lo, hi] of WIDE_RANGES) if (cp >= lo && cp <= hi) return 2;
  return 1;
}

/** Terminal cells a string occupies — an approximation of xterm's Unicode 6
 *  width table, good enough to tell whether a row reached the right edge. */
export function cellWidth(text: string): number {
  let w = 0;
  for (const ch of text) w += charWidth(ch);
  return w;
}

// ── Claude Code's gutter and borders ────────────────────────────────────────

const BOX_TOP_BOTTOM = /^\s*[╭╰┌└][─━]*[╮╯┐┘]\s*$/;
const BOX_ROW = /^(\s*)│(.*)│\s*$/;
/** A vertical rule between the borders: a table cell separator, not a box. */
const INTERIOR_VERTICAL = /[│┃║]/;
const isBoxRow = (l: string) => {
  const m = BOX_ROW.exec(l);
  return !!m && !INTERIOR_VERTICAL.test(m[2]);
};

/**
 * Blanks Claude Code's TUI markers without moving any text: `●` at column 0
 * and `⎿` at column 2 become spaces (so `rejoinHardWraps` still sees true
 * columns, and `trimSelection` dedents them away). A selection that is wholly
 * inside a `╭─╮ │ … │ ╰─╯` box loses the box. Anything less certain — a table
 * (`├─┼─┤` rows, or body rows alone: `│ a │ b │` has a `│` inside its borders),
 * a bullet elsewhere, a box mixed with outside text — is left as it is.
 *
 * `startCol` is the column the selection began at; when it is not 0 the first
 * line's column 0 is not the gutter.
 */
export function stripClaudeGutter(text: string, startCol = 0): string {
  if (!text) return text;
  let lines = text.split("\n");

  const content = lines.filter((l) => l.trim() !== "" && !BOX_TOP_BOTTOM.test(l));
  if (content.length > 0 && content.every(isBoxRow)) {
    lines = lines
      .filter((l) => !BOX_TOP_BOTTOM.test(l))
      .map((l) => l.replace(BOX_ROW, (_m, lead: string, inner: string) => `${lead} ${inner}`.trimEnd()));
  }

  return lines
    .map((line, i) => {
      if (i === 0 && startCol !== 0) return line;
      if (line.startsWith("● ")) return " " + line.slice(1);
      if (line.startsWith("  ⎿ ")) return "   " + line.slice(3);
      return line;
    })
    .join("\n");
}

// ── Claude Code's hard wraps ────────────────────────────────────────────────

export interface CopyGeometry {
  /** Terminal width in columns (`term.cols`). 0 / unknown disables rejoining. */
  cols: number;
  /** Column (0-based) where the selection's first row began. */
  startCol?: number;
}

/** A list marker or Claude prompt glyph, and the spaces after it. */
const MARKER = /^([-*+•●❯⎿]|\d{1,3}[.)]) +/;
/** A row that starts a border, rule or table line. */
const BORDER_START = /^[│┃├┤┌┐└┘╭╮╰╯─━┼]/;
/** Two or more spaces between words: column-aligned output, not prose. */
const TABULAR = /\S {2,}\S/;
/** Claude Code always renders its transcript at least 2 columns in. */
const MIN_GUTTER = 2;

interface Row {
  text: string;
  /** Column of the first non-space character. */
  lead: number;
  /** Column just past the last character. */
  end: number;
  /** Text after the indent. */
  body: string;
  /** Column where text after a list marker starts (or `lead` if none). */
  contentCol: number;
  hasMarker: boolean;
}

function toRow(text: string, offset: number): Row {
  const body = text.trimStart();
  const lead = offset + cellWidth(text.slice(0, text.length - body.length));
  const marker = body.match(MARKER);
  return {
    text,
    lead,
    end: offset + cellWidth(text),
    body,
    contentCol: lead + (marker ? cellWidth(marker[0]) : 0),
    hasMarker: !!marker,
  };
}

const lastToken = (s: string) => s.slice(s.lastIndexOf(" ") + 1);
const firstToken = (s: string) => s.split(" ", 1)[0];
/** First / last *code point* — an astral character (CJK Ext-B, most emoji) is two UTF-16 units. */
const firstChar = (s: string) => (s ? String.fromCodePoint(s.codePointAt(0)!) : "");
const lastChar = (s: string) => Array.from(s.slice(-2)).pop() ?? "";
const isWide = (ch: string) => !!ch && charWidth(ch) === 2;

/**
 * Re-joins rows that Claude Code broke to fit the terminal width.
 *
 * A break is undone only when all of these hold — each is a property every
 * Claude wrap has, so together they are conservative:
 *  - both rows have text, and the next row does not start a list item or a
 *    border/table line;
 *  - the next row is indented exactly to where the logical line's text starts
 *    (the gutter, or the hanging indent after `- ` / `1. `), and at least 2
 *    columns — so column-0 shell output is never touched;
 *  - the previous row is not column-aligned (no double spaces between words);
 *  - the next row's first word would *not* have fitted on the previous row,
 *    i.e. the wrapper had no choice but to break there.
 *
 * Joined with a space (Claude drops the space it broke at), except when a long
 * token such as a URL was split mid-token (see `splitMidToken`) or the break
 * is between two wide (CJK) characters.
 */
export function rejoinHardWraps(text: string, { cols, startCol = 0 }: CopyGeometry): string {
  if (!text || !(cols > 0)) return text;
  const rows = text.split("\n").map((t, i) => toRow(t, i === 0 ? startCol : 0));
  if (rows.length < 2) return text;

  const out: string[] = [rows[0].text];
  let head = rows[0];
  // A first row that began mid-row: what was left of it is unknown.
  let headPartial = startCol > 0 && !rows[0].hasMarker;

  for (let i = 1; i < rows.length; i++) {
    const prev = rows[i - 1];
    const cur = rows[i];
    const joiner = joinerFor(prev, cur, head, headPartial, cols);
    if (joiner === null) {
      out.push(cur.text);
      head = cur;
      headPartial = false;
    } else {
      out[out.length - 1] += joiner + cur.body;
    }
  }
  return out.join("\n");
}

function joinerFor(prev: Row, cur: Row, head: Row, headPartial: boolean, cols: number): string | null {
  if (!prev.body || !cur.body) return null;
  if (cur.hasMarker || BORDER_START.test(cur.body)) return null;
  if (cur.lead < MIN_GUTTER) return null;
  if (headPartial ? cur.lead > head.lead : cur.lead !== head.contentCol) return null;

  const prevText = prev === head && prev.hasMarker ? prev.body.replace(MARKER, "") : prev.body;
  if (TABULAR.test(prevText)) return null;

  // Claude's echo of a submitted prompt (`❯ …`) wraps one column short of
  // its replies — measured on 2.1.316 at 70 columns: rows end at 69, and a
  // long token is split at column 69.
  const width = head.body.startsWith("❯") ? cols - 1 : cols;
  if (prev.end > width) return null;
  const next = firstToken(cur.body);
  if (prev.end + 1 + cellWidth(next) <= width) return null;

  if (isWide(lastChar(prev.body)) && isWide(firstChar(cur.body))) return "";
  return prev.end === width && splitMidToken(prev, cur, next, width) ? "" : " ";
}

/** The start of a URL: a token that begins here was not cut off mid-token. */
const URL_START = /^[a-z][a-z0-9+.-]*:\/\//i;

/**
 * Did Claude break a single long token at the edge of `prev` (so the rows
 * join with no space), rather than moving the next word down whole?
 *
 * Ink's wrap-ansi splits a word only when it is wider than the line, so the
 * combined token must not fit in the line's available width. That alone
 * cannot tell `the` + a 66-cell URL (two words; the URL fits on a row and was
 * moved down) from one 69-cell token split after `the` — the lengths are the
 * same. So: a next-row token that *starts a URL* (`scheme://`) was moved down
 * whole, unless one of the two pieces fills its entire row, which only the
 * middle piece of a token longer than a line does.
 */
function splitMidToken(prev: Row, cur: Row, next: string, width: number): boolean {
  const tail = cellWidth(lastToken(prev.body));
  const head = cellWidth(next);
  if (tail + head <= width - cur.lead) return false; // would have been moved down whole
  const prevIsPiece = tail >= width - prev.lead;
  const curIsPiece = cur.end === width && head === cellWidth(cur.body);
  if (prevIsPiece || curIsPiece) return true;
  return !URL_START.test(next);
}

// ── the trimmed copy ────────────────────────────────────────────────────────

/** The full clean-up for a trimmed copy of an xterm selection. */
export function cleanSelection(text: string, geometry: CopyGeometry): string {
  if (!text) return text;
  const lf = text.replace(/\r\n?/g, "\n"); // getSelection() joins with CRLF on Windows
  const visible = sanitizeInvisibles(lf);
  const unframed = stripClaudeGutter(visible, geometry.startCol ?? 0);
  return trimSelection(rejoinHardWraps(unframed, geometry));
}
