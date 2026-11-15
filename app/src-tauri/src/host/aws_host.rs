//! `ProjectHost::Aws`: a project's container is an ECS Fargate task in one
//! of the account's environments, reached through an SSM port-forward to
//! the in-task agent (`ssm.rs`, `agent.rs`).
//!
//! Start: connection → `SdkConfig` → caller identity → environment record →
//! onboarding → the project's live task, if there is one (its token is read
//! back from the project secret, P2 — **a running task's token is never
//! rotated**); otherwise `prepare_workspace` (which rotates the token) →
//! access point available → RunTask → ready → tunnel → agent health →
//! registry ([`super::tunnels`]). A task this start ran is stopped again on
//! any later failure, so nothing billed is left behind.
//!
//! The decisions are made by [`start_core`] over the `aws/` traits and a
//! [`Connector`], so fakes exercise them; AWS SDK calls stay in `aws/`.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use aws_config::SdkConfig;
use futures_util::future::BoxFuture;

use super::agent::{self, AgentEndpoint};
use super::ssm::{EcsTask, PortForward, AGENT_PORT};
use super::tunnels::{self, LiveTunnel, TunnelHandle};
use super::watch::{AwsTaskLookup, TaskGroup, WatchedTask};
use super::ContainerTarget;
use crate::aws::environments::EnvironmentInfo;
use crate::aws::runtime::{
    check_onboarded, cluster_arn_of_task, cluster_of_task, read_agent_token, read_environment,
    remove_workspace_resources, wait_access_point, PollTiming, RemoveRequest, RuntimeApi,
    SdkRuntimeApi,
};
use crate::aws::tasks::{
    find_or_run, find_project_task, stopped_among, task_log_stream, wait_ready, SdkTaskApi,
    TaskApi, TaskInfo, TaskLogs, WaitTiming, LOG_TAIL_LINES, STOP_REASON,
};
use crate::aws::tofu_modules::{log_stream_prefix, owner_key, task_family};
use crate::aws::workspace::{prepare_workspace, SdkWorkspaceApi, WorkspaceApi};
use crate::aws::{caller_identity, sdk_config, CallerIdentity};
use crate::docker::ca_certs::{container_cert_name, ResolvedCa};
use crate::models::app_settings::ImageSource;
use crate::models::container_config::REGISTRY_IMAGE;
use crate::models::{AppSettings, Project, ProjectStatus};
use crate::storage::projects_store::ProjectsStore;
use crate::storage::settings_store::SettingsStore;

// Progress strings the start view shows, in order (Task 8 matches on none
// of them; they are for the user).
pub const PROGRESS_READING: &str = "Reading environment…";
pub const PROGRESS_PREPARING: &str = "Preparing workspace…";
pub const PROGRESS_STARTING: &str = "Starting task…";
pub const PROGRESS_WAITING: &str = "Waiting for the task (about 2 minutes)…";
pub const PROGRESS_CONNECTING: &str = "Connecting…";

/// A concurrent start won the RunTask race: the secret now holds a token
/// that task never saw.
pub const START_RACE: &str =
    "Another start of this workspace is in progress; try again in a minute";

/// Per-file cap on what is shipped in the project secret (R1).
pub const MAX_FILE_BYTES: u64 = 64 * 1024;
/// How long the agent gets to answer through a fresh tunnel.
const HEALTH_TIMEOUT: Duration = Duration::from_secs(30);
const HEALTH_INTERVAL: Duration = Duration::from_secs(1);
/// How long a terminal waits for a background re-attach.
pub const REATTACH_WAIT: Duration = Duration::from_secs(90);

fn s<E: std::fmt::Display>(e: E) -> String {
    e.to_string()
}

// ─────────────────────────────────────────────────────────────── inputs

