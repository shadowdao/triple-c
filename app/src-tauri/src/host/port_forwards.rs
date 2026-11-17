//! A project's port mappings on a host with no Docker port publishing (AWS).
//!
//! Docker publishes a mapping itself (`0.0.0.0:<host>` → the container's
//! network interface). A Fargate task has no such thing, so each TCP mapping
//! becomes a host **loopback** listener (`127.0.0.1` and `::1`, never a
//! wildcard) carried into the task over `socat` on an attached exec — the
//! auth bridge's [`PortForward`], which works through the agent like any
//! other exec (constraints Q6). Nothing is published beyond this machine,
//! and the UI says so.
//!
//! ## Two differences from Docker, both deliberate
//!
//! * **It reaches the workspace's loopback, not its network interface**
//!   (`socat` to `127.0.0.1:<port>`, or `[::1]:<port>` when only an IPv6
//!   loopback/wildcard socket listens there — see
//!   [`crate::auth_bridge::tunnel::mapping_command`]). So a mapping can
//!   reach a service that bound loopback precisely to be private — which is
//!   why the agent's port, the browser view's range and (when the project
//!   runs it) the gateway sidecar's [`GATEWAY_SIDECAR_PORT`] are refused.
//! * **No gate** ([`PortForward::bind_mapping`]): unlike the auth bridge, a
//!   client need not speak first and an HTTP request is not checked for a
//!   cross-site origin — Docker's publishing has neither, and a
//!   server-speaks-first protocol (MySQL, SMTP) must work. What a mapping
//!   exposes is what the user chose to expose, to this machine only.
//!
//! The tunnel's bounds still apply: 16 concurrent connections per mapping,
//! and a connection the workspace sends nothing on for 600 s is closed.
//!
//! ## Lifetime
//!
//! One set per project, opened after every connect (the start, and the
//! background re-attach after an app restart) and on a save of the mappings
//! while connected. Opens are serialised per project and re-read the stored
//! mappings under that lock, so the last save wins; an open keeps the
//! forwards it already has for an unchanged mapping (with their live
//! connections) and rebinds everything when the tunnel changed. The set is
//! closed with the project's tunnel — `tunnels::close`, `close_if_task` (the
//! task died) and `close_all` (app exit) call [`close`] / [`close_all`] — so
//! Stop, Remove, a task death and exit need nothing of their own.
//!
//! ## Idle stop
//!
//! An open listener costs the workspace nothing: no agent request is made
//! until a client connects. A connection is an attach, which counts as
//! activity while bytes flow — a client using a mapping keeps the workspace
//! from its idle stop; one connected but silent does not.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use crate::auth_bridge::tunnel::PortForward;
use crate::auth_bridge::{RESERVED_CONTAINER_PORTS, RESERVED_HOST_PORTS};
use crate::models::PortMapping;
use crate::storage::projects_store::ProjectsStore;

use super::ContainerTarget;

/// Where a warning about a mapping goes after the open has returned: the
/// first connection that finds nothing listening in the workspace.
pub type PortsWarn = Arc<dyn Fn(String) + Send + Sync>;

/// What the user is told when a mapping's workspace port has no listener.
pub fn nothing_listening(host_port: u16, container_port: u16) -> String {
    format!(
        "localhost:{host_port}: nothing is listening on port {container_port} in the workspace \
         (127.0.0.1 or ::1)"
    )
}

/// What became of one mapping: `error` is `None` when its forward is open.
#[derive(Debug, Clone, PartialEq)]
pub struct PortForwardResult {
    pub mapping: PortMapping,
    pub error: Option<String>,
}

/// The container port the gateway sidecar (Task 8, `gateway_sidecar`)
/// listens on, on the workspace's loopback.
pub const GATEWAY_SIDECAR_PORT: u16 = 4000;

/// A project's open forwards and the target (tunnel endpoint) they lead to.
struct OpenSet {
    target_id: String,
    forwards: Vec<PortForward>,
}

type Registry = HashMap<String, OpenSet>;

fn registry() -> &'static Mutex<Registry> {
    static R: std::sync::OnceLock<Mutex<Registry>> = std::sync::OnceLock::new();
    R.get_or_init(Default::default)
}

fn lock() -> std::sync::MutexGuard<'static, Registry> {
    registry().lock().unwrap_or_else(|e| e.into_inner())
}

