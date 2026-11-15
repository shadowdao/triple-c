//! The OpenTofu runner: a pinned, hash-verified `tofu` binary, the work
//! folder a deployment is applied from, the child environment it runs in,
//! and a detached `init` + `apply|destroy` whose log the app follows (D10).
//!
//! **Detached.** An apply must survive the app closing, so it is not a child
//! the app waits on. The app re-executes *its own binary* with the hidden
//! `--tofu-runner <workdir> <apply|destroy>` argument (`main.rs` hands that to
//! [`runner_main`] before anything Tauri or GTK starts). The runner runs
//! `tofu init` then `tofu apply|destroy`, appending both to `apply.log`, and
//! writes the exit code to `apply.exit` — always, even when init fails or
//! `tofu` cannot be started. No shell is involved anywhere. On Unix the
//! runner is its own session (`setsid`), so its process group is the
//! Cancel target; on Windows it is `DETACHED_PROCESS |
//! CREATE_NEW_PROCESS_GROUP`.
//!
//! **Credentials** reach `tofu` only through the environment the runner is
//! started with ([`child_env`]), never argv, files or the log. The runner
//! itself writes only its own status lines to the log, never its
//! environment.

use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::time::Duration;

use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::error::AwsError;
use super::tofu_modules::{module_files, state_key, Deployment, ModuleKind};
use crate::models::host_connection::{
    validate_supported_region, AwsAuth, HostConnection, HostKind,
};
use crate::storage::secure::AwsKeys;

/// The OpenTofu release the app downloads (the modules' `required_version`
/// admits it; Task 1 verified the modules against it).
pub const TOFU_VERSION: &str = "1.13.9";

#[cfg(windows)]
const TOFU_BIN: &str = "tofu.exe";
#[cfg(not(windows))]
const TOFU_BIN: &str = "tofu";

/// `(os, arch, sha256)` of `tofu_<ver>_<os>_<arch>.tar.gz`, copied from the
/// release's `tofu_1.13.9_SHA256SUMS`, whose detached GPG signature
/// (`tofu_1.13.9_SHA256SUMS.gpgsig`) verified against OpenTofu's signing key
/// `E3E6 E43D 84CB 852E ADB0 051D 0C0A F313 E5FD 9F80`
/// (https://get.opentofu.org/opentofu.asc). Every platform uses the
/// `.tar.gz` — OpenTofu publishes one for Windows too — so one extractor
/// covers them all. To bump: take the new SUMS file, verify its signature
/// the same way, and never hash your own download.
const RELEASES: &[(&str, &str, &str)] = &[
    (
        "linux",
        "amd64",
        "b34b327cc1ef07178207b6f2cd1f5276537d3e5b7be316500f6dac50d4c0a857",
    ),
    (
        "linux",
        "arm64",
        "5dc196dde75b667e4806d358310ec96742774cd0993c54c1237efee5644b69b8",
    ),
    (
        "darwin",
        "amd64",
        "347e2e5a2cfe2f368829572d308b2e7c1e0486b20a1779090ff06360f7551e4f",
    ),
    (
        "darwin",
        "arm64",
        "496f4c3064b5c3bff79174b3dae4faaf65a66774eb8e2bf074f895412c5f124f",
    ),
    (
        "windows",
        "amd64",
        "e0772c2fb51e68b58f8dad47452ae965478b696cfa4c42054b1da5b7538df4ef",
    ),
];

/// A release archive is ~30 MB; anything far past that is not one.
const MAX_ARCHIVE_BYTES: u64 = 256 * 1024 * 1024;

const RUNNER_FLAG: &str = "--tofu-runner";
/// How the runner learns which `tofu` to run. A path, not a secret; the
/// runner removes it before starting `tofu`.
const TOFU_BIN_ENV: &str = "TRIPLEC_TOFU_BIN";
const LOG_FILE: &str = "apply.log";
const PID_FILE: &str = "apply.pid";
const EXIT_FILE: &str = "apply.exit";

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

fn other(message: impl Into<String>) -> AwsError {
    AwsError::Other {
        message: message.into(),
    }
}