/// The image an AWS task runs (P5): registry images only.
pub fn aws_image(settings: &AppSettings) -> Result<String, String> {
    match settings.image_source {
        ImageSource::Registry => Ok(REGISTRY_IMAGE.to_string()),
        ImageSource::LocalBuild => {
            log::info!("AWS projects run the registry image; the local build exists only on this machine");
            Ok(REGISTRY_IMAGE.to_string())
        }
        ImageSource::Custom => {
            let name = settings.custom_image_name.as_deref().unwrap_or("").trim();
            if is_registry_reference(name) {
                Ok(name.to_string())
            } else {
                Err(format!(
                    "The custom image \"{}\" is not a registry reference, so AWS can't pull it. \
                     Use a name like registry.example.com/team/image:tag (Settings → Docker).",
                    name.escape_debug()
                ))
            }
        }
    }
}

/// `host/path[:tag]` or `user/repo[:tag]`: something Fargate can pull. A
/// bare name (a local tag) or a loopback registry is not.
fn is_registry_reference(name: &str) -> bool {
    if name.is_empty() || name.len() > 512 || name.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return false;
    }
    let Some((first, rest)) = name.split_once('/') else {
        return false;
    };
    if first.is_empty() || rest.is_empty() || rest.starts_with('/') {
        return false;
    }
    let host = first.split(':').next().unwrap_or("");
    !(host == "localhost" || host.starts_with("127.") || host == "[::1]")
}

/// [`aws_env`]'s answer: the pairs, and the resolved CA whose files ship too.
pub type AwsEnv = (Vec<(String, String)>, Option<ResolvedCa>);

/// The environment pairs an AWS task gets: [`workspace_env`] with the same
/// inputs Docker resolves, and Claude Code's sandbox forced off (R4: Fargate
/// support for bubblewrap is unverified). Returns the resolved CA too.
///
/// [`workspace_env`]: crate::docker::workspace_env::workspace_env
pub fn aws_env(project: &Project, settings: &AppSettings) -> Result<AwsEnv, String> {
    let mut p = project.clone();
    if p.sandbox_mode_enabled {
        log::info!(
            "Project {}: Claude Code's sandbox is turned off on AWS (not yet supported on Fargate)",
            project.id
        );
        p.sandbox_mode_enabled = false;
    }
    crate::docker::workspace_env::workspace_env_from_settings(&p, settings)
}

/// One file read for the secret, refusing anything over [`MAX_FILE_BYTES`].
fn read_capped(path: &Path, shown: &str) -> Result<Vec<u8>, String> {
    let len = std::fs::metadata(path)
        .map_err(|e| format!("Could not read {shown}: {e}"))?
        .len();
    if len > MAX_FILE_BYTES {
        return Err(format!(
            "{shown} is {len} bytes; files sent to an AWS workspace are limited to {} KiB each",
            MAX_FILE_BYTES / 1024
        ));
    }
    std::fs::read(path).map_err(|e| format!("Could not read {shown}: {e}"))
}

/// The regular files directly in `dir`: subdirectories skipped, and a
/// symlink only when it resolves to a file inside `dir`.
fn files_in(dir: &Path) -> Result<Vec<(String, PathBuf)>, String> {
    let root = std::fs::canonicalize(dir)
        .map_err(|e| format!("Could not read {}: {e}", dir.display()))?;
    let mut out = Vec::new();
    let entries =
        std::fs::read_dir(&root).map_err(|e| format!("Could not read {}: {e}", dir.display()))?;
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = entry.file_name().to_str().map(str::to_string) else {
            log::warn!("Skipping {} (not a UTF-8 name)", path.display());
            continue;
        };
        let Ok(ft) = entry.file_type() else { continue };
        let real = if ft.is_file() {
            path.clone()
        } else if ft.is_symlink() {
            match std::fs::canonicalize(&path) {
                Ok(t) if t.starts_with(&root) && t.is_file() => t,
                _ => {
                    log::warn!("Skipping {} (a link out of the folder, or not to a file)", path.display());
                    continue;
                }
            }
        } else {
            continue;
        };
        out.push((name, real));
    }
    out.sort();
    Ok(out)
}

