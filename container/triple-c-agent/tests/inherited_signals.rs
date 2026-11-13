//! Execs must start with default signal dispositions and an empty mask, no
//! matter what the agent itself inherited — the clean state a `docker exec`
//! child gets from runc.
//!
//! In production the entrypoint's supervisor runs the agent under
//! `trap '' HUP`, and an ignored disposition survives `exec`. These tests
//! reproduce that by ignoring SIGHUP (and SIGUSR1) in *this* test process,
//! which is where the in-process server spawns from. A disposition is
//! process-wide, which is why this is its own test binary rather than part of
//! `protocol.rs`.

use std::net::SocketAddr;
use std::sync::Once;
use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message;
use triple_c_agent::{serve, Config};

const TOKEN: &str = "test-token-0123456789abcdef0123456789abcdef";

fn ignore_signals_like_the_supervisor() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| unsafe {
        libc::signal(libc::SIGHUP, libc::SIG_IGN);
        libc::signal(libc::SIGUSR1, libc::SIG_IGN);
    });
}

/// A `/proc/<pid>/status` mask field (`SigIgn`, `SigBlk`, …) as a bitmask.
fn mask(status: &str, field: &str) -> u64 {
    let line = status
        .lines()
        .find(|l| l.starts_with(&format!("{field}:")))
        .unwrap_or_else(|| panic!("no {field} in status"));
    u64::from_str_radix(line.split_whitespace().nth(1).unwrap(), 16).unwrap()
}

fn bit(sig: libc::c_int) -> u64 {
    1 << (sig - 1)
}

struct Agent {
    addr: SocketAddr,
    http: reqwest::Client,
    _cwd: tempfile::TempDir,
    user: String,
}

async fn start() -> Agent {
    ignore_signals_like_the_supervisor();
    // The precondition, checked rather than assumed.
    let own = std::fs::read_to_string("/proc/self/status").unwrap();
    assert_ne!(mask(&own, "SigIgn") & bit(libc::SIGHUP), 0, "the test process must ignore SIGHUP");

    let cwd = tempfile::tempdir().unwrap();
    let config = Config {
        token: TOKEN.to_string(),
        default_cwd: cwd.path().to_path_buf(),
        base_env: vec![("PATH".into(), "/usr/local/bin:/usr/bin:/bin".into())],
        max_file_bytes: 1024,
        header_read_timeout: triple_c_agent::HEADER_READ_TIMEOUT,
        max_connections: triple_c_agent::MAX_CONNECTIONS,
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(serve(listener, config));
    let user = String::from_utf8(std::process::Command::new("id").arg("-un").output().unwrap().stdout)
        .unwrap()
        .trim()
        .to_string();
    Agent { addr, http: reqwest::Client::new(), _cwd: cwd, user }
}

impl Agent {
    async fn get(&self, path: &str) -> Value {
        let r = self.http.get(format!("http://{}{path}", self.addr)).bearer_auth(TOKEN).send().await.unwrap();
        assert_eq!(r.status().as_u16(), 200);
        r.json().await.unwrap()
    }
}

fn process_gone(pid: i32) -> bool {
    match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        Err(_) => true,
        Ok(s) => s.rsplit(')').next().unwrap_or("").trim_start().starts_with('Z'),
    }
}

#[tokio::test]
async fn execs_start_with_no_ignored_or_blocked_signals() {
    let a = start().await;
    let r = a
        .http
        .post(format!("http://{}/v1/exec", a.addr))
        .bearer_auth(TOKEN)
        .json(&json!({ "cmd": ["cat", "/proc/self/status"], "user": a.user }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status().as_u16(), 200);
    let v: Value = r.json().await.unwrap();
    assert_eq!(v["exit_code"], 0, "{v}");
    use base64::Engine;
    let status: String = v["chunks"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|c| c["s"] == "out")
        .map(|c| {
            let b = base64::engine::general_purpose::STANDARD.decode(c["d"].as_str().unwrap()).unwrap();
            String::from_utf8(b).unwrap()
        })
        .collect();
    assert_eq!(mask(&status, "SigIgn"), 0, "inherited ignored signals:\n{status}");
    assert_eq!(mask(&status, "SigBlk"), 0, "inherited blocked signals:\n{status}");
}

#[tokio::test]
async fn a_dropped_tty_attach_still_hangs_up_when_the_agent_ignores_sighup() {
    let a = start().await;
    let mut req = format!("ws://{}/v1/attach", a.addr).into_client_request().unwrap();
    req.headers_mut().insert("Authorization", format!("Bearer {TOKEN}").parse().unwrap());
    let mut ws = tokio_tungstenite::connect_async(req).await.unwrap().0;
    let spec = json!({
        "cmd": ["sh", "-c", "echo pid=$$; exec sleep 600"],
        "user": a.user, "tty": true, "attach_stdin": true,
    });
    ws.send(Message::Text(spec.to_string().into())).await.unwrap();

    let mut out = String::new();
    let pid: i32 = loop {
        match tokio::time::timeout(Duration::from_secs(10), ws.next()).await.expect("timed out") {
            Some(Ok(Message::Text(_))) => {} // "started"
            Some(Ok(Message::Binary(b))) => {
                out.push_str(&String::from_utf8_lossy(&b[1..]));
                if let Some(rest) = out.split("pid=").nth(1) {
                    if let Some(line) = rest.split_once(['\r', '\n']) {
                        break line.0.trim().parse().unwrap();
                    }
                }
            }
            other => panic!("unexpected {other:?}"),
        }
    };
    assert_eq!(a.get("/v1/activity").await["active_execs"], 1);
    // Let `exec sleep` happen, so the process we watch is the sleep.
    tokio::time::sleep(Duration::from_millis(200)).await;
    drop(ws);

    let deadline = Instant::now() + Duration::from_secs(5);
    while !process_gone(pid) {
        assert!(Instant::now() < deadline, "sleep (pid {pid}) survived the client disconnect");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let v = a.get("/v1/activity").await;
        if v["active_execs"] == 0 {
            break;
        }
        assert!(Instant::now() < deadline, "active_execs never returned to 0: {v}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}
