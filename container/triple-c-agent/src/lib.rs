//! `triple-c-agent`: everything `host::ContainerTarget` does through the Docker
//! API, done from inside the container over authenticated HTTP/WebSocket.
//!
//! The contract is `docs/superpowers/specs/2026-11-13-triple-c-agent-protocol.md`.
//! **Docker parity is the requirement**: the same `ExecSpec` must behave the
//! same here as through `docker exec`, because the app's call sites were
//! written against Docker and are not going to grow a second set of rules.
//! Each place this code reproduces a Docker behaviour says so where it does it.
//!
//! The library exists so the integration tests can run the real server
//! in-process on an ephemeral port; `main.rs` only reads the environment.

mod attach;
mod exec;
mod files;
mod pty;
mod spawn;
mod tunnel;
mod users;

use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::extract::{Request, State};
use axum::http::{header, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Json, Router};

/// The wire protocol version reported by `/v1/health`.
pub const PROTOCOL_VERSION: u32 = 1;

/// Upload ceiling for `PUT /v1/files`: the app's `MAX_DROP_BYTES`
/// (`docker/exec.rs`). The app checks it before sending, so this only has to
/// stop a body that lies about itself.
pub const MAX_DROP_BYTES: u64 = 256 * 1024 * 1024;

/// The default one-shot budget when a request omits `limit` — the app's
/// `MAX_ONESHOT_OUTPUT`. Every real client call sends its own.
pub const MAX_ONESHOT_OUTPUT: usize = 8 * 1024 * 1024;

/// The shortest token the agent will accept. There is no unauthenticated mode,
/// and a short token is close enough to none that it is refused the same way.
pub const MIN_TOKEN_BYTES: usize = 32;

/// Name of the env var carrying the token. `main` removes it from the
/// process's environment after reading it, so no exec inherits it — but that
/// only edits libc's copy: the kernel's record of the environment the process
/// was started with (`/proc/<pid>/environ`, readable by root and by the
/// agent's own uid) still holds it. Prefer [`TOKEN_FILE_ENV`], which is what
/// the entrypoint uses.
pub const TOKEN_ENV: &str = "TRIPLEC_AGENT_TOKEN";
pub const TOKEN_FILE_ENV: &str = "TRIPLEC_AGENT_TOKEN_FILE";
pub const ADDR_ENV: &str = "TRIPLEC_AGENT_ADDR";
pub const DEFAULT_ADDR: &str = "127.0.0.1:47900";

/// How long a client gets to send a complete request head. Until its token
/// has been checked a connection is anonymous, so this is what stops a
/// slowloris from holding sockets open forever.
pub const HEADER_READ_TIMEOUT: Duration = Duration::from_secs(10);

/// Concurrent HTTP connections. Far above what the app opens; a bound on
/// what an unauthenticated caller can make the agent hold.
pub const MAX_CONNECTIONS: usize = 256;

/// The largest WebSocket message (and frame) the agent accepts on attach and
/// tunnel sockets. The app sends stdin in frames of at most 64 KiB.
pub(crate) const MAX_WS_MESSAGE: usize = 1024 * 1024;

/// Everything the server needs, captured once at startup.
#[derive(Clone)]
pub struct Config {
    /// The bearer token every request must carry.
    pub token: String,
    /// Where an exec with `working_dir: null` runs: the agent's own cwd at
    /// startup, which is the container's `WorkingDir` — what `docker exec`
    /// without `-w` uses.
    pub default_cwd: PathBuf,
    /// The env every exec starts from: the agent's own at startup, minus the
    /// token. That is the container's configured env, which is what a
    /// `docker exec` inherits.
    pub base_env: Vec<(OsString, OsString)>,
    /// `PUT /v1/files` ceiling. [`MAX_DROP_BYTES`] in production; a field only
    /// so tests can exercise the refusal without a 256 MiB body.
    pub max_file_bytes: u64,
    /// [`HEADER_READ_TIMEOUT`] in production; a field so tests need not wait.
    pub header_read_timeout: Duration,
    /// [`MAX_CONNECTIONS`] in production.
    pub max_connections: usize,
}