/// The files the entrypoint writes to `/tmp/.host-ssh` and `/tmp/.host-ca`
/// (R1), as `ssh/<file>` / `ca/<file>` pairs: the project's effective SSH
/// key folder (or file) and the resolved CA, the same sources Docker mounts.
pub fn workspace_files(
    ssh_path: Option<&str>,
    ca: Option<&ResolvedCa>,
) -> Result<Vec<(String, Vec<u8>)>, String> {
    let mut out = Vec::new();
    if let Some(ssh) = ssh_path.map(str::trim).filter(|p| !p.is_empty()) {
        crate::commands::project_commands::validate_mounted_host_path(
            "the SSH key folder",
            None,
            Some(ssh),
        )?;
        let path = Path::new(ssh);
        if path.is_dir() {
            for (name, real) in files_in(path)? {
                let bytes = read_capped(&real, &format!("SSH file {name}"))?;
                out.push((format!("ssh/{name}"), bytes));
            }
        } else if path.is_file() {
            let name = path
                .file_name()
                .and_then(|n| n.to_str())
                .ok_or_else(|| format!("The SSH key path {ssh} has no usable file name"))?;
            out.push((format!("ssh/{name}"), read_capped(path, &format!("SSH file {name}"))?));
        } else {
            return Err(format!("The SSH key path {ssh} does not exist"));
        }
    }
    if let Some(ca) = ca {
        crate::commands::project_commands::validate_mounted_host_path(
            "the CA certificate path",
            None,
            Some(&ca.host_path),
        )?;
        if ca.is_dir {
            let allowed: Vec<PathBuf> = ca.cert_files.clone();
            for (name, real) in files_in(Path::new(&ca.host_path))? {
                // Only what `collect_cert_files` picked (by extension).
                let picked = allowed.iter().any(|f| {
                    f.file_name().and_then(|n| n.to_str()) == Some(name.as_str())
                });
                if picked {
                    out.push((format!("ca/{name}"), read_capped(&real, &format!("certificate {name}"))?));
                }
            }
        } else {
            let path = Path::new(&ca.host_path);
            let name = container_cert_name(
                &path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default(),
            );
            out.push((format!("ca/{name}"), read_capped(path, &format!("certificate {name}"))?));
        }
    }
    Ok(out)
}

// ──────────────────────────────────────────────────────────── connector

/// Opens the tunnel to a ready task and checks its agent answers.
pub trait Connector: Send + Sync {
    fn connect(
        &self,
        task: &TaskInfo,
        token: &str,
    ) -> impl Future<Output = Result<(Box<dyn TunnelHandle>, AgentEndpoint), String>> + Send;
}

/// The real one: an SSM port-forward, then `/v1/health` through it.
pub struct SsmConnector {
    pub sdk: SdkConfig,
}

/// The SSM target of a ready task's `workspace` container.
pub fn ecs_task(task: &TaskInfo) -> Result<EcsTask, String> {
    let cluster = cluster_of_task(&task.arn)
        .ok_or_else(|| format!("{} is not a task ARN with a cluster", task.arn))?;
    let task_id = task.arn.rsplit('/').next().unwrap_or_default().to_string();
    let runtime_id = task
        .runtime_id
        .clone()
        .ok_or_else(|| "The task's container has no runtime id yet".to_string())?;
    Ok(EcsTask { cluster: cluster.to_string(), task_id, runtime_id })
}

/// Wait for the agent behind `ep` to answer. A refusal (wrong token) is
/// final; silence is retried until [`HEALTH_TIMEOUT`].
async fn wait_healthy(ep: &AgentEndpoint) -> Result<(), String> {
    let deadline = tokio::time::Instant::now() + HEALTH_TIMEOUT;
    loop {
        match agent::is_running(ep).await {
            Ok(true) => return Ok(()),
            Ok(false) => {}
            Err(e) => return Err(format!("The workspace's agent refused the connection: {e}")),
        }
        if tokio::time::Instant::now() + HEALTH_INTERVAL > deadline {
            return Err(
                "The workspace's agent did not answer through the tunnel. Check that the image \
                 includes triple-c-agent, then start the project again."
                    .into(),
            );
        }
        tokio::time::sleep(HEALTH_INTERVAL).await;
    }
}

