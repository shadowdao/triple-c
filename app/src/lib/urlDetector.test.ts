import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { UrlDetector, flatten, osc8Targets, usableLink } from "./urlDetector";
import type { UrlSource } from "./urlDetector";

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

/**
 * One OSC 8 hyperlink emission: the whole URL in the parameter, a slice of it
 * as visible text.
 *
 * This is what `claude setup-token` actually prints — measured against 2.1.226,
 * a 346-character URL arrives as five of these, each carrying the complete URL
 * and 80 characters of it on screen.
 */
function osc8(uri: string, visible: string): string {
  return `\x1b]8;;${uri}\x07${visible}\x1b]8;;\x07`;
}

/** Slice `uri` into `width`-character visible pieces, each a full hyperlink. */
function slicedHyperlink(uri: string, width = COLS): string {
  const parts: string[] = [];
  for (let i = 0; i < uri.length; i += width) {
    parts.push(osc8(uri, uri.slice(i, i + width)));
  }
  return parts.join("\r\n");
}

/** The URL Claude Code prints, at the length it really is. */
const SIGN_IN_URL =
  "https://claude.ai/oauth/authorize?code=true&client_id=9d1c250a-e61b-44d9-88ed-5944d1962f5e" +
  "&response_type=code&redirect_uri=https%3A%2F%2Fconsole.anthropic.com%2Foauth%2Fcode%2Fcallback" +
  "&scope=org%3Acreate_api_key+user%3Aprofile+user%3Ainference&code_challenge=" +
  "vJ8Kq2mN4pR7sT9wX1zA3bC5dE6fG8hJ0kL2mN4pQ6r&code_challenge_method=S256&state=aB3dE5gH7jK9";

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

describe("usableLink", () => {
  // Ports `usable_sign_in_link` from `commands/auth_token_commands.rs` — a junk
  // filter, not the security decision. `sanitizeRelayUrl` is still what stands
  // between any of this and `openUrl`.
  it("accepts an ordinary authorize URL", () => {
    expect(usableLink(SIGN_IN_URL)).toBe(true);
  });

  it("rejects a scheme that is not http(s)", () => {
    expect(usableLink("file:///etc/passwd")).toBe(false);
    expect(usableLink("javascript:alert(1)")).toBe(false);
  });

  it("rejects anything outside printable ASCII", () => {
    // A control character is how a URL is smuggled past a display, and
    // `new URL()` strips some of them silently.
    expect(usableLink("https://example.com/\u0000x")).toBe(false);
    expect(usableLink("https://exa\u200bmple.com/x")).toBe(false);
    expect(usableLink("https://example.com/a b")).toBe(false);
  });
});

describe("osc8Targets", () => {
  it("lifts the whole URL out of a sliced emission", () => {
    const raw = slicedHyperlink(SIGN_IN_URL);
    // Every piece carries the complete URL, however little of it is on screen.
    expect(new Set(osc8Targets(raw))).toEqual(new Set([SIGN_IN_URL]));
  });

  it("ignores the closing half of a hyperlink", () => {
    expect(osc8Targets("\x1b]8;;\x07")).toEqual([]);
  });

  it("ignores other OSCs, including the URL relay's own", () => {
    expect(osc8Targets("\x1b]0;a window title\x07")).toEqual([]);
    expect(osc8Targets("\x1b]7777;open;aHR0cHM6Ly9leGFtcGxlLmNvbQ==\x07")).toEqual([]);
  });

  it("reads a hyperlink terminated by ST as well as by BEL", () => {
    expect(osc8Targets(`\x1b]8;id=1;${SIGN_IN_URL}\x1b\\text`)).toEqual([SIGN_IN_URL]);
  });
});

