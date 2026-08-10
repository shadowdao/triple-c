import { describe, it, expect } from "vitest";
import { authErrorMessage, extractSignInUrl } from "./useClaudeAuth";

describe("extractSignInUrl", () => {
  it("finds the authorize URL in realistic setup-token output", () => {
    const url =
      "https://claude.ai/oauth/authorize?code=true&client_id=abc&redirect_uri=https%3A%2F%2Fplatform.claude.com%2Foauth%2Fcode%2Fcallback";
    expect(
      extractSignInUrl(
        `Claude Code long-lived token setup\nBrowser didn't open? Use this url to sign in:\n${url}\n\nPaste code here if prompted > `,
      ),
    ).toBe(url);
  });

  it("returns null before the CLI has printed anything useful", () => {
    expect(extractSignInUrl("")).toBeNull();
    expect(extractSignInUrl("Starting `claude setup-token`…\n")).toBeNull();
  });

  it("drops trailing prose punctuation", () => {
    expect(extractSignInUrl("Visit https://claude.ai/oauth/authorize?x=1.")).toBe(
      "https://claude.ai/oauth/authorize?x=1",
    );
  });

  it("prefers the OAuth URL over unrelated links in the transcript", () => {
    const text =
      "Docs: https://docs.claude.com/en/docs/claude-code/setup-token-and-more-words\n" +
      "Sign in: https://claude.ai/oauth/authorize?code=true\n";
    expect(extractSignInUrl(text)).toBe("https://claude.ai/oauth/authorize?code=true");
  });

  it("keeps the full link when a TUI repaint also emitted a truncated one", () => {
    const full = "https://claude.ai/oauth/authorize?code=true&client_id=abcdefgh";
    const text = `https://claude.ai/oauth/authorize?code=tr\n${full}\n`;
    expect(extractSignInUrl(text)).toBe(full);
  });
});

describe("authErrorMessage", () => {
  it("passes a Tauri string rejection through verbatim", () => {
    const backend =
      "The container for 'api' is not running. Start it, then run authentication again.";
    expect(authErrorMessage(backend, "fallback")).toBe(backend);
  });

  it("uses an Error's message", () => {
    expect(authErrorMessage(new Error("channel closed"), "fallback")).toBe(
      "channel closed",
    );
  });

  it("falls back rather than stringifying an opaque value", () => {
    expect(authErrorMessage({ weird: true }, "Something went wrong.")).toBe(
      "Something went wrong.",
    );
    expect(authErrorMessage("   ", "Something went wrong.")).toBe(
      "Something went wrong.",
    );
  });
});