impl Connector for SsmConnector {
    async fn connect(
        &self,
        task: &TaskInfo,
        token: &str,
    ) -> Result<(Box<dyn TunnelHandle>, AgentEndpoint), String> {
        let target = ecs_task(task)?;
        let forward = PortForward::open(&self.sdk, &target, AGENT_PORT).await?;
        let ep = forward.endpoint(token);
        if let Err(e) = wait_healthy(&ep).await {
            forward.close().await;
            return Err(e);
        }
        Ok((Box::new(forward), ep))
    }
}

// ─────────────────────────────────────────────────────────────── start

/// What [`start_core`] needs besides the APIs.
pub struct StartInputs<'a> {
    pub project: &'a Project,
    pub who: &'a CallerIdentity,
    pub env_pairs: &'a [(String, String)],
    pub files: &'a [(String, Vec<u8>)],
    pub image: &'a str,
    pub wait: WaitTiming,
    pub access_point_wait: PollTiming,
}

/// A connected task.
pub struct Started {
    pub env: EnvironmentInfo,
    pub task: TaskInfo,
    pub forward: Box<dyn TunnelHandle>,
    pub endpoint: AgentEndpoint,
    /// The access point the task mounts (Hybrid). `None` when an existing
    /// task was attached to: the store's value stands.
    pub access_point_id: Option<String>,
    /// This start ran the task (rather than attaching to a running one).
    pub created: bool,
}

/// Append the task's last log lines to a wait failure that lacks them.
async fn with_log_tail<T: TaskApi>(api: &T, logs: &TaskLogs, arn: &str, err: String) -> String {
    if err.contains("Last log lines") || err.contains("wrote no log lines") || err.contains("log could not be read") {
        return err;
    }
    match api.log_tail(&logs.group, &task_log_stream(&logs.stream_prefix, arn), LOG_TAIL_LINES).await {
        Ok(lines) if !lines.is_empty() => format!("{err}\nLast log lines:\n{}", lines.join("\n")),
        _ => err,
    }
}

/// Stop a task this start ran, best-effort, so nothing billed is orphaned.
async fn stop_created<T: TaskApi>(api: &T, cluster: &str, arn: &str) {
    if let Err(e) = api.stop_task(cluster, arn, STOP_REASON).await {
        log::warn!("Could not stop task {arn} after a failed start: {e}");
    }
}

/// The start sequence; see the module docs.
pub async fn start_core<T: TaskApi, W: WorkspaceApi, R: RuntimeApi, C: Connector>(
    tasks: &T,
    work: &W,
    rt: &R,
    connector: &C,
    inp: &StartInputs<'_>,
    progress: &(dyn Fn(&str) + Send + Sync),
) -> Result<Started, String> {
    let project = inp.project;
    let (placement, cfg) = match (&project.aws_placement, &project.aws) {
        (Some(p), Some(c)) if project.is_aws() => (p, c),
        _ => return Err("The project is not set up to run on AWS".into()),
    };
    let region = placement.region.as_str();
    let ok = owner_key(region, &inp.who.user_id);

    progress(PROGRESS_READING);
    let env = read_environment(rt, &placement.environment, &inp.who.account, region)
        .await
        .map_err(s)?;
    check_onboarded(rt, &env.name, &ok).await.map_err(s)?;
    let family = task_family(&env.name, &ok, &project.id);
    let logs = TaskLogs {
        group: env.log_group.clone(),
        stream_prefix: log_stream_prefix(&ok, &project.id),
    };

    let existing = find_project_task(tasks, &env.cluster_arn, &family, &inp.who.user_id, &project.id)
        .await
        .map_err(s)?;

    let (task, token, access_point_id, created) = if let Some(task) = existing {
        // Attach: never rotate a running task's token (P2).
        log::info!("Project {}: attaching to its running task {}", project.id, task.arn);
        let token = read_agent_token(rt, &env.name, &ok, &project.id).await.map_err(s)?;
        progress(PROGRESS_WAITING);
        let task = match wait_ready(tasks, &env.cluster_arn, &task.arn, &logs, inp.wait, |_| {}).await {
            Ok(t) => t,
            Err(e) => return Err(with_log_tail(tasks, &logs, &task.arn, e.to_string()).await),
        };
        (task, token, None, false)
    } else {
        progress(PROGRESS_PREPARING);
        let prepared = prepare_workspace(
            work,
            inp.who,
            &env,
            project,
            inp.env_pairs,
            inp.files,
            inp.image,
        )
        .await
        .map_err(s)?;
        if let Some(ap) = &prepared.access_point_id {
            wait_access_point(rt, ap, inp.access_point_wait).await.map_err(s)?;
        }
        progress(PROGRESS_STARTING);
        let start = find_or_run(
            tasks,
            &env,
            &inp.who.user_id,
            &project.id,
            &prepared.task_definition_arn,
            cfg.spot,
        )
        .await
        .map_err(s)?;
        if start.reused {
            return Err(START_RACE.into());
        }
        progress(PROGRESS_WAITING);
        let arn = start.task.arn.clone();
        let task = match wait_ready(tasks, &env.cluster_arn, &arn, &logs, inp.wait, |_| {}).await {
            Ok(t) => t,
            Err(e) => {
                let msg = with_log_tail(tasks, &logs, &arn, e.to_string()).await;
                stop_created(tasks, &env.cluster_arn, &arn).await;
                return Err(msg);
            }
        };
        (task, prepared.agent_token, prepared.access_point_id, true)
    };

    progress(PROGRESS_CONNECTING);
    match connector.connect(&task, &token).await {
        Ok((forward, endpoint)) => Ok(Started {
            env,
            task,
            forward,
            endpoint,
            access_point_id,
            created,
        }),
        Err(e) => {
            if created {
                stop_created(tasks, &env.cluster_arn, &task.arn).await;
            }
            Err(e)
        }
    }
}

