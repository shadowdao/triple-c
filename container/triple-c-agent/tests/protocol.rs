//! End-to-end tests of the wire protocol
//! (`docs/superpowers/specs/2026-11-13-triple-c-agent-protocol.md`): the real
//! server, in-process on `127.0.0.1:0`, driven over real HTTP and WebSocket,
//! running as the current (non-root) user.

use std::ffi::OsString;
use std::net::SocketAddr;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use base64::Engine;
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message;
use triple_c_agent::{serve, Config};

const TOKEN: &str = "test-token-0123456789abcdef0123456789abcdef";

type Ws = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

struct Agent {
    addr: SocketAddr,
    http: reqwest::Client,
    /// The agent's startup cwd — where `working_dir: null` runs.
    cwd: tempfile::TempDir,
    /// Scratch space for the test's own files.
    tmp: tempfile::TempDir,
    user: String,
}

fn current_user() -> String {
    let out = std::process::Command::new("id").arg("-un").output().unwrap();
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

fn passwd_home(user: &str) -> String {
    std::fs::read_to_string("/etc/passwd")
        .unwrap()
        .lines()
        .map(|l| l.split(':').collect::<Vec<_>>())
        .find(|f| f[0] == user)
        .map(|f| f[5].to_string())
        .expect("current user is in /etc/passwd")
}

fn is_root() -> bool {
    unsafe { libc::geteuid() == 0 }
}

async fn start() -> Agent {
    start_with(|_| {}).await
}

async fn start_with(tweak: impl FnOnce(&mut Config)) -> Agent {
    let cwd = tempfile::tempdir().unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let mut config = Config {
        token: TOKEN.to_string(),
        default_cwd: cwd.path().to_path_buf(),
        base_env: vec![
            ("PATH".into(), "/usr/local/bin:/usr/bin:/bin".into()),
            ("FOO".into(), "base".into()),
            ("BASE_ONLY".into(), "base".into()),
            ("GONE".into(), "present".into()),
            // The agent runs as root with HOME=/root in production; passwd
            // must win over the base env for the exec's user.
            (OsString::from("HOME"), OsString::from("/agent-home-must-not-leak")),
        ],
        max_file_bytes: 1024,
        header_read_timeout: triple_c_agent::HEADER_READ_TIMEOUT,
        max_connections: triple_c_agent::MAX_CONNECTIONS,
    };
    tweak(&mut config);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(serve(listener, config));
    Agent { addr, http: reqwest::Client::new(), cwd, tmp, user: current_user() }
}

impl Agent {
    fn url(&self, path: &str) -> String {
        format!("http://{}{}", self.addr, path)
    }

    async fn get(&self, path: &str) -> (u16, Value) {
        let r = self.http.get(self.url(path)).bearer_auth(TOKEN).send().await.unwrap();
        let status = r.status().as_u16();
        (status, r.json().await.unwrap_or(Value::Null))
    }

    async fn post(&self, path: &str, body: Value) -> (u16, Value) {
        let r = self.http.post(self.url(path)).bearer_auth(TOKEN).json(&body).send().await.unwrap();
        let status = r.status().as_u16();
        (status, r.json().await.unwrap_or(Value::Null))
    }

    /// One-shot as the current user, defaulting the rest of the spec.
    async fn exec(&self, mut body: Value) -> (u16, Value) {
        if body.get("user").is_none() {
            body["user"] = json!(self.user);
        }
        self.post("/v1/exec", body).await
    }

    async fn ws(&self, path: &str) -> Ws {
        let mut req = format!("ws://{}{}", self.addr, path).into_client_request().unwrap();
        req.headers_mut()
            .insert("Authorization", format!("Bearer {TOKEN}").parse().unwrap());
        tokio_tungstenite::connect_async(req).await.unwrap().0
    }

    /// Open an attach and return the socket and the exec id.
    async fn attach(&self, mut spec: Value) -> (Ws, String) {
        if spec.get("user").is_none() {
            spec["user"] = json!(self.user);
        }
        let mut ws = self.ws("/v1/attach").await;
        ws.send(Message::Text(spec.to_string().into())).await.unwrap();
        let started = next_text(&mut ws).await;
        assert_eq!(started["type"], "started", "{started}");
        (ws, started["exec_id"].as_str().unwrap().to_string())
    }

    async fn wait_finished(&self, id: &str) -> Value {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let (s, v) = self.get(&format!("/v1/exec/{id}")).await;
            assert_eq!(s, 200, "{v}");
            if v["running"] == false {
                return v;
            }
            assert!(Instant::now() < deadline, "exec {id} never finished");
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
}

async fn next_text(ws: &mut Ws) -> Value {
    loop {
        match tokio::time::timeout(Duration::from_secs(10), ws.next()).await.expect("timed out") {
            Some(Ok(Message::Text(t))) => return serde_json::from_str(t.as_str()).unwrap(),
            Some(Ok(Message::Ping(_) | Message::Pong(_))) => continue,
            other => panic!("expected a text frame, got {other:?}"),
        }
    }
}

/// Everything until the exit frame: (stdout bytes, stderr bytes, exit code).
async fn collect_until_exit(ws: &mut Ws) -> (Vec<u8>, Vec<u8>, i64) {
    let (mut out, mut err) = (Vec::new(), Vec::new());
    loop {
        match tokio::time::timeout(Duration::from_secs(10), ws.next()).await.expect("timed out") {
            Some(Ok(Message::Binary(b))) => match b[0] {
                1 => out.extend_from_slice(&b[1..]),
                2 => err.extend_from_slice(&b[1..]),
                c => panic!("unknown channel {c}"),
            },
            Some(Ok(Message::Text(t))) => {
                let v: Value = serde_json::from_str(t.as_str()).unwrap();
                assert_eq!(v["type"], "exit", "{v}");
                return (out, err, v["code"].as_i64().unwrap());
            }
            other => panic!("unexpected {other:?}"),
        }
    }
}

/// Read stdout frames until `needle` has appeared.
async fn read_until(ws: &mut Ws, acc: &mut Vec<u8>, needle: &str) {
    while !String::from_utf8_lossy(acc).contains(needle) {
        match tokio::time::timeout(Duration::from_secs(10), ws.next()).await.expect("timed out") {
            Some(Ok(Message::Binary(b))) => {
                assert_eq!(b[0], 1, "tty output must all be channel 1");
                acc.extend_from_slice(&b[1..]);
            }
            other => panic!("unexpected {other:?} waiting for {needle:?}"),
        }
    }
}

/// (combined, stdout, stderr, [(stream, text)]) — what the client rebuilds.
fn decode(v: &Value) -> (String, String, String, Vec<(String, String)>) {
    let b64 = base64::engine::general_purpose::STANDARD;
    let (mut all, mut out, mut err, mut list) = (String::new(), String::new(), String::new(), vec![]);
    for c in v["chunks"].as_array().unwrap() {
        let s = c["s"].as_str().unwrap().to_string();
        let d = String::from_utf8(b64.decode(c["d"].as_str().unwrap()).unwrap()).unwrap();
        all.push_str(&d);
        if s == "out" { out.push_str(&d) } else { err.push_str(&d) }
        list.push((s, d));
    }
    (all, out, err, list)
}

fn sh(script: &str) -> Value {
    json!(["sh", "-c", script])
}

fn process_gone(pid: i32) -> bool {
    match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        Err(_) => true,
        // A zombie is dead; it is only waiting for whoever reaps orphans here.
        Ok(s) => s.rsplit(')').next().unwrap_or("").trim_start().starts_with('Z'),
    }
}

// ── auth / health / errors ─────────────────────────────────────────────────

#[tokio::test]
async fn missing_or_wrong_token_is_401_with_an_empty_body() {
    let a = start().await;
    for auth in [None, Some("Bearer nope"), Some(&*format!("Basic {TOKEN}")), Some(&*format!("Bearer {TOKEN}x"))] {
        let mut req = a.http.get(a.url("/v1/health"));
        if let Some(h) = auth {
            req = req.header("Authorization", h);
        }
        let r = req.send().await.unwrap();
        assert_eq!(r.status().as_u16(), 401, "{auth:?}");
        assert!(r.bytes().await.unwrap().is_empty());
    }
    // Unknown endpoints are behind the token too.
    let r = a.http.get(a.url("/v1/nope")).send().await.unwrap();
    assert_eq!(r.status().as_u16(), 401);
    // And so is a WebSocket upgrade.
    let req = format!("ws://{}/v1/attach", a.addr).into_client_request().unwrap();
    assert!(tokio_tungstenite::connect_async(req).await.is_err());
}

#[tokio::test]
async fn health_reports_agent_version_and_protocol() {
    let a = start().await;
    let (s, v) = a.get("/v1/health").await;
    assert_eq!(s, 200);
    assert_eq!(v["agent"], "triple-c-agent");
    assert_eq!(v["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(v["protocol"], 1);
}

#[tokio::test]
async fn bad_requests_are_400_with_a_json_error() {
    let a = start().await;
    let r = a
        .http
        .post(a.url("/v1/exec"))
        .bearer_auth(TOKEN)
        .header("content-type", "application/json")
        .body("{not json")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status().as_u16(), 400);
    assert_eq!(r.headers()["content-type"], "application/json");
    assert!(r.json::<Value>().await.unwrap()["error"].is_string());

    let (s, v) = a.exec(json!({ "cmd": [] })).await;
    assert_eq!(s, 400);
    assert!(v["error"].as_str().unwrap().contains("non-empty"));

    // Valid JSON without a Content-Type header is still accepted: the spec
    // says the body is JSON and says nothing about the header.
    let r = a
        .http
        .post(a.url("/v1/exec"))
        .bearer_auth(TOKEN)
        .body(json!({ "cmd": ["true"], "user": a.user }).to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(r.status().as_u16(), 200);
}

// ── one-shot ───────────────────────────────────────────────────────────────

#[tokio::test]
async fn oneshot_chunks_keep_arrival_order_across_streams() {
    let a = start().await;
    let (s, v) = a
        .exec(json!({ "cmd": sh("printf a; sleep 0.2; printf b >&2; sleep 0.2; printf c; sleep 0.2; printf d >&2") }))
        .await;
    assert_eq!(s, 200, "{v}");
    let (all, out, err, list) = decode(&v);
    assert_eq!(all, "abcd");
    assert_eq!(out, "ac");
    assert_eq!(err, "bd");
    let streams: Vec<_> = list.iter().map(|(s, _)| s.as_str()).collect();
    assert_eq!(streams, ["out", "err", "out", "err"]);
    assert_eq!(v["exit_code"], 0);
}

#[tokio::test]
async fn oneshot_exit_codes_match_docker() {
    let a = start().await;
    let (_, v) = a.exec(json!({ "cmd": sh("exit 3") })).await;
    assert_eq!(v["exit_code"], 3);
    // Killed by a signal: 128 + n, as Docker and a shell report it.
    let (_, v) = a.exec(json!({ "cmd": sh("kill -KILL $$") })).await;
    assert_eq!(v["exit_code"], 137);
    let (_, v) = a.exec(json!({ "cmd": sh("kill -TERM $$") })).await;
    assert_eq!(v["exit_code"], 143);
}

#[tokio::test]
async fn a_command_that_cannot_start_is_a_finished_exec_not_an_api_error() {
    let a = start().await;
    let (s, v) = a.exec(json!({ "cmd": ["definitely-not-a-binary-xyz"] })).await;
    assert_eq!(s, 200);
    assert_eq!(v["exit_code"], 127);
    let (_, _, err, list) = decode(&v);
    assert_eq!(list.len(), 1);
    assert!(err.contains("executable file not found in $PATH"), "{err}");

    // Present but not executable: 126.
    let f = a.tmp.path().join("not-exec");
    std::fs::write(&f, "#!/bin/sh\n").unwrap();
    std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o644)).unwrap();
    let (_, v) = a.exec(json!({ "cmd": [f] })).await;
    assert_eq!(v["exit_code"], 126);

    // An explicit path that does not exist: 127.
    let (_, v) = a.exec(json!({ "cmd": [a.tmp.path().join("missing")] })).await;
    assert_eq!(v["exit_code"], 127);

    // No such user: 126 with runc's text.
    let (s, v) = a.exec(json!({ "cmd": ["true"], "user": "no-such-user-triple-c" })).await;
    assert_eq!(s, 200);
    assert_eq!(v["exit_code"], 126);
    assert!(decode(&v).2.contains("unable to find user no-such-user-triple-c"));
}

#[tokio::test]
async fn a_non_root_agent_refuses_other_users_with_403() {
    if is_root() {
        eprintln!("skipped: running as root");
        return;
    }
    let a = start().await;
    let (s, v) = a.exec(json!({ "cmd": ["true"], "user": "root" })).await;
    assert_eq!(s, 403, "{v}");
    assert!(v["error"].is_string());
    // The agent's own user, by name and by uid, works.
    let (s, _) = a.exec(json!({ "cmd": ["true"] })).await;
    assert_eq!(s, 200);
    let uid = unsafe { libc::geteuid() }.to_string();
    let (s, v) = a.exec(json!({ "cmd": ["id", "-u"], "user": uid })).await;
    assert_eq!(s, 200);
    assert_eq!(decode(&v).1.trim(), uid);
    // Omitting the user means the agent's own.
    let (s, v) = a.post("/v1/exec", json!({ "cmd": ["id", "-un"] })).await;
    assert_eq!(s, 200);
    assert_eq!(decode(&v).1.trim(), a.user);
}

#[tokio::test]
async fn env_merges_over_the_base_and_sets_only_home_from_passwd() {
    let a = start().await;
    let script = r#"printf '%s|%s|%s|%s|%s|%s|%s' "$FOO" "$BAR" "$BASE_ONLY" "$HOME" "${USER-unset}" "${LOGNAME-unset}" "${GONE-unset}""#;
    let (_, v) = a
        .exec(json!({ "cmd": sh(script), "env": ["FOO=spec", "BAR=1", "BAR=2", "GONE"] }))
        .await;
    let home = passwd_home(&a.user);
    // Like `docker exec`: HOME from passwd, and no USER/LOGNAME invented.
    assert_eq!(decode(&v).1, format!("spec|2|base|{home}|unset|unset|unset"));

    // The spec's own HOME/USER/LOGNAME win over passwd.
    let (_, v) = a
        .exec(json!({ "cmd": sh(r#"printf '%s|%s|%s' "$HOME" "$USER" "$LOGNAME""#), "env": ["HOME=/custom", "USER=u", "LOGNAME=l"] }))
        .await;
    assert_eq!(decode(&v).1, "/custom|u|l");
}

#[tokio::test]
async fn cmd0_resolves_through_the_merged_path() {
    let a = start().await;
    let bin = a.tmp.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    let tool = bin.join("mytool");
    std::fs::write(&tool, "#!/bin/sh\necho \"mytool ran as $0\"\n").unwrap();
    std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o755)).unwrap();

    let (_, v) = a.exec(json!({ "cmd": ["mytool"] })).await;
    assert_eq!(v["exit_code"], 127, "not on the base PATH");
    let path = format!("PATH={}:/usr/bin:/bin", bin.display());
    let (_, v) = a.exec(json!({ "cmd": ["mytool"], "env": [path] })).await;
    assert_eq!(v["exit_code"], 0, "{v}");
    assert!(decode(&v).1.starts_with("mytool ran as"));
}

#[tokio::test]
async fn working_dir_null_is_the_agent_cwd_and_a_set_one_is_used() {
    let a = start().await;
    let (_, v) = a.exec(json!({ "cmd": ["pwd"], "working_dir": null })).await;
    assert_eq!(Path::new(decode(&v).1.trim()), a.cwd.path().canonicalize().unwrap());
    let (_, v) = a.exec(json!({ "cmd": ["pwd"], "working_dir": a.tmp.path() })).await;
    assert_eq!(Path::new(decode(&v).1.trim()), a.tmp.path().canonicalize().unwrap());
    let (s, v) = a.exec(json!({ "cmd": ["pwd"], "working_dir": "/no/such/dir" })).await;
    assert_eq!(s, 200);
    assert_eq!(v["exit_code"], 126);
}

#[tokio::test]
async fn output_over_the_limit_is_413_and_kills_the_process_group() {
    let a = start().await;
    let pidfile = a.tmp.path().join("bg.pid");
    // A backgrounded sleeper in the same process group, then a flood.
    let script = format!("sleep 30 & echo $! > {}; exec head -c 100000 /dev/zero", pidfile.display());
    let (s, v) = a.exec(json!({ "cmd": sh(&script), "limit": 1000 })).await;
    assert_eq!(s, 413, "{v}");
    assert_eq!(
        v["error"],
        "OUTPUT_LIMIT: Command output exceeded 1000 bytes and was abandoned"
    );
    assert!(v["error"].as_str().unwrap().starts_with("OUTPUT_LIMIT:"));
    let pid: i32 = std::fs::read_to_string(&pidfile).unwrap().trim().parse().unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !process_gone(pid) {
        assert!(Instant::now() < deadline, "background sleeper {pid} survived the limit kill");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // Exactly at the limit is still a success, as with `push_capped`.
    let (s, v) = a.exec(json!({ "cmd": sh("printf 12345"), "limit": 5 })).await;
    assert_eq!(s, 200);
    assert_eq!(decode(&v).0, "12345");
    let (s, _) = a.exec(json!({ "cmd": sh("printf 123456"), "limit": 5 })).await;
    assert_eq!(s, 413);
}

#[tokio::test]
async fn oneshot_stdin_is_dev_null() {
    let a = start().await;
    let (_, v) = a.exec(json!({ "cmd": ["cat"] })).await;
    assert_eq!(v["exit_code"], 0);
    assert_eq!(decode(&v).0, "");
}

// ── detached / status / resize ─────────────────────────────────────────────

#[tokio::test]
async fn detached_exec_can_be_polled_to_its_exit_code() {
    let a = start().await;
    let marker = a.tmp.path().join("ran");
    let script = format!("sleep 0.3; touch {}; exit 7", marker.display());
    let (s, v) = a.post("/v1/exec/detached", json!({ "cmd": sh(&script), "user": a.user })).await;
    assert_eq!(s, 200);
    let id = v["exec_id"].as_str().unwrap().to_string();
    let (_, v) = a.get(&format!("/v1/exec/{id}")).await;
    assert_eq!(v, json!({ "running": true, "exit_code": null }));
    let v = a.wait_finished(&id).await;
    assert_eq!(v, json!({ "running": false, "exit_code": 7 }));
    assert!(marker.exists());

    // Not a tty: resize is a conflict.
    let (s, _) = a.post(&format!("/v1/exec/{id}/resize"), json!({ "cols": 80, "rows": 24 })).await;
    assert_eq!(s, 409);

    // A missing binary still yields an id that reports 127.
    let (_, v) = a.post("/v1/exec/detached", json!({ "cmd": ["no-such-bin-xyz"], "user": a.user })).await;
    let v = a.wait_finished(v["exec_id"].as_str().unwrap()).await;
    assert_eq!(v["exit_code"], 127);
}

#[tokio::test]
async fn unknown_exec_ids_are_404() {
    let a = start().await;
    let (s, _) = a.get("/v1/exec/0000").await;
    assert_eq!(s, 404);
    let (s, _) = a.post("/v1/exec/0000/resize", json!({ "cols": 1, "rows": 1 })).await;
    assert_eq!(s, 404);
}

// ── attach ─────────────────────────────────────────────────────────────────

#[tokio::test]
async fn attach_pipes_stdin_and_close_stdin_gives_eof_and_an_exit_frame() {
    let a = start().await;
    let (mut ws, id) = a.attach(json!({ "cmd": ["cat"], "attach_stdin": true })).await;
    ws.send(Message::Binary(b"hello\n".to_vec().into())).await.unwrap();
    let mut acc = Vec::new();
    match ws.next().await.unwrap().unwrap() {
        Message::Binary(b) => {
            assert_eq!(b[0], 1);
            acc.extend_from_slice(&b[1..]);
        }
        m => panic!("{m:?}"),
    }
    assert_eq!(acc, b"hello\n");
    ws.send(Message::Text(json!({ "type": "close_stdin" }).to_string().into())).await.unwrap();
    let (out, err, code) = collect_until_exit(&mut ws).await;
    assert!(out.is_empty() && err.is_empty());
    assert_eq!(code, 0);
    assert_eq!(a.wait_finished(&id).await, json!({ "running": false, "exit_code": 0 }));
}

#[tokio::test]
async fn attach_without_tty_keeps_stderr_on_channel_2() {
    let a = start().await;
    let (mut ws, id) = a
        .attach(json!({ "cmd": sh("echo out; echo oops >&2; exit 4"), "attach_stdin": false }))
        .await;
    let (out, err, code) = collect_until_exit(&mut ws).await;
    assert_eq!(out, b"out\n");
    assert_eq!(err, b"oops\n");
    assert_eq!(code, 4);
    assert_eq!(a.wait_finished(&id).await["exit_code"], 4);
}

#[tokio::test]
async fn attach_without_stdin_gives_the_process_dev_null() {
    let a = start().await;
    let (mut ws, _) = a.attach(json!({ "cmd": ["cat"], "attach_stdin": false })).await;
    // Ignored: stdin is /dev/null, so cat exits on its own.
    let _ = ws.send(Message::Binary(b"ignored".to_vec().into())).await;
    let (out, _, code) = collect_until_exit(&mut ws).await;
    assert!(out.is_empty());
    assert_eq!(code, 0);
}

#[tokio::test]
async fn attach_tty_puts_everything_on_channel_1_and_resize_reaches_stty() {
    let a = start().await;
    let (mut ws, id) = a
        .attach(json!({
            "cmd": sh("stty size; echo to-stderr >&2; read x; stty size; echo \"term=$TERM\""),
            "tty": true, "attach_stdin": true, "cols": 80, "rows": 24,
        }))
        .await;
    let mut acc = Vec::new();
    read_until(&mut ws, &mut acc, "24 80").await;
    read_until(&mut ws, &mut acc, "to-stderr").await;
    let (s, _) = a.post(&format!("/v1/exec/{id}/resize"), json!({ "cols": 100, "rows": 40 })).await;
    assert_eq!(s, 204);
    ws.send(Message::Binary(b"\n".to_vec().into())).await.unwrap();
    let (out, err, code) = collect_until_exit(&mut ws).await;
    acc.extend_from_slice(&out);
    let text = String::from_utf8_lossy(&acc);
    assert!(err.is_empty(), "a tty has no channel 2");
    assert!(text.contains("40 100"), "{text}");
    // Docker's default TERM for a tty exec when the env has none.
    assert!(text.contains("term=xterm"), "{text}");
    assert_eq!(code, 0);
    // Finished: no longer resizable.
    let (s, _) = a.post(&format!("/v1/exec/{id}/resize"), json!({ "cols": 1, "rows": 1 })).await;
    assert_eq!(s, 409);
}

#[tokio::test]
async fn attach_reports_a_missing_binary_as_started_stderr_and_127() {
    let a = start().await;
    let (mut ws, id) = a.attach(json!({ "cmd": ["no-such-bin-xyz"], "attach_stdin": false })).await;
    let (out, err, code) = collect_until_exit(&mut ws).await;
    assert!(out.is_empty());
    assert!(String::from_utf8_lossy(&err).contains("executable file not found"));
    assert_eq!(code, 127);
    assert_eq!(a.wait_finished(&id).await["exit_code"], 127);
}

#[tokio::test]
async fn attach_refusals_are_an_error_frame() {
    let a = start().await;
    let mut ws = a.ws("/v1/attach").await;
    ws.send(Message::Text(json!({ "cmd": [] }).to_string().into())).await.unwrap();
    let v = next_text(&mut ws).await;
    assert_eq!(v["type"], "error");
    if !is_root() {
        let mut ws = a.ws("/v1/attach").await;
        ws.send(Message::Text(json!({ "cmd": ["true"], "user": "root" }).to_string().into())).await.unwrap();
        assert_eq!(next_text(&mut ws).await["type"], "error");
    }
}

#[tokio::test]
async fn a_dropped_non_tty_attach_closes_stdin_but_does_not_kill() {
    let a = start().await;
    let out = a.tmp.path().join("got");
    // cat sees EOF when the client goes away, then the shell carries on.
    let script = format!("cat > {o}; sleep 0.3; echo after >> {o}", o = out.display());
    let (mut ws, id) = a.attach(json!({ "cmd": sh(&script), "attach_stdin": true })).await;
    ws.send(Message::Binary(b"abc".to_vec().into())).await.unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    drop(ws);
    let v = a.wait_finished(&id).await;
    assert_eq!(v["exit_code"], 0);
    assert_eq!(std::fs::read_to_string(&out).unwrap(), "abcafter\n");
}

#[tokio::test]
async fn a_dropped_tty_attach_hangs_up_the_process_group() {
    let a = start().await;
    let (mut ws, id) = a
        .attach(json!({ "cmd": sh("echo ready; sleep 30"), "tty": true, "attach_stdin": true }))
        .await;
    let mut acc = Vec::new();
    read_until(&mut ws, &mut acc, "ready").await;
    drop(ws);
    let v = a.wait_finished(&id).await;
    // SIGHUP: 128 + 1.
    assert_eq!(v["exit_code"], 129);
}

// ── files ──────────────────────────────────────────────────────────────────

async fn put(a: &Agent, query: &[(&str, String)], body: Vec<u8>) -> (u16, Value) {
    let r = a
        .http
        .put(a.url("/v1/files"))
        .query(query)
        .bearer_auth(TOKEN)
        .body(body)
        .send()
        .await
        .unwrap();
    let s = r.status().as_u16();
    (s, r.json().await.unwrap_or(Value::Null))
}

#[tokio::test]
async fn put_file_writes_content_and_mode_and_replaces_by_unlinking() {
    let a = start().await;
    let dir = a.tmp.path().to_str().unwrap().to_string();
    let target = a.tmp.path().join("f.txt");
    // An existing read-only file with a second hard link: replacement must
    // unlink and create, leaving the other link's content alone.
    std::fs::write(&target, "old").unwrap();
    std::fs::hard_link(&target, a.tmp.path().join("other-link")).unwrap();
    std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o400)).unwrap();

    let (s, v) = put(
        &a,
        &[("dir", dir.clone()), ("name", "f.txt".into()), ("mode", "640".into())],
        b"new content".to_vec(),
    )
    .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["path"], format!("{dir}/f.txt"));
    assert_eq!(std::fs::read(&target).unwrap(), b"new content");
    assert_eq!(std::fs::metadata(&target).unwrap().permissions().mode() & 0o7777, 0o640);
    assert_eq!(std::fs::read(a.tmp.path().join("other-link")).unwrap(), b"old");

    // mode is exact, not filtered through the umask.
    let (s, _) = put(&a, &[("dir", dir.clone()), ("name", "x".into()), ("mode", "777".into())], vec![]).await;
    assert_eq!(s, 200);
    assert_eq!(std::fs::metadata(a.tmp.path().join("x")).unwrap().permissions().mode() & 0o7777, 0o777);

    // Own uid/gid are accepted when not root.
    let (uid, gid) = unsafe { (libc::geteuid(), libc::getegid()) };
    let (s, _) = put(
        &a,
        &[("dir", dir.clone()), ("name", "own".into()), ("uid", uid.to_string()), ("gid", gid.to_string())],
        b"x".to_vec(),
    )
    .await;
    assert_eq!(s, 200);
}

#[tokio::test]
async fn put_file_refusals() {
    let a = start().await;
    let dir = a.tmp.path().to_str().unwrap().to_string();
    for name in ["a/b", "..", ".", ""] {
        let (s, _) = put(&a, &[("dir", dir.clone()), ("name", name.into())], b"x".to_vec()).await;
        assert_eq!(s, 400, "name {name:?}");
    }
    let (s, _) = put(&a, &[("dir", "relative".into()), ("name", "n".into())], vec![]).await;
    assert_eq!(s, 400);
    // The directory is not created.
    let missing = a.tmp.path().join("missing");
    let (s, _) = put(&a, &[("dir", missing.to_str().unwrap().into()), ("name", "n".into())], vec![]).await;
    assert_eq!(s, 404);
    assert!(!missing.exists());
    // Over the ceiling (1 KiB in tests, MAX_DROP_BYTES in production): 413
    // and nothing left behind.
    let (s, _) = put(&a, &[("dir", dir.clone()), ("name", "big".into())], vec![0u8; 1025]).await;
    assert_eq!(s, 413);
    assert!(!a.tmp.path().join("big").exists());
    let (s, _) = put(&a, &[("dir", dir.clone()), ("name", "fits".into())], vec![0u8; 1024]).await;
    assert_eq!(s, 200);
    if !is_root() {
        let (s, _) = put(&a, &[("dir", dir.clone()), ("name", "o".into()), ("uid", "0".into())], vec![]).await;
        assert_eq!(s, 403);
        assert!(!a.tmp.path().join("o").exists());
    }
}

// ── archive ────────────────────────────────────────────────────────────────

async fn archive(a: &Agent, path: &Path) -> (u16, Vec<u8>) {
    let r = a
        .http
        .get(a.url("/v1/archive"))
        .query(&[("path", path.to_str().unwrap())])
        .bearer_auth(TOKEN)
        .send()
        .await
        .unwrap();
    let s = r.status().as_u16();
    if s == 200 {
        assert_eq!(r.headers()["content-type"], "application/x-tar");
    }
    (s, r.bytes().await.unwrap().to_vec())
}

fn entries(tar_bytes: &[u8]) -> Vec<(PathBuf, tar::EntryType, Vec<u8>)> {
    let mut ar = tar::Archive::new(tar_bytes);
    ar.entries()
        .unwrap()
        .map(|e| {
            let mut e = e.unwrap();
            let p = e.path().unwrap().into_owned();
            let t = e.header().entry_type();
            let mut d = Vec::new();
            std::io::Read::read_to_end(&mut e, &mut d).unwrap();
            (p, t, d)
        })
        .collect()
}

#[tokio::test]
async fn archive_of_a_file_has_its_basename_as_the_only_member() {
    let a = start().await;
    let f = a.tmp.path().join("hello.txt");
    std::fs::write(&f, "hi there").unwrap();
    std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o640)).unwrap();
    let (s, body) = archive(&a, &f).await;
    assert_eq!(s, 200);
    let mut ar = tar::Archive::new(&body[..]);
    let mut list = ar.entries().unwrap();
    let mut e = list.next().unwrap().unwrap();
    assert_eq!(e.path().unwrap(), Path::new("hello.txt"));
    assert_eq!(e.header().mode().unwrap() & 0o7777, 0o640);
    let mut d = String::new();
    std::io::Read::read_to_string(&mut e, &mut d).unwrap();
    assert_eq!(d, "hi there");
    drop(e);
    assert!(list.next().is_none());
}

