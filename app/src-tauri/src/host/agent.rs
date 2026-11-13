//! The app's side of `triple-c-agent`, the small server that runs inside a
//! project container and does over HTTP what the Docker variant does through
//! the Docker API.
//!
//! Wire protocol: `docs/superpowers/specs/2026-11-13-triple-c-agent-protocol.md`.
//! Its "Client mapping" table is what this file implements, one function per
//! [`ContainerTarget`](super::ContainerTarget) method. The contract is Docker
//! parity: every function here returns exactly what its `docker/exec.rs`
//! counterpart returns for the same command, including the shape of one-shot
//! output (combined in arrival order, or stdout/stderr split), the
//! `OUTPUT_LIMIT` refusal text, and the path an upload reports.
//!
//! The token never leaves this file except as the `Authorization` header:
//! [`AgentEndpoint`]'s `Debug` redacts it and no error string is built from a
//! request (reqwest and tungstenite errors name the URL, which carries no
//! secret, never the headers).

use std::collections::{HashMap, VecDeque};
use std::io;
use std::pin::Pin;
use std::sync::{Mutex, OnceLock};
use std::task::{Context, Poll};
use std::time::Duration;

use base64::Engine;
use bytes::Bytes;
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use tokio::io::AsyncWrite;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use crate::host::OUTPUT_LIMIT_MARKER;
use tokio_tungstenite::tungstenite::Message;
use tokio_util::sync::PollSender;

use super::{ArchiveStream, AttachedExec, ExecChunk, ExecSpec};
use crate::docker::exec::{
    container_join, output_limit_error, parse_user_ids, read_host_file_for_upload, user_ids_cmd,
    OneshotOutput, EXEC_EXIT_POLLS, EXEC_EXIT_POLL_INTERVAL, HOST_FILE_UPLOAD_MODE,
    MAX_ONESHOT_OUTPUT, USER_IDS_OUTPUT_LIMIT,
};

/// Connecting (TCP, and the WebSocket handshake) must not hang a caller on an
/// agent that is not there. Nothing else has an overall timeout: a one-shot
/// exec, an attach or an archive download can legitimately run for minutes.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// `is_running` is a liveness probe; it answers quickly or says "no".
const HEALTH_TIMEOUT: Duration = Duration::from_secs(3);
/// Whole-request ceiling for the small calls (detached start, exec status,
/// resize) and for reading any small body. The agent is not trusted: one that
/// accepts a connection and then says nothing must not hang the app. One-shot
/// execs, attach and archive have *no* overall timeout — commands can
/// legitimately run long and send nothing until they finish, as with Docker.
#[cfg(not(test))]
const SMALL_REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
/// Short in tests, so the hostile-agent tests finish quickly.
#[cfg(test)]
const SMALL_REQUEST_TIMEOUT: Duration = Duration::from_secs(1);
/// Largest WebSocket message either side may send; the agent enforces the same
/// (its output frames are at most 32 KiB). Stdin is split to fit.
const WS_MAX_MESSAGE: usize = 1024 * 1024;
const STDIN_FRAME_MAX: usize = 64 * 1024;
/// How long an attach waits for the agent's `started` reply after the
/// handshake. Spawning a process is fast; this only bounds a wedged agent.
const ATTACH_START_TIMEOUT: Duration = Duration::from_secs(30);
/// In-flight stdin frames per attach before `poll_write` applies backpressure.
const STDIN_QUEUE: usize = 64;

/// Where an agent listens and the bearer token it wants.
///
/// `base_url` is `http://127.0.0.1:<port>/`: the local end of an SSM
/// port-forward on AWS, or a published loopback port in the local harness.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct AgentEndpoint {
    pub base_url: url::Url,
    token: String,
}

impl std::fmt::Debug for AgentEndpoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentEndpoint")
            .field("base_url", &self.base_url.as_str())
            .field("token", &"<redacted>")
            .finish()
    }
}

