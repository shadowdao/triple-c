//! Shared Claude Code authentication — one long-lived token for every project.
//!
//! ## Why
//!
//! Without this, every container is its own authentication island: each one
//! needs `claude login`, each one opens a browser flow, each one stores its own
//! credential in its own config volume. `claude setup-token` mints a single
//! ~1-year OAuth token that Claude Code accepts via `CLAUDE_CODE_OAUTH_TOKEN`,
//! so one authentication event can cover the whole fleet.
//!
//! ## How the token is obtained
//!
//! Observed directly against Claude Code 2.1.226, because the flow is not what
//! the design assumed. `claude setup-token` prints an authorization URL whose
//! `redirect_uri` is **Anthropic-hosted**
//! (`https://platform.claude.com/oauth/code/callback`) — it does *not* start a
//! loopback listener. After signing in, the user copies a code off that page
//! and the CLI waits at a `Paste code here if prompted >` prompt on **stdin**.
//! It then prints the token.
//!
//! Two consequences:
//!
//! * The flow needs a way to deliver the pasted code, hence
//!   [`submit_claude_token_code`] and the stdin channel below. Without it the
//!   command would simply sit at the prompt until it timed out.
//!
//!   And the code can be *refused*. On a bad paste the CLI prints
//!   `OAuth error: Invalid code…` / `Press Enter to retry.` and then blocks on
//!   stdin waiting for that Enter — it does **not** exit. Nothing recognised
//!   that, so a rejected code used to wedge the flow until [`SETUP_TIMEOUT`]
//!   with the UI still claiming it was finishing. [`detect_code_rejection`]
//!   spots it, [`CODE_REJECTED_EVENT`] tells the user, and the Enter is sent
//!   for them so their next code has a prompt to land in — bounded by
//!   [`MAX_CODE_ATTEMPTS`], because a retry loop nobody can win is just the
//!   same hang with more steps.
//!
//! * The URL is emitted as an **OSC 8 hyperlink**, and the visible text of
//!   that hyperlink is sliced by the CLI into terminal-width pieces on
//!   separate lines. Measured against 2.1.226: the URL is 346 characters, and
//!   at 80 columns it arrives as five separate hyperlink emissions, each
//!   carrying the *complete* URL in its OSC 8 parameter and 80 characters of
//!   it as visible text. Scraping the visible text therefore yields a
//!   truncated URL that still parses, still points at claude.com, and still
//!   fails to authorise — the worst possible shape of wrong. The parameter is
//!   contiguous and authoritative, so [`AnsiStripper`] surfaces it and
//!   [`LINK_EVENT`] carries it to the UI, which allowlists it before it is
//!   shown or opened.
//! * [`crate::auth_bridge`] is **not involved**. There is no container-local
//!   listener to reach, so there is nothing for it to bridge.
//!
//!   An earlier version turned the bridge on for the duration "in case a
//!   future CLI version goes back to a loopback redirect", and turned it off
//!   again afterwards. That was wrong twice over. The bridge flag is
//!   *persisted* to `projects.json`, and the restore only ran if the future
//!   completed — so a force-quit, kill or panic inside the 15-minute
//!   [`SETUP_TIMEOUT`] left it latched on, and the app re-armed an
//!   unauthenticated loopback port mirror on every subsequent launch, for a
//!   project whose owner had never opted in. And it bought nothing: the
//!   speculative future it defended against would need code changes here
//!   anyway. The bridge remains available as the per-project setting it always
//!   was; this command does not touch it.
//!
//! ## Handling of the token itself
//!
//! The token never reaches the frontend. It is parsed out of the command's
//! output, written straight to the OS keychain, and from then on only
//! [`crate::docker::container`] reads it, to inject the env var. Everything
//! streamed to the UI passes through [`SecretRedactor`] first, and no command
//! here returns the token or accepts it as an argument.

use std::sync::OnceLock;
use std::time::Duration;

use futures_util::StreamExt;
use tauri::{AppHandle, Emitter, State};
use tokio::io::AsyncWriteExt;
use tokio::sync::{mpsc, oneshot, Mutex};

use crate::docker::container::is_container_running;
use crate::docker::exec::{create_attached_exec, wait_for_exec_exit, AttachedExec};
use crate::storage::secure;
use crate::AppState;

/// Milestones in the acquisition flow. Payload `{ project_id, message }`,
/// matching the `container-progress` convention.
const PROGRESS_EVENT: &str = "claude-token-progress";

/// Redacted output from `claude setup-token`, so the UI can show the user the
/// URL to visit. Payload `{ project_id, chunk }`.
const OUTPUT_EVENT: &str = "claude-token-output";

/// A sign-in URL lifted from an OSC 8 hyperlink parameter, which is the only
/// place the *whole* URL appears — see the module docs. Payload
/// `{ project_id, url }`.
///
/// This is a candidate, not a verdict: the payload is container output, so the
/// frontend re-parses it and applies the `ANTHROPIC_SIGN_IN_HOSTS` allowlist
/// before showing it and again before handing it to the OS opener.
const LINK_EVENT: &str = "claude-token-link";

/// The CLI refused the submitted code and is waiting to be handed another.
/// Payload `{ project_id, message, attempts_remaining }`.
const CODE_REJECTED_EVENT: &str = "claude-token-code-rejected";

/// How long to wait for the whole flow. Generous: the user has to switch to a
/// browser, sign in, and approve. Bounded so a wedged exec can't leak a task.
const SETUP_TIMEOUT: Duration = Duration::from_secs(15 * 60);

/// Documented shape of a `setup-token` credential.
const TOKEN_PREFIX: &str = "sk-ant-oat01-";

/// Minimum number of body characters after [`TOKEN_PREFIX`] for a match to be
/// believed. Real `setup-token` credentials run to ~90 body characters.
///
/// This must be close to that real length, not merely "longer than prose".
/// The earlier value of 32 accepted a *fragment* of a token — which is what a
/// line wrap produces if `stty cols 200` fails and the pty falls back to 80
/// columns, splitting the value across two lines. Two bad things follow from
/// accepting one:
///
///  * the fragment gets stored as if it were the credential, so every
///    container authenticates with a token that cannot work, and the failure
///    surfaces far from its cause;
///  * [`SecretRedactor`] masks the leading fragment because it carries the
///    `sk-ant-` marker, but the *tail* on the next line carries no marker and
///    is emitted to the UI in clear.
///
/// Set below the real length by enough margin to survive a modest change in
/// token format, and far above any fragment an 80-column wrap can produce (the
/// prefix alone eats 13 columns, so the longest possible first line fragment is
/// ~67). Rejecting a real-but-shorter token is a loud, recoverable failure —
/// "printed no recognisable token" — whereas accepting a fragment is silent.
const MIN_TOKEN_BODY: usize = 80;

/// Redaction is deliberately broader than extraction: anything shaped like an
/// Anthropic credential is masked on its way to the UI, not just `oat01` ones.
const SECRET_MARKER: &str = "sk-ant-";
const SECRET_PLACEHOLDER: &str = "sk-ant-<redacted>";
const MIN_SECRET_BODY: usize = 8;

/// Cap on how much text [`SecretRedactor`] will withhold waiting for a
/// candidate secret to end. Past this, it is not a token — release it (still
/// redacted) rather than swallow the UI's output.
const MAX_HOLDBACK: usize = 4096;

/// Cap on the retained transcript used for parsing. The token is printed at the
/// end, and a re-rendering TUI can repaint many times, so keeping the tail is
/// both sufficient and bounded.
const MAX_TRANSCRIPT: usize = 256 * 1024;

/// Characters that can appear in the body of an Anthropic credential.
fn is_token_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'-' || b == b'_'
}

/// How far into a line a break has to sit before it can be believed to be the
/// terminal's right margin rather than the end of a line of prose.
///
/// This is what makes reassembling a wrapped credential safe. Without it,
/// "join a credential-shaped run to whatever starts the next line" would happily
/// weld `sk-ant-oat01-…` in a usage example onto the first word below it and
/// manufacture a token-length string out of two unrelated things — silently
/// storing a credential that cannot work, which is the exact failure
/// [`MIN_TOKEN_BODY`] exists to prevent. A hard wrap, by contrast, always breaks
/// at the pty's width, and no pty this code can be handed is narrower than 40
/// columns (Docker's default is 80; [`SETUP_TOKEN_SCRIPT`] asks for 400).
const MIN_WRAP_COLUMN: usize = 40;

/// How many hard wraps one credential may be reassembled across. A ~103
/// character token needs one at 80 columns and two at 40; three is slack, and a
/// bound at all stops a pathological input walking the whole transcript.
const MAX_CREDENTIAL_WRAPS: usize = 3;

/// A credential-shaped run of characters, possibly spanning hard wraps.
struct CredentialRun {
    /// One past the last byte of the run, embedded line breaks included.
    end: usize,
    /// How many credential characters it holds, line breaks excluded.
    body_len: usize,
    /// The run ran into the end of the buffer, so more input could extend it.
    open: bool,
}

