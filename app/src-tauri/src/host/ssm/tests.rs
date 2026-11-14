//! The plugin handling and the supervisor, against a fake plugin (a shell
//! script) instead of AWS. The real thing is in `live_tests.rs`.

use super::*;

#[test]
fn the_ssm_target_names_cluster_task_and_container_runtime() {
    let t = EcsTask {
        cluster: "triple-c".into(),
        task_id: "9fb3a6cf".into(),
        runtime_id: "9fb3a6cf-0791508406".into(),
    };
    assert_eq!(t.ssm_target(), "ecs:triple-c_9fb3a6cf_9fb3a6cf-0791508406");
}

#[test]
fn arns_are_reduced_to_their_names() {
    assert_eq!(
        last_arn_segment("arn:aws:ecs:us-west-2:123456789012:cluster/triple-c", "cluster/"),
        "triple-c"
    );
    assert_eq!(
        last_arn_segment("arn:aws:ecs:us-west-2:123456789012:task/triple-c/9fb3a6cf", "/"),
        "9fb3a6cf"
    );
    assert_eq!(last_arn_segment("triple-c", "cluster/"), "triple-c");
    assert_eq!(last_arn_segment("9fb3a6cf", "/"), "9fb3a6cf");
}

#[test]
fn the_plugin_endpoint_follows_the_partition() {
    assert_eq!(ssm_endpoint("us-west-2"), "https://ssm.us-west-2.amazonaws.com");
    assert_eq!(ssm_endpoint("cn-north-1"), "https://ssm.cn-north-1.amazonaws.com.cn");
}

#[test]
fn a_port_clash_is_recognised_on_every_platform() {
    assert!(is_port_in_use("listen tcp 127.0.0.1:4: bind: address already in use"));
    assert!(is_port_in_use(
        "bind: Only one usage of each socket address (protocol/network address/port) is normally permitted."
    ));
    assert!(!is_port_in_use("TargetNotConnected"));
}

#[test]
fn backoff_grows_then_holds() {
    assert!(reconnect_delay(1) < reconnect_delay(2));
    assert_eq!(reconnect_delay(5), reconnect_delay(500));
}