/// The release file and its pinned SHA-256 for a platform. Accepts Rust's
/// names (`macos`, `x86_64`, `aarch64`) as well as OpenTofu's. Only the
/// platforms the app ships on are listed.
pub fn release_asset(os: &str, arch: &str) -> Option<(String, &'static str)> {
    let os = match os {
        "macos" | "darwin" => "darwin",
        "linux" => "linux",
        "windows" => "windows",
        _ => return None,
    };
    let arch = match arch {
        "x86_64" | "amd64" => "amd64",
        "aarch64" | "arm64" => "arm64",
        _ => return None,
    };
    RELEASES
        .iter()
        .find(|(o, a, _)| *o == os && *a == arch)
        .map(|(o, a, sha)| (format!("tofu_{TOFU_VERSION}_{o}_{a}.tar.gz"), *sha))
}

pub(crate) fn sha256_file(path: &Path) -> io::Result<String> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

/// Check `archive` against `expected_sha256` and only then extract the
/// `tofu` binary from it into `dest` (mode 755). Nothing else in the archive
/// is written. On a mismatch the archive is deleted and nothing is extracted.
pub fn verify_and_extract(
    archive: &Path,
    expected_sha256: &str,
    dest: &Path,
) -> Result<PathBuf, AwsError> {
    let actual = sha256_file(archive)
        .map_err(|e| other(format!("Could not read the OpenTofu download: {e}")))?;
    if !actual.eq_ignore_ascii_case(expected_sha256) {
        let _ = fs::remove_file(archive);
        return Err(other(format!(
            "The OpenTofu download did not match its pinned SHA-256 (expected {expected_sha256}, got {actual}); it was deleted"
        )));
    }

    let file = File::open(archive)
        .map_err(|e| other(format!("Could not open the OpenTofu download: {e}")))?;
    let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(file));
    let entries = tar
        .entries()
        .map_err(|e| other(format!("The OpenTofu archive is unreadable: {e}")))?;
    for entry in entries {
        let mut entry =
            entry.map_err(|e| other(format!("The OpenTofu archive is unreadable: {e}")))?;
        let is_bin = entry
            .path()
            .map(|p| {
                p.components()
                    .map(|c| c.as_os_str())
                    .eq([std::ffi::OsStr::new(TOFU_BIN)])
                    || p == Path::new(".").join(TOFU_BIN)
            })
            .unwrap_or(false);
        if !is_bin || !entry.header().entry_type().is_file() {
            continue;
        }
        let tmp = dest.join(format!(".{TOFU_BIN}.{}.tmp", rand::random::<u64>()));
        let result = (|| -> io::Result<()> {
            let mut out = File::create(&tmp)?;
            let copied = io::copy(&mut (&mut entry).take(MAX_ARCHIVE_BYTES), &mut out)?;
            if copied >= MAX_ARCHIVE_BYTES {
                return Err(io::Error::other("the binary is implausibly large"));
            }
            out.sync_all()?;
            drop(out);
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&tmp, fs::Permissions::from_mode(0o755))?;
            }
            fs::rename(&tmp, dest.join(TOFU_BIN))
        })();
        if let Err(e) = result {
            let _ = fs::remove_file(&tmp);
            return Err(other(format!("Could not extract OpenTofu: {e}")));
        }
        return Ok(dest.join(TOFU_BIN));
    }
    Err(other(format!("The OpenTofu archive has no {TOFU_BIN}")))
}

