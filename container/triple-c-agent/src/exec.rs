//! One-shot and detached execs, exec status, and resize.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use bytes::Bytes;
use base64::Engine;
use serde::Deserialize;
use tokio::io::AsyncReadExt;

use crate::pty::PtyMaster;
use crate::spawn::{self, ExecSpec, Io, PrepareError};
use crate::{error, AppState, CountGuard, MAX_ONESHOT_OUTPUT};

/// How long a finished exec stays answerable. The spec promises "at least 10
/// minutes": the app polls `GET /v1/exec/{id}` after an attach ends, and a
/// slow poll (a laptop waking from sleep) must not turn a real exit code into
/// an unknown id.
const KEEP_FINISHED: Duration = Duration::from_secs(10 * 60);

pub struct ExecEntry {
    pub running: bool,
    pub exit_code: Option<i32>,
    finished_at: Option<Instant>,
    /// Present only for tty execs that are still running — what `resize`
    /// needs. Dropped on exit so the terminal is released.
    pub pty: Option<Arc<PtyMaster>>,
}

/// Exec ids → status, for `detached` and `attach` execs.
#[derive(Default)]
pub struct Registry {
    map: Mutex<HashMap<String, ExecEntry>>,
}

impl Registry {
    /// Register a running exec and return its id. Expired entries are pruned
    /// on the way in, which bounds the map by the insert rate × 10 minutes.
    pub fn insert_running(&self, pty: Option<Arc<PtyMaster>>) -> String {
        self.insert(ExecEntry {
            running: true,
            exit_code: None,
            finished_at: None,
            pty,
        })
    }

    /// Register an exec that never started (126/127), so its id answers like
    /// any finished exec — which is how Docker presents it.
    pub fn insert_finished(&self, code: i32) -> String {
        self.insert(ExecEntry {
            running: false,
            exit_code: Some(code),
            finished_at: Some(Instant::now()),
            pty: None,
        })
    }

    fn insert(&self, entry: ExecEntry) -> String {
        let id = new_id();
        let mut map = self.map.lock().unwrap();
        map.retain(|_, e| e.finished_at.is_none_or(|t| t.elapsed() < KEEP_FINISHED));
        map.insert(id.clone(), entry);
        id
    }

    pub fn finish(&self, id: &str, code: i32) {
        if let Some(e) = self.map.lock().unwrap().get_mut(id) {
            e.running = false;
            e.exit_code = Some(code);
            e.finished_at = Some(Instant::now());
            e.pty = None;
        }
    }

    fn with<R>(&self, id: &str, f: impl FnOnce(&ExecEntry) -> R) -> Option<R> {
        self.map.lock().unwrap().get(id).map(f)
    }
}