/// The connection, region and SDK config of an AWS project.
async fn project_sdk(project: &Project, settings: &AppSettings) -> Result<(String, String, SdkConfig), String> {
    let host_id = project.host_id.as_deref().ok_or("The project has no host")?;
    let region = project
        .aws_placement
        .as_ref()
        .map(|p| p.region.clone())
        .ok_or("The project has no AWS placement")?;
    let conn = crate::commands::host_commands::aws_conn(host_id, settings)?;
    let sdk = sdk_config(&conn, &region).await.map_err(s)?;
    Ok((host_id.to_string(), region, sdk))
}

/// `ProjectHost::Aws::ensure_started`.
pub async fn ensure_started(
    project: &Project,
    settings: &AppSettings,
    progress: &(dyn Fn(&str) + Send + Sync),
) -> Result<ContainerTarget, String> {
    // Already connected and answering: nothing to do.
    if let Some(ep) = tunnels::get_endpoint(&project.id) {
        if matches!(agent::is_running(&ep).await, Ok(true)) {
            return Ok(ContainerTarget::Agent(ep));
        }
        tunnels::close(&project.id).await;
    }

    // Everything local first: a bad image or file fails before any AWS call.
    let image = aws_image(settings)?;
    let (env_pairs, ca) = aws_env(project, settings)?;
    let ssh = project
        .ssh_key_path
        .as_deref()
        .or(settings.default_ssh_key_path.as_deref());
    let files = workspace_files(ssh, ca.as_ref())?;

    let (connection_id, region, sdk) = project_sdk(project, settings).await?;
    let who = caller_identity(&sdk).await.map_err(s)?;
    let started = start_core(
        &SdkTaskApi::new(&sdk),
        &SdkWorkspaceApi::new(&sdk),
        &SdkRuntimeApi::new(&sdk),
        &SsmConnector { sdk: sdk.clone() },
        &StartInputs {
            project,
            who: &who,
            env_pairs: &env_pairs,
            files: &files,
            image: &image,
            wait: WaitTiming::default(),
            access_point_wait: PollTiming::default(),
        },
        progress,
    )
    .await?;

    log::info!(
        "Project {}: {} task {}",
        project.id,
        if started.created { "started" } else { "attached to" },
        started.task.arn
    );
    let endpoint = started.endpoint.clone();
    let access_point_id = started
        .access_point_id
        .or_else(|| project.aws.as_ref().and_then(|a| a.access_point_id.clone()));
    let ecs = ecs_task(&started.task)?;
    if let Some(old) = tunnels::register(
        &project.id,
        LiveTunnel {
            task: ecs,
            task_arn: started.task.arn.clone(),
            cluster_arn: started.env.cluster_arn.clone(),
            connection_id,
            region,
            sdk,
            forward: started.forward,
            endpoint: endpoint.clone(),
            access_point_id,
            task_definition_arn: started.task.task_definition_arn.clone(),
        },
    ) {
        old.forward.close().await;
    }
    Ok(ContainerTarget::Agent(endpoint))
}

