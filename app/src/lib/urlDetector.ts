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
 */

const ANSI_RE =
  /\x1b(?:\[[0-9;?]*[A-Za-z]|\][^\x07\x1b]*(?:\x07|\x1b\\)?|[()#][A-Za-z0-9]|.)/g;

const MAX_BUFFER = 8 * 1024; // 8 KB rolling buffer cap
const DEBOUNCE_MS = 300;
const CONFIRM_MS = 500; // extra wait when URL reaches end of buffer
const MIN_URL_LENGTH = 100;

export type UrlCallback = (url: string) => void;

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
  const lines = clean.split(/\r?\n/);
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

  constructor(callback: UrlCallback, columns: ColumnsGetter) {
    this.callback = callback;
    this.columns = columns;
  }

  /** Feed raw PTY output chunks. */
  feed(data: Uint8Array): void {
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
    // 1. Strip ANSI escape sequences
    const clean = this.buffer.replace(ANSI_RE, "");

    // 2. Flatten the buffer: rejoin hard wraps, terminate on everything else.
    const flat = flatten(clean, this.columns());

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
        this.callback(url);
      }
    }

    // Scan finished without a URL at the buffer end.
    // If we had a pending URL from a previous scan, it's now confirmed complete.
    if (this.pendingUrl) {
      this.emitPending();
    }
  }

  private emitPending(): void {
    if (this.pendingUrl && this.pendingUrl !== this.lastEmitted) {
      this.lastEmitted = this.pendingUrl;
      this.callback(this.pendingUrl);
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