#[tokio::test]
async fn archive_of_a_directory_recurses_under_its_basename_and_keeps_links() {
    let a = start().await;
    let d = a.tmp.path().join("tree");
    std::fs::create_dir_all(d.join("sub")).unwrap();
    std::fs::write(d.join("a.txt"), "A").unwrap();
    std::fs::write(d.join("sub/b.txt"), "B").unwrap();
    std::os::unix::fs::symlink("/etc/passwd", d.join("link")).unwrap();
    // A trailing slash names the same directory.
    let (s, body) = archive(&a, &PathBuf::from(format!("{}/", d.display()))).await;
    assert_eq!(s, 200);
    let list = entries(&body);
    let names: Vec<_> = list.iter().map(|(p, _, _)| p.to_str().unwrap().trim_end_matches('/').to_string()).collect();
    for want in ["tree", "tree/a.txt", "tree/sub", "tree/sub/b.txt", "tree/link"] {
        assert!(names.contains(&want.to_string()), "{want} missing from {names:?}");
    }
    assert_eq!(list[0].0.to_str().unwrap().trim_end_matches('/'), "tree", "root member first");
    let (_, t, _) = list.iter().find(|(p, _, _)| p.ends_with("link")).unwrap();
    assert!(t.is_symlink(), "symlinks are archived, not followed");
    let (_, _, data) = list.iter().find(|(p, _, _)| p.ends_with("sub/b.txt")).unwrap();
    assert_eq!(data, b"B");
}