describe("UrlDetector — OSC 8", () => {
  it("recovers the complete URL from a sliced hyperlink", () => {
    // The bug this branch exists for. `ANSI_RE` strips OSC sequences wholesale,
    // so the scraper never saw the parameter and reassembled the *visible*
    // pieces instead — a URL that parses, points at claude.ai, and cannot
    // authorise anything.
    const seen: [string, UrlSource][] = [];
    const d = new UrlDetector((u, src) => seen.push([u, src]), () => COLS);

    feed(d, "Open this link to sign in:\r\n" + slicedHyperlink(SIGN_IN_URL) + "\r\ndone\r\n");

    expect(seen[0]).toEqual([SIGN_IN_URL, "osc8"]);
  });

  it("does not emit the same hyperlink again when it is repainted", () => {
    const seen: [string, UrlSource][] = [];
    const d = new UrlDetector((u, s) => seen.push([u, s]), () => COLS);

    feed(d, slicedHyperlink(SIGN_IN_URL) + "\r\n");
    feed(d, slicedHyperlink(SIGN_IN_URL) + "\r\n");

    expect(seen.filter(([u]) => u === SIGN_IN_URL)).toHaveLength(1);
  });

  it("marks a scraped candidate as a guess, so the slot can refuse it", () => {
    // Nothing here decides precedence — that is `supersedes` in TerminalView —
    // but it is what makes the decision possible.
    const seen: [string, UrlSource][] = [];
    const d = new UrlDetector((u, s) => seen.push([u, s]), () => COLS);
    const url = "https://example.com/" + "z".repeat(120);

    feed(d, url + "\r\nnext\r\n");

    expect(seen).toEqual([[url, "heuristic"]]);
  });

  it("never hands back a truncated guess at a link it has already seen exactly", () => {
    // The defect: the prompt slot is emptied (dismissed, or auto-dismissed
    // after 30 s), the OSC 8 target is deduped for the session and cannot come
    // back, and the next repaint — sliced at a different offset, so a *new*
    // string — reassembles into a prefix of the real link that fills the empty
    // slot. It parses, it points at claude.ai, and it authorises nothing.
    //
    // Nothing here knows the slot was emptied, and that is the point: the rule
    // holds however many times it is.
    const seen: [string, UrlSource][] = [];
    const d = new UrlDetector((u, s) => seen.push([u, s]), () => COLS);

    feed(d, "Open this link to sign in:\r\n" + slicedHyperlink(SIGN_IN_URL) + "\r\ndone\r\n");
    expect(seen).toEqual([[SIGN_IN_URL, "osc8"]]);

    // …the user dismisses the toast; the TUI repaints the same link as plain
    // text, cut short by the frame it was painted into.
    feed(d, SIGN_IN_URL.slice(0, 150) + "\r\nWaiting for the browser…\r\n");

    expect(seen).toHaveLength(1);
    expect(seen.map(([u]) => u)).not.toContain(SIGN_IN_URL.slice(0, 150));
  });

  it("still offers a genuinely different link after an exact one", () => {
    // The suppression is a prefix rule, not "one prompt per session".
    const seen: string[] = [];
    const d = new UrlDetector((u) => seen.push(u), () => COLS);
    const other = "https://github.com/login/device?code=" + "x".repeat(90);

    feed(d, slicedHyperlink(SIGN_IN_URL) + "\r\n");
    feed(d, other + "\r\nnext\r\n");

    expect(seen).toEqual([SIGN_IN_URL, other]);
  });

  it("suppresses a guess at a URL the consumer reported from the relay", () => {
    // The OSC 7777 relay hands `TerminalView` a base64-encoded — therefore
    // exact — URL that this detector never sees. `noteExactUrl` is how it gets
    // told, so a dismissed relay prompt cannot be replaced by a scrape of the
    // same link either.
    const seen: string[] = [];
    const d = new UrlDetector((u) => seen.push(u), () => COLS);
    d.noteExactUrl(SIGN_IN_URL);

    feed(d, SIGN_IN_URL.slice(0, 150) + "\r\nnext\r\n");

    expect(seen).toEqual([]);
  });

  it("ignores a short hyperlink", () => {
    // `ls --hyperlink` decorates every filename; none of that is a prompt.
    const seen: string[] = [];
    const d = new UrlDetector((u) => seen.push(u), () => COLS);

    feed(d, osc8("https://example.com/a", "a") + "\r\nnext\r\n");

    expect(seen).toEqual([]);
  });
});

describe("UrlDetector — repaints and resizes", () => {
  it("treats a bare CR as a line break", () => {
    // A TUI repaints by returning to column 0 without a line feed. Splitting on
    // `\r?\n` alone leaves a whole frame on one "line", which is then longer
    // than the width — so the `===` test says "not wrapped" and a break the
    // terminal really did insert is never rejoined.
    const seen: string[] = [];
    const d = new UrlDetector((u) => seen.push(u), () => COLS);
    const url = "https://example.com/" + "q".repeat(100);

    feed(d, "spinner frame one\rspinner frame two\r" + ptyWrap(url) + "\r\ndone\r\n");

    expect(seen).toEqual([url]);
  });

  it("does not glue two repainted frames into one token", () => {
    const seen: string[] = [];
    const d = new UrlDetector((u) => seen.push(u), () => COLS);

    feed(
      d,
      "https://example.com/" + "a".repeat(90) + "\rhttps://evil.tld/" + "b".repeat(90) + "\r\n\r\ndone\r\n",
    );

    for (const url of seen) expect(new URL(url).host).not.toBe("example.comhttps");
  });

  it("reassembles with the width the bytes were printed at, not the current one", () => {
    // The scan runs 300 ms after the print. A resize inside that window used to
    // change every join decision retroactively: text wrapped at 80 columns,
    // rejoined against a width of 120, comes back as separate lines glued with
    // spaces — or, the other way round, as a URL nobody printed.
    const seen: string[] = [];
    let cols = COLS;
    const d = new UrlDetector((u) => seen.push(u), () => cols);
    const url =
      "https://accounts.example.com/o/oauth2/auth?client_id=1234567890-abcdefghijklmnop.apps.example.com&redirect_uri=http%3A%2F%2Flocalhost%3A45678";

    d.feed(new TextEncoder().encode(ptyWrap(url) + "\r\nWaiting…\r\n"));
    cols = 120; // the user drags the window wider before the debounce fires
    vi.advanceTimersByTime(2000);

    expect(seen).toEqual([url]);
  });

  it("drops text buffered at a width that no longer applies", () => {
    // Half printed at 80, half at 120: no single width reassembles both, so the
    // older half goes rather than being joined by a rule that is wrong for it.
    const seen: string[] = [];
    let cols = COLS;
    const d = new UrlDetector((u) => seen.push(u), () => cols);
    const url = "https://example.com/" + "m".repeat(120);

    d.feed(new TextEncoder().encode(ptyWrap(url.slice(0, 100))));
    cols = 120;
    feed(d, url.slice(100) + "\r\ndone\r\n");

    // Whatever survives, it is never a URL that was not printed.
    for (const u of seen) expect(url.startsWith(u) || u.startsWith(url)).toBe(true);
  });
});

describe("flatten — bare CR", () => {
  it("splits on a lone CR as well as on LF", () => {
    expect(flatten("abcde\rfghij", 5)).toBe("abcdefghij");
    expect(flatten("abc\rdef", 5)).toBe("abc def");
  });

  it("counts CRLF as one break, not two", () => {
    expect(flatten("abcde\r\nfghij", 5)).toBe("abcdefghij");
  });
});