impl Config {
    /// Capture the process's own cwd and env. Call after the token has been
    /// removed from the environment; the token is filtered here as well, so a
    /// caller that forgets cannot leak it into execs.
    pub fn from_process(token: String) -> std::io::Result<Config> {
        Ok(Config {
            token,
            default_cwd: std::env::current_dir()?,
            // None of the agent's own settings (`TRIPLEC_AGENT_*`) reach an
            // exec: the token is a secret, and the token-file path and listen
            // address are the agent's business — `docker exec` sees neither.
            base_env: std::env::vars_os()
                .filter(|(k, _)| !k.to_string_lossy().starts_with("TRIPLEC_AGENT_"))
                .collect(),
            max_file_bytes: MAX_DROP_BYTES,
            header_read_timeout: HEADER_READ_TIMEOUT,
            max_connections: MAX_CONNECTIONS,
        })
    }
}

/// Read the token as the spec says: the env var first, then the file. Refuses
/// anything empty or shorter than [`MIN_TOKEN_BYTES`] — the agent has no
/// unauthenticated mode to fall back to.
///
/// Surrounding whitespace is trimmed: a token file written by `echo` or a
/// secrets manager usually ends in a newline, and a trailing `\n` the client
/// never sends would be a token nobody can present.
pub fn load_token(env_token: Option<OsString>, file: Option<PathBuf>) -> Result<String, String> {
    let (raw, source) = match env_token.filter(|t| !t.is_empty()) {
        Some(t) => (
            t.into_string()
                .map_err(|_| format!("{TOKEN_ENV} is not valid UTF-8"))?,
            TOKEN_ENV.to_string(),
        ),
        None => match file {
            Some(path) => (
                read_private_file(&path)
                    .map_err(|e| format!("cannot use {TOKEN_FILE_ENV} ({}): {e}", path.display()))?,
                format!("{TOKEN_FILE_ENV} ({})", path.display()),
            ),
            None => {
                return Err(format!(
                    "no token: set {TOKEN_ENV} or {TOKEN_FILE_ENV}; refusing to start unauthenticated"
                ))
            }
        },
    };
    let token = raw.trim().to_string();
    if token.len() < MIN_TOKEN_BYTES {
        return Err(format!(
            "token from {source} is {} bytes; at least {MIN_TOKEN_BYTES} are required",
            token.len()
        ));
    }
    Ok(token)
}

/// Read a secret file, refusing one that anyone but its owner could read or
/// that this process does not own — as `ssh` does for key files. The token
/// is effectively root in the container; a world-readable copy of it would be
/// a way in for any process, not just root's. Checked on the opened file, so
/// the file judged is the file read.
fn read_private_file(path: &std::path::Path) -> Result<String, String> {
    use std::io::Read;
    use std::os::unix::fs::MetadataExt;
    let mut f = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let meta = f.metadata().map_err(|e| e.to_string())?;
    let euid = unsafe { libc::geteuid() };
    if meta.uid() != euid {
        return Err(format!("owned by uid {}, not this process's uid {euid}", meta.uid()));
    }
    if meta.mode() & 0o077 != 0 {
        return Err(format!(
            "mode {:o} lets others read it; it must be 0600 or stricter",
            meta.mode() & 0o777
        ));
    }
    let mut s = String::new();
    f.read_to_string(&mut s).map_err(|e| e.to_string())?;
    Ok(s)
}

/// Shared server state.
pub(crate) struct AppState {
    pub config: Config,
    pub execs: exec::Registry,
    pub activity: Activity,
}

/// What `/v1/activity` reports; the input to idle auto-stop.
pub(crate) struct Activity {
    last_unix: AtomicI64,
    pub execs: AtomicI64,
    pub tunnels: AtomicI64,
}

impl Activity {
    pub fn touch(&self) {
        self.last_unix.store(now_unix(), Ordering::Relaxed);
    }
}

/// Increments a counter for as long as it lives, so an exec or tunnel that
/// ends by any path — including a panic or a dropped task — stops counting.
pub(crate) struct CountGuard(Arc<AppState>, fn(&Activity) -> &AtomicI64);

