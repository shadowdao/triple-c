// Helpers for detecting whether Docker (or a Docker-compatible runtime) is
// installed on the host and, when missing, offering to install it for the user.
//
// We use the Docker convenience script on Linux and Rancher Desktop on macOS /
// Windows. On every platform we also surface an official documentation URL so
// users without a recognised package manager can install manually.

use serde::{Deserialize, Serialize};

pub mod platform;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstallOptions {
    /// "linux" | "macos" | "windows" | "unknown"
    pub os: String,
    /// User-facing name of what we'd install ("Docker Engine" / "Rancher Desktop").
    pub product_name: String,
    /// Whether we can kick off a one-click install with what's on this machine.
    pub can_auto_install: bool,
    /// Short identifier of the method we'd use ("pkexec", "brew", "winget", or None).
    pub auto_install_method: Option<String>,
    /// If auto-install isn't possible, a human-readable reason to show the user.
    pub auto_install_blocker: Option<String>,
    /// Official documentation URL for manual install.
    pub docs_url: String,
    /// Ordered manual install steps (plain text lines).
    pub manual_steps: Vec<String>,
    /// Notes to display after a successful auto-install (e.g. log out/back in).
    pub post_install_notes: Vec<String>,
}

pub fn detect_install_options() -> InstallOptions {
    if cfg!(target_os = "linux") {
        platform::linux_options()
    } else if cfg!(target_os = "macos") {
        platform::macos_options()
    } else if cfg!(target_os = "windows") {
        platform::windows_options()
    } else {
        InstallOptions {
            os: "unknown".into(),
            product_name: "Docker".into(),
            can_auto_install: false,
            auto_install_method: None,
            auto_install_blocker: Some("Unsupported operating system".into()),
            docs_url: "https://docs.docker.com/get-docker/".into(),
            manual_steps: vec![
                "Visit the Docker documentation and follow the install guide for your OS.".into(),
            ],
            post_install_notes: vec![],
        }
    }
}
