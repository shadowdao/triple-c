import { describe, it, expect } from "vitest";
import {
  sanitizeInvisibles,
  stripClaudeGutter,
  rejoinHardWraps,
  cleanSelection,
  cellWidth,
} from "./copyCleanup";

/**
 * Rows of a real Claude Code 2.1.316 reply, captured from a 70-column pty and
 * replayed through @xterm/headless 5.5 (what `getSelection()` would return for
 * a drag from column 0 of the first row to the end of the last). Claude
 * hard-wraps its own output — a real `\r` + cursor move between rows, never an
 * xterm soft wrap, so every row has `isWrapped === false` and the selection
 * carries a `\n` at each of Claude's breaks. Note the URL split mid-token at
 * exactly column 70, and the code line Claude also wrapped at a space.
 */
const CLAUDE_ROWS_70 = [
  "● The café’s naïve résumé handling means the parser — which was",
  "  written years ago by someone who has since left the team — silently",
  "  drops every accented character it cannot map.",
  "",
  "  See https://example.com/a/very/long/path/that/keeps/going/and/going/",
  "  until/it/wraps/index.html?query=1 for details.",
  "",
  "  - The first item is a long sentence that describes one important",
  "    consideration about the parser in considerable detail, so that it",
  "    lands near twenty-five words.",
  "  - The second item is another long sentence that explains the second",
  "    consideration for the reader, written to contain roughly the same",
  "    number of words as the first.",
  "",
  "  curl -fsSL https://example.com/install.sh | bash -s --",
  "  --prefix=/opt/some/really/long/prefix/path --yes",
];

describe("sanitizeInvisibles", () => {
  it("turns non-breaking and odd-width spaces into plain spaces", () => {
    expect(sanitizeInvisibles("git\u00A0status")).toBe("git status");
    expect(sanitizeInvisibles("a\u202Fb\u2007c\u2002d\u2009e\u200Af\u205Fg")).toBe("a b c d e f g");
  });

  it("removes zero-width spaces, word joiners and BOMs", () => {
    expect(sanitizeInvisibles("\uFEFFnpm\u200B run\u2060 build")).toBe("npm run build");
  });

  it("removes ZWJ/ZWNJ next to ASCII, where they only break commands", () => {
    expect(sanitizeInvisibles("ls\u200D -la\u200C")).toBe("ls -la");
  });

  it("keeps ZWJ inside emoji sequences and ZWNJ inside non-Latin words", () => {
    const family = "👨\u200D👩\u200D👧";
    expect(sanitizeInvisibles(`ok ${family} ok`)).toBe(`ok ${family} ok`);
    const persian = "می\u200Cخواهم"; // ZWNJ is orthographically required here
    expect(sanitizeInvisibles(persian)).toBe(persian);
    const devanagari = "क्\u200Dष";
    expect(sanitizeInvisibles(devanagari)).toBe(devanagari);
  });

  it("leaves legitimate non-ASCII text alone", () => {
    const text = "it’s café — naïve “quotes” 日本語\u3000テキスト 👍🏽 ● ⎿ │";
    expect(sanitizeInvisibles(text)).toBe(text); // U+3000 ideographic space kept
  });

  it("turns Unicode line/paragraph separators into newlines", () => {
    expect(sanitizeInvisibles("a\u2028b\u2029c")).toBe("a\nb\nc");
  });
});

describe("cellWidth", () => {
  it("counts wide CJK and emoji as two cells and zero-width marks as none", () => {
    expect(cellWidth("abc")).toBe(3);
    expect(cellWidth("日本")).toBe(4);
    expect(cellWidth("👍")).toBe(2);
    expect(cellWidth("é")).toBe(1);
    expect(cellWidth("—●⎿")).toBe(3);
  });
});

describe("stripClaudeGutter", () => {
  it("blanks Claude's message bullet at column 0, keeping columns", () => {
    expect(stripClaudeGutter("● Hello\n  world", 0)).toBe("  Hello\n  world");
  });

  it("blanks the tool-result elbow", () => {
    expect(stripClaudeGutter("● Bash(ls)\n  ⎿  a.txt\n     b.txt", 0)).toBe(
      "  Bash(ls)\n     a.txt\n     b.txt",
    );
  });

  it("does not touch a bullet that is not in the gutter", () => {
    expect(stripClaudeGutter("  ● item", 0)).toBe("  ● item");
    // Selection started mid-row: the first line's column 0 is not the gutter.
    expect(stripClaudeGutter("● x", 5)).toBe("● x");
  });

  it("strips box borders only when every line is inside the box", () => {
    const box = [
      "╭────────────────╮",
      "│ Do you want to │",
      "│   proceed?     │",
      "╰────────────────╯",
    ].join("\n");
    expect(stripClaudeGutter(box, 0)).toBe("  Do you want to\n    proceed?");
  });

  it("leaves tables and partial boxes alone", () => {
    const table = "│ a │ b │\n├───┼───┤\n│ 1 │ 2 │";
    expect(stripClaudeGutter(table, 0)).toBe(table);
    const mixed = "│ inside │\noutside";
    expect(stripClaudeGutter(mixed, 0)).toBe(mixed);
  });

  it("leaves a selection of only a table's body rows alone", () => {
    const body = "│ a │ b │\n│ 1 │ 2 │";
    expect(stripClaudeGutter(body, 0)).toBe(body);
    expect(stripClaudeGutter("│ only │ row │", 0)).toBe("│ only │ row │");
  });
});

