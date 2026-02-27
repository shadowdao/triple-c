use bollard::image::{BuildImageOptions, CreateImageOptions, ListImagesOptions};
use bollard::models::ImageSummary;
use futures_util::StreamExt;
use std::collections::HashMap;
use std::io::Write;

use super::client::get_docker;
use crate::models::container_config;

const DOCKERFILE: &str = include_str!("../../../../container/Dockerfile");
const ENTRYPOINT: &str = include_str!("../../../../container/entrypoint.sh");

pub async fn image_exists(image_name: &str) -> Result<bool, String> {
    let docker = get_docker()?;

    let filters: HashMap<String, Vec<String>> = HashMap::from([(
        "reference".to_string(),
        vec![image_name.to_string()],
    )]);

    let images: Vec<ImageSummary> = docker
        .list_images(Some(ListImagesOptions {
            filters,
            ..Default::default()
        }))
        .await
        .map_err(|e| format!("Failed to list images: {}", e))?;

    Ok(!images.is_empty())
}

pub async fn pull_image<F>(image_name: &str, on_progress: F) -> Result<(), String>
where
    F: Fn(String) + Send + 'static,
{
    let docker = get_docker()?;

    // Parse image name into from_image and tag
    let (from_image, tag) = if let Some(pos) = image_name.rfind(':') {
        // Check that the colon is part of a tag, not a port
        let after_colon = &image_name[pos + 1..];
        if after_colon.contains('/') {
            // The colon is part of a port (e.g., host:port/repo)
            (image_name, "latest")
        } else {
            (&image_name[..pos], after_colon)
        }
    } else {
        (image_name, "latest")
    };

    let options = CreateImageOptions {
        from_image,
        tag,
        ..Default::default()
    };

    let mut stream = docker.create_image(Some(options), None, None);

    while let Some(result) = stream.next().await {
        match result {
            Ok(info) => {
                let mut msg_parts = Vec::new();
                if let Some(ref status) = info.status {
                    msg_parts.push(status.clone());
                }
                if let Some(ref progress) = info.progress {
                    msg_parts.push(progress.clone());
                }
                if !msg_parts.is_empty() {
                    on_progress(msg_parts.join(" "));
                }
                if let Some(ref error) = info.error {
                    return Err(format!("Pull error: {}", error));
                }
            }
            Err(e) => return Err(format!("Pull stream error: {}", e)),
        }
    }

    Ok(())
}

pub async fn build_image<F>(on_progress: F) -> Result<(), String>
where
    F: Fn(String) + Send + 'static,
{
    let docker = get_docker()?;
    let full_name = container_config::local_build_image_name();

    let tar_bytes = create_build_context().map_err(|e| format!("Failed to create build context: {}", e))?;

    let options = BuildImageOptions {
        t: full_name.as_str(),
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

fn create_build_context() -> Result<Vec<u8>, std::io::Error> {
    let mut buf = Vec::new();
    {
        let mut archive = tar::Builder::new(&mut buf);

        let dockerfile_bytes = DOCKERFILE.as_bytes();
        let mut header = tar::Header::new_gnu();
        header.set_size(dockerfile_bytes.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        archive.append_data(&mut header, "Dockerfile", dockerfile_bytes)?;

        let entrypoint_bytes = ENTRYPOINT.as_bytes();
        let mut header = tar::Header::new_gnu();
        header.set_size(entrypoint_bytes.len() as u64);
        header.set_mode(0o755);
        header.set_cksum();
        archive.append_data(&mut header, "entrypoint.sh", entrypoint_bytes)?;

        archive.finish()?;
    }

    let _ = buf.flush();
    Ok(buf)
}
