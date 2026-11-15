//! The transport to an agent in an ECS task: one SSM port-forwarding session
//! (`AWS-StartPortForwardingSession`) from a loopback port on this machine to
//! the agent's port inside the task, kept open for as long as the project is
//! connected and reopened when it drops. Everything else — execs, files,
//! tunnels — then goes through [`AgentEndpoint`] exactly as it does locally.
//! Design: `docs/superpowers/specs/2026-11-13-aws-ecs-host-design.md`
//! ("Connectivity").
//!
//! The SSM data channel is a custom websocket protocol the Rust SDK does not
//! implement, so v1 runs AWS's `session-manager-plugin` as a child process,
//! the way the AWS CLI does: the app calls `ssm:StartSession` itself and hands
//! the plugin the stream URL and token.
//!
//! Three things here are deliberate:
//!
//! - **The session token never goes on a command line.** The plugin reads the
//!   `StartSession` response from the environment variable named by its first
//!   argument (`AWS_SSM_START_SESSION_RESPONSE`, what the AWS CLI does for
//!   plugins that support it), so it is not visible in `ps`.
//! - **The plugin gets the caller's current credentials in its environment,
//!   and nothing else of AWS's.** It needs them only to resume its data
//!   channel after a network blip (`ssm:ResumeSession`), which keeps every TCP
//!   connection through the tunnel — open terminals included — alive. Any
//!   `AWS_*` variable this process happens to have is removed first, so the
//!   plugin can never act as a different identity than the one that started
//!   the session. When those credentials have expired a resume fails, the
//!   plugin exits, and the supervisor here opens a fresh session with fresh
//!   credentials on the **same local port**, so an [`AgentEndpoint`] built
//!   once stays valid across reconnects (open connections do not survive that
//!   path; new ones succeed).
//! - **Whoever holds a [`PortForward`] must `close()` it when the app exits.**
//!   Tauri exits through `std::process::exit`, which drops no runtime tasks,
//!   so `kill_on_drop` never fires and the plugin would keep the session and
//!   the port until SSM's idle timeout.
//! - **The plugin listens on `127.0.0.1` only** (checked: plugin 1.2.896), and
//!   the agent behind it still wants its bearer token. The SSM session's IAM
//!   check is the outer gate.

use aws_config::SdkConfig;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::watch;
use tokio_util::sync::{CancellationToken, DropGuard};
use zeroize::Zeroizing;

use super::AgentEndpoint;

/// Where `triple-c-agent` listens inside the task (its default).
pub const AGENT_PORT: u16 = 47900;

const PORT_FORWARD_DOCUMENT: &str = "AWS-StartPortForwardingSession";
/// The environment variable the plugin reads the `StartSession` response from.
const RESPONSE_ENV: &str = "AWS_SSM_START_SESSION_RESPONSE";
/// What the plugin prints once its local listener is up.
const READY_LINE: &str = "Waiting for connections";
/// Plugin start to listener up. Measured at ~1-2 s; this bounds a wedged one.
const READY_TIMEOUT: Duration = if cfg!(test) {
    Duration::from_secs(2)
} else {
    Duration::from_secs(30)
};
/// The last lines of plugin output kept for error messages.
const OUTPUT_TAIL_LINES: usize = 20;
/// Reconnect delays; the last repeats. A reconnect that keeps failing for
/// about ten minutes gives up ([`TunnelState::Failed`]) — by then the task is
/// gone or the credentials need the user, and the host layer decides what the
/// project's state is.
const RECONNECT_BACKOFF: &[Duration] = &[
    Duration::from_secs(1),
    Duration::from_secs(2),
    Duration::from_secs(5),
    Duration::from_secs(10),
    Duration::from_secs(30),
];
const MAX_RECONNECT_ATTEMPTS: u32 = 25;
/// Attempts to find a free local port for the first connection.
const INITIAL_PORT_ATTEMPTS: u32 = 3;
const TERMINATE_TIMEOUT: Duration = Duration::from_secs(5);
/// How often an Up tunnel sends traffic of its own. Session Manager ends a
/// session after 20 minutes with no data (its default `idleSessionTimeout`;
/// measured: a silent tunnel was cut at 19.7 min), and with it every TCP
/// connection inside — an idle terminal tab included.
const KEEPALIVE_INTERVAL: Duration = if cfg!(test) {
    Duration::from_millis(200)
} else {
    Duration::from_secs(5 * 60)
};
const KEEPALIVE_TIMEOUT: Duration = Duration::from_secs(10);

