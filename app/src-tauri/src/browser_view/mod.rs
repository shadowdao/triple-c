//! Browser view — watch, and take over, the browser Claude is driving.
//!
//! ## What is actually being watched
//!
//! Playwright ships a live dashboard. A script inside the container calls
//! `await browser.bind('claude')`, which publishes a descriptor for the running
//! browser into `~/.cache/ms-playwright/b/`; `@playwright/mcp` does this for you.
//! `playwright-cli show --host 127.0.0.1 --port <p>` then serves a React viewer
//! that watches that directory, connects to the published browser, and gives you
//! a CDP screencast with full mouse and keyboard takeover — all of which works
//! with `headless: true`, which is the only thing that could work in a container.
//!
//! Discovery is *local filesystem*, so the viewer has to run in the same
//! container as the browsers. There is nothing a host-side viewer could see.
//!
//! ## Getting it onto the screen safely
//!
//! ```text
//!   webview <iframe>                    host                          container
//!   ────────────────                    ────                          ─────────
//!   http://127.0.0.1:47820/index.html
//!            ?ws=…&token=…   ────►  BrowserViewProxy   ──socat exec──►  playwright-cli show
//!                                   (token gate)          (Docker API)   127.0.0.1:39321
//! ```
//!
//! The proxy is the *only* host-bound socket, and it authenticates before a byte
//! reaches the container — see [`proxy`] for the gate, and for why the auth
//! bridge's unauthenticated [`PortForward`](crate::auth_bridge::tunnel::PortForward)
//! is deliberately not used to carry this port. The container-side viewer port is
//! additionally *reserved* with
//! [`crate::auth_bridge::RESERVED_CONTAINER_PORTS`], so that a project which
//! also has the auth bridge on cannot end up with the viewer mirrored onto the
//! host a second time, ungated.
//!
//! ## Lifecycle
//!
//! Off by default and per-project opt-in, exactly like `auth_bridge_enabled`.
//! One supervisor task per session owns the proxy and the viewer process, and it
//! is the only thing that tears them down, so every way a session can end funnels
//! through one code path:
//!
//! | Trigger | Path |
//! |---|---|
//! | Turned off in the UI | `set_browser_view_enabled(false)` → [`BrowserViewManager::stop`] |
//! | Container stopped, by the UI or otherwise | supervisor's `is_container_running` check |
//! | Project deleted | supervisor's `store.get()` check |
//! | Container rebuilt | old container stops → supervisor exits; the new one is not auto-started |
//! | Viewer died in the container | supervisor's periodic HTTP liveness probe |
//! | App exit | [`BrowserViewManager::stop_all`] |
//!
//! [`BrowserViewManager::stop`] awaits the supervisor, so the host port is
//! provably released before it returns.
//!
//! One honest gap, verified rather than assumed: `playwright-cli show` is only
//! a launcher — the dashboard it starts reparents to PID 1 and survives the
//! exec that spawned it. Every ordinary teardown path above calls
//! [`kill_dashboard`], which does stop it, but a *hard* app crash leaves the
//! dashboard running inside the container until the container stops. That
//! orphan is reachable on container loopback only: the host-side port dies with
//! the app, and [`crate::auth_bridge::RESERVED_CONTAINER_PORTS`] is a constant
//! precisely so the bridge will not mirror an orphan the next time the app
//! starts. The next [`BrowserViewManager::start`] reclaims it.

pub mod commands;
pub mod detect;
pub mod install;
pub mod popout;
pub mod proxy;

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use serde::Serialize;
use tauri::{AppHandle, Emitter};
use tokio::sync::{watch, Mutex};
use tokio::task::JoinHandle;

use crate::auth_bridge::proc_net::{self, PortFamily};
use crate::docker::container::is_container_running;
use crate::docker::exec::exec_oneshot;
use crate::storage::projects_store::ProjectsStore;

use detect::PlaywrightDetection;
use proxy::BrowserViewProxy;