// ──────────────────────────────────────────────────────────────── stop

/// StopTask the task in `task_arn`'s cluster.
pub async fn stop_task_arn<T: TaskApi>(api: &T, task_arn: &str) -> Result<(), String> {
    let cluster = cluster_of_task(task_arn)
        .ok_or_else(|| format!("{task_arn} is not a task ARN with a cluster"))?;
    api.stop_task(cluster, task_arn, STOP_REASON).await.map_err(s)
}

/// `ProjectHost::stop` for an agent target: the task behind a registered
/// tunnel. `None` when `ep` is not one of ours.
pub async fn stop_endpoint(ep: &AgentEndpoint) -> Option<Result<(), String>> {
    let (_connection, _region, cluster, arn) = tunnels::task_of(ep)?;
    let (project_id, sdk) = tunnels::owner_of(ep)?;
    let res = SdkTaskApi::new(&sdk)
        .stop_task(&cluster, &arn, STOP_REASON)
        .await
        .map_err(s);
    if res.is_ok() {
        tunnels::close(&project_id).await;
    }
    Some(res)
}

/// Stop an AWS project's task: through its tunnel when there is one, else
/// the stored task ARN, else whatever live task the project has (a crash
/// between RunTask and the store write). Closes the tunnel.
pub async fn stop_project(project: &Project, settings: &AppSettings) -> Result<(), String> {
    if let Some(ep) = tunnels::get_endpoint(&project.id) {
        if let Some(res) = stop_endpoint(&ep).await {
            return res;
        }
    }
    let (_, region, sdk) = project_sdk(project, settings).await?;
    let api = SdkTaskApi::new(&sdk);
    let stored = project.aws.as_ref().and_then(|a| a.task_arn.clone());
    let arn = match stored {
        Some(a) => Some(a),
        None => {
            let env = project.aws_placement.as_ref().map(|p| p.environment.clone()).unwrap_or_default();
            let who = caller_identity(&sdk).await.map_err(s)?;
            let ok = owner_key(&region, &who.user_id);
            find_project_task(&api, &format!("triple-c-{env}"), &task_family(&env, &ok, &project.id), &who.user_id, &project.id)
                .await
                .map_err(s)?
                .map(|t| t.arn)
        }
    };
    if let Some(arn) = arn {
        stop_task_arn(&api, &arn).await?;
    }
    tunnels::close(&project.id).await;
    Ok(())
}

// ────────────────────────────────────────────────────────────── remove

/// Stop the task and remove the project's AWS resources (task definition
/// revisions, the secret with a 7-day window, and with `discard_data` the
/// access point).
pub async fn remove_resources(
    project: &Project,
    settings: &AppSettings,
    discard_data: bool,
) -> Result<(), String> {
    tunnels::close(&project.id).await;
    let (_, region, sdk) = project_sdk(project, settings).await?;
    let who = caller_identity(&sdk).await.map_err(s)?;
    let ok = owner_key(&region, &who.user_id);
    let env = project.aws_placement.as_ref().map(|p| p.environment.clone()).unwrap_or_default();
    let aws = project.aws.as_ref();
    remove_workspace_resources(
        &SdkTaskApi::new(&sdk),
        &SdkWorkspaceApi::new(&sdk),
        &SdkRuntimeApi::new(&sdk),
        &RemoveRequest {
            env: &env,
            region: &region,
            owner_user_id: &who.user_id,
            owner_key: &ok,
            project_id: &project.id,
            stored_task_arn: aws.and_then(|a| a.task_arn.as_deref()),
            stored_access_point_id: aws.and_then(|a| a.access_point_id.as_deref()),
            discard_data,
        },
    )
    .await
    .map_err(s)
}