// ── The task ─────────────────────────────────────────────────────────────────

/// An ECS task container that an SSM session can target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EcsTask {
    /// The cluster's *name* (the SSM target wants the name, not the ARN).
    pub cluster: String,
    pub task_id: String,
    /// The container's runtime id, from `DescribeTasks`.
    pub runtime_id: String,
}

impl EcsTask {
    /// `ecs:<cluster>_<taskId>_<runtimeId>`, the SSM target for one container
    /// of one task.
    pub fn ssm_target(&self) -> String {
        format!("ecs:{}_{}_{}", self.cluster, self.task_id, self.runtime_id)
    }

    /// Look up `container` in `task` (an id or ARN) on `cluster` (a name or
    /// ARN), and check it can take a session now: the task is running and
    /// its ECS Exec agent is up. Fargate starts that agent a few seconds after
    /// the container, so "not ready yet" is a normal answer just after start.
    ///
    /// The AWS host builds its target from `aws::tasks::wait_ready`'s answer
    /// instead (`aws_host::ecs_task`); this stays for the live tests.
    #[allow(dead_code)]
    pub async fn resolve(
        ecs: &aws_sdk_ecs::Client,
        cluster: &str,
        task: &str,
        container: &str,
    ) -> Result<Self, String> {
        let cluster_name = last_arn_segment(cluster, "cluster/");
        let out = ecs
            .describe_tasks()
            .cluster(cluster)
            .tasks(task)
            .send()
            .await
            .map_err(|e| format!("Could not look up the ECS task: {}", sdk_error(&e)))?;
        let t = out
            .tasks()
            .first()
            .ok_or_else(|| format!("ECS task {task} was not found in cluster {cluster_name}"))?;
        let task_arn = t.task_arn().unwrap_or(task);
        let task_id = last_arn_segment(task_arn, "/").to_string();
        let status = t.last_status().unwrap_or("UNKNOWN");
        if status != "RUNNING" {
            return Err(format!("ECS task {task_id} is {status}, not RUNNING"));
        }
        let c = t
            .containers()
            .iter()
            .find(|c| c.name() == Some(container))
            .ok_or_else(|| format!("ECS task {task_id} has no container named {container}"))?;
        let runtime_id = c
            .runtime_id()
            .ok_or_else(|| format!("Container {container} in task {task_id} has no runtime id yet"))?
            .to_string();
        let exec_agent = c
            .managed_agents()
            .iter()
            .find(|a| a.name().map(|n| n.as_str()) == Some("ExecuteCommandAgent"));
        match exec_agent.and_then(|a| a.last_status()) {
            Some("RUNNING") => {}
            Some(s) => {
                return Err(format!(
                    "Container {container} in task {task_id} is not ready for connections yet (ECS Exec agent {s})"
                ))
            }
            None => {
                return Err(format!(
                    "ECS task {task_id} was started without ECS Exec, so it cannot be connected to"
                ))
            }
        }
        Ok(Self {
            cluster: cluster_name.to_string(),
            task_id,
            runtime_id,
        })
    }
}

/// The part of an ARN after the last `marker` (`.../cluster/name` → `name`),
/// or the whole string when it is not an ARN.
#[allow(dead_code)] // `EcsTask::resolve`
fn last_arn_segment<'a>(s: &'a str, marker: &str) -> &'a str {
    if !s.starts_with("arn:") {
        return s;
    }
    match s.rfind(marker) {
        Some(i) => &s[i + marker.len()..],
        None => s,
    }
}

