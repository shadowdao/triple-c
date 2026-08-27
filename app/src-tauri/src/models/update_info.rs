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

/// GitHub API release response (internal).
#[derive(Debug, Clone, Deserialize)]
pub struct GitHubRelease {
    pub tag_name: String,
    pub html_url: String,
    pub body: String,
    pub assets: Vec<GitHubAsset>,
    pub published_at: String,
    /// Whether GitHub itself has this release marked as a prerelease.
    /// `#[serde(default)]` rather than required: every response GitHub sends
    /// carries this, but nothing here should refuse to parse the rest of a
    /// release over one missing field. Defaults to `false` (offered) rather
    /// than `true` (excluded) — a missing field only happens if GitHub's API
    /// shape changes, and "API changed, therefore updates silently stop
    /// working forever" is the worse failure of the two.
    ///
    /// `build-app.yml`'s own mirror never publishes a prerelease, but
    /// `.gitea/workflows/backfill-releases.yml` forwards every Gitea release
    /// unfiltered, `prerelease` included. A preview release's `preview-<sha>`
    /// tag already fails semver parsing on its own, so this field is not what
    /// stops *that* case — it is what stops the case tag-parsing can't catch:
    /// a normally-tagged release (`v0.4.13`) that someone marks as a
    /// prerelease on Gitea (a hotfix candidate, an RC) and a backfill then
    /// mirrors as-is. Real defence for that case, not a no-op.
    #[serde(default)]
    pub prerelease: bool,
}

/// GitHub API asset response (internal).
#[derive(Debug, Clone, Deserialize)]
pub struct GitHubAsset {
    pub name: String,
    pub browser_download_url: String,
    pub size: u64,
}

/// Info returned to the frontend about an available container image update.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageUpdateInfo {
    /// The remote digest (e.g. sha256:abc...)
    pub remote_digest: String,
    /// The local digest, if available
    pub local_digest: Option<String>,
    /// When the remote image was last updated (if known)
    pub remote_updated_at: Option<String>,
}
