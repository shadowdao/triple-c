use serde::Deserialize;
use tauri::State;

use crate::docker;
use crate::models::{container_config, GitHubRelease, ImageUpdateInfo, ReleaseAsset, UpdateInfo};
use crate::AppState;

const RELEASES_URL: &str =
    "https://api.github.com/repos/shadowdao/triple-c/releases";

/// GHCR container-registry API base (OCI distribution spec).
const REGISTRY_API_BASE: &str =
    "https://ghcr.io/v2/shadowdao/triple-c-sandbox";

/// GHCR token endpoint for anonymous pull access.
const GHCR_TOKEN_URL: &str =
    "https://ghcr.io/token?scope=repository:shadowdao/triple-c-sandbox:pull";

#[tauri::command]
pub fn get_app_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

#[tauri::command]
pub async fn check_for_updates() -> Result<Option<UpdateInfo>, String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .map_err(|e| format!("Failed to create HTTP client: {}", e))?;

    let releases: Vec<GitHubRelease> = client
        .get(RELEASES_URL)
        .header("Accept", "application/json")
        .header("User-Agent", "triple-c-updater")
        .send()
        .await
        .map_err(|e| format!("Failed to fetch releases: {}", e))?
        .json()
        .await
        .map_err(|e| format!("Failed to parse releases: {}", e))?;

    let current_version = env!("CARGO_PKG_VERSION");
    let current_semver = parse_semver(current_version).unwrap_or((0, 0, 0));

    // Determine platform-specific asset extensions
    let platform_extensions: &[&str] = if cfg!(target_os = "windows") {
        &[".msi", ".exe"]
    } else if cfg!(target_os = "macos") {
        &[".dmg", ".app.tar.gz"]
    } else {
        &[".AppImage", ".deb", ".rpm"]
    };

    // Filter releases that have at least one asset matching the current platform
    let platform_releases: Vec<&GitHubRelease> = releases
        .iter()
        .filter(|r| {
            r.assets.iter().any(|a| {
                platform_extensions.iter().any(|ext| a.name.ends_with(ext))
            })
        })
        .collect();

    // Find the latest release with a higher semver version
    let mut best: Option<(&GitHubRelease, (u32, u32, u32))> = None;
    for release in &platform_releases {
        if let Some(ver) = parse_semver_from_tag(&release.tag_name) {
            if ver > current_semver {
                if best.is_none() || ver > best.unwrap().1 {
                    best = Some((release, ver));
                }
            }
        }
    }

    match best {
        Some((release, _)) => {
            // Only include assets matching the current platform
            let assets = release
                .assets
                .iter()
                .filter(|a| {
                    platform_extensions.iter().any(|ext| a.name.ends_with(ext))
                })
                .map(|a| ReleaseAsset {
                    name: a.name.clone(),
                    browser_download_url: a.browser_download_url.clone(),
                    size: a.size,
                })
                .collect();

            let version = extract_version_from_tag(&release.tag_name)
                .unwrap_or_else(|| release.tag_name.clone());

            Ok(Some(UpdateInfo {
                version,
                tag_name: release.tag_name.clone(),
                release_url: release.html_url.clone(),
                body: release.body.clone(),
                assets,
                published_at: release.published_at.clone(),
            }))
        }
        None => Ok(None),
    }
}

/// Parse a semver string like "0.2.5" -> (0, 2, 5)
fn parse_semver(version: &str) -> Option<(u32, u32, u32)> {
    let clean = version.trim_start_matches('v');
    let parts: Vec<&str> = clean.split('.').collect();
    if parts.len() >= 3 {
        let major = parts[0].parse().ok()?;
        let minor = parts[1].parse().ok()?;
        let patch = parts[2].parse().ok()?;
        Some((major, minor, patch))
    } else {
        None
    }
}

/// Parse semver from a tag like "v0.2.5" -> (0, 2, 5)
fn parse_semver_from_tag(tag: &str) -> Option<(u32, u32, u32)> {
    let clean = tag.trim_start_matches('v');
    parse_semver(clean)
}

