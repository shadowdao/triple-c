//! GitHub sign-in through `gh auth login --web` inside a running container, for
//! hosts that have no `gh` of their own. The token is read back through the
//! exec, returned to the caller for the keychain, and never emitted, logged or
//! left behind in the container.

use std::time::Duration;

use bollard::container::LogOutput;
use futures_util::{Stream, StreamExt};
use tauri::{AppHandle, Emitter};
use tokio::io::{AsyncWrite, AsyncWriteExt};
use tokio::sync::oneshot;

use crate::commands::auth_token_commands::{push_capped_tail, AnsiStripper, SUBMIT_ENTER_DELAY};
use crate::docker::exec::{
    create_attached_exec_as, exec_oneshot_as, wait_for_exec_exit, AttachedExec,
};

pub const CODE_EVENT: &str = "marketplace-gh-login-code";
pub const OUTPUT_EVENT: &str = "marketplace-gh-login-output";

const LOGIN_TIMEOUT: Duration = Duration::from_secs(10 * 60);
const TOKEN_BEGIN: &str = "__TRIPLEC_TOKEN_BEGIN__";
const TOKEN_END: &str = "__TRIPLEC_TOKEN_END__";
/// Common prefix of both markers: any line containing it is never shown.
const TOKEN_MARKER: &str = "__TRIPLEC_TOKEN";
const MAX_TRANSCRIPT: usize = 64 * 1024;
const MAX_PENDING_LINE: usize = 4096;

/// Pre-flight N9: on cancel or timeout the attach is dropped, but `gh auth
/// login` would keep polling in the container. This matches both it and the
/// script around it (whose text contains the same words); errors are ignored.
const CANCEL_PKILL: [&str; 3] = ["pkill", "-f", "gh auth login --hostname"];

/// Constant script; the host is `$1` (argv, never interpolated), because
/// `create_attached_exec_as` takes no env.
///
/// * `GH_CONFIG_DIR` / `GIT_CONFIG_GLOBAL` live in a temp dir removed on exit,
///   so the container is never left logged in. The `HUP INT TERM` trap turns a
///   signal (the pty closing, or the cancel `pkill`) into a normal exit so the
///   `EXIT` trap still runs — `sh` skips it when killed outright.
/// * `--git-protocol ssh --skip-ssh-key` avoids gh's "Authenticate Git with
///   your GitHub credentials?" prompt, which `https` triggers and which would
///   write a credential helper into the git config.
/// * `BROWSER=true` makes gh's "open the browser" step a no-op.
const GH_LOGIN_SCRIPT: &str = r#"set -eu
host="$1"
case "$host" in
  '' | -* | *[!A-Za-z0-9.-]*) echo "invalid host" >&2; exit 2 ;;
esac
export HOME=/home/claude
d=$(mktemp -d)
trap 'rm -rf "$d"' EXIT
trap 'exit 130' HUP INT TERM
export GH_CONFIG_DIR="$d" GIT_CONFIG_GLOBAL="$d/gitconfig" BROWSER=true
gh auth login --hostname "$host" --web --git-protocol ssh --skip-ssh-key --scopes repo
t=$(gh auth token --hostname "$host")
printf '\n%s%s%s\n' __TRIPLEC_TOKEN_BEGIN__ "$t" __TRIPLEC_TOKEN_END__
"#;

/// Pre-flight F13: the shared host rule, minus ports — `gh auth login
/// --hostname` takes a bare name.
pub fn valid_host(host: &str) -> bool {
    crate::marketplace::auth::valid_host(host) && !host.contains(':')
}

/// Remove terminal control sequences and carriage returns from one complete
/// piece of text. An unterminated sequence at the end is dropped. The login
/// itself uses a streaming [`AnsiStripper`], which carries a sequence split
/// across chunks instead; this one-shot form exists for tests only.
#[cfg(test)]
fn strip_ansi(s: &str) -> String {
    AnsiStripper::default().push(s.as_bytes())
}

