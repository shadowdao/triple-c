/**
 * Finds file paths in a line of terminal text.
 *
 * Pure: the xterm glue (`components/terminal/filePathLinkProvider.ts`) turns
 * buffer rows into a string and string offsets back into cells; this decides
 * what a path is. Deliberately conservative — a false link is an annoying
 * underline, a missed one is a copy-paste — so a token needs either a `/` or
 * a known extension, and never sits inside a URL.
 */

export interface FilePathMatch {
  /** Indices into the input; `end` exclusive. Covers path + suffix, not wrapping. */
  start: number;
  end: number;
  path: string;
  line?: number;
  col?: number;
  endLine?: number;
}

/** Extensions that make a slash-less token (`index.ts`, `notes.md`) a path. */
const KNOWN_EXTENSIONS = new Set([
  "md", "markdown", "txt", "rst", "json", "jsonc", "yaml", "yml", "toml", "ini", "cfg", "conf",
  "env", "lock", "js", "jsx", "mjs", "cjs", "ts", "tsx", "rs", "py", "rb", "go", "java", "kt",
  "c", "h", "cc", "cpp", "hpp", "cs", "php", "swift", "scala", "lua", "sh", "bash", "zsh",
  "fish", "ps1", "html", "htm", "xml", "svelte", "vue", "css", "scss", "sass", "less", "sql",
  "graphql", "proto", "diff", "patch", "csv", "tsv", "log", "svg", "png", "jpg", "jpeg", "gif",
  "webp",
]);

/** Extensionless names that are files by convention. */
const KNOWN_BASENAMES = new Set([
  "Makefile", "Dockerfile", "Rakefile", "Gemfile", "Procfile", "Vagrantfile", "LICENSE",
  "README", "CHANGELOG", "PKGBUILD",
]);

/**
 * A candidate token: path characters, optionally starting with `/`, `./`, `../`
 * or `.` (dotfile). Excludes the wrapping characters the surrounding markdown
 * leaves (`(`, `)`, `[`, `]`, backtick, quotes) and whitespace.
 */
const TOKEN = /(?:\.{1,2}\/|\/)?[A-Za-z0-9_.\-~+@]+(?:\/[A-Za-z0-9_.\-~+@]+)*\/?/g;
const URL_SCHEME = /[a-z][a-z0-9+.-]*:\/\//gi;
const LINE_SUFFIX = /^(?::(\d+)(?::(\d+))?(?:-(\d+))?|#L(\d+)(?:-L?(\d+))?)/;
const VERSION_LIKE = /^v?\d+(\.\d+)+$/;
const TRAILING_PUNCT = /[.,;:]+$/;

function isPathLike(token: string): boolean {
  if (VERSION_LIKE.test(token)) return false;
  const base = token.slice(token.lastIndexOf("/") + 1);
  if (base === "" || base === "." || base === "..") return false;
  if (KNOWN_BASENAMES.has(base)) return true;

  const hasSlash = token.includes("/");
  const dot = base.lastIndexOf(".");

  if (dot === 0) {
    // Dotfile (.gitignore, .env). With a slash the name itself counts as
    // "having an extension"; without one it must be a known dotfile.
    if (hasSlash) return true;
    return KNOWN_EXTENSIONS.has(base.slice(1).toLowerCase()) || base === ".gitignore" || base === ".env";
  }
  if (dot < 0) return false; // no extension at all — never a path
  // A real extension. With a slash any extension will do; without one it
  // must be a known source/doc extension.
  if (hasSlash) return true;
  return KNOWN_EXTENSIONS.has(base.slice(dot + 1).toLowerCase());
}

function urlSpans(text: string): Array<[number, number]> {
  const spans: Array<[number, number]> = [];
  for (const m of text.matchAll(URL_SCHEME)) {
    const start = m.index ?? 0;
    // A URL runs to the next whitespace or closing bracket/quote.
    const rest = text.slice(start);
    const len = rest.search(/[\s)\]'"`>]/);
    spans.push([start, len < 0 ? text.length : start + len]);
  }
  return spans;
}

export function findFilePathLinks(text: string): FilePathMatch[] {
  const urls = urlSpans(text);
  const insideUrl = (i: number) => urls.some(([s, e]) => i >= s && i < e);
  const out: FilePathMatch[] = [];

  for (const m of text.matchAll(TOKEN)) {
    const start = m.index ?? 0;
    let token = m[0];
    if (insideUrl(start)) continue;

    // Trailing sentence punctuation is not part of the name.
    const trimmed = token.replace(TRAILING_PUNCT, "");
    if (trimmed !== token) token = trimmed;
    if (token.endsWith("/")) token = token.slice(0, -1);
    if (!token || !isPathLike(token)) continue;

    let end = start + token.length;
    // `line`/`col`/`endLine` are set explicitly to `undefined` (rather than
    // left absent) so callers that assert on them with `toMatchObject` see
    // the key, not a missing property.
    const match: FilePathMatch = { start, end, path: token, line: undefined, col: undefined, endLine: undefined };

    // The suffix sits right after the *trimmed* token: `TOKEN` may have
    // consumed a trailing `.` that `TRAILING_PUNCT` then removed, so search
    // from `start + token.length`, not from the end of the raw match.
    const after = text.slice(start + token.length);
    const s = LINE_SUFFIX.exec(after);
    if (s) {
      if (s[1] !== undefined) {
        match.line = Number(s[1]);
        if (s[2] !== undefined) match.col = Number(s[2]);
        if (s[3] !== undefined) match.endLine = Number(s[3]);
      } else if (s[4] !== undefined) {
        match.line = Number(s[4]);
        if (s[5] !== undefined) match.endLine = Number(s[5]);
      }
      end += s[0].length;
      match.end = end;
    }
    out.push(match);
  }
  return out;
}