/// `<root>/tofu/<ver>/tofu[.exe]`, downloaded and verified on first use.
/// A binary already there is reused only if it still hashes to what was
/// recorded when it was extracted from a verified archive.
#[allow(dead_code)] // used by phase-4 commands
pub async fn ensure_tofu(root: &Path) -> Result<PathBuf, AwsError> {
    let (file, sha) = release_asset(std::env::consts::OS, std::env::consts::ARCH)
        .ok_or_else(|| other("OpenTofu isn't available for this platform"))?;
    let dir = root.join("tofu").join(TOFU_VERSION);
    fs::create_dir_all(&dir)
        .map_err(|e| other(format!("Could not create {}: {e}", dir.display())))?;
    let bin = dir.join(TOFU_BIN);
    let marker = dir.join(format!("{TOFU_BIN}.sha256"));

    let (b, m) = (bin.clone(), marker.clone());
    let reusable = tokio::task::spawn_blocking(move || {
        let recorded = fs::read_to_string(&m).ok()?;
        let actual = sha256_file(&b).ok()?;
        Some(recorded.trim() == actual)
    })
    .await
    .ok()
    .flatten()
    .unwrap_or(false);
    if reusable {
        return Ok(bin);
    }
    let _ = fs::remove_file(&bin);
    let _ = fs::remove_file(&marker);

    let url =
        format!("https://github.com/opentofu/opentofu/releases/download/v{TOFU_VERSION}/{file}");
    let archive = dir.join(format!("{file}.{}.part", rand::random::<u64>()));
    let downloaded = download(&url, &archive).await;
    if let Err(e) = downloaded {
        let _ = fs::remove_file(&archive);
        return Err(e);
    }
    let (a, d) = (archive.clone(), dir.clone());
    let extracted = tokio::task::spawn_blocking(move || {
        let bin = verify_and_extract(&a, sha, &d)?;
        let _ = fs::remove_file(&a);
        let hash = sha256_file(&bin).map_err(|e| other(format!("Could not hash OpenTofu: {e}")))?;
        fs::write(d.join(format!("{TOFU_BIN}.sha256")), hash)
            .map_err(|e| other(format!("Could not record the OpenTofu hash: {e}")))?;
        Ok::<_, AwsError>(bin)
    })
    .await
    .map_err(|e| other(format!("OpenTofu extraction failed: {e}")))?;
    if extracted.is_err() {
        let _ = fs::remove_file(&archive);
    }
    extracted
}

async fn download(url: &str, to: &Path) -> Result<(), AwsError> {
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(20))
        .timeout(Duration::from_secs(600))
        .build()
        .map_err(|e| other(format!("Could not start the OpenTofu download: {e}")))?;
    let mut resp = client
        .get(url)
        .send()
        .await
        .and_then(|r| r.error_for_status())
        .map_err(|e| other(format!("Could not download OpenTofu: {e}")))?;
    let mut out = File::create(to)
        .map_err(|e| other(format!("Could not save the OpenTofu download: {e}")))?;
    let mut total = 0u64;
    while let Some(chunk) = resp
        .chunk()
        .await
        .map_err(|e| other(format!("The OpenTofu download was interrupted: {e}")))?
    {
        total += chunk.len() as u64;
        if total > MAX_ARCHIVE_BYTES {
            return Err(other("The OpenTofu download is implausibly large"));
        }
        out.write_all(&chunk)
            .map_err(|e| other(format!("Could not save the OpenTofu download: {e}")))?;
    }
    out.sync_all()
        .map_err(|e| other(format!("Could not save the OpenTofu download: {e}")))
}

/// The folder one deployment is initialised and applied from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Workdir {
    pub dir: PathBuf,
}

