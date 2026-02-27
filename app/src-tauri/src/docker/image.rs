use bollard::image::{BuildImageOptions, ListImagesOptions};
use bollard::models::ImageSummary;
use futures_util::StreamExt;
use std::collections::HashMap;
use std::io::Write;

use super::client::get_docker;
use crate::models::container_config;

const DOCKERFILE: &str = include_str!("../../../../container/Dockerfile");
const ENTRYPOINT: &str = include_str!("../../../../container/entrypoint.sh");

pub async fn image_exists() -> Result<bool, String> {
    let docker = get_docker()?;
    let full_name = container_config::full_image_name();

    let filters: HashMap<String, Vec<String>> = HashMap::from([(
        "reference".to_string(),
        vec![full_name],
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

pub async fn build_image<F>(on_progress: F) -> Result<(), String>
where
    F: Fn(String) + Send + 'static,
{
    let docker = get_docker()?;
    let full_name = container_config::full_image_name();

    // Create a tar archive in memory containing Dockerfile and entrypoint.sh
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

        // Add Dockerfile
        let dockerfile_bytes = DOCKERFILE.as_bytes();
        let mut header = tar::Header::new_gnu();
        header.set_size(dockerfile_bytes.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        archive.append_data(&mut header, "Dockerfile", dockerfile_bytes)?;

        // Add entrypoint.sh
        let entrypoint_bytes = ENTRYPOINT.as_bytes();
        let mut header = tar::Header::new_gnu();
        header.set_size(entrypoint_bytes.len() as u64);
        header.set_mode(0o755);
        header.set_cksum();
        archive.append_data(&mut header, "entrypoint.sh", entrypoint_bytes)?;

        archive.finish()?;
    }

    // Flush to make sure all data is written
    let _ = buf.flush();
    Ok(buf)
}
