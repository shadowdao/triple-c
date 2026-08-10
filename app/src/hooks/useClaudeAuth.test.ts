import { describe, it, expect } from "vitest";
import {
  authErrorMessage,
  extractSignInUrl,
  pickSignInUrl,
} from "./useClaudeAuth";

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

  // ── Why the scraper is only the fallback ─────────────────────────────────
  // `claude setup-token` emits the URL as an OSC 8 hyperlink and slices the
  // *visible* text of it to the terminal width, so the transcript holds five
  // 80-character pieces of a 346-character URL. Each piece is a valid,
  // Anthropic-hosted, oauth-looking URL — and none of them authorises
  // anything.

  it("cannot recover a URL the CLI sliced across lines, which is why the hyperlink wins", () => {
    const slices = [
      FULL_URL.slice(0, 80),
      FULL_URL.slice(80, 160),
      FULL_URL.slice(160, 240),
      FULL_URL.slice(240, 320),
      FULL_URL.slice(320),
    ];
    const scraped = extractSignInUrl(slices.join("\n"));

    // Documenting the limit, not endorsing it: the pieces share no prefix, so
    // the "extends the current pick" rule cannot join them, and guessing at
    // line joins on an untrusted stream is not on the table.
    expect(scraped).toBe(slices[0]);
    expect(scraped).not.toBe(FULL_URL);

    // The hyperlink parameter carries the whole thing, and that is what the
    // hook prefers.
    expect(pickSignInUrl([FULL_URL])).toBe(FULL_URL);
  });
});

/** The real sign-in URL, at its measured length (346 characters, Claude Code
 *  2.1.226). */
const FULL_URL =
  "https://claude.com/cai/oauth/authorize?code=true&client_id=9d1c250a-e61b-44d9-88ed-5944d1962f5e&response_type=code&redirect_uri=https%3A%2F%2Fplatform.claude.com%2Foauth%2Fcode%2Fcallback&scope=user%3Ainference&code_challenge=RUX5MlWvwld1dmpvF_aPIJQWMBmffuJt4dOdL13zWAg&code_challenge_method=S256&state=su-x9PgZzvkBd3-um6G1llLNDgxptyO6HERvvCSrTbg";

describe("pickSignInUrl", () => {
  it("keeps a 346-character authorize URL intact", () => {
    expect(FULL_URL).toHaveLength(346);
    expect(pickSignInUrl([FULL_URL])).toBe(FULL_URL);
  });

  it("applies the same host allowlist to a hyperlink target", () => {
    // An OSC 8 parameter is container output like anything else, and it is
    // never displayed — so it is the *easier* place to hide a hostile host.
    expect(pickSignInUrl(["https://evil.tld/cai/oauth/authorize"])).toBeNull();
    expect(
      pickSignInUrl(["https://claude.ai@evil.tld/oauth/authorize"]),
    ).toBeNull();
    expect(pickSignInUrl(["javascript:alert(1)"])).toBeNull();
    expect(pickSignInUrl([])).toBeNull();
  });

  it("does not let a later hyperlink displace the one already shown", () => {
    const real = `${FULL_URL}`;
    const spoof = "https://claude.com.evil.tld/cai/oauth/authorize?code=true";
    expect(pickSignInUrl([real, spoof])).toBe(real);
    expect(pickSignInUrl([spoof, real])).toBe(real);
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
