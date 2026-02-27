use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContainerInfo {
    pub container_id: String,
    pub project_id: String,
    pub status: String,
    pub image: String,
}

pub const IMAGE_NAME: &str = "triple-c";
pub const IMAGE_TAG: &str = "latest";

pub fn full_image_name() -> String {
    format!("{IMAGE_NAME}:{IMAGE_TAG}")
}
