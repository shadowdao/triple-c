use bollard::container::{
    Config, CreateContainerOptions, ListContainersOptions, RemoveContainerOptions,
    StartContainerOptions, StopContainerOptions,
};
use bollard::image::BuildImageOptions;
use bollard::models::{HostConfig, Mount, MountTypeEnum, PortBinding};
use futures_util::StreamExt;
use std::collections::HashMap;
use std::io::Write;

use super::client::get_docker;
use crate::models::app_settings::{SttSettings, SttStatus};

const STT_CONTAINER_NAME: &str = "triple-c-stt";
const STT_MODEL_VOLUME: &str = "triple-c-stt-model-cache";
const STT_REGISTRY_IMAGE: &str = "ghcr.io/shadowdao/triple-c-stt:latest";
const STT_LOCAL_IMAGE: &str = "triple-c-stt:latest";
const STT_DOCKERFILE: &str = include_str!("../../../../stt-container/Dockerfile");
const STT_SERVER: &str = include_str!("../../../../stt-container/server.py");

pub async fn get_stt_status(settings: &SttSettings) -> Result<SttStatus, String> {
    let image_exists = super::image::image_exists(STT_REGISTRY_IMAGE).await.unwrap_or(false)
        || super::image::image_exists(STT_LOCAL_IMAGE).await.unwrap_or(false);

    let (container_exists, running, model) = match find_stt_container().await? {
        Some((_, state, env_model)) => (true, state == "running", env_model),
        None => (false, false, settings.model.clone()),
    };

    Ok(SttStatus {
        container_exists,
        running,
        port: settings.port,
        model,
        image_exists,
    })
}

async fn find_stt_container() -> Result<Option<(String, String, String)>, String> {
    let docker = get_docker()?;

    let filters: HashMap<String, Vec<String>> = HashMap::from([(
        "name".to_string(),
        vec![format!("/{}", STT_CONTAINER_NAME)],
    )]);

    let containers = docker
        .list_containers(Some(ListContainersOptions {
            all: true,
            filters,
            ..Default::default()
        }))
        .await
        .map_err(|e| format!("Failed to list containers: {}", e))?;

    if let Some(container) = containers.first() {
        let id = container.id.clone().unwrap_or_default();
        let state = container.state.clone().unwrap_or_default();

        // Extract WHISPER_MODEL from container env
        let model = container
            .labels
            .as_ref()
            .and_then(|l| l.get("triple-c.stt.model"))
            .cloned()
            .unwrap_or_else(|| "tiny".to_string());

        return Ok(Some((id, state, model)));
    }

    Ok(None)
}

async fn create_stt_container(settings: &SttSettings) -> Result<String, String> {
    let docker = get_docker()?;

    // Try local image first, fall back to registry
    let image = if super::image::image_exists(STT_LOCAL_IMAGE).await.unwrap_or(false) {
        STT_LOCAL_IMAGE.to_string()
    } else if super::image::image_exists(STT_REGISTRY_IMAGE).await.unwrap_or(false) {
        STT_REGISTRY_IMAGE.to_string()
    } else {
        return Err("STT image not found. Please build or pull the image first.".to_string());
    };

    let port_binding = PortBinding {
        host_ip: Some("127.0.0.1".to_string()),
        host_port: Some(settings.port.to_string()),
    };

    let mut port_bindings = HashMap::new();
    port_bindings.insert(
        "9876/tcp".to_string(),
        Some(vec![port_binding]),
    );

    let host_config = HostConfig {
        port_bindings: Some(port_bindings),
        mounts: Some(vec![Mount {
            target: Some("/root/.cache/huggingface".to_string()),
            source: Some(STT_MODEL_VOLUME.to_string()),
            typ: Some(MountTypeEnum::VOLUME),
            ..Default::default()
        }]),
        ..Default::default()
    };

    let mut labels = HashMap::new();
    labels.insert(
        "triple-c.stt.model".to_string(),
        settings.model.clone(),
    );
    labels.insert(
        "triple-c.stt.port".to_string(),
        settings.port.to_string(),
    );

    let config = Config {
        image: Some(image),
        env: Some(vec![format!("WHISPER_MODEL={}", settings.model)]),
        host_config: Some(host_config),
        labels: Some(labels),
        ..Default::default()
    };

    let options = CreateContainerOptions {
        name: STT_CONTAINER_NAME,
        ..Default::default()
    };

    let response = docker
        .create_container(Some(options), config)
        .await
        .map_err(|e| format!("Failed to create STT container: {}", e))?;

    Ok(response.id)
}

