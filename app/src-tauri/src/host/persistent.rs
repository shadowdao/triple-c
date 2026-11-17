//! Persistent terminals on AWS workspaces: every terminal tab (Claude and
//! bash) runs inside its own tmux server in the workspace, so it outlives the
//! app closing or the SSM tunnel dropping, and the app attaches to it again on
//! its next connect. Docker projects never come here.
//!
//! The rules, and why:
//!
//! * **One tmux server per tab**, `tmux -L triple-c-<session id>`. A server
//!   takes its environment from the client that started it, so each tab's
//!   server has exactly that tab's exec environment — the copy-at-connect
//!   credential pairs (`aws_host::terminal_env`) travel in `ExecSpec.env` as
//!   before and never on argv (`new-session -e` would put them in `ps`).
//! * **The tab's session id is the key.** The frontend's UUID, persisted per
//!   project by `storage::terminal_store`, so a tab reopened after an app
//!   restart gets the same id (its rename, `renamed_session_names`, too).
//! * **Opening** a tab is `new-session -A` (attach when it already exists);
//!   **reattaching** is `attach-session`, which never creates: a session that
//!   ended while the app was away must not come back as a fresh `claude`.
//! * **Closing a tab ends the session** (`kill-server`), as today. App exit, a
//!   dropped tunnel and the agent's SIGHUP on a client that went away only
//!   **detach** — the server daemonised into its own session, so the hangup
//!   reaches the client alone.
//! * The tmux config is `/etc/triple-c/tmux.conf` (`container/triple-c-tmux.conf`):
//!   no prefix key, no status line, output in xterm.js's own scrollback.
//! * An image without tmux or that config runs terminals the old way, with a
//!   one-time notice per tunnel ([`plan`]).
//!
//! Everything in here runs through [`ContainerTarget`].

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::ContainerTarget;

/// The tmux config baked into the image (outside the seed-only home).
pub const TMUX_CONF: &str = "/etc/triple-c/tmux.conf";
/// Socket name prefix (`-L`): `/tmp/tmux-<uid>/triple-c-<key>`.
pub const SOCKET_PREFIX: &str = "triple-c-";
/// The outer terminal the tmux client talks to: xterm.js. The agent would
/// otherwise default a tty exec to `TERM=xterm`, and tmux would then
/// downsample every colour to 8.
pub const OUTER_TERM: &str = "TERM=xterm-256color";
/// tmux user option holding the tab's label, for a session another machine
/// opened (the app's own sessions keep theirs in the store).
pub const NAME_OPTION: &str = "@triple_c_name";
/// How much pane history a re-opened tab gets replayed.
pub const HISTORY_LINES: u32 = 5000;
/// Byte cap on that replay (escape sequences included).
pub const HISTORY_LIMIT: usize = 4 * 1024 * 1024;
/// Every small tmux exec (probe, list, has-session, kill).
pub const SMALL_EXEC_TIMEOUT: Duration = Duration::from_secs(20);
/// The notice an image without tmux gets, once per tunnel.
pub const NO_TMUX_NOTICE: &str = "This workspace image has no tmux, so terminals end when the app \
     closes. Use an image built after persistent terminals were added (e.g. \
     ghcr.io/shadowdao/triple-c-sandbox:edge).";
/// The longest tab label kept in tmux.
const MAX_NAME_CHARS: usize = 80;

/// What a terminal runs. Also the tmux session name (`-s`), so a session the
/// app does not know still says what it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TerminalKind {
    Claude,
    Bash,
}

impl TerminalKind {
    pub fn as_str(self) -> &'static str {
        match self {
            TerminalKind::Claude => "claude",
            TerminalKind::Bash => "bash",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "claude" => Some(TerminalKind::Claude),
            "bash" => Some(TerminalKind::Bash),
            _ => None,
        }
    }
}

/// A session key is a socket name and an argv element: UUID characters only.
pub fn validate_key(key: &str) -> Result<(), String> {
    if key.is_empty()
        || key.len() > 64
        || !key.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
    {
        return Err("Invalid terminal session id".to_string());
    }
    Ok(())
}

pub fn socket_name(key: &str) -> String {
    format!("{SOCKET_PREFIX}{key}")
}

/// A tab label as tmux keeps it: no control characters (the listing is one
/// tab-separated line per session), at most [`MAX_NAME_CHARS`], and no
/// trailing `;` or `\` — tmux splits its argv into commands at an argument
/// that *ends* in `;`.
pub fn clean_name(name: Option<&str>) -> Option<String> {
    let cleaned: String = name?
        .chars()
        .filter(|c| !c.is_control())
        .take(MAX_NAME_CHARS)
        .collect();
    let cleaned = cleaned.trim().trim_end_matches([';', '\\']).trim().to_string();
    (!cleaned.is_empty()).then_some(cleaned)
}