impl CountGuard {
    pub fn execs(st: &Arc<AppState>) -> CountGuard {
        st.activity.execs.fetch_add(1, Ordering::Relaxed);
        CountGuard(st.clone(), |a| &a.execs)
    }
    pub fn tunnels(st: &Arc<AppState>) -> CountGuard {
        st.activity.tunnels.fetch_add(1, Ordering::Relaxed);
        CountGuard(st.clone(), |a| &a.tunnels)
    }
}

impl Drop for CountGuard {
    fn drop(&mut self) {
        (self.1)(&self.0.activity).fetch_sub(1, Ordering::Relaxed);
    }
}

pub(crate) fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// The spec's error shape: `{"error": "<message>"}` with a JSON content type.
pub(crate) fn error(status: StatusCode, msg: impl Into<String>) -> Response {
    (status, Json(serde_json::json!({ "error": msg.into() }))).into_response()
}

/// Every route, behind the token gate.
fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/v1/health", get(health))
        .route("/v1/activity", get(activity))
        .route("/v1/exec", post(exec::oneshot))
        .route("/v1/exec/detached", post(exec::detached))
        .route("/v1/exec/{id}", get(exec::status))
        .route("/v1/exec/{id}/resize", post(exec::resize))
        .route("/v1/attach", get(attach::upgrade))
        .route(
            "/v1/files",
            // The handler streams the body and enforces its own ceiling, so
            // axum's 2 MB default must not cut it off first.
            put(files::put_file).layer(axum::extract::DefaultBodyLimit::disable()),
        )
        .route("/v1/archive", get(files::archive))
        .route("/v1/tunnel", get(tunnel::upgrade))
        .fallback(|| async { error(StatusCode::NOT_FOUND, "no such endpoint") })
        // `layer`, not `route_layer`: the fallback must be behind the token
        // too, or an unauthenticated caller could map the API by 404 vs 401.
        .layer(middleware::from_fn_with_state(state.clone(), auth))
        .with_state(state)
}

/// Serve the protocol on `listener`. Runs until the task is dropped: accept
/// errors (a full fd table, a connection reset before accept) are transient
/// and retried, as `axum::serve` does.
///
/// Our own accept loop rather than `axum::serve`, because that gives hyper no
/// timer, so hyper's header-read timeout never fires and an anonymous
/// connection that trickles (or never sends) its request head is held
/// forever. Here every connection gets [`Config::header_read_timeout`] to
/// deliver a complete head, and at most [`Config::max_connections`] are
/// served at once. HTTP/1.1 only — all the protocol needs — with upgrades.
///
/// At the cap the loop **waits** for a slot before accepting, rather than
/// accepting and dropping: excess clients queue in the kernel's backlog and
/// are served as soon as a slot frees, which the header timeout guarantees
/// for any slot an idle or slow client holds. Dropping would instead fail the
/// app's own requests outright whenever the cap is reached.
///
/// A connection's slot is released when hyper is done with it. A WebSocket
/// upgrade ends hyper's part (the socket then belongs to the session task),
/// so attach and tunnel sessions do not count against the cap — and they only
/// exist past the token check.
pub async fn serve(listener: tokio::net::TcpListener, config: Config) -> std::io::Result<()> {
    use hyper_util::rt::{TokioIo, TokioTimer};
    use hyper_util::service::TowerToHyperService;

    let header_timeout = config.header_read_timeout;
    let slots = Arc::new(tokio::sync::Semaphore::new(config.max_connections.max(1)));
    let state = Arc::new(AppState {
        config,
        execs: exec::Registry::default(),
        activity: Activity {
            last_unix: AtomicI64::new(now_unix()),
            execs: AtomicI64::new(0),
            tunnels: AtomicI64::new(0),
        },
    });
    let app = router(state);
    loop {
        let permit = slots.clone().acquire_owned().await.expect("the semaphore is never closed");
        let stream = match listener.accept().await {
            Ok((stream, _)) => stream,
            Err(e) => {
                // ECONNABORTED and friends are per-connection; EMFILE/ENFILE
                // clear as connections close. Either way, back off and retry.
                if !matches!(
                    e.kind(),
                    std::io::ErrorKind::ConnectionAborted | std::io::ErrorKind::ConnectionReset
                ) {
                    eprintln!("triple-c-agent: accept failed: {e}");
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
                continue;
            }
        };
        let service = TowerToHyperService::new(app.clone());
        tokio::spawn(async move {
            let _slot = permit;
            let conn = hyper::server::conn::http1::Builder::new()
                .timer(TokioTimer::new())
                .header_read_timeout(header_timeout)
                .serve_connection(TokioIo::new(stream), service)
                .with_upgrades();
            // An error here is the client's (reset, malformed request, header
            // timeout); there is nobody to report it to.
            let _ = conn.await;
        });
    }
}

/// Bearer-token gate on every request. A failure is `401` with an **empty**
/// body (spec): nothing about the API is disclosed to a caller without the
/// token.
async fn auth(State(st): State<Arc<AppState>>, req: Request, next: Next) -> Response {
    let presented = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.as_bytes().strip_prefix(b"Bearer "));
    let ok = presented.is_some_and(|p| constant_time_eq(p, st.config.token.as_bytes()));
    if !ok {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    // `health` and `activity` are the idle monitor's own probes; counting them
    // would mean an idle container could never look idle.
    let path = req.uri().path();
    if path != "/v1/health" && path != "/v1/activity" {
        st.activity.touch();
    }
    next.run(req).await
}

/// Compare without an early exit, so response timing says nothing about how
/// many leading bytes of a guess were right. The length is not secret (the
/// minimum is public and tokens are generated at a fixed size), but it is
/// folded in rather than short-circuited anyway.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    let mut diff: usize = a.len() ^ b.len();
    for i in 0..a.len().max(b.len()) {
        let x = a.get(i).copied().unwrap_or(0);
        let y = b.get(i).copied().unwrap_or(0);
        diff |= (x ^ y) as usize;
    }
    diff == 0
}