// ─────────────────────────────────────────────────────────── reconcile

/// What reconcile found for a Running AWS project.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reconciled {
    /// The task is up (or on its way): re-open the tunnel.
    Live(Box<TaskInfo>),
    /// The task stopped, or ECS no longer knows it: the project is Stopped.
    Gone,
}

/// Ask ECS about a project's stored task. No stored task is `Gone`.
pub async fn reconcile_task<T: TaskApi>(api: &T, task_arn: Option<&str>) -> Result<Reconciled, String> {
    let Some(arn) = task_arn else {
        return Ok(Reconciled::Gone);
    };
    let Some(cluster) = cluster_of_task(arn) else {
        return Ok(Reconciled::Gone);
    };
    let found = api.describe(cluster, &[arn.to_string()]).await.map_err(s)?;
    Ok(match found.into_iter().find(|t| t.arn == arn) {
        Some(t)
            if t.desired_status == "RUNNING"
                && matches!(t.last_status.as_str(), "PROVISIONING" | "PENDING" | "ACTIVATING" | "RUNNING") =>
        {
            Reconciled::Live(Box::new(t))
        }
        _ => Reconciled::Gone,
    })
}

/// Reconcile one AWS project the store has as Running (or Starting/
/// Stopping/Error left by a crash): a gone task makes it Stopped (its task
/// ARN cleared); a live one keeps it Running. An ECS that can't be asked
/// changes nothing — marking Stopped would hide a billed task.
pub async fn reconcile_project<T: TaskApi>(
    api: &T,
    store: &ProjectsStore,
    project: &Project,
) -> Result<Reconciled, String> {
    let arn = project.aws.as_ref().and_then(|a| a.task_arn.clone());
    let r = reconcile_task(api, arn.as_deref()).await?;
    match &r {
        Reconciled::Gone => {
            store.update_status(&project.id, ProjectStatus::Stopped)?;
            if let Some(a) = project.aws.as_ref() {
                store.set_aws_runtime(&project.id, None, a.access_point_id.clone(), a.task_definition_arn.clone())?;
            }
        }
        Reconciled::Live(_) => {
            if project.status != ProjectStatus::Running {
                store.update_status(&project.id, ProjectStatus::Running)?;
            }
        }
    }
    Ok(r)
}

/// `reconcile_project_statuses` for an AWS project: decide, and when the
/// task is live re-open its tunnel in the background (terminals wait for
/// it through [`tunnels::wait_endpoint`]). Never blocks on the tunnel.
pub async fn reconcile(project: &Project, settings: &AppSettings, store: Arc<ProjectsStore>) {
    let (connection_id, region, sdk) = match project_sdk(project, settings).await {
        Ok(v) => v,
        Err(e) => {
            log::warn!("Project {}: could not reach AWS to reconcile ({e}); status left as is", project.id);
            return;
        }
    };
    let api = SdkTaskApi::new(&sdk);
    match reconcile_project(&api, &store, project).await {
        Ok(Reconciled::Live(task)) => {
            tunnels::set_pending(&project.id, true);
            let project = project.clone();
            tauri::async_runtime::spawn(async move {
                if let Err(e) = reattach(&project, *task, connection_id, region, sdk, &store).await {
                    log::warn!("Project {}: could not re-open the tunnel to its task: {e}", project.id);
                    tunnels::set_pending(&project.id, false);
                }
            });
        }
        Ok(Reconciled::Gone) => {
            log::info!("Project '{}' ({}): its task is gone — set to Stopped", project.name, project.id)
        }
        Err(e) => log::warn!("Project {}: could not check its task ({e}); status left as is", project.id),
    }
}