/// An SDK error with its cause chain (the bare `Display` is often just
/// "service error").
fn sdk_error(e: &(dyn std::error::Error + 'static)) -> String {
    format!("{}", aws_sdk_ssm::error::DisplayErrorContext(e))
}

// ── The plugin ───────────────────────────────────────────────────────────────

/// Find `session-manager-plugin`: `TRIPLEC_SSM_PLUGIN` if set, then `PATH`,
/// then where AWS's installers put it (the GUI app on macOS does not get the
/// shell's `PATH`, so the install locations are not optional).
pub fn find_plugin() -> Result<PathBuf, String> {
    if let Some(p) = std::env::var_os("TRIPLEC_SSM_PLUGIN").filter(|p| !p.is_empty()) {
        let p = PathBuf::from(p);
        return if p.is_file() {
            Ok(p)
        } else {
            Err(format!("TRIPLEC_SSM_PLUGIN names {}, which is not a file", p.display()))
        };
    }
    let name = if cfg!(windows) { "session-manager-plugin.exe" } else { "session-manager-plugin" };
    let on_path = std::env::var_os("PATH")
        .map(|paths| std::env::split_paths(&paths).map(|d| d.join(name)).collect::<Vec<_>>())
        .unwrap_or_default();
    let installed: Vec<PathBuf> = if cfg!(windows) {
        ["ProgramFiles", "ProgramFiles(x86)"]
            .iter()
            .filter_map(|v| std::env::var_os(v))
            .map(|d| PathBuf::from(d).join(r"Amazon\SessionManagerPlugin\bin").join(name))
            .collect()
    } else {
        [
            "/usr/local/sessionmanagerplugin/bin",
            "/usr/local/bin",
            "/opt/homebrew/bin",
            "/usr/bin",
        ]
        .iter()
        .map(|d| Path::new(d).join(name))
        .collect()
    };
    on_path
        .into_iter()
        .chain(installed)
        .find(|p| p.is_file())
        .ok_or_else(|| {
            "The AWS Session Manager plugin (session-manager-plugin) is not installed. It is \
             what connects to a workspace on AWS; install it from \
             https://docs.aws.amazon.com/systems-manager/latest/userguide/session-manager-working-with-install-plugin.html"
                .to_string()
        })
}

/// A running plugin process with a listener on the local port.
struct Running {
    child: Child,
    session_id: String,
    output: OutputTail,
}

/// The last lines a plugin printed (stdout and stderr), for error text.
#[derive(Clone, Default)]
struct OutputTail(Arc<Mutex<Vec<String>>>);

impl OutputTail {
    fn push(&self, line: String) {
        let mut v = self.0.lock().unwrap();
        v.push(line);
        if v.len() > OUTPUT_TAIL_LINES {
            v.remove(0);
        }
    }

    /// The most informative recent line: the last non-blank one.
    fn last(&self) -> Option<String> {
        self.0.lock().unwrap().iter().rev().find(|l| !l.trim().is_empty()).cloned()
    }

    fn joined(&self) -> String {
        self.0
            .lock()
            .unwrap()
            .iter()
            .filter(|l| !l.trim().is_empty())
            .cloned()
            .collect::<Vec<_>>()
            .join("; ")
    }
}

/// How to start the plugin for one session. Everything but `env_secret` is
/// safe to log; that is the `StartSession` response.
struct PluginLaunch {
    program: PathBuf,
    args: Vec<String>,
    env: Vec<(String, Zeroizing<String>)>,
    session_id: String,
}

/// Start the plugin and wait until it says its listener is up. Its output is
/// then drained in the background for good (a full pipe would stall it).
async fn spawn_plugin(launch: PluginLaunch) -> Result<Running, String> {
    let mut cmd = Command::new(&launch.program);
    cmd.args(&launch.args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    // Only the credentials this session was started with: nothing AWS-shaped
    // from our own environment rides along.
    for (k, _) in std::env::vars_os() {
        if k.to_string_lossy().starts_with("AWS_") {
            cmd.env_remove(&k);
        }
    }
    for (k, v) in &launch.env {
        cmd.env(k, v.as_str());
    }
    #[cfg(windows)]
    {
        // CREATE_NO_WINDOW: no console flashing up per (re)connect.
        cmd.creation_flags(0x0800_0000);
    }
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("Could not start {}: {e}", launch.program.display()))?;

    let output = OutputTail::default();
    let (ready_tx, mut ready_rx) = watch::channel(false);
    let stdout = child.stdout.take().expect("stdout is piped");
    let stderr = child.stderr.take().expect("stderr is piped");
    drain(stdout, output.clone(), Some(ready_tx));
    drain(stderr, output.clone(), None);

    let ready = async {
        loop {
            if *ready_rx.borrow_and_update() {
                return true;
            }
            if ready_rx.changed().await.is_err() {
                // stdout closed without the ready line: the plugin is exiting.
                return *ready_rx.borrow();
            }
        }
    };
    tokio::select! {
        r = tokio::time::timeout(READY_TIMEOUT, ready) => match r {
            Ok(true) => Ok(Running { child, session_id: launch.session_id, output }),
            Ok(false) => {
                let _ = child.wait().await;
                Err(plugin_failed(&output))
            }
            Err(_) => {
                let _ = child.kill().await;
                Err(format!(
                    "The SSM port forward did not come up within {} s ({})",
                    READY_TIMEOUT.as_secs(),
                    output.joined()
                ))
            }
        },
        // Exited before (or without) printing the ready line.
        status = child.wait() => {
            // Let the drains catch the last lines before reporting them.
            tokio::time::sleep(Duration::from_millis(50)).await;
            let _ = status;
            Err(plugin_failed(&output))
        }
    }
}

fn plugin_failed(output: &OutputTail) -> String {
    match output.joined() {
        s if s.is_empty() => "The SSM port forward exited before it was ready".to_string(),
        s => format!("The SSM port forward exited before it was ready: {s}"),
    }
}

fn drain(
    stream: impl AsyncRead + Unpin + Send + 'static,
    tail: OutputTail,
    ready: Option<watch::Sender<bool>>,
) {
    tokio::spawn(async move {
        let mut lines = BufReader::new(stream).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            log::debug!("session-manager-plugin: {line}");
            if let Some(tx) = &ready {
                if line.contains(READY_LINE) {
                    let _ = tx.send(true);
                }
            }
            tail.push(line);
        }
        // `ready` drops here; a waiter sees the channel close.
    });
}