/// One path component of a work folder: no separators, no `.`/`..`, and
/// only characters that are also safe inside a quoted HCL string.
fn safe_segment(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 128
        && s != "."
        && s != ".."
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// Write `kind`'s module into `<root>/work/<bucket>/<state key without
/// .tfstate>/` with `backend.hcl` and `terraform.tfvars.json` (0600 on
/// Unix). Refuses while a run is in progress there. `.tf` files and the lock
/// file left by an older module version are removed first, so a file a later
/// version dropped can't linger and still be loaded.
#[allow(dead_code)] // used by phase-4 commands
pub fn materialize(
    root: &Path,
    kind: ModuleKind,
    deployment: Deployment,
    bucket: &str,
    region: &str,
    vars: &Value,
) -> Result<Workdir, AwsError> {
    if !safe_segment(bucket) {
        return Err(other("Invalid state bucket name"));
    }
    if region.trim() != region {
        return Err(other("Invalid region"));
    }
    let region = validate_supported_region(region).map_err(other)?;
    let key = state_key(deployment);
    let stem = key
        .strip_suffix(".tfstate")
        .ok_or_else(|| other("Invalid state key"))?;
    if !stem.split('/').all(safe_segment) {
        return Err(other("Invalid deployment name"));
    }
    let mut dir = root.join("work").join(bucket);
    dir.extend(stem.split('/'));
    let wd = Workdir { dir };
    if is_running(&wd) {
        return Err(other("A deploy is already running for this deployment"));
    }

    let io = |e: io::Error| other(format!("Could not write the work folder: {e}"));
    fs::create_dir_all(&wd.dir).map_err(io)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(root.join("work"), fs::Permissions::from_mode(0o700)).map_err(io)?;
    }
    for entry in fs::read_dir(&wd.dir).map_err(io)? {
        let entry = entry.map_err(io)?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let module_file =
            name.ends_with(".tf") || name.ends_with(".tf.json") || name == ".terraform.lock.hcl";
        if module_file && entry.file_type().map_err(io)?.is_file() {
            fs::remove_file(entry.path()).map_err(io)?;
        }
    }
    for (name, text) in module_files(kind) {
        fs::write(wd.dir.join(name), text).map_err(io)?;
    }
    let backend = format!(
        "bucket = \"{bucket}\"\nkey = \"{key}\"\nregion = \"{region}\"\nencrypt = true\nuse_lockfile = true\n"
    );
    fs::write(wd.dir.join("backend.hcl"), backend).map_err(io)?;
    let json = serde_json::to_vec_pretty(vars)
        .map_err(|e| other(format!("Could not encode the module variables: {e}")))?;
    write_private(&wd.dir.join("terraform.tfvars.json"), &json).map_err(io)?;
    Ok(wd)
}

/// Replace `path` with a file only its owner can read (Unix). The old file
/// is removed first, since opening an existing file keeps its mode.
fn write_private(path: &Path, bytes: &[u8]) -> io::Result<()> {
    match fs::remove_file(path) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
        _ => {}
    }
    let mut opts = OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts.open(path)?;
    f.write_all(bytes)?;
    f.sync_all()
}

/// What `tofu` may see of the app's own environment. An allowlist, not
/// "everything minus AWS_/TF_/TOFU_": anything else the app was started with
/// (`LD_PRELOAD`, tokens, the AppImage's library paths) has no business in
/// an apply.
/// - `PATH`: `credential_process`/SSO helpers and the AppImage runtime's
///   `fusermount`.
/// - `HOME` / `USERPROFILE`: where the AWS provider finds `~/.aws` (config,
///   credentials, SSO cache) for a profile connection.
/// - `TMPDIR` / `TMP` / `TEMP`: scratch space for tofu and its provider.
/// - `SystemRoot`: Go's networking and crypto fail on Windows without it.
/// - The proxy variables and `SSL_CERT_FILE` / `SSL_CERT_DIR`: a corporate
///   network needs them for the provider download and every AWS call.
const INHERITED: &[&str] = &[
    "PATH",
    "HOME",
    "USERPROFILE",
    "TMPDIR",
    "TMP",
    "TEMP",
    "SystemRoot",
    "HTTPS_PROXY",
    "HTTP_PROXY",
    "NO_PROXY",
    "https_proxy",
    "http_proxy",
    "no_proxy",
    "SSL_CERT_FILE",
    "SSL_CERT_DIR",
];

fn inherited(name: &str) -> bool {
    INHERITED.iter().any(|a| {
        if cfg!(windows) {
            a.eq_ignore_ascii_case(name)
        } else {
            *a == name
        }
    })
}

