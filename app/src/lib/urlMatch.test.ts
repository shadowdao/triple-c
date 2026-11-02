import { describe, it, expect } from "vitest";
import { TERMINAL_URL_REGEX, findUrls, trimUrlCandidate } from "./urlMatch";

/**
 * Every URL `WebLinksAddon` would underline in `line`.
 *
 * Mirrors `LinkComputer.computeLink` in `@xterm/addon-web-links`: it rebuilds
 * the regex with the `g` flag added to whatever flags it was given and
 * `exec`s in a loop. Run the same way here so the test exercises the regex as
 * the addon will, not as a one-off `match` would.
 */
function underlined(line: string): string[] {
  const rex = new RegExp(TERMINAL_URL_REGEX.source, (TERMINAL_URL_REGEX.flags || "") + "g");
  const out: string[] = [];
  let m: RegExpExecArray | null;
  while ((m = rex.exec(line))) out.push(m[0]);
  return out;
}

describe("TERMINAL_URL_REGEX", () => {
  it("carries no g or y flag, which the addon adds itself", () => {
    expect(TERMINAL_URL_REGEX.flags).not.toMatch(/[gy]/);
  });

  it.each([
    ["See https://example.com/docs.", "https://example.com/docs"],
    ["at https://example.com/docs, then", "https://example.com/docs"],
    ["is it https://example.com/docs?", "https://example.com/docs"],
    ["wow https://example.com/docs!", "https://example.com/docs"],
    ["go to https://example.com/docs: it", "https://example.com/docs"],
    ["https://example.com/docs; and", "https://example.com/docs"],
    ["end https://example.com/docs...", "https://example.com/docs"],
    ["(https://example.com/foo)", "https://example.com/foo"],
    ["(see https://example.com/foo).", "https://example.com/foo"],
    ["[text](https://x.y/z)", "https://x.y/z"],
    ["[text](https://x.y/z).", "https://x.y/z"],
    ["<https://x.example>", "https://x.example"],
    ["[https://x.example/a]", "https://x.example/a"],
    ["{https://x.example/a}", "https://x.example/a"],
    ["\"https://x.example/a\"", "https://x.example/a"],
    ["'https://x.example/a'", "https://x.example/a"],
    ["`https://x.example/a`", "https://x.example/a"],
    ["**https://x.example/a**", "https://x.example/a"],
    ["https://x.example/a/", "https://x.example/a/"],
  ])("leaves the trailing punctuation off %j", (line, url) => {
    expect(underlined(line)).toEqual([url]);
  });

  it.each([
    "https://en.wikipedia.org/wiki/Foo_(bar)",
    "https://example.com/search?q=a.b&x=1",
    "https://example.com/a,b/c",
    "https://example.com/path/file.tar.gz",
    "https://example.com/#/route",
    "https://example.com/a?arr[]=1&arr[]=2",
    "http://[::1]:8080/x",
    "https://example.com/a)b",
    "https://example.com/a(b",
    "https://example.com/api/{id}/x",
    // OAuth state and code_challenge are base64url, which can end in `_`, `-`
    // or `=` padding. None of those may be trimmed.
    "https://claude.ai/oauth/authorize?code=true&state=abc-_",
    "https://claude.ai/oauth/authorize?code_challenge=abc-",
    "https://example.com/x?t=YWJj==",
    "https://example.com/~user",
  ])("keeps %j whole", (url) => {
    expect(underlined(url)).toEqual([url]);
    expect(underlined(`see ${url} now`)).toEqual([url]);
  });

  it("keeps a balanced paren and drops the one that wraps the link", () => {
    expect(underlined("(https://en.wikipedia.org/wiki/Foo_(bar))")).toEqual([
      "https://en.wikipedia.org/wiki/Foo_(bar)",
    ]);
    expect(underlined("[Foo](https://en.wikipedia.org/wiki/Foo_(bar)).")).toEqual([
      "https://en.wikipedia.org/wiki/Foo_(bar)",
    ]);
  });

  it("finds several links on one line", () => {
    expect(underlined("a https://a.example/1, b (https://b.example/2).")).toEqual([
      "https://a.example/1",
      "https://b.example/2",
    ]);
  });

  it("matches nothing after a bare scheme", () => {
    expect(underlined("https://.")).toEqual([]);
    expect(underlined("https:// x")).toEqual([]);
  });

  it("still stops at control characters", () => {
    expect(underlined("https://example.com/a\x1bb")).toEqual(["https://example.com/a"]);
    expect(underlined("https://example.com/a\x00b")).toEqual(["https://example.com/a"]);
    expect(underlined("https://example.com/a\x7fb")).toEqual(["https://example.com/a"]);
  });

  it("does not hang on a long run of punctuation", () => {
    const line = "https://example.com/" + "(.".repeat(5000) + ")".repeat(5000);
    const t0 = Date.now();
    underlined(line);
    expect(Date.now() - t0).toBeLessThan(1000);
  });
});

describe("findUrls", () => {
  it("returns the trimmed URL and the extent of the raw run", () => {
    expect(findUrls("See https://example.com/docs. next")).toEqual([
      { url: "https://example.com/docs", index: 4, rawEnd: 29 },
    ]);
  });
});

describe("trimUrlCandidate", () => {
  it("trims trailing punctuation and unbalanced closers", () => {
    expect(trimUrlCandidate("https://example.com/docs.")).toBe("https://example.com/docs");
    expect(trimUrlCandidate("https://example.com/foo).")).toBe("https://example.com/foo");
    expect(trimUrlCandidate("https://en.wikipedia.org/wiki/Foo_(bar)")).toBe(
      "https://en.wikipedia.org/wiki/Foo_(bar)",
    );
  });

  it("returns null for something that is not a URL at all", () => {
    expect(trimUrlCandidate("https://.")).toBeNull();
    expect(trimUrlCandidate("not a url")).toBeNull();
  });
});
