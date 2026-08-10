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

  // ── The spoof this function exists to refuse ──────────────────────────────
  // The transcript is container output. Everything below is a URL a misbehaving
  // sandboxed agent can print at will, and the modal renders whatever comes
  // back under a heading that says "Sign in with Anthropic".

  it("rejects userinfo that makes an attacker's host read as Anthropic's", () => {
    // Displays as `https://claude.ai...` in anything that truncates; navigates
    // to evil.tld and harvests the real credential.
    const spoof =
      "https://claude.ai@evil.tld/oauth/authorize?" + "padding=".repeat(40);
    expect(extractSignInUrl(`Use this url to sign in:\n${spoof}\n`)).toBeNull();
  });

  it("does not let a longer hostile URL displace the real one", () => {
    const real = "https://claude.ai/oauth/authorize?code=true&client_id=abc";
    const longer =
      "https://evil.tld/oauth/authorize?" + "x".repeat(real.length * 2);
    expect(extractSignInUrl(`${real}\n${longer}\n`)).toBe(real);
    // ...and the same when the hostile one is printed first.
    expect(extractSignInUrl(`${longer}\n${real}\n`)).toBe(real);
  });

  it("rejects a host that merely contains an Anthropic domain", () => {
    expect(
      extractSignInUrl("Sign in: https://claude.ai.evil.tld/oauth/authorize\n"),
    ).toBeNull();
    expect(
      extractSignInUrl("Sign in: https://evil.tld/claude.ai/oauth/authorize\n"),
    ).toBeNull();
  });

  it("rejects non-http schemes and control characters smuggled into the link", () => {
    expect(extractSignInUrl("Open javascript:alert(1) to continue\n")).toBeNull();
    expect(
      extractSignInUrl("https://claude.ai/oauth\u0000/authorize\n"),
    ).toBe("https://claude.ai/oauth");
  });

  it("takes the first legitimate link, not the longest", () => {
    const first = "https://claude.ai/oauth/authorize?code=true";
    const second = "https://platform.claude.com/oauth/authorize?code=true&more=1";
    expect(extractSignInUrl(`${first}\n${second}\n`)).toBe(first);
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
