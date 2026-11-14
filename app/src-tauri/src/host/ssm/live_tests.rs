//! The real thing: an SSM port forward to `triple-c-agent` in a running ECS
//! task, used through `ContainerTarget::Agent` exactly as the app will.
//!
//! Ignored; needs AWS credentials (the default chain — profile, SSO or env),
//! `session-manager-plugin`, and a task started with ECS Exec from an image
//! that has the agent:
//!
//! - `TRIPLEC_SSM_E2E_CLUSTER`, `TRIPLEC_SSM_E2E_TASK` (name/id or ARN)
//! - `TRIPLEC_SSM_E2E_TOKEN` — the agent token the task was given
//! - `TRIPLEC_SSM_E2E_CONTAINER` — default `workspace`
//! - `TRIPLEC_SSM_E2E_HOLD_MINUTES` — only for the hold test
//!
//! The SSM session's IAM gate is what these exercise against AWS; the agent's
//! Docker parity is already pinned by `host::agent::live_tests`.

use super::*;
use crate::host::{ContainerTarget, ExecChunk};
use futures_util::StreamExt;
use std::time::Instant;
use tokio::io::AsyncWriteExt;

fn var(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("{name} is not set (see this file's header)"))
}

async fn connect() -> (PortForward, ContainerTarget, Duration) {
    let sdk = aws_config::load_defaults(aws_config::BehaviorVersion::latest()).await;
    let ecs = aws_sdk_ecs::Client::new(&sdk);
    let container = std::env::var("TRIPLEC_SSM_E2E_CONTAINER").unwrap_or_else(|_| "workspace".into());
    let task = EcsTask::resolve(&ecs, &var("TRIPLEC_SSM_E2E_CLUSTER"), &var("TRIPLEC_SSM_E2E_TASK"), &container)
        .await
        .expect("resolve the task");
    let t0 = Instant::now();
    let pf = PortForward::open(&sdk, &task, AGENT_PORT).await.expect("open the port forward");
    let opened = t0.elapsed();
    let target = ContainerTarget::Agent(pf.endpoint(var("TRIPLEC_SSM_E2E_TOKEN")));
    (pf, target, opened)
}

async fn wait_up(pf: &PortForward, within: Duration) -> Duration {
    let t0 = Instant::now();
    let mut s = pf.state();
    tokio::time::timeout(within, s.wait_for(|s| *s == TunnelState::Up))
        .await
        .expect("the tunnel did not come back")
        .unwrap();
    t0.elapsed()
}

async fn whoami(t: &ContainerTarget) -> Result<String, String> {
    t.exec_oneshot(vec!["whoami".into()]).await.map(|s| s.trim().to_string())
}

#[tokio::test]
#[ignore]
async fn the_agent_works_through_an_ssm_port_forward_and_survives_a_drop() {
    let (pf, t, opened) = connect().await;
    eprintln!("port forward up in {opened:?} on 127.0.0.1:{}", pf.local_port());

    // ── the operations the app uses ──
    assert!(t.is_running().await.unwrap());
    assert_eq!(whoami(&t).await.unwrap(), "claude");

    let n = 20;
    let t0 = Instant::now();
    for _ in 0..n {
        t.exec_oneshot(vec!["true".into()]).await.unwrap();
    }
    eprintln!("one-shot exec round trip: {:?} average over {n}", t0.elapsed() / n);

    // attach with stdin
    let mut exec = t
        .attach(crate::host::ExecSpec::new(vec!["head".into(), "-n1".into()]))
        .await
        .unwrap();
    exec.input.write_all(b"through ssm\n").await.unwrap();
    exec.input.shutdown().await.unwrap();
    let mut out = Vec::new();
    while let Some(Ok(c)) = exec.output.next().await {
        if let ExecChunk::StdOut(b) = c {
            out.extend_from_slice(&b);
        }
    }
    assert_eq!(String::from_utf8_lossy(&out), "through ssm\n");

    // 4 MiB up and back
    let data: Vec<u8> = (0..4 * 1024 * 1024u32).map(|i| (i % 251) as u8).collect();
    let t0 = Instant::now();
    t.upload_bytes_with_ids("/tmp", "ssm-e2e.bin", &data, 0o644, (1000, 1000)).await.unwrap();
    let up = t0.elapsed();
    let t0 = Instant::now();
    let mut tar = Vec::new();
    let mut s = t.download_archive("/tmp/ssm-e2e.bin");
    while let Some(chunk) = s.next().await {
        tar.extend_from_slice(&chunk.unwrap());
    }
    let down = t0.elapsed();
    let mut a = tar::Archive::new(&tar[..]);
    let mut e = a.entries().unwrap().next().unwrap().unwrap();
    let mut back = Vec::new();
    std::io::Read::read_to_end(&mut e, &mut back).unwrap();
    assert!(back == data, "the file came back different");
    eprintln!("4 MiB: upload {up:?}, download {down:?}");
    t.exec_oneshot(vec!["rm".into(), "-f".into(), "/tmp/ssm-e2e.bin".into()]).await.unwrap();

    // ── a dropped session: kill the plugin, as a crash or sleep would ──
    let pid = pf.plugin_pid().unwrap();
    std::process::Command::new("kill").args(["-9", &pid.to_string()]).status().unwrap();
    let mut s = pf.state();
    s.wait_for(|s| *s != TunnelState::Up).await.unwrap();
    let back_in = wait_up(&pf, Duration::from_secs(60)).await;
    eprintln!("reconnected in {back_in:?} (same port {})", pf.local_port());
    assert_eq!(whoami(&t).await.unwrap(), "claude", "the same endpoint works after a reconnect");

    // ── close ends it ──
    let port = pf.local_port();
    pf.close().await;
    assert!(
        std::net::TcpStream::connect(("127.0.0.1", port)).is_err(),
        "something still listens on the closed tunnel's port"
    );
}

