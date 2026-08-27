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

/// The build-time preview suffix, if one was baked in and isn't blank.
///
/// The bundle version itself (`tauri.conf.json`, `Cargo.toml`, `package.json`)
/// is never given a `-preview.<sha>` suffix — `build-app-preview.yml` strips
/// it before patching those files, because the Windows MSI's `ProductVersion`
/// is a fixed-width numeric field with no room for one, and nothing here can
/// verify a change to that without an actual Windows build. `TRIPLE_C_BUILD_SUFFIX`
/// is the workaround: set as a build-time env var in the preview workflow
/// only, so `option_env!` bakes it into the binary without the bundle version
/// ever seeing it. A production build sets nothing, so `option_env!` reads
/// `None` here — see triple-c#32.
///
/// The single source of truth for "is this a preview build": both
/// `get_app_version()` (what the About panel shows) and `check_for_updates()`
/// (whether a same-numbered release counts as an update — see `pick_update`)
/// read this rather than each calling `option_env!` themselves, so the two
/// can never silently disagree about which build this is.
fn preview_build_suffix() -> Option<&'static str> {
    option_env!("TRIPLE_C_BUILD_SUFFIX").filter(|s| !s.is_empty())
}

fn format_app_version(base: &str, build_suffix: Option<&str>) -> String {
    match build_suffix {
        Some(suffix) if !suffix.is_empty() => format!("{}-{}", base, suffix),
        _ => base.to_string(),
    }
}