/// Emitted whenever a project's browser view starts, stops or fails.
/// Payload: `{ project_id, status: BrowserViewStatus }`.
const BROWSER_VIEW_EVENT: &str = "browser-view-changed";

/// Container-side ports the viewer may bind, tried in order. The dashboard is a
/// per-workspace singleton inside the container, so only one is ever in use at
/// a time; the range exists only so an unrelated service already sitting on the
/// first port doesn't take the feature down.
///
/// This *is* [`crate::auth_bridge::RESERVED_CONTAINER_PORTS`] — the bridge must
/// never mirror these, so the two cannot be allowed to drift.
const VIEWER_PORTS: std::ops::RangeInclusive<u16> = crate::auth_bridge::RESERVED_CONTAINER_PORTS;

/// How often the supervisor re-checks that the session still has a reason to
/// exist. Matches the auth bridge's cadence.
const SUPERVISE_INTERVAL: Duration = Duration::from_secs(2);

/// Supervisor ticks between HTTP liveness probes of the viewer. The two cheap
/// checks run every tick; this one costs a container exec, so it runs at 1/5
/// the rate (~10s).
const LIVENESS_EVERY: u32 = 5;

/// Ceiling on one readiness/liveness probe. Enforced inside the container by
/// Node and again here, so neither a wedged daemon nor a wedged exec can stall
/// the supervisor.
const PROBE_TIMEOUT: Duration = Duration::from_secs(4);

/// How long to wait for `playwright-cli show` to start answering HTTP.
const READY_TIMEOUT: Duration = Duration::from_secs(30);
const READY_POLL: Duration = Duration::from_millis(400);

// ─────────────────────────────────────────────────────────────────────────────
// IPC response model
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BrowserViewState {
    /// Not running. Either never started, or stopped.
    Off,
    /// Running and reachable at `url`.
    Running,
    /// The container can't serve this — see `message` for what to install.
    Unavailable,
}

#[derive(Debug, Clone, Serialize)]
pub struct BrowserViewStatus {
    /// The per-project opt-in. Off by default.
    pub enabled: bool,
    pub state: BrowserViewState,
    /// Fully-formed, token-bearing URL for the pane's iframe. Loopback only.
    pub url: Option<String>,
    pub host_port: Option<u16>,
    pub container_port: Option<u16>,
    /// RFC 3339 timestamp of when the viewer came up.
    pub started_at: Option<String>,
    /// What was found in the container. Present even when unusable, because
    /// that is exactly when the user needs to see it.
    pub detection: Option<PlaywrightDetection>,
    /// Human-readable explanation, set whenever `state` isn't `Running`.
    pub message: Option<String>,
}

impl BrowserViewStatus {
    fn off(enabled: bool) -> Self {
        Self {
            enabled,
            state: BrowserViewState::Off,
            url: None,
            host_port: None,
            container_port: None,
            started_at: None,
            detection: None,
            message: None,
        }
    }

