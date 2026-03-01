/**
 * Detects long URLs that span multiple hard-wrapped lines in PTY output.
 *
 * The Linux PTY hard-wraps long lines with \r\n at the terminal column width,
 * which breaks xterm.js WebLinksAddon URL detection. This class reassembles
 * those wrapped URLs and fires a callback for ones >= 100 chars.
 */

const ANSI_RE =
  /\x1b(?:\[[0-9;?]*[A-Za-z]|\][^\x07\x1b]*(?:\x07|\x1b\\)?|[()#][A-Za-z0-9]|.)/g;

const MAX_BUFFER = 8 * 1024; // 8 KB rolling buffer cap
const DEBOUNCE_MS = 300;
const MIN_URL_LENGTH = 100;

export type UrlCallback = (url: string) => void;

export class UrlDetector {
  private decoder = new TextDecoder();
  private buffer = "";
  private timer: ReturnType<typeof setTimeout> | null = null;
  private lastEmitted = "";
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

    // Debounce — scan after 300 ms of silence
    if (this.timer !== null) clearTimeout(this.timer);
    this.timer = setTimeout(() => {
      this.timer = null;
      this.scan();
    }, DEBOUNCE_MS);
  }

  private scan(): void {
    const clean = this.buffer.replace(ANSI_RE, "");
    const lines = clean.replace(/\r\n/g, "\n").replace(/\r/g, "\n").split("\n");

    for (let i = 0; i < lines.length; i++) {
      const match = lines[i].match(/https?:\/\/[^\s'"]+/);
      if (!match) continue;

      // Start with the URL fragment found on this line
      let url = match[0];

      // Concatenate subsequent continuation lines (non-empty, no spaces, no leading whitespace)
      for (let j = i + 1; j < lines.length; j++) {
        const next = lines[j];
        if (!next || next.startsWith(" ") || next.includes(" ")) break;
        url += next;
        i = j; // skip this line in the outer loop
      }

      if (url.length >= MIN_URL_LENGTH && url !== this.lastEmitted) {
        this.lastEmitted = url;
        this.callback(url);
      }
    }
  }

  dispose(): void {
    if (this.timer !== null) {
      clearTimeout(this.timer);
      this.timer = null;
    }
  }
}