#[tokio::test]
async fn archive_of_a_missing_path_is_404() {
    let a = start().await;
    let (s, _) = archive(&a, &a.tmp.path().join("nope")).await;
    assert_eq!(s, 404);
}

// ── tunnel ─────────────────────────────────────────────────────────────────

#[tokio::test]
async fn tunnel_pipes_bytes_both_ways_and_closes_with_the_tcp_side() {
    let a = start().await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        // Echo one message with a prefix, then hang up.
        let (mut s, _) = listener.accept().await.unwrap();
        let mut buf = [0u8; 64];
        let n = s.read(&mut buf).await.unwrap();
        s.write_all(b"echo:").await.unwrap();
        s.write_all(&buf[..n]).await.unwrap();
    });
    let mut ws = a.ws(&format!("/v1/tunnel?port={port}")).await;
    ws.send(Message::Binary(b"ping".to_vec().into())).await.unwrap();
    let mut got = Vec::new();
    loop {
        match tokio::time::timeout(Duration::from_secs(5), ws.next()).await.unwrap() {
            Some(Ok(Message::Binary(b))) => got.extend_from_slice(&b),
            Some(Ok(Message::Close(_))) | None => break,
            other => panic!("{other:?}"),
        }
    }
    assert_eq!(got, b"echo:ping");
}

