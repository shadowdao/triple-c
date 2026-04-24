use std::path::PathBuf;
use std::process::Stdio;

use tauri::{AppHandle, Emitter};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;

use super::InstallOptions;

const PROGRESS_EVENT: &str = "docker-install-progress";

fn which(cmd: &str) -> bool {
    find_on_path(cmd).is_some()
}

/// Search PATH for an executable, plus a handful of well-known locations that
/// GUI-launched apps on macOS/Linux typically miss (Homebrew prefixes, etc.).
fn find_on_path(cmd: &str) -> Option<PathBuf> {
    #[cfg(unix)]
    let extra: &[&str] = &[
        "/opt/homebrew/bin",
        "/usr/local/bin",
        "/usr/bin",
        "/bin",
    ];
    #[cfg(windows)]
    let extra: &[&str] = &[];

    if let Ok(path) = std::env::var("PATH") {
        let sep = if cfg!(windows) { ';' } else { ':' };
        for dir in path.split(sep).chain(extra.iter().copied()) {
            let candidate = PathBuf::from(dir).join(cmd);
            if candidate.is_file() {
                return Some(candidate);
            }
            #[cfg(windows)]
            for ext in ["exe", "cmd", "bat"] {
                let mut with_ext = candidate.clone();
                with_ext.set_extension(ext);
                if with_ext.is_file() {
                    return Some(with_ext);
                }
            }
        }
    }
    for dir in extra {
        let candidate = PathBuf::from(dir).join(cmd);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

async fn stream(app: &AppHandle, mut child: tokio::process::Child) -> Result<(), String> {
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();

    let app_out = app.clone();
    let out_task = tokio::spawn(async move {
        if let Some(out) = stdout {
            let mut lines = BufReader::new(out).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let _ = app_out.emit(PROGRESS_EVENT, line);
            }
        }
    });

    let app_err = app.clone();
    let err_task = tokio::spawn(async move {
        if let Some(err) = stderr {
            let mut lines = BufReader::new(err).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let _ = app_err.emit(PROGRESS_EVENT, line);
            }
        }
    });

    let status = child
        .wait()
        .await
        .map_err(|e| format!("install process failed: {}", e))?;
    let _ = out_task.await;
    let _ = err_task.await;

    if !status.success() {
        return Err(format!(
            "installer exited with status {}",
            status.code().map(|c| c.to_string()).unwrap_or_else(|| "signal".into())
        ));
    }
    Ok(())
}

// ─── Linux ───────────────────────────────────────────────────────────────────

pub fn linux_options() -> InstallOptions {
    let has_pkexec = which("pkexec");
    let has_curl = which("curl");

    let (can_auto, blocker) = match (has_pkexec, has_curl) {
        (true, true) => (true, None),
        (false, _) => (
            false,
            Some("pkexec not found — install policykit-1 or follow manual steps.".into()),
        ),
        (_, false) => (
            false,
            Some("curl not found — install curl or follow manual steps.".into()),
        ),
    };

    InstallOptions {
        os: "linux".into(),
        product_name: "Docker Engine".into(),
        can_auto_install: can_auto,
        auto_install_method: if can_auto { Some("pkexec".into()) } else { None },
        auto_install_blocker: blocker,
        docs_url: "https://docs.docker.com/engine/install/".into(),
        manual_steps: vec![
            "Open a terminal.".into(),
            "Run: curl -fsSL https://get.docker.com | sh".into(),
            "Add yourself to the docker group: sudo usermod -aG docker $USER".into(),
            "Log out and log back in for group changes to take effect.".into(),
        ],
        post_install_notes: vec![
            "Log out and log back in (or reboot) so your user picks up the docker group.".into(),
            "If Docker isn't detected after re-login, start the service: sudo systemctl start docker".into(),
        ],
    }
}

