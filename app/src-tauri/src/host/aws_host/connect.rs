//! Copy-at-connect (phase 6, ruling Q2): an AWS project whose credentials
//! are "copied when you connect" keeps them out of its Secrets Manager
//! secret (`aws::workspace`, `PayloadExtras::connect_only`) and brings them
//! in after every connect instead — a start, an attach, the background
//! re-attach when the app starts.
//!
//! The credentials are [`ConnectBundle`]: the SSH and CA files (the same
//! [`workspace_files`](super::workspace_files) the secret would carry) and
//! the credential environment pairs (`SECRET_ENV_KEYS` and every custom
//! variable, `aws::workspace::credential_pairs`). [`push_connect_bundle`]
//! uploads them through the agent, as root, into root-only staging on the
//! task's `/dev/shm` tmpfs and runs `triple-c-apply-connect` (container), which
//! installs the CA, merges the SSH files, writes the git credential helper
//! and re-clones what failed. The terminals the app opens get the pairs in
//! their own `ExecSpec.env` ([`terminal_env`]); nothing puts them into the
//! agent's environment, so other execs, cron jobs and the scheduler never
//! see them. **Values are never logged**: errors carry names and the
//! script's own (value-free) output only.

use std::time::Duration;

use zeroize::Zeroizing;

use super::super::{tunnels, ContainerTarget};
use crate::aws::workspace::{credential_pairs, validate_file_key};
use crate::models::project::SecretsDelivery;
use crate::models::{AppSettings, Project};

/// The in-container half (container/triple-c-apply-connect).
pub const APPLY_CONNECT: &str = "/usr/local/bin/triple-c-apply-connect";
/// Root 0700 on the tmpfs (made by the entrypoint for the agent token).
pub const CONNECT_SHM: &str = "/dev/shm/triple-c";
/// Staging for the files: `ssh/` and `ca/` under it.
pub const CONNECT_STAGE: &str = "/dev/shm/triple-c/connect";
pub const CONNECT_ENV_FILE: &str = "connect.env";
/// Every staged file is root 0600.
const STAGED_MODE: u32 = 0o600;
/// The whole push (a few uploads and two execs through the tunnel).
pub const PUSH_TIMEOUT: Duration = Duration::from_secs(120);
/// The most of the script's output a warning carries.
const OUTPUT_TAIL_LINES: usize = 10;

/// A credential value, zeroed when dropped (the settings export's
/// precedent). It narrows, not closes, the window: the values were read from
/// the keychain into a `Project`, and the copies an `ExecSpec` or an upload
/// body needs are plain buffers owned by those layers.
pub type SecretBytes = Zeroizing<Vec<u8>>;
pub type SecretString = Zeroizing<String>;

/// What copy-at-connect copies in.
#[derive(Clone, Default)]
pub struct ConnectBundle {
    /// `ssh/<file>` / `ca/<file>`, as in the secret's `TRIPLEC_FILES`.
    pub files: Vec<(String, SecretBytes)>,
    /// The credential environment pairs.
    pub env: Vec<(String, SecretString)>,
}

/// Names only: a bundle holds keys and tokens.
impl std::fmt::Debug for ConnectBundle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConnectBundle")
            .field("files", &self.files.iter().map(|(k, _)| k.as_str()).collect::<Vec<_>>())
            .field("env", &self.env.iter().map(|(k, _)| k.as_str()).collect::<Vec<_>>())
            .finish()
    }
}

/// Whether `project` copies its credentials in at connect.
pub fn copies_at_connect(project: &Project) -> bool {
    project.is_aws()
        && project
            .aws
            .as_ref()
            .is_some_and(|a| a.secrets_delivery == SecretsDelivery::CopyAtConnect)
}

/// The bundle for `project`, whose keychain secrets must already be loaded
/// (`load_secrets_for_project`): the same sources the secret would use.
pub fn connect_credentials(project: &Project, settings: &AppSettings) -> Result<ConnectBundle, String> {
    let (_, ca) = super::aws_env(project, settings)?;
    let ssh = project
        .ssh_key_path
        .as_deref()
        .or(settings.default_ssh_key_path.as_deref());
    let files = super::workspace_files(ssh, ca.as_ref())?
        .into_iter()
        .map(|(k, v)| (k, Zeroizing::new(v)))
        .collect();
    Ok(ConnectBundle { files, env: connect_env(project, settings) })
}