#[tokio::test]
async fn tunnel_to_a_closed_port_sends_an_error_frame() {
    let a = start().await;
    // Bind and drop to get a port that is very likely closed.
    let port = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let mut ws = a.ws(&format!("/v1/tunnel?port={port}")).await;
    let v = next_text(&mut ws).await;
    assert_eq!(v["type"], "error");
    assert!(v["error"].as_str().unwrap().contains(&port.to_string()));
    let (s, _) = a.get("/v1/tunnel?port=nope").await;
    assert_eq!(s, 400);
}

// ── activity ───────────────────────────────────────────────────────────────

#[tokio::test]
async fn activity_counts_execs_and_tunnels_and_ignores_its_own_probes() {
    let a = start().await;
    let (_, v0) = a.get("/v1/activity").await;
    assert_eq!(v0["active_execs"], 0);
    assert_eq!(v0["active_tunnels"], 0);
    let t0 = v0["last_activity_unix"].as_i64().unwrap();

    tokio::time::sleep(Duration::from_millis(1100)).await;
    a.get("/v1/health").await;
    a.get("/v1/activity").await;
    let (_, v) = a.get("/v1/activity").await;
    assert_eq!(v["last_activity_unix"].as_i64().unwrap(), t0, "health/activity are not activity");

    let (_, d) = a.post("/v1/exec/detached", json!({ "cmd": ["sleep", "1"], "user": a.user })).await;
    let (_, v) = a.get("/v1/activity").await;
    assert!(v["last_activity_unix"].as_i64().unwrap() > t0);
    assert_eq!(v["active_execs"], 1);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        let (mut s, _) = listener.accept().await.unwrap();
        let mut b = [0u8; 8];
        let _ = s.read(&mut b).await;
    });
    let ws = a.ws(&format!("/v1/tunnel?port={port}")).await;
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let (_, v) = a.get("/v1/activity").await;
        if v["active_tunnels"] == 1 {
            break;
        }
        assert!(Instant::now() < deadline, "tunnel never counted");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    drop(ws);
    a.wait_finished(d["exec_id"].as_str().unwrap()).await;
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let (_, v) = a.get("/v1/activity").await;
        if v["active_tunnels"] == 0 && v["active_execs"] == 0 {
            break;
        }
        assert!(Instant::now() < deadline, "counters never returned to zero: {v}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

// ── connection limits ──────────────────────────────────────────────────────

/// Wait for the server to close `s`: `Ok(elapsed)` on EOF or reset,
/// `Err` if it is still open after `within`.
async fn closed_within(s: &mut tokio::net::TcpStream, within: Duration) -> Result<Duration, ()> {
    let t0 = Instant::now();
    let mut buf = [0u8; 1024];
    loop {
        match tokio::time::timeout(within.saturating_sub(t0.elapsed()), s.read(&mut buf)).await {
            Err(_) => return Err(()),
            Ok(Ok(0)) | Ok(Err(_)) => return Ok(t0.elapsed()),
            // hyper may answer a timed-out head with a 408 before closing.
            Ok(Ok(_)) => continue,
        }
    }
}

#[tokio::test]
async fn a_slow_request_head_is_cut_off_by_the_header_timeout() {
    let a = start_with(|c| c.header_read_timeout = Duration::from_secs(1)).await;

    // A partial request line, never finished: slowloris.
    let mut partial = tokio::net::TcpStream::connect(a.addr).await.unwrap();
    partial.write_all(b"GET /v1/heal").await.unwrap();
    // A connection that sends nothing at all.
    let mut silent = tokio::net::TcpStream::connect(a.addr).await.unwrap();
    // A head that trickles in a byte at a time, each well inside the timeout.
    let mut trickle = tokio::net::TcpStream::connect(a.addr).await.unwrap();
    let trickler = tokio::spawn(async move {
        for b in b"GET /v1/health HTTP/1.1\r\nHost: x\r\nX-Pad: aaaaaaaaaaaaaaaaaaaaaaaa".iter() {
            if trickle.write_all(&[*b]).await.is_err() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        trickle
    });

    let e = closed_within(&mut partial, Duration::from_secs(15)).await.expect("partial head never closed");
    assert!(e < Duration::from_secs(5), "closed only after {e:?}");
    closed_within(&mut silent, Duration::from_secs(15)).await.expect("silent connection never closed");
    let mut trickle = trickler.await.unwrap();
    closed_within(&mut trickle, Duration::from_secs(15)).await.expect("trickling head never closed");

    // The server is still serving, and a normal request is untouched.
    let (s, v) = a.get("/v1/health").await;
    assert_eq!(s, 200, "{v}");
}

#[tokio::test]
async fn connections_past_the_cap_wait_for_a_slot_rather_than_failing() {
    let a = start_with(|c| {
        c.max_connections = 2;
        c.header_read_timeout = Duration::from_secs(1);
    })
    .await;
    // Fill both slots with connections that never send anything.
    let _hog1 = tokio::net::TcpStream::connect(a.addr).await.unwrap();
    let _hog2 = tokio::net::TcpStream::connect(a.addr).await.unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    // A real request queues until the header timeout frees a slot.
    let t0 = Instant::now();
    let (s, _) = a.get("/v1/health").await;
    assert_eq!(s, 200);
    assert!(t0.elapsed() >= Duration::from_millis(500), "was not held back: {:?}", t0.elapsed());
}

// ── root only ──────────────────────────────────────────────────────────────

#[tokio::test]
#[ignore = "needs root: runs execs as nobody to check stdio is chowned to the exec user"]
async fn as_root_stdio_belongs_to_the_exec_user() {
    assert!(is_root(), "run with sudo -E cargo test -- --ignored");
    let a = start().await;

    // One-shot: stdout/stderr are pipes; reopening them by path needs the
    // user to own them, as runc arranges.
    let (s, v) = a
        .post(
            "/v1/exec",
            json!({ "cmd": sh("echo x > /dev/stdout && echo y > /dev/stderr && echo ok"), "user": "nobody" }),
        )
        .await;
    assert_eq!(s, 200, "{v}");
    let (_, out, err, _) = decode(&v);
    assert_eq!(v["exit_code"], 0, "stdout={out:?} stderr={err:?}");
    assert_eq!(out, "x\nok\n");
    assert_eq!(err, "y\n");

    // Non-tty attach: the same, over pipes the attach owns.
    let (mut ws, _) = a
        .attach(json!({ "cmd": sh("echo x > /dev/stdout && echo y > /dev/stderr"), "user": "nobody" }))
        .await;
    let (out, err, code) = collect_until_exit(&mut ws).await;
    assert_eq!((code, out.as_slice(), err.as_slice()), (0, &b"x\n"[..], &b"y\n"[..]));

    // Tty attach: the slave is the user's, not `root tty`.
    let (mut ws, _) = a
        .attach(json!({ "cmd": sh("stat -c %U $(tty)"), "user": "nobody", "tty": true }))
        .await;
    let (out, _, code) = collect_until_exit(&mut ws).await;
    assert_eq!(code, 0);
    assert_eq!(String::from_utf8_lossy(&out).trim(), "nobody");

    // /dev/null is never handed over.
    let (s, v) = a.post("/v1/exec/detached", json!({ "cmd": ["true"], "user": "nobody" })).await;
    assert_eq!(s, 200, "{v}");
    a.wait_finished(v["exec_id"].as_str().unwrap()).await;
    use std::os::unix::fs::MetadataExt;
    assert_eq!(std::fs::metadata("/dev/null").unwrap().uid(), 0);
}

#[tokio::test]
#[ignore = "needs root: switches to another user (nobody) and chowns an upload"]
async fn as_root_execs_switch_user_and_uploads_chown() {
    assert!(is_root(), "run with sudo -E cargo test -- --ignored");
    let a = start().await;
    let (s, v) = a
        .post("/v1/exec", json!({ "cmd": sh("id -u; id -g; echo $HOME; echo $USER"), "user": "nobody" }))
        .await;
    assert_eq!(s, 200, "{v}");
    let out = decode(&v).1;
    let lines: Vec<_> = out.lines().collect();
    assert_eq!(lines[0], "65534");
    // runc sets only HOME for the exec's user; USER stays unset unless the
    // container env has it (the base env here does not) — see `merge_env`.
    assert_eq!(lines[3], "");
    assert_eq!(lines[2], passwd_home("nobody"));

    let dir = a.tmp.path().to_str().unwrap().to_string();
    let (s, _) = put(
        &a,
        &[("dir", dir), ("name", "owned".into()), ("uid", "65534".into()), ("gid", "65534".into()), ("mode", "4755".into())],
        b"x".to_vec(),
    )
    .await;
    assert_eq!(s, 200);
    let m = std::fs::metadata(a.tmp.path().join("owned")).unwrap();
    use std::os::unix::fs::MetadataExt;
    assert_eq!((m.uid(), m.gid()), (65534, 65534));
    // Mode applied after chown, so the setuid bit survives.
    assert_eq!(m.mode() & 0o7777, 0o4755);
}