async fn run_linux_install(app: &AppHandle) -> Result<(), String> {
    // Grab the current username so pkexec (which runs as root) can add the
    // original invoking user to the docker group.
    let invoking_user = std::env::var("USER")
        .or_else(|_| std::env::var("LOGNAME"))
        .map_err(|_| "could not determine invoking username".to_string())?;

    // Write a self-contained installer script to a temp file. Running the
    // Docker convenience script then appending the user to the docker group
    // and enabling the service.
    let script = format!(
        r#"#!/bin/sh
set -e
echo "[triple-c] Downloading Docker install script..."
curl -fsSL https://get.docker.com -o /tmp/triple-c-get-docker.sh
echo "[triple-c] Running Docker install script (may take a few minutes)..."
sh /tmp/triple-c-get-docker.sh
rm -f /tmp/triple-c-get-docker.sh
echo "[triple-c] Adding {user} to docker group..."
usermod -aG docker "{user}" || true
echo "[triple-c] Enabling docker service..."
systemctl enable --now docker 2>/dev/null || service docker start 2>/dev/null || true
echo "[triple-c] Install complete. Log out and back in to use Docker without sudo."
"#,
        user = invoking_user
    );

    let script_path: PathBuf = std::env::temp_dir().join("triple-c-install-docker.sh");
    tokio::fs::write(&script_path, script)
        .await
        .map_err(|e| format!("failed to write install script: {}", e))?;

    let _ = app.emit(
        PROGRESS_EVENT,
        format!("Requesting administrator privileges via pkexec..."),
    );

    let child = Command::new("pkexec")
        .arg("sh")
        .arg(&script_path)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("failed to launch pkexec: {}", e))?;

    let result = stream(app, child).await;
    let _ = tokio::fs::remove_file(&script_path).await;
    result
}

// ─── macOS ───────────────────────────────────────────────────────────────────

pub fn macos_options() -> InstallOptions {
    let has_brew = which("brew");
    InstallOptions {
        os: "macos".into(),
        product_name: "Rancher Desktop".into(),
        can_auto_install: has_brew,
        auto_install_method: if has_brew { Some("brew".into()) } else { None },
        auto_install_blocker: if has_brew {
            None
        } else {
            Some("Homebrew not found — use the manual download.".into())
        },
        docs_url: "https://docs.rancherdesktop.io/getting-started/installation/".into(),
        manual_steps: vec![
            "Download the Rancher Desktop .dmg from the official site.".into(),
            "Open the .dmg and drag Rancher Desktop into Applications.".into(),
            "Launch Rancher Desktop and complete the first-run setup (choose dockerd/moby).".into(),
            "Once the Docker socket is available, come back and click Refresh.".into(),
        ],
        post_install_notes: vec![
            "Launch Rancher Desktop from Applications if it didn't open automatically.".into(),
            "In Preferences, make sure the container engine is set to dockerd (moby).".into(),
        ],
    }
}

async fn run_macos_install(app: &AppHandle) -> Result<(), String> {
    let brew = find_on_path("brew")
        .ok_or_else(|| "Homebrew not found — follow the manual steps instead.".to_string())?;
    let _ = app.emit(
        PROGRESS_EVENT,
        format!("Running: {} install --cask rancher", brew.display()),
    );
    let child = Command::new(&brew)
        .args(["install", "--cask", "rancher"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("failed to launch brew: {}", e))?;
    stream(app, child).await
}

// ─── Windows ─────────────────────────────────────────────────────────────────

pub fn windows_options() -> InstallOptions {
    let has_winget = which("winget");
    InstallOptions {
        os: "windows".into(),
        product_name: "Rancher Desktop".into(),
        can_auto_install: has_winget,
        auto_install_method: if has_winget { Some("winget".into()) } else { None },
        auto_install_blocker: if has_winget {
            None
        } else {
            Some("winget not found — use the manual download.".into())
        },
        docs_url: "https://docs.rancherdesktop.io/getting-started/installation/".into(),
        manual_steps: vec![
            "Download the Rancher Desktop .msi from the official site.".into(),
            "Run the installer and accept the WSL2 prompts if asked.".into(),
            "Launch Rancher Desktop and complete the first-run setup (choose dockerd/moby).".into(),
            "Once the Docker engine is running, come back and click Refresh.".into(),
        ],
        post_install_notes: vec![
            "Launch Rancher Desktop from the Start menu if it didn't open automatically.".into(),
            "In Preferences > Container Engine, make sure dockerd (moby) is selected.".into(),
        ],
    }
}

async fn run_windows_install(app: &AppHandle) -> Result<(), String> {
    let _ = app.emit(
        PROGRESS_EVENT,
        "Running: winget install --id SUSE.RancherDesktop -e --accept-package-agreements --accept-source-agreements".to_string(),
    );
    let child = Command::new("winget")
        .args([
            "install",
            "--id",
            "SUSE.RancherDesktop",
            "-e",
            "--accept-package-agreements",
            "--accept-source-agreements",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("failed to launch winget: {}", e))?;
    stream(app, child).await
}

// ─── Dispatcher ──────────────────────────────────────────────────────────────

pub async fn run_install(app: &AppHandle) -> Result<(), String> {
    if cfg!(target_os = "linux") {
        run_linux_install(app).await
    } else if cfg!(target_os = "macos") {
        run_macos_install(app).await
    } else if cfg!(target_os = "windows") {
        run_windows_install(app).await
    } else {
        Err("auto-install is not supported on this OS".into())
    }
}