#[tauri::command]
pub fn get_app_version() -> String {
    format_app_version(env!("CARGO_PKG_VERSION"), preview_build_suffix())
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

    // `current_version` above is always the bare, stripped `CARGO_PKG_VERSION`
    // — the preview workflow patches `Cargo.toml` with that before compiling,
    // never the `-preview.<sha>`-suffixed one `get_app_version()` reports —
    // so a preview build and the release it precedes compile to the identical
    // numeric tuple by construction (see `build-app-preview.yml`'s "highest
    // tag used, +1" computation). A strict `>` therefore never fires for the
    // one release a preview most needs to be offered. `is_preview_build`
    // relaxes that one comparison to `>=` so "there is a real release at my
    // own number" reads as an update, without touching the production case
    // — see `pick_update`.
    let is_preview_build = preview_build_suffix().is_some();

    match pick_update(&releases, current_semver, platform_extensions, is_preview_build) {
        Some(release) => {
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

/// Pick the newest available update out of a release list, or `None` if
/// nothing beats `current_semver`. Pure and synchronous — split out of
/// `check_for_updates` so the prerelease/platform/version filtering can be
/// tested without a live HTTP call.
///
/// Three filters, all of which must pass: not a prerelease (see the long
/// comment on `GitHubRelease::prerelease`), at least one asset for this
/// platform, and a tag that parses as semver *and* beats what is running. A
/// tag that does not parse — `preview-<sha>` (the shape
/// `build-app-preview.yml` actually creates release tags with), most
/// realistically — is skipped rather than erroring, the same as it always
/// has been; nothing here changes what an update tag is expected to look
/// like, only what channel it is allowed to come from.
///
/// `is_preview_build` relaxes "beats" from `>` to `>=`. A preview build's
/// `current_semver` is the bare number it was compiled with, which is by
/// construction identical to the release it precedes — see the comment at
/// `check_for_updates`'s call site — so a strict `>` would never fire for
/// exactly the release a preview install most needs to be told about.
fn pick_update<'a>(
    releases: &'a [GitHubRelease],
    current_semver: (u32, u32, u32),
    platform_extensions: &[&str],
    is_preview_build: bool,
) -> Option<&'a GitHubRelease> {
    releases
        .iter()
        .filter(|r| !r.prerelease)
        .filter(|r| {
            r.assets
                .iter()
                .any(|a| platform_extensions.iter().any(|ext| a.name.ends_with(ext)))
        })
        .filter_map(|r| parse_semver_from_tag(&r.tag_name).map(|ver| (r, ver)))
        .filter(|(_, ver)| {
            if is_preview_build {
                *ver >= current_semver
            } else {
                *ver > current_semver
            }
        })
        .max_by_key(|(_, ver)| *ver)
        .map(|(r, _)| r)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::GitHubAsset;

    // ── format_app_version ──────────────────────────────────────────────

    #[test]
    fn a_production_build_reports_the_bare_version() {
        assert_eq!(format_app_version("0.4.12", None), "0.4.12");
        // An empty env var (set but blank) must not print a trailing dash.
        assert_eq!(format_app_version("0.4.12", Some("")), "0.4.12");
    }

    #[test]
    fn a_preview_build_reports_its_suffix() {
        assert_eq!(
            format_app_version("0.4.12", Some("preview.a1b2c3d")),
            "0.4.12-preview.a1b2c3d"
        );
    }

    // ── pick_update ──────────────────────────────────────────────────────

    fn release(tag: &str, prerelease: bool, asset_names: &[&str]) -> GitHubRelease {
        GitHubRelease {
            tag_name: tag.to_string(),
            html_url: format!("https://example.invalid/{}", tag),
            body: String::new(),
            assets: asset_names
                .iter()
                .map(|name| GitHubAsset {
                    name: name.to_string(),
                    browser_download_url: String::new(),
                    size: 0,
                })
                .collect(),
            published_at: "2026-01-01T00:00:00Z".to_string(),
            prerelease,
        }
    }

    const LINUX_EXTENSIONS: &[&str] = &[".AppImage", ".deb", ".rpm"];

    #[test]
    fn a_prerelease_is_never_offered_even_if_its_tag_would_otherwise_win() {
        let releases = vec![release("v9.9.9", true, &["app-9.9.9.AppImage"])];
        assert!(pick_update(&releases, (0, 4, 10), LINUX_EXTENSIONS, false).is_none());
    }

    #[test]
    fn a_release_with_no_asset_for_this_platform_is_skipped() {
        let releases = vec![release("v0.4.12", false, &["app-0.4.12.msi"])];
        assert!(pick_update(&releases, (0, 4, 10), LINUX_EXTENSIONS, false).is_none());
    }

    #[test]
    fn a_release_that_is_not_newer_is_not_offered() {
        let releases = vec![release("v0.4.10", false, &["app.AppImage"])];
        assert!(pick_update(&releases, (0, 4, 10), LINUX_EXTENSIONS, false).is_none());
    }

    #[test]
    fn an_untagged_or_unparseable_release_is_skipped_not_fatal() {
        // A `-preview.<sha>` tag is exactly the shape this must not choke on
        // or mistake for an update — it simply never parses as a bare semver.
        let releases = vec![
            release("preview-a1b2c3d", false, &["app.AppImage"]),
            release("v0.4.12", false, &["app.AppImage"]),
        ];
        let best = pick_update(&releases, (0, 4, 10), LINUX_EXTENSIONS, false).unwrap();
        assert_eq!(best.tag_name, "v0.4.12");
    }

    #[test]
    fn the_highest_qualifying_version_wins_not_the_first_or_last_in_the_list() {
        let releases = vec![
            release("v0.4.11", false, &["app.AppImage"]),
            release("v0.4.13", false, &["app.AppImage"]),
            release("v0.4.12", false, &["app.AppImage"]),
        ];
        let best = pick_update(&releases, (0, 4, 10), LINUX_EXTENSIONS, false).unwrap();
        assert_eq!(best.tag_name, "v0.4.13");
    }

    // ── is_preview_build (>= instead of >) ─────────────────────────────────

    /// The exact scenario triple-c#32 was filed to fix: a preview compiled as
    /// `0.4.12-preview.<sha>` (bare `CARGO_PKG_VERSION` "0.4.12") must be
    /// offered the `v0.4.12` release that follows it, even though the two
    /// compute to the identical numeric tuple.
    #[test]
    fn a_preview_build_is_offered_the_release_it_precedes() {
        let releases = vec![release("v0.4.12", false, &["app.AppImage"])];
        assert!(pick_update(&releases, (0, 4, 12), LINUX_EXTENSIONS, false).is_none());
        let best = pick_update(&releases, (0, 4, 12), LINUX_EXTENSIONS, true).unwrap();
        assert_eq!(best.tag_name, "v0.4.12");
    }

    #[test]
    fn a_preview_build_is_not_offered_an_older_release() {
        let releases = vec![release("v0.4.11", false, &["app.AppImage"])];
        assert!(pick_update(&releases, (0, 4, 12), LINUX_EXTENSIONS, true).is_none());
    }

    #[test]
    fn a_production_build_still_requires_strictly_newer() {
        // A production build must never treat "equal" as an update — that
        // would perpetually re-offer the version already running.
        let releases = vec![release("v0.4.12", false, &["app.AppImage"])];
        assert!(pick_update(&releases, (0, 4, 12), LINUX_EXTENSIONS, false).is_none());
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