pub async fn ensure_stt_running(settings: &SttSettings) -> Result<SttStatus, String> {
    let docker = get_docker()?;

    // Check if container exists and if settings match
    if let Some((id, state, model)) = find_stt_container().await? {
        let needs_recreate = model != settings.model;

        if needs_recreate {
            // Settings changed, recreate
            if state == "running" {
                docker
                    .stop_container(&id, None::<StopContainerOptions>)
                    .await
                    .map_err(|e| format!("Failed to stop STT container: {}", e))?;
            }
            docker
                .remove_container(
                    &id,
                    Some(RemoveContainerOptions {
                        force: true,
                        ..Default::default()
                    }),
                )
                .await
                .map_err(|e| format!("Failed to remove STT container: {}", e))?;
        } else if state == "running" {
            return get_stt_status(settings).await;
        } else {
            // Container exists but stopped, start it
            docker
                .start_container(&id, None::<StartContainerOptions<String>>)
                .await
                .map_err(|e| format!("Failed to start STT container: {}", e))?;
            return get_stt_status(settings).await;
        }
    }

    // Create and start new container
    let id = create_stt_container(settings).await?;
    docker
        .start_container(&id, None::<StartContainerOptions<String>>)
        .await
        .map_err(|e| format!("Failed to start STT container: {}", e))?;

    get_stt_status(settings).await
}

pub async fn stop_stt_container() -> Result<(), String> {
    let docker = get_docker()?;

    if let Some((id, state, _)) = find_stt_container().await? {
        if state == "running" {
            docker
                .stop_container(&id, None::<StopContainerOptions>)
                .await
                .map_err(|e| format!("Failed to stop STT container: {}", e))?;
        }
    }

    Ok(())
}

pub async fn pull_stt_image<F>(on_progress: F) -> Result<(), String>
where
    F: Fn(String) + Send + 'static,
{
    super::image::pull_image(STT_REGISTRY_IMAGE, on_progress).await
}

pub async fn build_stt_image<F>(on_progress: F) -> Result<(), String>
where
    F: Fn(String) + Send + 'static,
{
    let docker = get_docker()?;

    let tar_bytes = create_stt_build_context()
        .map_err(|e| format!("Failed to create STT build context: {}", e))?;

    let options = BuildImageOptions {
        t: STT_LOCAL_IMAGE,
        rm: true,
        forcerm: true,
        ..Default::default()
    };

    let mut stream = docker.build_image(options, None, Some(tar_bytes.into()));

    while let Some(result) = stream.next().await {
        match result {
            Ok(output) => {
                if let Some(stream) = output.stream {
                    on_progress(stream);
                }
                if let Some(error) = output.error {
                    return Err(format!("Build error: {}", error));
                }
            }
            Err(e) => return Err(format!("Build stream error: {}", e)),
        }
    }

    Ok(())
}

fn create_stt_build_context() -> Result<Vec<u8>, std::io::Error> {
    let mut buf = Vec::new();
    {
        let mut archive = tar::Builder::new(&mut buf);

        let mut dockerfile_header = tar::Header::new_gnu();
        dockerfile_header.set_size(STT_DOCKERFILE.len() as u64);
        dockerfile_header.set_mode(0o644);
        dockerfile_header.set_cksum();
        archive.append_data(&mut dockerfile_header, "Dockerfile", STT_DOCKERFILE.as_bytes())?;

        let mut server_header = tar::Header::new_gnu();
        server_header.set_size(STT_SERVER.len() as u64);
        server_header.set_mode(0o644);
        server_header.set_cksum();
        archive.append_data(&mut server_header, "server.py", STT_SERVER.as_bytes())?;

        archive.finish()?;
    }

    let _ = buf.flush();
    Ok(buf)
}