/// Walk a credential body forwards from `body_start`, stepping over the hard
/// line breaks a pty inserts when the value is wider than the terminal.
///
/// Wrapping is not hypothetical and it is not harmless. `stty cols` in
/// [`SETUP_TOKEN_SCRIPT`] fails *silently* (`2>/dev/null || true`), and an
/// 80-column fallback splits the ~103 character token across two lines. Before
/// this, that produced two bad outcomes at once: the parser saw only a
/// too-short fragment and the whole sign-in failed for no visible reason, while
/// [`redact_complete`] masked the first line — which carries the `sk-ant-`
/// marker — and printed the *second* line, the tail of a live credential,
/// straight to the UI. Both halves are handled here so the two can never
/// disagree about where a credential ends.
///
/// The column test is measured from the last line break *in the buffer given*.
/// For [`SecretRedactor`], text earlier on the same line may already have been
/// emitted and drained, so the measured column can be shorter than the true one
/// — which can only make the scan *refuse* a join it would otherwise make,
/// never invent one. In practice it does not bite: the redactor withholds from
/// the marker onwards, so the whole credential and any wrap inside it are
/// together in the buffer by the time this runs.
fn scan_credential_body(bytes: &[u8], body_start: usize) -> CredentialRun {
    let mut end = body_start;
    let mut body_len = 0usize;
    let mut wraps = 0usize;

    loop {
        while end < bytes.len() && is_token_byte(bytes[end]) {
            end += 1;
            body_len += 1;
        }

        // Ran out of input mid-run: the rest may be in the next chunk.
        if end >= bytes.len() {
            return CredentialRun { end, body_len, open: true };
        }
        if bytes[end] != b'\n' || wraps >= MAX_CREDENTIAL_WRAPS {
            return CredentialRun { end, body_len, open: false };
        }

        // A run already long enough to *be* a credential does not need
        // continuing, and continuing it is how the reassembly turns into a
        // fabrication machine: a repainting TUI prints `Your token: <token>\n`
        // over and over, so the character after the break is very often another
        // token character belonging to the next frame entirely. Stopping here
        // means the only runs ever joined are the ones too short to stand alone
        // — which is exactly what a wrap produces.
        //
        // The cost is a token wrapped at a width between ~93 and ~102 columns,
        // where the first line would already clear this bar. No pty in this flow
        // is that size (Docker gives 80, [`SETUP_TOKEN_SCRIPT`] asks for 400),
        // and the outcome there is the pre-existing loud failure, not a wrong
        // credential.
        if body_len >= MIN_TOKEN_BODY {
            return CredentialRun { end, body_len, open: false };
        }

        let line_start = bytes[..end]
            .iter()
            .rposition(|b| *b == b'\n')
            .map_or(0, |i| i + 1);
        if end - line_start < MIN_WRAP_COLUMN {
            return CredentialRun { end, body_len, open: false };
        }

        // The break is at a plausible margin, so whether this is a wrap turns on
        // what follows it. A continuation resumes in column 0 with more
        // credential characters; anything else — a blank line, an indent, prose
        // — ends the run.
        if end + 1 >= bytes.len() {
            return CredentialRun { end, body_len, open: true };
        }
        if !is_token_byte(bytes[end + 1]) {
            return CredentialRun { end, body_len, open: false };
        }

        wraps += 1;
        end += 1;
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Token extraction
// ─────────────────────────────────────────────────────────────────────────────

/// Pull the long-lived token out of `claude setup-token`'s output.
///
/// Strict by construction, because the alternative to failing is storing
/// garbage that silently breaks every container:
///   * the value must carry the documented `sk-ant-oat01-` prefix;
///   * the prefix must not be glued to the tail of a longer word;
///   * at least [`MIN_TOKEN_BODY`] token characters must follow it.
///
/// Those characters may be split across hard line breaks — see
/// [`scan_credential_body`] — and the breaks are removed from the value. The
/// length floor is applied to the *reassembled* body, so a fragment is still
/// never accepted on its own; reassembly only ever turns a failure into the
/// whole credential, never a fragment into a plausible one.
///
/// The **last** match wins. The command narrates before it succeeds, and a TUI
/// may repaint the same frame repeatedly, so earlier matches are either prose
/// or superseded repaints of the same value.
pub fn parse_setup_token(output: &str) -> Option<String> {
    let bytes = output.as_bytes();
    let mut found = None;
    let mut cursor = 0usize;

    while let Some(offset) = output[cursor..].find(TOKEN_PREFIX) {
        let start = cursor + offset;
        cursor = start + TOKEN_PREFIX.len();

        // `xsk-ant-oat01-…` is not a token, it is a substring of something else.
        if start > 0 && is_token_byte(bytes[start - 1]) {
            continue;
        }

        let run = scan_credential_body(bytes, start + TOKEN_PREFIX.len());
        if run.body_len < MIN_TOKEN_BODY {
            continue;
        }

        // `run.end` lands on a non-token byte or the end of the buffer, and
        // every non-token byte is either ASCII or a UTF-8 lead byte, so both
        // ends are char boundaries.
        found = Some(output[start..run.end].replace('\n', ""));
    }

    found
}

// ─────────────────────────────────────────────────────────────────────────────
// Redaction
// ─────────────────────────────────────────────────────────────────────────────

/// Mask every *complete* credential in `text`.
///
/// A credential wrapped across lines is masked as one span, line breaks
/// included, so the placeholder replaces the whole thing rather than leaving the
/// tail visible on the next line. That welds the two display lines together;
/// losing a line break in the transcript is a fair price for not printing half a
/// live credential to the UI.
fn redact_complete(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut copied = 0usize;
    let mut cursor = 0usize;

    while let Some(offset) = text[cursor..].find(SECRET_MARKER) {
        let start = cursor + offset;
        cursor = start + SECRET_MARKER.len();

        if start > 0 && is_token_byte(bytes[start - 1]) {
            continue;
        }
        let run = scan_credential_body(bytes, start + SECRET_MARKER.len());
        if run.body_len < MIN_SECRET_BODY {
            continue;
        }

        out.push_str(&text[copied..start]);
        out.push_str(SECRET_PLACEHOLDER);
        copied = run.end;
        cursor = run.end;
    }

    out.push_str(&text[copied..]);
    out
}

/// Where the tail that might still grow into a credential begins. Everything
/// before this index is safe to emit; everything from it must be withheld until
/// more input arrives. Returns `text.len()` when nothing needs withholding.
fn holdback_index(text: &str) -> usize {
    let bytes = text.as_bytes();

    // A credential already under way: the last marker whose body runs to the end
    // of what we have, so the next chunk could extend it. If the *last* marker
    // fails that test, no earlier one can pass it either — the disqualifying
    // character lies after them all. A body that stops at a hard wrap counts as
    // still open, because the continuation is what the next chunk will bring.
    if let Some(start) = text.rfind(SECRET_MARKER) {
        let clean_start = start == 0 || !is_token_byte(bytes[start - 1]);
        if clean_start && scan_credential_body(bytes, start + SECRET_MARKER.len()).open {
            return start;
        }
    }

    // Otherwise: a marker truncated mid-way by the chunk boundary.
    for len in (1..SECRET_MARKER.len()).rev() {
        if text.len() >= len && text.is_char_boundary(text.len() - len)
            && &text[text.len() - len..] == &SECRET_MARKER[..len]
        {
            return text.len() - len;
        }
    }

    text.len()
}

/// Masks credentials out of a stream, tolerating a secret split across chunk
/// boundaries by withholding any tail that could still turn into one.
#[derive(Default)]
struct SecretRedactor {
    pending: String,
}

impl SecretRedactor {
    /// Absorb `chunk` and return the text that is now safe to show.
    fn push(&mut self, chunk: &str) -> String {
        self.pending.push_str(chunk);

        let mut split = holdback_index(&self.pending);
        if self.pending.len() - split > MAX_HOLDBACK {
            split = self.pending.len();
        }

        let emit = redact_complete(&self.pending[..split]);
        self.pending.drain(..split);
        emit
    }

    /// Release whatever is still withheld. The stream is over, so a partial
    /// credential can no longer grow — but it is still redacted on the way out.
    fn flush(&mut self) -> String {
        let out = redact_complete(&self.pending);
        self.pending.clear();
        out
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Terminal control-sequence stripping
// ─────────────────────────────────────────────────────────────────────────────

/// Length in bytes of the UTF-8 character starting with `b`.
fn utf8_len(b: u8) -> usize {
    if b < 0x80 {
        1
    } else if b >> 5 == 0b110 {
        2
    } else if b >> 4 == 0b1110 {
        3
    } else if b >> 3 == 0b11110 {
        4
    } else {
        1
    }
}

/// CSI final bytes that move the cursor. Claude Code's TUI lays text out by
/// jumping to a column (`ESC [ 9 G`) instead of emitting spaces, so deleting
/// these outright would weld neighbouring words together — which at best
/// garbles the URL the user has to read, and at worst welds a preceding word
/// onto the token and makes the parser reject it. They become a space instead:
/// a separator can never fabricate or destroy a match.
const CURSOR_MOVE_FINALS: &[u8] = b"ABCDEFGHd";

/// Escape intermediates that introduce a **three**-byte sequence: `ESC`, the
/// intermediate, then one final byte.
///
/// Claude Code prefixes every repaint frame with `ESC ( B` (designate ASCII as
/// G0). Treating that as a two-byte escape — which is what "anything else is two
/// bytes" did — consumed `ESC (` and emitted the `B` as ordinary text. Mostly
/// that was a stray letter in the transcript; landing immediately before a
/// token it would have glued `B` onto `sk-ant-oat01-…` and made
/// [`parse_setup_token`] reject a perfectly good credential.
const ESCAPE_INTERMEDIATES: &[u8] = b"()*+-./#%";

/// Largest OSC 8 target this will carry. Real authorize URLs are ~350
/// characters (measured: 346 against 2.1.226); anything past a few kilobytes is
/// not a link, and the frontend's own relay cap is 8192.
const MAX_LINK_LENGTH: usize = 8192;

/// Cap on undrained OSC 8 targets, so a container printing hyperlinks in a loop
/// cannot grow [`AnsiStripper`] without bound. The caller drains after every
/// chunk, so reaching this means something pathological is happening.
const MAX_PENDING_LINKS: usize = 32;

/// Pull the link target out of an OSC 8 payload — everything between `ESC ]`
/// and the terminator.
///
/// Shape: `8;<params>;<uri>`, e.g. `8;id=1umaq0e;https://claude.com/…`. The
/// closing half of a hyperlink is `8;;` and so yields `None`, as does any other
/// OSC (window title, the URL relay's own OSC 7777, …).
fn osc8_target(payload: &[u8]) -> Option<String> {
    let payload = std::str::from_utf8(payload).ok()?;
    let rest = payload.strip_prefix("8;")?;
    let uri = &rest[rest.find(';')? + 1..];
    if uri.is_empty() || uri.len() > MAX_LINK_LENGTH {
        return None;
    }
    Some(uri.to_string())
}

/// Strip terminal control sequences from the front of `bytes`, stopping at the
/// first incomplete sequence or truncated character. Returns the clean text, any
/// OSC 8 link targets found, and how many bytes were consumed.
fn strip_ansi_prefix(bytes: &[u8]) -> (String, Vec<String>, usize) {
    let mut out = String::with_capacity(bytes.len());
    let mut links: Vec<String> = Vec::new();
    let mut i = 0usize;

    let consumed = 'scan: loop {
        if i >= bytes.len() {
            break 'scan i;
        }
        match bytes[i] {
            0x1b => {
                if i + 1 >= bytes.len() {
                    break 'scan i;
                }
                match bytes[i + 1] {
                    // CSI: parameter/intermediate bytes, then a final 0x40..=0x7e.
                    b'[' => {
                        let mut j = i + 2;
                        while j < bytes.len() && !(0x40..=0x7e).contains(&bytes[j]) {
                            j += 1;
                        }
                        if j >= bytes.len() {
                            break 'scan i;
                        }
                        if CURSOR_MOVE_FINALS.contains(&bytes[j]) {
                            out.push(' ');
                        }
                        i = j + 1;
                    }
                    // OSC: runs until BEL or ST (ESC \).
                    b']' => {
                        let payload_start = i + 2;
                        let mut j = payload_start;
                        let payload_end;
                        loop {
                            if j >= bytes.len() {
                                break 'scan i;
                            }
                            if bytes[j] == 0x07 {
                                payload_end = j;
                                j += 1;
                                break;
                            }
                            if bytes[j] == 0x1b {
                                if j + 1 >= bytes.len() {
                                    break 'scan i;
                                }
                                if bytes[j + 1] == b'\\' {
                                    payload_end = j;
                                    j += 2;
                                    break;
                                }
                            }
                            j += 1;
                        }
                        // The one part of an OSC worth keeping: the hyperlink
                        // target, which is the only contiguous copy of the
                        // sign-in URL the CLI emits.
                        if let Some(uri) = osc8_target(&bytes[payload_start..payload_end]) {
                            links.push(uri);
                        }
                        i = j;
                    }
                    // Three-byte escapes: charset designation and friends.
                    b if ESCAPE_INTERMEDIATES.contains(&b) => {
                        if i + 2 >= bytes.len() {
                            break 'scan i;
                        }
                        i += 3;
                    }
                    // Two-byte escapes (keypad mode, index, …).
                    _ => i += 2,
                }
            }
            // A repaint returns to column 0. Turn that into a line break so the
            // old frame's trailing text cannot be glued onto the new frame's
            // leading text — which could otherwise fabricate a "token". A run of
            // CRs immediately before a LF is just the pty's ONLCR translation,
            // so it collapses into that single LF rather than blank lines.
            b'\r' => {
                let mut j = i;
                while j < bytes.len() && bytes[j] == b'\r' {
                    j += 1;
                }
                if j >= bytes.len() {
                    break 'scan i;
                }
                if bytes[j] != b'\n' {
                    out.push('\n');
                }
                i = j;
            }
            b'\n' => {
                out.push('\n');
                i += 1;
            }
            b'\t' => {
                out.push('\t');
                i += 1;
            }
            0x00..=0x1f | 0x7f => i += 1,
            b => {
                let len = utf8_len(b);
                if i + len > bytes.len() {
                    break 'scan i;
                }
                if let Ok(s) = std::str::from_utf8(&bytes[i..i + len]) {
                    out.push_str(s);
                }
                i += len;
            }
        }
    };

    (out, links, consumed)
}

/// Cap on the bytes [`AnsiStripper`] will hold waiting for a control sequence
/// to terminate.
///
/// [`strip_ansi_prefix`] stops at the first *incomplete* sequence and the
/// remainder is carried to the next chunk — which is correct while the
/// sequence really is going to end, and unbounded when it never does. A single
/// `ESC ]` with no BEL and no ST swallows everything the container prints for
/// the rest of the 15-minute [`SETUP_TIMEOUT`], and the buffer grows with it;
/// `transcript` and [`SecretRedactor::pending`] are both already capped, so
/// this was the one remaining way to make the flow eat memory. Generous enough
/// that no legitimate sequence comes close (OSC 8 hyperlinks are the longest
/// thing Claude Code emits, in the low hundreds of bytes).
const MAX_ANSI_CARRY: usize = 64 * 1024;

/// Stateful wrapper around [`strip_ansi_prefix`] that carries an incomplete
/// trailing sequence over to the next chunk.
#[derive(Default)]
struct AnsiStripper {
    carry: Vec<u8>,
    /// OSC 8 link targets seen since the last [`AnsiStripper::take_links`].
    /// Kept out of the return value so every existing caller and test of
    /// `push` keeps reading as "bytes in, visible text out".
    links: Vec<String>,
}

impl AnsiStripper {
    fn push(&mut self, chunk: &[u8]) -> String {
        self.carry.extend_from_slice(chunk);
        let (mut out, links, consumed) = strip_ansi_prefix(&self.carry);
        self.record_links(links);
        self.carry.drain(..consumed);

        // Past the cap the leading sequence is not going to terminate. Drop
        // its introducer and re-strip: the bytes behind it are then treated as
        // ordinary text rather than discarded, so a token hiding inside a
        // runaway OSC still reaches the parser. Dropping one byte per
        // overflowing chunk is enough — the cap can only be re-crossed by a
        // fresh chunk, which re-enters here.
        if self.carry.len() > MAX_ANSI_CARRY {
            log::warn!(
                "`claude setup-token` emitted an unterminated control sequence \
                 longer than {} bytes — treating it as text",
                MAX_ANSI_CARRY
            );
        }
        while self.carry.len() > MAX_ANSI_CARRY {
            self.carry.drain(..1);
            let (more, links, consumed) = strip_ansi_prefix(&self.carry);
            self.record_links(links);
            self.carry.drain(..consumed);
            out.push_str(&more);
        }

        out
    }

    fn record_links(&mut self, links: Vec<String>) {
        for link in links {
            if self.links.len() >= MAX_PENDING_LINKS {
                break;
            }
            self.links.push(link);
        }
    }

    /// Hand over the hyperlink targets seen so far and forget them.
    fn take_links(&mut self) -> Vec<String> {
        std::mem::take(&mut self.links)
    }
}

/// Whether an OSC 8 target is worth forwarding to the UI as a sign-in candidate.
///
/// Deliberately shallow. The frontend re-parses it, applies the
/// `ANTHROPIC_SIGN_IN_HOSTS` allowlist before it is displayed, and applies it
/// again at the sink before `openUrl` — that is where the security decision
/// lives, and duplicating a host allowlist here would be a second place for it
/// to go stale. All this does is keep obvious junk off the wire.
fn usable_sign_in_link(uri: &str) -> bool {
    if !uri.starts_with("https://") && !uri.starts_with("http://") {
        return false;
    }
    if uri.len() > MAX_LINK_LENGTH {
        return false;
    }
    // Printable ASCII only. Control characters and whitespace are exactly how a
    // URL is smuggled past a display, and `new URL()` on the other side strips
    // some of them silently; a real authorize URL is percent-encoded anyway.
    if !uri.bytes().all(|b| (0x21..=0x7e).contains(&b)) {
        return false;
    }
    // This path bypasses [`SecretRedactor`] entirely, so nothing
    // credential-shaped is allowed to ride it.
    if uri.contains(SECRET_MARKER) {
        return false;
    }
    true
}

// ─────────────────────────────────────────────────────────────────────────────
// Rejected codes
// ─────────────────────────────────────────────────────────────────────────────

/// How many codes may be refused before the flow gives up.
///
/// The CLI will retry forever, which is the wrong bound for a dialog: the same
/// truncated clipboard pasted a fourth time will fail a fourth time, and a flow
/// that never resolves is indistinguishable from the hang this replaced.
const MAX_CODE_ATTEMPTS: usize = 3;

/// How much recent output to keep for [`detect_code_rejection`]. The message
/// lands within a few hundred characters of the paste; anything older belongs to
/// a previous attempt.
const REJECTION_SCAN_WINDOW: usize = 4096;

/// Phrases `claude setup-token` prints when it refuses a pasted code.
///
/// Measured against 2.1.226 under a pty. The CLI writes
///
/// ```text
/// OAuth error: Invalid code. Please make sure the full code was copied
/// Press Enter to retry.
/// ```
///
/// on two lines placed with cursor motion rather than newlines, then blocks on
/// stdin. Either phrase is enough — matching both would make a change to one of
/// them silently restore the hang.
const CODE_REJECTED_MARKERS: &[&str] = &["invalid code", "press enter to retry"];

/// Append `chunk` to `buf`, keeping no more than `cap` bytes of the tail.
fn push_capped_tail(buf: &mut String, chunk: &str, cap: usize) {
    buf.push_str(chunk);
    if buf.len() <= cap {
        return;
    }
    let cut = buf.len() - cap;
    let cut = (cut..buf.len())
        .find(|i| buf.is_char_boundary(*i))
        .unwrap_or(buf.len());
    buf.drain(..cut);
}

/// Whether `text` shows the CLI has refused a code and parked on stdin.
///
/// Whitespace is collapsed before matching because [`strip_ansi_prefix`] turns
/// the cursor moves that lay this message out into spaces and line breaks, so
/// the phrase arrives with runs of blanks inside it that are not in the source
/// string.
fn detect_code_rejection(text: &str) -> bool {
    let normalized: String = text
        .to_ascii_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    CODE_REJECTED_MARKERS
        .iter()
        .any(|marker| normalized.contains(marker))
}

// ─────────────────────────────────────────────────────────────────────────────
// Commands
// ─────────────────────────────────────────────────────────────────────────────

/// Stdin of the acquisition currently in flight, so [`submit_claude_token_code`]
/// can answer the CLI's `Paste code here` prompt.
///
/// `Some` exactly while a flow is running, which doubles as the single-flight
/// guard: the token is global, so two concurrent logins would race to overwrite
/// each other's keychain entry and neither could tell which prompt it was
/// feeding.
static PENDING_INPUT: OnceLock<Mutex<Option<mpsc::UnboundedSender<Vec<u8>>>>> = OnceLock::new();

fn pending_input() -> &'static Mutex<Option<mpsc::UnboundedSender<Vec<u8>>>> {
    PENDING_INPUT.get_or_init(|| Mutex::new(None))
}

/// Abort channel for the in-flight flow, claimed and released in lockstep with
/// [`PENDING_INPUT`].
///
/// Without this the only exits are "finished" and "timed out", so a user who
/// closes the dialog would be locked out by the single-flight guard until
/// `SETUP_TIMEOUT` elapsed.
static CANCEL_TX: OnceLock<Mutex<Option<oneshot::Sender<()>>>> = OnceLock::new();

fn cancel_slot() -> &'static Mutex<Option<oneshot::Sender<()>>> {
    CANCEL_TX.get_or_init(|| Mutex::new(None))
}

fn emit_progress(app: &AppHandle, project_id: &str, message: &str) {
    let _ = app.emit(
        PROGRESS_EVENT,
        serde_json::json!({ "project_id": project_id, "message": message }),
    );
}

fn emit_output(app: &AppHandle, project_id: &str, chunk: &str) {
    let _ = app.emit(
        OUTPUT_EVENT,
        serde_json::json!({ "project_id": project_id, "chunk": chunk }),
    );
}

fn emit_link(app: &AppHandle, project_id: &str, url: &str) {
    let _ = app.emit(
        LINK_EVENT,
        serde_json::json!({ "project_id": project_id, "url": url }),
    );
}

fn emit_code_rejected(app: &AppHandle, project_id: &str, message: &str, remaining: usize) {
    let _ = app.emit(
        CODE_REJECTED_EVENT,
        serde_json::json!({
            "project_id": project_id,
            "message": message,
            "attempts_remaining": remaining,
        }),
    );
}

/// Shell run inside the container.
///
/// * `stty` widens the pty before Claude Code starts, so its layout engine does
///   not wrap the token or the sign-in URL across lines. Docker's default exec
///   pty is 80 columns; both are longer than that. Setting it here rather than
///   via a post-start resize avoids racing the process's startup.
///
///   400 columns, not 200: the sign-in URL is 346 characters (measured against
///   2.1.226), so at 200 it wrapped anyway. Widening is *not* the fix for that
///   — this line is `|| true` and fails silently, which is precisely how the
///   truncated-URL bug survived — but it removes wrapping as a variable
///   everywhere else in the flow. The two things that must survive a wrap
///   regardless are handled directly: the URL comes from the OSC 8 parameter,
///   and [`scan_credential_body`] reassembles a split token.
/// * The `unset` line strips inherited auth so `setup-token` runs against a
///   clean claude.ai login instead of warning about, or deferring to, whatever
///   credential the container is already configured with — including a shared
///   token from a previous run, which is likely the very thing being replaced.
const SETUP_TOKEN_SCRIPT: &str = r#"stty cols 400 rows 50 2>/dev/null || true
unset CLAUDE_CODE_OAUTH_TOKEN ANTHROPIC_API_KEY ANTHROPIC_AUTH_TOKEN ANTHROPIC_BASE_URL \
      ANTHROPIC_MODEL CLAUDE_CODE_USE_BEDROCK AWS_BEARER_TOKEN_BEDROCK
exec claude setup-token"#;

/// Run `claude setup-token` in the container and return the token it printed.
/// Streams redacted output as it arrives and forwards anything arriving on
/// `input_rx` (the user's pasted code) to the command's stdin.
async fn run_setup_token(
    app: &AppHandle,
    project_id: &str,
    container_id: &str,
    mut input_rx: mpsc::UnboundedReceiver<Vec<u8>>,
    mut cancel_rx: oneshot::Receiver<()>,
) -> Result<String, String> {
    // A pty (`tty = true`) because `setup-token` renders an interactive TUI and
    // reads the pasted code in raw mode, which a plain pipe cannot provide.
    let AttachedExec {
        exec_id,
        mut output,
        mut input,
    } = create_attached_exec(
        container_id,
        vec![
            "sh".to_string(),
            "-c".to_string(),
            SETUP_TOKEN_SCRIPT.to_string(),
        ],
        true,
    )
    .await?;

    let mut stripper = AnsiStripper::default();
    let mut redactor = SecretRedactor::default();
    let mut transcript = String::new();
    let deadline = tokio::time::Instant::now() + SETUP_TIMEOUT;

    // The last hyperlink forwarded, so the five consecutive emissions the CLI
    // makes for one wrapped URL become one event. Only the immediately previous
    // one is remembered — the CLI reprints the same URL after a retry, and a
    // genuinely different one would be news.
    let mut last_link: Option<String> = None;
    // Recent visible output, scanned for a rejection message. Only filled while
    // a code is outstanding, so a repaint of an old message cannot re-fire.
    let mut recent = String::new();
    let mut awaiting_code_result = false;
    let mut rejected_codes = 0usize;

    loop {
        // Writing stdin and reading stdout are driven from the same loop: with
        // a hijacked exec both halves ride one socket, and `input` must stay
        // alive for the whole session anyway — dropping it early would tear the
        // output stream down with it.
        let next = tokio::select! {
            // Cancellation wins the race so a user who gives up isn't held by
            // the single-flight guard until the timeout. Dropping `input` and
            // `output` on return tears the exec down with them.
            _ = &mut cancel_rx => {
                return Err(
                    "Authentication cancelled. No token was stored.".to_string()
                );
            }
            Some(data) = input_rx.recv() => {
                if let Err(e) = input.write_all(&data).await {
                    return Err(format!(
                        "Could not send the code to `claude setup-token`: {}. No token was stored.",
                        e
                    ));
                }
                let _ = input.flush().await;
                // Arm the rejection detector. Anything the CLI says from here
                // on is a verdict on *this* code.
                awaiting_code_result = true;
                recent.clear();
                continue;
            }
            next = tokio::time::timeout_at(deadline, output.next()) => match next {
                Ok(next) => next,
                Err(_) => {
                    return Err(format!(
                        "Timed out after {} minutes waiting for `claude setup-token` to finish. \
                         No token was stored.",
                        SETUP_TIMEOUT.as_secs() / 60
                    ))
                }
            },
        };

        let frame = match next {
            Some(Ok(frame)) => frame,
            Some(Err(e)) => {
                return Err(format!(
                    "Lost the connection to `claude setup-token`: {}. No token was stored.",
                    e
                ))
            }
            None => break,
        };

        let visible = stripper.push(&frame.into_bytes());

        // Before the emptiness check: a chunk can be nothing but hyperlink
        // wrappers, which is exactly the chunk carrying the sign-in URL.
        for link in stripper.take_links() {
            if !usable_sign_in_link(&link) || last_link.as_deref() == Some(link.as_str()) {
                continue;
            }
            emit_link(app, project_id, &link);
            last_link = Some(link);
        }

        if visible.is_empty() {
            continue;
        }

        // A refused code parks the CLI on stdin instead of ending it, so this is
        // the only thing between the user and a 15-minute wait.
        if awaiting_code_result {
            push_capped_tail(&mut recent, &visible, REJECTION_SCAN_WINDOW);
            if detect_code_rejection(&recent) {
                awaiting_code_result = false;
                recent.clear();
                rejected_codes += 1;

                if rejected_codes >= MAX_CODE_ATTEMPTS {
                    return Err(format!(
                        "`claude setup-token` rejected the code {} times, so the sign-in was \
                         abandoned. No token was stored. The code is long and easy to \
                         truncate — copy all of it from the Anthropic page, then start \
                         authentication again.",
                        rejected_codes
                    ));
                }

                // The CLI is parked on "Press Enter to retry" and will not draw
                // its paste prompt again until it gets that Enter. Send it, so
                // the user's next code has somewhere to land. Bounded above, and
                // announced below — this is a retry the user is told about, not
                // a loop hidden from them.
                if let Err(e) = input.write_all(b"\r").await {
                    return Err(format!(
                        "`claude setup-token` rejected the code and could not be asked to \
                         retry: {}. No token was stored.",
                        e
                    ));
                }
                let _ = input.flush().await;

                let remaining = MAX_CODE_ATTEMPTS - rejected_codes;
                let message = format!(
                    "That code was rejected — `claude setup-token` reports the full code was \
                     not copied. Copy it again from the Anthropic page and submit it; {} \
                     attempt{} left.",
                    remaining,
                    if remaining == 1 { "" } else { "s" }
                );
                emit_code_rejected(app, project_id, &message, remaining);
                emit_progress(app, project_id, &message);
            }
        }

        transcript.push_str(&visible);
        if transcript.len() > MAX_TRANSCRIPT {
            // Keep the tail: that is where the token lands.
            let cut = transcript.len() - MAX_TRANSCRIPT / 2;
            let cut = (cut..transcript.len())
                .find(|i| transcript.is_char_boundary(*i))
                .unwrap_or(transcript.len());
            transcript.drain(..cut);
        }

        let safe = redactor.push(&visible);
        if !safe.is_empty() {
            emit_output(app, project_id, &safe);
        }
    }

    let tail = redactor.flush();
    if !tail.is_empty() {
        emit_output(app, project_id, &tail);
    }

    // `None` means the exit code could not be determined, not that it was zero.
    // Falling through to the token parse is right either way — a run that
    // printed a token succeeded whatever Docker says about it — but it is worth
    // saying so rather than silently calling it a clean exit.
    match wait_for_exec_exit(&exec_id).await {
        Some(0) => {}
        Some(code) => {
            return Err(format!(
                "`claude setup-token` exited with status {}. No token was stored — \
                 see the command output above for what went wrong.",
                code
            ))
        }
        None => log::warn!(
            "Could not read the exit status of `claude setup-token`; judging the run \
             by whether it printed a token"
        ),
    }

    parse_setup_token(&transcript).ok_or_else(|| {
        if rejected_codes > 0 {
            format!(
                "`claude setup-token` ended without a token after rejecting {} code{}. \
                 Nothing was stored. Copy the whole code from the Anthropic page — it is \
                 long and easy to truncate — then start authentication again.",
                rejected_codes,
                if rejected_codes == 1 { "" } else { "s" }
            )
        } else {
            "`claude setup-token` finished but printed no recognisable token. \
             Nothing was stored. This usually means the login was cancelled, or the \
             account has no Claude subscription (long-lived tokens require one)."
                .to_string()
        }
    })
}

/// Mint a shared, long-lived Claude Code token by running `claude setup-token`
/// inside `project_id`'s container, and store it in the OS keychain.
///
/// The project only lends its container — a place to run the CLI that already
/// has Claude Code installed. The resulting token is global, and is used by
/// every Anthropic-backend project that has not opted out.
#[tauri::command]
pub async fn acquire_claude_token(
    project_id: String,
    app_handle: AppHandle,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let project = state
        .projects_store
        .get(&project_id)
        .ok_or_else(|| format!("Project {} not found", project_id))?;

    let container_id = project.container_id.clone().ok_or_else(|| {
        format!(
            "Project '{}' has no container yet. Start it, then run authentication again.",
            project.name
        )
    })?;
    if !is_container_running(&container_id).await.unwrap_or(false) {
        return Err(format!(
            "The container for '{}' is not running. Start it, then run authentication again.",
            project.name
        ));
    }

    // Claim the flow before touching anything else, so a second caller bounces
    // off the guard rather than half-configuring the same project.
    let (input_tx, input_rx) = mpsc::unbounded_channel::<Vec<u8>>();
    let (cancel_tx, cancel_rx) = oneshot::channel::<()>();
    {
        // Both slots are claimed under the input lock held first, and released
        // in the same order below, so the guard and its abort channel can never
        // disagree about whether a flow is live.
        let mut slot = pending_input().lock().await;
        if slot.is_some() {
            return Err(
                "A Claude authentication flow is already running. Finish or cancel it first."
                    .to_string(),
            );
        }
        *slot = Some(input_tx);
        *cancel_slot().lock().await = Some(cancel_tx);
    }

    // No auth-bridge elevation here — see the module docs. `setup-token` has no
    // container-local callback to bridge, and the flag that would enable one is
    // *persisted*, so a crash anywhere inside the 15-minute timeout used to
    // leave an unauthenticated port mirror armed for good.
    emit_progress(
        &app_handle,
        &project_id,
        "Running `claude setup-token` — sign in at the URL below, then submit the code it gives you.",
    );

    let result =
        run_setup_token(&app_handle, &project_id, &container_id, input_rx, cancel_rx).await;

    // Release the flow. Nothing else needs unwinding: this command changes no
    // persisted state until the token itself is stored, which is the point.
    *pending_input().lock().await = None;
    *cancel_slot().lock().await = None;

    let token = result?;
    secure::store_claude_oauth_token(&token)?;

    log::info!(
        "Stored a shared Claude authentication token (acquired via project {})",
        project_id
    );
    emit_progress(
        &app_handle,
        &project_id,
        "Token stored in the OS keychain. Restart your Anthropic-backend containers to use it.",
    );

    Ok(())
}

/// Answer the `Paste code here if prompted >` prompt of a running
/// [`acquire_claude_token`] with the code shown after signing in.
///
/// Takes no project id: the flow is single-flight and the token is global, so
/// there is only ever one prompt waiting.
#[tauri::command]
pub async fn submit_claude_token_code(code: String) -> Result<(), String> {
    let code = code.trim();
    if code.is_empty() {
        return Err("Enter the code shown after signing in.".to_string());
    }
    // The code goes to a TUI text input. A newline or escape embedded in it
    // would submit early or drive the widget, so reject control characters
    // outright rather than trying to sanitise them.
    if code.chars().any(char::is_control) {
        return Err("That code contains invalid characters. Copy it again and retry.".to_string());
    }

    let slot = pending_input().lock().await;
    let sender = slot.as_ref().ok_or_else(|| {
        "No Claude authentication flow is waiting for a code. Start authentication first."
            .to_string()
    })?;

    let mut keystrokes = code.as_bytes().to_vec();
    keystrokes.push(b'\r');
    sender
        .send(keystrokes)
        .map_err(|_| "The authentication flow has already ended.".to_string())
}

/// Abort an in-flight [`acquire_claude_token`].
///
/// Tears the `setup-token` exec down and releases the single-flight guard, so
/// the user can immediately try again rather than waiting out `SETUP_TIMEOUT`.
/// A no-op when nothing is running, so closing the dialog twice is harmless.
#[tauri::command]
pub async fn cancel_claude_token() -> Result<(), String> {
    let Some(sender) = cancel_slot().lock().await.take() else {
        return Ok(());
    };
    // `Err` only means the flow finished between the take and the send, which
    // is exactly the outcome cancelling wanted.
    let _ = sender.send(());
    Ok(())
}

/// Whether a shared Claude token exists. Deliberately a boolean — no command
/// here ever hands the token itself to the frontend.
#[tauri::command]
pub async fn has_claude_token() -> Result<bool, String> {
    Ok(secure::has_claude_oauth_token())
}

/// The tail of every refusal [`crate::project_lock::try_acquire`] produces.
///
/// [`crate::docker::container::scrub_secrets_from_snapshots`] folds two very
/// different things into one `failed` list: an image that genuinely could not
/// be rewritten, and one that was never *attempted* because another operation
/// held the project. Only the second is retryable, and only the second should
/// be described to the user as "come back in a minute" rather than "reset this
/// project". Splitting them needs a discriminator, and the refusal string is
/// the only one that crosses the module boundary — `try_acquire` returns
/// `Result<ProjectGuard, String>`, and `container.rs` pushes that `String`
/// through unchanged.
///
/// Matching on prose is normally a mistake, so this is pinned by
/// [`tests::a_real_lock_refusal_is_recognised_as_retryable`], which builds a
/// refusal by actually taking a guard rather than by copying the wording. If
/// `project_lock` ever rephrases, that test fails instead of this silently
/// misclassifying a credential that was left in place.
const PROJECT_BUSY_MARKER: &str = "Wait for it to finish before ";

/// Whether a scrub failure means "somebody else has this project right now",
/// which is transient, rather than "this image cannot be rewritten", which is
/// not. Nothing bollard returns contains [`PROJECT_BUSY_MARKER`].
fn is_project_busy_refusal(reason: &str) -> bool {
    reason.contains(PROJECT_BUSY_MARKER)
}

/// What a cleanup managed to reach. Every field is about copies of the token
/// that live *outside* the keychain — snapshot images — so the same shape
/// serves [`clear_claude_token`], where the keychain entry is already gone by
/// the time this is returned, and [`sweep_claude_token_snapshots`], where the
/// keychain was never touched.
///
/// Three lists rather than one, because "we rewrote it", "we could not rewrite
/// it" and "we did not try" are three different things to tell somebody who
/// just revoked a credential, and only the last one is fixed by waiting.
#[derive(Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct ClearTokenOutcome {
    /// Snapshot images that were holding the token and have been rewritten.
    pub snapshots_scrubbed: Vec<String>,
    /// Images still holding it, with the reason each could not be rewritten.
    /// Non-empty means the revocation is **incomplete** and the UI must say so.
    pub snapshots_failed: Vec<String>,
    /// Images still holding it that were **not attempted**, because another
    /// operation held the project (a start, a compaction, a migration). Also an
    /// incomplete revocation — but a retryable one, and the UI must not offer
    /// "Reset the project" as the remedy for it.
    pub snapshots_skipped: Vec<String>,
    /// Rewritten, but the pre-rewrite image object could not be deleted because
    /// a container is still running off it. Worth mentioning, not worth
    /// alarming about — see `SnapshotScrubReport::superseded_retained`.
    pub snapshots_superseded: Vec<String>,
    /// Set when Docker could not be reached at all, so nothing is known about
    /// what is still on disk.
    pub docker_unavailable: Option<String>,
}

