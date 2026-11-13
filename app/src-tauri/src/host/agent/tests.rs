//! The agent client against a **fake** agent: a few axum handlers that speak
//! just enough of the v1 protocol to exercise each client path, and record
//! what they were sent. It is not the real server (that lives in `agent/`)
//! and does not try to behave like one beyond the wire shapes.

use super::*;
use crate::host::{ContainerTarget, OUTPUT_LIMIT_MARKER};
use axum::extract::ws::{Message as WsMessage, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use serde_json::{json, Value};
use std::sync::Arc;
use tokio::io::AsyncWriteExt;

/// Obviously fake; long enough to be unmistakable in an error string.
const TOKEN: &str = "fake-agent-token-for-unit-tests-only-0000";

#[derive(Default)]
struct Seen {
    auth: Vec<String>,
    exec_bodies: Vec<Value>,
    detached_bodies: Vec<Value>,
    status_polls: u32,
    resizes: Vec<(String, Value)>,
    uploads: Vec<(HashMap<String, String>, Vec<u8>)>,
    attach_hello: Option<Value>,
    stdin: Vec<u8>,
    close_stdin: bool,
}

struct Fake {
    seen: std::sync::Mutex<Seen>,
}

type St = State<Arc<Fake>>;

impl Fake {
    fn authed(&self, h: &HeaderMap) -> bool {
        let got = h
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        let ok = got == format!("Bearer {}", TOKEN);
        self.seen.lock().unwrap().auth.push(got);
        ok
    }
}

fn unauthorized() -> Response {
    StatusCode::UNAUTHORIZED.into_response()
}

fn b64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

async fn exec(State(f): St, h: HeaderMap, Json(body): Json<Value>) -> Response {
    if !f.authed(&h) {
        return unauthorized();
    }
    f.seen.lock().unwrap().exec_bodies.push(body.clone());
    match body["cmd"][0].as_str().unwrap_or("") {
        "sh" => Json(json!({"chunks": [{"s": "out", "d": b64(b"1000\n1001\n")}], "exit_code": 0}))
            .into_response(),
        // stdout and stderr interleaved; `ü` split across two stdout chunks;
        // one byte that is not UTF-8 at all on stderr.
        "mixed" => Json(json!({
            "chunks": [
                {"s": "out", "d": b64(b"first ")},
                {"s": "err", "d": b64(b"find: /x: denied\n")},
                {"s": "out", "d": b64(&[0xc3])},
                {"s": "out", "d": b64(&[0xbc, b'!'])},
                {"s": "err", "d": b64(&[0xff])},
            ],
            "exit_code": 3
        }))
        .into_response(),
        "big" => (
            StatusCode::PAYLOAD_TOO_LARGE,
            Json(json!({"error": "OUTPUT_LIMIT: Command output exceeded 5 bytes and was abandoned"})),
        )
            .into_response(),
        _ => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": "boom"}))).into_response(),
    }
}

async fn detached(State(f): St, h: HeaderMap, Json(body): Json<Value>) -> Response {
    if !f.authed(&h) {
        return unauthorized();
    }
    f.seen.lock().unwrap().detached_bodies.push(body);
    Json(json!({"exec_id": "d1"})).into_response()
}

/// `d1` reports running for two polls, then exit 7. Everything else is 404 —
/// including the attach's `a1`, so an attach's exit can only have come from
/// the client's own record of the `exit` frame.
async fn status(State(f): St, h: HeaderMap, Path(id): Path<String>) -> Response {
    if !f.authed(&h) {
        return unauthorized();
    }
    if id != "d1" {
        return (StatusCode::NOT_FOUND, Json(json!({"error": "no such exec"}))).into_response();
    }
    let mut s = f.seen.lock().unwrap();
    s.status_polls += 1;
    if s.status_polls < 3 {
        Json(json!({"running": true, "exit_code": null})).into_response()
    } else {
        Json(json!({"running": false, "exit_code": 7})).into_response()
    }
}

async fn resize(State(f): St, h: HeaderMap, Path(id): Path<String>, Json(body): Json<Value>) -> Response {
    if !f.authed(&h) {
        return unauthorized();
    }
    f.seen.lock().unwrap().resizes.push((id, body));
    StatusCode::NO_CONTENT.into_response()
}

async fn files(
    State(f): St,
    h: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
    body: Bytes,
) -> Response {
    if !f.authed(&h) {
        return unauthorized();
    }
    let path = format!("{}/{}", q["dir"], q["name"]);
    f.seen.lock().unwrap().uploads.push((q, body.to_vec()));
    Json(json!({"path": path})).into_response()
}

async fn archive(State(f): St, h: HeaderMap, Query(q): Query<HashMap<String, String>>) -> Response {
    if !f.authed(&h) {
        return unauthorized();
    }
    if q["path"] == "/missing" {
        return (StatusCode::NOT_FOUND, Json(json!({"error": "no such path"}))).into_response();
    }
    let parts: Vec<Result<Bytes, std::io::Error>> = vec![
        Ok(Bytes::from_static(b"tar-part-1;")),
        Ok(Bytes::from_static(b"tar-part-2;")),
        Ok(Bytes::from(q["path"].clone().into_bytes())),
    ];
    axum::body::Body::from_stream(futures_util::stream::iter(parts)).into_response()
}

async fn health(State(f): St, h: HeaderMap) -> Response {
    if !f.authed(&h) {
        return unauthorized();
    }
    Json(json!({"agent": "triple-c-agent", "version": "0.0.0-fake", "protocol": 1})).into_response()
}

async fn attach_ws(State(f): St, h: HeaderMap, ws: WebSocketUpgrade) -> Response {
    if !f.authed(&h) {
        return unauthorized();
    }
    ws.on_upgrade(move |socket| fake_attach(f, socket))
}

/// started → stdout, stderr, an unknown channel → collect stdin until
/// `close_stdin` → echo it on stdout → exit 5 → close.
async fn fake_attach(f: Arc<Fake>, mut ws: WebSocket) {
    let Some(Ok(WsMessage::Text(hello))) = ws.recv().await else { return };
    let hello: Value = serde_json::from_str(hello.as_str()).unwrap();
    let refuse = hello["cmd"][0] == "nope";
    f.seen.lock().unwrap().attach_hello = Some(hello);
    if refuse {
        let _ = ws
            .send(WsMessage::Text(r#"{"type":"error","error":"no such user"}"#.into()))
            .await;
        return;
    }
    let _ = ws
        .send(WsMessage::Text(r#"{"type":"started","exec_id":"a1"}"#.into()))
        .await;
    for frame in [&b"\x01out"[..], b"\x02err", b"\x09odd"] {
        let _ = ws.send(WsMessage::Binary(Bytes::copy_from_slice(frame))).await;
    }
    while let Some(Ok(msg)) = ws.recv().await {
        match msg {
            WsMessage::Binary(b) => f.seen.lock().unwrap().stdin.extend_from_slice(&b),
            WsMessage::Text(t) if t.as_str().contains("close_stdin") => {
                f.seen.lock().unwrap().close_stdin = true;
                break;
            }
            _ => {}
        }
    }
    let mut echo = vec![1u8];
    echo.extend_from_slice(&f.seen.lock().unwrap().stdin);
    let _ = ws.send(WsMessage::Binary(echo.into())).await;
    let _ = ws.send(WsMessage::Text(r#"{"type":"exit","code":5}"#.into())).await;
    let _ = ws.close().await;
}

async fn fake_agent() -> (AgentEndpoint, Arc<Fake>) {
    let fake = Arc::new(Fake {
        seen: std::sync::Mutex::new(Seen::default()),
    });
    let app = Router::new()
        .route("/v1/health", get(health))
        .route("/v1/exec", post(exec))
        .route("/v1/exec/detached", post(detached))
        .route("/v1/exec/{id}", get(status))
        .route("/v1/exec/{id}/resize", post(resize))
        .route("/v1/files", put(files))
        .route("/v1/archive", get(archive))
        .route("/v1/attach", get(attach_ws))
        .with_state(fake.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (AgentEndpoint::loopback(port, TOKEN), fake)
}

fn target(ep: &AgentEndpoint) -> ContainerTarget {
    ContainerTarget::Agent(ep.clone())
}

fn cmd(c: &str) -> Vec<String> {
    vec![c.to_string()]
}

// ── endpoint ─────────────────────────────────────────────────────────────────

#[test]
fn the_token_never_appears_in_debug_output() {
    let ep = AgentEndpoint::loopback(47900, TOKEN);
    let dbg = format!("{:?} {:?}", ep, ContainerTarget::Agent(ep.clone()));
    assert!(!dbg.contains(TOKEN), "{}", dbg);
    assert!(dbg.contains("127.0.0.1:47900"));
    assert!(dbg.contains("redacted"));
}

#[test]
fn an_agent_target_is_identified_by_its_base_url() {
    let a = ContainerTarget::Agent(AgentEndpoint::loopback(47900, TOKEN));
    assert_eq!(a.id(), "http://127.0.0.1:47900/");
    assert_eq!(a, ContainerTarget::Agent(AgentEndpoint::loopback(47900, TOKEN)));
    assert_ne!(a, ContainerTarget::Agent(AgentEndpoint::loopback(47901, TOKEN)));
    let mut set = std::collections::HashSet::new();
    set.insert(a.clone());
    assert!(set.contains(&a));
}

#[test]
fn only_plain_http_endpoints_are_accepted() {
    let ok = AgentEndpoint::new(url::Url::parse("http://127.0.0.1:1/x").unwrap(), TOKEN).unwrap();
    assert_eq!(ok.base_url.as_str(), "http://127.0.0.1:1/x/");
    assert_eq!(ok.url("v1/health").as_str(), "http://127.0.0.1:1/x/v1/health");
    assert_eq!(ok.ws_url("v1/attach").as_str(), "ws://127.0.0.1:1/x/v1/attach");
    assert!(AgentEndpoint::new(url::Url::parse("https://h/").unwrap(), TOKEN).is_err());
    assert!(AgentEndpoint::new(url::Url::parse("file:///tmp").unwrap(), TOKEN).is_err());
}

// ── auth ─────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn every_request_carries_the_bearer_token() {
    let (ep, fake) = fake_agent().await;
    let t = target(&ep);
    assert!(t.is_running().await.unwrap());
    t.exec_oneshot(cmd("sh")).await.unwrap();
    let auth = fake.seen.lock().unwrap().auth.clone();
    assert_eq!(auth.len(), 2);
    assert!(auth.iter().all(|a| a == &format!("Bearer {}", TOKEN)));
}

#[tokio::test]
async fn a_wrong_token_is_an_error_that_does_not_carry_any_token() {
    let (ep, _) = fake_agent().await;
    let wrong_token = "wrong-fake-token-for-unit-tests-only-1111";
    let bad = ContainerTarget::Agent(AgentEndpoint::new(ep.base_url.clone(), wrong_token).unwrap());

    let e = bad.exec_oneshot(cmd("sh")).await.unwrap_err();
    assert!(e.contains("401"), "{}", e);
    assert!(e.starts_with("Failed to start exec"), "{}", e);
    let Err(attach_err) = bad.attach(ExecSpec::new(cmd("sh"))).await else {
        panic!("attach with a wrong token succeeded")
    };
    assert!(attach_err.contains("401"), "{}", attach_err);
    let health_err = bad.is_running().await.unwrap_err();
    let up_err = bad.upload_bytes("/tmp", "x", b"x", 0o644).await.unwrap_err();
    for e in [&e, &attach_err, &health_err, &up_err] {
        assert!(!e.contains(wrong_token) && !e.contains(TOKEN), "{}", e);
    }
}

#[tokio::test]
async fn other_errors_carry_the_agents_message_and_no_token() {
    let (ep, _) = fake_agent().await;
    let e = target(&ep).exec_oneshot(cmd("fail")).await.unwrap_err();
    assert_eq!(e, "Failed to start exec: boom");
}

// ── one-shot execs ───────────────────────────────────────────────────────────

#[tokio::test]
async fn every_oneshot_variant_reassembles_what_docker_would_return() {
    let (ep, fake) = fake_agent().await;
    let t = target(&ep);
    // Arrival order, `ü` rejoined across chunks, the stray 0xff lossy.
    let combined = "first find: /x: denied\nü!\u{fffd}";
    let env = vec!["K=V".to_string()];

    assert_eq!(t.exec_oneshot(cmd("mixed")).await.unwrap(), combined);
    assert_eq!(t.exec_oneshot_env(cmd("mixed"), env.clone()).await.unwrap(), combined);
    assert_eq!(
        t.exec_oneshot_env_status(cmd("mixed"), env.clone()).await.unwrap(),
        (combined.to_string(), 3)
    );
    assert_eq!(
        t.exec_oneshot_as("root", cmd("mixed"), env.clone()).await.unwrap(),
        (combined.to_string(), 3)
    );
    assert_eq!(
        t.exec_oneshot_limited("root", cmd("mixed"), 4096).await.unwrap(),
        (combined.to_string(), 3)
    );
    assert_eq!(
        t.exec_oneshot_streams_as("root", cmd("mixed"), env.clone()).await.unwrap(),
        ("first ü!".to_string(), "find: /x: denied\n\u{fffd}".to_string(), 3)
    );

    let bodies = fake.seen.lock().unwrap().exec_bodies.clone();
    let max = MAX_ONESHOT_OUTPUT as u64;
    let expect = [
        ("claude", json!([]), max),
        ("claude", json!(["K=V"]), max),
        ("claude", json!(["K=V"]), max),
        ("root", json!(["K=V"]), max),
        ("root", json!([]), 4096),
        ("root", json!(["K=V"]), max),
    ];
    assert_eq!(bodies.len(), expect.len());
    for (b, (user, env, limit)) in bodies.iter().zip(expect) {
        assert_eq!(b["user"], user);
        assert_eq!(b["env"], env);
        assert_eq!(b["limit"].as_u64(), Some(limit));
        assert_eq!(b["cmd"], json!(["mixed"]));
        // Docker's one-shot names no working dir and no tty.
        assert_eq!(b["working_dir"], Value::Null);
        assert_eq!(b["tty"], false);
    }
}

#[tokio::test]
async fn an_output_limit_refusal_passes_through_verbatim() {
    let (ep, _) = fake_agent().await;
    let e = target(&ep).exec_oneshot_limited("claude", cmd("big"), 5).await.unwrap_err();
    assert_eq!(e, "OUTPUT_LIMIT: Command output exceeded 5 bytes and was abandoned");
    assert!(e.starts_with(OUTPUT_LIMIT_MARKER));
}

#[tokio::test]
async fn the_client_also_enforces_the_limit_on_what_it_buffers() {
    // An agent that ignored `limit` still cannot make the client buffer past it.
    let (ep, _) = fake_agent().await;
    let e = target(&ep).exec_oneshot_limited("claude", cmd("mixed"), 8).await.unwrap_err();
    assert!(e.starts_with(OUTPUT_LIMIT_MARKER), "{}", e);
}

#[tokio::test]
async fn user_ids_come_from_the_same_probe_as_docker() {
    let (ep, fake) = fake_agent().await;
    assert_eq!(target(&ep).user_ids().await, (1000, 1001));
    let b = fake.seen.lock().unwrap().exec_bodies[0].clone();
    assert_eq!(b["cmd"], json!(["sh", "-c", "id -u; id -g"]));
    assert_eq!(b["user"], "claude");
    assert_eq!(b["limit"], 256);
}

// ── detached, status, resize ─────────────────────────────────────────────────

#[tokio::test]
async fn a_detached_exec_is_polled_until_it_reports_its_exit() {
    let (ep, fake) = fake_agent().await;
    let t = target(&ep);
    let spec = ExecSpec::new(cmd("sleep")).user("root").working_dir(None).no_stdin();
    let id = t.exec_detached(spec).await.unwrap();
    assert_eq!(id, "d1");
    assert_eq!(t.wait_exec_exit(&id).await, Some(7));
    let s = fake.seen.lock().unwrap();
    assert_eq!(s.status_polls, 3);
    assert_eq!(s.detached_bodies[0]["user"], "root");
    assert_eq!(s.detached_bodies[0]["working_dir"], Value::Null);
    drop(s);
    assert_eq!(t.wait_exec_exit("unknown").await, None);
}

#[tokio::test]
async fn resize_posts_cols_and_rows() {
    let (ep, fake) = fake_agent().await;
    target(&ep).resize_exec("a1", 120, 40).await.unwrap();
    let r = fake.seen.lock().unwrap().resizes.clone();
    assert_eq!(r, vec![("a1".to_string(), json!({"cols": 120, "rows": 40}))]);
}

// ── attach ───────────────────────────────────────────────────────────────────

#[tokio::test]
async fn attach_maps_channels_forwards_stdin_and_ends_on_exit() {
    let (ep, fake) = fake_agent().await;
    let t = target(&ep);
    let spec = ExecSpec::new(cmd("cat")).env(vec!["A=1".into()]).tty(true);
    let AttachedExec {
        exec_id,
        mut output,
        mut input,
    } = t.attach(spec).await.unwrap();
    assert_eq!(exec_id, "a1");

    assert_eq!(output.next().await.unwrap().unwrap(), ExecChunk::StdOut(Bytes::from_static(b"out")));
    assert_eq!(output.next().await.unwrap().unwrap(), ExecChunk::StdErr(Bytes::from_static(b"err")));
    assert_eq!(output.next().await.unwrap().unwrap(), ExecChunk::Other(Bytes::from_static(b"odd")));

    input.write_all(b"hello ").await.unwrap();
    input.write_all(b"agent").await.unwrap();
    input.shutdown().await.unwrap();

    assert_eq!(
        output.next().await.unwrap().unwrap(),
        ExecChunk::StdOut(Bytes::from_static(b"hello agent"))
    );
    // The exit frame ends the stream…
    assert!(output.next().await.is_none());
    // …and is what answers wait_exec_exit (the fake 404s `a1`).
    assert_eq!(t.wait_exec_exit(&exec_id).await, Some(5));

    let s = fake.seen.lock().unwrap();
    assert!(s.close_stdin);
    assert_eq!(s.stdin, b"hello agent");
    let hello = s.attach_hello.clone().unwrap();
    assert_eq!(hello["cmd"], json!(["cat"]));
    assert_eq!(hello["user"], "claude");
    assert_eq!(hello["working_dir"], "/workspace");
    assert_eq!(hello["env"], json!(["A=1"]));
    assert_eq!(hello["tty"], true);
    assert_eq!(hello["attach_stdin"], true);
}

#[tokio::test]
async fn an_attach_the_agent_refuses_is_an_error() {
    let (ep, _) = fake_agent().await;
    let Err(e) = target(&ep).attach(ExecSpec::new(cmd("nope"))).await else {
        panic!("a refused attach succeeded")
    };
    assert_eq!(e, "Failed to start exec: no such user");
}

// ── files ────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn uploads_put_the_bytes_with_dir_name_mode_and_owner() {
    let (ep, fake) = fake_agent().await;
    let t = target(&ep);
    assert_eq!(
        t.upload_bytes_with_ids("/tmp/", "x.txt", b"hi", 0o755, (1000, 1001))
            .await
            .unwrap(),
        "/tmp/x.txt"
    );
    assert_eq!(
        t.upload_bytes("/home/claude/in", "p.tar", b"tar", 0o644).await.unwrap(),
        "/home/claude/in/p.tar"
    );
    // The terminal drop's nested name: Docker's tar entry would land it in the
    // subdirectory, so the agent is asked for that directory.
    assert_eq!(
        t.upload_bytes_with_ids("/tmp", "triple-c-drops/a.txt", b"a", 0o644, (1, 2))
            .await
            .unwrap(),
        "/tmp/triple-c-drops/a.txt"
    );
    assert!(t.upload_bytes("/tmp", "../etc/x", b"", 0o644).await.is_err());

    let up = fake.seen.lock().unwrap().uploads.clone();
    let q = |i: usize, k: &str| up[i].0[k].clone();
    assert_eq!((q(0, "dir"), q(0, "name"), q(0, "mode")), ("/tmp".into(), "x.txt".into(), "755".into()));
    assert_eq!((q(0, "uid"), q(0, "gid")), ("1000".into(), "1001".into()));
    assert_eq!(up[0].1, b"hi");
    assert_eq!((q(1, "uid"), q(1, "gid"), q(1, "mode")), ("0".into(), "0".into(), "644".into()));
    assert_eq!((q(2, "dir"), q(2, "name")), ("/tmp/triple-c-drops".into(), "a.txt".into()));
    assert_eq!(up.len(), 3);
}

#[tokio::test]
async fn a_host_file_upload_reads_through_the_shared_checked_reader() {
    let (ep, fake) = fake_agent().await;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("notes.txt");
    std::fs::write(&path, b"from the host").unwrap();
    let path = path.canonicalize().unwrap();
    let t = target(&ep);
    let got = t
        .upload_host_file_with_ids(path.to_str().unwrap(), "/workspace/", "notes.txt", (7, 8))
        .await
        .unwrap();
    assert_eq!(got, "/workspace/notes.txt");
    // Without ids: resolved through the uid probe first.
    t.upload_host_file(path.to_str().unwrap(), "/tmp", "n2.txt").await.unwrap();

    let up = fake.seen.lock().unwrap().uploads.clone();
    assert_eq!(up[0].1, b"from the host");
    assert_eq!(up[0].0["mode"], "644");
    assert_eq!((up[0].0["uid"].as_str(), up[0].0["gid"].as_str()), ("7", "8"));
    assert_eq!((up[1].0["uid"].as_str(), up[1].0["gid"].as_str()), ("1000", "1001"));

    let missing = dir.path().join("gone.txt");
    let e = t
        .upload_host_file_with_ids(missing.to_str().unwrap(), "/tmp", "g", (1, 1))
        .await
        .unwrap_err();
    assert!(e.starts_with("Failed to read"), "{}", e);
}

#[tokio::test]
async fn an_archive_is_streamed_and_a_missing_path_is_an_error() {
    let (ep, _) = fake_agent().await;
    let t = target(&ep);
    let mut s = t.download_archive("/workspace/a dir");
    let mut got = Vec::new();
    while let Some(chunk) = s.next().await {
        got.extend_from_slice(&chunk.unwrap());
    }
    assert_eq!(got, b"tar-part-1;tar-part-2;/workspace/a dir");

    let mut s = t.download_archive("/missing");
    let e = s.next().await.unwrap().unwrap_err();
    assert_eq!(e, "Failed to download archive: no such path");
    assert!(s.next().await.is_none());
}

// ── state ────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn is_running_follows_whether_the_agent_answers() {
    let (ep, _) = fake_agent().await;
    assert!(target(&ep).is_running().await.unwrap());
    assert!(!target(&ep).has_exited().await);

    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    drop(l);
    let gone = ContainerTarget::Agent(AgentEndpoint::loopback(port, TOKEN));
    assert!(!gone.is_running().await.unwrap());
    // Silence is not a positive exit.
    assert!(!gone.has_exited().await);
}

// ── a hostile agent ──────────────────────────────────────────────────────────
// The agent runs inside the container, which is not trusted with the host.

/// Serve `app` on an ephemeral loopback port; return the port.
async fn serve(app: Router) -> u16 {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    port
}

#[tokio::test]
async fn a_redirect_from_the_agent_is_not_followed() {
    let hits = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let elsewhere = {
        let hits = hits.clone();
        serve(Router::new().fallback(move || {
            let hits = hits.clone();
            async move {
                hits.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                "followed"
            }
        }))
        .await
    };
    let to = format!("http://127.0.0.1:{}/anything", elsewhere);
    let agent = serve(Router::new().fallback(move || {
        let to = to.clone();
        async move { (StatusCode::TEMPORARY_REDIRECT, [(axum::http::header::LOCATION, to)]) }
    }))
    .await;
    let t = ContainerTarget::Agent(AgentEndpoint::loopback(agent, TOKEN));

    assert!(t.exec_detached(ExecSpec::new(cmd("true"))).await.is_err());
    assert!(t.exec_oneshot(cmd("true")).await.is_err());
    assert!(t.is_running().await.is_err(), "a 3xx health answer is not 'running'");
    assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 0, "the client followed a redirect");
}

#[tokio::test]
async fn an_oversized_error_body_is_neither_buffered_nor_passed_through_whole() {
    // 8 MiB of error text: past the small-body cap, so it is not even parsed.
    let huge = serve(Router::new().fallback(|| async {
        let body = json!({ "error": "x".repeat(8 * 1024 * 1024) });
        (StatusCode::INTERNAL_SERVER_ERROR, Json(body))
    }))
    .await;
    let e = ContainerTarget::Agent(AgentEndpoint::loopback(huge, TOKEN))
        .exec_detached(ExecSpec::new(cmd("true")))
        .await
        .unwrap_err();
    assert!(e.len() < 1024, "error was {} bytes", e.len());
    assert!(e.contains("500"), "{e}");

    // Under the body cap but long: parsed, then cut to the display cap.
    let long = serve(Router::new().fallback(|| async {
        let body = json!({ "error": "é".repeat(20 * 1024) });
        (StatusCode::INTERNAL_SERVER_ERROR, Json(body))
    }))
    .await;
    let e = ContainerTarget::Agent(AgentEndpoint::loopback(long, TOKEN))
        .exec_detached(ExecSpec::new(cmd("true")))
        .await
        .unwrap_err();
    assert!(e.len() <= 4 * 1024 + 64, "error was {} bytes", e.len());
    assert!(e.ends_with('…'), "{e}");
}