// ── Opening sessions ─────────────────────────────────────────────────────────

/// Opens and ends sessions. The real one calls SSM and runs the plugin; tests
/// substitute a fake plugin without AWS.
trait SessionOpener: Send + Sync + 'static {
    fn open(&self, local_port: u16) -> impl Future<Output = Result<Running, String>> + Send;
    fn terminate(&self, session_id: String) -> impl Future<Output = ()> + Send;
}

struct SsmOpener {
    sdk: SdkConfig,
    /// The credentials last handed to the plugin, reused until they near
    /// expiry. Each fetch from the provider can be a `credential_process`
    /// (an MFA or biometric prompt), an STS call or an SSO call, and the
    /// provider in `SdkConfig` is not the SDK's cached one.
    plugin_creds: tokio::sync::Mutex<Option<aws_sdk_ssm::config::Credentials>>,
    ssm: aws_sdk_ssm::Client,
    region: String,
    plugin: PathBuf,
    target: String,
    remote_port: u16,
}

impl SessionOpener for SsmOpener {
    async fn open(&self, local_port: u16) -> Result<Running, String> {
        let out = self
            .ssm
            .start_session()
            .target(&self.target)
            .document_name(PORT_FORWARD_DOCUMENT)
            .parameters("portNumber", vec![self.remote_port.to_string()])
            .parameters("localPortNumber", vec![local_port.to_string()])
            .send()
            .await
            .map_err(|e| format!("Could not start the SSM session: {}", sdk_error(&e)))?;
        let session_id = out.session_id().unwrap_or_default().to_string();
        // From here the session exists on AWS's side. If this future is
        // dropped before it returns — a close or drop racing a reconnect, a
        // caller timing out the first open — the guard ends the session.
        let guard = SessionGuard {
            ssm: self.ssm.clone(),
            session_id: Some(session_id.clone()),
        };
        let response = Zeroizing::new(
            serde_json::json!({
                "SessionId": session_id,
                "StreamUrl": out.stream_url().unwrap_or_default(),
                "TokenValue": out.token_value().unwrap_or_default(),
            })
            .to_string(),
        );
        let request = serde_json::json!({
            "Target": self.target,
            "DocumentName": PORT_FORWARD_DOCUMENT,
            "Parameters": {
                "portNumber": [self.remote_port.to_string()],
                "localPortNumber": [local_port.to_string()],
            },
        })
        .to_string();

        let mut env = vec![
            (RESPONSE_ENV.to_string(), response),
            ("AWS_REGION".to_string(), Zeroizing::new(self.region.clone())),
        ];
        env.extend(self.plugin_credentials().await);

        let launch = PluginLaunch {
            program: self.plugin.clone(),
            // <response env var> <region> StartSession <profile> <request> <endpoint>:
            // what the AWS CLI passes. No profile: credentials come from env.
            args: vec![
                RESPONSE_ENV.to_string(),
                self.region.clone(),
                "StartSession".to_string(),
                String::new(),
                request,
                ssm_endpoint(&self.region),
            ],
            env,
            session_id: session_id.clone(),
        };
        match spawn_plugin(launch).await {
            Ok(r) => {
                guard.disarm();
                Ok(r)
            }
            Err(e) => {
                // No plugin is attached to the session; don't leave it open
                // until the idle timeout.
                guard.terminate().await;
                Err(e)
            }
        }
    }

