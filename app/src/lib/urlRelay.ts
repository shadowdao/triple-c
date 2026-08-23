/**
 * URL relay — host side of `container/triple-c-open` — and the single URL
 * validator every `openUrl` call site in the app is required to go through.
 *
 * A CLI inside the container has no browser. When it wants to open a URL
 * (`gh auth login`, `aws sso login`, `gcloud auth login`, anything honouring
 * `$BROWSER` or shelling out to `xdg-open`), the container-side shim writes
 *
 *     ESC ] 7777 ; open ; <base64(url)> BEL
 *
 * to its controlling terminal. xterm.js routes that to an OSC 7777 handler,
 * which lands here.
 *
 * THE CONTAINER IS THE UNTRUSTED SIDE OF THIS BOUNDARY. Everything arriving
 * over the relay is attacker-controlled if the sandboxed agent misbehaves, so
 * this module is a validator first and a convenience second:
 *
 *  - only `http:` and `https:` survive — `file:`, `javascript:`, `data:` and
 *    every custom/registered URI handler are rejected. A container able to
 *    make the host open arbitrary schemes could reach local files, in-page
 *    script, or any protocol handler the OS has registered, which is a real
 *    escalation out of the sandbox.
 *  - embedded credentials (`https://user:pass@host`) are rejected: they are a
 *    display-spoofing vector in the confirmation toast and in the address bar.
 *  - control characters, whitespace and oversized payloads are rejected before
 *    parsing, so the relay can't be used to smuggle escape sequences or to
 *    push a megabyte of text into the UI.
 *  - the URL is returned in WHATWG-normalized form, so what the user is shown
 *    in the toast is exactly what gets opened.
 *
 * Opening is never automatic — see `RelayRateLimiter` and the confirmation
 * toast in TerminalView.
 *
 * The relay is not the only route from the container to the host's browser.
 * The heuristic long-URL detector (`urlDetector.ts`) and the `claude
 * setup-token` sign-in link (`useClaudeAuth.ts`) both scrape the same
 * untrusted PTY byte stream, so they use this validator too — with an added
 * host allowlist in the sign-in case, where exactly one origin is legitimate.
 * Keep this the only implementation: a second copy is a second place for a
 * rule to go missing.
 *
 * `web_terminal/terminal.html` is the one unavoidable duplicate — it is
 * embedded standalone via `include_str!()` and cannot import this module.
 * `urlRelay.embedded.test.ts` extracts that copy and runs it against the same
 * table of cases, so the two cannot drift silently.
 */

/** Private OSC identifier used by the relay. Chosen to avoid the numbers in
 *  common use (0-19, 22, 52, 104, 110-119, 133, 777, 1337). */
export const URL_RELAY_OSC = 7777;

/** Hard cap on a relayed URL. Real OAuth URLs run to a few hundred chars. */
export const MAX_RELAY_URL_LENGTH = 8192;

/**
 * Whether `candidate` contains a character that disqualifies it before it is
 * ever parsed.
 *
 * Whitespace and C0/DEL matter most: `new URL()` silently *strips* tab, LF and
 * CR, so `"java\nscript:alert(1)"` would otherwise parse as a `javascript:`
 * URL. Quote characters are rejected on top of that: `"`, `'` and a backtick
 * are all illegal in a URL per RFC 3986, and this string ends up as an
 * argument to an OS-level opener — a path that on Windows has historically
 * run through a command interpreter, where a quote ends the argument and
 * whatever follows is the next command. Nothing legitimate loses out; a URL
 * that really needs one carries it percent-encoded.
 *
 * Written as a scan rather than a regex literal so the C0 range is expressed
 * as code points and cannot be quietly mangled by an editing tool.
 */
function hasForbiddenChar(candidate: string): boolean {
  for (const ch of candidate) {
    const code = ch.codePointAt(0) ?? 0;
    // C0 controls, space, and DEL.
    if (code <= 0x20 || code === 0x7f) return true;
    // C1 controls — not stripped by `new URL()`, invisible in the toast.
    if (code >= 0x80 && code <= 0x9f) return true;
    if (ch === '"' || ch === "'" || ch === "`") return true;
    // Any other Unicode whitespace (NBSP, ideographic space, ...).
    if (ch.trim() === "") return true;
  }
  return false;
}

/**
 * Registrable domains the Anthropic sign-in flow may send the user to.
 *
 * `claude setup-token` prints a `claude.ai` authorize URL and redirects to
 * `platform.claude.com`; `anthropic.com` covers the console. Anything else in
 * the transcript is not a sign-in link, whatever it claims.
 */
export const ANTHROPIC_SIGN_IN_HOSTS = [
  "claude.ai",
  "claude.com",
  "anthropic.com",
] as const;

export interface SanitizeUrlOptions {
  /**
   * Registrable domains the URL's host must match — either exactly, or as a
   * subdomain (`platform.claude.com` matches `claude.com`). Omit to allow any
   * host: the relay deliberately does, because opening a third-party OAuth
   * page is the entire point of it.
   */
  allowHosts?: readonly string[];
}

/** True when `host` is `domain` itself or a subdomain of it. */
function hostMatches(host: string, domain: string): boolean {
  return host === domain || host.endsWith(`.${domain}`);
}

/**
 * Validate a URL that something untrusted asked the host to open.
 *
 * @returns the normalized URL, or `null` if it must not be opened.
 */