/// 64 hex characters from the kernel's CSPRNG — Docker's exec id shape. Ids
/// are not secrets (the token is the boundary), but guessable ones would let
/// one client poke at another's execs for no reason.
fn new_id() -> String {
    let mut bytes = [0u8; 32];
    let mut filled = 0;
    while filled < bytes.len() {
        let n = unsafe {
            libc::getrandom(bytes[filled..].as_mut_ptr().cast(), bytes.len() - filled, 0)
        };
        if n > 0 {
            filled += n as usize;
        } else if std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted {
            // getrandom(2) cannot fail on a supported kernel once the pool is
            // initialised; panicking beats handing out predictable ids.
            panic!("getrandom failed: {}", std::io::Error::last_os_error());
        }
    }
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Map a spec refusal onto an HTTP error response.
pub fn refusal(status: StatusCode, msg: String) -> Response {
    error(status, msg)
}

/// Parse a JSON body without insisting on `Content-Type: application/json`.
/// The spec says bodies are JSON and says nothing about the header, so a
/// client that omits it (curl `-d`, a hand-rolled hyper request) must not get
/// a 415 for a body that is perfectly good JSON.
fn parse_json<T: serde::de::DeserializeOwned>(body: &[u8]) -> Result<T, String> {
    serde_json::from_slice(body).map_err(|e| format!("invalid request body: {e}"))
}

#[derive(Deserialize)]
pub struct OneshotRequest {
    #[serde(flatten)]
    spec: ExecSpec,
    #[serde(default)]
    limit: Option<usize>,
}

/// `POST /v1/exec`.
pub async fn oneshot(
    State(st): State<Arc<AppState>>,
    body: Bytes,
) -> Response {
    let req: OneshotRequest = match parse_json(&body) {
        Ok(r) => r,
        Err(e) => return error(StatusCode::BAD_REQUEST, e),
    };
    let limit = req.limit.unwrap_or(MAX_ONESHOT_OUTPUT);
    // A one-shot never has a tty (spec), whatever the spec's `tty` says.
    let prepared = spawn::prepare(&st.config, &req.spec, false);
    let child = prepared.and_then(|p| p.spawn(Io::Null, Io::Piped, Io::Piped, false));
    let mut child = match child {
        Ok(c) => c,
        Err(PrepareError::Refused(s, m)) => return refusal(s, m),
        Err(PrepareError::CannotStart { code, message }) => {
            // Docker's shape for an exec that could not start: a finished exec
            // with 126/127 and the reason on stderr — not an API error.
            return oneshot_response(&[(false, format!("{message}\n").into_bytes())], code);
        }
    };
    let _active = CountGuard::execs(&st);
    let pid = child.id();
    let mut stdout = child.stdout.take().expect("piped stdout");
    let mut stderr = child.stderr.take().expect("piped stderr");

    // (is_stdout, bytes) in arrival order; adjacent reads from the same
    // stream are merged, which changes nothing a client can rebuild.
    let mut chunks: Vec<(bool, Vec<u8>)> = Vec::new();
    let mut total = 0usize;
    let (mut out_open, mut err_open) = (true, true);
    let mut ob = vec![0u8; 16 * 1024];
    let mut eb = vec![0u8; 16 * 1024];
    while out_open || err_open {
        let (is_out, n) = tokio::select! {
            r = stdout.read(&mut ob), if out_open => (true, r.unwrap_or(0)),
            r = stderr.read(&mut eb), if err_open => (false, r.unwrap_or(0)),
        };
        if n == 0 {
            if is_out { out_open = false } else { err_open = false }
            continue;
        }
        // The same test as `docker/exec.rs`'s `push_capped`: refuse the chunk
        // that would take the total past `limit`, so exactly `limit` bytes is
        // still a success on both transports.
        if total + n > limit {
            // Kill the whole process group, not just the pid: a pipeline or a
            // backgrounded writer would otherwise keep running with nobody
            // reading. Reaped in the background so the response is not held
            // hostage by a process stuck in uninterruptible sleep.
            spawn::signal_group(pid, libc::SIGKILL);
            tokio::spawn(async move {
                let _ = child.wait().await;
            });
            // Byte-for-byte the Docker path's message, so the client can pass
            // it through and `OUTPUT_LIMIT_MARKER` checks keep matching.
            return error(
                StatusCode::PAYLOAD_TOO_LARGE,
                format!("OUTPUT_LIMIT: Command output exceeded {limit} bytes and was abandoned"),
            );
        }
        total += n;
        let data = if is_out { &ob[..n] } else { &eb[..n] };
        match chunks.last_mut() {
            Some((s, buf)) if *s == is_out => buf.extend_from_slice(data),
            _ => chunks.push((is_out, data.to_vec())),
        }
    }
    let code = match child.wait().await {
        Ok(status) => spawn::exit_code(status),
        Err(e) => return error(StatusCode::INTERNAL_SERVER_ERROR, format!("wait failed: {e}")),
    };
    oneshot_response(&chunks, code)
}

fn oneshot_response(chunks: &[(bool, Vec<u8>)], code: i32) -> Response {
    let b64 = base64::engine::general_purpose::STANDARD;
    let chunks: Vec<_> = chunks
        .iter()
        .map(|(is_out, d)| serde_json::json!({ "s": if *is_out { "out" } else { "err" }, "d": b64.encode(d) }))
        .collect();
    Json(serde_json::json!({ "chunks": chunks, "exit_code": code })).into_response()
}

/// `POST /v1/exec/detached`: stdin, stdout and stderr all `/dev/null`.
pub async fn detached(
    State(st): State<Arc<AppState>>,
    body: Bytes,
) -> Response {
    let spec: ExecSpec = match parse_json(&body) {
        Ok(r) => r,
        Err(e) => return error(StatusCode::BAD_REQUEST, e),
    };
    let child = spawn::prepare(&st.config, &spec, false)
        .and_then(|p| p.spawn(Io::Null, Io::Null, Io::Null, false));
    let id = match child {
        Ok(mut child) => {
            let id = st.execs.insert_running(None);
            let guard = CountGuard::execs(&st);
            let (st2, id2) = (st.clone(), id.clone());
            tokio::spawn(async move {
                let _active = guard;
                let code = child.wait().await.map(spawn::exit_code).unwrap_or(255);
                st2.execs.finish(&id2, code);
            });
            id
        }
        Err(PrepareError::Refused(s, m)) => return refusal(s, m),
        // Docker accepts a detached exec of a missing binary and reports the
        // failure as its exit code; the output has nowhere to go.
        Err(PrepareError::CannotStart { code, .. }) => st.execs.insert_finished(code),
    };
    Json(serde_json::json!({ "exec_id": id })).into_response()
}

/// `GET /v1/exec/{id}`.
pub async fn status(State(st): State<Arc<AppState>>, Path(id): Path<String>) -> Response {
    match st.execs.with(&id, |e| (e.running, e.exit_code)) {
        Some((running, code)) => {
            Json(serde_json::json!({ "running": running, "exit_code": code })).into_response()
        }
        None => error(StatusCode::NOT_FOUND, format!("no such exec: {id}")),
    }
}

#[derive(Deserialize)]
pub struct ResizeRequest {
    cols: u16,
    rows: u16,
}

/// `POST /v1/exec/{id}/resize`: `204`, or `409` for anything that is not a
/// running tty exec.
pub async fn resize(
    State(st): State<Arc<AppState>>,
    Path(id): Path<String>,
    body: Bytes,
) -> Response {
    let req: ResizeRequest = match parse_json(&body) {
        Ok(r) => r,
        Err(e) => return error(StatusCode::BAD_REQUEST, e),
    };
    let pty = match st.execs.with(&id, |e| e.pty.clone()) {
        None => return error(StatusCode::NOT_FOUND, format!("no such exec: {id}")),
        Some(None) => return error(StatusCode::CONFLICT, "exec is not a running tty exec"),
        Some(Some(p)) => p,
    };
    match pty.resize(req.cols, req.rows) {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => error(StatusCode::INTERNAL_SERVER_ERROR, format!("resize failed: {e}")),
    }
}
