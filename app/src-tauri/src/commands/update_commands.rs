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
    let current_semver = parse_semver(current_version).unwrap_or((0, 0, 0));

    // Determine platform suffix for tag filtering
    let platform_suffix: &str = if cfg!(target_os = "windows") {
        "-win"
    } else if cfg!(target_os = "macos") {
        "-mac"
    } else {
        "" // Linux uses bare tags (no suffix)
    };

    // Filter releases by platform tag suffix
    let platform_releases: Vec<&GiteaRelease> = releases
        .iter()
        .filter(|r| {
            if platform_suffix.is_empty() {
                // Linux: bare tag only (no -win, no -mac)
                !r.tag_name.ends_with("-win") && !r.tag_name.ends_with("-mac")
            } else {
                r.tag_name.ends_with(platform_suffix)
            }
        })
        .collect();

    // Find the latest release with a higher semver version
    let mut best: Option<(&GiteaRelease, (u32, u32, u32))> = None;
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

/// Parse semver from a tag like "v0.2.5", "v0.2.5-win", "v0.2.5-mac" -> (0, 2, 5)
fn parse_semver_from_tag(tag: &str) -> Option<(u32, u32, u32)> {
    let clean = tag.trim_start_matches('v');
    // Remove platform suffix
    let clean = clean.strip_suffix("-win")
        .or_else(|| clean.strip_suffix("-mac"))
        .unwrap_or(clean);
    parse_semver(clean)
}

/// Extract a clean version string from a tag like "v0.2.5-win" -> "0.2.5"
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