impl AgentEndpoint {
    /// An endpoint at `base_url`, which must be a plain `http://` URL with a
    /// host. A trailing `/` is added so endpoint paths join beneath it.
    pub fn new(mut base_url: url::Url, token: impl Into<String>) -> Result<Self, String> {
        if base_url.scheme() != "http" || base_url.host().is_none() {
            return Err(format!(
                "An agent endpoint must be an http:// URL with a host, not {}",
                base_url
            ));
        }
        if !base_url.path().ends_with('/') {
            let path = format!("{}/", base_url.path());
            base_url.set_path(&path);
        }
        base_url.set_query(None);
        base_url.set_fragment(None);
        let token = token.into();
        // It goes in an HTTP header; refuse anything that cannot, here, once,
        // rather than failing (or silently degrading) on every request.
        if token.is_empty() || !token.bytes().all(|b| b.is_ascii_graphic()) {
            return Err("An agent token must be non-empty printable ASCII with no spaces".to_string());
        }
        Ok(Self {
            base_url,
            token,
        })
    }

    /// The agent on `127.0.0.1:<port>`.
    pub fn loopback(port: u16, token: impl Into<String>) -> Self {
        let url = url::Url::parse(&format!("http://127.0.0.1:{}/", port))
            .expect("a loopback URL always parses");
        Self::new(url, token).expect("a loopback URL is a valid endpoint")
    }

    fn url(&self, path: &str) -> url::Url {
        self.base_url
            .join(path)
            .expect("endpoint paths are fixed relative paths")
    }

    fn ws_url(&self, path: &str) -> url::Url {
        let mut u = self.url(path);
        u.set_scheme("ws").expect("http -> ws is a valid scheme change");
        u
    }

    fn bearer(&self) -> String {
        format!("Bearer {}", self.token)
    }

    /// The `Authorization` header, marked sensitive so it is never logged or
    /// shown by `Debug` (reqwest/hyper honour the flag).
    fn auth(&self) -> HeaderValue {
        let mut v = HeaderValue::from_str(&self.bearer())
            .unwrap_or_else(|_| HeaderValue::from_static("Bearer invalid-token"));
        v.set_sensitive(true);
        v
    }
}

// ── HTTP plumbing ────────────────────────────────────────────────────────────

/// One client for every agent: connection pooling is per host, and building a
/// client per call would redo TLS-less setup for nothing. No proxy — the agent
/// is always on loopback, and a corporate `HTTP_PROXY` must not capture it.
fn http() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .no_proxy()
            // The agent lives inside the container, which is not trusted with
            // the host: a redirect would let it point this client — running in
            // the host app — at any other URL. A 3xx is an error (it fails the
            // `is_success` checks like any other non-2xx).
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("a reqwest client with no TLS config always builds")
    })
}

#[derive(Deserialize)]
struct ErrorBody {
    error: String,
}

/// Ceiling on any small JSON body from the agent (errors, exec ids, exec
/// status). The agent is inside the container, so a body it sends must not be
/// able to grow host memory without bound; real ones are tens of bytes.
const SMALL_BODY_CAP: usize = 64 * 1024;

/// Ceiling on error text passed through to the UI.
const ERROR_TEXT_CAP: usize = 4 * 1024;

/// Parse a small JSON body, reading at most [`SMALL_BODY_CAP`] bytes.
async fn small_json<T: serde::de::DeserializeOwned>(resp: reqwest::Response) -> Option<T> {
    let raw = tokio::time::timeout(SMALL_REQUEST_TIMEOUT, read_capped(resp, SMALL_BODY_CAP))
        .await
        .ok()?
        .ok()??;
    serde_json::from_slice(&raw).ok()
}

/// The agent's `{"error": …}` text, if the body has one — bounded in both
/// bytes read and length returned.
async fn error_text(resp: reqwest::Response) -> Option<String> {
    let mut e = small_json::<ErrorBody>(resp).await?.error;
    if e.len() > ERROR_TEXT_CAP {
        let mut cut = ERROR_TEXT_CAP;
        while !e.is_char_boundary(cut) {
            cut -= 1;
        }
        e.truncate(cut);
        e.push('…');
    }
    Some(e)
}

/// `"<context>: <why>"` for a non-success response.
async fn status_error(context: &str, resp: reqwest::Response) -> String {
    let status = resp.status();
    if status == reqwest::StatusCode::UNAUTHORIZED {
        return format!("{}: the agent rejected the credentials (401)", context);
    }
    match error_text(resp).await {
        Some(e) => format!("{}: {}", context, e),
        None => format!("{}: agent returned HTTP {}", context, status.as_u16()),
    }
}