impl ClearTokenOutcome {
    /// Whether a copy of the credential is known — or suspected — to still be
    /// reachable, so the caller should offer to run the sweep again.
    ///
    /// `snapshots_superseded` is deliberately not counted: that image is
    /// untagged, nothing new is built from it, and it goes away on the next
    /// restart. Re-running would report it forever and train the user to
    /// ignore the warning.
    pub fn needs_another_pass(&self) -> bool {
        !self.snapshots_failed.is_empty()
            || !self.snapshots_skipped.is_empty()
            || self.docker_unavailable.is_some()
    }
}

/// Fold a scrub report into the IPC shape, splitting the busy projects out of
/// the failures. Separate from the command so it can be tested without Docker.
fn summarise_scrub(report: crate::docker::container::SnapshotScrubReport) -> ClearTokenOutcome {
    let mut outcome = ClearTokenOutcome {
        snapshots_scrubbed: report.scrubbed,
        snapshots_superseded: report.superseded_retained,
        docker_unavailable: report.unavailable,
        ..Default::default()
    };
    for (image, reason) in report.failed {
        let line = format!("{}: {}", image, reason);
        if is_project_busy_refusal(&reason) {
            outcome.snapshots_skipped.push(line);
        } else {
            outcome.snapshots_failed.push(line);
        }
    }
    outcome
}