/// Only the credential pairs of `project` (secrets loaded): no file is read,
/// so a missing SSH or CA file never keeps the API keys from a terminal.
pub fn connect_env(project: &Project, settings: &AppSettings) -> Vec<(String, SecretString)> {
    let pairs = crate::docker::workspace_env::workspace_env_with_ca(project, settings, None);
    let custom =
        crate::docker::workspace_env::custom_env_keys(project, &settings.global_custom_env_vars);
    credential_pairs(&pairs, &custom)
        .into_iter()
        .map(|(k, v)| (k, Zeroizing::new(v)))
        .collect()
}

/// `project` as the store holds it, with its keychain secrets read in.
fn with_secrets(project: &Project) -> Project {
    let mut p = project.clone();
    crate::commands::project_commands::load_secrets_for_project(&mut p);
    p
}

/// `connect.env`: one `KEY=<base64 of the value>` line per pair, so a value
/// with a newline or `=` reads back whole and nothing has to be quoted.
pub fn connect_env_file(env: &[(String, SecretString)]) -> SecretBytes {
    use base64::Engine;
    let b64 = base64::engine::general_purpose::STANDARD;
    let mut out = Zeroizing::new(String::new());
    for (k, v) in env {
        out.push_str(k);
        out.push('=');
        b64.encode_string(v.as_bytes(), &mut out);
        out.push('\n');
    }
    Zeroizing::new(out.as_bytes().to_vec())
}

/// `KEY=VALUE` strings for an `ExecSpec`.
pub fn exec_env(env: &[(String, SecretString)]) -> Vec<String> {
    env.iter().map(|(k, v)| format!("{k}={}", v.as_str())).collect()
}

/// What a push needs from the container: root execs and root uploads.
pub trait ConnectTarget: Send + Sync {
    fn exec_root(
        &self,
        cmd: Vec<String>,
    ) -> impl std::future::Future<Output = Result<(String, i64), String>> + Send;
    fn upload_root(
        &self,
        dir: &str,
        name: &str,
        data: &[u8],
        mode: u32,
    ) -> impl std::future::Future<Output = Result<(), String>> + Send;
}

impl ConnectTarget for ContainerTarget {
    async fn exec_root(&self, cmd: Vec<String>) -> Result<(String, i64), String> {
        self.exec_oneshot_as("root", cmd, Vec::new()).await
    }
    async fn upload_root(&self, dir: &str, name: &str, data: &[u8], mode: u32) -> Result<(), String> {
        self.upload_bytes_with_ids(dir, name, data, mode, (0, 0)).await.map(|_| ())
    }
}

fn tail(output: &str) -> String {
    let lines: Vec<&str> = output.lines().filter(|l| !l.trim().is_empty()).collect();
    lines[lines.len().saturating_sub(OUTPUT_TAIL_LINES)..].join("\n")
}

async fn run_apply<T: ConnectTarget>(t: &T, args: &[&str]) -> Result<String, String> {
    let mut cmd = vec![APPLY_CONNECT.to_string()];
    cmd.extend(args.iter().map(|a| a.to_string()));
    let (out, code) = t.exec_root(cmd).await?;
    match code {
        0 => Ok(out),
        // `exec` of a missing file: an image from before copy-at-connect.
        126 | 127 => Err(
            "This workspace's image has no triple-c-apply-connect (it predates copying \
             credentials at connect); use a newer image, or store the credentials in AWS"
                .into(),
        ),
        _ => Err(format!("triple-c-apply-connect failed ({code}):\n{}", tail(&out))),
    }
}