    async fn terminate(&self, session_id: String) {
        terminate_session(&self.ssm, session_id).await
    }
}

async fn terminate_session(ssm: &aws_sdk_ssm::Client, session_id: String) {
    if session_id.is_empty() {
        return;
    }
    let call = ssm.terminate_session().session_id(&session_id).send();
    match tokio::time::timeout(TERMINATE_TIMEOUT, call).await {
        Ok(Ok(_)) => {}
        Ok(Err(e)) => log::warn!("TerminateSession {session_id}: {}", sdk_error(&e)),
        Err(_) => log::warn!("TerminateSession {session_id}: timed out"),
    }
}

/// Ends a started session unless disarmed: on an error path explicitly, and
/// on drop (a cancelled open) from a spawned task.
struct SessionGuard {
    ssm: aws_sdk_ssm::Client,
    session_id: Option<String>,
}

impl SessionGuard {
    fn disarm(mut self) {
        self.session_id = None;
    }

    async fn terminate(mut self) {
        if let Some(id) = self.session_id.take() {
            terminate_session(&self.ssm, id).await;
        }
    }
}

impl Drop for SessionGuard {
    fn drop(&mut self) {
        if let Some(id) = self.session_id.take() {
            if let Ok(rt) = tokio::runtime::Handle::try_current() {
                let ssm = self.ssm.clone();
                rt.spawn(async move { terminate_session(&ssm, id).await });
            }
        }
    }
}

/// How long before expiry cached plugin credentials are refreshed. A plugin
/// started with them may need them for a resume well after it starts.
const PLUGIN_CREDS_MIN_REMAINING: Duration = Duration::from_secs(15 * 60);

impl SsmOpener {
    /// The caller's credentials as the plugin's environment, for its own
    /// `ResumeSession` calls. Without them a resume fails and the
    /// supervisor's full reconnect takes over, so a failure here is not fatal.
    async fn plugin_credentials(&self) -> Vec<(String, Zeroizing<String>)> {
        use aws_sdk_ssm::config::ProvideCredentials;
        let mut cached = self.plugin_creds.lock().await;
        let fresh = |c: &aws_sdk_ssm::config::Credentials| match c.expiry() {
            None => true,
            Some(exp) => exp
                .duration_since(std::time::SystemTime::now())
                .map(|left| left > PLUGIN_CREDS_MIN_REMAINING)
                .unwrap_or(false),
        };
        if !cached.as_ref().is_some_and(fresh) {
            *cached = None;
            if let Some(provider) = self.sdk.credentials_provider() {
                match provider.provide_credentials().await {
                    Ok(c) => *cached = Some(c),
                    Err(e) => log::warn!("No credentials for the SSM plugin's resume: {}", sdk_error(&e)),
                }
            }
        }
        let Some(c) = cached.as_ref() else {
            return vec![];
        };
        let mut v = vec![
            ("AWS_ACCESS_KEY_ID".to_string(), Zeroizing::new(c.access_key_id().to_string())),
            (
                "AWS_SECRET_ACCESS_KEY".to_string(),
                Zeroizing::new(c.secret_access_key().to_string()),
            ),
        ];
        if let Some(t) = c.session_token() {
            v.push(("AWS_SESSION_TOKEN".to_string(), Zeroizing::new(t.to_string())));
        }
        v
    }
}

fn ssm_endpoint(region: &str) -> String {
    let suffix = if region.starts_with("cn-") { "amazonaws.com.cn" } else { "amazonaws.com" };
    format!("https://ssm.{region}.{suffix}")
}

/// A free loopback port, for the plugin to bind. The plugin binds it a moment
/// later, so another process can take it in between; the first connection
/// retries with a new port when that happens.
fn free_local_port() -> Result<u16, String> {
    std::net::TcpListener::bind(("127.0.0.1", 0))
        .and_then(|l| l.local_addr())
        .map(|a| a.port())
        .map_err(|e| format!("Could not find a free local port: {e}"))
}

// ── The port forward ─────────────────────────────────────────────────────────

