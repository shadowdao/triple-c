import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { UrlDetector, flatten } from "./urlDetector";

const COLS = 80;
const enc = new TextEncoder();

/** Feed text and let the debounce + confirmation timers run. */
function feed(detector: UrlDetector, text: string) {
  detector.feed(enc.encode(text));
  vi.advanceTimersByTime(2000);
}

/** Hard-wrap the way a PTY does: a break every `cols` characters, nothing lost. */
function ptyWrap(text: string, cols = COLS): string {
  const lines: string[] = [];
  for (let i = 0; i < text.length; i += cols) lines.push(text.slice(i, i + cols));
  return lines.join("\r\n");
}

beforeEach(() => vi.useFakeTimers());
afterEach(() => vi.useRealTimers());

describe("flatten", () => {
  it("rejoins a break the terminal inserted at the width", () => {
    expect(flatten("abcde\nfghij", 5)).toBe("abcdefghij");
  });

  it("keeps a break that arrived before the width as a separator", () => {
    expect(flatten("abc\ndef", 5)).toBe("abc def");
  });

  it("rejoins nothing when the width isn't known", () => {
    // Better to lose a wrapped URL than to invent one.
    expect(flatten("abcde\nfghij", 0)).toBe("abcde fghij");
  });
});

describe("UrlDetector", () => {
  it("reconstructs a URL the PTY hard-wrapped mid-token", () => {
    const seen: string[] = [];
    const d = new UrlDetector((u) => seen.push(u), () => COLS);
    const url =
      "https://accounts.example.com/o/oauth2/auth?client_id=1234567890-abcdefghijklmnop.apps.example.com&redirect_uri=http%3A%2F%2Flocalhost%3A45678&scope=openid+email+profile";

    feed(d, "Open this link:\r\n" + ptyWrap(url) + "\r\nWaiting for the browser…\r\n");

    expect(seen).toEqual([url]);
  });

  it("does not glue the text that follows a link onto it", () => {
    // The bug this file exists for. A terminal wrapping a paragraph emits the
    // break *instead of* the space, so deleting every break produced
    // `…/tag/preview-63f3c54Butitprovesyournitpick…` — a different host and
    // path from the one on screen, opened on the user's machine.
    const seen: string[] = [];
    const d = new UrlDetector((u) => seen.push(u), () => COLS);
    const url =
      "https://repo.anhonesthost.net/CyberCoveLLC/Triple-C/releases/tag/preview-63f3c54-with-a-long-enough-suffix-to-scan";

    feed(
      d,
      [
        url,
        "But it proves your nitpick perfectly: every file in it says 0.3.0.",
        "That is the hard-coded patch number.",
      ].join("\r\n") + "\r\n",
    );

    expect(seen).toEqual([url]);
    expect(seen[0]).not.toContain("But");
    // And the host is exactly what was printed — no characters lost.
    expect(new URL(seen[0]).host).toBe("repo.anhonesthost.net");
  });

  it("stops at the end of a short line even when more output follows", () => {
    const seen: string[] = [];
    const d = new UrlDetector((u) => seen.push(u), () => COLS);
    const url = "https://example.com/" + "a".repeat(90);

    feed(d, `${url}\r\nnext line of output\r\n`);

    expect(seen).toEqual([url]);
  });

  it("joins the next line when a token ends exactly at the width", () => {
    // The one case the width rule cannot decide: a URL whose length is an exact
    // multiple of the column count looks identical to one that was cut. Pinned
    // as known behaviour rather than pretended away — the toast still shows the
    // whole candidate, and nothing opens without the user pressing Open.
    const seen: string[] = [];
    const d = new UrlDetector((u) => seen.push(u), () => COLS);
    const url = "https://example.com/" + "c".repeat(2 * COLS - 20); // exactly 2 lines

    feed(d, `${ptyWrap(url)}\r\nTAIL\r\n`);

    expect(seen).toEqual([url + "TAIL"]);
  });

  it("ignores anything under the length threshold", () => {
    const seen: string[] = [];
    const d = new UrlDetector((u) => seen.push(u), () => COLS);

    feed(d, "see https://example.com/short\r\nmore text\r\n");

    expect(seen).toEqual([]);
  });

  it("emits a wrapped URL once, not once per chunk", () => {
    const seen: string[] = [];
    const d = new UrlDetector((u) => seen.push(u), () => COLS);
    const url = "https://example.com/" + "b".repeat(120);

    feed(d, ptyWrap(url));
    feed(d, "\r\ndone\r\n");

    expect(seen).toEqual([url]);
  });
});
