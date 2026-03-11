use bollard::network::{CreateNetworkOptions, InspectNetworkOptions};
use std::collections::HashMap;

use super::client::get_docker;

/// Network name for a project's MCP containers.
fn project_network_name(project_id: &str) -> String {
    format!("triple-c-net-{}", project_id)
}

/// Ensure a Docker bridge network exists for the project.
/// Returns the network name.
pub async fn ensure_project_network(project_id: &str) -> Result<String, String> {
    let docker = get_docker()?;
    let network_name = project_network_name(project_id);

    // Check if network already exists
    match docker
        .inspect_network(&network_name, None::<InspectNetworkOptions<String>>)
        .await
    {
        Ok(_) => {
            log::debug!("Network {} already exists", network_name);
            return Ok(network_name);
        }
        Err(_) => {
            // Network doesn't exist, create it
        }
    }

    let options = CreateNetworkOptions {
        name: network_name.clone(),
        driver: "bridge".to_string(),
        labels: HashMap::from([
            ("triple-c.managed".to_string(), "true".to_string()),
            ("triple-c.project-id".to_string(), project_id.to_string()),
        ]),
        ..Default::default()
    };

    docker
        .create_network(options)
        .await
        .map_err(|e| format!("Failed to create network {}: {}", network_name, e))?;

    log::info!("Created Docker network {}", network_name);
    Ok(network_name)
}

/// Connect a container to the project network.
#[allow(dead_code)]
pub async fn connect_container_to_network(
    container_id: &str,
    network_name: &str,
) -> Result<(), String> {
    let docker = get_docker()?;

    let config = bollard::network::ConnectNetworkOptions {
        container: container_id.to_string(),
        ..Default::default()
    };

    docker
        .connect_network(network_name, config)
        .await
        .map_err(|e| {
            format!(
                "Failed to connect container {} to network {}: {}",
                container_id, network_name, e
            )
        })?;

    log::debug!(
        "Connected container {} to network {}",
        container_id,
        network_name
    );
    Ok(())
}

/// Remove the project network (best-effort). Disconnects all containers first.
pub async fn remove_project_network(project_id: &str) -> Result<(), String> {
    let docker = get_docker()?;
    let network_name = project_network_name(project_id);

    // Inspect to get connected containers
    let info = match docker
        .inspect_network(&network_name, None::<InspectNetworkOptions<String>>)
        .await
    {
        Ok(info) => info,
        Err(_) => {
            log::debug!(
                "Network {} not found, nothing to remove",
                network_name
            );
            return Ok(());
        }
    };

    // Disconnect all containers
    if let Some(containers) = info.containers {
        for (container_id, _) in containers {
            let disconnect_opts = bollard::network::DisconnectNetworkOptions {
                container: container_id.clone(),
                force: true,
            };
            if let Err(e) = docker
                .disconnect_network(&network_name, disconnect_opts)
                .await
            {
                log::warn!(
                    "Failed to disconnect container {} from network {}: {}",
                    container_id,
                    network_name,
                    e
                );
            }
        }
    }

    // Remove the network
    match docker.remove_network(&network_name).await {
        Ok(_) => log::info!("Removed Docker network {}", network_name),
        Err(e) => log::warn!("Failed to remove network {}: {}", network_name, e),
    }

    Ok(())
}