/// The complete environment for the runner and the `tofu` it starts: the
/// [`INHERITED`] variables from `parent` (the real caller passes
/// `std::env::vars()`), the connection's own credentials — `AWS_PROFILE` for
/// a profile, the three key variables for a keys connection — and the
/// app's `TF_*` / `CHECKPOINT_*` settings. Nothing else AWS-, TF- or
/// TOFU-shaped. A profile uses the default `~/.aws` paths, as
/// `aws/connection.rs` does.
#[allow(dead_code)] // used by phase-4 commands
pub fn child_env(
    conn: &HostConnection,
    keys: Option<&AwsKeys>,
    plugin_cache: &Path,
    parent: impl IntoIterator<Item = (String, String)>,
) -> Vec<(String, String)> {
    let mut env: Vec<(String, String)> = parent.into_iter().filter(|(k, _)| inherited(k)).collect();
    let HostKind::Aws { auth, .. } = &conn.kind;
    match auth {
        AwsAuth::Profile { name } => env.push(("AWS_PROFILE".into(), name.clone())),
        AwsAuth::Keys { .. } => {
            if let Some(keys) = keys {
                env.push(("AWS_ACCESS_KEY_ID".into(), keys.access_key_id.clone()));
                env.push((
                    "AWS_SECRET_ACCESS_KEY".into(),
                    keys.secret_access_key.clone(),
                ));
                if let Some(token) = &keys.session_token {
                    env.push(("AWS_SESSION_TOKEN".into(), token.clone()));
                }
            }
        }
    }
    env.push((
        "TF_PLUGIN_CACHE_DIR".into(),
        plugin_cache.to_string_lossy().into_owned(),
    ));
    env.push(("TF_IN_AUTOMATION".into(), "1".into()));
    env.push(("TF_INPUT".into(), "0".into()));
    env.push(("CHECKPOINT_DISABLE".into(), "1".into()));
    env
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TofuOp {
    Apply,
    Destroy,
}

impl TofuOp {
    fn as_arg(self) -> &'static str {
        match self {
            TofuOp::Apply => "apply",
            TofuOp::Destroy => "destroy",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        match s {
            "apply" => Some(TofuOp::Apply),
            "destroy" => Some(TofuOp::Destroy),
            _ => None,
        }
    }
}

/// The program to re-execute as the runner. Inside an AppImage that is the
/// AppImage file, not `current_exe()`: the latter lives on the AppImage's
/// FUSE mount, which is unmounted when the app exits — under a runner that
/// is meant to outlive it.
fn runner_program() -> io::Result<PathBuf> {
    #[cfg(target_os = "linux")]
    if let Some(appimage) = std::env::var_os("APPIMAGE").map(PathBuf::from) {
        if appimage.is_file() {
            return Ok(appimage);
        }
    }
    std::env::current_exe()
}

fn remove_if_present(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

/// Start `tofu init` then `apply|destroy` in a detached runner and return
/// its PID (also written to `apply.pid`). The previous run's log and exit
/// code are cleared first. `env` is the runner's entire environment
/// ([`child_env`]); credentials go nowhere else.
#[allow(dead_code)] // used by phase-4 commands
pub fn start_detached(
    tofu: &Path,
    wd: &Workdir,
    op: TofuOp,
    env: &[(String, String)],
) -> Result<u32, AwsError> {
    if is_running(wd) {
        return Err(other("A deploy is already running for this deployment"));
    }
    let io = |e: io::Error| other(format!("Could not start the deploy: {e}"));
    for f in [LOG_FILE, EXIT_FILE, PID_FILE] {
        remove_if_present(&wd.dir.join(f)).map_err(io)?;
    }
    let mut cmd = Command::new(runner_program().map_err(io)?);
    cmd.arg(RUNNER_FLAG)
        .arg(&wd.dir)
        .arg(op.as_arg())
        .env_clear()
        .envs(env.iter().map(|(k, v)| (k, v)))
        .env(TOFU_BIN_ENV, tofu)
        .current_dir(&wd.dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // SAFETY: setsid is async-signal-safe and touches no Rust state.
        unsafe {
            cmd.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
    }
    let mut child = cmd.spawn().map_err(io)?;
    let pid = child.id();
    let tmp = wd.dir.join(format!("{PID_FILE}.tmp"));
    fs::write(&tmp, pid.to_string())
        .and_then(|_| fs::rename(&tmp, wd.dir.join(PID_FILE)))
        .map_err(io)?;
    // Reap it if it finishes while the app is still open (no zombie).
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(pid)
}

fn read_pid(wd: &Workdir) -> Option<u32> {
    fs::read_to_string(wd.dir.join(PID_FILE))
        .ok()?
        .trim()
        .parse()
        .ok()
        .filter(|p| *p > 0)
}

#[cfg(unix)]
fn pid_alive(pid: u32) -> bool {
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return false;
    };
    // SAFETY: signal 0 only checks that the process exists.
    let r = unsafe { libc::kill(pid, 0) };
    r == 0 || io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(windows)]
fn pid_alive(pid: u32) -> bool {
    use std::os::windows::process::CommandExt;
    // No Win32 bindings in the tree; tasklist is always present.
    Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}"), "/NH", "/FO", "CSV"])
        .creation_flags(CREATE_NO_WINDOW)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).contains(&format!("\"{pid}\"")))
        .unwrap_or(false)
}

