/**
 * Where a plain-text URL in terminal output starts and ends.
 *
 * The old pattern, `https?:\/\/[^\s'"`<>\x00-\x20\x7f]+`, ran to the next
 * whitespace, so the prose around a link came with it: `See https://x/docs.`
 * underlined and opened `https://x/docs.`, `(https://x/a)` opened
 * `https://x/a)`, and a markdown link `[t](https://x/a)` the same. This uses
 * the rules GitHub's autolinker and linkify apply:
 *
 * - A URL never *ends* in `.` `,` `;` `:` `!` `?` `*` — those are sentence
 *   punctuation (and markdown emphasis) when they come last. In the middle
 *   (`a.b`, `a,b`, `?q=`) they are kept.
 * - A closing `)` `]` `}` is kept only when it closes a bracket opened inside
 *   the URL — `https://en.wikipedia.org/wiki/Foo_(bar)` keeps its paren,
 *   `(https://x/a)` loses the one that wraps it. A closer followed by more URL
 *   (`/a)b`) is kept either way, since it cannot be wrapping anything.
 * - `-` `_` `~` `=` `/` are never trimmed: OAuth `state` and
 *   `code_challenge` are base64url, which can end in any of the first two,
 *   and `=` is base64 padding.
 *
 * It is a single regex rather than a match-then-trim because `WebLinksAddon`
 * underlines exactly what its regex matches: trimming only the URL handed to
 * the click handler would still paint the `.` as part of the link. The
 * bracket rule is one level deep, which covers every real case (Wikipedia
 * disambiguations, IPv6 hosts, `a[]=` query keys) without needing a parser.
 *
 * The set of characters a URL may contain at all is unchanged — whitespace,
 * quotes, backtick, `<` `>`, the whole C0 range and DEL still end a match —
 * and nothing here is a security decision: `sanitizeRelayUrl` still decides
 * what may be opened.
 */

/** Any character a URL may contain, minus brackets (handled separately). */
const C = "[^\\s'\"`<>()\\[\\]{}\\x00-\\x20\\x7f]";
/** A character a URL may end with: `C` minus trailing punctuation. */
const END_CHAR = "[^\\s'\"`<>()\\[\\]{}\\x00-\\x20\\x7f.,;:!?*]";
/** A one-level balanced bracket group: `(bar)`, `[::1]`, `[]`, `{id}`. */
const GROUP = `\\(${C}*\\)|\\[${C}*\\]|\\{${C}*\\}`;
/** An opener that starts no balanced group (`/a(b`), so it is plain text. */
const LONE_OPEN = `\\((?!${C}*\\))|\\[(?!${C}*\\])|\\{(?!${C}*\\})`;
/** A closer, allowed mid-URL; the final position excludes it unless balanced. */
const CLOSE = "[)\\]}]";

const URL_SOURCE = `https?:\\/\\/(?:${GROUP}|${C}|${LONE_OPEN}|${CLOSE})*(?:${GROUP}|${END_CHAR})`;

/**
 * The regex handed to `WebLinksAddon` as `urlRegex`.
 *
 * No flags: `LinkComputer.computeLink` rebuilds it with `g` appended to
 * whatever flags it has, so giving it `g` here would be redundant and `y`
 * would break the scan.
 */
export const TERMINAL_URL_REGEX = new RegExp(URL_SOURCE);

const ANCHORED = new RegExp(`^(?:${URL_SOURCE})`);

/**
 * The maximal run of URL characters starting at a scheme — the old pattern.
 * Still useful for knowing how far the raw text ran, e.g. whether it reached
 * the end of a buffer that may still be growing.
 */
// eslint-disable-next-line no-control-regex
const RAW_RUN = /https?:\/\/[^\s'"`<>\x00-\x20\x7f]+/g;

/**
 * The URL at the start of `candidate`, with trailing punctuation and
 * unbalanced closers removed; `null` if nothing URL-shaped is left.
 */
export function trimUrlCandidate(candidate: string): string | null {
  const m = ANCHORED.exec(candidate);
  return m ? m[0] : null;
}

export interface UrlMatch {
  /** The URL as it should be shown and opened. */
  url: string;
  /** Where it starts in the input. */
  index: number;
  /** Where the raw run of URL characters ended, trailing punctuation included. */
  rawEnd: number;
}

/** Every URL in `text`, trimmed, with the extent of the raw run it came from. */
export function findUrls(text: string): UrlMatch[] {
  const out: UrlMatch[] = [];
  RAW_RUN.lastIndex = 0;
  let m: RegExpExecArray | null;
  while ((m = RAW_RUN.exec(text)) !== null) {
    const url = trimUrlCandidate(m[0]);
    if (url) out.push({ url, index: m.index, rawEnd: m.index + m[0].length });
  }
  return out;
}
