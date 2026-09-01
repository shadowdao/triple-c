import { describe, it, expect } from "vitest";
import { CLAUDE_SOFT_NEWLINE, toClaudePayload } from "./claudeInput";

describe("toClaudePayload", () => {
  it("is ESC+CR, the sequence Claude Code's own /terminal-setup installs", () => {
    expect(CLAUDE_SOFT_NEWLINE).toBe("\x1b\r");
  });

  it("replaces every newline so the note arrives as one prompt", () => {
    // Typed raw, each \n submits — the note would arrive as three truncated
    // messages instead of one.
    expect(toClaudePayload("one\ntwo\nthree")).toBe("one\x1b\rtwo\x1b\rthree");
  });

  it("normalises CRLF, which is what a paste from Windows carries", () => {
    expect(toClaudePayload("one\r\ntwo")).toBe("one\x1b\rtwo");
  });

  it("leaves single-line text untouched", () => {
    expect(toClaudePayload("just one line")).toBe("just one line");
  });

  it("never appends a terminator", () => {
    // The note lands in the prompt unsubmitted; the user presses Enter. An
    // unsent prompt is recoverable, a sent one is not.
    expect(toClaudePayload("text").endsWith("\r")).toBe(false);
    expect(toClaudePayload("text\n")).toBe("text\x1b\r");
  });
});
