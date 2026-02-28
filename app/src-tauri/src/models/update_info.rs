use serde::{Deserialize, Serialize};

/// Info returned to the frontend about an available update.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateInfo {
    pub version: String,
    pub tag_name: String,
    pub release_url: String,
    pub body: String,
    pub assets: Vec<ReleaseAsset>,
    pub published_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReleaseAsset {
    pub name: String,
    pub browser_download_url: String,
    pub size: u64,
}

/// Gitea API release response (internal).
#[derive(Debug, Clone, Deserialize)]
pub struct GiteaRelease {
    pub tag_name: String,
    pub html_url: String,
    pub body: String,
    pub assets: Vec<GiteaAsset>,
    pub published_at: String,
}

/// Gitea API asset response (internal).
#[derive(Debug, Clone, Deserialize)]
pub struct GiteaAsset {
    pub name: String,
    pub browser_download_url: String,
    pub size: u64,
}