/// Upload `bundle` and apply it (see the module docs): `--prepare`, the
/// files and `connect.env` (root 0600), then the apply.
pub async fn push_connect_bundle<T: ConnectTarget>(t: &T, bundle: &ConnectBundle) -> Result<(), String> {
    run_apply(t, &["--prepare"]).await?;
    for (key, data) in &bundle.files {
        validate_file_key(key)?;
        let (kind, name) = key.split_once('/').expect("validated");
        t.upload_root(&format!("{CONNECT_STAGE}/{kind}"), name, data, STAGED_MODE)
            .await
            .map_err(|e| format!("Could not copy {key} into the workspace: {e}"))?;
    }
    t.upload_root(CONNECT_SHM, CONNECT_ENV_FILE, &connect_env_file(&bundle.env), STAGED_MODE)
        .await
        .map_err(|e| format!("Could not copy the credential variables into the workspace: {e}"))?;
    let out = run_apply(t, &[]).await?;
    for line in out.lines().filter(|l| l.contains("warning")) {
        log::warn!("{line}");
    }
    Ok(())
}

/// Copy `project`'s credentials into its connected workspace. `Ok(false)`
/// when there is nothing to do (not copy-at-connect, or no tunnel).
pub async fn apply_connect(project: &Project, settings: &AppSettings) -> Result<bool, String> {
    if !copies_at_connect(project) {
        return Ok(false);
    }
    let Some(ep) = tunnels::get_endpoint(&project.id) else {
        return Ok(false);
    };
    let (p, s) = (project.clone(), settings.clone());
    let bundle = tokio::task::spawn_blocking(move || connect_credentials(&with_secrets(&p), &s))
        .await
        .map_err(|e| format!("Could not gather the credentials: {e}"))??;
    let target = ContainerTarget::Agent(ep);
    tokio::time::timeout(PUSH_TIMEOUT, push_connect_bundle(&target, &bundle))
        .await
        .map_err(|_| format!("Copying the credentials took more than {} s", PUSH_TIMEOUT.as_secs()))??;
    log::info!(
        "Project {}: credentials copied into the workspace ({} file(s), {} variable(s))",
        project.id,
        bundle.files.len(),
        bundle.env.len()
    );
    Ok(true)
}

/// After a connect: [`apply_connect`] in the background, `on_warning` on a
/// failure while the workspace is still connected (a Stop meanwhile is not
/// worth a warning). `false` (nothing spawned) unless the project copies at
/// connect and is connected.
pub fn spawn_connect_push(
    project: &Project,
    settings: &AppSettings,
    on_warning: impl FnOnce(String) + Send + 'static,
) -> bool {
    if !copies_at_connect(project) || tunnels::get_endpoint(&project.id).is_none() {
        return false;
    }
    let (project, settings) = (project.clone(), settings.clone());
    tauri::async_runtime::spawn(async move {
        if let Err(e) = apply_connect(&project, &settings).await {
            log::warn!("Project {}: could not copy the credentials in: {e}", project.id);
            if tunnels::get_endpoint(&project.id).is_some() {
                on_warning(e);
            }
        }
    });
    true
}

/// The environment a terminal the app opens adds to its exec: the
/// credential pairs for a copy-at-connect project, nothing otherwise (they
/// are already in the task's environment). Only the pairs ([`connect_env`]:
/// no SSH or CA file is read); the keychain is read off the async runtime.
pub async fn terminal_env(project: &Project, settings: &AppSettings) -> Vec<String> {
    if !copies_at_connect(project) {
        return Vec::new();
    }
    let (p, s) = (project.clone(), settings.clone());
    let gathered = tokio::task::spawn_blocking(move || connect_env(&with_secrets(&p), &s))
        .await
        .map_err(|e| format!("could not gather them: {e}"));
    terminal_env_with(project, || gathered)
}

/// [`terminal_env`] over a given source of the pairs.
pub fn terminal_env_with(
    project: &Project,
    credentials: impl FnOnce() -> Result<Vec<(String, SecretString)>, String>,
) -> Vec<String> {
    if !copies_at_connect(project) {
        return Vec::new();
    }
    match credentials() {
        Ok(env) => exec_env(&env),
        Err(e) => {
            log::warn!(
                "Project {}: the terminal opens without the project's credentials: {e}",
                project.id
            );
            Vec::new()
        }
    }
}