describe("rejoinHardWraps", () => {
  it("rejoins a real Claude Code reply captured at 70 columns", () => {
    const sel = stripClaudeGutter(CLAUDE_ROWS_70.join("\n"), 0);
    expect(rejoinHardWraps(sel, { cols: 70 })).toBe(
      [
        "  The café’s naïve résumé handling means the parser — which was written years ago by someone who has since left the team — silently drops every accented character it cannot map.",
        "",
        "  See https://example.com/a/very/long/path/that/keeps/going/and/going/until/it/wraps/index.html?query=1 for details.",
        "",
        "  - The first item is a long sentence that describes one important consideration about the parser in considerable detail, so that it lands near twenty-five words.",
        "  - The second item is another long sentence that explains the second consideration for the reader, written to contain roughly the same number of words as the first.",
        "",
        "  curl -fsSL https://example.com/install.sh | bash -s -- --prefix=/opt/some/really/long/prefix/path --yes",
      ].join("\n"),
    );
  });

  it("rejoins Claude's echo of a submitted prompt, which wraps one column short", () => {
    // Same capture: the transcript's echo of the prompt that was typed.
    const echo = [
      "❯ Reply with exactly the following markdown and nothing else (no",
      "  tools). First a paragraph: \"The café’s naïve résumé handling means",
      "  the parser — which was written years ago by someone who has since",
      "  left the team — silently drops every accented character it cannot",
      "  map.\" Then a line: \"See https://example.com/a/very/long/path/that/k",
      "  eeps/going/and/going/until/it/wraps/index.html?query=1 for",
      "  details.\" Then a bullet list with two items, each a long sentence",
      "  of about 25 words. Then a bash code block containing: curl -fsSL",
      "  https://example.com/install.sh | bash -s --",
      "  --prefix=/opt/some/really/long/prefix/path --yes",
    ].join("\n");
    const typed =
      "Reply with exactly the following markdown and nothing else (no tools). First a paragraph: \"The café’s naïve résumé handling means the parser — which was written years ago by someone who has since left the team — silently drops every accented character it cannot map.\" Then a line: \"See https://example.com/a/very/long/path/that/keeps/going/and/going/until/it/wraps/index.html?query=1 for details.\" Then a bullet list with two items, each a long sentence of about 25 words. Then a bash code block containing: curl -fsSL https://example.com/install.sh | bash -s -- --prefix=/opt/some/really/long/prefix/path --yes";
    expect(rejoinHardWraps(echo, { cols: 70 })).toBe(`❯ ${typed}`);
  });

  it("joins a word that exactly filled the row with a space", () => {
    // 70 cells: the last token ("wordabc") and the next ("next") are short,
    // so this was a break at a space that happened to land on the edge.
    const full = "  " + "x".repeat(60) + " wordabc";
    expect(cellWidth(full)).toBe(70);
    expect(rejoinHardWraps(`${full}\n  next words`, { cols: 70 })).toBe(`${full} next words`);
  });

  it("does not join short lines (code, prose that ended early)", () => {
    const code = "  def foo():\n      return 1\n  print(foo())";
    expect(rejoinHardWraps(code, { cols: 70 })).toBe(code);
    const prose = "  This sentence ends well before the edge.\n  So does this one.";
    expect(rejoinHardWraps(prose, { cols: 70 })).toBe(prose);
  });

  it("does not join a line whose next word would have fitted", () => {
    const line = "  " + "a ".repeat(30).trimEnd(); // 61 cells
    expect(rejoinHardWraps(`${line}\n  bb`, { cols: 70 })).toBe(`${line}\n  bb`);
  });

  it("does not join across a change of indentation", () => {
    const a = "  " + "word ".repeat(13).trimEnd(); // 66 cells
    expect(rejoinHardWraps(`${a}\n      indented-continuation`, { cols: 70 })).toBe(
      `${a}\n      indented-continuation`,
    );
  });

  it("does not join into the next list item or a border", () => {
    const a = "  - " + "word ".repeat(12).trimEnd(); // 63 cells
    const next = "  - another item with a longword";
    expect(rejoinHardWraps(`${a}\n${next}`, { cols: 70 })).toBe(`${a}\n${next}`);
    expect(rejoinHardWraps(`${a}\n    │ table`, { cols: 70 })).toBe(`${a}\n    │ table`);
  });

  it("leaves column-0 shell output alone (ls-style columns, plain output)", () => {
    const ls =
      "Cargo.lock  Cargo.toml  README.md  build.rs  capabilities  gen  icons  s\nsrc  target  tauri.conf.json";
    expect(rejoinHardWraps(ls, { cols: 70 })).toBe(ls);
    const plain = "x".repeat(65) + "\nyyyyyyyyyy";
    expect(rejoinHardWraps(plain, { cols: 70 })).toBe(plain);
  });

  it("leaves tabular rows alone even when indented", () => {
    const t = "  NAME          STATUS    PORTS      CREATED AT            SIZE\n  triple-c-abc  running   8080/tcp";
    expect(rejoinHardWraps(t, { cols: 70 })).toBe(t);
  });

  it("joins wide-character text without inserting a space", () => {
    const a = "  " + "日本語".repeat(11); // 2 + 66 = 68 cells; the next wide char needs 2 more
    expect(cellWidth(a)).toBe(68);
    expect(rejoinHardWraps(`${a}\n  の続き`, { cols: 69 })).toBe(`${a}の続き`);
  });

  it("joins astral wide characters (CJK Ext-B) without inserting a space", () => {
    const a = "  " + "\u{20000}".repeat(33); // 2 + 66 = 68 cells
    expect(cellWidth(a)).toBe(68);
    expect(rejoinHardWraps(`${a}\n  \u{20001}\u{20002}`, { cols: 69 })).toBe(`${a}\u{20001}\u{20002}`);
    const e = "  " + "\u{1F600}".repeat(33); // emoji, also two cells each
    expect(rejoinHardWraps(`${e}\n  \u{1F601}`, { cols: 69 })).toBe(`${e}\u{1F601}`);
  });

  it("keeps the space when a short word ended at the edge and a URL that fits was moved down whole", () => {
    const prev = "  " + "x".repeat(64) + " the";
    expect(cellWidth(prev)).toBe(70);
    const url = "https://example.com/" + "p".repeat(35) + "/index.html";
    expect(url.length).toBe(66); // fits in the 68 cells after the gutter
    expect(rejoinHardWraps(`${prev}\n  ${url}`, { cols: 70 })).toBe(`${prev} ${url}`);
  });

  it("still joins without a space when a row-filling piece of a long token is followed by a URL start", () => {
    // A redirect URL whose split happened to land just before its inner `https://`.
    const url = "https://r.example/" + "r".repeat(47) + "?u=https://inner.example/x";
    const rows = ["  " + url.slice(0, 68), "  " + url.slice(68) + " ok"];
    expect(cellWidth(rows[0])).toBe(70);
    expect(rows[1].startsWith("  https://")).toBe(true);
    expect(rejoinHardWraps(rows.join("\n"), { cols: 70 })).toBe(`  ${url} ok`);
  });

  it("rejoins a URL split over three rows without spaces", () => {
    const url = "https://example.com/" + "q".repeat(130) + "/end.html";
    const rows = ["  See " + url.slice(0, 64), "  " + url.slice(64, 132), "  " + url.slice(132) + " ok"];
    expect(rows.map(cellWidth).slice(0, 2)).toEqual([70, 70]);
    expect(rejoinHardWraps(rows.join("\n"), { cols: 70 })).toBe(`  See ${url} ok`);
  });

  it("uses the selection's start column for a first line that began mid-row", () => {
    // Selection started at column 18, so the first line is only 51
    // characters but ended at column 69 of its row.
    const first = "written years ago by someone who has since left the";
    expect(rejoinHardWraps(`${first}\n  team — silently`, { cols: 70, startCol: 18 })).toBe(
      `${first} team — silently`,
    );
    // Without the start column it looks short and is left alone.
    expect(rejoinHardWraps(`${first}\n  team — silently`, { cols: 70 })).toBe(
      `${first}\n  team — silently`,
    );
  });

  it("leaves text unchanged when cols is unknown", () => {
    const sel = CLAUDE_ROWS_70.join("\n");
    expect(rejoinHardWraps(sel, { cols: 0 })).toBe(sel);
  });
});