async fn send(
    context: &str,
    req: reqwest::RequestBuilder,
) -> Result<reqwest::Response, String> {
    req.send().await.map_err(|e| format!("{}: {}", context, e))
}

/// Read a response body into memory, refusing past `cap` bytes.
async fn read_capped(mut resp: reqwest::Response, cap: usize) -> Result<Option<Vec<u8>>, String> {
    let mut body = Vec::new();
    while let Some(chunk) = resp
        .chunk()
        .await
        .map_err(|e| format!("Exec output error: {}", e))?
    {
        if body.len() + chunk.len() > cap {
            return Ok(None);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(Some(body))
}

/// ExecSpec as the protocol spells it.
fn spec_json(spec: &ExecSpec) -> serde_json::Map<String, serde_json::Value> {
    let v = serde_json::json!({
        "cmd": spec.cmd,
        "user": spec.user,
        "working_dir": spec.working_dir,
        "env": spec.env,
        "tty": spec.tty,
    });
    match v {
        serde_json::Value::Object(m) => m,
        _ => unreachable!("json! of an object literal is an object"),
    }
}

// ── one-shot execs ───────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct OneshotChunk {
    s: String,
    d: String,
}

#[derive(Deserialize)]
struct OneshotResponse {
    chunks: Vec<OneshotChunk>,
    exit_code: i64,
}

/// `POST /v1/exec`, reassembled into the same [`OneshotOutput`] the Docker
/// path builds. Like Docker's one-shot, there is no tty, stdin is empty, and
/// the working directory is the container's default.
async fn exec_oneshot_raw(
    ep: &AgentEndpoint,
    user: &str,
    cmd: Vec<String>,
    env: Vec<String>,
    limit: usize,
) -> Result<OneshotOutput, String> {
    const CONTEXT: &str = "Failed to start exec";
    let spec = ExecSpec::new(cmd).user(user).working_dir(None).env(env).tty(false);
    let mut body = spec_json(&spec);
    body.insert("limit".into(), serde_json::json!(limit));

    let resp = send(
        CONTEXT,
        http()
            .post(ep.url("v1/exec"))
            .header(reqwest::header::AUTHORIZATION, ep.auth())
            .json(&body),
    )
    .await?;

    let status = resp.status();
    if status == reqwest::StatusCode::PAYLOAD_TOO_LARGE {
        // Verbatim when it is the limit message (callers match on
        // OUTPUT_LIMIT_MARKER); anything else the agent puts in a 413 is not
        // trusted to speak for it.
        return Err(error_text(resp)
            .await
            .filter(|e| e.starts_with(OUTPUT_LIMIT_MARKER))
            .unwrap_or_else(|| output_limit_error(limit)));
    }
    if !status.is_success() {
        return Err(status_error(CONTEXT, resp).await);
    }

    // The agent enforces `limit`, but this is container-controlled input
    // being buffered into the host, so the client bounds it too: base64 is
    // 4/3 of the payload and each chunk carries a few bytes of JSON, so a
    // body past this is over the limit however it was chunked.
    let cap = limit.saturating_mul(8).saturating_add(64 * 1024);
    let raw = read_capped(resp, cap)
        .await?
        .ok_or_else(|| output_limit_error(limit))?;
    let parsed: OneshotResponse = serde_json::from_slice(&raw)
        .map_err(|e| format!("Exec output error: malformed agent response: {}", e))?;

    let mut out = OneshotOutput::new();
    let b64 = base64::engine::general_purpose::STANDARD;
    for chunk in parsed.chunks {
        let from_stdout = match chunk.s.as_str() {
            "out" => true,
            "err" => false,
            other => {
                return Err(format!(
                    "Exec output error: unknown stream {:?} in agent response",
                    other
                ))
            }
        };
        let bytes = b64
            .decode(chunk.d.as_bytes())
            .map_err(|e| format!("Exec output error: bad base64 from agent: {}", e))?;
        out.push(from_stdout, &bytes, limit)?;
    }
    out.exit_code = parsed.exit_code;
    Ok(out)
}

/// Combined output and exit code under `limit` — `exec_oneshot_limited` and,
/// with [`MAX_ONESHOT_OUTPUT`], every other combined-output variant.
pub async fn exec_oneshot_limited(
    ep: &AgentEndpoint,
    user: &str,
    cmd: Vec<String>,
    env: Vec<String>,
    limit: usize,
) -> Result<(String, i64), String> {
    let out = exec_oneshot_raw(ep, user, cmd, env, limit).await?;
    Ok((out.text(), out.exit_code))
}

/// `exec_oneshot_as`: combined output under [`MAX_ONESHOT_OUTPUT`].
pub async fn exec_oneshot_as(
    ep: &AgentEndpoint,
    user: &str,
    cmd: Vec<String>,
    env: Vec<String>,
) -> Result<(String, i64), String> {
    exec_oneshot_limited(ep, user, cmd, env, MAX_ONESHOT_OUTPUT).await
}

/// `exec_oneshot_streams_as`: `(stdout, stderr, exit_code)`.
pub async fn exec_oneshot_streams_as(
    ep: &AgentEndpoint,
    user: &str,
    cmd: Vec<String>,
    env: Vec<String>,
) -> Result<(String, String, i64), String> {
    let out = exec_oneshot_raw(ep, user, cmd, env, MAX_ONESHOT_OUTPUT).await?;
    Ok((out.stdout(), out.stderr(), out.exit_code))
}

// ── detached execs, status, resize ───────────────────────────────────────────

#[derive(Deserialize)]
struct ExecIdResponse {
    exec_id: String,
}

pub async fn exec_detached(ep: &AgentEndpoint, spec: &ExecSpec) -> Result<String, String> {
    const CONTEXT: &str = "Failed to start exec";
    let resp = send(
        CONTEXT,
        http()
            .post(ep.url("v1/exec/detached"))
            .timeout(SMALL_REQUEST_TIMEOUT)
            .header(reqwest::header::AUTHORIZATION, ep.auth())
            .json(&spec_json(spec)),
    )
    .await?;
    if !resp.status().is_success() {
        return Err(status_error(CONTEXT, resp).await);
    }
    small_json::<ExecIdResponse>(resp)
        .await
        .map(|r| r.exec_id)
        .ok_or_else(|| format!("{}: malformed agent response", CONTEXT))
}

#[derive(Deserialize)]
struct ExecStatus {
    running: bool,
    exit_code: Option<i64>,
}

/// Exit codes attach streams saw in their `exit` frame, so `wait_exec_exit`
/// can answer without a round trip. Bounded: a long-running app opens many
/// sessions, and the agent keeps the same answer for ten minutes anyway.
struct ExitBook {
    codes: HashMap<(String, String), i64>,
    order: VecDeque<(String, String)>,
}

const EXIT_BOOK_CAP: usize = 256;

fn exit_book() -> &'static Mutex<ExitBook> {
    static BOOK: OnceLock<Mutex<ExitBook>> = OnceLock::new();
    BOOK.get_or_init(|| {
        Mutex::new(ExitBook {
            codes: HashMap::new(),
            order: VecDeque::new(),
        })
    })
}

