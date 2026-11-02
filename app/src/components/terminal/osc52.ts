/**
 * Decodes the payload of an OSC 52 clipboard *write* (`Pc ; Pd`, where `Pd` is
 * base64) into the text to put on the clipboard.
 *
 * The base64 carries **bytes**, and every sender this app sees — the
 * container's xclip/xsel/pbcopy shim (`printf '%s' | base64`) and Claude Code's
 * own OSC 52 copy — sends UTF-8. `atob` alone returns one UTF-16 code unit per
 * byte, so it turned `’` into `â\x80\x99` and `é` into `Ã©` (the `â€™` mojibake
 * users saw after pasting). The bytes are therefore decoded as UTF-8,
 * non-fatally: a bad or truncated byte becomes U+FFFD and only that byte is
 * lost. There is deliberately no Latin-1 fallback — re-reading the *whole*
 * payload as Latin-1 because of one bad byte brought the mojibake back for
 * everything else, and no sender here emits Latin-1.
 *
 * Returns `null` for a read request (`?`), a malformed sequence, or invalid
 * base64 — the caller writes nothing in those cases. Pure function.
 */
const UTF8 = new TextDecoder("utf-8");

export function decodeOsc52(data: string): string | null {
  const idx = data.indexOf(";");
  if (idx === -1) return null;
  const payload = data.substring(idx + 1);
  if (payload === "?") return null; // clipboard read request, not supported

  let binary: string;
  try {
    binary = atob(payload); // tolerates ASCII whitespace, as base64 without -w0 emits
  } catch {
    return null;
  }

  const bytes = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i++) bytes[i] = binary.charCodeAt(i);
  return UTF8.decode(bytes);
}