async fn health() -> Response {
    Json(serde_json::json!({
        "agent": "triple-c-agent",
        "version": env!("CARGO_PKG_VERSION"),
        "protocol": PROTOCOL_VERSION,
    }))
    .into_response()
}

async fn activity(State(st): State<Arc<AppState>>) -> Response {
    Json(serde_json::json!({
        "last_activity_unix": st.activity.last_unix.load(Ordering::Relaxed),
        "active_execs": st.activity.execs.load(Ordering::Relaxed),
        "active_tunnels": st.activity.tunnels.load(Ordering::Relaxed),
    }))
    .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constant_time_eq_matches_plain_equality() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"abcd"));
        assert!(!constant_time_eq(b"", b"a"));
        // A length difference of exactly 256 must not wrap to "equal length".
        assert!(!constant_time_eq(&[0u8; 256], b""));
    }

    #[test]
    fn a_token_file_others_can_read_is_refused() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("token");
        std::fs::write(&path, "y".repeat(40)).unwrap();

        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        let e = load_token(None, Some(path.clone())).unwrap_err();
        assert!(e.contains("0600"), "{e}");

        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(load_token(None, Some(path.clone())).unwrap(), "y".repeat(40));
    }

    #[test]
    fn the_token_never_reaches_the_exec_base_env() {
        // `main` removes the var before capturing; this is the second guard,
        // for a caller that forgets.
        std::env::set_var(TOKEN_ENV, "x".repeat(40));
        std::env::set_var(TOKEN_FILE_ENV, "/run/triple-c/agent-token");
        let cfg = Config::from_process("t".into()).unwrap();
        std::env::remove_var(TOKEN_ENV);
        std::env::remove_var(TOKEN_FILE_ENV);
        assert!(cfg.base_env.iter().all(|(k, _)| !k.to_string_lossy().starts_with("TRIPLEC_AGENT_")));
        assert!(cfg.base_env.iter().any(|(k, _)| k == "PATH"));
    }

    #[test]
    fn token_rules() {
        let long = "x".repeat(32);
        assert_eq!(load_token(Some(long.clone().into()), None).unwrap(), long);
        assert!(load_token(Some("short".into()), None).is_err());
        assert!(load_token(None, None).is_err());
        // An empty env var falls through to the file.
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("tok");
        std::fs::write(&f, format!("{long}\n")).unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        assert_eq!(load_token(Some("".into()), Some(f.clone())).unwrap(), long);
        std::fs::write(&f, "\n").unwrap();
        assert!(load_token(None, Some(f)).is_err());
        assert!(load_token(None, Some(dir.path().join("missing"))).is_err());
    }
}