fn record_exit(base_url: &str, exec_id: &str, code: i64) {
    let key = (base_url.to_string(), exec_id.to_string());
    let mut book = exit_book().lock().unwrap_or_else(|p| p.into_inner());
    if book.codes.insert(key.clone(), code).is_none() {
        book.order.push_back(key);
        while book.order.len() > EXIT_BOOK_CAP {
            if let Some(old) = book.order.pop_front() {
                book.codes.remove(&old);
            }
        }
    }
}

fn recorded_exit(base_url: &str, exec_id: &str) -> Option<i64> {
    let book = exit_book().lock().unwrap_or_else(|p| p.into_inner());
    book.codes
        .get(&(base_url.to_string(), exec_id.to_string()))
        .copied()
}

/// Poll `GET /v1/exec/{id}` with Docker's budget. `None` on any error, an
/// unknown id, a finished exec with no code, or the budget running out —
/// exactly where `docker::exec::wait_for_exec_exit` says `None`.
pub async fn wait_exec_exit(ep: &AgentEndpoint, exec_id: &str) -> Option<i64> {
    // Docker's budget is a number of polls; bound the wall clock too, so a
    // slow-answering agent cannot stretch it (each poll is also bounded).
    let budget = EXEC_EXIT_POLL_INTERVAL * EXEC_EXIT_POLLS as u32 + SMALL_REQUEST_TIMEOUT;
    tokio::time::timeout(budget, wait_exec_exit_polls(ep, exec_id)).await.ok()?
}

