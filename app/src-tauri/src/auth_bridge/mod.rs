//! Auth Bridge — lets browser-based OAuth logins run by CLIs *inside* a
//! container complete against the browser on the *host*.
//!
//! ## The problem
//!
//! `claude login`, Concourse's `fly login`, `aws sso login` and friends all use
//! the same pattern: start a throwaway HTTP listener on a random loopback port,
//! then open a browser at a provider URL whose redirect points back to
//! `http://localhost:<that port>/callback`. Run inside a container, the listener
//! is on the *container's* loopback, the browser is on the *host's*, and the
//! callback goes nowhere — the login just hangs. The ports are ephemeral and not
//! configurable, so nothing can be pre-published at container creation time.
//!
//! ## The mechanism
//!
//! While the bridge is enabled for a running project, poll the container every
//! [`POLL_INTERVAL`] for loopback TCP listeners (see [`proc_net`]). For each one
//! that appears, bind the *same* port on the host's loopback and proxy each
//! accepted connection into the container over `docker exec … socat` (see
//! [`tunnel`]). When the in-container listener goes away, drop the host
//! listener. The host and container therefore agree on the port number, which is
//! the whole trick: the redirect URL the provider was given resolves correctly
//! on both sides.
//!
//! ## Lifecycle and teardown
//!
//! One poller task per project. It is the only thing that owns
//! [`PortForward`]s, and it always tears them down on its way out, so every way
//! the bridge can end funnels through the same code:
//!
//! | Trigger | Path |
//! |---|---|
//! | Bridge disabled | `set_auth_bridge_enabled(false)` → [`AuthBridgeManager::stop`] |
//! | Container stopped via UI | `stop_project_container` → [`AuthBridgeManager::stop`] |
//! | Container stopped/died another way | poller's own `is_container_running` check → loop exits |
//! | Project deleted | `remove_project` → [`AuthBridgeManager::stop`]; also the poller's `store.get()` check |
//! | Container rebuilt | `rebuild_project_container` → stop, then start re-arms it |
//! | App exit | window `CloseRequested` → [`AuthBridgeManager::stop_all`] |
//!
//! [`AuthBridgeManager::stop`] awaits the poller, so host ports are provably
//! released before it returns. As a backstop for any path that skips all of the
//! above (a panicking poller, an aborted task), `PortForward`'s [`Drop`] aborts
//! the accept loop, which drops the socket.

pub mod proc_net;
pub mod tunnel;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde::Serialize;
use tauri::{AppHandle, Emitter};
use tokio::sync::{watch, Mutex};
use tokio::task::JoinHandle;

use crate::docker::container::is_container_running;
use crate::docker::exec::{exec_oneshot_limited, PROC_NET_OUTPUT_LIMIT};
use crate::storage::projects_store::ProjectsStore;

use proc_net::PortFamily;
use tunnel::PortForward;

/// How often the container is polled for new/vanished loopback listeners.
/// Short enough that a login redirect isn't left waiting, cheap enough to run
/// continuously (one `cat` of two procfs files per tick).
const POLL_INTERVAL: Duration = Duration::from_secs(2);

/// Emitted whenever the bridged-port set (or the conflict set) changes.
/// Payload: `{ project_id, status: AuthBridgeStatus }`.
const AUTH_BRIDGE_EVENT: &str = "auth-bridge-changed";

// ─────────────────────────────────────────────────────────────────────────────
// IPC response models
// ─────────────────────────────────────────────────────────────────────────────

/// A port currently bound on the host loopback and forwarded into the container.
#[derive(Debug, Clone, Serialize)]
pub struct BridgedPort {
    pub port: u16,
    pub family: PortFamily,
    /// RFC 3339 timestamp of when the host listener was bound.
    pub bridged_at: String,
}