/// The per-project lock every open and close holds, so two opens (a save on
/// every blur, a save during a start's open) never interleave.
fn project_lock(project_id: &str) -> Arc<tokio::sync::Mutex<()>> {
    static L: std::sync::OnceLock<Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>> =
        std::sync::OnceLock::new();
    L.get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .entry(project_id.to_string())
        .or_default()
        .clone()
}

/// Why one mapping can't be forwarded to an AWS workspace, if it can't.
/// `None` when it is fine.
fn mapping_problem(m: &PortMapping, gateway_sidecar: bool) -> Option<String> {
    if !m.protocol.eq_ignore_ascii_case("tcp") {
        return Some(format!(
            "{} → {} ({}): only TCP ports can be forwarded to an AWS workspace",
            m.host_port,
            m.container_port,
            m.protocol.to_ascii_uppercase()
        ));
    }
    if RESERVED_CONTAINER_PORTS.contains(&m.container_port) {
        return Some(format!(
            "Container port {} is reserved for the browser view ({}–{})",
            m.container_port,
            RESERVED_CONTAINER_PORTS.start(),
            RESERVED_CONTAINER_PORTS.end()
        ));
    }
    if m.container_port == super::ssm::AGENT_PORT {
        return Some(format!(
            "Container port {} is the workspace agent's own port",
            super::ssm::AGENT_PORT
        ));
    }
    if gateway_sidecar && m.container_port == GATEWAY_SIDECAR_PORT {
        return Some(format!(
            "Container port {GATEWAY_SIDECAR_PORT} is the gateway sidecar's, which holds the \
             model provider's key"
        ));
    }
    if RESERVED_HOST_PORTS.contains(&m.host_port) {
        return Some(format!(
            "Host port {} is reserved for the browser view ({}–{})",
            m.host_port,
            RESERVED_HOST_PORTS.start(),
            RESERVED_HOST_PORTS.end()
        ));
    }
    None
}

/// A row the editor saved before both its ports were typed (the editor
/// saves on blur): nothing to check or forward yet, and nothing to say.
fn incomplete(m: &PortMapping) -> bool {
    m.host_port == 0 || m.container_port == 0
}

/// An AWS project's port mappings, checked in full: TCP only, no reserved
/// port (the browser view's two ranges, the agent's port, the gateway
/// sidecar's when it runs), and no host port twice. The first problem,
/// worded for the user.
pub fn validate_aws_port_mappings(mappings: &[PortMapping], gateway_sidecar: bool) -> Result<(), String> {
    validate_changed(mappings, gateway_sidecar, |_| true)
}

/// [`validate_aws_port_mappings`] on a save, against what is stored:
/// `stored` is `None` when the project was not on AWS before (everything is
/// checked). Otherwise only rows the save adds or edits are — a stored row
/// this check would refuse (a UDP mapping kept from before) does not block
/// every unrelated save; it is skipped, with a warning, at each connect.
/// Turning the gateway sidecar on checks everything again.
pub fn validate_aws_port_mappings_update(
    stored: Option<(&[PortMapping], bool)>,
    mappings: &[PortMapping],
    gateway_sidecar: bool,
) -> Result<(), String> {
    match stored {
        Some((old, old_gateway)) if old_gateway == gateway_sidecar => {
            validate_changed(mappings, gateway_sidecar, |m| !old.contains(m))
        }
        _ => validate_aws_port_mappings(mappings, gateway_sidecar),
    }
}

fn validate_changed(
    mappings: &[PortMapping],
    gateway_sidecar: bool,
    changed: impl Fn(&PortMapping) -> bool,
) -> Result<(), String> {
    let mut seen: HashMap<u16, bool> = HashMap::new();
    for m in mappings.iter().filter(|m| !incomplete(m)) {
        let is_changed = changed(m);
        if is_changed {
            if let Some(problem) = mapping_problem(m, gateway_sidecar) {
                return Err(problem);
            }
        }
        match seen.get(&m.host_port) {
            Some(&other_changed) if is_changed || other_changed => {
                return Err(format!("Host port {} is mapped twice", m.host_port))
            }
            Some(_) => {}
            None => {
                seen.insert(m.host_port, is_changed);
            }
        }
    }
    Ok(())
}

/// Whether `target` is still the project's connected workspace.
fn still_connected(project_id: &str, target: &ContainerTarget) -> bool {
    match target {
        ContainerTarget::Agent(ep) => {
            super::tunnels::get_endpoint(project_id).is_some_and(|e| e.base_url == ep.base_url)
        }
        ContainerTarget::Docker { .. } => false,
    }
}