export function sanitizeRelayUrl(
  raw: unknown,
  options: SanitizeUrlOptions = {},
): string | null {
  if (typeof raw !== "string") return null;

  const candidate = raw.trim();
  if (candidate.length === 0) return null;
  if (candidate.length > MAX_RELAY_URL_LENGTH) return null;

  if (hasForbiddenChar(candidate)) return null;

  let parsed: URL;
  try {
    parsed = new URL(candidate);
  } catch {
    return null;
  }

  // Scheme allowlist. Nothing else, ever.
  if (parsed.protocol !== "http:" && parsed.protocol !== "https:") return null;

  // A special-scheme URL with no host is nonsense and, on some platforms,
  // resolves in surprising ways.
  if (parsed.hostname === "") return null;

  // Embedded credentials spoof the displayed origin: `https://claude.ai@evil.tld/x`
  // reads as claude.ai in anything that truncates, and navigates to evil.tld.
  if (parsed.username !== "" || parsed.password !== "") return null;

  if (options.allowHosts) {
    const host = parsed.hostname.toLowerCase();
    if (!options.allowHosts.some((domain) => hostMatches(host, domain))) {
      return null;
    }
  }

  const normalized = parsed.toString();
  if (normalized.length > MAX_RELAY_URL_LENGTH) return null;

  return normalized;
}

/**
 * Whether `next` is the same link as `current`, only longer.
 *
 * The single rule that lets a later candidate displace an earlier one when both
 * were scraped from the same untrusted stream. A repainting TUI lands a
 * truncated copy of a link in the transcript before the complete one, and this
 * is what joins them back up — safely, because a longer string sharing a prefix
 * with the current pick necessarily has the same scheme, host and port, so an
 * attacker cannot use it to move the origin.
 *
 * Longest-wins without the prefix test is what this replaced, and it handed the
 * choice to the attacker: pad a hostile URL and it displaces the real one.
 *
 * Used by `pickSignInUrl` (`hooks/useClaudeAuth.ts`) and by the terminal's URL
 * prompt slot (`components/terminal/TerminalView.tsx`). One implementation, on
 * purpose.
 */
export function extendsUrl(next: string, current: string): boolean {
  return next.length > current.length && next.startsWith(current);
}

/**
 * Whether this is a URL that signs the user in to Anthropic.
 *
 * Used to decide *presentation*, not permission — the toast makes the
 * container-side browser the default action for these, because the OAuth
 * callback listener is inside the container and the host has nothing to catch
 * it with. It is deliberately the same host allowlist the sign-in flow itself
 * uses, so the two cannot disagree about what a sign-in link is.
 */
export function isAnthropicSignInUrl(url: string): boolean {
  const safe = sanitizeRelayUrl(url, { allowHosts: ANTHROPIC_SIGN_IN_HOSTS });
  if (!safe) return false;
  return /oauth|authorize|login|sign-?in/i.test(safe);
}

/**
 * The origin of an already-sanitized URL, for display.
 *
 * The origin is the only part of a URL that decides where the user's
 * credentials end up, so it is the one part an ellipsis must never eat. Every
 * place that shows a URL the user is about to open shows this separately, at
 * full length, next to the truncatable remainder.
 *
 * Returns `null` for input that does not parse — callers pass
 * {@link sanitizeRelayUrl} output, so that would be a bug rather than an
 * attack.
 */
export function urlOrigin(url: string): string | null {
  try {
    return new URL(url).origin;
  } catch {
    return null;
  }
}

/**
 * Parse the payload of an OSC 7777 sequence (everything between `ESC]7777;`
 * and the terminator).
 *
 * Expected shape: `open;<base64(url)>`. The URL is base64-encoded so that a
 * `;`, a BEL or an ESC inside it cannot break out of the sequence.
 *
 * @returns the validated URL, or `null` if the payload is malformed or the
 *          URL fails {@link sanitizeRelayUrl}.
 */
export function parseUrlRelayOsc(data: string): string | null {
  if (typeof data !== "string") return null;

  const sep = data.indexOf(";");
  if (sep === -1) return null;

  const verb = data.slice(0, sep);
  if (verb !== "open") return null;

  const payload = data.slice(sep + 1);
  if (payload.length === 0) return null;
  // base64 of the length cap, plus slack for padding.
  if (payload.length > MAX_RELAY_URL_LENGTH * 2) return null;
  if (!/^[A-Za-z0-9+/]+=*$/.test(payload)) return null;

  let decoded: string;
  try {
    const binary = atob(payload);
    const bytes = Uint8Array.from(binary, (c) => c.charCodeAt(0));
    decoded = new TextDecoder("utf-8", { fatal: true }).decode(bytes);
  } catch {
    return null;
  }

  return sanitizeRelayUrl(decoded);
}

/**
 * Throttles relay requests so a runaway (or hostile) process in the container
 * can't bury the UI in prompts.
 *
 * Two limits: a sliding window on total requests, and a short dedup window so
 * a retry loop around a single URL produces one prompt rather than twenty.
 */
export class RelayRateLimiter {
  private readonly maxInWindow: number;
  private readonly windowMs: number;
  private readonly dedupeMs: number;
  private timestamps: number[] = [];
  private lastUrl: string | null = null;
  private lastUrlAt = 0;

  constructor(maxInWindow = 5, windowMs = 10_000, dedupeMs = 5_000) {
    this.maxInWindow = maxInWindow;
    this.windowMs = windowMs;
    this.dedupeMs = dedupeMs;
  }

  /** @returns true if this request should be surfaced to the user. */
  allow(url: string, now: number = Date.now()): boolean {
    if (url === this.lastUrl && now - this.lastUrlAt < this.dedupeMs) {
      this.lastUrlAt = now;
      return false;
    }

    this.timestamps = this.timestamps.filter((t) => now - t < this.windowMs);
    if (this.timestamps.length >= this.maxInWindow) return false;

    this.timestamps.push(now);
    this.lastUrl = url;
    this.lastUrlAt = now;
    return true;
  }
}
