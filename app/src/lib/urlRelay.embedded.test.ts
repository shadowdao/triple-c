import { describe, it, expect } from "vitest";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { sanitizeRelayUrl, MAX_RELAY_URL_LENGTH } from "./urlRelay";

/**
 * The web terminal (`src-tauri/src/web_terminal/terminal.html`) is embedded
 * into the Rust binary with `include_str!()` and served as one standalone
 * file, so it cannot import `urlRelay.ts`. It therefore carries a hand-copied
 * duplicate of `sanitizeRelayUrl` — and a hand-copied security check that no
 * test can reach is a check that quietly rots.
 *
 * This test reaches it: it pulls the marked block straight out of the HTML,
 * evaluates it, and asserts it agrees with the TypeScript original on every
 * case. Divergence fails here rather than shipping.
 */

// Vitest runs with `app/` as its root; `import.meta.url` is an http URL under
// the jsdom environment, so resolve from the working directory instead.
const HTML_PATH = resolve(
  process.cwd(),
  "src-tauri/src/web_terminal/terminal.html",
);

const START_MARKER = "─── shared-url-sanitizer ";
const END_MARKER = "─── end shared-url-sanitizer ";

/** Extract and evaluate the embedded copy. */
function loadEmbeddedSanitizer(): (raw: unknown) => string | null {
  const html = readFileSync(HTML_PATH, "utf8");

  const start = html.indexOf(START_MARKER);
  const end = html.indexOf(END_MARKER);
  if (start === -1 || end === -1 || end < start) {
    throw new Error(
      `Could not find the shared-url-sanitizer markers in ${HTML_PATH}. ` +
        "If the block was renamed or removed, update this test — do not delete it.",
    );
  }

  const block = html.slice(html.indexOf("\n", start) + 1, end);
  if (!block.includes("function sanitizeRelayUrl(")) {
    throw new Error(
      "The shared-url-sanitizer block no longer defines sanitizeRelayUrl().",
    );
  }

  // `RELAY_MAX_URL` is declared elsewhere in the page; supply it here with the
  // same value the TypeScript module uses, which is also what the page sets.
  const factory = new Function(
    "RELAY_MAX_URL",
    `${block}\nreturn sanitizeRelayUrl;`,
  );
  return factory(MAX_RELAY_URL_LENGTH) as (raw: unknown) => string | null;
}

const embeddedSanitize = loadEmbeddedSanitizer();

/**
 * Every case both copies must agree on. Deliberately the union of the two
 * threat models, not the easy half.
 */
const CASES: unknown[] = [
  // Accepted.
  "https://example.com/",
  "http://example.com/x",
  "https://EXAMPLE.com",
  "https://my-host.example.com/a-b_c~d/e.f?g=h-i#j-k",
  "http://127.0.0.1:41703/callback?code=abc",
  "  https://example.com/padded  ",
  "https://example.com/x\n",
  "https://claude.ai/oauth/authorize?code=true&client_id=abc",

  // Scheme.
  "javascript:alert(1)",
  "JavaScript:alert(1)",
  "data:text/html,<script>alert(1)</script>",
  "file:///etc/passwd",
  "vscode://x",
  "java\nscript:alert(1)",

  // Malformed / hostile.
  "",
  "   ",
  "example.com",
  "https://",
  "https:///etc/passwd",
  "https://user:pass@example.com/",
  "https://claude.ai@evil.tld/oauth/authorize",
  "https://example.com/a b",
  "https://example.com/a\r\nb",
  "https://example.com/\u001b]0;pwned\u0007",
  "https://example.com/a\u0000b",
  "https://example.com/a\u007fb",
  "https://example.com/a\u0085b",
  "https://example.com/a\u00a0b",
  'https://example.com/a"b',
  "https://example.com/a'b",
  "https://example.com/a`b",
  `https://example.com/${"a".repeat(MAX_RELAY_URL_LENGTH)}`,

  // Non-strings.
  null,
  undefined,
  42,
  {},
];

describe("terminal.html's embedded sanitizer", () => {
  it("is present and extractable", () => {
    expect(typeof embeddedSanitize).toBe("function");
  });

  it("agrees with lib/urlRelay.ts on every case", () => {
    for (const input of CASES) {
      expect(
        embeddedSanitize(input),
        `embedded copy disagrees for input: ${JSON.stringify(input)?.slice(0, 120)}`,
      ).toEqual(sanitizeRelayUrl(input));
    }
  });

  it("rejects the userinfo spoof that reads as an Anthropic origin", () => {
    expect(embeddedSanitize("https://claude.ai@evil.tld/oauth/authorize")).toBeNull();
  });

  it("rejects quote characters, which the OS opener may treat as syntax", () => {
    expect(embeddedSanitize('https://example.com/a"b')).toBeNull();
    expect(embeddedSanitize("https://example.com/a`b")).toBeNull();
  });
});