/// Re-open the tunnel to a live task found at startup.
async fn reattach(
    project: &Project,
    task: TaskInfo,
    connection_id: String,
    region: String,
    sdk: SdkConfig,
    store: &ProjectsStore,
) -> Result<(), String> {
    let env = project.aws_placement.as_ref().map(|p| p.environment.clone()).unwrap_or_default();
    let who = caller_identity(&sdk).await.map_err(s)?;
    let ok = owner_key(&region, &who.user_id);
    let rt = SdkRuntimeApi::new(&sdk);
    let api = SdkTaskApi::new(&sdk);
    let token = read_agent_token(&rt, &env, &ok, &project.id).await.map_err(s)?;
    let cluster_arn = cluster_arn_of_task(&task.arn).ok_or("bad task ARN")?;
    let logs = TaskLogs { group: format!("/triple-c/{env}"), stream_prefix: log_stream_prefix(&ok, &project.id) };
    let task = wait_ready(&api, &cluster_arn, &task.arn, &logs, WaitTiming::default(), |_| {})
        .await
        .map_err(s)?;
    let (forward, endpoint) = SsmConnector { sdk: sdk.clone() }.connect(&task, &token).await?;
    if let Some(old) = tunnels::register(
        &project.id,
        LiveTunnel {
            task: ecs_task(&task)?,
            task_arn: task.arn.clone(),
            cluster_arn,
            connection_id,
            region,
            sdk,
            forward,
            endpoint: endpoint.clone(),
            access_point_id: project.aws.as_ref().and_then(|a| a.access_point_id.clone()),
            task_definition_arn: task.task_definition_arn.clone(),
        },
    ) {
        old.forward.close().await;
    }
    // The local port is new, so the stored target id is too.
    store.set_container_id(&project.id, Some(endpoint.base_url.to_string()))?;
    log::info!("Project {}: tunnel to task {} re-opened", project.id, task.arn);
    Ok(())
}

// ─────────────────────────────────────────────────────────────── watch

/// The AWS death watch's lookup: Running AWS projects from the store, their
/// targets from the registry (or, while unattached, an endpoint with the
/// stored id so `container_watch::settle` still matches), and
/// `stopped_among` per connection/region/cluster.
pub struct StoreTaskLookup {
    pub projects: Arc<ProjectsStore>,
    pub settings: Arc<SettingsStore>,
}

/// A Running AWS project's watched task, if it has one.
pub fn watched_task(p: &Project) -> Option<WatchedTask> {
    if p.status != ProjectStatus::Running || !p.is_aws() {
        return None;
    }
    let arn = p.aws.as_ref()?.task_arn.clone()?;
    let target = match tunnels::get_endpoint(&p.id) {
        Some(ep) => ep,
        None => AgentEndpoint::new(url::Url::parse(p.container_id.as_deref()?).ok()?, "unattached").ok()?,
    };
    if Some(target.base_url.as_str()) != p.container_id.as_deref() {
        return None;
    }
    Some(WatchedTask {
        target: ContainerTarget::Agent(target),
        group: TaskGroup {
            connection_id: p.host_id.clone()?,
            region: p.aws_placement.as_ref()?.region.clone(),
            cluster_arn: cluster_arn_of_task(&arn)?,
        },
        task_arn: arn,
    })
}

impl AwsTaskLookup for StoreTaskLookup {
    fn running_tasks(&self) -> Vec<WatchedTask> {
        self.projects.list().iter().filter_map(watched_task).collect()
    }

    fn stopped<'a>(
        &'a self,
        group: &'a TaskGroup,
        arns: &'a [String],
    ) -> BoxFuture<'a, Result<Vec<String>, String>> {
        Box::pin(async move {
            let settings = self.settings.get();
            let conn = crate::commands::host_commands::aws_conn(&group.connection_id, &settings)?;
            let sdk = sdk_config(&conn, &group.region).await.map_err(s)?;
            let stopped = stopped_among(&SdkTaskApi::new(&sdk), &group.cluster_arn, arns)
                .await
                .map_err(s)?;
            record_stopped(&self.projects.list(), &stopped);
            Ok(stopped)
        })
    }
}

/// Tell the registry which targets' tasks ECS reports stopped, so
/// `has_exited` confirms them (R3).
pub fn record_stopped(projects: &[Project], stopped: &[String]) {
    for w in projects.iter().filter_map(watched_task) {
        if stopped.contains(&w.task_arn) {
            tunnels::mark_stopped(w.target.id());
        }
    }
}

#[cfg(test)]
mod tests;