/// Read gh's device code and URL. Returns (code, url).
///
/// Two wordings are known:
/// * older gh: `! First copy your one-time code: XXXX-XXXX`, then either a
///   URL or "Press Enter to open <host> in your browser";
/// * gh 2.101 (the image's): `! One-time code (XXXX-XXXX) copied to
///   clipboard`, then "Press Enter to open https://<host>/login/device in your
///   browser...".
///
/// The URL is the first `https://…/login/device` word, else
/// `https://<host>/login/device`.
pub fn parse_device_prompt(output: &str, host: &str) -> Option<(String, String)> {
    const LABEL: &str = "one-time code";
    // ASCII lowercasing keeps byte offsets, so `at` indexes `output` too.
    let at = output.to_ascii_lowercase().find(LABEL)? + LABEL.len();
    let rest = output[at..].trim_start_matches(|c: char| c == ':' || c == '(' || c.is_whitespace());
    let code: String = rest
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '-')
        .collect();
    // Something must follow the code (`)` or a line break): a code at the
    // very end may still be growing in the next frame.
    if code.len() < 6 || !code.contains('-') || rest.len() == code.len() {
        return None;
    }
    let url = output
        .split_whitespace()
        .find(|w| w.starts_with("https://") && w.contains("/login/device"))
        .map(|w| {
            w.trim_end_matches(|c: char| !c.is_ascii_alphanumeric() && c != '/')
                .to_string()
        })
        .unwrap_or_else(|| format!("https://{host}/login/device"));
    Some((code, url))
}

pub fn extract_token(text: &str) -> Option<String> {
    let start = text.find(TOKEN_BEGIN)? + TOKEN_BEGIN.len();
    let end = start + text[start..].find(TOKEN_END)?;
    let token = text[start..end].trim();
    if token.is_empty() || token.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return None;
    }
    Some(token.to_string())
}

/// Append `chunk` and hand back the complete lines, minus any line carrying the
/// token markers. A partial line waits in `pending` (so a marker split across
/// chunks is never shown), and is dropped if it grows past a bound.
pub fn take_display_lines(pending: &mut String, chunk: &str) -> String {
    pending.push_str(chunk);
    let Some(last_nl) = pending.rfind('\n') else {
        if pending.len() > MAX_PENDING_LINE {
            pending.clear();
        }
        return String::new();
    };
    let complete: String = pending.drain(..=last_nl).collect();
    complete
        .lines()
        .filter(|l| !l.contains(TOKEN_MARKER))
        .map(|l| format!("{l}\n"))
        .collect()
}

/// What to show when the login ends without a token: the last few lines, with
/// any marker line removed.
fn failure_tail(transcript: &str) -> String {
    let lines: Vec<&str> = transcript
        .lines()
        .filter(|l| !l.contains(TOKEN_MARKER) && !l.trim().is_empty())
        .collect();
    lines[lines.len().saturating_sub(5)..].join("\n")
}

/// Pre-flight N9 / review fix 1: stop the in-container login after any
/// failed attempt.
async fn kill_container_login(container_id: &str) {
    let cmd = CANCEL_PKILL.iter().map(|s| s.to_string()).collect();
    let _ = exec_oneshot_as(container_id, "claude", cmd, vec![]).await;
}

/// Hand `result` back, running `cleanup` first when it is a failure.
///
/// Review fix 1: a tty exec keeps running after its attach is dropped, so any
/// login that ends without a token — cancel, timeout, a lost stream, a failed
/// write, gh exiting without one — must stop the in-container login, or gh
/// keeps polling and its temp `GH_CONFIG_DIR` (which receives the token if the
/// user finishes in the browser) outlives the attempt.
async fn cleanup_on_error<T, C, Fut>(result: Result<T, String>, cleanup: C) -> Result<T, String>
where
    C: FnOnce() -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    if result.is_err() {
        cleanup().await;
    }
    result
}

