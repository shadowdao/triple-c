//! Port mappings against real loopback sockets and a **fake** agent that
//! answers `/v1/attach` by echoing stdin back as stdout — the socat it was
//! asked to run is recorded, never run.

use super::*;
use crate::storage::projects_store::ProjectsStore;
use crate::host::agent::AgentEndpoint;
use crate::host::tunnels;
use axum::extract::ws::{Message as WsMessage, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::response::Response;
use axum::routing::get;
use axum::Router;
use futures_util::SinkExt;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

#[derive(Default)]
struct Fake {
    /// Every request the agent saw, of any kind.
    requests: AtomicUsize,
    /// The command of each attach.
    cmds: std::sync::Mutex<Vec<Vec<String>>>,
}

async fn attach_ws(State(f): State<Arc<Fake>>, ws: WebSocketUpgrade) -> Response {
    f.requests.fetch_add(1, Ordering::SeqCst);
    ws.on_upgrade(move |socket| echo(f, socket))
}

async fn echo(f: Arc<Fake>, mut ws: WebSocket) {
    let Some(Ok(WsMessage::Text(hello))) = ws.recv().await else { return };
    let hello: serde_json::Value = serde_json::from_str(hello.as_str()).unwrap();
    let cmd = hello["cmd"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    f.cmds.lock().unwrap().push(cmd);
    let _ = ws.send(WsMessage::Text(r#"{"type":"started","exec_id":"a1"}"#.into())).await;
    while let Some(Ok(msg)) = ws.recv().await {
        match msg {
            WsMessage::Binary(b) => {
                let mut out = vec![1u8];
                out.extend_from_slice(&b);
                if ws.send(WsMessage::Binary(out.into())).await.is_err() {
                    return;
                }
            }
            WsMessage::Text(t) if t.as_str().contains("close_stdin") => break,
            _ => {}
        }
    }
    let _ = ws.send(WsMessage::Text(r#"{"type":"exit","code":0}"#.into())).await;
    let _ = ws.close().await;
}

async fn counted(State(f): State<Arc<Fake>>) -> &'static str {
    f.requests.fetch_add(1, Ordering::SeqCst);
    "{}"
}

/// A fake agent, registered as `project_id`'s connected tunnel; its target.
async fn connected_agent(project_id: &str) -> (ContainerTarget, Arc<Fake>, Arc<AtomicUsize>) {
    let fake = Arc::new(Fake::default());
    let app = Router::new()
        .route("/v1/attach", get(attach_ws))
        .fallback(counted)
        .with_state(fake.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let closes = Arc::new(AtomicUsize::new(0));
    tunnels::register(project_id, tunnels::tests::fake_tunnel(port, closes.clone()));
    (ContainerTarget::Agent(AgentEndpoint::loopback(port, "tok")), fake, closes)
}

fn pid(tag: &str) -> String {
    format!("port-forwards-test-{tag}-{}", uuid::Uuid::new_v4())
}

/// A host port nothing listens on right now.
fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

fn tcp(host_port: u16, container_port: u16) -> PortMapping {
    PortMapping { host_port, container_port, protocol: "tcp".into() }
}

async fn round_trip(port: u16, msg: &[u8]) -> Vec<u8> {
    let mut c = TcpStream::connect(("127.0.0.1", port)).await.expect("connect");
    c.write_all(msg).await.unwrap();
    let mut buf = vec![0u8; msg.len()];
    tokio::time::timeout(std::time::Duration::from_secs(5), c.read_exact(&mut buf))
        .await
        .expect("echo in time")
        .expect("echo");
    buf
}

fn port_is_free(port: u16) -> bool {
    std::net::TcpListener::bind(("127.0.0.1", port)).is_ok()
}

#[tokio::test]
async fn a_busy_host_port_fails_its_mapping_alone_and_the_other_forwards() {
    let p = pid("busy");
    let (target, fake, _) = connected_agent(&p).await;
    let busy = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let busy_port = busy.local_addr().unwrap().port();
    let free = free_port();

    let results = open_for(&p, &target, &[tcp(busy_port, 3000), tcp(free, 5432)]).await;

    assert_eq!(results.len(), 2);
    let e = results[0].error.as_deref().expect("the busy port fails");
    assert!(e.contains("already in use"), "{e}");
    assert!(results[1].error.is_none(), "{:?}", results[1].error);
    assert_eq!(failure_detail(&results).as_deref(), Some(e));
    assert_eq!(open_ports(&p), vec![free]);

    // Bytes go through the agent's attach, to the *container* port.
    assert_eq!(round_trip(free, b"ping").await, b"ping");
    assert_eq!(
        fake.cmds.lock().unwrap().as_slice(),
        [vec!["socat".to_string(), "-".into(), "TCP:127.0.0.1:5432".into()]]
    );

    // Stop / Remove / task death all close the tunnel: the mappings go too.
    tunnels::close(&p).await;
    assert!(open_ports(&p).is_empty());
    assert!(port_is_free(free));
}

#[tokio::test]
async fn a_server_speaks_first_protocol_is_not_held_back() {
    // Unlike the auth bridge, a mapping forwards a client that has said
    // nothing yet (MySQL, SMTP): the attach opens on connect.
    let p = pid("silent");
    let (target, fake, _) = connected_agent(&p).await;
    let port = free_port();
    assert!(open_for(&p, &target, &[tcp(port, 3306)]).await[0].error.is_none());
    let _client = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(3);
    while fake.cmds.lock().unwrap().is_empty() {
        assert!(tokio::time::Instant::now() < deadline, "no attach for a silent client");
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    tunnels::close(&p).await;
}

#[tokio::test]
async fn an_open_listener_with_no_connection_makes_no_agent_request() {
    // So it can never count as activity and keep a workspace from its idle stop.
    let p = pid("idle");
    let (target, fake, _) = connected_agent(&p).await;
    let port = free_port();
    assert!(open_for(&p, &target, &[tcp(port, 8080)]).await[0].error.is_none());
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    assert_eq!(fake.requests.load(Ordering::SeqCst), 0);
    tunnels::close(&p).await;
}

#[tokio::test]
async fn udp_reserved_and_duplicate_mappings_are_refused_per_mapping() {
    let p = pid("refused");
    let (target, _, _) = connected_agent(&p).await;
    let ok = free_port();
    let udp = PortMapping { host_port: free_port(), container_port: 53, protocol: "udp".into() };
    let results = open_for(
        &p,
        &target,
        &[
            udp,
            tcp(ok, 8080),
            tcp(ok, 9090),
            tcp(free_port(), *RESERVED_CONTAINER_PORTS.start()),
            tcp(free_port(), crate::host::ssm::AGENT_PORT),
            tcp(*RESERVED_HOST_PORTS.start(), 8080),
            tcp(0, 0),
        ],
    )
    .await;
    let errors: Vec<Option<&str>> = results.iter().map(|r| r.error.as_deref()).collect();
    assert_eq!(errors.len(), 6, "an unfinished row is skipped: {errors:?}");
    assert!(errors[0].unwrap().contains("only TCP"), "{errors:?}");
    assert!(errors[1].is_none());
    assert!(errors[2].unwrap().contains("another mapping"), "{errors:?}");
    assert!(errors[3].unwrap().contains("browser view"), "{errors:?}");
    assert!(errors[4].unwrap().contains("agent"), "{errors:?}");
    assert!(errors[5].unwrap().contains("browser view"), "{errors:?}");
    assert_eq!(open_ports(&p), vec![ok]);
    tunnels::close(&p).await;
}

#[tokio::test]
async fn a_reconnect_replaces_the_set_on_the_same_host_ports() {
    let p = pid("replace");
    let (target, _, _) = connected_agent(&p).await;
    let port = free_port();
    assert!(open_for(&p, &target, &[tcp(port, 8080)]).await[0].error.is_none());
    // A background re-attach registers a new tunnel and opens again.
    let (target2, fake2, _) = connected_agent(&p).await;
    let again = open_for(&p, &target2, &[tcp(port, 8080)]).await;
    assert!(again[0].error.is_none(), "{:?}", again[0].error);
    assert_eq!(round_trip(port, b"hi").await, b"hi");
    assert_eq!(fake2.cmds.lock().unwrap().len(), 1, "the new set reaches the new tunnel");
    tunnels::close(&p).await;
    assert!(port_is_free(port));
}

#[tokio::test]
async fn nothing_is_kept_when_the_workspace_is_no_longer_connected() {
    let p = pid("gone");
    let (target, _, _) = connected_agent(&p).await;
    tunnels::close(&p).await; // a Stop raced the connect
    let port = free_port();
    let results = open_for(&p, &target, &[tcp(port, 8080)]).await;
    assert!(results[0].error.as_deref().unwrap().contains("disconnected"));
    assert!(open_ports(&p).is_empty());
    assert!(port_is_free(port));
}

#[tokio::test]
async fn a_task_death_closes_the_mappings_but_a_stale_report_does_not() {
    let p = pid("death");
    let (target, _, _) = connected_agent(&p).await;
    let port = free_port();
    open_for(&p, &target, &[tcp(port, 8080)]).await;
    tunnels::close_if_task(&p, "arn:some-older-task").await;
    assert_eq!(open_ports(&p), vec![port]);
    let ContainerTarget::Agent(ep) = &target else { unreachable!() };
    let (_, _, _, arn) = tunnels::task_of(ep).unwrap();
    tunnels::close_if_task(&p, &arn).await;
    assert!(open_ports(&p).is_empty());
    assert!(port_is_free(port));
}

#[test]
fn an_aws_project_saves_only_tcp_unreserved_unique_mappings() {
    let v = |m: &[PortMapping]| validate_aws_port_mappings(m, false);
    assert!(v(&[tcp(3000, 3000), tcp(8081, 8080), tcp(0, 0), tcp(5000, 0), tcp(4001, 4000)]).is_ok());
    let udp = PortMapping { host_port: 5353, container_port: 5353, protocol: "udp".into() };
    let e = v(&[udp]).unwrap_err();
    assert!(e.contains("only TCP") && e.contains("UDP"), "{e}");
    assert!(v(&[tcp(3000, 3000), tcp(3000, 4000)]).unwrap_err().contains("mapped twice"));
    assert!(v(&[tcp(4000, 39321)]).is_err());
    assert!(v(&[tcp(4000, 47900)]).is_err());
    assert!(v(&[tcp(47820, 4000)]).is_err());
}

#[test]
fn the_gateway_sidecars_port_is_refused_while_the_project_runs_it() {
    // A mapping reaches the workspace's *loopback*, where the sidecar holds
    // the provider key behind 127.0.0.1:4000.
    let m = [tcp(4001, GATEWAY_SIDECAR_PORT)];
    assert!(validate_aws_port_mappings(&m, false).is_ok());
    let e = validate_aws_port_mappings(&m, true).unwrap_err();
    assert!(e.contains("gateway sidecar"), "{e}");
    // Turning the sidecar on re-checks rows that were already stored.
    assert!(validate_aws_port_mappings_update(Some((&m, false)), &m, true).is_err());
    assert!(validate_aws_port_mappings_update(Some((&m, false)), &m, false).is_ok());
}

#[test]
fn a_stored_row_that_would_be_refused_does_not_block_other_saves() {
    let udp = PortMapping { host_port: 5353, container_port: 5353, protocol: "udp".into() };
    let stored = vec![udp.clone(), tcp(3000, 3000)];
    // Unchanged, plus a new valid row: fine.
    let saved = vec![udp.clone(), tcp(3000, 3000), tcp(8080, 8080)];
    assert!(validate_aws_port_mappings_update(Some((&stored, false)), &saved, false).is_ok());
    // A new UDP row, or an edit to one, is refused.
    let edited = PortMapping { host_port: 5354, ..udp.clone() };
    let e = validate_aws_port_mappings_update(Some((&stored, false)), &[edited, tcp(3000, 3000)], false);
    assert!(e.unwrap_err().contains("only TCP"));
    // A new row taking a stored row's host port is refused.
    let dup = vec![udp.clone(), tcp(3000, 3000), tcp(3000, 4000)];
    assert!(validate_aws_port_mappings_update(Some((&stored, false)), &dup, false)
        .unwrap_err()
        .contains("mapped twice"));
    // Newly on AWS: everything is checked.
    assert!(validate_aws_port_mappings_update(None, &stored, false).is_err());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn overlapping_opens_converge_on_the_last_stored_set_without_false_errors() {
    // The editor saves on every blur; each save opens. Each open re-reads
    // the stored mappings under the project's lock, so overlapping opens
    // neither fight over a host port nor leave an older save's set behind.
    let p = pid("concurrent");
    let (target, _, _) = connected_agent(&p).await;
    let (a, b, c) = (free_port(), free_port(), free_port());
    let sets = [
        vec![tcp(a, 8080), tcp(b, 8081)],
        vec![tcp(b, 8081), tcp(c, 8082)],
        vec![tcp(a, 8085), tcp(c, 8082)],
        vec![tcp(b, 8090), tcp(c, 8082)],
    ];
    let stored = Arc::new(std::sync::Mutex::new(sets[0].clone()));
    let go = Arc::new(tokio::sync::Barrier::new(8));
    let mut opens = Vec::new();
    for i in 0..8 {
        let (p2, t2, s2, go2) = (p.clone(), target.clone(), stored.clone(), go.clone());
        let mine = sets[i % sets.len()].clone();
        opens.push(tokio::spawn(async move {
            go2.wait().await;
            open_latest(&p2, &t2, move || {
                let mut s = s2.lock().unwrap();
                *s = mine; // this save lands, then its open reads the store
                Some((s.clone(), false))
            })
            .await
        }));
    }
    for o in opens {
        let results = o.await.unwrap();
        assert!(results.iter().all(|r| r.error.is_none()), "{results:?}");
    }
    let last = stored.lock().unwrap().clone();
    let mut want: Vec<u16> = last.iter().map(|m| m.host_port).collect();
    want.sort_unstable();
    assert_eq!(open_ports(&p), want, "the registry holds the last stored set");
    tunnels::close(&p).await;
}

#[tokio::test]
async fn a_save_keeps_unchanged_forwards_and_their_live_connections() {
    let p = pid("diff");
    let (target, fake, _) = connected_agent(&p).await;
    let (a, b) = (free_port(), free_port());
    open_for(&p, &target, &[tcp(a, 8080)]).await;
    let mut client = TcpStream::connect(("127.0.0.1", a)).await.unwrap();
    client.write_all(b"one").await.unwrap();
    let mut buf = [0u8; 3];
    client.read_exact(&mut buf).await.unwrap();

    let r = open_for(&p, &target, &[tcp(a, 8080), tcp(b, 8081)]).await;
    assert!(r.iter().all(|r| r.error.is_none()), "{r:?}");
    // The same connection still carries bytes: its forward was kept.
    client.write_all(b"two").await.unwrap();
    client.read_exact(&mut buf).await.unwrap();
    assert_eq!(&buf, b"two");
    assert_eq!(fake.cmds.lock().unwrap().len(), 1, "no new attach for the kept connection");

    // A changed container port rebinds the same host port.
    let r = open_for(&p, &target, &[tcp(a, 9090)]).await;
    assert!(r[0].error.is_none(), "{:?}", r[0].error);
    assert_eq!(round_trip(a, b"x").await, b"x");
    assert_eq!(fake.cmds.lock().unwrap().last().unwrap()[2], "TCP:127.0.0.1:9090");
    assert!(port_is_free(b));
    tunnels::close(&p).await;
}

#[tokio::test]
async fn a_stored_project_is_read_when_the_open_runs() {
    let p = pid("store");
    let dir = tempfile::tempdir().unwrap();
    let mut project = crate::models::Project::new("x".into(), vec![]);
    project.id = p.clone();
    project.host_id = Some("h1".into());
    project.aws_placement =
        Some(crate::models::AwsPlacement { region: "us-west-2".into(), environment: "dev".into() });
    project.aws = Some(Default::default());
    let (port, busy) = (free_port(), std::net::TcpListener::bind("127.0.0.1:0").unwrap());
    let busy_port = busy.local_addr().unwrap().port();
    project.port_mappings = vec![tcp(port, 8080), tcp(busy_port, 8081)];
    let store = ProjectsStore::over_file_for_test(dir.path().join("projects.json"), vec![project]);
    assert_eq!(open_for_project(&store, &p).await, None, "not connected: nothing, no warning");
    let (_target, _, _) = connected_agent(&p).await;
    let warning = open_for_project(&store, &p).await.expect("the busy port is reported");
    assert!(warning.contains(&busy_port.to_string()) && warning.contains("already in use"), "{warning}");
    assert_eq!(open_ports(&p), vec![port]);
    tunnels::close(&p).await;
}

#[tokio::test]
async fn the_auth_bridges_forward_still_waits_for_a_client_to_speak() {
    // The generalised `PortForward::bind` keeps the bridge's gate: a silent
    // client costs no attach (contrast `a_server_speaks_first_protocol_…`).
    let p = pid("bridge");
    let (target, fake, _) = connected_agent(&p).await;
    let port = free_port();
    let mut fwd = PortForward::bind(target, port, port, PortFamily::V4).await.unwrap();
    assert_eq!((fwd.port, fwd.container_port), (port, port));
    let _client = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    assert_eq!(fake.requests.load(Ordering::SeqCst), 0);
    fwd.shutdown().await;
    tunnels::close(&p).await;
}