/// Where the tunnel is. Watch it with [`PortForward::state`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TunnelState {
    Up,
    /// The session dropped and is being reopened. `last_error` is why the
    /// previous one ended or the previous attempt failed.
    Reconnecting { attempt: u32, last_error: String },
    /// Reconnecting gave up. Nothing listens on the local port.
    Failed(String),
    /// [`PortForward::close`] ran, or the handle was dropped.
    Closed,
}

/// One SSM port-forward to an agent, supervised: reopened on the same local
/// port when it drops, ended (`TerminateSession`) when this handle is closed
/// or dropped.
pub struct PortForward {
    local_port: u16,
    state: watch::Receiver<TunnelState>,
    #[allow(dead_code)] // `plugin_pid()`, for diagnostics and the tests
    plugin_pid: watch::Receiver<Option<u32>>,
    supervisor: Option<tokio::task::JoinHandle<()>>,
    _cancel: DropGuard,
    cancel: CancellationToken,
}

impl std::fmt::Debug for PortForward {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PortForward")
            .field("local_port", &self.local_port)
            .field("state", &*self.state.borrow())
            .finish()
    }
}

impl PortForward {
    /// Open a port forward from a free loopback port to `remote_port` in
    /// `task`, and return once the local listener is up. Errors (no plugin,
    /// IAM denial, the task not reachable) come back here rather than as a
    /// state.
    pub async fn open(sdk: &SdkConfig, task: &EcsTask, remote_port: u16) -> Result<Self, String> {
        let region = sdk
            .region()
            .ok_or("No AWS region is configured for this connection")?
            .to_string();
        let opener = SsmOpener {
            sdk: sdk.clone(),
            plugin_creds: Default::default(),
            ssm: aws_sdk_ssm::Client::new(sdk),
            region,
            plugin: find_plugin()?,
            target: task.ssm_target(),
            remote_port,
        };
        Self::open_with(opener).await
    }

    async fn open_with<O: SessionOpener>(opener: O) -> Result<Self, String> {
        let mut last_err = String::new();
        for _ in 0..INITIAL_PORT_ATTEMPTS {
            let port = free_local_port()?;
            match opener.open(port).await {
                Ok(running) => return Ok(Self::supervise(opener, port, running)),
                Err(e) if is_port_in_use(&e) => last_err = e,
                Err(e) => return Err(e),
            }
        }
        Err(last_err)
    }

    fn supervise<O: SessionOpener>(opener: O, local_port: u16, running: Running) -> Self {
        let (state_tx, state) = watch::channel(TunnelState::Up);
        let (pid_tx, plugin_pid) = watch::channel(running.child.id());
        let cancel = CancellationToken::new();
        let supervisor = tokio::spawn(run_supervisor(
            opener,
            local_port,
            running,
            state_tx,
            pid_tx,
            cancel.clone(),
        ));
        Self {
            local_port,
            state,
            plugin_pid,
            supervisor: Some(supervisor),
            _cancel: cancel.clone().drop_guard(),
            cancel,
        }
    }

    #[allow(dead_code)] // diagnostics and the tests (Task 8 may show the state)
    pub fn local_port(&self) -> u16 {
        self.local_port
    }

    /// The agent at the local end of this tunnel. Stays valid across
    /// reconnects (the port does not change).
    pub fn endpoint(&self, token: impl Into<String>) -> AgentEndpoint {
        AgentEndpoint::loopback(self.local_port, token)
    }

    #[allow(dead_code)] // diagnostics and the tests (Task 8 may show the state)
    pub fn state(&self) -> watch::Receiver<TunnelState> {
        self.state.clone()
    }

    /// The running plugin's process id, if one is running.
    #[allow(dead_code)] // diagnostics and the tests (Task 8 may show the state)
    pub fn plugin_pid(&self) -> Option<u32> {
        *self.plugin_pid.borrow()
    }

    /// Stop the plugin and end the session, and wait for both.
    pub async fn close(mut self) {
        self.cancel.cancel();
        if let Some(s) = self.supervisor.take() {
            let _ = s.await;
        }
    }
}

fn is_port_in_use(e: &str) -> bool {
    let e = e.to_ascii_lowercase();
    e.contains("address already in use") || e.contains("only one usage of each socket address")
}

