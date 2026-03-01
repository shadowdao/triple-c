/**
 * Detects long URLs that span multiple hard-wrapped lines in PTY output.
 *
 * The Linux PTY hard-wraps long lines with \r\n at the terminal column width,
 * which breaks xterm.js WebLinksAddon URL detection. This class reassembles
 * those wrapped URLs and fires a callback for ones >= 100 chars.
 *
 * Two-phase approach: when a URL candidate extends to the end of the buffer,
 * emission is deferred (the rest of the URL may arrive in the next PTY chunk).
 * A confirmation timer emits the pending URL if no further data arrives.
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
    const clean = this.buffer.replace(ANSI_RE, "");
    const lines = clean.replace(/\r\n/g, "\n").replace(/\r/g, "\n").split("\n");

    // Remove trailing empty elements (artifacts of trailing \n from split)
    while (lines.length > 0 && lines[lines.length - 1] === "") {
      lines.pop();
    }

    if (lines.length === 0) return;

    for (let i = 0; i < lines.length; i++) {
      const match = lines[i].match(/https?:\/\/[^\s'"]+/);
      if (!match) continue;

      // Start with the URL fragment found on this line
      let url = match[0];
      let lastLineIndex = i;

      // Concatenate subsequent continuation lines (non-empty, no spaces, no leading whitespace)
      for (let j = i + 1; j < lines.length; j++) {
        const next = lines[j];
        if (!next || next.startsWith(" ") || next.includes(" ")) break;
        url += next;
        lastLineIndex = j;
        i = j; // skip this line in the outer loop
      }

      if (url.length < MIN_URL_LENGTH) continue;

      // If the URL reaches the last line of the buffer, the rest may still
      // be arriving in the next PTY chunk — defer emission.
      if (lastLineIndex >= lines.length - 1) {
        this.pendingUrl = url;
        this.confirmTimer = setTimeout(() => {
          this.confirmTimer = null;
          this.emitPending();
        }, CONFIRM_MS);
        return;
      }

      // URL is clearly complete (more content follows it in the buffer)
      this.pendingUrl = null;
      if (url !== this.lastEmitted) {
        this.lastEmitted = url;
        this.callback(url);
      }
    }

    // Scan finished without finding a URL reaching the buffer end.
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