async fn wait_exec_exit_polls(ep: &AgentEndpoint, exec_id: &str) -> Option<i64> {
    if let Some(code) = recorded_exit(ep.base_url.as_str(), exec_id) {
        return Some(code);
    }
    let url = ep.url(&format!("v1/exec/{}", urlencode_segment(exec_id)));
    for _ in 0..EXEC_EXIT_POLLS {
        if let Some(code) = recorded_exit(ep.base_url.as_str(), exec_id) {
            return Some(code);
        }
        let resp = http()
            .get(url.clone())
            .timeout(SMALL_REQUEST_TIMEOUT)
            .header(reqwest::header::AUTHORIZATION, ep.auth())
            .send()
            .await
            .ok()?;
        if !resp.status().is_success() {
            return None;
        }
        let status: ExecStatus = small_json(resp).await?;
        if !status.running {
            return status.exit_code;
        }
        tokio::time::sleep(EXEC_EXIT_POLL_INTERVAL).await;
    }
    None
}

pub async fn resize_exec(ep: &AgentEndpoint, exec_id: &str, cols: u16, rows: u16) -> Result<(), String> {
    const CONTEXT: &str = "Failed to resize exec";
    let resp = send(
        CONTEXT,
        http()
            .post(ep.url(&format!("v1/exec/{}/resize", urlencode_segment(exec_id))))
            .timeout(SMALL_REQUEST_TIMEOUT)
            .header(reqwest::header::AUTHORIZATION, ep.auth())
            .json(&serde_json::json!({ "cols": cols, "rows": rows })),
    )
    .await?;
    if !resp.status().is_success() {
        return Err(status_error(CONTEXT, resp).await);
    }
    Ok(())
}

/// Exec ids come from the agent; escape them anyway so one can never turn
/// into a different path.
fn urlencode_segment(s: &str) -> String {
    url::form_urlencoded::byte_serialize(s.as_bytes()).collect()
}

// ── attach ───────────────────────────────────────────────────────────────────

type Ws = tokio_tungstenite::WebSocketStream<
    tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
>;

#[derive(Deserialize)]
struct ControlFrame {
    #[serde(rename = "type")]
    kind: String,
    exec_id: Option<String>,
    code: Option<i64>,
    error: Option<String>,
}

/// What the stdin writer hands the socket task.
enum StdinCmd {
    Data(Bytes),
    Close,
}

/// Open `GET /v1/attach` and authenticate. Shared by attach (and, later,
/// tunnels).
async fn connect_ws(ep: &AgentEndpoint, path: &str, context: &str) -> Result<Ws, String> {
    let mut req = ep
        .ws_url(path)
        .as_str()
        .into_client_request()
        .map_err(|e| format!("{}: {}", context, e))?;
    let auth = ep.auth();
    req.headers_mut()
        .insert(tokio_tungstenite::tungstenite::http::header::AUTHORIZATION, auth);
    let config = tokio_tungstenite::tungstenite::protocol::WebSocketConfig::default()
        .max_message_size(Some(WS_MAX_MESSAGE))
        .max_frame_size(Some(WS_MAX_MESSAGE));
    match tokio::time::timeout(
        CONNECT_TIMEOUT,
        tokio_tungstenite::connect_async_with_config(req, Some(config), false),
    )
    .await
    {
        Err(_) => Err(format!("{}: timed out connecting to the agent", context)),
        Ok(Err(tokio_tungstenite::tungstenite::Error::Http(resp)))
            if resp.status() == tokio_tungstenite::tungstenite::http::StatusCode::UNAUTHORIZED =>
        {
            Err(format!("{}: the agent rejected the credentials (401)", context))
        }
        Ok(Err(e)) => Err(format!("{}: {}", context, e)),
        Ok(Ok((ws, _))) => Ok(ws),
    }
}