    fn unavailable(enabled: bool, detection: PlaywrightDetection, message: String) -> Self {
        Self {
            enabled,
            state: BrowserViewState::Unavailable,
            detection: Some(detection),
            message: Some(message),
            ..Self::off(enabled)
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Manager
// ─────────────────────────────────────────────────────────────────────────────

/// Everything a live session exposes to `status()`. Fixed once the session is
/// up, so it can be cloned out from under the map lock.
#[derive(Debug, Clone)]
struct SessionMeta {
    url: String,
    host_port: u16,
    container_port: u16,
    started_at: String,
    detection: PlaywrightDetection,
}

struct Session {
    /// Distinguishes this supervisor from a later one for the same project, so
    /// a supervisor that exits late can't evict its replacement.
    epoch: u64,
    cancel: watch::Sender<bool>,
    meta: SessionMeta,
    supervisor: JoinHandle<()>,
}

type SessionMap = Arc<Mutex<HashMap<String, Session>>>;

#[derive(Default)]
pub struct BrowserViewManager {
    sessions: SessionMap,
    /// The per-project opt-in.
    ///
    /// NOTE: in memory only, so it does not survive an app restart. The durable
    /// home for this is a `browser_view_enabled: bool` field on
    /// `models::Project` (see the report) — `models/project.rs` is out of scope
    /// for this change, so the flag lives here and the wiring is otherwise
    /// identical to `auth_bridge_enabled`.
    enabled: Mutex<std::collections::HashSet<String>>,
    next_epoch: AtomicU64,
}

/// Process-wide handle.
///
/// Deliberately *not* a field on `AppState`: keeping it here means the feature
/// needs no edit to `lib.rs` beyond declaring the module and registering the
/// commands, and it lets teardown paths reach it without threading state.
pub fn manager() -> &'static Arc<BrowserViewManager> {
    static MANAGER: OnceLock<Arc<BrowserViewManager>> = OnceLock::new();
    MANAGER.get_or_init(|| Arc::new(BrowserViewManager::default()))
}

impl BrowserViewManager {
    pub async fn is_enabled(&self, project_id: &str) -> bool {
        self.enabled.lock().await.contains(project_id)
    }

    async fn set_enabled(&self, project_id: &str, enabled: bool) {
        let mut set = self.enabled.lock().await;
        if enabled {
            set.insert(project_id.to_string());
        } else {
            set.remove(project_id);
        }
    }

    /// Current status without touching the container.
    pub async fn status(&self, project_id: &str) -> BrowserViewStatus {
        let enabled = self.is_enabled(project_id).await;
        match self.sessions.lock().await.get(project_id) {
            Some(session) => BrowserViewStatus {
                enabled,
                state: BrowserViewState::Running,
                url: Some(session.meta.url.clone()),
                host_port: Some(session.meta.host_port),
                container_port: Some(session.meta.container_port),
                started_at: Some(session.meta.started_at.clone()),
                detection: Some(session.meta.detection.clone()),
                message: None,
            },
            None => BrowserViewStatus::off(enabled),
        }
    }

    /// Probe the container and, if it can serve a viewer, bring one up.
    ///
    /// Idempotent: a call while a live session exists returns that session's
    /// status untouched, so re-opening the tab does not restart the dashboard.
    pub async fn start(
        &self,
        project_id: String,
        container_id: String,
        app: AppHandle,
        store: Arc<ProjectsStore>,
    ) -> Result<BrowserViewStatus, String> {
        self.set_enabled(&project_id, true).await;

        // Bind the answer before acting on it: `status()` takes the same lock,
        // and this mutex is not reentrant.
        let already_live = self
            .sessions
            .lock()
            .await
            .get(&project_id)
            .is_some_and(|s| !s.supervisor.is_finished());
        if already_live {
            return Ok(self.status(&project_id).await);
        }

        let detection = detect::detect(&container_id).await?;
        if !detection.is_usable() {
            let blocker = detection.blocker().unwrap_or_else(|| {
                "Playwright is present but incomplete in this container.".to_string()
            });
            let status = BrowserViewStatus::unavailable(true, detection, blocker);
            emit(&app, &project_id, &status);
            return Ok(status);
        }
        // `is_usable()` already established this, so the fallback is unreachable.
        let cli_entry = detection.cli_entry.clone().unwrap_or_default();

        // The dashboard is a per-workspace singleton keyed on a unix socket in
        // the temp dir, not on a port. Verified: while one is running, a second
        // `show --port` prints "Dashboard is running pid=…", exits 0, and
        // *ignores the port you asked for*. So always reclaim first — including
        // a daemon this app orphaned in an earlier run, since it outlives us.
        // Doing this before choosing a port also frees the one a previous
        // session was using, so sessions don't walk up the range. Best-effort:
        // a container with no dashboard makes this a no-op.
        let _ = kill_dashboard(&container_id, &cli_entry).await;

        let container_port = pick_viewer_port(&container_id).await?;
        launch_viewer(&container_id, &cli_entry, container_port).await?;

        // Wait for it to actually answer, and learn the entry URL while we're
        // there — see `probe_entry_path` for why that matters. This, not the
        // launcher's stdout, is the readiness signal: verified that the
        // "Listening on …" line is printed only on the very first start.
        let entry_path = match wait_until_ready(&container_id, container_port).await {
            Ok(path) => path,
            Err(e) => {
                let log = read_viewer_log(&container_id).await;
                let _ = kill_dashboard(&container_id, &cli_entry).await;
                return Err(explain_start_failure(&e, &log));
            }
        };

        let token = generate_token();
        // `--host 127.0.0.1` is ours to set, so the family is known and there is
        // no need to go back to /proc/net to work it out.
        let proxy = match BrowserViewProxy::bind(
            container_id.clone(),
            container_port,
            PortFamily::V4,
            token.clone(),
        )
        .await
        {
            Ok(p) => p,
            Err(e) => {
                let _ = kill_dashboard(&container_id, &cli_entry).await;
                return Err(e);
            }
        };

        let meta = SessionMeta {
            url: build_url(proxy.port, &entry_path, &token),
            host_port: proxy.port,
            container_port,
            started_at: chrono::Utc::now().to_rfc3339(),
            detection,
        };

        let epoch = self.next_epoch.fetch_add(1, Ordering::Relaxed);
        let (cancel_tx, cancel_rx) = watch::channel(false);
        let supervisor = tokio::spawn(supervise(
            project_id.clone(),
            container_id.clone(),
            cli_entry,
            container_port,
            epoch,
            app.clone(),
            store,
            self.sessions.clone(),
            cancel_rx,
            proxy,
        ));

        log::info!(
            "Browser view: project {} → 127.0.0.1:{} → container 127.0.0.1:{}",
            project_id,
            meta.host_port,
            container_port
        );

        self.sessions.lock().await.insert(
            project_id.clone(),
            Session {
                epoch,
                cancel: cancel_tx,
                meta,
                supervisor,
            },
        );

        let status = self.status(&project_id).await;
        emit(&app, &project_id, &status);
        Ok(status)
    }

    /// Stop one project's view and wait until its host port has been released.
    pub async fn stop(&self, project_id: &str) {
        self.set_enabled(project_id, false).await;
        // Remove under the lock, then release it before awaiting: the
        // supervisor takes the same lock to deregister itself on exit.
        let session = self.sessions.lock().await.remove(project_id);
        if let Some(session) = session {
            let _ = session.cancel.send(true);
            let _ = session.supervisor.await;
            log::info!("Browser view: stopped for project {}", project_id);
        }
    }

    /// Stop every view. Used on app exit.
    pub async fn stop_all(&self) {
        let sessions: Vec<(String, Session)> = self.sessions.lock().await.drain().collect();
        for (project_id, session) in sessions {
            let _ = session.cancel.send(true);
            let _ = session.supervisor.await;
            log::info!("Browser view: stopped for project {}", project_id);
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Supervisor
// ─────────────────────────────────────────────────────────────────────────────

/// Owns the proxy and the viewer process for one session and is the only thing
/// that tears them down, so a session can't half-die.
#[allow(clippy::too_many_arguments)]
async fn supervise(
    project_id: String,
    container_id: String,
    cli_entry: String,
    container_port: u16,
    epoch: u64,
    app: AppHandle,
    store: Arc<ProjectsStore>,
    sessions: SessionMap,
    mut cancel: watch::Receiver<bool>,
    mut proxy: BrowserViewProxy,
) {
    let mut ticks: u32 = 0;
    loop {
        if store.get(&project_id).is_none() {
            log::info!("Browser view: project {} is gone — tearing down", project_id);
            break;
        }
        if !is_container_running(&container_id).await.unwrap_or(false) {
            log::info!(
                "Browser view: container for project {} is no longer running — tearing down",
                project_id
            );
            break;
        }
        // The dashboard is a detached daemon, so there is no process handle to
        // watch: liveness has to be an actual request. That costs an exec, so
        // it runs at a coarser cadence than the two cheap checks above.
        ticks = ticks.wrapping_add(1);
        if ticks % LIVENESS_EVERY == 0 {
            // Cancellation races the probe, not just the sleep, so stopping the
            // view never waits out an in-flight exec.
            let alive = tokio::select! {
                _ = cancel.changed() => break,
                res = probe_entry_path(&container_id, container_port) => res.is_ok(),
            };
            if !alive {
                log::warn!(
                    "Browser view: the viewer for project {} stopped answering — tearing down",
                    project_id
                );
                break;
            }
        }

        tokio::select! {
            _ = cancel.changed() => break,
            _ = tokio::time::sleep(SUPERVISE_INTERVAL) => {}
        }
    }

    proxy.shutdown().await;
    let _ = kill_dashboard(&container_id, &cli_entry).await;

    // Deregister, unless a newer session has already taken this project's slot.
    {
        let mut map = sessions.lock().await;
        if map.get(&project_id).is_some_and(|s| s.epoch == epoch) {
            map.remove(&project_id);
        }
    }

    // A pop-out outlives the tab, so nothing else would take it down: the
    // window would sit there showing a frozen last frame of a viewer that no
    // longer exists. The session owns it, and this is where the session ends.
    popout::close(&app, &project_id);

    let enabled = manager().is_enabled(&project_id).await;
    emit(&app, &project_id, &BrowserViewStatus::off(enabled));
}

// ─────────────────────────────────────────────────────────────────────────────
// The viewer process
// ─────────────────────────────────────────────────────────────────────────────

/// Where the detached viewer's own output goes, so a failed start still has
/// something to show the user.
const VIEWER_LOG: &str = "/tmp/triple-c-browser-view.log";

/// Start `playwright-cli show`, detached.
///
/// `playwright-cli show` is a *launcher*: verified that it spawns
/// `playwright-core/lib/entry/dashboardApp.js`, which reparents to PID 1 and
/// outlives both the launcher and the exec that started it. So there is no
/// point tying a process lifetime to the exec's stdin — signalling the launcher
/// leaves the dashboard bound to its port and still serving. Teardown is
/// [`kill_dashboard`], which is the only thing verified to actually stop it.
///
/// Consequently this is a fire-and-forget exec: the launcher's output is
/// redirected to [`VIEWER_LOG`] (both so `exec_oneshot` can return immediately
/// rather than waiting on an inherited stdout, and so a failure has a trail),
/// and readiness is established by [`wait_until_ready`] instead.
async fn launch_viewer(container_id: &str, cli_entry: &str, port: u16) -> Result<(), String> {
    // `NO_UPDATE_NOTIFIER` stops the CLI phoning registry.npmjs.org on every
    // launch; the container may have no egress, and we don't want to wait out a
    // DNS timeout before the dashboard binds.
    let script = format!(
        "{}; NO_UPDATE_NOTIFIER=1 nohup node {} show --host 127.0.0.1 --port {} >{} 2>&1 &",
        WORKDIR_PREFIX,
        shell_quote(cli_entry),
        port,
        VIEWER_LOG
    );
    exec_oneshot(
        container_id,
        vec!["sh".to_string(), "-c".to_string(), script],
    )
    .await
    .map(|_| ())
    .map_err(|e| format!("Could not start the Playwright viewer: {}", e))
}

/// The dashboard singleton is keyed on a hash of the working directory, so
/// `show` and `show --kill` must agree on one. `exec_oneshot` doesn't set a
/// working directory (it inherits the image's), and `/workspace` is both what
/// the image sets today and where Claude actually runs — but pinning it here
/// means a change to the image can't silently split the two into different
/// singletons, leaving a dashboard nothing can kill.
const WORKDIR_PREFIX: &str = "cd /workspace 2>/dev/null || true";

/// Stop the dashboard daemon. Verified to free the port and stop answering.
async fn kill_dashboard(container_id: &str, cli_entry: &str) -> Result<String, String> {
    let script = format!(
        "{}; NO_UPDATE_NOTIFIER=1 node {} show --kill",
        WORKDIR_PREFIX,
        shell_quote(cli_entry)
    );
    exec_oneshot(
        container_id,
        vec!["sh".to_string(), "-c".to_string(), script],
    )
    .await
}

/// Turn a failed start into something the user can act on.
///
/// The one failure worth naming is the singleton clash: if a dashboard we
/// couldn't reclaim is still alive, the launcher exits 0 having printed
/// "Dashboard is running pid=…" and having silently ignored the port we asked
/// for, so all the caller sees is a port that never answers.
fn explain_start_failure(err: &str, log: &str) -> String {
    let log = log.trim();
    if log.contains("Dashboard is running") {
        return format!(
            "Another Playwright dashboard is already running in this container and would not \
             give up its port. Stop it from a terminal in the container with \
             `npx playwright-cli show --kill`, then try again.\n\nViewer output:\n{}",
            log
        );
    }
    if log.is_empty() {
        err.to_string()
    } else {
        format!("{}\n\nViewer output:\n{}", err, log)
    }
}

/// Tail of the viewer's own output, for a start that didn't come up.
async fn read_viewer_log(container_id: &str) -> String {
    exec_oneshot(
        container_id,
        vec!["tail".to_string(), "-n".to_string(), "40".to_string(), VIEWER_LOG.to_string()],
    )
    .await
    .unwrap_or_default()
}

// ─────────────────────────────────────────────────────────────────────────────
// Readiness, ports, URLs
// ─────────────────────────────────────────────────────────────────────────────

/// First port in [`VIEWER_PORTS`] that nothing in the container is listening on.
async fn pick_viewer_port(container_id: &str) -> Result<u16, String> {
    let text = exec_oneshot(
        container_id,
        vec![
            "cat".to_string(),
            "/proc/net/tcp".to_string(),
            "/proc/net/tcp6".to_string(),
        ],
    )
    .await
    .unwrap_or_default();
    let taken = proc_net::parse_loopback_listeners(&text);
    VIEWER_PORTS
        .clone()
        .find(|p| !taken.contains_key(p))
        .ok_or_else(|| {
            format!(
                "No free port in {}–{} inside the container for the Playwright viewer.",
                VIEWER_PORTS.start(),
                VIEWER_PORTS.end()
            )
        })
}

/// Poll the viewer until it answers, and return the path the pane should load.
async fn wait_until_ready(container_id: &str, port: u16) -> Result<String, String> {
    let deadline = tokio::time::Instant::now() + READY_TIMEOUT;
    loop {
        let last = match probe_entry_path(container_id, port).await {
            Ok(path) => return Ok(path),
            Err(e) => e,
        };
        if tokio::time::Instant::now() >= deadline {
            return Err(format!(
                "The Playwright viewer did not start listening on container port {} within {}s ({}).",
                port,
                READY_TIMEOUT.as_secs(),
                last
            ));
        }
        tokio::time::sleep(READY_POLL).await;
    }
}

const PROBE_MARKER: &str = "__TRIPLE_C_BV_PATH__";

/// Ask the viewer, from inside the container, what it wants to be loaded as.
///
/// `GET /` answers `302 Location: /index.html?ws=<guid>`, where the guid is the
/// dashboard's own per-run capability for its WebSocket. Resolving that here and
/// pointing the iframe straight at the final URL means the pane never traverses
/// a redirect — which matters, because a redirect drops the `?token=` the proxy
/// gate wants and would leave a fresh connection to be authorised with nothing.
/// A `200` (no redirect) is fine too; then the entry point is just `/`.
async fn probe_entry_path(container_id: &str, port: u16) -> Result<String, String> {
    // The request is bounded on both sides. Verified: the dashboard answers a
    // bad WebSocket path by holding the socket open forever rather than
    // erroring, so "no reply" is a state this probe has to be able to leave —
    // otherwise a wedged daemon would wedge the supervisor, and `stop()` waits
    // on the supervisor.
    let script = format!(
        r#"const q=require("http").get({{host:"127.0.0.1",port:{},path:"/",headers:{{host:"127.0.0.1:{}"}}}},r=>{{process.stdout.write("\n{}"+r.statusCode+" "+(r.headers.location||"/")+"\n");r.resume();process.exit(0);}});q.on("error",e=>{{process.stderr.write(String(e.message));process.exit(1);}});q.setTimeout({},()=>{{process.stderr.write("timed out waiting for the viewer");q.destroy();process.exit(1);}});"#,
        port,
        port,
        PROBE_MARKER,
        PROBE_TIMEOUT.as_millis()
    );
    let out = tokio::time::timeout(
        PROBE_TIMEOUT * 2,
        exec_oneshot(
            container_id,
            vec!["node".to_string(), "-e".to_string(), script],
        ),
    )
    .await
    .map_err(|_| "the viewer probe did not return".to_string())??;
    parse_entry_probe(&out)
}

/// Turn the readiness probe's output into the path to load.
fn parse_entry_probe(out: &str) -> Result<String, String> {
    let Some(idx) = out.find(PROBE_MARKER) else {
        let trimmed = out.trim();
        return Err(if trimmed.is_empty() {
            "no response".to_string()
        } else {
            trimmed.lines().next_back().unwrap_or(trimmed).to_string()
        });
    };
    let line = out[idx + PROBE_MARKER.len()..]
        .lines()
        .next()
        .unwrap_or("")
        .trim();
    let (status, location) = line.split_once(' ').unwrap_or((line, "/"));
    match status {
        "301" | "302" | "303" | "307" | "308" => {
            // Only same-origin, absolute paths — the dashboard never sends
            // anything else, and following an off-host redirect through the
            // pane would be a nasty surprise.
            if location.starts_with('/') {
                Ok(location.to_string())
            } else {
                Ok("/".to_string())
            }
        }
        "200" => Ok("/".to_string()),
        other => Err(format!("viewer answered HTTP {}", other)),
    }
}

/// The pane's iframe URL: the viewer's own entry path with our session token
/// appended, on the host loopback port the gate is listening on.
fn build_url(host_port: u16, entry_path: &str, token: &str) -> String {
    let sep = if entry_path.contains('?') { '&' } else { '?' };
    format!(
        "http://127.0.0.1:{}{}{}token={}",
        host_port, entry_path, sep, token
    )
}

/// Single-quote a path for `sh -c`. Paths from `require.resolve` never contain
/// quotes in practice, but this is a shell command line and the cost of being
/// sure is one line.
fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// 256 bits of URL-safe randomness, matching `web_terminal`'s token shape.
fn generate_token() -> String {
    use base64::Engine;
    use rand::Rng;
    let mut rng = rand::rng();
    let bytes: Vec<u8> = (0..32).map(|_| rng.random::<u8>()).collect();
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&bytes)
}

fn emit(app: &AppHandle, project_id: &str, status: &BrowserViewStatus) {
    let _ = app.emit(
        BROWSER_VIEW_EVENT,
        serde_json::json!({ "project_id": project_id, "status": status }),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_redirect_becomes_the_entry_path() {
        let out = format!("\n{}302 /index.html?ws=abc123\n", PROBE_MARKER);
        assert_eq!(parse_entry_probe(&out).unwrap(), "/index.html?ws=abc123");
    }

    #[test]
    fn a_plain_200_entry_point_is_the_root() {
        let out = format!("\n{}200 /\n", PROBE_MARKER);
        assert_eq!(parse_entry_probe(&out).unwrap(), "/");
    }

    #[test]
    fn an_off_host_redirect_is_not_followed() {
        let out = format!("\n{}302 https://evil.example/\n", PROBE_MARKER);
        assert_eq!(parse_entry_probe(&out).unwrap(), "/");
    }

    #[test]
    fn a_refused_connection_is_an_error_the_poller_can_retry() {
        // Verified shape: node writes this to stderr with no trailing newline.
        let err = parse_entry_probe("connect ECONNREFUSED 127.0.0.1:39321").unwrap_err();
        assert!(err.contains("ECONNREFUSED"), "{}", err);
        assert_eq!(parse_entry_probe("").unwrap_err(), "no response");
        assert!(parse_entry_probe("timed out waiting for the viewer")
            .unwrap_err()
            .contains("timed out"));
    }

    #[test]
    fn an_unexpected_status_is_surfaced_rather_than_loaded() {
        let out = format!("\n{}500 /\n", PROBE_MARKER);
        assert!(parse_entry_probe(&out).unwrap_err().contains("500"));
    }

    #[test]
    fn the_pane_url_is_loopback_and_carries_the_token() {
        let url = build_url(47820, "/index.html?ws=abc", "TOKEN");
        assert_eq!(url, "http://127.0.0.1:47820/index.html?ws=abc&token=TOKEN");
        assert!(url.starts_with("http://127.0.0.1:"));

        // A viewer that doesn't redirect gets a `?`, not a stray `&`.
        assert_eq!(
            build_url(47821, "/", "T"),
            "http://127.0.0.1:47821/?token=T"
        );
    }

    #[test]
    fn tokens_are_unique_and_url_safe() {
        let a = generate_token();
        let b = generate_token();
        assert_ne!(a, b);
        assert_eq!(a.len(), 43); // 32 bytes, base64url, unpadded
        assert!(a.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'));
    }

    #[test]
    fn shell_quoting_survives_a_hostile_path() {
        assert_eq!(shell_quote("/a/b/cli.js"), "'/a/b/cli.js'");
        assert_eq!(
            shell_quote("/a/'; rm -rf /; '"),
            r#"'/a/'\''; rm -rf /; '\'''"#
        );
    }

    #[test]
    fn a_singleton_clash_is_named_rather_than_left_as_a_dead_port() {
        let msg = explain_start_failure(
            "did not start listening on container port 39321 within 30s",
            "Dashboard is running pid=1823\n",
        );
        assert!(msg.contains("show --kill"), "{}", msg);
        assert!(msg.contains("pid=1823"), "{}", msg);
    }

    #[test]
    fn an_ordinary_start_failure_keeps_the_error_and_any_log() {
        assert_eq!(explain_start_failure("boom", "   "), "boom");
        let msg = explain_start_failure("boom", "EADDRINUSE 39321");
        assert!(msg.starts_with("boom"), "{}", msg);
        assert!(msg.contains("EADDRINUSE 39321"), "{}", msg);
    }

    #[test]
    fn the_viewer_port_range_is_bounded() {
        assert_eq!(VIEWER_PORTS.clone().count(), 8);
        // The auth bridge refuses to mirror exactly this range; if they ever
        // drifted apart the pane would gain an ungated second front door.
        assert_eq!(VIEWER_PORTS, crate::auth_bridge::RESERVED_CONTAINER_PORTS);
    }

    #[test]
    fn an_off_status_says_nothing_is_running() {
        let s = BrowserViewStatus::off(true);
        assert!(s.enabled);
        assert_eq!(s.state, BrowserViewState::Off);
        assert!(s.url.is_none());
    }

    #[test]
    fn an_unavailable_status_keeps_the_detail_the_user_needs() {
        let mut d = PlaywrightDetection::default();
        d.node_version = Some("22.11.0".to_string());
        let s = BrowserViewStatus::unavailable(true, d, "install it".to_string());
        assert_eq!(s.state, BrowserViewState::Unavailable);
        assert_eq!(s.message.as_deref(), Some("install it"));
        assert!(s.detection.is_some());
        assert!(s.url.is_none());
    }
}
