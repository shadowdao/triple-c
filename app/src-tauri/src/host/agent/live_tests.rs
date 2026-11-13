//! The real agent against `docker exec`, in one real container: every
//! operation goes through `ContainerTarget::Docker` and
//! `ContainerTarget::Agent` on the *same* container and must come back the
//! same. That is the agent protocol's whole requirement ("Docker parity is the
//! requirement"), checked end to end rather than against a fake.
//!
//! Needs a Docker daemon and an image whose entrypoint starts the agent:
//! `TRIPLEC_AGENT_E2E_IMAGE`, default `tc-agent-e2e:latest` (the sandbox image
//! plus this branch's `entrypoint.sh` and `triple-c-agent`).

use super::*;
use crate::host::{ContainerTarget, ExecChunk, ExecSpec, OUTPUT_LIMIT_MARKER};
use futures_util::StreamExt;
use std::process::Command;
use std::time::Duration;
use tokio::io::AsyncWriteExt;

fn docker_cli(args: &[&str]) -> String {
    let out = Command::new("docker").args(args).output().expect("docker cli");
    assert!(out.status.success(), "docker {:?}: {}", args, String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn sh(script: &str) -> Vec<String> {
    vec!["sh".into(), "-c".into(), script.into()]
}

/// Drain an attached exec: (stdout, stderr).
async fn drain(exec: crate::host::AttachedExec) -> (String, String) {
    let mut out = exec.output;
    let (mut o, mut e) = (Vec::new(), Vec::new());
    let collect = async {
        while let Some(chunk) = out.next().await {
            match chunk {
                Ok(ExecChunk::StdOut(b)) => o.extend_from_slice(&b),
                Ok(ExecChunk::StdErr(b)) => e.extend_from_slice(&b),
                Ok(ExecChunk::Other(_)) => {}
                Err(_) => break,
            }
        }
    };
    tokio::time::timeout(Duration::from_secs(20), collect).await.expect("attach output ended");
    (String::from_utf8_lossy(&o).into_owned(), String::from_utf8_lossy(&e).into_owned())
}

/// Fail with both answers side by side.
fn same<T: PartialEq + std::fmt::Debug>(what: &str, docker: T, agent: T) -> Result<(), String> {
    if docker == agent {
        Ok(())
    } else {
        Err(format!("{what} differs\n  docker: {docker:?}\n  agent:  {agent:?}"))
    }
}

async fn parity(d: &ContainerTarget, a: &ContainerTarget) -> Result<(), String> {
    // ── one-shot ─────────────────────────────────────────────────────────────
    let script = sh("echo out1; echo err1 >&2; echo out2; exit 3");
    same(
        "streams + exit code",
        d.exec_oneshot_streams_as("claude", script.clone(), vec![]).await?,
        a.exec_oneshot_streams_as("claude", script, vec![]).await?,
    )?;

    for user in ["claude", "root"] {
        let env = sh("env | sort");
        let strip = |s: String| -> String {
            // `docker exec` inherits the container's configured env, which in
            // this harness carries the agent's settings (the token included —
            // the local-harness trade-off entrypoint.sh documents). The agent
            // never passes its own settings on.
            s.lines().filter(|l| !l.starts_with("TRIPLEC_AGENT_")).collect::<Vec<_>>().join("\n")
        };
        let (de, dc) = d.exec_oneshot_as(user, env.clone(), vec![]).await?;
        let (ae, ac) = a.exec_oneshot_as(user, env, vec![]).await?;
        same(&format!("environment as {user}"), (strip(de), dc), (strip(ae), ac))?;
    }

    same("default working dir", d.exec_oneshot(vec!["pwd".into()]).await?, a.exec_oneshot(vec!["pwd".into()]).await?)?;

    let probe = sh("echo \"$FOO|$HOME|$USER|$(id -un)\"");
    let env = vec!["FOO=bar".to_string(), "HOME=/tmp".to_string()];
    same(
        "env overrides",
        d.exec_oneshot_env_status(probe.clone(), env.clone()).await?,
        a.exec_oneshot_env_status(probe, env).await?,
    )?;

    same("user ids", d.user_ids().await, a.user_ids().await)?;

    let flood = sh("head -c 100000 /dev/zero");
    let de = d.exec_oneshot_limited("claude", flood.clone(), 1000).await.unwrap_err();
    let ae = a.exec_oneshot_limited("claude", flood, 1000).await.unwrap_err();
    if !de.starts_with(OUTPUT_LIMIT_MARKER) {
        return Err(format!("docker limit error: {de}"));
    }
    same("output limit error", de, ae)?;

    let (_, dcode) = d.exec_oneshot_env_status(vec!["no-such-binary-tc".into()], vec![]).await?;
    let (_, acode) = a.exec_oneshot_env_status(vec!["no-such-binary-tc".into()], vec![]).await?;
    same("missing binary exit code", dcode, acode)?;

    // ── files ────────────────────────────────────────────────────────────────
    let ids = d.user_ids().await;
    d.upload_bytes_with_ids("/tmp", "via-docker.txt", b"hello\n", 0o640, ids).await?;
    a.upload_bytes_with_ids("/tmp", "via-agent.txt", b"hello\n", 0o640, ids).await?;
    let stat = |n: &str| sh(&format!("stat -c '%a %u %g %s' /tmp/{n} && cat /tmp/{n}"));
    same(
        "uploaded file (mode, owner, size, content)",
        d.exec_oneshot(stat("via-docker.txt")).await?,
        a.exec_oneshot(stat("via-agent.txt")).await?,
    )?;

    d.exec_oneshot(sh("rm -rf /tmp/arch && mkdir -p /tmp/arch/sub && echo x > /tmp/arch/x && echo y > /tmp/arch/sub/y")).await?;
    let list = |t: &ContainerTarget| {
        let mut s = t.download_archive("/tmp/arch");
        async move {
            let mut bytes = Vec::new();
            while let Some(c) = s.next().await {
                bytes.extend_from_slice(&c?);
            }
            let mut names: Vec<(String, Vec<u8>)> = tar::Archive::new(&bytes[..])
                .entries()
                .map_err(|e| e.to_string())?
                .map(|e| {
                    let mut e = e.unwrap();
                    let name = e.path().unwrap().to_string_lossy().trim_end_matches('/').to_string();
                    let mut data = Vec::new();
                    std::io::Read::read_to_end(&mut e, &mut data).unwrap();
                    (name, data)
                })
                .collect();
            names.sort();
            Ok::<_, String>(names)
        }
    };
    same("archive members and contents", list(d).await?, list(a).await?)?;

    // ── attached ─────────────────────────────────────────────────────────────
    for (label, t) in [("docker", d), ("agent", a)] {
        let mut exec = t.attach_exec(vec!["head".into(), "-n1".into()], false).await?;
        exec.input.write_all(b"ping\n").await.map_err(|e| e.to_string())?;
        exec.input.flush().await.map_err(|e| e.to_string())?;
        let id = exec.exec_id.clone();
        let (o, e) = drain(exec).await;
        same(&format!("{label}: attach echo"), ("ping\n".to_string(), String::new()), (o, e))?;
        same(&format!("{label}: attach exit"), Some(0), t.wait_exec_exit(&id).await)?;
    }

    let mut sizes = Vec::new();
    for t in [d, a] {
        let exec = t
            .attach(ExecSpec::new(sh("sleep 1; stty size")).tty(true))
            .await?;
        t.resize_exec(&exec.exec_id, 100, 40).await?;
        let (o, _) = drain(exec).await;
        sizes.push(o.trim().to_string());
    }
    same("tty size after resize", sizes[0].clone(), sizes[1].clone())?;
    same("tty size value", "40 100".to_string(), sizes[1].clone())?;

    // ── detached ─────────────────────────────────────────────────────────────
    let mut codes = Vec::new();
    for t in [d, a] {
        let id = t.exec_detached(ExecSpec::new(sh("sleep 0.3; exit 5"))).await?;
        codes.push(t.wait_exec_exit(&id).await);
    }
    same("detached exit code", codes[0], codes[1])?;

    same("is_running", d.is_running().await?, a.is_running().await?)?;
    Ok(())
}

#[tokio::test]
#[ignore = "needs a Docker daemon and an image whose entrypoint starts triple-c-agent (TRIPLEC_AGENT_E2E_IMAGE)"]
async fn the_agent_answers_exactly_as_docker_exec_does() {
    let image = std::env::var("TRIPLEC_AGENT_E2E_IMAGE").unwrap_or_else(|_| "tc-agent-e2e:latest".into());
    let token = format!("{}{}", uuid::Uuid::new_v4().simple(), uuid::Uuid::new_v4().simple());
    let name = format!("tc-agent-e2e-{}", uuid::Uuid::new_v4().simple());
    docker_cli(&[
        "run", "-d", "--name", &name, "-w", "/workspace",
        "-e", &format!("TRIPLEC_AGENT_TOKEN={token}"),
        // Docker forwards a published port to the container's own address,
        // not its loopback, so the harness binds the agent wide *inside* the
        // container and publishes it to the host's loopback only.
        "-e", "TRIPLEC_AGENT_ADDR=0.0.0.0:47900",
        "-p", "127.0.0.1::47900",
        &image,
    ]);
    let outcome = async {
        let mapped = docker_cli(&["port", &name, "47900/tcp"]);
        let port: u16 = mapped.lines().next().and_then(|l| l.rsplit(':').next()).and_then(|p| p.parse().ok())
            .ok_or_else(|| format!("no published port in {mapped:?}"))?;
        // Two ways in, because where *this test* runs decides which one routes:
        // on the host (where the app runs) the published loopback port; from
        // inside another container on the same daemon, the published port is
        // on the host's loopback, not ours, but the container's bridge address
        // is reachable.
        let ip = docker_cli(&["inspect", "-f", "{{range .NetworkSettings.Networks}}{{.IPAddress}} {{end}}", &name]);
        let mut candidates = vec![AgentEndpoint::loopback(port, token.clone())];
        for addr in ip.split_whitespace() {
            let url = url::Url::parse(&format!("http://{addr}:47900/")).map_err(|e| e.to_string())?;
            candidates.push(AgentEndpoint::new(url, token.clone())?);
        }
        let docker = ContainerTarget::docker(name.clone());

        // The entrypoint starts the agent last, after setup (including a
        // bounded `claude update`), so health is the readiness signal.
        let mut agent = None;
        'wait: for _ in 0..240 {
            for ep in &candidates {
                let t = ContainerTarget::Agent(ep.clone());
                if t.is_running().await.unwrap_or(false) {
                    agent = Some(t);
                    break 'wait;
                }
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        let Some(agent) = agent else {
            return Err(format!("agent never answered on {candidates:?}; logs:\n{}", docker_cli(&["logs", "--tail", "40", &name])));
        };
        eprintln!("agent reached at {}", agent.id());
        parity(&docker, &agent).await
    }
    .await;
    let logs = docker_cli(&["logs", "--tail", "20", &name]);
    let _ = Command::new("docker").args(["rm", "-f", &name]).output();
    if let Err(e) = outcome {
        panic!("{e}\n--- container log tail ---\n{logs}");
    }
}