/// Pump gh's output until the exec ends, emitting code and display events and
/// pressing Enter at gh's prompt. `Ok` is the transcript of a stream that ended
/// normally; every other ending is `Err`. Takes the attach halves by value, so
/// they are closed by the time this returns.
async fn drive_login<S, W, E>(
    mut output: S,
    mut input: W,
    cancel: &mut oneshot::Receiver<()>,
    deadline: tokio::time::Instant,
    account_id: &str,
    host: &str,
    mut emit: E,
) -> Result<String, String>
where
    S: Stream<Item = Result<LogOutput, bollard::errors::Error>> + Unpin,
    W: AsyncWrite + Unpin,
    E: FnMut(&'static str, serde_json::Value),
{
    let mut stripper = AnsiStripper::default();
    let mut transcript = String::new();
    let mut pending = String::new();
    let mut code_sent = false;
    let mut enter_sent = false;

    loop {
        let next = tokio::select! {
            _ = &mut *cancel => {
                return Err("GitHub sign-in cancelled. Nothing was stored.".to_string());
            }
            next = tokio::time::timeout_at(deadline, output.next()) => match next {
                Ok(next) => next,
                Err(_) => {
                    return Err(format!(
                        "Timed out after {} minutes waiting for the GitHub sign-in. Nothing was stored.",
                        LOGIN_TIMEOUT.as_secs() / 60
                    ));
                }
            },
        };
        let frame = match next {
            Some(Ok(frame)) => frame,
            Some(Err(e)) => {
                return Err(format!(
                    "Lost the connection to gh: {e}. Nothing was stored."
                ))
            }
            None => return Ok(transcript),
        };
        let text = stripper.push(&frame.into_bytes());
        push_capped_tail(&mut transcript, &text, MAX_TRANSCRIPT);

        let shown = take_display_lines(&mut pending, &text);
        if !shown.is_empty() {
            emit(
                OUTPUT_EVENT,
                serde_json::json!({ "account_id": account_id, "chunk": shown }),
            );
        }
        if !code_sent {
            if let Some((code, url)) = parse_device_prompt(&transcript, host) {
                emit(
                    CODE_EVENT,
                    serde_json::json!({ "account_id": account_id, "code": code, "url": url }),
                );
                code_sent = true;
            }
        }
        if code_sent && !enter_sent && transcript.contains("Press Enter") {
            // The Enter is its own write, after a pause (PR #64): arriving with
            // other bytes it can be read as part of a paste and swallowed.
            tokio::time::sleep(SUBMIT_ENTER_DELAY).await;
            input
                .write_all(b"\r")
                .await
                .map_err(|e| format!("Could not answer gh's prompt: {e}. Nothing was stored."))?;
            let _ = input.flush().await;
            enter_sent = true;
        }
    }
}

/// Run `gh auth login --web` in the container and return the token it minted.
///
/// Once the exec exists there is exactly one way out: the result of the inner
/// block goes through [`cleanup_on_error`], so only a token read back skips
/// the in-container kill.
pub async fn run_gh_container_login(
    app: &AppHandle,
    account_id: &str,
    container_id: &str,
    host: &str,
    mut cancel: oneshot::Receiver<()>,
) -> Result<String, String> {
    if !valid_host(host) {
        return Err(format!("{host:?} is not a valid host name."));
    }
    let AttachedExec {
        exec_id,
        output,
        input,
    } = create_attached_exec_as(
        container_id,
        vec![
            "sh".to_string(),
            "-c".to_string(),
            GH_LOGIN_SCRIPT.to_string(),
            "triple-c-gh-login".to_string(),
            host.to_string(),
        ],
        true,
        "claude",
        "/home/claude",
    )
    .await?;

    let deadline = tokio::time::Instant::now() + LOGIN_TIMEOUT;
    let result = async {
        let transcript = drive_login(
            output,
            input,
            &mut cancel,
            deadline,
            account_id,
            host,
            |event, payload| {
                let _ = app.emit(event, payload);
            },
        )
        .await?;
        if let Some(token) = extract_token(&transcript) {
            return Ok(token);
        }
        let status = wait_for_exec_exit(&exec_id).await;
        Err(format!(
            "gh did not complete the sign-in (exit status {}). Nothing was stored.\n{}",
            status
                .map(|c| c.to_string())
                .unwrap_or_else(|| "unknown".to_string()),
            failure_tail(&transcript)
        ))
    }
    .await;
    cleanup_on_error(result, || kill_container_login(container_id)).await
}

#[cfg(test)]
mod tests {
    use super::*;

    const GH_PROMPT: &str = "! First copy your one-time code: 4F2A-9C1B\nPress Enter to open github.com in your browser... ";

    #[test]
    fn the_device_code_is_read_and_the_url_defaults_to_the_host() {
        assert_eq!(
            parse_device_prompt(GH_PROMPT, "github.com"),
            Some((
                "4F2A-9C1B".to_string(),
                "https://github.com/login/device".to_string()
            ))
        );
    }

    #[test]
    fn an_explicit_device_url_wins() {
        let out = "! First copy your one-time code: AB12-CD34\nOpen this URL to continue in your web browser: https://ghe.example.com/login/device\n";
        assert_eq!(
            parse_device_prompt(out, "ghe.example.com"),
            Some((
                "AB12-CD34".to_string(),
                "https://ghe.example.com/login/device".to_string()
            ))
        );
    }

    /// gh 2.101.0 (the image's gh, integration report check 6), after ANSI
    /// stripping: the code is in parentheses and the URL is on the Enter line.
    const GH_2_101_PROMPT: &str = "! One-time code (4F2A-9C1B) copied to clipboard\nPress Enter to open https://github.com/login/device in your browser... ";

    #[test]
    fn the_gh_2_101_wording_is_read() {
        assert_eq!(
            parse_device_prompt(GH_2_101_PROMPT, "github.com"),
            Some((
                "4F2A-9C1B".to_string(),
                "https://github.com/login/device".to_string()
            ))
        );
    }

    #[test]
    fn the_url_comes_from_the_press_enter_line() {
        let out = "! One-time code (AB12-CD34) copied to clipboard\nPress Enter to open https://ghe.example.com/login/device in your browser... ";
        assert_eq!(
            parse_device_prompt(out, "github.com"),
            Some((
                "AB12-CD34".to_string(),
                "https://ghe.example.com/login/device".to_string()
            ))
        );
    }

    #[test]
    fn the_gh_2_101_wording_without_a_code_is_no_prompt() {
        assert_eq!(parse_device_prompt("! One-time code (", "github.com"), None);
        assert_eq!(
            parse_device_prompt("! One-time code (4F2A", "github.com"),
            None
        );
    }

    #[test]
    fn a_code_cut_by_a_frame_boundary_is_not_a_code_yet() {
        assert_eq!(
            parse_device_prompt("! One-time code (4F2A-9C", "github.com"),
            None
        );
        assert_eq!(
            parse_device_prompt("! First copy your one-time code: 4F2A-9C", "github.com"),
            None
        );
    }

    #[test]
    fn no_code_yet_means_no_prompt() {
        assert_eq!(
            parse_device_prompt("! First copy your one-time", "github.com"),
            None
        );
        assert_eq!(parse_device_prompt("", "github.com"), None);
    }

    #[test]
    fn the_token_is_taken_from_between_the_markers() {
        let out = "✓ Logged in\n__TRIPLEC_TOKEN_BEGIN__test-token-not-real__TRIPLEC_TOKEN_END__\n";
        assert_eq!(extract_token(out), Some("test-token-not-real".to_string()));
        assert_eq!(
            extract_token("__TRIPLEC_TOKEN_BEGIN__test-token-not-real"),
            None,
            "unterminated"
        );
        assert_eq!(
            extract_token("__TRIPLEC_TOKEN_BEGIN____TRIPLEC_TOKEN_END__"),
            None,
            "empty"
        );
        assert_eq!(
            extract_token("__TRIPLEC_TOKEN_BEGIN__a b__TRIPLEC_TOKEN_END__"),
            None,
            "whitespace"
        );
    }

    #[test]
    fn only_complete_lines_are_shown_and_the_token_line_never_is() {
        let mut pending = String::new();
        assert_eq!(
            take_display_lines(&mut pending, "! First copy your one-"),
            ""
        );
        assert_eq!(
            take_display_lines(&mut pending, "time code: 4F2A-9C1B\nPress"),
            "! First copy your one-time code: 4F2A-9C1B\n"
        );
        assert_eq!(pending, "Press");
        let shown = take_display_lines(
            &mut pending,
            " Enter\n__TRIPLEC_TOKEN_BEGIN__test-token-not-real__TRIPLEC_TOKEN_END__\ndone\n",
        );
        assert_eq!(shown, "Press Enter\ndone\n");
        assert!(!shown.contains("test-token-not-real"));
    }

    #[test]
    fn escape_sequences_and_carriage_returns_are_removed() {
        assert_eq!(strip_ansi("\u{1b}[1;32m✓\u{1b}[0m done\r\n"), "✓ done\n");
        assert_eq!(
            strip_ansi("a\u{1b}]8;;https://x\u{7}link\u{1b}]8;;\u{7}b"),
            "alinkb"
        );
        assert_eq!(strip_ansi("cut\u{1b}["), "cut");
    }

    #[test]
    fn hosts_are_plain_names() {
        assert!(valid_host("github.com"));
        assert!(valid_host("ghe.corp-1.example"));
        for bad in ["", "-x", "a b", "a;b", "a/b", "$(id)"] {
            assert!(!valid_host(bad), "{bad:?}");
        }
    }

    /// Pre-flight F13: the shared `auth::valid_host` accepts `host:port`, but
    /// `gh auth login --hostname` takes a bare name, so a port is refused here.
    #[test]
    fn hosts_with_a_port_are_refused() {
        assert!(crate::marketplace::auth::valid_host("ghe.corp:8443"));
        assert!(!valid_host("ghe.corp:8443"));
        assert!(!valid_host("ghe.corp:"));
    }

    #[test]
    fn the_failure_tail_never_carries_the_token() {
        let transcript = "! First copy your one-time code: 4F2A-9C1B\n\
            __TRIPLEC_TOKEN_BEGIN__test-token-not-real__TRIPLEC_TOKEN_END__\n\
            error: something odd\n";
        let tail = failure_tail(transcript);
        assert!(!tail.contains("test-token-not-real"));
        assert!(tail.contains("error: something odd"));
    }

    /// Pre-flight N9: the cancel/timeout `pkill -f` pattern has to match the
    /// `gh` command line the script runs.
    #[test]
    fn the_cancel_pattern_matches_the_script() {
        assert_eq!(CANCEL_PKILL[0], "pkill");
        assert_eq!(CANCEL_PKILL[1], "-f");
        assert!(GH_LOGIN_SCRIPT.contains(CANCEL_PKILL[2]));
    }

    /// Review fix 1: every failed login tears the container side down, and a
    /// successful one does not.
    mod teardown {
        use super::super::*;
        use bollard::container::LogOutput;
        use futures_util::stream;
        use std::pin::Pin;
        use std::sync::{Arc, Mutex};
        use std::task::{Context, Poll};

        type Frame = Result<LogOutput, bollard::errors::Error>;

        fn out(s: &'static str) -> Frame {
            Ok(LogOutput::StdOut { message: s.into() })
        }

        fn lost() -> Frame {
            Err(bollard::errors::Error::DockerResponseServerError {
                status_code: 500,
                message: "connection reset".to_string(),
            })
        }

        /// Records every write separately; or fails every write.
        #[derive(Clone, Default)]
        struct Keys {
            writes: Arc<Mutex<Vec<Vec<u8>>>>,
            broken: bool,
        }

        impl tokio::io::AsyncWrite for Keys {
            fn poll_write(
                self: Pin<&mut Self>,
                _: &mut Context<'_>,
                buf: &[u8],
            ) -> Poll<std::io::Result<usize>> {
                if self.broken {
                    return Poll::Ready(Err(std::io::Error::other("pipe closed")));
                }
                self.writes.lock().unwrap().push(buf.to_vec());
                Poll::Ready(Ok(buf.len()))
            }
            fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
                Poll::Ready(Ok(()))
            }
            fn poll_shutdown(
                self: Pin<&mut Self>,
                _: &mut Context<'_>,
            ) -> Poll<std::io::Result<()>> {
                Poll::Ready(Ok(()))
            }
        }

        const PROMPT: &str = "! First copy your one-time code: 4F2A-9C1B\r\nPress Enter to open github.com in your browser... ";

        async fn drive<S>(
            frames: S,
            keys: Keys,
            cancel: &mut oneshot::Receiver<()>,
            deadline: tokio::time::Instant,
        ) -> (
            Result<String, String>,
            Vec<(&'static str, serde_json::Value)>,
        )
        where
            S: futures_util::Stream<Item = Frame> + Unpin,
        {
            let mut events = Vec::new();
            let r = drive_login(
                frames,
                keys,
                cancel,
                deadline,
                "acct-1",
                "github.com",
                |e, p| events.push((e, p)),
            )
            .await;
            (r, events)
        }

        fn far() -> tokio::time::Instant {
            tokio::time::Instant::now() + LOGIN_TIMEOUT
        }

        #[tokio::test]
        async fn cleanup_runs_on_every_failure_and_never_on_success() {
            let runs = Arc::new(Mutex::new(0));
            let count = || {
                let runs = runs.clone();
                async move { *runs.lock().unwrap() += 1 }
            };
            let ok: Result<String, String> = Ok("test-token-not-real".into());
            assert!(cleanup_on_error(ok, count).await.is_ok());
            assert_eq!(*runs.lock().unwrap(), 0);
            let err: Result<String, String> = Err("boom".into());
            assert_eq!(cleanup_on_error(err, count).await, Err("boom".into()));
            assert_eq!(*runs.lock().unwrap(), 1);
        }

        #[tokio::test(start_paused = true)]
        async fn a_complete_login_returns_the_transcript_and_presses_enter_alone() {
            let keys = Keys::default();
            let (_tx, mut cancel) = oneshot::channel();
            let frames = stream::iter(vec![
                out(PROMPT),
                out("\r\n\u{2713} Logged in\r\n"),
                out("__TRIPLEC_TOKEN_BEGIN__test-token-"),
                out("not-real__TRIPLEC_TOKEN_END__\r\n"),
            ]);
            let (r, events) = drive(frames, keys.clone(), &mut cancel, far()).await;
            let transcript = r.unwrap();
            assert_eq!(
                extract_token(&transcript),
                Some("test-token-not-real".into())
            );
            assert_eq!(*keys.writes.lock().unwrap(), vec![b"\r".to_vec()]);
            assert!(events.contains(&(
                CODE_EVENT,
                serde_json::json!({
                    "account_id": "acct-1",
                    "code": "4F2A-9C1B",
                    "url": "https://github.com/login/device"
                })
            )));
            for (_, payload) in &events {
                assert!(!payload.to_string().contains("test-token-not-real"));
            }
        }

        /// The raw bytes gh 2.101.0 prints under a tty (integration report
        /// check 6), with a fake code: the code event goes out and Enter is
        /// pressed, or gh never starts polling.
        #[tokio::test(start_paused = true)]
        async fn gh_2_101_gets_its_code_event_and_its_enter() {
            let keys = Keys::default();
            let (_tx, mut cancel) = oneshot::channel();
            let frames = stream::iter(vec![
                out("\u{1b}]11;?\u{1b}\\\u{1b}[6n"),
                out("\r\n"),
                out("\u{1b}]52;c;NEYyQS05QzFC\u{7}\u{1b}[0;33m!\u{1b}[0m One-time code (\u{1b}[0;1;39m4F2A-9C1B\u{1b}[0m) copied to clipboard\r\n\u{1b}[0;1;39mPress Enter\u{1b}[0m to open https://github.com/login/device in your browser... "),
            ]);
            let (r, events) = drive(frames, keys.clone(), &mut cancel, far()).await;
            assert!(r.is_ok());
            assert_eq!(*keys.writes.lock().unwrap(), vec![b"\r".to_vec()]);
            assert!(events.contains(&(
                CODE_EVENT,
                serde_json::json!({
                    "account_id": "acct-1",
                    "code": "4F2A-9C1B",
                    "url": "https://github.com/login/device"
                })
            )));
        }

        #[tokio::test]
        async fn a_lost_stream_is_a_failure() {
            let (_tx, mut cancel) = oneshot::channel();
            let frames = stream::iter(vec![out(PROMPT), lost()]);
            let (r, _) = drive(frames, Keys::default(), &mut cancel, far()).await;
            assert!(r.unwrap_err().contains("Lost the connection"));
        }

        #[tokio::test(start_paused = true)]
        async fn a_failed_enter_is_a_failure() {
            let keys = Keys {
                broken: true,
                ..Default::default()
            };
            let (_tx, mut cancel) = oneshot::channel();
            let frames = stream::iter(vec![out(PROMPT)]);
            let (r, _) = drive(frames, keys, &mut cancel, far()).await;
            assert!(r.unwrap_err().contains("Could not answer"));
        }

        #[tokio::test]
        async fn a_cancel_is_a_failure() {
            let (tx, mut cancel) = oneshot::channel();
            tx.send(()).unwrap();
            let (r, _) = drive(stream::pending(), Keys::default(), &mut cancel, far()).await;
            assert!(r.unwrap_err().contains("cancelled"));
        }

        #[tokio::test(start_paused = true)]
        async fn a_timeout_is_a_failure() {
            let (_tx, mut cancel) = oneshot::channel();
            let deadline = tokio::time::Instant::now() + Duration::from_secs(1);
            let (r, _) = drive(stream::pending(), Keys::default(), &mut cancel, deadline).await;
            assert!(r.unwrap_err().contains("Timed out"));
        }
    }

    /// The script end to end against a stand-in `gh`, as a login would run it
    /// inside the container (minus Docker).
    #[cfg(unix)]
    mod script {
        use super::super::*;
        use std::os::unix::fs::PermissionsExt;
        use std::path::{Path, PathBuf};
        use std::process::{Command, Stdio};

        /// A fake `gh` that records its environment into `log_dir` and prints
        /// the fixture token for `auth token`. `login_body` runs for `auth login`.
        fn fake_gh(dir: &Path, log_dir: &Path, login_body: &str) -> PathBuf {
            let bin = dir.join("bin");
            std::fs::create_dir_all(&bin).unwrap();
            let gh = bin.join("gh");
            std::fs::write(
                &gh,
                format!(
                    "#!/bin/sh\n\
                     log='{log}'\n\
                     case \"$1 $2\" in\n\
                     'auth login')\n\
                       printf '%s\\n' \"$GH_CONFIG_DIR\" > \"$log/config_dir\"\n\
                       printf '%s\\n' \"$GIT_CONFIG_GLOBAL\" > \"$log/git_config\"\n\
                       printf '%s\\n' \"$BROWSER\" > \"$log/browser\"\n\
                       printf '%s\\n' \"$*\" > \"$log/args\"\n\
                       echo 'token-in-config' > \"$GH_CONFIG_DIR/hosts.yml\"\n\
                       {login}\n\
                       ;;\n\
                     'auth token') echo test-token-not-real ;;\n\
                     *) exit 9 ;;\n\
                     esac\n",
                    log = log_dir.display(),
                    login = login_body,
                ),
            )
            .unwrap();
            std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();
            bin
        }

        fn script_command(bin: &Path, tmp: &Path, host: &str) -> Command {
            let mut cmd = Command::new("sh");
            cmd.arg("-c")
                .arg(GH_LOGIN_SCRIPT)
                .arg("triple-c-gh-login")
                .arg(host)
                .env(
                    "PATH",
                    format!("{}:{}", bin.display(), std::env::var("PATH").unwrap()),
                )
                .env("TMPDIR", tmp);
            cmd
        }

        fn read(p: PathBuf) -> String {
            std::fs::read_to_string(p).unwrap().trim().to_string()
        }

        #[test]
        fn the_token_comes_back_and_the_temp_config_is_gone() {
            let root = tempfile::tempdir().unwrap();
            let log = root.path().join("log");
            let tmp = root.path().join("tmp");
            std::fs::create_dir_all(&log).unwrap();
            std::fs::create_dir_all(&tmp).unwrap();
            let bin = fake_gh(root.path(), &log, "echo '✓ Logged in'");

            let out = script_command(&bin, &tmp, "github.com").output().unwrap();
            assert!(
                out.status.success(),
                "{}",
                String::from_utf8_lossy(&out.stderr)
            );
            let stdout = String::from_utf8_lossy(&out.stdout);
            assert_eq!(
                extract_token(&stdout),
                Some("test-token-not-real".to_string())
            );

            let config_dir = read(log.join("config_dir"));
            assert!(
                config_dir.starts_with(tmp.to_str().unwrap()),
                "{config_dir}"
            );
            assert!(
                !Path::new(&config_dir).exists(),
                "temp GH_CONFIG_DIR left behind"
            );
            assert_eq!(
                read(log.join("git_config")),
                format!("{config_dir}/gitconfig")
            );
            assert_eq!(read(log.join("browser")), "true");
            assert_eq!(
                read(log.join("args")),
                "auth login --hostname github.com --web --git-protocol ssh --skip-ssh-key --scopes repo"
            );
            assert_eq!(std::fs::read_dir(&tmp).unwrap().count(), 0);
        }

        #[test]
        fn the_script_refuses_a_bad_host_on_its_own() {
            let root = tempfile::tempdir().unwrap();
            let log = root.path().join("log");
            std::fs::create_dir_all(&log).unwrap();
            let bin = fake_gh(root.path(), &log, "true");
            for bad in ["", "-x", "a;b", "$(id)", "a:1"] {
                let out = script_command(&bin, root.path(), bad).output().unwrap();
                assert_eq!(out.status.code(), Some(2), "{bad:?}");
                assert!(!log.join("args").exists(), "gh ran for {bad:?}");
            }
        }

        /// Pre-flight N9: a cancel `pkill`s the login; the temp config must
        /// still be removed when the script dies by signal.
        #[test]
        fn a_killed_login_still_removes_the_temp_config() {
            use std::os::unix::process::CommandExt;

            let root = tempfile::tempdir().unwrap();
            let log = root.path().join("log");
            let tmp = root.path().join("tmp");
            std::fs::create_dir_all(&log).unwrap();
            std::fs::create_dir_all(&tmp).unwrap();
            let bin = fake_gh(root.path(), &log, "touch \"$log/started\"; sleep 30");

            let mut child = script_command(&bin, &tmp, "github.com")
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .process_group(0)
                .spawn()
                .unwrap();
            let started = log.join("started");
            for _ in 0..200 {
                if started.exists() {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(25));
            }
            assert!(started.exists(), "fake gh never started");
            let config_dir = read(log.join("config_dir"));
            assert!(Path::new(&config_dir).exists());

            // Like `pkill -f`, which matches both the script and gh.
            let pgid = child.id().to_string();
            let killed = Command::new("kill")
                .args(["-s", "TERM", "--", &format!("-{pgid}")])
                .status()
                .unwrap();
            assert!(killed.success(), "kill failed");
            let sent = std::time::Instant::now();
            child.wait().unwrap();
            assert!(
                sent.elapsed() < std::time::Duration::from_secs(10),
                "the script outlived the signal"
            );
            assert!(
                !Path::new(&config_dir).exists(),
                "temp GH_CONFIG_DIR left behind"
            );
        }
    }
}