/// Which halves of a cleanup to run.
///
/// The distinction has to exist **on the wire**, not in a toast string. The UI
/// offers a "Retry snapshot cleanup" button after an incomplete revocation, and
/// while [`clear_claude_token`] was the only command behind it that button was
/// a *second revoke* wearing a retry's label: it deleted the keychain entry
/// unconditionally, with no confirmation, in a panel that survived the user
/// re-authenticating from the button directly above it. Pressing it then threw
/// away the token they had just acquired and said only that some images had
/// been checked.
///
/// [`Cleanup::ImagesOnly`] is the honest primitive the retry actually wanted:
/// the images are the durable record of what is left to do, so re-deriving the
/// work from Docker needs no keychain entry and must not consume one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Cleanup {
    /// Delete the keychain entry, then rewrite the images. What "Revoke" does,
    /// behind its confirmation modal.
    KeychainThenImages,
    /// Rewrite the images and leave the keychain entirely alone. What "Retry
    /// snapshot cleanup" and "Check snapshot images" do.
    ImagesOnly,
}

/// The body of both cleanup commands, with its two halves injected so the
/// *order* — and the fact that [`Cleanup::ImagesOnly`] never reaches the
/// keychain at all — can be tested without a keychain or a Docker daemon.
async fn run_cleanup<K, S, F>(
    what: Cleanup,
    delete_keychain: K,
    sweep: S,
) -> Result<ClearTokenOutcome, String>
where
    K: FnOnce() -> Result<(), String>,
    S: FnOnce() -> F,
    F: std::future::Future<Output = crate::docker::container::SnapshotScrubReport>,
{
    let revoking = what == Cleanup::KeychainThenImages;

    if revoking {
        // First, and before anything slow — see "Why the keychain goes first".
        // Nothing has been touched if this fails, so the error is the whole
        // answer: the caller still has its Revoke button, and the standalone
        // sweep is available for the images regardless.
        if let Err(e) = delete_keychain() {
            log::error!(
                "Could not delete the shared Claude token from the keychain; no snapshot image \
                 was touched: {}",
                e
            );
            return Err(e);
        }
        log::info!("Cleared the shared Claude authentication token from the keychain");
    }

    let report = sweep().await;
    let swept_clean = !report.left_something_behind();
    let outcome = summarise_scrub(report);

    let lead = if revoking {
        "Revoked the shared Claude token but"
    } else {
        "Swept the snapshot images but"
    };
    for image in &outcome.snapshots_failed {
        log::warn!("{} could not clear it from {}", lead, image);
    }
    for image in &outcome.snapshots_skipped {
        log::warn!(
            "{} left it in {} — the project was busy; running the snapshot sweep again will retry",
            lead,
            image
        );
    }
    if let Some(ref reason) = outcome.docker_unavailable {
        log::warn!("{} checked no snapshot image at all: {}", lead, reason);
    }
    if swept_clean && !outcome.needs_another_pass() {
        log::info!(
            "No snapshot image is still holding the shared Claude token ({} rewritten)",
            outcome.snapshots_scrubbed.len()
        );
    }

    Ok(outcome)
}

