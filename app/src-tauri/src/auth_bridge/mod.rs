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
use crate::docker::exec::exec_oneshot;
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
        let map = self.bridges.lock().await;
        match map.get(project_id) {
            Some(bridge) => bridge.state.lock().await.snapshot(enabled),
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
        let cmd = vec![
            "cat".to_string(),
            "/proc/net/tcp".to_string(),
            "/proc/net/tcp6".to_string(),
        ];
        // Cancellation races the exec, not just the sleep, so disabling the
        // bridge or stopping the container doesn't wait out an in-flight poll.
        let discovery = tokio::select! {
            _ = cancel.changed() => break,
            res = exec_oneshot(&container_id, cmd) => res,
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
fn skipped_ports(project: &crate::models::Project) -> HashSet<u16> {
    project
        .port_mappings
        .iter()
        .flat_map(|m| [m.container_port, m.host_port])
        .collect()
}

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
                    st.conflicts.insert(port, reason);
                    changed = true;
                }
            }
        }
    }

    changed
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
    fn no_mappings_means_nothing_is_skipped() {
        assert!(skipped_ports(&project_with_mappings(vec![])).is_empty());
    }
}
