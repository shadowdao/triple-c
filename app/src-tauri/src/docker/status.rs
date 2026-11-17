//! Why Docker is unavailable, not just whether it is.
//!
//! A daemon that failed to start (systemd `start-limit-hit`, Docker Desktop
//! still booting) and a socket the user may not open both used to surface as
//! "Docker not detected — install it", which sends people to reinstall
//! something they already have. This module tells the cases apart from the
//! ping's error and a few cheap filesystem probes. It never runs a process
//! (no `systemctl`, no `docker info`).

use serde::Serialize;
use std::error::Error as StdError;
use std::io;
use std::path::{Path, PathBuf};

/// Mirrors `DockerStatus` in `src/lib/types.ts`.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct DockerStatus {
    /// `running` | `not_running` | `no_permission` | `not_installed`
    pub state: DockerState,
    /// The endpoint that was tried — `DOCKER_HOST` when set, else the default socket/pipe.
    pub endpoint: String,
    /// The connection error, for the "details" line. `None` when running.
    pub detail: Option<String>,
    /// `linux` | `macos` | `windows` | `unknown` — picks the guidance to show.
    pub os: &'static str,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DockerState {
    Running,
    NotRunning,
    NoPermission,
    NotInstalled,
}

/// What the ping told us, reduced to what matters for the classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PingOutcome {
    Ok,
    /// The socket/pipe exists but the OS refused to let us open it.
    PermissionDenied,
    /// Nothing answered: socket missing, connection refused, timeout, other.
    Unreachable,
}

/// The pure decision. `installed` is true when any installed-ness probe hit.
pub fn classify(ping: PingOutcome, installed: bool) -> DockerState {
    match ping {
        PingOutcome::Ok => DockerState::Running,
        // Getting EACCES means something is listening on a socket we can't
        // open — Docker is there; the user lacks access.
        PingOutcome::PermissionDenied => DockerState::NoPermission,
        PingOutcome::Unreachable if installed => DockerState::NotRunning,
        PingOutcome::Unreachable => DockerState::NotInstalled,
    }
}

/// Walk an error's `source()` chain for an `io::Error` and map its kind.
/// Falls back to the rendered message, since some layers (hyper-util's
/// connect errors, named pipes) only keep the OS error as text.
pub fn ping_outcome_from_error(err: &(dyn StdError + 'static)) -> PingOutcome {
    let mut cur: Option<&(dyn StdError + 'static)> = Some(err);
    let mut text = String::new();
    while let Some(e) = cur {
        if let Some(ioe) = e.downcast_ref::<io::Error>() {
            if ioe.kind() == io::ErrorKind::PermissionDenied {
                return PingOutcome::PermissionDenied;
            }
        }
        text.push_str(&e.to_string().to_lowercase());
        text.push('\n');
        cur = e.source();
    }
    if text.contains("permission denied") || text.contains("access is denied") || text.contains("os error 13") {
        PingOutcome::PermissionDenied
    } else {
        PingOutcome::Unreachable
    }
}

/// The endpoint bollard's `connect_with_local_defaults` will use.
pub fn endpoint(docker_host: Option<&str>) -> String {
    match docker_host.map(str::trim).filter(|h| !h.is_empty()) {
        Some(h) => h.to_string(),
        None if cfg!(windows) => "npipe:////./pipe/docker_engine".to_string(),
        None => "unix:///var/run/docker.sock".to_string(),
    }
}

pub fn current_os() -> &'static str {
    if cfg!(target_os = "linux") {
        "linux"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else if cfg!(windows) {
        "windows"
    } else {
        "unknown"
    }
}

/// Paths whose existence means Docker is installed on this OS. Pure so the
/// list can be tested; `docker_installed` does the filesystem checks.
pub fn install_markers(os: &str, home: Option<&Path>, docker_host: Option<&str>) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = Vec::new();
    // A unix:// DOCKER_HOST whose socket file exists counts, whatever the OS.
    if let Some(p) = docker_host.and_then(|h| h.trim().strip_prefix("unix://")) {
        if !p.is_empty() {
            v.push(PathBuf::from(p));
        }
    }
    match os {
        "linux" => {
            v.extend(
                ["/usr/bin/dockerd", "/usr/local/bin/dockerd", "/usr/bin/docker", "/var/run/docker.sock", "/run/docker.sock"]
                    .iter()
                    .map(PathBuf::from),
            );
            if let Some(h) = home {
                v.push(h.join(".docker/desktop"));
                v.push(h.join("bin/dockerd")); // rootless install script
            }
        }
        "macos" => {
            v.push(PathBuf::from("/Applications/Docker.app"));
            v.push(PathBuf::from("/var/run/docker.sock"));
            v.push(PathBuf::from("/usr/local/bin/docker"));
            if let Some(h) = home {
                v.push(h.join("Applications/Docker.app"));
                v.push(h.join(".docker/run/docker.sock"));
            }
        }
        "windows" => {
            v.push(PathBuf::from(r"C:\Program Files\Docker\Docker\Docker Desktop.exe"));
            v.push(PathBuf::from(r"C:\Program Files\Docker\Docker\resources\bin\docker.exe"));
        }
        _ => {}
    }
    v
}

/// Executable names to look for on `PATH`.
pub fn path_binaries(os: &str) -> &'static [&'static str] {
    match os {
        "windows" => &["docker.exe"],
        _ => &["docker", "dockerd"],
    }
}

