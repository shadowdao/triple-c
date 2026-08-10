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

        let body_start = start + TOKEN_PREFIX.len();
        let mut end = body_start;
        while end < bytes.len() && is_token_byte(bytes[end]) {
            end += 1;
        }
        if end - body_start < MIN_TOKEN_BODY {
            continue;
        }

        found = Some(output[start..end].to_string());
    }

    found
}

// ─────────────────────────────────────────────────────────────────────────────
// Redaction
// ─────────────────────────────────────────────────────────────────────────────

/// Mask every *complete* credential in `text`.
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
        let body_start = start + SECRET_MARKER.len();
        let mut end = body_start;
        while end < bytes.len() && is_token_byte(bytes[end]) {
            end += 1;
        }
        if end - body_start < MIN_SECRET_BODY {
            continue;
        }

        out.push_str(&text[copied..start]);
        out.push_str(SECRET_PLACEHOLDER);
        copied = end;
        cursor = end;
    }

    out.push_str(&text[copied..]);
    out
}

/// Where the tail that might still grow into a credential begins. Everything
/// before this index is safe to emit; everything from it must be withheld until
/// more input arrives. Returns `text.len()` when nothing needs withholding.
fn holdback_index(text: &str) -> usize {
    let bytes = text.as_bytes();

    // A credential already under way: the last marker with nothing but token
    // characters after it. If the *last* marker fails that test, no earlier one
    // can pass it either — the disqualifying character lies after them all.
    if let Some(start) = text.rfind(SECRET_MARKER) {
        let clean_start = start == 0 || !is_token_byte(bytes[start - 1]);
        let body_all_token = bytes[start + SECRET_MARKER.len()..]
            .iter()
            .all(|b| is_token_byte(*b));
        if clean_start && body_all_token {
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

/// Strip terminal control sequences from the front of `bytes`, stopping at the
/// first incomplete sequence or truncated character. Returns the clean text and
/// how many bytes were consumed.
fn strip_ansi_prefix(bytes: &[u8]) -> (String, usize) {
    let mut out = String::with_capacity(bytes.len());
    let mut i = 0usize;

    while i < bytes.len() {
        match bytes[i] {
            0x1b => {
                if i + 1 >= bytes.len() {
                    return (out, i);
                }
                match bytes[i + 1] {
                    // CSI: parameter/intermediate bytes, then a final 0x40..=0x7e.
                    b'[' => {
                        let mut j = i + 2;
                        while j < bytes.len() && !(0x40..=0x7e).contains(&bytes[j]) {
                            j += 1;
                        }
                        if j >= bytes.len() {
                            return (out, i);
                        }
                        if CURSOR_MOVE_FINALS.contains(&bytes[j]) {
                            out.push(' ');
                        }
                        i = j + 1;
                    }
                    // OSC: runs until BEL or ST (ESC \).
                    b']' => {
                        let mut j = i + 2;
                        loop {
                            if j >= bytes.len() {
                                return (out, i);
                            }
                            if bytes[j] == 0x07 {
                                j += 1;
                                break;
                            }
                            if bytes[j] == 0x1b {
                                if j + 1 >= bytes.len() {
                                    return (out, i);
                                }
                                if bytes[j + 1] == b'\\' {
                                    j += 2;
                                    break;
                                }
                            }
                            j += 1;
                        }
                        i = j;
                    }
                    // Two-byte escapes (charset selection, keypad mode, …).
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
                    return (out, i);
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
                    return (out, i);
                }
                if let Ok(s) = std::str::from_utf8(&bytes[i..i + len]) {
                    out.push_str(s);
                }
                i += len;
            }
        }
    }

    (out, i)
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
}

impl AnsiStripper {
    fn push(&mut self, chunk: &[u8]) -> String {
        self.carry.extend_from_slice(chunk);
        let (mut out, consumed) = strip_ansi_prefix(&self.carry);
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
            let (more, consumed) = strip_ansi_prefix(&self.carry);
            self.carry.drain(..consumed);
            out.push_str(&more);
        }

        out
    }
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

/// Shell run inside the container.
///
/// * `stty` widens the pty before Claude Code starts, so its layout engine does
///   not wrap the token or the sign-in URL across lines. Docker's default exec
///   pty is 80 columns; both are longer than that. Setting it here rather than
///   via a post-start resize avoids racing the process's startup.
/// * The `unset` line strips inherited auth so `setup-token` runs against a
///   clean claude.ai login instead of warning about, or deferring to, whatever
///   credential the container is already configured with — including a shared
///   token from a previous run, which is likely the very thing being replaced.
const SETUP_TOKEN_SCRIPT: &str = r#"stty cols 200 rows 50 2>/dev/null || true
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
        if visible.is_empty() {
            continue;
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

    let exit_code = wait_for_exec_exit(&exec_id).await.unwrap_or(0);
    if exit_code != 0 {
        return Err(format!(
            "`claude setup-token` exited with status {}. No token was stored — \
             see the command output above for what went wrong.",
            exit_code
        ));
    }

    parse_setup_token(&transcript).ok_or_else(|| {
        "`claude setup-token` finished but printed no recognisable token. \
         Nothing was stored. This usually means the login was cancelled, or the \
         account has no Claude subscription (long-lived tokens require one)."
            .to_string()
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

/// What [`clear_claude_token`] managed to reach. The keychain entry is always
/// gone by the time this is returned — the rest is about copies of the token
/// that live outside it.
#[derive(Debug, Default, serde::Serialize)]
pub struct ClearTokenOutcome {
    /// Snapshot images that were holding the token and have been rewritten.
    pub snapshots_scrubbed: Vec<String>,
    /// Images still holding it, with the reason each could not be rewritten.
    /// Non-empty means the revocation is **incomplete** and the UI must say so.
    pub snapshots_failed: Vec<String>,
    /// Rewritten, but the pre-rewrite image object could not be deleted because
    /// a container is still running off it. Worth mentioning, not worth
    /// alarming about — see `SnapshotScrubReport::superseded_retained`.
    pub snapshots_superseded: Vec<String>,
    /// Set when Docker could not be reached at all, so nothing is known about
    /// what is still on disk.
    pub docker_unavailable: Option<String>,
}

/// Forget the shared Claude token.
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
///    [`scrub_secrets_from_snapshots`] does here.
///
/// The keychain deletion is never rolled back if the scrub fails; a partially
/// completed revocation is still better than none, and the outcome is reported
/// so the UI can be explicit about what is left.
#[tauri::command]
pub async fn clear_claude_token() -> Result<ClearTokenOutcome, String> {
    secure::delete_claude_oauth_token()?;
    log::info!("Cleared the shared Claude authentication token");

    let report = crate::docker::container::scrub_secrets_from_snapshots().await;
    if report.left_something_behind() {
        log::warn!(
            "Revoked the shared Claude token but {} snapshot image(s) may still contain it",
            report.failed.len()
        );
    }

    Ok(ClearTokenOutcome {
        snapshots_scrubbed: report.scrubbed,
        snapshots_failed: report
            .failed
            .into_iter()
            .map(|(image, reason)| format!("{}: {}", image, reason))
            .collect(),
        snapshots_superseded: report.superseded_retained,
        docker_unavailable: report.unavailable,
    })
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
    // ── Truncated credentials ────────────────────────────────────────────
    // `stty cols 200` runs before Claude Code starts precisely so the ~103
    // character token never wraps. If that fails, the pty falls back to 80
    // columns and the token arrives split across two lines. The old floor of
    // 32 body characters believed the first half.

    #[test]
    fn a_token_shorter_than_a_real_one_is_rejected() {
        let fragment = format!("{}{}", TOKEN_PREFIX, "M".repeat(MIN_TOKEN_BODY - 1));
        assert_eq!(
            parse_setup_token(&format!("Your token: {}\n", fragment)),
            None
        );
    }

    #[test]
    fn a_line_wrapped_token_yields_nothing_rather_than_half_a_credential() {
        let tok = token('N');
        // Column 12 is where `Your token: ` ends, so an 80-column pty breaks
        // the value 68 characters in.
        let wrapped = format!("Your token: {}\n{}\n", &tok[..68], &tok[68..]);
        assert_eq!(
            parse_setup_token(&wrapped),
            None,
            "a wrapped token must fail loudly, not be stored truncated"
        );
    }

    #[test]
    fn a_real_length_token_is_still_accepted() {
        // Guards the floor from being raised past what Anthropic actually mints.
        let tok = token('O');
        assert_eq!(tok.len() - TOKEN_PREFIX.len(), 90);
        assert!(90 > MIN_TOKEN_BODY);
        assert_eq!(parse_setup_token(&format!("{}\n", tok)), Some(tok));
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
}