/// Forget the shared Claude token, and remove the copies of it that outlive the
/// keychain entry.
///
/// Deleting the keychain entry is the easy half. The token also exists in two
/// other places, and a "Revoke" button that leaves either of them behind is
/// telling the user something untrue:
///
///  * **Running containers** hold it in their environment. That resolves
///    itself: the rotation id in `triple-c.claude-token-version` no longer
///    matches, so the next start recreates the container and
///    `MANAGED_AUTH_KEYS` blanks the variable.
///  * **Snapshot images** hold it in `Config.Env`, and nothing resolves that on
///    its own — the image outlives every container built from it, and
///    `docker image inspect` will keep printing a live ~1-year credential for
///    as long as the image exists. New commits no longer bake it in (see
///    [`crate::docker::container::commit_container_snapshot`]), but images
///    committed by earlier builds have to be rewritten, which is what
///    [`crate::docker::container::scrub_secrets_from_snapshots`] does here.
///
/// ## Why the keychain goes first
///
/// The sweep is not quick. It lists every `triple-c-snapshot-*` image and then
/// inspects, creates, commits and removes *per image*, over bollard's Docker
/// socket with its 120-second-per-request default. Deferring the keychain
/// delete behind all of that leaves the credential live for the whole window
/// while the UI says "Revoking…", and two separate things go wrong in it:
///
///  * A quit, a crash or a kill mid-sweep and the entry was never deleted at
///    all. The token the user believes they revoked is still in the keychain,
///    still ~1-year valid, and still injected into every container start.
///  * [`has_claude_token`] stays true throughout, and
///    [`crate::docker::container::create_container`] reads the keychain at
///    container-**create** time rather than at app start. The per-project
///    [`crate::project_lock::ProjectOp::SecretScrub`] guard is released as soon
///    as that one project's image has been rewritten — so a project scrubbed
///    early in the sweep can be started again later in the *same* sweep and be
///    handed a fresh copy of the credential in its env. The images end up clean
///    and the running fleet does not.
///
/// An earlier version ran the sweep first, on the argument that a crash
/// mid-sweep would otherwise leave the token in an image with the keychain
/// entry — and therefore the Revoke button — already gone. That argument was
/// about *recoverability*, and [`sweep_claude_token_snapshots`] answers it
/// directly: the images are the durable record, so the retry needs no keychain
/// entry to exist and no persisted to-do list. The comment that ordering
/// carried ("no window in which a scrubbed image is re-poisoned") was true of
/// images and silent about containers, which is where the leak was, and silent
/// about the minutes the token stayed live.
///
/// The keychain deletion is never rolled back if the scrub then fails; a
/// partially completed revocation is still better than none, and the outcome is
/// reported so the UI can be explicit about what is left.
#[tauri::command]
pub async fn clear_claude_token() -> Result<ClearTokenOutcome, String> {
    run_cleanup(
        Cleanup::KeychainThenImages,
        secure::delete_claude_oauth_token,
        crate::docker::container::scrub_secrets_from_snapshots,
    )
    .await
}

