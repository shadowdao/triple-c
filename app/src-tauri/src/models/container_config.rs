use serde::{Deserialize, Serialize};

use super::app_settings::ImageSource;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContainerInfo {
    pub container_id: String,
    pub project_id: String,
    pub status: String,
    pub image: String,
}

pub const LOCAL_IMAGE_NAME: &str = "triple-c";
pub const IMAGE_TAG: &str = "latest";
pub const REGISTRY_IMAGE: &str = "repo.anhonesthost.net/cybercovellc/triple-c/triple-c-sandbox:latest";

pub fn local_build_image_name() -> String {
    format!("{LOCAL_IMAGE_NAME}:{IMAGE_TAG}")
}

pub fn resolve_image_name(source: &ImageSource, custom: &Option<String>) -> String {
    match source {
        ImageSource::Registry => REGISTRY_IMAGE.to_string(),
        ImageSource::LocalBuild => local_build_image_name(),
        ImageSource::Custom => custom
            .as_deref()
            .filter(|s| !s.is_empty())
            .unwrap_or(REGISTRY_IMAGE)
            .to_string(),
    }
}
