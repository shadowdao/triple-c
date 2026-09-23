/**
 * Byte-faithful text for the editor: a save must change only what the user
 * edited. CodeMirror normalises every line break to "\n" and the UTF-8
 * decoder drops a BOM, so both are recorded on load and restored on save.
 */
import type { Editability } from "./editability";

export type LineEnding = "\n" | "\r\n" | "\r";
export interface TextFormat { bom: boolean; eol: LineEnding }

const BOM = [0xef, 0xbb, 0xbf];
const NOT_UTF8 = "This file is not valid UTF-8, so it is read-only.";

/** The most common separator in the text; "\n" on a tie or with no breaks. */
function dominantEol(text: string): LineEnding {
  let crlf = 0, lf = 0, cr = 0;
  for (let i = 0; i < text.length; i++) {
    const c = text.charCodeAt(i);
    if (c === 13) {
      if (text.charCodeAt(i + 1) === 10) { crlf++; i++; } else cr++;
    } else if (c === 10) lf++;
  }
  if (crlf > lf && crlf >= cr) return "\r\n";
  if (cr > lf && cr > crlf) return "\r";
  return "\n";
}

export function decodeViewerText(
  bytes: Uint8Array,
  editability: Editability,
): { text: string; editability: Editability; format: TextFormat } {
  const bom = bytes.length >= 3 && BOM.every((b, i) => bytes[i] === b);
  const body = bom ? bytes.subarray(3) : bytes;
  let text: string;
  if (!editability.editable) {
    text = new TextDecoder("utf-8", { ignoreBOM: true }).decode(body);
  } else {
    // An editable file must round-trip, so invalid UTF-8 (which the lenient
    // decoder would turn into U+FFFD, and a save would write back) is read-only.
    try {
      text = new TextDecoder("utf-8", { fatal: true, ignoreBOM: true }).decode(body);
    } catch {
      text = new TextDecoder("utf-8", { ignoreBOM: true }).decode(body);
      editability = { kind: "text", editable: false, reason: NOT_UTF8 };
    }
  }
  return { text, editability, format: { bom, eol: dominantEol(text) } };
}

/** The editor's "\n"-joined text back to the file's bytes. */
export function encodeViewerText(text: string, format: TextFormat): Uint8Array {
  const body = new TextEncoder().encode(format.eol === "\n" ? text : text.split("\n").join(format.eol));
  if (!format.bom) return body;
  const out = new Uint8Array(body.length + 3);
  out.set(BOM, 0);
  out.set(body, 3);
  return out;
}