/// `GET /v1/attach`: the same [`AttachedExec`] Docker hands back.
///
/// Output: channel 1 → [`ExecChunk::StdOut`], 2 → [`ExecChunk::StdErr`]; the
/// `exit` frame ends the stream and is remembered for [`wait_exec_exit`].
/// Input: each write is one binary frame; `shutdown` sends `close_stdin`.
/// Dropping both halves closes the socket.
pub async fn attach(ep: &AgentEndpoint, spec: &ExecSpec) -> Result<AttachedExec, String> {
    const CONTEXT: &str = "Failed to start exec";
    let mut ws = connect_ws(ep, "v1/attach", CONTEXT).await?;

    let mut hello = spec_json(spec);
    hello.insert("attach_stdin".into(), serde_json::json!(spec.attach_stdin));
    let hello = serde_json::Value::Object(hello).to_string();
    ws.send(Message::text(hello))
        .await
        .map_err(|e| format!("{}: {}", CONTEXT, e))?;

    let exec_id = match tokio::time::timeout(ATTACH_START_TIMEOUT, wait_started(&mut ws)).await {
        Err(_) => return Err(format!("{}: the agent did not start the command", CONTEXT)),
        Ok(r) => r.map_err(|e| format!("{}: {}", CONTEXT, e))?,
    };

    let (mut sink, stream) = ws.split();

    // stdin: a bounded queue drained by one task that owns the socket's write
    // half. The task ends — dropping the write half — when the writer is
    // dropped or the socket fails.
    let (tx, mut rx) = tokio::sync::mpsc::channel::<StdinCmd>(STDIN_QUEUE);
    tokio::spawn(async move {
        while let Some(cmd) = rx.recv().await {
            let msg = match cmd {
                StdinCmd::Data(b) => Message::binary(b),
                StdinCmd::Close => Message::text(r#"{"type":"close_stdin"}"#),
            };
            if sink.send(msg).await.is_err() {
                break;
            }
        }
    });

    let base_url = ep.base_url.as_str().to_string();
    let id_for_stream = exec_id.clone();
    let output = futures_util::stream::unfold(Some(stream), move |state| {
        let base_url = base_url.clone();
        let exec_id = id_for_stream.clone();
        async move {
            let mut stream = state?;
            loop {
                match stream.next().await? {
                    Err(e) => return Some((Err(format!("Exec output error: {}", e)), None)),
                    Ok(Message::Binary(b)) => {
                        if b.is_empty() {
                            continue;
                        }
                        let payload = b.slice(1..);
                        let chunk = match b[0] {
                            1 => ExecChunk::StdOut(payload),
                            2 => ExecChunk::StdErr(payload),
                            _ => ExecChunk::Other(payload),
                        };
                        return Some((Ok(chunk), Some(stream)));
                    }
                    Ok(Message::Text(t)) => {
                        let frame: ControlFrame = match serde_json::from_str(t.as_str()) {
                            Ok(f) => f,
                            Err(_) => continue,
                        };
                        match frame.kind.as_str() {
                            "exit" => {
                                if let Some(code) = frame.code {
                                    record_exit(&base_url, &exec_id, code);
                                }
                                return None;
                            }
                            "error" => {
                                let e = frame.error.unwrap_or_else(|| "agent error".into());
                                return Some((Err(format!("Exec output error: {}", e)), None));
                            }
                            _ => continue,
                        }
                    }
                    Ok(Message::Close(_)) => return None,
                    Ok(_) => continue,
                }
            }
        }
    });

    Ok(AttachedExec {
        exec_id,
        output: Box::pin(output),
        input: Box::pin(AttachInput {
            tx: PollSender::new(tx),
            close_sent: false,
        }),
    })
}

/// Read until the agent says `started` (its exec id) or `error`.
async fn wait_started(ws: &mut Ws) -> Result<String, String> {
    while let Some(msg) = ws.next().await {
        match msg.map_err(|e| e.to_string())? {
            Message::Text(t) => {
                let frame: ControlFrame = serde_json::from_str(t.as_str())
                    .map_err(|e| format!("malformed agent frame: {}", e))?;
                match frame.kind.as_str() {
                    "started" => {
                        return frame
                            .exec_id
                            .ok_or_else(|| "agent sent no exec id".to_string())
                    }
                    "error" => {
                        return Err(frame.error.unwrap_or_else(|| "agent error".into()))
                    }
                    _ => continue,
                }
            }
            Message::Close(_) => break,
            _ => continue,
        }
    }
    Err("the agent closed the connection before starting the command".to_string())
}

/// An attach's stdin.
struct AttachInput {
    tx: PollSender<StdinCmd>,
    close_sent: bool,
}

fn broken_pipe() -> io::Error {
    io::Error::new(io::ErrorKind::BrokenPipe, "agent connection closed")
}

impl AsyncWrite for AttachInput {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        if buf.is_empty() {
            return Poll::Ready(Ok(0));
        }
        if this.close_sent {
            return Poll::Ready(Err(broken_pipe()));
        }
        match this.tx.poll_reserve(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(Err(_)) => Poll::Ready(Err(broken_pipe())),
            Poll::Ready(Ok(())) => {
                // One frame per write, at most STDIN_FRAME_MAX; `write_all`
                // loops for the rest. Keeps every frame under the agent's
                // WebSocket message limit.
                let n = buf.len().min(STDIN_FRAME_MAX);
                this.tx
                    .send_item(StdinCmd::Data(Bytes::copy_from_slice(&buf[..n])))
                    .map_err(|_| broken_pipe())?;
                Poll::Ready(Ok(n))
            }
        }
    }

    /// Frames are flushed by the socket task as it sends them.
    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    /// EOF on the child's stdin: `{"type":"close_stdin"}`, once.
    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        if this.close_sent {
            return Poll::Ready(Ok(()));
        }
        match this.tx.poll_reserve(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(Err(_)) => Poll::Ready(Err(broken_pipe())),
            Poll::Ready(Ok(())) => {
                this.tx.send_item(StdinCmd::Close).map_err(|_| broken_pipe())?;
                this.close_sent = true;
                Poll::Ready(Ok(()))
            }
        }
    }
}