describe("cleanSelection", () => {
  it("produces paste-ready text from a Claude reply (gutter, wraps, dedent)", () => {
    const out = cleanSelection(CLAUDE_ROWS_70.join("\n"), { cols: 70 });
    expect(out.split("\n")[0]).toBe(
      "The café’s naïve résumé handling means the parser — which was written years ago by someone who has since left the team — silently drops every accented character it cannot map.",
    );
    expect(out).toContain(
      "\nSee https://example.com/a/very/long/path/that/keeps/going/and/going/until/it/wraps/index.html?query=1 for details.\n",
    );
    expect(out.endsWith("\ncurl -fsSL https://example.com/install.sh | bash -s -- --prefix=/opt/some/really/long/prefix/path --yes")).toBe(true);
  });

  it("normalizes Windows line endings from xterm's getSelection", () => {
    expect(cleanSelection("  a\r\n  b\r\n", { cols: 70 })).toBe("a\nb");
  });

  it("strips invisible characters", () => {
    expect(cleanSelection("  git\u202Fcommit\u200B -m x", { cols: 70 })).toBe("git commit -m x");
  });

  it("keeps the old trimSelection behaviour for ordinary text", () => {
    expect(cleanSelection("    def foo():\n        return 1\n", { cols: 80 })).toBe(
      "def foo():\n    return 1",
    );
  });
});