fn on_path(name: &str) -> bool {
    std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).any(|dir| dir.join(name).is_file()))
        .unwrap_or(false)
}

/// Cheap installed-ness probe: stats a handful of paths, runs nothing.
pub fn docker_installed(os: &str, docker_host: Option<&str>) -> bool {
    let home = dirs::home_dir();
    if install_markers(os, home.as_deref(), docker_host).iter().any(|p| p.exists()) {
        return true;
    }
    if os == "linux" {
        // Rootless Docker's socket lives in the user's runtime dir.
        if let Some(rt) = std::env::var_os("XDG_RUNTIME_DIR") {
            if Path::new(&rt).join("docker.sock").exists() {
                return true;
            }
        }
    }
    path_binaries(os).iter().any(|b| on_path(b))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fmt;

    #[test]
    fn ping_ok_is_running_regardless_of_probes() {
        assert_eq!(classify(PingOutcome::Ok, false), DockerState::Running);
        assert_eq!(classify(PingOutcome::Ok, true), DockerState::Running);
    }

    #[test]
    fn unreachable_and_installed_is_not_running() {
        // The reported case: dockerd present, systemd gave up restarting it.
        assert_eq!(classify(PingOutcome::Unreachable, true), DockerState::NotRunning);
    }

    #[test]
    fn unreachable_and_nothing_found_is_not_installed() {
        assert_eq!(classify(PingOutcome::Unreachable, false), DockerState::NotInstalled);
    }

    #[test]
    fn permission_denied_is_no_permission_even_without_probes() {
        assert_eq!(classify(PingOutcome::PermissionDenied, false), DockerState::NoPermission);
        assert_eq!(classify(PingOutcome::PermissionDenied, true), DockerState::NoPermission);
    }

    /// An error that wraps another, the way bollard → hyper → io do.
    #[derive(Debug)]
    struct Wrap(&'static str, Option<io::Error>);
    impl fmt::Display for Wrap {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str(self.0)
        }
    }
    impl StdError for Wrap {
        fn source(&self) -> Option<&(dyn StdError + 'static)> {
            self.1.as_ref().map(|e| e as _)
        }
    }

    #[test]
    fn nested_io_permission_denied_is_found() {
        let e = Wrap("error trying to connect", Some(io::Error::from(io::ErrorKind::PermissionDenied)));
        assert_eq!(ping_outcome_from_error(&e), PingOutcome::PermissionDenied);
    }

    #[test]
    fn nested_connection_refused_and_not_found_are_unreachable() {
        let refused = Wrap("error trying to connect", Some(io::Error::from(io::ErrorKind::ConnectionRefused)));
        let missing = Wrap("error trying to connect", Some(io::Error::from(io::ErrorKind::NotFound)));
        assert_eq!(ping_outcome_from_error(&refused), PingOutcome::Unreachable);
        assert_eq!(ping_outcome_from_error(&missing), PingOutcome::Unreachable);
    }

    #[test]
    fn permission_denied_only_in_text_is_found() {
        let e = Wrap("client error (Connect): Permission denied (os error 13)", None);
        assert_eq!(ping_outcome_from_error(&e), PingOutcome::PermissionDenied);
    }

    #[test]
    fn endpoint_respects_docker_host() {
        assert_eq!(endpoint(Some("tcp://10.0.0.5:2375")), "tcp://10.0.0.5:2375");
        assert_eq!(endpoint(Some("  ")), endpoint(None));
        let default = endpoint(None);
        assert!(default.starts_with("unix://") || default.starts_with("npipe://"), "{default}");
    }

    #[test]
    fn install_markers_cover_each_os() {
        let home = Path::new("/home/u");
        let linux = install_markers("linux", Some(home), None);
        assert!(linux.contains(&PathBuf::from("/usr/bin/dockerd")));
        assert!(linux.contains(&PathBuf::from("/var/run/docker.sock")));
        assert!(linux.contains(&home.join(".docker/desktop")));

        let mac = install_markers("macos", Some(home), None);
        assert!(mac.contains(&PathBuf::from("/Applications/Docker.app")));
        assert!(mac.contains(&home.join(".docker/run/docker.sock")));

        let win = install_markers("windows", None, None);
        assert!(win.contains(&PathBuf::from(r"C:\Program Files\Docker\Docker\Docker Desktop.exe")));
        assert_eq!(path_binaries("windows"), &["docker.exe"]);
        assert!(path_binaries("linux").contains(&"dockerd"));
    }

    #[test]
    fn unix_docker_host_socket_is_a_marker() {
        let m = install_markers("linux", None, Some("unix:///run/user/1000/docker.sock"));
        assert_eq!(m[0], PathBuf::from("/run/user/1000/docker.sock"));
        let tcp = install_markers("linux", None, Some("tcp://host:2375"));
        assert!(!tcp.iter().any(|p| p.to_string_lossy().contains("host")));
    }

    #[test]
    fn status_serializes_with_snake_case_state() {
        let s = DockerStatus {
            state: DockerState::NotRunning,
            endpoint: "unix:///var/run/docker.sock".into(),
            detail: Some("refused".into()),
            os: "linux",
        };
        let json = serde_json::to_value(&s).unwrap();
        assert_eq!(json["state"], "not_running");
        assert_eq!(json["os"], "linux");
    }
}