/// Open `mappings` for `project_id` into `target` (its connected workspace).
/// See [`open_latest`]; the gateway sidecar is taken as off. Test helper.
#[cfg(test)]
pub async fn open_for(
    project_id: &str,
    target: &ContainerTarget,
    mappings: &[PortMapping],
) -> Vec<PortForwardResult> {
    let mappings = mappings.to_vec();
    open_latest(project_id, target, None, move || Some((mappings, false))).await
}

/// Open the project's mappings into `target`, replacing the set it had.
/// `read` gives the mappings and whether the gateway sidecar runs, and is
/// called **under the project's lock** — so the last of several overlapping
/// opens applies what is stored last. `None` from it closes the set.
///
/// One result per mapping, in order: a refused mapping, a host port mapped
/// twice or a busy host port is an error for that mapping only, and the
/// others still open. A mapping already open into the same target is kept,
/// live connections and all. If the project's tunnel is no longer `target`
/// once they are bound (a Stop or a newer connect raced this), nothing is
/// kept.
pub async fn open_latest(
    project_id: &str,
    target: &ContainerTarget,
    warn: Option<PortsWarn>,
    read: impl FnOnce() -> Option<(Vec<PortMapping>, bool)> + Send,
) -> Vec<PortForwardResult> {
    let plock = project_lock(project_id);
    let _held = plock.lock().await;
    let Some((mappings, gateway_sidecar)) = read() else {
        log::info!("Project {project_id}: no longer an AWS project; closing its port forwards");
        close_unlocked(project_id).await;
        return Vec::new();
    };
    log::info!(
        "Project {project_id}: opening port mappings into {} ({} stored, {} incomplete)",
        target.id(),
        mappings.len(),
        mappings.iter().filter(|m| incomplete(m)).count()
    );

    // What can be forwarded, in order; the first row wins a host port.
    let mut results: Vec<PortForwardResult> = Vec::new();
    let mut wanted: Vec<(usize, u16, u16)> = Vec::new();
    let mut taken = HashSet::new();
    for m in mappings.iter().filter(|m| !incomplete(m)) {
        let error = if let Some(problem) = mapping_problem(m, gateway_sidecar) {
            Some(problem)
        } else if !taken.insert(m.host_port) {
            Some(format!(
                "{} → {}: host port {} is already used by another mapping",
                m.host_port, m.container_port, m.host_port
            ))
        } else {
            wanted.push((results.len(), m.host_port, m.container_port));
            None
        };
        results.push(PortForwardResult { mapping: m.clone(), error });
    }

    // Keep what is already open into this target and still wanted; free
    // every other host port first, so a changed mapping can rebind it.
    let previous = lock().remove(project_id);
    let mut kept: HashMap<(u16, u16), PortForward> = HashMap::new();
    let mut dropped = Vec::new();
    if let Some(prev) = previous {
        let same_target = prev.target_id == target.id();
        for f in prev.forwards {
            let key = (f.port, f.container_port);
            if same_target && wanted.iter().any(|&(_, h, c)| (h, c) == key) {
                kept.insert(key, f);
            } else {
                dropped.push(f);
            }
        }
    }
    shutdown_all(dropped).await;

    let mut opened = Vec::new();
    for (i, host_port, container_port) in wanted {
        if let Some(f) = kept.remove(&(host_port, container_port)) {
            opened.push(f);
            continue;
        }
        let on_refused = warn.clone().map(|w| {
            let notice: crate::auth_bridge::tunnel::RefusedNotice =
                Arc::new(move || w(nothing_listening(host_port, container_port)));
            notice
        });
        match PortForward::bind_mapping(target.clone(), host_port, container_port, on_refused).await {
            Ok(fwd) => {
                if let Some(w) = &fwd.ipv6_warning {
                    log::warn!("Project {project_id}: port mapping {host_port}: {w}");
                }
                log::info!(
                    "Project {project_id}: forwarding localhost:{host_port} → workspace port {container_port}"
                );
                opened.push(fwd);
            }
            Err(e) => {
                results[i].error = Some(if e.kind() == std::io::ErrorKind::AddrInUse {
                    format!(
                        "{host_port} → {container_port}: host port {host_port} is already in use on this machine"
                    )
                } else {
                    format!("{host_port} → {container_port}: could not listen on host port {host_port}: {e}")
                });
            }
        }
    }

    lock().insert(
        project_id.to_string(),
        OpenSet { target_id: target.id().to_string(), forwards: opened },
    );
    // Checked *after* the set is registered: a tunnel close that ran before
    // this sees it gone here; one that runs after removes it itself.
    if !still_connected(project_id, target) {
        log::warn!(
            "Project {project_id}: the workspace's tunnel is no longer {} — closing the port forwards just opened",
            target.id()
        );
        close_unlocked(project_id).await;
        for r in results.iter_mut().filter(|r| r.error.is_none()) {
            r.error = Some("the workspace disconnected".into());
        }
    }
    for r in &results {
        match &r.error {
            None => log::info!(
                "Project {project_id}: port mapping localhost:{} → workspace port {} is open",
                r.mapping.host_port,
                r.mapping.container_port
            ),
            Some(e) => log::warn!("Project {project_id}: port mapping not open: {e}"),
        }
    }
    results
}