// ── files ────────────────────────────────────────────────────────────────────

/// `(uid, gid)` of `claude`, falling back to `(1000, 1000)` — the same probe
/// and parse as Docker's `container_user_ids`.
pub async fn user_ids(ep: &AgentEndpoint) -> (u64, u64) {
    let (out, _) = exec_oneshot_limited(ep, "claude", user_ids_cmd(), Vec::new(), USER_IDS_OUTPUT_LIMIT)
        .await
        .unwrap_or_default();
    parse_user_ids(&out)
}

/// Split Docker's tar-entry `name` (which may carry a relative directory, as
/// the terminal drop's `triple-c-drops/<file>` does) into the `dir` and
/// single-component `name` `PUT /v1/files` takes. Docker's extractor would
/// put the file at the same place.
fn split_upload_target(dest_dir: &str, file_name: &str) -> Result<(String, String), String> {
    let parts: Vec<&str> = file_name.split('/').filter(|p| !p.is_empty()).collect();
    let Some((name, parents)) = parts.split_last() else {
        return Err(format!("Failed to upload file to container: empty file name {:?}", file_name));
    };
    if parts.iter().any(|p| *p == "." || *p == "..") {
        return Err(format!(
            "Failed to upload file to container: {:?} is not a plain relative name",
            file_name
        ));
    }
    let mut dir = dest_dir.trim_end_matches('/').to_string();
    for p in parents {
        dir.push('/');
        dir.push_str(p);
    }
    if dir.is_empty() {
        dir.push('/');
    }
    Ok((dir, name.to_string()))
}