/// Extract a clean version string from a tag like "v0.2.5" -> "0.2.5"
fn extract_version_from_tag(tag: &str) -> Option<String> {
    let (major, minor, patch) = parse_semver_from_tag(tag)?;
    Some(format!("{}.{}.{}", major, minor, patch))
}

/// Check whether a newer container image is available in the registry.
///
/// Compares the local image digest with the remote registry digest using the
/// Docker Registry HTTP API v2.  Only applies when the image source is
/// "registry" (the default); for local builds or custom images we cannot
/// meaningfully check for remote updates.
#[tauri::command]
pub async fn check_image_update(
    state: State<'_, AppState>,
) -> Result<Option<ImageUpdateInfo>, String> {
    let settings = state.settings_store.get();

    // Only check for registry images
    if settings.image_source != crate::models::app_settings::ImageSource::Registry {
        return Ok(None);
    }

    let image_name =
        container_config::resolve_image_name(&settings.image_source, &settings.custom_image_name);

    // 1. Get local image digest via Docker
    let local_digest = docker::get_local_image_digest(&image_name).await.ok().flatten();

    // 2. Get remote digest from the GHCR container registry (OCI distribution spec)
    let remote_digest = fetch_remote_digest("latest").await?;

    // No remote digest available — nothing to compare
    let remote_digest = match remote_digest {
        Some(d) => d,
        None => return Ok(None),
    };

    // If local digest matches remote, no update
    if let Some(ref local) = local_digest {
        if *local == remote_digest {
            return Ok(None);
        }
    }

    // There's a difference (or no local image at all)
    Ok(Some(ImageUpdateInfo {
        remote_digest,
        local_digest,
        remote_updated_at: None,
    }))
}

/// Fetch the digest of a tag from GHCR using the OCI / Docker Registry HTTP API v2.
///
/// GHCR requires authentication even for public images, so we first obtain an
/// anonymous token, then issue a HEAD request to /v2/<repo>/manifests/<tag>
/// and read the `Docker-Content-Digest` header.
async fn fetch_remote_digest(tag: &str) -> Result<Option<String>, String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .map_err(|e| format!("Failed to create HTTP client: {}", e))?;

    // 1. Obtain anonymous bearer token from GHCR
    let token = match fetch_ghcr_token(&client).await {
        Ok(t) => t,
        Err(e) => {
            log::warn!("Failed to obtain GHCR token: {}", e);
            return Ok(None);
        }
    };

    // 2. HEAD the manifest with the token
    let url = format!("{}/manifests/{}", REGISTRY_API_BASE, tag);

    let response = client
        .head(&url)
        .header(
            "Accept",
            "application/vnd.docker.distribution.manifest.v2+json, application/vnd.oci.image.index.v1+json",
        )
        .header("Authorization", format!("Bearer {}", token))
        .send()
        .await;

    match response {
        Ok(resp) => {
            if !resp.status().is_success() {
                log::warn!(
                    "Registry returned status {} when checking image digest",
                    resp.status()
                );
                return Ok(None);
            }
            // The digest is returned in the Docker-Content-Digest header
            if let Some(digest) = resp.headers().get("docker-content-digest") {
                if let Ok(val) = digest.to_str() {
                    return Ok(Some(val.to_string()));
                }
            }
            Ok(None)
        }
        Err(e) => {
            log::warn!("Failed to check registry for image update: {}", e);
            Ok(None)
        }
    }
}

/// Fetch an anonymous bearer token from GHCR for pulling public images.
async fn fetch_ghcr_token(client: &reqwest::Client) -> Result<String, String> {
    #[derive(Deserialize)]
    struct TokenResponse {
        token: String,
    }

    let resp: TokenResponse = client
        .get(GHCR_TOKEN_URL)
        .header("User-Agent", "triple-c-updater")
        .send()
        .await
        .map_err(|e| format!("GHCR token request failed: {}", e))?
        .json()
        .await
        .map_err(|e| format!("Failed to parse GHCR token response: {}", e))?;

    Ok(resp.token)
}