/// Rewrite every snapshot image that still carries a credential, **without
/// touching the keychain**.
///
/// This is the retry, and it is its own command because the retry is its own
/// act. `docker commit` copied the token into each project's snapshot image;
/// rewriting those images is a cleanup that has nothing to do with whether a
/// token is stored today, and folding it into [`clear_claude_token`] made every
/// press of "Retry snapshot cleanup" an unconfirmed credential deletion.
///
/// Safe to call at any time and in any state:
///
///  * with a token stored — a snapshot committed by an older build carries the
///    *current* token, and clearing it out of the image does not stop the
///    keychain entry being injected on the next container start;
///  * with nothing stored — images committed by earlier builds still carry
///    whatever token was live when they were committed, which is exactly the
///    case the old sweep-first ordering could strand;
///  * repeatedly — the work is re-derived from Docker each time, so an image
///    whose project was busy on the last pass is simply picked up on this one.
#[tauri::command]
pub async fn sweep_claude_token_snapshots() -> Result<ClearTokenOutcome, String> {
    run_cleanup(
        Cleanup::ImagesOnly,
        // Never called; the `ImagesOnly` branch is the entire point of this
        // command, and a change that made it reachable must fail loudly rather
        // than delete a credential quietly.
        || -> Result<(), String> { unreachable!("an images-only sweep must never touch the keychain") },
        crate::docker::container::scrub_secrets_from_snapshots,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A token-shaped value of realistic length.
    fn token(seed: char) -> String {
        format!("{}{}", TOKEN_PREFIX, std::iter::repeat(seed).take(90).collect::<String>())
    }

    #[test]
    fn extracts_the_token_from_realistic_output() {
        let tok = token('A');
        let output = format!(
            "Claude Code long-lived token setup\n\
             Opening browser to https://claude.ai/oauth/authorize?code=true\n\
             Login successful!\n\n\
             Your token:\n{}\n\n\
             Set CLAUDE_CODE_OAUTH_TOKEN to this value.\n",
            tok
        );
        assert_eq!(parse_setup_token(&output), Some(tok));
    }

    #[test]
    fn ignores_prose_decoys_and_still_finds_the_real_token() {
        let tok = token('B');
        let output = format!(
            "Set the env var like so:\n  export CLAUDE_CODE_OAUTH_TOKEN=sk-ant-oat01-...\n\
             or CLAUDE_CODE_OAUTH_TOKEN=sk-ant-oat01-<your-token-here>\n\
             Your token: {}\n",
            tok
        );
        assert_eq!(parse_setup_token(&output), Some(tok));
    }

    #[test]
    fn a_decoy_on_its_own_yields_nothing() {
        let output = "Usage: export CLAUDE_CODE_OAUTH_TOKEN=sk-ant-oat01-...\n";
        assert_eq!(parse_setup_token(output), None);
    }

    #[test]
    fn no_match_returns_none_rather_than_guessing() {
        assert_eq!(parse_setup_token(""), None);
        assert_eq!(
            parse_setup_token("error: authentication cancelled by the user\n"),
            None
        );
        // Right length, wrong product prefix.
        assert_eq!(
            parse_setup_token(&format!("sk-ant-api03-{}\n", "C".repeat(90))),
            None
        );
    }

    #[test]
    fn multiple_matches_take_the_last() {
        let old = token('D');
        let new = token('E');
        let output = format!(
            "Replacing existing token {}\n...\nYour new token: {}\n",
            old, new
        );
        assert_eq!(parse_setup_token(&output), Some(new));
    }

    #[test]
    fn a_repainted_tui_frame_yields_the_same_token_once() {
        let tok = token('F');
        // Same frame drawn three times, as a TUI would.
        let output = format!("Your token: {}\n", tok).repeat(3);
        assert_eq!(parse_setup_token(&output), Some(tok));
    }

    #[test]
    fn a_prefix_glued_to_a_longer_word_is_not_a_token() {
        let output = format!("notasecretsk-ant-oat01-{}\n", "G".repeat(90));
        assert_eq!(parse_setup_token(&output), None);
    }

    #[test]
    fn the_token_stops_at_the_first_non_token_character() {
        let tok = token('H');
        let output = format!("token=\"{}\", expires=2027-08-09\n", tok);
        assert_eq!(parse_setup_token(&output), Some(tok));
    }

    #[test]
    fn redaction_masks_a_token_in_one_piece() {
        let mut r = SecretRedactor::default();
        let mut seen = r.push(&format!("Your token: {}\n", token('I')));
        seen.push_str(&r.flush());
        assert!(!seen.contains(TOKEN_PREFIX));
        assert!(seen.contains(SECRET_PLACEHOLDER));
        assert!(seen.contains("Your token: "));
    }

    #[test]
    fn redaction_survives_a_token_split_across_chunks() {
        let tok = token('J');
        let mut r = SecretRedactor::default();
        let mut seen = String::new();
        // Split mid-prefix and again mid-body — the worst case for a naive
        // per-chunk regex.
        seen.push_str(&r.push("Your token: sk-a"));
        seen.push_str(&r.push(&tok[4..40]));
        seen.push_str(&r.push(&tok[40..]));
        seen.push_str(&r.push("\ndone\n"));
        seen.push_str(&r.flush());
        assert!(!seen.contains(TOKEN_PREFIX), "leaked: {}", seen);
        assert!(seen.contains(SECRET_PLACEHOLDER));
        assert!(seen.ends_with("\ndone\n"));
    }

    #[test]
    fn redaction_leaves_ordinary_text_alone() {
        let mut r = SecretRedactor::default();
        let mut seen = r.push("Visit https://claude.ai/oauth/authorize?code=abc-def to continue\n");
        seen.push_str(&r.flush());
        assert_eq!(
            seen,
            "Visit https://claude.ai/oauth/authorize?code=abc-def to continue\n"
        );
    }

    #[test]
    fn ansi_stripping_recovers_the_token_from_a_styled_frame() {
        let tok = token('K');
        let framed = format!(
            "\x1b[2J\x1b[H\x1b[1;36mYour token:\x1b[0m\r\n\x1b[32m{}\x1b[0m\r\n",
            tok
        );
        let mut s = AnsiStripper::default();
        let visible = s.push(framed.as_bytes());
        assert!(!visible.contains('\x1b'));
        assert_eq!(parse_setup_token(&visible), Some(tok));
    }

    /// Claude Code's TUI positions words with `ESC [ n G` instead of spaces
    /// (verified against 2.1.226). Deleting those would weld words together.
    #[test]
    fn ansi_stripping_turns_column_jumps_into_separators() {
        let mut s = AnsiStripper::default();
        let visible = s.push(b"\x1b[38;2;215;119;87mWelcome\x1b[9Gto\x1b[12GClaude\x1b[19GCode\x1b[39m");
        assert_eq!(visible, "Welcome to Claude Code");
    }

    /// The failure this protects against: a column jump immediately before the
    /// token would, if simply deleted, glue the preceding word onto the prefix
    /// and make `parse_setup_token` reject a perfectly good token.
    #[test]
    fn a_column_jump_before_the_token_does_not_hide_it() {
        let tok = token('L');
        let framed = format!("\x1b[2GToken\x1b[8G{}\r\n", tok);
        let mut s = AnsiStripper::default();
        let visible = s.push(framed.as_bytes());
        // The leading jump is an indent, so it becomes a space too.
        assert_eq!(visible, format!(" Token {}\n", tok));
        assert_eq!(parse_setup_token(&visible), Some(tok));
    }

    /// A pty with ONLCR emits `\r\r\n` at end of line; that is one break.
    #[test]
    fn carriage_return_runs_before_a_newline_collapse() {
        let mut s = AnsiStripper::default();
        let visible = s.push(b"one\r\r\ntwo\r\r\n");
        assert_eq!(visible, "one\ntwo\n");
    }

    /// A bare CR is a repaint, and must still break the line so the old frame's
    /// tail cannot be welded onto the new frame's head.
    #[test]
    fn a_bare_carriage_return_breaks_the_line() {
        let mut s = AnsiStripper::default();
        let visible = s.push(b"sk-ant-oat01-old\rsk-ant-oat01-new");
        assert_eq!(visible, "sk-ant-oat01-old\nsk-ant-oat01-new");
    }

    #[test]
    fn ansi_stripping_removes_osc8_hyperlink_wrappers() {
        let mut s = AnsiStripper::default();
        let visible = s.push(b"\x1b]8;id=1;https://claude.com/x\x07https://claude.com/x\x1b]8;;\x07");
        assert_eq!(visible, "https://claude.com/x");
    }

    #[test]
    fn ansi_stripping_handles_a_sequence_split_across_chunks() {
        let mut s = AnsiStripper::default();
        let mut visible = s.push(b"a\x1b[3");
        visible.push_str(&s.push(b"1mb"));
        assert_eq!(visible, "ab");
    }
    // ── Truncated and wrapped credentials ────────────────────────────────
    // `stty cols 400` runs before Claude Code starts precisely so the ~103
    // character token never wraps. But that line is `|| true` and fails
    // silently, and an 80-column fallback splits the value across two lines.
    // A fragment must never be accepted; the whole value, reassembled, must be.

    #[test]
    fn a_token_shorter_than_a_real_one_is_rejected() {
        let fragment = format!("{}{}", TOKEN_PREFIX, "M".repeat(MIN_TOKEN_BODY - 1));
        assert_eq!(
            parse_setup_token(&format!("Your token: {}\n", fragment)),
            None
        );
    }

    #[test]
    fn a_truncated_token_with_no_continuation_is_still_rejected() {
        let tok = token('N');
        // The first half of an 80-column wrap, and nothing after it.
        let half = format!("Your token: {}\n", &tok[..68]);
        assert_eq!(
            parse_setup_token(&half),
            None,
            "half a credential must fail loudly, not be stored truncated"
        );
    }

    #[test]
    fn a_line_wrapped_token_is_reassembled_whole() {
        let tok = token('N');
        // Column 12 is where `Your token: ` ends, so an 80-column pty breaks
        // the value 68 characters in.
        let wrapped = format!("Your token: {}\n{}\n", &tok[..68], &tok[68..]);
        assert_eq!(
            parse_setup_token(&wrapped),
            Some(tok),
            "the halves of a wrapped token belong to one credential"
        );
    }

    #[test]
    fn a_token_wrapped_twice_is_reassembled_whole() {
        let tok = token('N');
        // A 40-column pty needs two breaks for a 103-character value.
        let wrapped = format!("{}\n{}\n{}\n", &tok[..40], &tok[40..80], &tok[80..]);
        assert_eq!(parse_setup_token(&wrapped), Some(tok));
    }

    /// The guard that keeps reassembly from becoming a fabrication machine: a
    /// break early in a line is prose, not the terminal's right margin, so the
    /// two sides are two different things and must not be welded together.
    #[test]
    fn a_break_before_the_margin_does_not_join_two_lines() {
        let tail = "N".repeat(70);
        let text = format!("{}ABC\n{}\n", TOKEN_PREFIX, tail);
        assert_eq!(
            parse_setup_token(&text),
            None,
            "a short first line is prose; joining it would invent a credential"
        );
    }

    /// A repainting TUI prints the same line again and again, so the character
    /// after a line break is very often the start of the next frame. Joining
    /// there would hand back `<token>Your` instead of `<token>`.
    #[test]
    fn a_repaint_after_a_complete_token_is_not_joined_onto_it() {
        let tok = token('P');
        let output = format!("Your token: {}\n", tok).repeat(3);
        assert_eq!(parse_setup_token(&output), Some(tok));
    }

    /// The wrap has to produce a *whole* credential to be believed. Two short
    /// pieces that still fall short of the floor are not one.
    #[test]
    fn joining_a_wrap_does_not_lower_the_length_floor() {
        let body_a = "N".repeat(45);
        let body_b = "N".repeat(20);
        let text = format!("{}{}\n{}\n", TOKEN_PREFIX, body_a, body_b);
        assert_eq!(parse_setup_token(&text), None);
    }

    /// The security half of the same bug. Before the parser could reassemble a
    /// wrapped token, the redactor could not either: it masked the first line,
    /// which carries the `sk-ant-` marker, and printed the second — the tail of
    /// a live credential — to the UI in clear.
    #[test]
    fn redaction_masks_both_halves_of_a_wrapped_token() {
        let tok = token('R');
        let mut r = SecretRedactor::default();
        let mut seen = r.push(&format!("Your token: {}\n{}\n", &tok[..68], &tok[68..]));
        seen.push_str(&r.flush());
        assert!(!seen.contains(TOKEN_PREFIX), "leaked: {}", seen);
        assert!(
            !seen.contains(&tok[68..]),
            "the tail of the credential reached the UI: {}",
            seen
        );
        assert!(seen.contains(SECRET_PLACEHOLDER));
    }

    /// …and the same when the wrap lands on a chunk boundary, which is the way
    /// it actually arrives off the socket.
    #[test]
    fn redaction_masks_a_wrapped_token_split_across_chunks() {
        let tok = token('S');
        let mut r = SecretRedactor::default();
        let mut seen = r.push(&format!("Your token: {}", &tok[..68]));
        seen.push_str(&r.push("\n"));
        seen.push_str(&r.push(&tok[68..]));
        seen.push_str(&r.push("\ndone\n"));
        seen.push_str(&r.flush());
        assert!(!seen.contains(TOKEN_PREFIX), "leaked: {}", seen);
        assert!(!seen.contains(&tok[68..]), "leaked tail: {}", seen);
        assert!(seen.ends_with("\ndone\n"));
    }

    #[test]
    fn a_real_length_token_is_still_accepted() {
        // Guards the floor from being raised past what Anthropic actually mints.
        let tok = token('O');
        assert_eq!(tok.len() - TOKEN_PREFIX.len(), 90);
        assert!(90 > MIN_TOKEN_BODY);
        assert_eq!(parse_setup_token(&format!("{}\n", tok)), Some(tok));
    }

    // ── OSC 8 sign-in link ───────────────────────────────────────────────
    // The URL only exists in one piece inside the hyperlink parameter. Its
    // visible text is chopped into terminal-width slices, each of them a
    // separate, complete hyperlink emission — measured against 2.1.226 at 80
    // columns, where a 346-character URL arrives as five of them.

    /// The real sign-in URL's shape and length, from the pty capture.
    fn sign_in_url() -> String {
        let url = format!(
            "https://claude.com/cai/oauth/authorize?code=true&client_id=9d1c250a-e61b-44d9-88ed-\
             5944d1962f5e&response_type=code&redirect_uri=https%3A%2F%2Fplatform.claude.com%2F\
             oauth%2Fcode%2Fcallback&scope=user%3Ainference&code_challenge={}&\
             code_challenge_method=S256&state=su-{}",
            "R".repeat(43),
            "x".repeat(40)
        );
        assert!(url.len() > 300, "the point of this fixture is its length");
        url
    }

    /// One hyperlink emission: the whole URL in the parameter, `visible` on
    /// screen, then the empty `8;;` that closes it.
    fn osc8(url: &str, visible: &str) -> String {
        format!(
            "\x1b]8;id=1umaq0e;{}\x07\x1b[38;2;153;153;153m{}\x1b[39m\x1b]8;;\x07",
            url, visible
        )
    }

    #[test]
    fn the_full_url_survives_a_display_text_wrapped_mid_url() {
        let url = sign_in_url();
        let mut frame = String::new();
        for slice in url.as_bytes().chunks(80) {
            frame.push_str(&osc8(&url, std::str::from_utf8(slice).unwrap()));
            frame.push_str("\r\r\n");
        }

        let mut s = AnsiStripper::default();
        let visible = s.push(frame.as_bytes());
        let links = s.take_links();

        // The visible text is exactly the broken form the old scraper saw.
        assert!(visible.contains(&format!("{}\n", &url[..80])));
        assert!(!visible.contains(&url));

        assert!(links.iter().all(|l| *l == url), "links: {:?}", links);
        assert_eq!(links.len(), 5, "one emission per display slice");
        assert!(usable_sign_in_link(&links[0]));
    }

    #[test]
    fn the_closing_half_of_a_hyperlink_is_not_a_link() {
        let mut s = AnsiStripper::default();
        s.push(b"\x1b]8;;\x07");
        assert!(s.take_links().is_empty());
    }

    #[test]
    fn a_non_hyperlink_osc_is_not_a_link() {
        let mut s = AnsiStripper::default();
        // Window title, and the app's own URL relay sequence.
        s.push(b"\x1b]0;a title\x07\x1b]7777;open;aHR0cHM6Ly9ldmlsLnRsZA==\x07");
        assert!(s.take_links().is_empty());
    }

    #[test]
    fn a_hyperlink_split_across_chunks_still_yields_its_target() {
        let url = sign_in_url();
        let frame = osc8(&url, &url[..80]);
        let (head, tail) = frame.as_bytes().split_at(60);

        let mut s = AnsiStripper::default();
        s.push(head);
        assert!(s.take_links().is_empty(), "incomplete: nothing to report yet");
        s.push(tail);
        assert_eq!(s.take_links().first().map(String::as_str), Some(url.as_str()));
    }

    #[test]
    fn only_plausible_http_targets_reach_the_frontend() {
        assert!(usable_sign_in_link("https://claude.ai/oauth/authorize?code=true"));
        assert!(usable_sign_in_link("http://127.0.0.1:8123/callback"));
        // The host allowlist lives on the frontend; these are the shallow
        // checks that keep junk off the wire.
        assert!(!usable_sign_in_link("file:///etc/passwd"));
        assert!(!usable_sign_in_link("javascript:alert(1)"));
        assert!(!usable_sign_in_link("https://claude.ai/a b"));
        assert!(!usable_sign_in_link("https://claude.ai/\u{7f}"));
        assert!(!usable_sign_in_link(&format!(
            "https://claude.ai/?t={}",
            "A".repeat(MAX_LINK_LENGTH)
        )));
        // Nothing credential-shaped may ride the one path that skips redaction.
        assert!(!usable_sign_in_link(&format!(
            "https://claude.ai/?t={}",
            token('T')
        )));
    }

    /// Claude Code prefixes every repaint frame with `ESC ( B`. Treating that
    /// as a two-byte escape emitted the `B` as text — and a stray `B` glued to
    /// the front of a token makes [`parse_setup_token`] refuse it.
    #[test]
    fn a_charset_designation_does_not_leave_a_letter_behind() {
        let tok = token('U');
        let mut s = AnsiStripper::default();
        let visible = s.push(format!("\x1b(B\x0f{}\r\n", tok).as_bytes());
        assert_eq!(visible, format!("{}\n", tok));
        assert_eq!(parse_setup_token(&visible), Some(tok));
    }

    // ── Rejected codes ───────────────────────────────────────────────────

    /// The exact bytes 2.1.226 emits after a bad paste, from the pty capture.
    const REJECTION_FRAME: &[u8] =
        b"\x1b(B\x0f\x1b[2K\x1b[1A\x1b[2K\x1b[G\x1b[1A\r\x1b[1C\x1b[4A\x1b[38;2;255;107;128m\
          OAuth error: Invalid code. Please make sure the full code was copied\
          \r\x1b[2B\x1b[39m\x1b[K\r\x1b[1B \x1b[38;2;177;185;249mPress \x1b[1mEnter\x1b[22m \
          to retry.\x1b[39m\x1b[K\r\x1b[5A";

    #[test]
    fn the_real_rejection_frame_is_recognised_after_ansi_stripping() {
        let mut s = AnsiStripper::default();
        let visible = s.push(REJECTION_FRAME);
        assert!(
            detect_code_rejection(&visible),
            "a rejected code must be seen, not waited out: {:?}",
            visible
        );
    }

    #[test]
    fn cursor_motion_inside_the_message_does_not_hide_it() {
        // The layout engine can place these words with column jumps, which
        // become runs of spaces. Matching must survive that.
        let mut s = AnsiStripper::default();
        let visible = s.push(b"\x1b[2GPress\x1b[9GEnter\x1b[16Gto\x1b[20Gretry.");
        assert!(detect_code_rejection(&visible));
    }

    #[test]
    fn ordinary_progress_output_is_not_mistaken_for_a_rejection() {
        assert!(!detect_code_rejection(
            "Browser didn't open? Use the url below to sign in (c to copy)"
        ));
        assert!(!detect_code_rejection("Paste code here if prompted >"));
        assert!(!detect_code_rejection("Login successful! Your token:"));
        assert!(!detect_code_rejection(""));
    }

    /// The retry budget has to be finite: the CLI itself will loop forever, and
    /// a flow that never resolves is the hang this replaced wearing a hat.
    #[test]
    fn the_retry_budget_is_bounded_and_leaves_room_for_a_retry() {
        assert!(MAX_CODE_ATTEMPTS >= 2, "one attempt is not a retry");
        assert!(MAX_CODE_ATTEMPTS <= 5, "the budget must actually run out");
    }

    // ── Bounded buffering ────────────────────────────────────────────────

    #[test]
    fn an_unterminated_control_sequence_cannot_grow_the_carry_without_bound() {
        let mut s = AnsiStripper::default();
        // An OSC introducer with no BEL and no ST. `strip_ansi_prefix` cannot
        // know it has ended, so every byte after it is carried — for the whole
        // 15-minute timeout, if nothing caps it.
        let mut seen = s.push(b"\x1b]8;id=1;");
        for _ in 0..40 {
            seen.push_str(&s.push(&vec![b'A'; 4096]));
        }
        assert!(
            s.carry.len() <= MAX_ANSI_CARRY,
            "carry grew to {} bytes",
            s.carry.len()
        );
        assert!(!seen.is_empty(), "the withheld text must be released, not dropped");
    }

    #[test]
    fn a_token_printed_after_a_runaway_sequence_is_still_found() {
        let tok = token('Q');
        let mut s = AnsiStripper::default();
        let mut seen = s.push(b"\x1b]8;id=1;");
        for _ in 0..40 {
            seen.push_str(&s.push(&vec![b'A'; 4096]));
        }
        seen.push_str(&s.push(format!("\nYour token: {}\n", tok).as_bytes()));
        assert_eq!(parse_setup_token(&seen), Some(tok));
    }

    // ── Revocation: what the sweep leaves behind, and how it is described ──

    use crate::docker::container::SnapshotScrubReport;
    use crate::project_lock::{try_acquire, ProjectOp};

    /// The classifier is a substring match on a message another module owns,
    /// which is only safe if something notices when that module rephrases. So
    /// build the refusal the way production does — by actually losing the
    /// race — rather than by pasting the wording in here.
    #[test]
    fn a_real_lock_refusal_is_recognised_as_retryable() {
        let project = "auth-token-test-busy-project";
        let _held = try_acquire(project, ProjectOp::Compaction).expect("first claim");
        let refusal = try_acquire(project, ProjectOp::SecretScrub)
            .expect_err("a second claim on the same project must be refused");

        assert!(
            is_project_busy_refusal(&refusal),
            "project_lock's refusal is no longer recognised as retryable: {:?}",
            refusal
        );
    }

    #[test]
    fn a_docker_failure_is_not_mistaken_for_a_busy_project() {
        for reason in [
            "could not inspect: error trying to connect: No such file or directory",
            "could not create a scratch container: conflict: name already in use",
            "an untagged snapshot image holds a credential and cannot be rewritten",
        ] {
            assert!(
                !is_project_busy_refusal(reason),
                "{:?} was misclassified as a transient lock refusal",
                reason
            );
        }
    }

    #[test]
    fn summarise_scrub_separates_a_busy_project_from_a_broken_image() {
        let project = "auth-token-test-summarise-busy";
        let _held = try_acquire(project, ProjectOp::Recreate).expect("first claim");
        let refusal = try_acquire(project, ProjectOp::SecretScrub).expect_err("refused");

        let outcome = summarise_scrub(SnapshotScrubReport {
            scrubbed: vec!["triple-c-snapshot-a:latest".into()],
            failed: vec![
                ("triple-c-snapshot-b:latest".into(), refusal),
                (
                    "triple-c-snapshot-c:latest".into(),
                    "could not create a scratch container: no such image".into(),
                ),
            ],
            superseded_retained: vec!["triple-c-snapshot-a:latest".into()],
            unavailable: None,
        });

        assert_eq!(outcome.snapshots_scrubbed, vec!["triple-c-snapshot-a:latest"]);
        assert_eq!(outcome.snapshots_skipped.len(), 1, "{:?}", outcome);
        assert!(outcome.snapshots_skipped[0].starts_with("triple-c-snapshot-b:latest: "));
        assert_eq!(outcome.snapshots_failed.len(), 1, "{:?}", outcome);
        assert!(outcome.snapshots_failed[0].starts_with("triple-c-snapshot-c:latest: "));
        // The whole point: a skipped image is never folded into the scrubbed
        // list, which is what "success" is rendered from.
        assert!(!outcome.snapshots_scrubbed.iter().any(|s| s.contains("snapshot-b")));
    }

    #[test]
    fn a_clean_sweep_needs_no_second_pass() {
        let outcome = summarise_scrub(SnapshotScrubReport {
            scrubbed: vec!["triple-c-snapshot-a:latest".into()],
            ..Default::default()
        });
        assert!(!outcome.needs_another_pass());
    }

    #[test]
    fn a_retained_superseded_image_alone_does_not_ask_for_a_second_pass() {
        // The tag is clean; what is left is untagged and dies with the running
        // container. Asking the user to sweep again would never stop.
        let outcome = summarise_scrub(SnapshotScrubReport {
            scrubbed: vec!["triple-c-snapshot-a:latest".into()],
            superseded_retained: vec!["triple-c-snapshot-a:latest".into()],
            ..Default::default()
        });
        assert!(!outcome.needs_another_pass());
    }

    #[test]
    fn anything_still_holding_the_credential_asks_for_a_second_pass() {
        let skipped = summarise_scrub(SnapshotScrubReport {
            failed: vec![(
                "triple-c-snapshot-b:latest".into(),
                format!("This project is being reset. {}resetting it.", PROJECT_BUSY_MARKER),
            )],
            ..Default::default()
        });
        assert!(skipped.needs_another_pass());
        assert_eq!(skipped.snapshots_skipped.len(), 1);

        let failed = summarise_scrub(SnapshotScrubReport {
            failed: vec![("triple-c-snapshot-c:latest".into(), "could not inspect: boom".into())],
            ..Default::default()
        });
        assert!(failed.needs_another_pass());

        let blind = summarise_scrub(SnapshotScrubReport {
            unavailable: Some("Docker is not running".into()),
            ..Default::default()
        });
        assert!(blind.needs_another_pass());
        assert!(blind.snapshots_scrubbed.is_empty());
    }

    /// The IPC contract the frontend reads. A field renamed on this side and
    /// not on that one is a silent "nothing was skipped".
    #[test]
    fn the_outcome_serialises_under_the_names_the_frontend_reads() {
        let json = serde_json::to_value(ClearTokenOutcome::default()).expect("serialise");
        let object = json.as_object().expect("an object");
        for key in [
            "snapshots_scrubbed",
            "snapshots_failed",
            "snapshots_skipped",
            "snapshots_superseded",
            "docker_unavailable",
        ] {
            assert!(object.contains_key(key), "missing {} in {:?}", key, object);
        }
    }

    // ── The order of a revocation, and what a retry may touch ─────────────
    //
    // `run_cleanup` takes both halves as arguments precisely so this can be
    // asserted with no keychain and no Docker daemon: the recorded order *is*
    // the subject. Sweep-first put a live ~1-year credential behind a
    // per-image inspect/create/commit/rmi loop — minutes, at bollard's
    // 120s-per-request default — during which `has_claude_token` stayed true
    // and `create_container` kept handing the token to anything started.

    /// Records which half ran, in order.
    type Trace = std::sync::Arc<std::sync::Mutex<Vec<&'static str>>>;

    fn trace() -> Trace {
        std::sync::Arc::new(std::sync::Mutex::new(Vec::new()))
    }

    fn scrubbed_one() -> SnapshotScrubReport {
        SnapshotScrubReport {
            scrubbed: vec!["triple-c-snapshot-a:latest".into()],
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn the_keychain_entry_is_gone_before_the_first_image_is_touched() {
        let t = trace();
        let (tk, ts) = (t.clone(), t.clone());

        let outcome = run_cleanup(
            Cleanup::KeychainThenImages,
            move || {
                tk.lock().unwrap().push("keychain");
                Ok(())
            },
            move || async move {
                ts.lock().unwrap().push("sweep");
                scrubbed_one()
            },
        )
        .await
        .expect("a cleanup whose halves both succeed is not an error");

        assert_eq!(
            *t.lock().unwrap(),
            ["keychain", "sweep"],
            "the token stayed in the keychain — and therefore in every container created — \
             for the whole length of the image sweep"
        );
        assert_eq!(outcome.snapshots_scrubbed, vec!["triple-c-snapshot-a:latest"]);
    }

    #[tokio::test]
    async fn a_keychain_failure_leaves_the_images_untouched_and_is_reported() {
        let t = trace();
        let ts = t.clone();

        let err = run_cleanup(
            Cleanup::KeychainThenImages,
            || Err("the keychain is locked".to_string()),
            move || async move {
                ts.lock().unwrap().push("sweep");
                SnapshotScrubReport::default()
            },
        )
        .await
        .expect_err("a keychain that refused the delete must be reported, not swallowed");

        assert_eq!(err, "the keychain is locked");
        assert!(
            t.lock().unwrap().is_empty(),
            "images were rewritten for a revocation that never happened; the report is then \
             discarded with the error and the user is told nothing they can act on"
        );
    }

    /// The bug the images-only primitive exists to close: the "Retry snapshot
    /// cleanup" button used to run `clear_claude_token`, so pressing it after
    /// re-authenticating deleted the brand-new token with no confirmation.
    #[tokio::test]
    async fn an_images_only_cleanup_never_reaches_the_keychain() {
        let t = trace();
        let (tk, ts) = (t.clone(), t.clone());

        let outcome = run_cleanup(
            Cleanup::ImagesOnly,
            move || {
                tk.lock().unwrap().push("keychain");
                Ok(())
            },
            move || async move {
                ts.lock().unwrap().push("sweep");
                scrubbed_one()
            },
        )
        .await
        .expect("a sweep-only cleanup is not an error");

        assert_eq!(
            *t.lock().unwrap(),
            ["sweep"],
            "the retry deleted a credential nobody confirmed deleting"
        );
        assert_eq!(outcome.snapshots_scrubbed, vec!["triple-c-snapshot-a:latest"]);
    }

    /// …and it still has to report what it could not finish, because "run it
    /// again once that project is idle" is the whole affordance.
    #[tokio::test]
    async fn an_images_only_cleanup_still_reports_what_it_could_not_finish() {
        let outcome = run_cleanup(
            Cleanup::ImagesOnly,
            || -> Result<(), String> { unreachable!("images only") },
            || async {
                SnapshotScrubReport {
                    failed: vec![(
                        "triple-c-snapshot-b:latest".into(),
                        format!("This project is being started. {}removing a credential from its snapshot.", PROJECT_BUSY_MARKER),
                    )],
                    ..Default::default()
                }
            },
        )
        .await
        .expect("an image left for the next pass is not a command failure");

        assert!(outcome.needs_another_pass());
        assert_eq!(outcome.snapshots_skipped.len(), 1, "{:?}", outcome);
        assert!(outcome.snapshots_failed.is_empty(), "{:?}", outcome);
    }
}
