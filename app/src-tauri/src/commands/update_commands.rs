use tauri::State;

use crate::docker;
use crate::models::{container_config, GiteaRelease, ImageUpdateInfo, ReleaseAsset, UpdateInfo};
use crate::AppState;

const RELEASES_URL: &str =
    "https://repo.anhonesthost.net/api/v1/repos/cybercovellc/triple-c/releases";

/// Gitea container-registry tag object (v2 manifest).
const REGISTRY_API_BASE: &str =
    "https://repo.anhonesthost.net/v2/cybercovellc/triple-c/triple-c-sandbox";

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

    let releases: Vec<GiteaRelease> = client
        .get(RELEASES_URL)
        .header("Accept", "application/json")
        .send()
        .await
        .map_err(|e| format!("Failed to fetch releases: {}", e))?
        .json()
        .await
        .map_err(|e| format!("Failed to parse releases: {}", e))?;

    let current_version = env!("CARGO_PKG_VERSION");
    let is_windows = cfg!(target_os = "windows");

    // Filter releases by platform tag suffix
    let platform_releases: Vec<&GiteaRelease> = releases
        .iter()
        .filter(|r| {
            if is_windows {
                r.tag_name.ends_with("-win")
            } else {
                !r.tag_name.ends_with("-win")
            }
        })
        .collect();

    // Find the latest release with a higher patch version
    // Version format: 0.1.X or v0.1.X (tag may have prefix/suffix)
    let current_patch = parse_patch_version(current_version).unwrap_or(0);

    let mut best: Option<(&GiteaRelease, u32)> = None;
    for release in &platform_releases {
        if let Some(patch) = parse_patch_from_tag(&release.tag_name) {
            if patch > current_patch {
                if best.is_none() || patch > best.unwrap().1 {
                    best = Some((release, patch));
                }
            }
        }
    }

    match best {
        Some((release, _)) => {
            let assets = release
                .assets
                .iter()
                .map(|a| ReleaseAsset {
                    name: a.name.clone(),
                    browser_download_url: a.browser_download_url.clone(),
                    size: a.size,
                })
                .collect();

            // Reconstruct version string from tag
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

/// Parse patch version from a semver string like "0.1.5" -> 5
fn parse_patch_version(version: &str) -> Option<u32> {
    let clean = version.trim_start_matches('v');
    let parts: Vec<&str> = clean.split('.').collect();
    if parts.len() >= 3 {
        parts[2].parse().ok()
    } else {
        None
    }
}

/// Parse patch version from a tag like "v0.1.5", "v0.1.5-win", "0.1.5" -> 5
fn parse_patch_from_tag(tag: &str) -> Option<u32> {
    let clean = tag.trim_start_matches('v');
    // Remove platform suffix
    let clean = clean.strip_suffix("-win").unwrap_or(clean);
    parse_patch_version(clean)
}

/// Extract a clean version string from a tag like "v0.1.5-win" -> "0.1.5"
fn extract_version_from_tag(tag: &str) -> Option<String> {
    let clean = tag.trim_start_matches('v');
    let clean = clean.strip_suffix("-win").unwrap_or(clean);
    // Validate it looks like a version
    let parts: Vec<&str> = clean.split('.').collect();
    if parts.len() >= 3 && parts.iter().all(|p| p.parse::<u32>().is_ok()) {
        Some(clean.to_string())
    } else {
        None
    }
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

    // 2. Get remote digest from the Gitea container registry (OCI distribution spec)
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

/// Fetch the digest of a tag from the Gitea container registry using the
/// OCI / Docker Registry HTTP API v2.
///
/// We issue a HEAD request to /v2/<repo>/manifests/<tag> and read the
/// `Docker-Content-Digest` header that the registry returns.
async fn fetch_remote_digest(tag: &str) -> Result<Option<String>, String> {
    let url = format!("{}/manifests/{}", REGISTRY_API_BASE, tag);

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .map_err(|e| format!("Failed to create HTTP client: {}", e))?;

    let response = client
        .head(&url)
        .header(
            "Accept",
            "application/vnd.docker.distribution.manifest.v2+json, application/vnd.oci.image.index.v1+json",
        )
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