/// A loopback listener that was discovered but could not be bridged.
#[derive(Debug, Clone, Serialize)]
pub struct PortConflict {
    pub port: u16,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct AuthBridgeStatus {
    pub enabled: bool,
    pub active_ports: Vec<BridgedPort>,
    pub conflicts: Vec<PortConflict>,
}

impl AuthBridgeStatus {
    fn disabled() -> Self {
        Self {
            enabled: false,
            active_ports: Vec::new(),
            conflicts: Vec::new(),
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Manager
// ─────────────────────────────────────────────────────────────────────────────

/// Everything the poller owns for one project. Live ports and conflicts sit
/// behind an `Arc<Mutex<…>>` so `get_auth_bridge_status` can read them without
/// disturbing the poller.
#[derive(Default)]
struct BridgeState {
    forwards: BTreeMap<u16, PortForward>,
    conflicts: BTreeMap<u16, String>,
}

impl BridgeState {
    fn snapshot(&self, enabled: bool) -> AuthBridgeStatus {
        AuthBridgeStatus {
            enabled,
            active_ports: self
                .forwards
                .values()
                .map(|f| BridgedPort {
                    port: f.port,
                    family: f.family,
                    bridged_at: f.bridged_at.clone(),
                })
                .collect(),
            conflicts: self
                .conflicts
                .iter()
                .map(|(port, reason)| PortConflict {
                    port: *port,
                    reason: reason.clone(),
                })
                .collect(),
        }
    }
}

struct ProjectBridge {
    /// Distinguishes this poller from a later one for the same project, so a
    /// poller that exits late can't remove its replacement's map entry.
    epoch: u64,
    cancel: watch::Sender<bool>,
    state: Arc<Mutex<BridgeState>>,
    poller: JoinHandle<()>,
}

type BridgeMap = Arc<Mutex<HashMap<String, ProjectBridge>>>;

#[derive(Default)]
pub struct AuthBridgeManager {
    bridges: BridgeMap,
    next_epoch: AtomicU64,
}

impl AuthBridgeManager {
    pub fn new() -> Self {
        Self::default()
    }

    /// Start polling for `project_id`. Idempotent: a call while a live poller
    /// already exists for the project is a no-op.
    pub async fn start(
        &self,
        project_id: String,
        container_id: String,
        app: AppHandle,
        store: Arc<ProjectsStore>,
    ) {
        let mut map = self.bridges.lock().await;

        // A finished poller has already torn its ports down, so its entry is
        // just a husk and can be replaced. A live one means we're already on.
        if map
            .get(&project_id)
            .is_some_and(|b| !b.poller.is_finished())
        {
            return;
        }

        let epoch = self.next_epoch.fetch_add(1, Ordering::Relaxed);
        let state = Arc::new(Mutex::new(BridgeState::default()));
        let (cancel_tx, cancel_rx) = watch::channel(false);

        log::info!(
            "Auth bridge: starting for project {} (container {})",
            project_id,
            &container_id[..container_id.len().min(12)]
        );

        let poller = tokio::spawn(poll_loop(
            project_id.clone(),
            container_id,
            epoch,
            app,
            store,
            state.clone(),
            self.bridges.clone(),
            cancel_rx,
        ));

        map.insert(
            project_id,
            ProjectBridge {
                epoch,
                cancel: cancel_tx,
                state,
                poller,
            },
        );
    }

    /// Stop the bridge for one project and wait until every host port it held
    /// has been released.
    pub async fn stop(&self, project_id: &str) {
        // Remove under the lock, then release it before awaiting: the poller
        // takes the same lock to deregister itself on exit.
        let bridge = self.bridges.lock().await.remove(project_id);
        if let Some(bridge) = bridge {
            let _ = bridge.cancel.send(true);
            let _ = bridge.poller.await;
            log::info!("Auth bridge: stopped for project {}", project_id);
        }
    }

    /// Stop every bridge. Used on app exit.
    pub async fn stop_all(&self) {
        let bridges: Vec<(String, ProjectBridge)> =
            self.bridges.lock().await.drain().collect();
        for (project_id, bridge) in bridges {
            let _ = bridge.cancel.send(true);
            let _ = bridge.poller.await;
            log::info!("Auth bridge: stopped for project {}", project_id);
        }
    }

    /// Current status. `enabled` comes from the persisted project record, so a
    /// project whose bridge is on but whose container is stopped still reports
    /// `enabled: true` with no active ports.
    pub async fn status(&self, project_id: &str, enabled: bool) -> AuthBridgeStatus {
        // Clone the per-project handle out and drop the map lock before taking
        // the state lock. Holding both across the nested await is not a
        // deadlock — the order is consistently bridges→state — but it puts a
        // cheap UI status call behind whatever the poller is doing under
        // `state`, and behind every other project's status call too.
        let state = self.bridges.lock().await.get(project_id).map(|b| b.state.clone());
        match state {
            Some(state) => state.lock().await.snapshot(enabled),
            None => AuthBridgeStatus {
                enabled,
                ..AuthBridgeStatus::disabled()
            },
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Poller
// ─────────────────────────────────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
async fn poll_loop(
    project_id: String,
    container_id: String,
    epoch: u64,
    app: AppHandle,
    store: Arc<ProjectsStore>,
    state: Arc<Mutex<BridgeState>>,
    bridges: BridgeMap,
    mut cancel: watch::Receiver<bool>,
) {
    let mut exec_failures: u32 = 0;

    loop {
        // Stop conditions checked every tick, so the bridge winds itself down
        // even when nothing calls `stop()` (container died, project deleted
        // out from under us, flag flipped off by another path).
        let project = match store.get(&project_id) {
            Some(p) => p,
            None => {
                log::info!("Auth bridge: project {} is gone — tearing down", project_id);
                break;
            }
        };
        if !project.auth_bridge_enabled {
            log::info!("Auth bridge: disabled for project {} — tearing down", project_id);
            break;
        }
        if !is_container_running(&container_id).await.unwrap_or(false) {
            log::info!(
                "Auth bridge: container for project {} is no longer running — tearing down",
                project_id
            );
            break;
        }

        // One exec per tick reads both procfs files.
        //
        // Absolute path, deliberately: the image's `ENV PATH` puts a
        // container-writable directory first, so a bare `cat` is a name the
        // container can rebind to a shim that prints whatever it likes. It
        // still could not make us bind a *non-loopback* port, but it decides
        // how much output this loop ingests and how many host ports it is asked
        // for, which is why the call is also length-capped and the result
        // count is capped in `reconcile`.
        let cmd = vec![
            "/usr/bin/cat".to_string(),
            "/proc/net/tcp".to_string(),
            "/proc/net/tcp6".to_string(),
        ];
        // Cancellation races the exec, not just the sleep, so disabling the
        // bridge or stopping the container doesn't wait out an in-flight poll.
        let discovery = tokio::select! {
            _ = cancel.changed() => break,
            res = exec_oneshot_limited(&container_id, cmd, PROC_NET_OUTPUT_LIMIT) => res,
        };

        match discovery {
            Ok(text) => {
                exec_failures = 0;
                let discovered = proc_net::parse_loopback_listeners(&text);
                let skip = skipped_ports(&project);
                if reconcile(&container_id, &discovered, &skip, &state).await {
                    emit_status(&app, &project_id, &state, true).await;
                }
            }
            Err(e) => {
                exec_failures += 1;
                // Transient failures happen (container restarting, engine busy);
                // only complain once per streak.
                if exec_failures == 1 {
                    log::warn!(
                        "Auth bridge: failed to read /proc/net/tcp in container for project {}: {}",
                        project_id,
                        e
                    );
                }
            }
        }

        tokio::select! {
            _ = cancel.changed() => break,
            _ = tokio::time::sleep(POLL_INTERVAL) => {}
        }
    }

    teardown(&project_id, &state).await;
    emit_status(
        &app,
        &project_id,
        &state,
        store
            .get(&project_id)
            .is_some_and(|p| p.auth_bridge_enabled),
    )
    .await;

    // Deregister, unless a newer poller has already taken this project's slot.
    let mut map = bridges.lock().await;
    if map.get(&project_id).is_some_and(|b| b.epoch == epoch) {
        map.remove(&project_id);
    }
}

/// Ports Docker already handles for this project. A container port that is
/// explicitly published has a host-side path already, and the mapping's host
/// port is a binding we must not fight over.
///
/// [`RESERVED_CONTAINER_PORTS`] is folded in as well: those are container
/// loopback listeners another feature owns and exposes on its own,
/// authenticated terms.
fn skipped_ports(project: &crate::models::Project) -> HashSet<u16> {
    let mut skip: HashSet<u16> = project
        .port_mappings
        .iter()
        .flat_map(|m| [m.container_port, m.host_port])
        .collect();
    skip.extend(RESERVED_CONTAINER_PORTS.clone());
    skip.extend(RESERVED_HOST_PORTS.clone());
    skip
}

// ─────────────────────────────────────────────────────────────────────────────
// Reservations
// ─────────────────────────────────────────────────────────────────────────────

/// Container loopback ports another feature owns, which the bridge must leave
/// alone.
///
/// The bridge's contract is "mirror every container loopback listener onto the
/// same host port, **unauthenticated**" — correct for the throwaway OAuth
/// callback listeners it exists for, wrong for anything sensitive. The
/// browser-view pane runs Playwright's dashboard on a container loopback port
/// in this range and puts a token-gated listener in front of it; mirroring that
/// port here would quietly publish an ungated second door to full control of a
/// browser inside the container.
///
/// This is a constant rather than a registry the pane populates at runtime, and
/// that is the point: Playwright's dashboard is a detached daemon that outlives
/// the app, so after a crash an orphaned viewer can still be listening with
/// nothing in this process left to remember it. A static range is the only form
/// of the rule that survives a restart. It must stay in step with
/// `browser_view::VIEWER_PORTS`, which asserts on it.
pub const RESERVED_CONTAINER_PORTS: std::ops::RangeInclusive<u16> = 39321..=39328;

/// Host ports another feature binds on demand, which the bridge must not take
/// first.
///
/// These are the browser-view proxy's host ports. The bridge binds *host* ports
/// named by the container, so a container listening on 47820 would have the
/// bridge take the host side of that number — and then the browser-view pane,
/// which only binds when the user opens it, finds its port gone. The two ranges
/// are separate constants because they guard opposite ends of the same
/// mechanism: [`RESERVED_CONTAINER_PORTS`] is about not *publishing* something,
/// this one is about not *stealing* something.
pub const RESERVED_HOST_PORTS: std::ops::RangeInclusive<u16> =
    crate::browser_view::proxy::PROXY_PORTS;

/// Most host ports the bridge will hold for one project at a time.
///
/// The discovery input is entirely container-controlled, and each
/// [`PortForward`] costs two listeners plus a task, so without a cap a
/// container that reports tens of thousands of fake listeners exhausts the
/// app's file descriptors and the host's ephemeral ports in a single tick. A
/// real login flow uses one or two ports at a time; anything past a couple of
/// dozen is not a login.
const MAX_FORWARDS: usize = 24;

/// Most conflicts recorded at once, so a flood of unbindable ports can't grow
/// the status payload (and the UI list) without bound either.
const MAX_CONFLICTS: usize = 32;

/// Bring the set of host listeners in line with what the container is currently
/// listening on. Returns whether anything the UI cares about changed.
async fn reconcile(
    container_id: &str,
    discovered: &BTreeMap<u16, PortFamily>,
    skip: &HashSet<u16>,
    state: &Arc<Mutex<BridgeState>>,
) -> bool {
    let mut changed = false;
    let mut st = state.lock().await;

    // Drop host listeners whose container-side counterpart vanished, became
    // covered by an explicit port mapping, or changed address family (a family
    // change alters the socat target, so it has to be rebound below).
    let stale: Vec<u16> = st
        .forwards
        .iter()
        .filter(|(port, forward)| match discovered.get(port) {
            None => true,
            Some(_) if skip.contains(port) => true,
            Some(family) => *family != forward.family,
        })
        .map(|(port, _)| *port)
        .collect();
    for port in stale {
        if let Some(mut forward) = st.forwards.remove(&port) {
            forward.shutdown().await;
            log::info!("Auth bridge: released host port {}", port);
            changed = true;
        }
    }

    // Forget conflicts for ports that are no longer relevant.
    let before = st.conflicts.len();
    st.conflicts
        .retain(|port, _| discovered.contains_key(port) && !skip.contains(port));
    changed |= st.conflicts.len() != before;

    for (&port, &family) in discovered {
        if skip.contains(&port) || st.forwards.contains_key(&port) {
            continue;
        }
        if st.forwards.len() >= MAX_FORWARDS {
            // Don't even attempt the bind: the point of the cap is to stop the
            // container dictating how many host resources we take.
            changed |= note_conflict(
                &mut st,
                port,
                format!(
                    "The auth bridge is already holding {} ports for this project; \
                     {} was not bridged.",
                    MAX_FORWARDS, port
                ),
            );
            continue;
        }
        match PortForward::bind(container_id.to_string(), port, family).await {
            Ok(forward) => {
                if st.conflicts.remove(&port).is_some() {
                    log::info!("Auth bridge: host port {} became available", port);
                }
                log::info!(
                    "Auth bridge: bridging 127.0.0.1:{} → container {} ({:?})",
                    port,
                    family.socat_target(port),
                    family
                );
                st.forwards.insert(port, forward);
                changed = true;
            }
            Err(e) => {
                // Conflict policy: never fight for a port. Something else on the
                // host owns it — another project's bridge, or an unrelated
                // process. Skip it, record why so the UI can say so, and retry
                // on later ticks in case the owner releases it. Warn only on
                // the transition so a long-lived conflict doesn't spam the log.
                let reason = format!(
                    "Host port {} is already in use ({}); not bridged.",
                    port, e
                );
                if st.conflicts.get(&port) != Some(&reason) {
                    log::warn!("Auth bridge: {}", reason);
                }
                changed |= note_conflict(&mut st, port, reason);
            }
        }
    }

    changed
}

/// Record why a port wasn't bridged, up to [`MAX_CONFLICTS`]. Returns whether
/// the recorded set changed.
fn note_conflict(state: &mut BridgeState, port: u16, reason: String) -> bool {
    match state.conflicts.get(&port) {
        Some(existing) if *existing == reason => false,
        Some(_) => {
            state.conflicts.insert(port, reason);
            true
        }
        None if state.conflicts.len() < MAX_CONFLICTS => {
            state.conflicts.insert(port, reason);
            true
        }
        None => false,
    }
}

/// Release every host port held for this project. Awaits each shutdown, so on
/// return nothing is bound.
async fn teardown(project_id: &str, state: &Arc<Mutex<BridgeState>>) {
    let mut st = state.lock().await;
    let forwards = std::mem::take(&mut st.forwards);
    st.conflicts.clear();
    let count = forwards.len();
    for (_, mut forward) in forwards {
        forward.shutdown().await;
    }
    if count > 0 {
        log::info!(
            "Auth bridge: released {} host port(s) for project {}",
            count,
            project_id
        );
    }
}

async fn emit_status(
    app: &AppHandle,
    project_id: &str,
    state: &Arc<Mutex<BridgeState>>,
    enabled: bool,
) {
    let status = state.lock().await.snapshot(enabled);
    let _ = app.emit(
        AUTH_BRIDGE_EVENT,
        serde_json::json!({
            "project_id": project_id,
            "status": status,
        }),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{PortMapping, Project, ProjectPath};

    fn project_with_mappings(mappings: Vec<(u16, u16)>) -> Project {
        let mut p = Project::new(
            "test".to_string(),
            vec![ProjectPath {
                host_path: "/tmp".to_string(),
                mount_name: "tmp".to_string(),
            }],
        );
        p.port_mappings = mappings
            .into_iter()
            .map(|(host_port, container_port)| PortMapping {
                host_port,
                container_port,
                protocol: "tcp".to_string(),
            })
            .collect();
        p
    }

    #[test]
    fn ports_already_published_by_docker_are_skipped() {
        let skip = skipped_ports(&project_with_mappings(vec![(3000, 3000), (8081, 8080)]));
        assert!(skip.contains(&3000));
        // Both ends of an asymmetric mapping are off limits: the container port
        // is already reachable, and the host port is Docker's binding.
        assert!(skip.contains(&8080));
        assert!(skip.contains(&8081));
        assert!(!skip.contains(&34567));
    }

    #[test]
    fn no_mappings_means_nothing_but_the_reserved_ranges_are_skipped() {
        let skip = skipped_ports(&project_with_mappings(vec![]));
        assert_eq!(
            skip.len(),
            RESERVED_CONTAINER_PORTS.clone().count() + RESERVED_HOST_PORTS.clone().count()
        );
    }

    #[test]
    fn the_browser_views_host_ports_are_never_taken() {
        // The bridge binds *host* ports chosen by the container, so without
        // this it can take the port the browser-view proxy will want later —
        // that pane binds on demand, so first-come would win.
        let skip = skipped_ports(&project_with_mappings(vec![]));
        for port in RESERVED_HOST_PORTS {
            assert!(skip.contains(&port), "host port {} should be reserved", port);
        }
        assert!(!skip.contains(&(RESERVED_HOST_PORTS.end() + 1)));
    }

    #[test]
    fn conflicts_stop_being_recorded_past_the_cap() {
        let mut st = BridgeState::default();
        for port in 1000u16..1000 + MAX_CONFLICTS as u16 {
            assert!(note_conflict(&mut st, port, "busy".to_string()));
        }
        // Past the cap: new ports are dropped rather than growing the status
        // payload the UI renders.
        assert!(!note_conflict(&mut st, 9999, "busy".to_string()));
        assert_eq!(st.conflicts.len(), MAX_CONFLICTS);
        // A changed reason for a port already tracked still updates.
        assert!(!note_conflict(&mut st, 1000, "busy".to_string()));
        assert!(note_conflict(&mut st, 1000, "different".to_string()));
        assert_eq!(st.conflicts.len(), MAX_CONFLICTS);
    }

    #[tokio::test]
    async fn the_host_ports_one_container_can_demand_are_capped() {
        // The container fully controls the discovery input (it can shim the
        // probe command), and each forward costs two listeners plus a task —
        // uncapped, one tick could exhaust the app's fds and the host's
        // ephemeral ports.
        let discovered: BTreeMap<u16, PortFamily> =
            (45000u16..45200).map(|p| (p, PortFamily::V4)).collect();
        let state = Arc::new(Mutex::new(BridgeState::default()));

        reconcile("no-such-container", &discovered, &HashSet::new(), &state).await;

        let mut st = state.lock().await;
        assert!(
            st.forwards.len() <= MAX_FORWARDS,
            "bridged {} ports, cap is {}",
            st.forwards.len(),
            MAX_FORWARDS
        );
        assert!(st.conflicts.len() <= MAX_CONFLICTS);
        // Nowhere near the 200 the "container" asked for.
        assert!(st.forwards.len() + st.conflicts.len() < discovered.len());

        for (_, mut forward) in std::mem::take(&mut st.forwards) {
            forward.shutdown().await;
        }
    }

    #[test]
    fn the_browser_views_ports_are_never_mirrored() {
        // Mirroring these would publish an ungated second door to the
        // Playwright dashboard, which the pane deliberately keeps behind a
        // token-checking listener.
        let skip = skipped_ports(&project_with_mappings(vec![]));
        for port in RESERVED_CONTAINER_PORTS {
            assert!(skip.contains(&port), "port {} should be reserved", port);
        }
        assert!(!skip.contains(&(RESERVED_CONTAINER_PORTS.end() + 1)));

        // Reservations coexist with Docker's own published ports.
        let skip = skipped_ports(&project_with_mappings(vec![(3000, 3000)]));
        assert!(skip.contains(RESERVED_CONTAINER_PORTS.start()));
        assert!(skip.contains(&3000));
    }
}