async fn put_file(
    ep: &AgentEndpoint,
    dest_dir: &str,
    file_name: &str,
    data: Vec<u8>,
    mode: u32,
    (uid, gid): (u64, u64),
) -> Result<(), String> {
    const CONTEXT: &str = "Failed to upload file to container";
    let (dir, name) = split_upload_target(dest_dir, file_name)?;
    let mut url = ep.url("v1/files");
    url.query_pairs_mut()
        .append_pair("dir", &dir)
        .append_pair("name", &name)
        .append_pair("mode", &format!("{:o}", mode))
        .append_pair("uid", &uid.to_string())
        .append_pair("gid", &gid.to_string());
    // Generous, but finite: a minute plus 256 KiB/s for the payload.
    let timeout = Duration::from_secs(60 + (data.len() as u64) / (256 * 1024));
    let resp = send(
        CONTEXT,
        http()
            .put(url)
            .timeout(timeout)
            .header(reqwest::header::AUTHORIZATION, ep.auth())
            .header(reqwest::header::CONTENT_TYPE, "application/octet-stream")
            .body(data),
    )
    .await?;
    if !resp.status().is_success() {
        return Err(status_error(CONTEXT, resp).await);
    }
    Ok(())
}

/// Docker's `upload_bytes_with_ids`: same reported path.
pub async fn upload_bytes_with_ids(
    ep: &AgentEndpoint,
    dest_dir: &str,
    file_name: &str,
    data: &[u8],
    mode: u32,
    ids: (u64, u64),
) -> Result<String, String> {
    put_file(ep, dest_dir, file_name, data.to_vec(), mode, ids).await?;
    Ok(format!("{}/{}", dest_dir.trim_end_matches('/'), file_name))
}

/// Docker's `upload_host_file_with_ids`: the host file is read (and checked)
/// by the same function, mode 0644, same reported path.
pub async fn upload_host_file_with_ids(
    ep: &AgentEndpoint,
    host_path: &str,
    dest_dir: &str,
    dest_name: &str,
    ids: (u64, u64),
) -> Result<String, String> {
    let path = host_path.to_string();
    let data = tokio::task::spawn_blocking(move || read_host_file_for_upload(&path))
        .await
        .map_err(|e| format!("Upload task panicked: {}", e))??;
    put_file(ep, dest_dir, dest_name, data, HOST_FILE_UPLOAD_MODE, ids).await?;
    Ok(container_join(dest_dir, dest_name))
}

/// `GET /v1/archive`, streamed. Dropping the stream drops the response,
/// which closes the connection and cancels the read on the agent.
pub fn download_archive(ep: &AgentEndpoint, path: &str) -> ArchiveStream {
    const CONTEXT: &str = "Failed to download archive";
    let mut url = ep.url("v1/archive");
    url.query_pairs_mut().append_pair("path", path);
    let req = http()
        .get(url)
        .header(reqwest::header::AUTHORIZATION, ep.auth());

    enum State {
        Start(reqwest::RequestBuilder),
        Body(reqwest::Response),
    }
    Box::pin(futures_util::stream::unfold(
        Some(State::Start(req)),
        |state| async move {
            let mut resp = match state? {
                State::Start(req) => match req.send().await {
                    Err(e) => return Some((Err(format!("{}: {}", CONTEXT, e)), None)),
                    Ok(r) if !r.status().is_success() => {
                        return Some((Err(status_error(CONTEXT, r).await), None))
                    }
                    Ok(r) => r,
                },
                State::Body(r) => r,
            };
            loop {
                match resp.chunk().await {
                    Err(e) => return Some((Err(format!("{}: {}", CONTEXT, e)), None)),
                    Ok(None) => return None,
                    Ok(Some(b)) if b.is_empty() => continue,
                    Ok(Some(b)) => return Some((Ok(b), Some(State::Body(resp)))),
                }
            }
        },
    ))
}

// ── state ────────────────────────────────────────────────────────────────────

/// `GET /v1/health` answers → `true`. Can't connect (or times out) → `false`,
/// the same "not running" an inspect failure gives on Docker. An agent that
/// answers with an error status is reachable but unusable, which is an
/// `Err` rather than a quiet `false`.
pub async fn is_running(ep: &AgentEndpoint) -> Result<bool, String> {
    let resp = match http()
        .get(ep.url("v1/health"))
        .header(reqwest::header::AUTHORIZATION, ep.auth())
        .timeout(HEALTH_TIMEOUT)
        .send()
        .await
    {
        Ok(r) => r,
        Err(_) => return Ok(false),
    };
    if !resp.status().is_success() {
        return Err(status_error("Agent health check failed", resp).await);
    }
    Ok(true)
}

#[cfg(test)]
mod tests;
#[cfg(test)]
mod live_tests;