fn tmux_base(key: &str) -> Vec<String> {
    vec![
        "tmux".into(),
        "-L".into(),
        socket_name(key),
        "-f".into(),
        TMUX_CONF.into(),
    ]
}

/// The attached exec of a tab: `inner` (the unchanged command — the Claude
/// launch with its update prelude, or `bash -l`) as the only window of a new
/// tmux server, or an attach to it if it is already there. The label goes in
/// [`NAME_OPTION`] as a second tmux command. No environment value is ever an
/// argument here.
pub fn new_session_cmd(
    key: &str,
    kind: TerminalKind,
    name: Option<&str>,
    inner: Vec<String>,
) -> Result<Vec<String>, String> {
    validate_key(key)?;
    if inner.is_empty() {
        return Err("No command to run".into());
    }
    // tmux would read an argument ending in `;` as the end of a command and
    // run the rest as tmux commands. Our scripts end in a newline; refuse
    // rather than guess if that ever changes.
    if inner.iter().any(|a| a.ends_with(';')) {
        return Err("A terminal command argument may not end with ';'".into());
    }
    let mut cmd = tmux_base(key);
    cmd.extend([
        "new-session".into(),
        "-A".into(),
        "-s".into(),
        kind.as_str().into(),
        "--".into(),
    ]);
    cmd.extend(inner);
    if let Some(n) = clean_name(name) {
        cmd.extend([";".into(), "set-option".into(), "-q".into(), NAME_OPTION.into(), n]);
    }
    Ok(cmd)
}

/// Attach to an existing session; fails (and creates nothing) when it is gone.
pub fn attach_cmd(key: &str) -> Result<Vec<String>, String> {
    validate_key(key)?;
    let mut cmd = tmux_base(key);
    cmd.push("attach-session".into());
    Ok(cmd)
}

/// End one session: its server and everything in it (SIGHUP to the panes).
pub fn kill_cmd(key: &str) -> Result<Vec<String>, String> {
    validate_key(key)?;
    Ok(vec!["tmux".into(), "-L".into(), socket_name(key), "kill-server".into()])
}

/// The pane's history above the screen, colours kept, wrapped lines joined
/// (xterm.js rewraps them at its own width).
pub fn history_cmd(key: &str) -> Result<Vec<String>, String> {
    validate_key(key)?;
    Ok(vec![
        "tmux".into(),
        "-L".into(),
        socket_name(key),
        "capture-pane".into(),
        "-p".into(),
        "-e".into(),
        "-J".into(),
        "-S".into(),
        format!("-{HISTORY_LINES}"),
        "-E".into(),
        "-1".into(),
    ])
}

/// Exit 0 when the image can keep terminals: tmux and our config.
pub fn probe_cmd() -> Vec<String> {
    vec![
        "sh".into(),
        "-c".into(),
        format!("command -v tmux >/dev/null 2>&1 && [ -r {TMUX_CONF} ]"),
    ]
}

/// The tmux socket directory of the exec's user, as tmux computes it.
const SOCKET_DIR: &str = r#"d="${TMUX_TMPDIR:-/tmp}/tmux-$(id -u)""#;

