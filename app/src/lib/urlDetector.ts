/**
 * Detects long URLs that span multiple hard-wrapped lines in PTY output.
 *
 * The Linux PTY hard-wraps long lines with \r\n at the terminal column width,
 * which breaks xterm.js WebLinksAddon URL detection. This class flattens
 * the buffer (stripping PTY wraps, converting blank lines to spaces) and
 * matches URLs with a single regex, firing a callback for ones >= 100 chars.
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

export class UrlDetector {
  private decoder = new TextDecoder();
  private buffer = "";
  private timer: ReturnType<typeof setTimeout> | null = null;
  private confirmTimer: ReturnType<typeof setTimeout> | null = null;
  private lastEmitted = "";
  private pendingUrl: string | null = null;
  private callback: UrlCallback;

  constructor(callback: UrlCallback) {
    this.callback = callback;
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

    // 2. Flatten the buffer:
    //    - Blank lines (2+ consecutive line breaks) → space (real paragraph break / URL terminator)
    //    - Remaining \r and \n → removed (PTY hard-wrap artifacts)
    const flat = clean
      .replace(/(\r?\n){2,}/g, " ")
      .replace(/[\r\n]/g, "");

    if (!flat) return;

    // 3. Match URLs on the flattened string — spans across wrapped lines naturally
    const urlRe = /https?:\/\/[^\s'"<>\x07]+/g;
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
