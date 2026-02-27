use bollard::Docker;
use std::sync::OnceLock;

static DOCKER: OnceLock<Result<Docker, String>> = OnceLock::new();

pub fn get_docker() -> Result<&'static Docker, String> {
    let result = DOCKER.get_or_init(|| {
        Docker::connect_with_local_defaults()
            .map_err(|e| format!("Failed to connect to Docker daemon: {}", e))
    });
    match result {
        Ok(docker) => Ok(docker),
        Err(e) => Err(e.clone()),
    }
}

pub async fn check_docker_available() -> Result<bool, String> {
    let docker = get_docker()?;
    match docker.ping().await {
        Ok(_) => Ok(true),
        Err(e) => Err(format!("Docker daemon not responding: {}", e)),
    }
}