/// One line per running session: `key \t kind \t created \t attached \t name`.
/// A socket whose server is gone (tmux leaves the file) answers nothing and
/// is skipped.
pub fn list_cmd() -> Vec<String> {
    vec![
        "sh".into(),
        "-c".into(),
        format!(
            r#"{SOCKET_DIR}
command -v tmux >/dev/null 2>&1 || exit 0
for s in "$d"/{SOCKET_PREFIX}*; do
  [ -S "$s" ] || continue
  n=${{s##*/}}
  line=$(tmux -L "$n" display-message -p '#{{session_name}}	#{{session_created}}	#{{session_attached}}	#{{{NAME_OPTION}}}' 2>/dev/null) || continue
  printf '%s\t%s\n' "${{n#{SOCKET_PREFIX}}}" "$line"
done
exit 0"#
        ),
    ]
}

/// End every session (Stop, before the WIP push's snapshot), then wait —
/// bounded — until no tmux server of ours is left, so a pane's last writes
/// land before the snapshot.
pub fn end_all_cmd() -> Vec<String> {
    vec![
        "sh".into(),
        "-c".into(),
        format!(
            r#"{SOCKET_DIR}
command -v tmux >/dev/null 2>&1 || exit 0
for s in "$d"/{SOCKET_PREFIX}*; do
  [ -S "$s" ] && tmux -L "${{s##*/}}" kill-server 2>/dev/null
done
i=0
while pgrep -u "$(id -u)" -f '^tmux -L {SOCKET_PREFIX}' >/dev/null 2>&1 && [ $i -lt 50 ]; do
  sleep 0.1; i=$((i + 1))
done
exit 0"#
        ),
    ]
}

/// A session found running in the workspace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunningSession {
    pub key: String,
    pub kind: TerminalKind,
    pub created_unix: u64,
    pub attached: bool,
    pub name: Option<String>,
}

/// [`list_cmd`]'s output; lines that do not parse (a session someone made by
/// hand under our prefix) are skipped.
pub fn parse_list(out: &str) -> Vec<RunningSession> {
    out.lines()
        .filter_map(|line| {
            let mut f = line.splitn(5, '\t');
            let key = f.next()?.to_string();
            validate_key(&key).ok()?;
            let kind = TerminalKind::parse(f.next()?)?;
            let created_unix = f.next()?.trim().parse().ok()?;
            let attached = f.next()?.trim().parse::<u32>().ok()? > 0;
            let name = clean_name(f.next());
            Some(RunningSession { key, kind, created_unix, attached, name })
        })
        .collect()
}

/// The exec environment of a tmux client: the caller's pairs (copy-at-connect
/// credentials, which the server it starts inherits) plus [`OUTER_TERM`].
pub fn client_env(mut env: Vec<String>) -> Vec<String> {
    env.retain(|e| !e.starts_with("TERM="));
    env.push(OUTER_TERM.to_string());
    env
}

// ───────────────────────────────────────────────────────────── probe cache

/// Per target id (an endpoint URL, new for every tunnel): does the image
/// keep terminals, and was the "no tmux" notice given yet.
#[derive(Debug, Clone, Copy, Default)]
struct Probe {
    supported: bool,
    noticed: bool,
}

fn probes() -> &'static Mutex<HashMap<String, Probe>> {
    static P: std::sync::OnceLock<Mutex<HashMap<String, Probe>>> = std::sync::OnceLock::new();
    P.get_or_init(Default::default)
}

/// Forget what was probed through `target_id` — a new tunnel registered on
/// it (`tunnels::register`), which may reach another task's image.
pub fn forget_probe(target_id: &str) {
    probes().lock().unwrap_or_else(|e| e.into_inner()).remove(target_id);
}

/// Whether `target`'s image has tmux and the config; asked once per tunnel.
/// `None` when the probe could not run (asked again next time).
pub async fn probe(target: &ContainerTarget) -> Option<bool> {
    if let Some(p) = probes().lock().unwrap_or_else(|e| e.into_inner()).get(target.id()) {
        return Some(p.supported);
    }
    let res = tokio::time::timeout(
        SMALL_EXEC_TIMEOUT,
        target.exec_oneshot_background("claude", probe_cmd(), 4096),
    )
    .await;
    let supported = match res {
        Ok(Ok((_, code))) => code == 0,
        Ok(Err(e)) => {
            log::warn!("Could not ask the workspace whether it has tmux: {e}");
            return None;
        }
        Err(_) => {
            log::warn!("Asking the workspace whether it has tmux timed out");
            return None;
        }
    };
    probes()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .entry(target.id().to_string())
        .or_insert(Probe { supported, noticed: false });
    Some(supported)
}

/// [`probe`], an unanswered probe counting as no.
pub async fn supported(target: &ContainerTarget) -> bool {
    probe(target).await.unwrap_or(false)
}

/// The "no tmux" notice, the first time it is asked for on a tunnel whose
/// probe said no (never for a probe that could not run).
fn take_notice(target_id: &str) -> Option<String> {
    let mut p = probes().lock().unwrap_or_else(|e| e.into_inner());
    let entry = p.get_mut(target_id)?;
    if entry.supported || entry.noticed {
        return None;
    }
    entry.noticed = true;
    Some(NO_TMUX_NOTICE.to_string())
}

// ───────────────────────────────────────────────────────────────── launch

/// How a terminal is launched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchPlan {
    pub cmd: Vec<String>,
    pub env: Vec<String>,
    /// Runs in tmux and outlives the app.
    pub persistent: bool,
    /// Say this once (no tmux in the image).
    pub notice: Option<String>,
}