async fn shutdown_all(set: Vec<PortForward>) {
    futures_util::future::join_all(set.into_iter().map(|mut f| async move { f.shutdown().await }))
        .await;
}

/// Close the project's forwards (its ports are free when this returns).
/// Waits for an open in progress.
pub async fn close(project_id: &str) {
    let plock = project_lock(project_id);
    let _held = plock.lock().await;
    close_unlocked(project_id).await;
}

async fn close_unlocked(project_id: &str) {
    let set = lock().remove(project_id);
    if let Some(set) = set {
        if !set.forwards.is_empty() {
            log::info!("Project {project_id}: closing {} port forward(s)", set.forwards.len());
        }
        shutdown_all(set.forwards).await;
    }
}

/// Close every project's forwards. The shutdown path's step (via
/// `tunnels::close_all`). An open in progress finds the tunnel gone and
/// closes its own set.
pub async fn close_all() {
    let all: Vec<PortForward> = lock().drain().flat_map(|(_, v)| v.forwards).collect();
    shutdown_all(all).await;
}

/// After a connect (or a save of the mappings while connected): open an
/// AWS project's mappings, **as stored when the open runs**, into its
/// connected workspace. The warning detail for the mappings that failed, if
/// any did; `None` for a local project (Docker publishes its own), and for a
/// workspace that is not (or no longer) connected **unless the project is
/// Running** — mappings that should be open and aren't are never silent.
/// `warn` receives the later "nothing is listening" notices.
pub async fn open_for_project(
    store: &ProjectsStore,
    project_id: &str,
    warn: Option<PortsWarn>,
) -> Option<String> {
    let project = store.get(project_id).filter(|p| p.is_aws())?;
    let wanted = project.port_mappings.iter().filter(|m| !incomplete(m)).count();
    let running = || {
        store.get(project_id).is_some_and(|p| p.status == crate::models::ProjectStatus::Running)
    };
    let Some(endpoint) = super::tunnels::get_endpoint(&project.id) else {
        log::info!("Project {project_id}: no tunnel to the workspace; {wanted} port mapping(s) not opened");
        return (wanted > 0 && running()).then(|| {
            "The workspace is not connected, so its port mappings were not opened. Start the \
             project again."
                .to_string()
        });
    };
    let target = ContainerTarget::Agent(endpoint);
    let results = open_latest(project_id, &target, warn, || {
        store.get(project_id).filter(|p| p.is_aws()).map(|p| {
            let gateway = p.aws.as_ref().is_some_and(|a| a.gateway_sidecar);
            (p.port_mappings, gateway)
        })
    })
    .await;
    if !still_connected(project_id, &target) && !running() {
        return None; // a Stop raced the open: nothing to say
    }
    failure_detail(&results)
}

/// The host ports open for the project.
#[cfg(test)]
pub fn open_ports(project_id: &str) -> Vec<u16> {
    let mut ports: Vec<u16> =
        lock().get(project_id).map(|v| v.forwards.iter().map(|f| f.port).collect()).unwrap_or_default();
    ports.sort_unstable();
    ports
}

/// The `workspace-warning` detail for the mappings that failed, if any did.
pub fn failure_detail(results: &[PortForwardResult]) -> Option<String> {
    let lines: Vec<&str> = results.iter().filter_map(|r| r.error.as_deref()).collect();
    (!lines.is_empty()).then(|| lines.join("\n"))
}

#[cfg(test)]
#[path = "port_forwards/tests.rs"]
mod tests;
