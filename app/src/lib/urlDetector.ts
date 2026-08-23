/**
 * Detects long URLs that span multiple hard-wrapped lines in PTY output.
 *
 * The Linux PTY hard-wraps long lines with \r\n at the terminal column width,
 * which breaks xterm.js WebLinksAddon URL detection. This class flattens
 * the buffer (rejoining hard wraps, treating every other break as a
 * terminator) and matches URLs with a single regex, firing a callback for ones
 * >= 100 chars.
 *
 * ## Which line breaks may be deleted
 *
 * Only the ones the *terminal* inserted. A hard wrap happens at exactly the
 * column width, so a line that reached the width was cut mid-token and its
 * break must be removed to put the token back together; a line that stopped
 * short ended for its own reasons and its break is a real separator.
 *
 * Deleting every break instead — which this did — glues unrelated output onto
 * the end of a URL. Observed for real: a wrapped paragraph following a link
 * became `…/tag/preview-63f3c54Butitprovesyournitpick…`, because a terminal
 * that wraps at a space emits the break *instead of* the space, so removing
 * the break removes the separator too. That candidate is a different URL from
 * the one on screen, and the user is the one who has to notice.
 *
 * When a URL match extends to the end of the flattened buffer, emission is
 * deferred (more chunks may still be arriving). A confirmation timer emits
 * the pending URL if no further data arrives within 500 ms.
 *
 * ## OSC 8 comes first, and the scraping is the fallback
 *
 * Everything above is guesswork over what a terminal *painted*. When the
 * program emits an **OSC 8 hyperlink** there is no guesswork to do: the
 * complete URL is in the sequence's parameter, contiguous and exact, however
 * the visible text was sliced.
 *
 * That distinction is the whole reason this file grew a second branch. Claude
 * Code prints its sign-in link as an OSC 8 hyperlink whose *visible* text is
 * cut into terminal-width pieces on separate lines — measured against 2.1.226,
 * a 346-character URL arrives as five emissions, each carrying the whole URL in
 * its parameter and 80 characters of it on screen. `ANSI_RE` strips OSC
 * sequences wholesale, so the scraper never saw the parameter and reassembled
 * the visible pieces instead: a URL that parses, that points at claude.com, and
 * that cannot authorise anything. The backend hit this first and solved it the
 * same way — see `commands/auth_token_commands.rs`, whose `osc8_target` and
 * `usable_sign_in_link` this mirrors.
 *
 * So each emitted candidate is tagged with where it came from, and the consumer
 * refuses to let a `heuristic` candidate displace an `osc8` one.
 */

