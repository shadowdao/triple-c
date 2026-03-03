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
    let docker = get_docker()?;
    match docker.ping().await {
        Ok(_) => Ok(true),
        Err(_) => {
            // Connection object exists but daemon not responding — clear cache
            let mut guard = DOCKER.lock().map_err(|e| format!("Lock poisoned: {}", e))?;
            *guard = None;
            Ok(false)
        }
    }
}