/// The pure decision: a persistent launch for an AWS project whose image
/// keeps terminals, the old launch otherwise.
#[allow(clippy::too_many_arguments)]
pub fn plan_with(
    is_aws: bool,
    supported: bool,
    notice: Option<String>,
    key: &str,
    kind: TerminalKind,
    name: Option<&str>,
    inner: Vec<String>,
    env: Vec<String>,
) -> Result<LaunchPlan, String> {
    if !is_aws {
        return Ok(LaunchPlan { cmd: inner, env, persistent: false, notice: None });
    }
    if !supported {
        return Ok(LaunchPlan { cmd: inner, env, persistent: false, notice });
    }
    Ok(LaunchPlan {
        cmd: new_session_cmd(key, kind, name, inner)?,
        env: client_env(env),
        persistent: true,
        notice: None,
    })
}

/// [`plan_with`] for a real target: probes (cached per tunnel) on AWS only.
pub async fn plan(
    is_aws: bool,
    target: &ContainerTarget,
    key: &str,
    kind: TerminalKind,
    name: Option<&str>,
    inner: Vec<String>,
    env: Vec<String>,
) -> Result<LaunchPlan, String> {
    if !is_aws {
        return plan_with(false, false, None, key, kind, name, inner, env);
    }
    validate_key(key)?;
    let ok = supported(target).await;
    let notice = if ok { None } else { take_notice(target.id()) };
    if !ok && notice.is_none() {
        log::info!("Terminal {key}: not persistent (no tmux in the image, or it could not be asked)");
    }
    plan_with(true, ok, notice, key, kind, name, inner, env)
}

// ─────────────────────────────────────────────────────── workspace actions

async fn small(target: &ContainerTarget, cmd: Vec<String>) -> Result<(String, i64), String> {
    tokio::time::timeout(SMALL_EXEC_TIMEOUT, target.exec_oneshot_background("claude", cmd, 256 * 1024))
        .await
        .map_err(|_| "The workspace did not answer in time".to_string())?
}

/// The sessions running in the workspace (none on an image without tmux).
pub async fn list(target: &ContainerTarget) -> Result<Vec<RunningSession>, String> {
    if !supported(target).await {
        return Ok(Vec::new());
    }
    let (out, code) = small(target, list_cmd()).await?;
    if code != 0 {
        return Err(format!("Listing the workspace's terminals failed (exit {code})"));
    }
    Ok(parse_list(&out))
}

/// End the session `key` (closing its tab). Gone already is success.
pub async fn end(target: &ContainerTarget, key: &str) -> Result<(), String> {
    small(target, kill_cmd(key)?).await.map(|_| ())
}

/// End every session in the workspace, bounded; logged, never an error (a
/// Stop must go on).
pub async fn end_all(target: &ContainerTarget) {
    let res = tokio::time::timeout(
        SMALL_EXEC_TIMEOUT,
        target.exec_oneshot_as("claude", end_all_cmd(), Vec::new()),
    )
    .await;
    match res {
        Ok(Ok(_)) => {}
        Ok(Err(e)) => log::warn!("Could not end the workspace's terminals: {e}"),
        Err(_) => log::warn!("Ending the workspace's terminals timed out"),
    }
}

/// The pane history of `key`, for a freshly opened tab; `None` when it can't
/// be read (the attach still goes ahead).
pub async fn history(target: &ContainerTarget, key: &str) -> Option<String> {
    let cmd = history_cmd(key).ok()?;
    let res = tokio::time::timeout(
        SMALL_EXEC_TIMEOUT,
        target.exec_oneshot_background("claude", cmd, HISTORY_LIMIT),
    )
    .await;
    match res {
        Ok(Ok((out, 0))) => Some(out),
        _ => None,
    }
}

/// What a fresh xterm is fed before the attach: the history, one line per
/// row, then `rows` newlines so all of it scrolls into xterm.js's scrollback
/// before tmux clears and draws the screen (an erase does not push lines
/// into xterm.js's scrollback).
pub fn replay_bytes(history: &str, rows: u16) -> Vec<u8> {
    let body = history.trim_end_matches('\n');
    if body.is_empty() {
        return Vec::new();
    }
    let mut out = String::with_capacity(body.len() + rows as usize * 2 + 16);
    for line in body.split('\n') {
        out.push_str(line);
        // A captured line ends with its own SGR state; reset so colour
        // never bleeds into the padding.
        out.push_str("\x1b[0m\r\n");
    }
    for _ in 0..rows {
        out.push_str("\r\n");
    }
    out.into_bytes()
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod live_tests;