async fn run_supervisor<O: SessionOpener>(
    opener: O,
    local_port: u16,
    mut running: Running,
    state: watch::Sender<TunnelState>,
    pid: watch::Sender<Option<u32>>,
    cancel: CancellationToken,
) {
    loop {
        // ── Up: wait for the plugin to exit, or for close; keep it busy ──
        let mut keepalive = tokio::time::interval_at(
            tokio::time::Instant::now() + KEEPALIVE_INTERVAL,
            KEEPALIVE_INTERVAL,
        );
        keepalive.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let exited = loop {
            tokio::select! {
                s = running.child.wait() => break s,
                _ = keepalive.tick() => {
                    tokio::spawn(keepalive_probe(local_port));
                }
                _ = cancel.cancelled() => {
                    let _ = running.child.kill().await;
                    opener.terminate(running.session_id).await;
                    let _ = pid.send(None);
                    let _ = state.send(TunnelState::Closed);
                    return;
                }
            }
        };
        let _ = pid.send(None);
        // Let the drains catch the last lines.
        tokio::time::sleep(Duration::from_millis(50)).await;
        // Both halves: SSM's own reason is in the output ("timed out due to
        // inactivity"), but after a kill the last line is just whatever the
        // plugin printed before, so the exit status has to be there too.
        let mut last_error = match running.output.last() {
            Some(line) => format!("the SSM session ended ({}; last output: {line})", describe_exit(exited)),
            None => format!("the SSM session ended ({})", describe_exit(exited)),
        };
        log::warn!("SSM port forward on 127.0.0.1:{local_port}: {last_error}; reconnecting");
        opener.terminate(running.session_id).await;

        // ── Reconnecting ──
        let mut attempt = 0;
        running = loop {
            attempt += 1;
            if attempt > MAX_RECONNECT_ATTEMPTS {
                let _ = state.send(TunnelState::Failed(last_error));
                // Nothing to clean up; wait for the handle to go.
                cancel.cancelled().await;
                let _ = state.send(TunnelState::Closed);
                return;
            }
            let _ = state.send(TunnelState::Reconnecting {
                attempt,
                last_error: last_error.clone(),
            });
            let delay = reconnect_delay(attempt);
            tokio::select! {
                _ = tokio::time::sleep(delay) => {}
                _ = cancel.cancelled() => {
                    let _ = state.send(TunnelState::Closed);
                    return;
                }
            }
            let opened = tokio::select! {
                r = opener.open(local_port) => r,
                _ = cancel.cancelled() => {
                    let _ = state.send(TunnelState::Closed);
                    return;
                }
            };
            match opened {
                Ok(r) => break r,
                Err(e) => {
                    log::warn!("SSM port forward on 127.0.0.1:{local_port}: reconnect {attempt}: {e}");
                    last_error = e;
                }
            }
        };
        let _ = pid.send(running.child.id());
        let _ = state.send(TunnelState::Up);
        log::info!("SSM port forward on 127.0.0.1:{local_port}: reconnected after {attempt} attempt(s)");
    }
}

/// One request through the tunnel so SSM sees data: an *unauthenticated*
/// `GET /v1/health`. The agent answers `401` before it counts anything as
/// activity, so this keeps the session alive without keeping the workspace
/// "in use" for idle auto-stop — and it needs no token here.
async fn keepalive_probe(local_port: u16) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let probe = async {
        let mut s = tokio::net::TcpStream::connect(("127.0.0.1", local_port)).await?;
        s.write_all(b"GET /v1/health HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n")
            .await?;
        let mut sink = [0u8; 512];
        while s.read(&mut sink).await? > 0 {}
        Ok::<_, std::io::Error>(())
    };
    match tokio::time::timeout(KEEPALIVE_TIMEOUT, probe).await {
        Ok(Ok(())) => {}
        Ok(Err(e)) => log::debug!("SSM keepalive on 127.0.0.1:{local_port}: {e}"),
        Err(_) => log::debug!("SSM keepalive on 127.0.0.1:{local_port}: timed out"),
    }
}

fn reconnect_delay(attempt: u32) -> Duration {
    let d = RECONNECT_BACKOFF[(attempt as usize).saturating_sub(1).min(RECONNECT_BACKOFF.len() - 1)];
    if cfg!(test) {
        d / 100
    } else {
        d
    }
}

fn describe_exit(s: std::io::Result<std::process::ExitStatus>) -> String {
    match s {
        Ok(s) => format!("plugin {s}"),
        Err(e) => format!("plugin wait failed: {e}"),
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod live_tests;