/// A run is in progress: `apply.pid` names a live process and `apply.exit`
/// doesn't exist yet. (A PID reused after a reboot can read as live; the
/// state lock is the other half of "a deploy is in progress".)
pub fn is_running(wd: &Workdir) -> bool {
    !wd.dir.join(EXIT_FILE).exists() && read_pid(wd).is_some_and(pid_alive)
}

/// Cancel: SIGINT to the runner's process group, so `tofu` stops gracefully
/// and saves state. The runner survives it and still writes `apply.exit`.
#[allow(dead_code)] // used by phase-4 commands
pub fn interrupt(wd: &Workdir) -> Result<(), AwsError> {
    if !is_running(wd) {
        return Err(other("No deploy is running for this deployment"));
    }
    #[cfg(unix)]
    {
        let pid = read_pid(wd).ok_or_else(|| other("No deploy is running"))?;
        let pgid = libc::pid_t::try_from(pid).map_err(|_| other("Invalid runner PID"))?;
        // SAFETY: plain kill(2) on the runner's own process group (setsid
        // made its PID the group id).
        if unsafe { libc::kill(-pgid, libc::SIGINT) } != 0 {
            return Err(other(format!(
                "Could not interrupt the deploy: {}",
                io::Error::last_os_error()
            )));
        }
        Ok(())
    }
    #[cfg(windows)]
    {
        Err(other(
            "Cancelling a running deploy isn't supported on Windows yet",
        ))
    }
}

/// One line of `apply.log`.
#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct TofuEvent {
    pub ts: String,
    pub level: String,
    /// The `-json` message's `type`; `"log"` for plain text (init, runner).
    pub kind: String,
    pub resource: Option<String>,
    pub action: Option<String>,
    pub message: String,
}

/// A log line can be long (a diagnostic's detail); the UI needs a readable
/// message, not a megabyte.
const MAX_MESSAGE_CHARS: usize = 4000;

fn capped(s: &str) -> String {
    match s.char_indices().nth(MAX_MESSAGE_CHARS) {
        Some((i, _)) => format!("{}…", &s[..i]),
        None => s.to_string(),
    }
}

