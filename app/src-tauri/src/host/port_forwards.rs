//! A project's port mappings on a host with no Docker port publishing (AWS).
//!
//! Docker publishes a mapping itself (`0.0.0.0:<host>` → the container). A
//! Fargate task has no such thing, so each TCP mapping becomes a host
//! **loopback** listener (`127.0.0.1` and `::1`, never a wildcard) carried
//! into the task over `socat` on an attached exec — the auth bridge's
//! [`PortForward`], which works through the agent like any other exec
//! (constraints Q6). Nothing is published beyond this machine, and the UI
//! says so.
//!
//! One set per project, opened after every connect (the start, and the
//! background re-attach after an app restart) and replacing the set before
//! it. It is closed with the project's tunnel — `tunnels::close`,
//! `close_if_task` (the task died) and `close_all` (app exit) all call
//! [`close`] / [`close_all`] — so Stop, Remove, a task death and exit need
//! nothing of their own.
//!
//! An open listener costs the workspace nothing: no agent request is made
//! until a client connects, so it never counts as activity for the idle
//! stop. A connection is an attach, which counts only while bytes flow.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

use crate::auth_bridge::proc_net::PortFamily;
use crate::auth_bridge::tunnel::PortForward;
use crate::auth_bridge::{RESERVED_CONTAINER_PORTS, RESERVED_HOST_PORTS};
use crate::models::PortMapping;

use super::ContainerTarget;

/// What became of one mapping: `error` is `None` when its forward is open.
#[derive(Debug, Clone, PartialEq)]
pub struct PortForwardResult {
    pub mapping: PortMapping,
    pub error: Option<String>,
}

fn registry() -> &'static Mutex<HashMap<String, Vec<PortForward>>> {
    static R: std::sync::OnceLock<Mutex<HashMap<String, Vec<PortForward>>>> =
        std::sync::OnceLock::new();
    R.get_or_init(Default::default)
}

fn lock() -> std::sync::MutexGuard<'static, HashMap<String, Vec<PortForward>>> {
    registry().lock().unwrap_or_else(|e| e.into_inner())
}

/// Why one mapping can't be forwarded to an AWS workspace, if it can't —
/// regardless of what else is configured. `None` when it is fine.
fn mapping_problem(m: &PortMapping) -> Option<String> {
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

/// An AWS project's port mappings, checked on save: TCP only, no reserved
/// port (the browser view's two ranges, the agent's port), and no host port
/// twice. The first problem, worded for the user.
pub fn validate_aws_port_mappings(mappings: &[PortMapping]) -> Result<(), String> {
    let mut seen = HashSet::new();
    for m in mappings {
        if incomplete(m) {
            continue;
        }
        if let Some(problem) = mapping_problem(m) {
            return Err(problem);
        }
        if !seen.insert(m.host_port) {
            return Err(format!("Host port {} is mapped twice", m.host_port));
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

/// Open `mappings` for `project_id` into `target` (its connected workspace),
/// replacing the set it had. One result per mapping, in order: a busy host
/// port, a host port mapped twice or a refused mapping is an error for that
/// mapping only, and the others still open.
///
/// If the project's tunnel is no longer `target` once they are bound (a Stop
/// or a newer connect raced this), nothing is kept.
pub async fn open_for(
    project_id: &str,
    target: &ContainerTarget,
    mappings: &[PortMapping],
) -> Vec<PortForwardResult> {
    // The old set first, so a re-attach can bind the same host ports again.
    close(project_id).await;

    let mut opened = Vec::new();
    let mut results = Vec::new();
    let mut taken = HashSet::new();
    for m in mappings {
        let error = if incomplete(m) {
            continue;
        } else if let Some(problem) = mapping_problem(m) {
            Some(problem)
        } else if !taken.insert(m.host_port) {
            Some(format!(
                "{} → {}: host port {} is already used by another mapping",
                m.host_port, m.container_port, m.host_port
            ))
        } else {
            match PortForward::bind_mapping(target.clone(), m.host_port, m.container_port, PortFamily::V4)
                .await
            {
                Ok(fwd) => {
                    if let Some(w) = &fwd.ipv6_warning {
                        log::warn!("Project {project_id}: port mapping {}: {w}", m.host_port);
                    }
                    log::info!(
                        "Project {project_id}: forwarding localhost:{} → workspace port {}",
                        m.host_port,
                        m.container_port
                    );
                    opened.push(fwd);
                    None
                }
                Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => Some(format!(
                    "{} → {}: host port {} is already in use on this machine",
                    m.host_port, m.container_port, m.host_port
                )),
                Err(e) => Some(format!(
                    "{} → {}: could not listen on host port {}: {e}",
                    m.host_port, m.container_port, m.host_port
                )),
            }
        };
        results.push(PortForwardResult { mapping: m.clone(), error });
    }

    let previous = lock().insert(project_id.to_string(), opened);
    shutdown_all(previous.unwrap_or_default()).await;
    // Checked *after* the set is registered: a tunnel close that ran before
    // this sees it gone here; one that runs after removes it itself.
    if !still_connected(project_id, target) {
        close(project_id).await;
        for r in results.iter_mut().filter(|r| r.error.is_none()) {
            r.error = Some("the workspace disconnected".into());
        }
    }
    results
}

async fn shutdown_all(set: Vec<PortForward>) {
    futures_util::future::join_all(set.into_iter().map(|mut f| async move { f.shutdown().await }))
        .await;
}

/// Close the project's forwards (its ports are free when this returns).
pub async fn close(project_id: &str) {
    let set = lock().remove(project_id);
    if let Some(set) = set {
        shutdown_all(set).await;
    }
}

/// Close every project's forwards. The shutdown path's step (via
/// `tunnels::close_all`).
pub async fn close_all() {
    let all: Vec<PortForward> = lock().drain().flat_map(|(_, v)| v).collect();
    shutdown_all(all).await;
}

/// After a connect (or a save of the mappings while connected): open an
/// AWS project's mappings into its connected workspace, replacing the set it
/// had. The warning detail for the mappings that failed, if any did; `None`
/// as well for a local project (Docker publishes its own) or a workspace
/// that is not (or no longer) connected — nothing to warn about there.
pub async fn open_for_project(project: &crate::models::Project) -> Option<String> {
    if !project.is_aws() {
        return None;
    }
    let target = ContainerTarget::Agent(super::tunnels::get_endpoint(&project.id)?);
    let results = open_for(&project.id, &target, &project.port_mappings).await;
    if !still_connected(&project.id, &target) {
        return None;
    }
    failure_detail(&results)
}

/// The host ports open for the project.
#[cfg(test)]
pub fn open_ports(project_id: &str) -> Vec<u16> {
    lock().get(project_id).map(|v| v.iter().map(|f| f.port).collect()).unwrap_or_default()
}

/// The `workspace-warning` detail for the mappings that failed, if any did.
pub fn failure_detail(results: &[PortForwardResult]) -> Option<String> {
    let lines: Vec<&str> = results.iter().filter_map(|r| r.error.as_deref()).collect();
    (!lines.is_empty()).then(|| lines.join("\n"))
}

#[cfg(test)]
#[path = "port_forwards/tests.rs"]
mod tests;