const ANSI_RE =
  /\x1b(?:\[[0-9;?]*[A-Za-z]|\][^\x07\x1b]*(?:\x07|\x1b\\)?|[()#][A-Za-z0-9]|.)/g;

/**
 * OSC 8 hyperlink: `ESC ] 8 ; <params> ; <uri> (BEL | ESC \\)`.
 *
 * The params field is `key=value` pairs separated by `:`, never `;`, so the
 * first `;` after the `8;` ends it — the same split `osc8_target` makes in
 * Rust. The closing half of a hyperlink is `8;;` with an empty uri.
 */
// eslint-disable-next-line no-control-regex
const OSC8_RE = /\x1b\]8;([^;\x07\x1b]*);([^\x07\x1b]*)(?:\x07|\x1b\\)/g;

const MAX_BUFFER = 8 * 1024; // 8 KB rolling buffer cap
const DEBOUNCE_MS = 300;
const CONFIRM_MS = 500; // extra wait when URL reaches end of buffer
const MIN_URL_LENGTH = 100;

/** Mirrors `MAX_LINK_LENGTH` in `commands/auth_token_commands.rs`. */
const MAX_LINK_LENGTH = 8192;

/** Bound on remembered OSC 8 targets, so a program printing a fresh hyperlink
 *  every frame cannot grow this without limit. */
const MAX_REMEMBERED_LINKS = 32;

/**
 * Where a candidate came from, which is the same thing as how much it can be
 * trusted to be *complete*.
 *
 * `osc8` is lifted verbatim out of a hyperlink parameter; `heuristic` was
 * reassembled from painted text and may be a truncated guess. The consumer
 * uses this to decide precedence — see `promptUrl` in `TerminalView.tsx`.
 */
export type UrlSource = "osc8" | "heuristic";

export type UrlCallback = (url: string, source: UrlSource) => void;

/**
 * Whether an OSC 8 target is worth offering as a candidate at all.
 *
 * A direct port of `usable_sign_in_link` in
 * `commands/auth_token_commands.rs`, and deliberately just as shallow: this is
 * a junk filter, not the security decision. `sanitizeRelayUrl` is still the
 * only thing standing between any of this and `openUrl`, and duplicating its
 * rules here would be a second place for them to go stale.
 *
 * The one rule from the Rust that is not ported is its `sk-ant-` check: that
 * exists because the backend's link path bypasses `SecretRedactor`, and there
 * is no redactor on this side to bypass.
 */
export function usableLink(uri: string): boolean {
  if (!uri.startsWith("https://") && !uri.startsWith("http://")) return false;
  if (uri.length > MAX_LINK_LENGTH) return false;
  // Printable ASCII only. Control characters and whitespace are exactly how a
  // URL is smuggled past a display, and `new URL()` strips some of them
  // silently; a real authorize URL is percent-encoded anyway.
  for (let i = 0; i < uri.length; i++) {
    const code = uri.charCodeAt(i);
    if (code < 0x21 || code > 0x7e) return false;
  }
  return true;
}

/**
 * Every usable OSC 8 hyperlink target in `raw`, in the order they were emitted.
 *
 * Takes the *unstripped* stream: `ANSI_RE` deletes OSC sequences wholesale, so
 * by the time the buffer is clean the parameter this reads is already gone.
 */
export function osc8Targets(raw: string): string[] {
  const out: string[] = [];
  OSC8_RE.lastIndex = 0;
  let m: RegExpExecArray | null;
  while ((m = OSC8_RE.exec(raw)) !== null) {
    const uri = m[2];
    // `8;;` closes a hyperlink and carries no target.
    if (uri && usableLink(uri)) out.push(uri);
  }
  return out;
}

/**
 * How wide the terminal is right now.
 *
 * A getter, not a number: the width changes with every window resize, and a
 * stale one silently turns joining back into guesswork.
 */
export type ColumnsGetter = () => number;

/**
 * Rejoin the line breaks the terminal inserted; turn the rest into spaces.
 *
 * A line of exactly `columns` visible characters was cut by the terminal, so
 * its break is deleted and the two halves are put back together. Anything
 * shorter ended on its own and becomes a space — a URL cannot contain one, so
 * that is also what stops a match running into whatever followed.
 *
 * `columns` of 0 or less means "not known yet"; nothing is rejoined, which
 * costs a wrapped URL rather than inventing one.
 *
 * One case stays ambiguous and cannot be resolved here: a token that happens to
 * end exactly at the width is indistinguishable from one the terminal cut, so
 * the following line is joined to it. The candidate is still shown in full and
 * confirmed by the user before anything opens.
 */
export function flatten(clean: string, columns: number): string {
  // A bare `\r` is a break too. A TUI repaints a frame by returning to column
  // 0 without a line feed, so splitting on `\r?\n` alone leaves a whole
  // frame's worth of text on one "line" — which is then far longer than
  // `columns`, so the `===` test below says "not wrapped" and a URL the
  // terminal really did cut is never rejoined. Splitting here is also what the
  // backend does with a lone CR (`strip_ansi_prefix`), for the same reason.
  const lines = clean.split(/\r\n|\r|\n/);
  let out = "";
  for (let i = 0; i < lines.length; i++) {
    out += lines[i];
    if (i === lines.length - 1) break;
    // `===`, not `>=`. A line *longer* than the width was never cut by the
    // terminal — the stream simply contained no break there, so the break that
    // follows it is the application's own and separates two things.
    const wrapped = columns > 0 && lines[i].length === columns;
    if (!wrapped) out += " ";
  }
  return out;
}

export class UrlDetector {
  private decoder = new TextDecoder();
  private buffer = "";
  private timer: ReturnType<typeof setTimeout> | null = null;
  private confirmTimer: ReturnType<typeof setTimeout> | null = null;
  private lastEmitted = "";
  private pendingUrl: string | null = null;
  private callback: UrlCallback;
  private columns: ColumnsGetter;
  /**
   * The width in effect when the buffered bytes were *printed*, sampled in
   * `feed`.
   *
   * Not read in `scan`, which is where it used to be read: the scan happens
   * 300 ms after the print, and a resize inside that window would reassemble
   * text wrapped at 80 columns using a width of 120 — every join decision
   * wrong, and a fabricated URL out the other end. `-1` means "nothing fed
   * yet".
   */
  private feedColumns = -1;
  /** OSC 8 targets already offered, so a hyperlink repainted every frame does
   *  not re-prompt. Bounded by {@link MAX_REMEMBERED_LINKS}. */
  private emittedLinks = new Set<string>();

  constructor(callback: UrlCallback, columns: ColumnsGetter) {
    this.callback = callback;
    this.columns = columns;
  }

  /** Feed raw PTY output chunks. */
  feed(data: Uint8Array): void {
    const columns = this.columns();
    if (this.feedColumns !== -1 && columns !== this.feedColumns) {
      // The buffered text was wrapped at a width that no longer applies, and
      // the new text will be wrapped at this one. There is no single width
      // that reassembles both, so the older half is dropped rather than
      // joined by a rule that is now wrong for it. Costs a URL that was
      // mid-print across a resize; never invents one.
      this.buffer = "";
      this.pendingUrl = null;
    }
    this.feedColumns = columns;

    this.buffer += this.decoder.decode(data, { stream: true });

    // Cap buffer to avoid unbounded growth
    if (this.buffer.length > MAX_BUFFER) {
      this.buffer = this.buffer.slice(-MAX_BUFFER);
    }

    // Cancel pending timers — new data arrived, rescan from scratch
    if (this.timer !== null) clearTimeout(this.timer);
    if (this.confirmTimer !== null) {
      clearTimeout(this.confirmTimer);
      this.confirmTimer = null;
    }

    // Debounce — scan after 300 ms of silence
    this.timer = setTimeout(() => {
      this.timer = null;
      this.scan();
    }, DEBOUNCE_MS);
  }

  private scan(): void {
    // 0. The exact copy first. An OSC 8 parameter needs no reassembly, so
    //    anything found here beats whatever the steps below reconstruct — and
    //    it has to be read from the raw buffer, because step 1 deletes the
    //    sequence that carries it.
    this.scanLinks();

    // 1. Strip ANSI escape sequences
    const clean = this.buffer.replace(ANSI_RE, "");

    // 2. Flatten the buffer: rejoin hard wraps, terminate on everything else.
    //    The width is the one that was in effect when these bytes were
    //    printed, not the one the terminal happens to have now.
    const flat = flatten(clean, this.feedColumns);

    if (!flat) return;

    // 3. Match URLs on the flattened string — spans across wrapped lines naturally.
    //    The negated class stops at anything illegal in a URL, which must
    //    include the *whole* C0 range and DEL, not just BEL: an escape or a NUL
    //    swallowed into the middle of a match becomes a URL that renders as one
    //    thing in the toast and resolves as another. Everything emitted here is
    //    still re-validated by `sanitizeRelayUrl` before it can reach `openUrl`;
    //    stopping the match early only means the legitimate prefix survives
    //    instead of the whole candidate being thrown away.
    // eslint-disable-next-line no-control-regex
    const urlRe = /https?:\/\/[^\s'"`<>\x00-\x20\x7f]+/g;
    let m: RegExpExecArray | null;

    while ((m = urlRe.exec(flat)) !== null) {
      const url = m[0];

      // 4. Filter by length
      if (url.length < MIN_URL_LENGTH) continue;

      // 5. If the match extends to the very end of the flattened string,
      //    more chunks may still be arriving — defer emission.
      if (m.index + url.length >= flat.length) {
        this.pendingUrl = url;
        this.confirmTimer = setTimeout(() => {
          this.confirmTimer = null;
          this.emitPending();
        }, CONFIRM_MS);
        return;
      }

      // 6. URL is clearly complete (more content follows) — dedup + emit
      this.pendingUrl = null;
      if (url !== this.lastEmitted) {
        this.lastEmitted = url;
        this.callback(url, "heuristic");
      }
    }

    // Scan finished without a URL at the buffer end.
    // If we had a pending URL from a previous scan, it's now confirmed complete.
    if (this.pendingUrl) {
      this.emitPending();
    }
  }

  /**
   * Offer every OSC 8 target in the buffer that has not been offered before.
   *
   * `lastEmitted` is moved along with them so an identical string arriving on
   * the heuristic path a moment later is recognised as the same candidate
   * rather than fired a second time.
   */
  private scanLinks(): void {
    for (const uri of osc8Targets(this.buffer)) {
      if (uri.length < MIN_URL_LENGTH) continue;
      if (this.emittedLinks.has(uri)) continue;
      if (this.emittedLinks.size >= MAX_REMEMBERED_LINKS) {
        this.emittedLinks.clear();
      }
      this.emittedLinks.add(uri);
      this.lastEmitted = uri;
      this.callback(uri, "osc8");
    }
  }

  private emitPending(): void {
    if (this.pendingUrl && this.pendingUrl !== this.lastEmitted) {
      this.lastEmitted = this.pendingUrl;
      this.callback(this.pendingUrl, "heuristic");
    }
    this.pendingUrl = null;
  }

  dispose(): void {
    if (this.timer !== null) {
      clearTimeout(this.timer);
      this.timer = null;
    }
    if (this.confirmTimer !== null) {
      clearTimeout(this.confirmTimer);
      this.confirmTimer = null;
    }
  }
}