/// A `tofu -json` line as an event; any other non-blank line becomes a
/// `"log"` event. `apply_errored` is reported at level `error` (tofu itself
/// logs it as `info`), and a diagnostic carries its detail.
pub fn parse_log_line(line: &str) -> Option<TofuEvent> {
    let line = line.trim_end_matches(['\r', '\n']);
    if line.trim().is_empty() {
        return None;
    }
    let plain = || TofuEvent {
        ts: String::new(),
        level: "info".into(),
        kind: "log".into(),
        resource: None,
        action: None,
        message: capped(line),
    };
    let Ok(Value::Object(m)) = serde_json::from_str::<Value>(line.trim()) else {
        return Some(plain());
    };
    let s = |v: Option<&Value>| v.and_then(Value::as_str).map(str::to_string);
    let kind = s(m.get("type")).unwrap_or_else(|| "log".into());
    let mut level = s(m.get("@level")).unwrap_or_else(|| "info".into());
    let mut message = s(m.get("@message")).unwrap_or_default();
    let hook = m.get("hook").or_else(|| m.get("change"));
    let mut resource = s(hook.and_then(|h| h.pointer("/resource/addr")));
    let action = s(hook.and_then(|h| h.get("action")));
    if kind == "apply_errored" {
        level = "error".into();
    }
    if let Some(d) = m.get("diagnostic") {
        if let Some(sev) = s(d.get("severity")) {
            level = sev;
        }
        resource = resource.or_else(|| s(d.get("address")));
        if let Some(detail) = s(d.get("detail")).filter(|d| !d.trim().is_empty()) {
            message = format!("{message}: {}", detail.trim());
        }
    }
    Some(TofuEvent {
        ts: s(m.get("@timestamp")).unwrap_or_default(),
        level,
        kind,
        resource,
        action,
        message: capped(&message),
    })
}

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct TofuOutcome {
    pub ok: bool,
    pub exit_code: i32,
    /// The last change summary ("Apply complete! …") on success; the last
    /// error otherwise.
    pub summary: Option<String>,
}

fn read_new(path: &Path, offset: u64) -> io::Result<Vec<u8>> {
    let mut f = match File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    f.seek(SeekFrom::Start(offset))?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf)?;
    Ok(buf)
}

/// Follow `apply.log` from byte 0, handing each complete line's event to
/// `on_events` exactly once, until `apply.exit` appears. Reattaching after a
/// restart is the same call. If the runner dies without an exit code (killed,
/// power loss) the outcome is a failure rather than waiting forever.
#[allow(dead_code)] // used by phase-4 commands
pub async fn follow(
    wd: &Workdir,
    poll: Duration,
    mut on_events: impl FnMut(&[TofuEvent]),
) -> Result<TofuOutcome, AwsError> {
    let log = wd.dir.join(LOG_FILE);
    let exit = wd.dir.join(EXIT_FILE);
    let mut offset = 0u64;
    let mut pending: Vec<u8> = Vec::new();
    let mut last_summary: Option<String> = None;
    let mut last_error: Option<String> = None;
    loop {
        // Checked before reading: the runner writes the exit code only after
        // tofu has written its last line, so this read sees everything.
        let finished = exit.exists();
        let runner_gone =
            !finished && read_pid(wd).is_some_and(|p| !pid_alive(p)) && !exit.exists();
        let new = read_new(&log, offset)
            .map_err(|e| other(format!("Could not read the deploy log: {e}")))?;
        offset += new.len() as u64;
        pending.extend_from_slice(&new);

        let mut events = Vec::new();
        while let Some(pos) = pending.iter().position(|b| *b == b'\n') {
            let line: Vec<u8> = pending.drain(..=pos).collect();
            events.extend(parse_log_line(&String::from_utf8_lossy(&line)));
        }
        if (finished || runner_gone) && !pending.is_empty() {
            events.extend(parse_log_line(&String::from_utf8_lossy(&pending)));
            pending.clear();
        }
        for e in &events {
            if e.kind == "change_summary" {
                last_summary = Some(e.message.clone());
            }
            if e.level == "error" {
                last_error = Some(e.message.clone());
            }
        }
        if !events.is_empty() {
            on_events(&events);
        }

        if finished {
            let text = fs::read_to_string(&exit)
                .map_err(|e| other(format!("Could not read the deploy's exit code: {e}")))?;
            let exit_code = text.trim().parse::<i32>().unwrap_or(-1);
            let ok = exit_code == 0;
            let summary = if ok {
                last_summary
            } else {
                last_error.or(last_summary)
            };
            return Ok(TofuOutcome {
                ok,
                exit_code,
                summary,
            });
        }
        if runner_gone {
            return Ok(TofuOutcome {
                ok: false,
                exit_code: -1,
                summary: Some(
                    "The deploy stopped without reporting a result; the state lock may still be held"
                        .into(),
                ),
            });
        }
        tokio::time::sleep(poll).await;
    }
}

