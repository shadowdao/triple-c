import { describe, expect, it } from "vitest";
import { decodeViewerText, encodeViewerText } from "./textFormat";
import type { Editability } from "./editability";

const editable: Editability = { kind: "text", editable: true, reason: null };
const bytes = (s: string) => new TextEncoder().encode(s);
const BOM = [0xef, 0xbb, 0xbf];

/** What CodeMirror hands back: every line break normalised to "\n". */
const asEditorText = (s: string) => s.replace(/\r\n?/g, "\n");

describe("decodeViewerText / encodeViewerText", () => {
  it("round-trips an LF file byte for byte", () => {
    const d = decodeViewerText(bytes("a\nb\n"), editable);
    expect(d.format).toEqual({ bom: false, eol: "\n" });
    expect(Array.from(encodeViewerText(asEditorText(d.text), d.format))).toEqual(Array.from(bytes("a\nb\n")));
  });

  it("keeps CRLF line endings through the editor's LF buffer", () => {
    const d = decodeViewerText(bytes("a\r\nb\r\nc"), editable);
    expect(d.format.eol).toBe("\r\n");
    const edited = asEditorText(d.text).replace("b", "B");
    expect(new TextDecoder().decode(encodeViewerText(edited, d.format))).toBe("a\r\nB\r\nc");
  });

  it("uses the dominant separator for a mixed file", () => {
    expect(decodeViewerText(bytes("a\r\nb\r\nc\nd"), editable).format.eol).toBe("\r\n");
    expect(decodeViewerText(bytes("a\nb\nc\r\nd"), editable).format.eol).toBe("\n");
    expect(decodeViewerText(bytes("a\rb\rc"), editable).format.eol).toBe("\r");
  });

  it("strips a UTF-8 BOM from the text and puts it back on save", () => {
    const d = decodeViewerText(new Uint8Array([...BOM, ...bytes("hi\n")]), editable);
    expect(d.text).toBe("hi\n");
    expect(d.format.bom).toBe(true);
    expect(Array.from(encodeViewerText("hi\n", d.format))).toEqual([...BOM, ...bytes("hi\n")]);
  });

  it("makes invalid UTF-8 read-only rather than rewriting it", () => {
    const d = decodeViewerText(new Uint8Array([0x61, 0xff, 0x62]), editable);
    expect(d.editability).toMatchObject({ editable: false, reason: expect.stringMatching(/not valid UTF-8/) });
  });
});