/// Hold one tunnel for `TRIPLEC_SSM_E2E_HOLD_MINUTES` (default 60): a probe
/// every minute for the first and last stretch, and a silent stretch in the
/// middle longer than Session Manager's default 20-minute idle timeout. One
/// attached exec stays open the whole time. Prints a timeline of tunnel
/// states; fails only if the tunnel cannot serve a request at the end.
#[tokio::test]
#[ignore]
async fn an_ssm_port_forward_holds_for_a_long_session() {
    let minutes: u64 = std::env::var("TRIPLEC_SSM_E2E_HOLD_MINUTES")
        .ok()
        .and_then(|m| m.parse().ok())
        .unwrap_or(60);
    let (pf, t, opened) = connect().await;
    let start = Instant::now();
    let stamp = move || format!("[{:>5.1} min]", start.elapsed().as_secs_f64() / 60.0);
    eprintln!("{} up in {opened:?}", stamp());

    // Log every state change as it happens.
    let mut states = pf.state();
    let log_states = tokio::spawn(async move {
        while states.changed().await.is_ok() {
            eprintln!("{} tunnel: {:?}", stamp(), *states.borrow());
        }
    });

    // A long-lived connection: does one TCP stream survive the whole hold?
    let mut held = t.attach(crate::host::ExecSpec::new(vec!["cat".into()])).await.unwrap();

    let idle_from = minutes / 4;
    let idle_to = idle_from + 25.min(minutes / 2);
    let mut failures = 0;
    for m in 0..minutes {
        if m < idle_from || m >= idle_to {
            let t0 = Instant::now();
            match whoami(&t).await {
                Ok(_) => eprintln!("{} probe ok ({:?})", stamp(), t0.elapsed()),
                Err(e) => {
                    failures += 1;
                    eprintln!("{} probe FAILED: {e}", stamp())
                }
            }
        } else if m == idle_from {
            eprintln!("{} silent until minute {idle_to}", stamp());
        }
        tokio::time::sleep(Duration::from_secs(60)).await;
    }

    // Did the held stream survive? Echo through it.
    let survived = async {
        held.input.write_all(b"still here\n").await.ok()?;
        let mut got = Vec::new();
        while let Ok(Some(Ok(c))) = tokio::time::timeout(Duration::from_secs(10), held.output.next()).await {
            if let ExecChunk::StdOut(b) = c {
                got.extend_from_slice(&b);
                if got.ends_with(b"\n") {
                    break;
                }
            }
        }
        Some(got == b"still here\n")
    }
    .await
    .unwrap_or(false);
    eprintln!("{} long-lived attach survived: {survived}; probe failures: {failures}", stamp());

    assert_eq!(whoami(&t).await.unwrap(), "claude", "the tunnel serves at the end");
    pf.close().await;
    log_states.abort();
}