// ---------------------------------------------------------------------------
// The runner (the app binary re-executed with `--tofu-runner`).

/// `main.rs` calls this first. `None` unless the arguments are
/// `--tofu-runner <workdir> <apply|destroy>`; otherwise the runner's exit
/// code, for the process to exit with.
pub fn runner_main(args: impl IntoIterator<Item = OsString>) -> Option<i32> {
    let args: Vec<OsString> = args.into_iter().skip(1).take(4).collect();
    if args.first().map(|a| a != RUNNER_FLAG).unwrap_or(true) {
        return None;
    }
    let (Some(dir), Some(op), None) = (args.get(1), args.get(2), args.get(3)) else {
        return Some(2);
    };
    let Some(op) = op.to_str().and_then(TofuOp::parse) else {
        return Some(2);
    };
    let dir = PathBuf::from(dir);
    if !dir.is_dir() {
        return Some(2);
    }
    let code = match OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join(LOG_FILE))
    {
        Ok(mut log) => run_steps(&dir, op, &mut log),
        Err(_) => 1,
    };
    write_exit(&dir, code);
    Some(code)
}

#[cfg(unix)]
extern "C" fn ignore_signal(_: libc::c_int) {}

fn run_steps(dir: &Path, op: TofuOp, log: &mut File) -> i32 {
    let Some(tofu) = std::env::var_os(TOFU_BIN_ENV) else {
        let _ = writeln!(log, "triple-c: no OpenTofu binary was given to the runner");
        return 1;
    };
    // Cancel signals the whole group. The runner must outlive it to record
    // the exit code, so it catches SIGINT with a handler that does nothing.
    // A handler, not SIG_IGN: an ignored disposition would be inherited
    // through exec, and tofu would ignore the Cancel too.
    #[cfg(unix)]
    // SAFETY: installs an empty, async-signal-safe handler.
    unsafe {
        libc::signal(
            libc::SIGINT,
            ignore_signal as extern "C" fn(libc::c_int) as libc::sighandler_t,
        );
    }
    let init = [
        "init",
        "-input=false",
        "-no-color",
        "-backend-config=backend.hcl",
    ];
    let code = step(Path::new(&tofu), dir, &init, log);
    if code != 0 {
        let _ = writeln!(log, "triple-c: tofu init failed (exit {code})");
        return code;
    }
    let run = [op.as_arg(), "-auto-approve", "-json", "-input=false"];
    let code = step(Path::new(&tofu), dir, &run, log);
    let _ = writeln!(log, "triple-c: tofu {} finished (exit {code})", op.as_arg());
    code
}

fn step(tofu: &Path, dir: &Path, args: &[&str], log: &mut File) -> i32 {
    let _ = writeln!(log, "triple-c: running tofu {}", args[0]);
    let (Ok(out), Ok(err)) = (log.try_clone(), log.try_clone()) else {
        return 1;
    };
    let mut cmd = Command::new(tofu);
    cmd.args(args)
        .current_dir(dir)
        .env_remove(TOFU_BIN_ENV)
        .stdin(Stdio::null())
        .stdout(out)
        .stderr(err);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    match cmd.status() {
        Ok(status) => exit_code(status),
        Err(e) => {
            let _ = writeln!(log, "triple-c: could not start OpenTofu: {e}");
            127
        }
    }
}

fn exit_code(status: ExitStatus) -> i32 {
    if let Some(code) = status.code() {
        return code;
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(sig) = status.signal() {
            return 128 + sig;
        }
    }
    1
}

/// Written via a rename so a follower never reads a half-written code.
fn write_exit(dir: &Path, code: i32) {
    let tmp = dir.join(format!("{EXIT_FILE}.tmp"));
    if fs::write(&tmp, format!("{code}\n")).is_ok() && fs::rename(&tmp, dir.join(EXIT_FILE)).is_ok()
    {
        return;
    }
    let _ = fs::write(dir.join(EXIT_FILE), format!("{code}\n"));
}

#[cfg(test)]
mod tests;