#[cfg(unix)]
mod with_a_fake_plugin {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// A fake `session-manager-plugin`: a shell script.
    fn script(dir: &Path, name: &str, body: &str) -> PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        p
    }

    /// Runs the script as `/bin/sh <script>`: the script is only ever read,
    /// never exec'd, so a parallel test's fork holding it open for writing
    /// for a moment cannot fail the spawn with ETXTBSY ("Text file busy").
    fn launch(script: PathBuf, secret: &str) -> PluginLaunch {
        PluginLaunch {
            program: "/bin/sh".into(),
            args: vec![
                script.display().to_string(),
                RESPONSE_ENV.into(),
                "us-west-2".into(),
                "StartSession".into(),
            ],
            env: vec![(RESPONSE_ENV.into(), Zeroizing::new(secret.into()))],
            session_id: "s-1".into(),
        }
    }

    const READY: &str = "echo 'Port 1 opened for sessionId s.'; echo 'Waiting for connections...'";

    #[tokio::test]
    async fn the_session_token_travels_in_the_environment_never_on_the_command_line() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("seen");
        // Nothing AWS-shaped from our own environment may reach the plugin.
        // (A name no other test reads, so setting it here is not a race.)
        std::env::set_var("AWS_TRIPLEC_SSM_TEST_LEAK", "leaked");
        let p = script(
            dir.path(),
            "plugin",
            &format!(
                "echo \"argv=$*\" > {o}; echo \"env=${r}\" >> {o}; echo \"leak=$AWS_TRIPLEC_SSM_TEST_LEAK\" >> {o}; {READY}; exec sleep 30",
                o = out.display(),
                r = RESPONSE_ENV
            ),
        );
        let running = spawn_plugin(launch(p, "the-secret-token")).await.expect("ready");
        let seen = std::fs::read_to_string(&out).unwrap();
        std::env::remove_var("AWS_TRIPLEC_SSM_TEST_LEAK");
        assert!(seen.contains("env=the-secret-token"), "{seen}");
        let argv = seen.lines().find(|l| l.starts_with("argv=")).unwrap();
        assert!(!argv.contains("the-secret-token"), "{argv}");
        assert!(argv.contains(RESPONSE_ENV), "the plugin is told which variable to read: {argv}");
        assert!(seen.contains("leak=\n"), "{seen}");
        drop(running); // kill_on_drop
    }

    #[tokio::test]
    async fn a_plugin_that_exits_early_says_why() {
        let dir = tempfile::tempdir().unwrap();
        let p = script(
            dir.path(),
            "plugin",
            "echo 'An error occurred (TargetNotConnected) when calling the StartSession operation' >&2; exit 255",
        );
        let err = spawn_plugin(launch(p, "x")).await.err().expect("must fail");
        assert!(err.contains("TargetNotConnected"), "{err}");
    }

    #[tokio::test]
    async fn a_plugin_that_never_gets_ready_is_killed_and_reported() {
        let dir = tempfile::tempdir().unwrap();
        let p = script(dir.path(), "plugin", "echo 'Starting session'; exec sleep 30");
        let err = spawn_plugin(launch(p, "x")).await.err().expect("must time out");
        assert!(err.contains("did not come up"), "{err}");
        assert!(err.contains("Starting session"), "{err}");
    }

    /// Opens sessions by running a fake plugin; records what it was asked.
    struct FakeOpener {
        plugin: PathBuf,
        ports: Arc<Mutex<Vec<u16>>>,
        terminated: Arc<Mutex<Vec<String>>>,
        opened: Arc<AtomicU32>,
        /// Fail opens whose 1-based number is in here, with this message.
        fail: Vec<(u32, &'static str)>,
    }

    impl FakeOpener {
        fn new(plugin: PathBuf) -> Self {
            Self {
                plugin,
                ports: Default::default(),
                terminated: Default::default(),
                opened: Default::default(),
                fail: vec![],
            }
        }
    }

    impl SessionOpener for FakeOpener {
        async fn open(&self, local_port: u16) -> Result<Running, String> {
            let n = self.opened.fetch_add(1, Ordering::SeqCst) + 1;
            self.ports.lock().unwrap().push(local_port);
            if let Some((_, msg)) = self.fail.iter().find(|(i, _)| *i == n) {
                return Err(msg.to_string());
            }
            let mut l = launch(self.plugin.clone(), "x");
            l.session_id = format!("s-{n}");
            spawn_plugin(l).await
        }

        async fn terminate(&self, session_id: String) {
            self.terminated.lock().unwrap().push(session_id);
        }
    }

    async fn wait_for(state: &mut watch::Receiver<TunnelState>, want: impl Fn(&TunnelState) -> bool) -> TunnelState {
        let s = tokio::time::timeout(Duration::from_secs(10), state.wait_for(|s| want(s)))
            .await
            .expect("state did not arrive")
            .expect("supervisor gone")
            .clone();
        s
    }

    fn alive(pid: u32) -> bool {
        std::process::Command::new("kill")
            .args(["-0", &pid.to_string()])
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    fn kill(pid: u32) {
        std::process::Command::new("kill").args(["-9", &pid.to_string()]).status().unwrap();
    }

    #[tokio::test]
    async fn a_dropped_session_is_reopened_on_the_same_local_port() {
        let dir = tempfile::tempdir().unwrap();
        let p = script(dir.path(), "plugin", &format!("{READY}; exec sleep 30"));
        let opener = FakeOpener::new(p);
        let (ports, terminated) = (opener.ports.clone(), opener.terminated.clone());

        let pf = PortForward::open_with(opener).await.expect("open");
        let mut state = pf.state();
        assert_eq!(*state.borrow(), TunnelState::Up);
        let first = pf.plugin_pid().expect("a plugin runs");

        kill(first);
        wait_for(&mut state, |s| matches!(s, TunnelState::Reconnecting { .. })).await;
        wait_for(&mut state, |s| *s == TunnelState::Up).await;
        let second = pf.plugin_pid().expect("a new plugin runs");
        assert_ne!(first, second);

        let ports = ports.lock().unwrap().clone();
        assert_eq!(ports.len(), 2, "{ports:?}");
        assert_eq!(ports[0], ports[1], "the endpoint must stay valid: same port");
        assert_eq!(pf.local_port(), ports[0]);
        assert_eq!(*terminated.lock().unwrap(), vec!["s-1".to_string()], "the dead session is ended");

        pf.close().await;
        assert!(!alive(second));
    }

    #[tokio::test]
    async fn close_kills_the_plugin_and_ends_the_session() {
        let dir = tempfile::tempdir().unwrap();
        let p = script(dir.path(), "plugin", &format!("{READY}; exec sleep 30"));
        let opener = FakeOpener::new(p);
        let terminated = opener.terminated.clone();
        let pf = PortForward::open_with(opener).await.expect("open");
        let pid = pf.plugin_pid().unwrap();
        let mut state = pf.state();
        pf.close().await;
        assert_eq!(*state.borrow_and_update(), TunnelState::Closed);
        assert!(!alive(pid), "the plugin outlived close()");
        assert_eq!(*terminated.lock().unwrap(), vec!["s-1".to_string()]);
    }

    #[tokio::test]
    async fn dropping_the_handle_also_ends_the_session() {
        let dir = tempfile::tempdir().unwrap();
        let p = script(dir.path(), "plugin", &format!("{READY}; exec sleep 30"));
        let opener = FakeOpener::new(p);
        let terminated = opener.terminated.clone();
        let pf = PortForward::open_with(opener).await.expect("open");
        let pid = pf.plugin_pid().unwrap();
        let mut state = pf.state();
        drop(pf);
        wait_for(&mut state, |s| *s == TunnelState::Closed).await;
        assert!(!alive(pid));
        assert_eq!(*terminated.lock().unwrap(), vec!["s-1".to_string()]);
    }

    #[tokio::test]
    async fn the_first_open_moves_off_a_port_someone_else_took() {
        let dir = tempfile::tempdir().unwrap();
        let p = script(dir.path(), "plugin", &format!("{READY}; exec sleep 30"));
        let mut opener = FakeOpener::new(p);
        opener.fail = vec![(1, "listen tcp 127.0.0.1:1: bind: address already in use")];
        let opened = opener.opened.clone();
        let pf = PortForward::open_with(opener).await.expect("second port works");
        assert_eq!(opened.load(Ordering::SeqCst), 2);
        pf.close().await;
    }

    #[tokio::test]
    async fn other_first_open_errors_are_returned_not_retried() {
        let dir = tempfile::tempdir().unwrap();
        let p = script(dir.path(), "plugin", &format!("{READY}; exec sleep 30"));
        let mut opener = FakeOpener::new(p);
        opener.fail = vec![(1, "AccessDeniedException: not authorized to perform ssm:StartSession")];
        let opened = opener.opened.clone();
        let err = PortForward::open_with(opener).await.err().expect("must fail");
        assert!(err.contains("AccessDenied"), "{err}");
        assert_eq!(opened.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn reconnecting_gives_up_after_the_limit_and_says_why() {
        let dir = tempfile::tempdir().unwrap();
        let p = script(dir.path(), "plugin", &format!("{READY}; exec sleep 30"));
        let mut opener = FakeOpener::new(p);
        // Open 1 works; every reconnect after it fails.
        opener.fail = (2..=MAX_RECONNECT_ATTEMPTS + 1)
            .map(|i| (i, "TargetNotConnected: the task stopped"))
            .collect();
        let pf = PortForward::open_with(opener).await.expect("open");
        let mut state = pf.state();
        kill(pf.plugin_pid().unwrap());
        let s = tokio::time::timeout(
            Duration::from_secs(30),
            state.wait_for(|s| matches!(s, TunnelState::Failed(_))),
        )
        .await
        .expect("gave up")
        .unwrap()
        .clone();
        assert_eq!(s, TunnelState::Failed("TargetNotConnected: the task stopped".into()));
        assert_eq!(pf.plugin_pid(), None);
        pf.close().await;
        assert_eq!(*state.borrow_and_update(), TunnelState::Closed);
    }
}

/// A cancelled open (its future dropped after `StartSession` succeeded) must
/// still end the session: the guard's drop sends `TerminateSession`. Checked
/// against a fake SSM endpoint that records what it is asked.
#[tokio::test]
async fn a_dropped_open_still_ends_its_session() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let (seen_tx, mut seen_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    tokio::spawn(async move {
        loop {
            let (mut sock, _) = listener.accept().await.unwrap();
            let tx = seen_tx.clone();
            tokio::spawn(async move {
                let mut buf = vec![0u8; 64 * 1024];
                let mut req = Vec::new();
                // Headers and the small JSON body arrive together here.
                while let Ok(n) = sock.read(&mut buf).await {
                    if n == 0 {
                        break;
                    }
                    req.extend_from_slice(&buf[..n]);
                    let text = String::from_utf8_lossy(&req);
                    if text.contains("SessionId") {
                        break;
                    }
                }
                let _ = tx.send(String::from_utf8_lossy(&req).into_owned());
                let body = r#"{"SessionId":"s-dropped"}"#;
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/x-amz-json-1.1\r\nContent-Length: {}\r\n\r\n{body}",
                    body.len()
                );
                let _ = sock.write_all(resp.as_bytes()).await;
            });
        }
    });

    let conf = aws_sdk_ssm::Config::builder()
        .behavior_version(aws_sdk_ssm::config::BehaviorVersion::latest())
        .region(aws_sdk_ssm::config::Region::new("us-west-2"))
        .credentials_provider(aws_sdk_ssm::config::Credentials::new("AKIDTEST", "secret", None, None, "test"))
        .endpoint_url(format!("http://127.0.0.1:{port}"))
        .build();
    let guard = SessionGuard {
        ssm: aws_sdk_ssm::Client::from_conf(conf),
        session_id: Some("s-dropped".into()),
    };
    drop(guard);
    let req = tokio::time::timeout(Duration::from_secs(10), seen_rx.recv())
        .await
        .expect("no TerminateSession arrived")
        .unwrap();
    assert!(req.contains("AmazonSSM.TerminateSession"), "{req}");
    assert!(req.contains("s-dropped"), "{req}");
}
