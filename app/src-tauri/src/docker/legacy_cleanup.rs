//! One-release migration shim for the removed built-in MCP feature.
//!
//! Older releases created a per-project user-defined bridge network
//! (`triple-c-net-<projectId>`) plus one container per Docker-backed MCP
//! server, and attached the project container to that network. Now that MCP
//! support is gone, those leftovers have to be torn down — a container whose
//! `NetworkMode` names a network that no longer exists refuses to start, so
//! the cleanup is paired with a forced container recreation (see
//! `container_needs_recreation`).
//!
//! Everything here is best-effort: failures are logged and never abort the
//! caller, and absent resources are a silent no-op. This module can be deleted
//! a release after all users have migrated.

use bollard::container::{ListContainersOptions, RemoveContainerOptions};
use bollard::network::InspectNetworkOptions;
use std::collections::HashMap;

use super::client::get_docker;

/// Network name used by the old MCP implementation for a project.
fn legacy_network_name(project_id: &str) -> String {
    format!("triple-c-net-{}", project_id)
}

/// Force-remove every leftover MCP server container.
///
/// Matched by the `triple-c.mcp-server` label rather than by name, so
/// containers survive even if the MCP server definitions they came from are
/// already gone from storage. Best-effort: errors are logged and skipped.
pub async fn remove_legacy_mcp_containers(project_id: &str) {
    let docker = match get_docker() {
        Ok(d) => d,
        Err(e) => {
            log::debug!(
                "Skipping legacy MCP container cleanup for project {}: {}",
                project_id,
                e
            );
            return;
        }
    };

    let filters: HashMap<String, Vec<String>> = HashMap::from([(
        "label".to_string(),
        vec!["triple-c.mcp-server".to_string()],
    )]);

    let containers = match docker
        .list_containers(Some(ListContainersOptions {
            all: true,
            filters,
            ..Default::default()
        }))
        .await
    {
        Ok(c) => c,
        Err(e) => {
            log::warn!("Failed to list legacy MCP containers: {}", e);
            return;
        }
    };

    for container in containers {
        let Some(id) = container.id else { continue };
        match docker
            .remove_container(
                &id,
                Some(RemoveContainerOptions {
                    force: true,
                    ..Default::default()
                }),
            )
            .await
        {
            Ok(_) => log::info!("Removed legacy MCP container {}", id),
            Err(e) => log::warn!("Failed to remove legacy MCP container {}: {}", id, e),
        }
    }
}

/// Remove the old per-project Docker network, disconnecting any remaining
/// members first (a network with attached endpoints cannot be deleted).
///
/// Silent no-op when the network does not exist. Best-effort: errors are
/// logged and never propagated.
pub async fn remove_legacy_project_network(project_id: &str) {
    let docker = match get_docker() {
        Ok(d) => d,
        Err(e) => {
            log::debug!(
                "Skipping legacy network cleanup for project {}: {}",
                project_id,
                e
            );
            return;
        }
    };
    let network_name = legacy_network_name(project_id);

    // Inspect to discover connected containers; absence means nothing to do.
    let info = match docker
        .inspect_network(&network_name, None::<InspectNetworkOptions<String>>)
        .await
    {
        Ok(info) => info,
        Err(_) => {
            log::debug!("Legacy network {} not present, nothing to do", network_name);
            return;
        }
    };

    if let Some(containers) = info.containers {
        for container_id in containers.into_keys() {
            let disconnect_opts = bollard::network::DisconnectNetworkOptions {
                container: container_id.clone(),
                force: true,
            };
            if let Err(e) = docker
                .disconnect_network(&network_name, disconnect_opts)
                .await
            {
                log::warn!(
                    "Failed to disconnect container {} from legacy network {}: {}",
                    container_id,
                    network_name,
                    e
                );
            }
        }
    }

    match docker.remove_network(&network_name).await {
        Ok(_) => log::info!("Removed legacy Docker network {}", network_name),
        Err(e) => log::warn!("Failed to remove legacy network {}: {}", network_name, e),
    }
}
