import { describe, it, expect, vi, beforeEach } from "vitest";

/**
 * Tests the OSC 52 clipboard parsing logic used in TerminalView.
 * Extracted here to validate the decode/write path independently.
 */

// Mirrors the handler registered in TerminalView.tsx
function handleOsc52(data: string): string | null {
  const idx = data.indexOf(";");
  if (idx === -1) return null;
  const payload = data.substring(idx + 1);
  if (payload === "?") return null;
  try {
    return atob(payload);
  } catch {
    return null;
  }
}

describe("OSC 52 clipboard handler", () => {
  it("decodes a valid clipboard write sequence", () => {
    // "c;BASE64" where BASE64 encodes "https://example.com"
    const encoded = btoa("https://example.com");
    const result = handleOsc52(`c;${encoded}`);
    expect(result).toBe("https://example.com");
  });

  it("decodes multi-line content", () => {
    const text = "line1\nline2\nline3";
    const encoded = btoa(text);
    const result = handleOsc52(`c;${encoded}`);
    expect(result).toBe(text);
  });

  it("handles primary selection target (p)", () => {
    const encoded = btoa("selected text");
    const result = handleOsc52(`p;${encoded}`);
    expect(result).toBe("selected text");
  });

  it("returns null for clipboard read request (?)", () => {
    expect(handleOsc52("c;?")).toBe(null);
  });

  it("returns null for missing semicolon", () => {
    expect(handleOsc52("invalid")).toBe(null);
  });

  it("returns null for invalid base64", () => {
    expect(handleOsc52("c;!!!not-base64!!!")).toBe(null);
  });

  it("handles empty payload after selection target", () => {
    // btoa("") = ""
    const result = handleOsc52("c;");
    expect(result).toBe("");
  });
});
