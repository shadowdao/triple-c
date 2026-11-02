import { describe, it, expect } from "vitest";
import { decodeOsc52 } from "./osc52";

/** Base64 of the UTF-8 bytes of `text` — what the container shim
 *  (`printf '%s' "$data" | base64`) and Claude Code's own OSC 52 both send. */
function utf8Base64(text: string): string {
  return Buffer.from(text, "utf8").toString("base64");
}

describe("decodeOsc52", () => {
  it("decodes a valid clipboard write sequence", () => {
    expect(decodeOsc52(`c;${utf8Base64("https://example.com")}`)).toBe("https://example.com");
  });

  it("decodes multi-line content", () => {
    const text = "line1\nline2\nline3";
    expect(decodeOsc52(`c;${utf8Base64(text)}`)).toBe(text);
  });

  it("handles primary selection target (p) and an empty target", () => {
    expect(decodeOsc52(`p;${utf8Base64("selected text")}`)).toBe("selected text");
    expect(decodeOsc52(`;${utf8Base64("selected text")}`)).toBe("selected text");
  });

  // The user-visible bug: `atob` returns one char per *byte*, so every
  // multi-byte UTF-8 sequence came out as Latin-1 mojibake (’ → "â\x80\x99",
  // é → "Ã©"), which Windows apps render as `â€™`.
  it("decodes UTF-8 multi-byte text instead of mojibaking it", () => {
    const text = "it’s café — naïve 日本語 👍🏽 👨\u200D👩\u200D👧";
    const decoded = decodeOsc52(`c;${utf8Base64(text)}`);
    expect(decoded).toBe(text);
    expect(decoded).not.toContain("Ã");
    expect(decoded).not.toContain("â");
  });

  // One bad or truncated byte must not re-mojibake the whole payload: only
  // the bad bytes become U+FFFD, the valid UTF-8 around them is kept.
  it("replaces only invalid bytes with U+FFFD, keeping the valid UTF-8", () => {
    const bytes = Buffer.concat([Buffer.from("it’s ", "utf8"), Buffer.from([0xff]), Buffer.from(" café", "utf8")]);
    expect(decodeOsc52(`c;${bytes.toString("base64")}`)).toBe("it’s \uFFFD café");
  });

  it("keeps the text before a multi-byte sequence truncated at the end", () => {
    const full = Buffer.from("naïve ’", "utf8");
    const truncated = full.subarray(0, full.length - 1); // drop the last byte of ’
    expect(decodeOsc52(`c;${truncated.toString("base64")}`)).toBe("naïve \uFFFD");
  });

  it("does not guess Latin-1 for a payload that is not UTF-8", () => {
    // Every sender this app sees sends UTF-8; a Latin-1 é is just a bad byte.
    const latin1 = Buffer.from([0x63, 0x61, 0x66, 0xe9]).toString("base64");
    expect(decodeOsc52(`c;${latin1}`)).toBe("caf\uFFFD");
  });

  it("tolerates whitespace/newlines inside the base64 (base64 without -w0)", () => {
    const b64 = utf8Base64("x".repeat(100));
    const wrapped = b64.slice(0, 76) + "\n" + b64.slice(76);
    expect(decodeOsc52(`c;${wrapped}`)).toBe("x".repeat(100));
  });

  it("returns null for clipboard read request (?)", () => {
    expect(decodeOsc52("c;?")).toBe(null);
  });

  it("returns null for missing semicolon", () => {
    expect(decodeOsc52("invalid")).toBe(null);
  });

  it("returns null for invalid base64", () => {
    expect(decodeOsc52("c;!!!not-base64!!!")).toBe(null);
  });

  it("handles empty payload after selection target", () => {
    expect(decodeOsc52("c;")).toBe("");
  });
});
