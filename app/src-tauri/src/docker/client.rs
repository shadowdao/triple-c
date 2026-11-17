use bollard::Docker;
use std::sync::Mutex;

static DOCKER: Mutex<Option<Docker>> = Mutex::new(None);

pub fn get_docker() -> Result<Docker, String> {
    let mut guard = DOCKER.lock().map_err(|e| format!("Lock poisoned: {}", e))?;
    if let Some(docker) = guard.as_ref() {
        return Ok(docker.clone());
    }
    let docker = Docker::connect_with_local_defaults()
        .map_err(|e| format!("Failed to connect to Docker daemon: {}", e))?;
    guard.replace(docker.clone());
    Ok(docker)
}

pub async fn check_docker_available() -> Result<bool, String> {
    Ok(docker_status().await.state == super::status::DockerState::Running)
}

/// Ping the daemon and, when it does not answer, say why: not installed, installed
/// but not running, or running but refusing us. See `docker/status.rs`.
pub async fn docker_status() -> super::status::DockerStatus {
    use super::status::{self, PingOutcome};
    let docker_host = std::env::var("DOCKER_HOST").ok();
    let os = status::current_os();
    let endpoint = status::endpoint(docker_host.as_deref());

    let (ping, detail) = match get_docker() {
        Err(e) => (PingOutcome::Unreachable, Some(e)),
        Ok(docker) => match docker.ping().await {
            Ok(_) => (PingOutcome::Ok, None),
            Err(e) => {
                // Connection object exists but daemon not responding — clear cache
                if let Ok(mut guard) = DOCKER.lock() {
                    *guard = None;
                }
                (status::ping_outcome_from_error(&e), Some(e.to_string()))
            }
        },
    };
    let installed = ping == PingOutcome::Unreachable
        && (status::docker_installed(os, docker_host.as_deref())
            // An explicit DOCKER_HOST means the user has a daemon somewhere.
            || docker_host.as_deref().is_some_and(|h| !h.trim().is_empty()));
    status::DockerStatus { state: status::classify(ping, installed), endpoint, detail, os }
}

/// The daemon socket a project container gets bind-mounted when Docker access
/// is enabled and no path is configured.
pub fn default_docker_socket() -> String {
    if cfg!(target_os = "windows") {
        "//./pipe/docker_engine".to_string()
    } else {
        "/var/run/docker.sock".to_string()
    }
}
