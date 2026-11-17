//! Persistent terminals end to end, through the real agent in a real
//! container: a tab's tmux server survives its client going away (the
//! agent's SIGHUP), is listed, keeps its own environment (and nothing secret
//! on argv), replays its history, reattaches, and ends.
//!
//! Needs a Docker daemon and an image whose entrypoint starts the agent and
//! that has tmux and `/etc/triple-c/tmux.conf`: `TRIPLEC_PERSIST_E2E_IMAGE`,
//! default `tc-tmux-e2e:latest` (e.g. the sandbox `:edge` image plus
//! `apt-get install tmux` and `COPY triple-c-tmux.conf /etc/triple-c/tmux.conf`).
//!
//! `cargo test host::persistent::live_tests -- --ignored`

use super::*;
use crate::host::{AgentEndpoint, ContainerTarget, ExecSpec};
use futures_util::StreamExt;
use std::process::Command;
use std::time::Duration;
use tokio::io::AsyncWriteExt;

fn docker_cli(args: &[&str]) -> String {
    let out = Command::new("docker").args(args).output().expect("docker cli");
    assert!(out.status.success(), "docker {:?}: {}", args, String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// Read an attached exec's output until `needle` shows up (or time runs out).
async fn read_until(out: &mut crate::host::ExecOutputStream, needle: &str, secs: u64) -> String {
    let mut seen = String::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(secs);
    while !seen.contains(needle) {
        match tokio::time::timeout_at(deadline, out.next()).await {
            Ok(Some(Ok(c))) => seen.push_str(&String::from_utf8_lossy(&c.into_bytes())),
            _ => break,
        }
    }
    seen
}

async fn start_agent_container(image: &str) -> (String, ContainerTarget) {
    let token = format!("{}{}", uuid::Uuid::new_v4().simple(), uuid::Uuid::new_v4().simple());
    let name = format!("tc-persist-e2e-{}", uuid::Uuid::new_v4().simple());
    docker_cli(&[
        "run", "-d", "--init", "--name", &name, "-w", "/workspace",
        "-e", &format!("TRIPLEC_AGENT_TOKEN={token}"),
        "-e", "TRIPLEC_AGENT_ADDR=0.0.0.0:47900",
        "-p", "127.0.0.1::47900",
        image,
    ]);
    let mapped = docker_cli(&["port", &name, "47900/tcp"]);
    let port: u16 = mapped.lines().next().and_then(|l| l.rsplit(':').next()).and_then(|p| p.parse().ok()).unwrap();
    let ip = docker_cli(&["inspect", "-f", "{{range .NetworkSettings.Networks}}{{.IPAddress}} {{end}}", &name]);
    let mut candidates = vec![AgentEndpoint::loopback(port, token.clone())];
    for addr in ip.split_whitespace() {
        let url = url::Url::parse(&format!("http://{addr}:47900/")).unwrap();
        candidates.push(AgentEndpoint::new(url, token.clone()).unwrap());
    }
    for _ in 0..240 {
        for ep in &candidates {
            let t = ContainerTarget::Agent(ep.clone());
            if t.is_running().await.unwrap_or(false) {
                return (name, t);
            }
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    let logs = docker_cli(&["logs", "--tail", "40", &name]);
    let _ = Command::new("docker").args(["rm", "-f", &name]).output();
    panic!("agent never answered; logs:\n{logs}");
}

async fn scenario(t: &ContainerTarget) -> Result<(), String> {
    let check = |ok: bool, what: &str| if ok { Ok(()) } else { Err(what.to_string()) };
    check(probe(t).await == Some(true), "the image has tmux and the config")?;

    let k1 = uuid::Uuid::new_v4().to_string();
    let k2 = uuid::Uuid::new_v4().to_string();
    let inner = |marker: &str| {
        vec![
            "bash".to_string(),
            "-c".to_string(),
            format!("printf 'secret=%s\\n' \"$TC_SECRET\"; for i in $(seq 1 300); do echo hist$i; done; echo {marker}; exec bash --norc\n"),
        ]
    };
    let p1 = plan_with(true, true, None, &k1, TerminalKind::Bash, Some("first"), inner("READY1"), vec!["TC_SECRET=alpha-not-real".into()])?;
    let p2 = plan_with(true, true, None, &k2, TerminalKind::Claude, None, inner("READY2"), vec!["TC_SECRET=beta-not-real".into()])?;

    // Open both tabs.
    let mut a1 = t.attach(ExecSpec::new(p1.cmd.clone()).tty(true).env(p1.env.clone())).await?;
    let seen = read_until(&mut a1.output, "READY1", 20).await;
    check(seen.contains("secret=alpha-not-real"), &format!("tab 1 has its own env: {seen:?}"))?;
    let mut a2 = t.attach(ExecSpec::new(p2.cmd.clone()).tty(true).env(p2.env.clone())).await?;
    let seen = read_until(&mut a2.output, "READY2", 20).await;
    check(seen.contains("secret=beta-not-real"), &format!("tab 2 has its own env: {seen:?}"))?;

    // No secret on any command line in the workspace.
    let (ps, _) = t.exec_oneshot_as("claude", vec!["ps".into(), "-eo".into(), "args".into()], vec![]).await?;
    check(!ps.contains("alpha-not-real") && !ps.contains("beta-not-real"), &format!("secret on argv: {ps}"))?;

    // The app goes away: drop the client. The agent SIGHUPs its process group.
    drop(a1);
    tokio::time::sleep(Duration::from_secs(2)).await;
    let listed = list(t).await?;
    let s1 = listed.iter().find(|s| s.key == k1).ok_or(format!("tab 1 survived its client: {listed:?}"))?;
    check(!s1.attached && s1.kind == TerminalKind::Bash && s1.name.as_deref() == Some("first"), &format!("{s1:?}"))?;
    let s2 = listed.iter().find(|s| s.key == k2).ok_or("tab 2 listed")?;
    check(s2.attached && s2.kind == TerminalKind::Claude, &format!("{s2:?}"))?;

    // History for a fresh tab, then reattach (never creating).
    let h = history(t, &k1).await.ok_or("history readable")?;
    check(h.contains("hist1\n") && h.contains("hist250"), &format!("history: {} bytes", h.len()))?;
    let mut r1 = t.attach(ExecSpec::new(attach_cmd(&k1)?).tty(true).env(client_env(vec![]))).await?;
    r1.input.write_all(b"echo back-$((40+2))\r").await.map_err(|e| e.to_string())?;
    let seen = read_until(&mut r1.output, "back-42", 20).await;
    check(seen.contains("back-42"), &format!("reattached and typing works: {seen:?}"))?;
    // Still the original server's environment.
    r1.input.write_all(b"echo env-$TC_SECRET\r").await.map_err(|e| e.to_string())?;
    let seen = read_until(&mut r1.output, "env-alpha-not-real", 20).await;
    check(seen.contains("env-alpha-not-real"), "the server kept its env")?;

    // Attaching to a key that is gone fails and creates nothing.
    let ghost = uuid::Uuid::new_v4().to_string();
    let mut g = t.attach(ExecSpec::new(attach_cmd(&ghost)?).tty(true).env(client_env(vec![]))).await?;
    let _ = read_until(&mut g.output, "\u{0}", 5).await;
    check(!list(t).await?.iter().any(|s| s.key == ghost), "attach-session created nothing")?;

    // A `claude` inside tmux is what the idle watcher sees; tmux is not.
    r1.input.write_all(b"cp /bin/sleep /tmp/claude && /tmp/claude 30 &\r").await.map_err(|e| e.to_string())?;
    tokio::time::sleep(Duration::from_secs(1)).await;
    let (pg, code) = t.exec_oneshot_as("claude", vec!["pgrep".into(), "-x".into(), "claude".into()], vec![]).await?;
    check(code == 0 && !pg.trim().is_empty(), "pgrep -x claude sees it")?;
    let (_, code) = t.exec_oneshot_as("claude", vec!["pgrep".into(), "-x".into(), "tmux".into()], vec![]).await?;
    check(code != 0, "no process is named plain `tmux` (servers are `tmux: server`)")?;

    // Closing a tab ends it; Stop ends the rest.
    end(t, &k1).await?;
    let _ = read_until(&mut r1.output, "\u{0}", 5).await;
    check(!list(t).await?.iter().any(|s| s.key == k1), "tab 1 ended")?;
    end_all(t).await;
    check(list(t).await?.is_empty(), "every terminal ended")?;
    let (pg, _) = t.exec_oneshot_as("claude", vec!["sh".into(), "-c".into(), "pgrep -f '^tmux -L triple-c-' || true".into()], vec![]).await?;
    check(pg.trim().is_empty(), &format!("no tmux left: {pg}"))?;
    drop(a2);
    Ok(())
}

#[tokio::test]
#[ignore = "needs a Docker daemon and an agent image with tmux (TRIPLEC_PERSIST_E2E_IMAGE)"]
async fn a_terminal_outlives_its_client_and_reattaches_through_the_agent() {
    let image = std::env::var("TRIPLEC_PERSIST_E2E_IMAGE").unwrap_or_else(|_| "tc-tmux-e2e:latest".into());
    let (name, target) = start_agent_container(&image).await;
    let outcome = scenario(&target).await;
    let logs = docker_cli(&["logs", "--tail", "20", &name]);
    let _ = Command::new("docker").args(["rm", "-f", &name]).output();
    if let Err(e) = outcome {
        panic!("{e}\n--- container log tail ---\n{logs}");
    }
}
