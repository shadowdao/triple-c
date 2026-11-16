//! A project's ECS task: find the one already running, or run one; wait
//! until it is ready; stop it; and read why it stopped.
//!
//! **A task is trusted by its tags, not its family.** Family names are built
//! from a 12-character id prefix and any workspace user may register into any
//! `triple-c-*` family, so a task listed under the project's family is reused
//! only when DescribeTasks shows the caller's own `triple-c:owner` (which only
//! the caller can set, at RunTask) and this project's `triple-c:project`.
//!
//! **RunTask names an exact revision.** Never a bare family: "the latest
//! revision" of a family is whatever anyone registered last.
//!
//! **A quota refusal is final.** Fargate's vCPU limit (and its kin) surface as
//! [`AwsError::QuotaExceeded`] with a message saying what to do; nothing here
//! retries a RunTask, and the SDK's own retries are made idempotent by a
//! client token.

use std::future::Future;
use std::time::Duration;

use aws_config::SdkConfig;
use aws_sdk_ecs::operation::run_task::RunTaskInput;
use aws_sdk_ecs::types::{
    AssignPublicIp, AwsVpcConfiguration, CapacityProviderStrategyItem, DesiredStatus, LaunchType,
    ManagedAgentName, NetworkConfiguration, Tag, Task, TaskField,
};

use super::environments::EnvironmentInfo;
use super::error::{classify_sdk_error, AwsError};
pub use super::tofu_modules::task_log_stream;
use super::workspace::{workspace_tags, CONTAINER_NAME, TAG_OWNER, TAG_PROJECT};

/// The `reason` StopTask records when the app stops a task.
pub const STOP_REASON: &str = "Stopped from Triple-C";
/// Log lines shown when a task stops before it is ready.
pub const LOG_TAIL_LINES: i32 = 50;
const SPOT_PROVIDER: &str = "FARGATE_SPOT";
/// DescribeTasks takes at most 100 tasks per call.
const DESCRIBE_BATCH: usize = 100;
/// ListTasks pages read per status; a project has one task, so this is slack.
const MAX_LIST_PAGES: usize = 5;

fn other(message: impl Into<String>) -> AwsError {
    AwsError::Other {
        message: message.into(),
    }
}

/// What the app needs to know about a task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskInfo {
    pub arn: String,
    /// `PROVISIONING`, `PENDING`, `ACTIVATING`, `RUNNING`, `DEACTIVATING`,
    /// `STOPPING`, `DEPROVISIONING`, `STOPPED`.
    pub last_status: String,
    pub desired_status: String,
    /// `HEALTHY`, `UNHEALTHY`, `UNKNOWN`.
    pub health: Option<String>,
    /// ECS `stopCode` (`EssentialContainerExited`, `SpotInterruption`, ...).
    pub stop_code: Option<String>,
    pub stopped_reason: Option<String>,
    pub task_definition_arn: String,
    /// The `workspace` container's runtime id (needed for an SSM target).
    pub runtime_id: Option<String>,
    /// The `workspace` container's ExecuteCommandAgent is RUNNING.
    pub exec_agent_running: bool,
    /// The `workspace` container's exit code, once it has one.
    pub exit_code: Option<i32>,
    /// Its `triple-c:owner` / `triple-c:project` tags (DescribeTasks with
    /// TAGS; RunTask's answer carries the tags it was given).
    pub owner: Option<String>,
    pub project: Option<String>,
}

impl TaskInfo {
    fn is_stopping(&self) -> bool {
        self.last_status == "STOPPED" || self.desired_status == "STOPPED"
    }
    /// On its way up or up, and meant to stay that way.
    fn is_live(&self) -> bool {
        self.desired_status == "RUNNING"
            && matches!(
                self.last_status.as_str(),
                "PROVISIONING" | "PENDING" | "ACTIVATING" | "RUNNING"
            )
    }
    fn is_ready(&self) -> bool {
        self.last_status == "RUNNING" && self.exec_agent_running && self.runtime_id.is_some()
    }
}

fn tag_value(tags: &[Tag], key: &str) -> Option<String> {
    tags.iter()
        .find(|t| t.key() == Some(key))
        .and_then(|t| t.value())
        .map(str::to_string)
}

/// An SDK task as a [`TaskInfo`]. The container facts come from the
/// `workspace` container only.
pub(crate) fn task_info(t: &Task) -> TaskInfo {
    let workspace = t
        .containers()
        .iter()
        .find(|c| c.name() == Some(CONTAINER_NAME));
    TaskInfo {
        arn: t.task_arn().unwrap_or_default().to_string(),
        last_status: t.last_status().unwrap_or_default().to_string(),
        desired_status: t.desired_status().unwrap_or_default().to_string(),
        health: t.health_status().map(|h| h.as_str().to_string()),
        stop_code: t.stop_code().map(|c| c.as_str().to_string()),
        stopped_reason: t.stopped_reason().map(str::to_string),
        task_definition_arn: t.task_definition_arn().unwrap_or_default().to_string(),
        runtime_id: workspace.and_then(|c| c.runtime_id()).map(str::to_string),
        exec_agent_running: workspace.is_some_and(|c| {
            c.managed_agents().iter().any(|a| {
                a.name() == Some(&ManagedAgentName::ExecuteCommandAgent)
                    && a.last_status() == Some("RUNNING")
            })
        }),
        exit_code: workspace.and_then(|c| c.exit_code()),
        owner: tag_value(t.tags(), TAG_OWNER),
        project: tag_value(t.tags(), TAG_PROJECT),
    }
}

/// What running and watching tasks asks AWS. Like `WorkspaceApi`, each
/// method returns a `Send` future and an impl may write them as `async fn`;
/// the decisions are made by the functions below, so a fake exercises them.
pub trait TaskApi: Send + Sync {
    /// RunTask. A quota refusal is [`AwsError::QuotaExceeded`].
    fn run_task(
        &self,
        req: RunTaskInput,
    ) -> impl Future<Output = Result<TaskInfo, AwsError>> + Send;
    /// DescribeTasks (with tags). Tasks ECS no longer knows are left out.
    fn describe(
        &self,
        cluster: &str,
        arns: &[String],
    ) -> impl Future<Output = Result<Vec<TaskInfo>, AwsError>> + Send;
    /// ListTasks of exactly `family`, desired status RUNNING and PENDING.
    fn list_project_tasks(
        &self,
        cluster: &str,
        family: &str,
    ) -> impl Future<Output = Result<Vec<String>, AwsError>> + Send;
    fn stop_task(
        &self,
        cluster: &str,
        arn: &str,
        reason: &str,
    ) -> impl Future<Output = Result<(), AwsError>> + Send;
    /// The last `limit` lines of a log stream, oldest first. A stream (or
    /// group) that does not exist is no lines, not an error.
    fn log_tail(
        &self,
        group: &str,
        stream: &str,
        limit: i32,
    ) -> impl Future<Output = Result<Vec<String>, AwsError>> + Send;
}

// ------------------------------------------------------------- run request

/// The RunTask request: the exact revision, on the environment's cluster and
/// network, with ECS Exec on and exactly the `tags` given (no propagation,
/// no ECS-managed tags, no overrides — P1). Spot runs on the `FARGATE_SPOT`
/// capacity provider with no launch type (the two are exclusive).
pub fn run_task_request(
    env: &EnvironmentInfo,
    task_def_arn: &str,
    spot: bool,
    tags: &[(String, String)],
) -> RunTaskInput {
    const BUILT: &str = "every required field is set";
    let mut req = RunTaskInput::builder()
        .cluster(&env.cluster_arn)
        .task_definition(task_def_arn)
        .count(1)
        .enable_execute_command(true)
        .network_configuration(
            NetworkConfiguration::builder()
                .awsvpc_configuration(
                    AwsVpcConfiguration::builder()
                        .set_subnets(Some(env.subnet_ids.clone()))
                        .security_groups(&env.security_group_id)
                        .assign_public_ip(if env.assign_public_ip {
                            AssignPublicIp::Enabled
                        } else {
                            AssignPublicIp::Disabled
                        })
                        .build()
                        .expect(BUILT),
                )
                .build(),
        )
        .set_tags(Some(
            tags.iter()
                .map(|(k, v)| Tag::builder().key(k).value(v).build())
                .collect(),
        ))
        // SDK retries of this call must not start a second task.
        .client_token(uuid::Uuid::new_v4().to_string());
    req = if spot {
        req.capacity_provider_strategy(
            CapacityProviderStrategyItem::builder()
                .capacity_provider(SPOT_PROVIDER)
                .weight(1)
                .build()
                .expect(BUILT),
        )
    } else {
        req.launch_type(LaunchType::Fargate)
    };
    req.build().expect(BUILT)
}

/// The family of `arn:...:task-definition/<family>:<revision>`, when the
/// revision is a number. `None` for a bare family or anything else.
pub fn revision_family(arn: &str) -> Option<&str> {
    if !arn.starts_with("arn:") {
        return None;
    }
    let (_, rest) = arn.rsplit_once(":task-definition/")?;
    let (family, rev) = rest.rsplit_once(':')?;
    (!family.is_empty()
        && !family.contains('/')
        && !rev.is_empty()
        && rev.bytes().all(|c| c.is_ascii_digit()))
    .then_some(family)
}

// ------------------------------------------------------------- find or run

/// The project's live task (provisioning through running, not being
/// stopped), owned by the caller and tagged with this project. A RUNNING
/// one is preferred.
pub async fn find_project_task<A: TaskApi>(
    api: &A,
    cluster: &str,
    family: &str,
    owner_user_id: &str,
    project_id: &str,
) -> Result<Option<TaskInfo>, AwsError> {
    let arns = api.list_project_tasks(cluster, family).await?;
    if arns.is_empty() {
        return Ok(None);
    }
    let mut mine: Vec<TaskInfo> = Vec::new();
    for chunk in arns.chunks(DESCRIBE_BATCH) {
        mine.extend(api.describe(cluster, chunk).await?.into_iter().filter(|t| {
            t.owner.as_deref() == Some(owner_user_id)
                && t.project.as_deref() == Some(project_id)
                && revision_family(&t.task_definition_arn) == Some(family)
                && t.is_live()
        }));
    }
    mine.sort_by_key(|t| t.last_status != "RUNNING");
    Ok(mine.into_iter().next())
}

/// What [`find_or_run`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskStart {
    pub task: TaskInfo,
    /// An existing task was found (nothing was run). Its agent token is the
    /// one it started with — see [`find_project_task`] in the report for
    /// why a caller should look before rotating the token.
    pub reused: bool,
}

/// The project's live task if there is one (Review Focus 2: never a second
/// task for a project), else RunTask with the exact `task_def_arn`.
pub async fn find_or_run<A: TaskApi>(
    api: &A,
    env: &EnvironmentInfo,
    owner_user_id: &str,
    project_id: &str,
    task_def_arn: &str,
    spot: bool,
) -> Result<TaskStart, AwsError> {
    let family = revision_family(task_def_arn).ok_or_else(|| {
        other(format!(
            "\"{task_def_arn}\" is not an exact task definition revision"
        ))
    })?;
    if !family.starts_with("triple-c-") {
        return Err(other(format!(
            "\"{task_def_arn}\" is not a Triple-C task definition"
        )));
    }
    if let Some(task) =
        find_project_task(api, &env.cluster_arn, family, owner_user_id, project_id).await?
    {
        return Ok(TaskStart { task, reused: true });
    }
    let tags = workspace_tags(owner_user_id, project_id, &env.name);
    let task = api
        .run_task(run_task_request(env, task_def_arn, spot, &tags))
        .await?;
    Ok(TaskStart {
        task,
        reused: false,
    })
}

// ------------------------------------------------------------------ quotas

/// Whether an AWS message says an account limit or quota was hit.
pub fn looks_like_quota(text: &str) -> bool {
    let t = text.to_lowercase();
    [
        "you've reached the limit",
        "you\u{2019}ve reached the limit",
        "limitexceeded",
        "limit exceeded",
        "quota",
    ]
    .iter()
    .any(|s| t.contains(s))
}

/// The error for a RunTask refused on a quota.
pub fn quota_error(detail: &str) -> AwsError {
    AwsError::QuotaExceeded {
        message: format!(
            "AWS would not start the task because an account limit was reached: {detail}. \
             Stop another workspace, or ask an administrator to raise the Fargate quota \
             (Service Quotas, AWS Fargate) for this region. Triple-C does not retry."
        ),
    }
}

/// `e` as a quota error when its message is one; otherwise unchanged.
pub fn quota_or(e: AwsError) -> AwsError {
    match e {
        AwsError::Other { ref message } if looks_like_quota(message) => quota_error(message),
        e => e,
    }
}

/// RunTask's answer: its one task, or an error from its `failures`
/// (`(reason, detail)`), a quota one when they say so.
pub fn run_task_outcome(
    tasks: Vec<TaskInfo>,
    failures: Vec<(Option<String>, Option<String>)>,
) -> Result<TaskInfo, AwsError> {
    if let Some(task) = tasks.into_iter().find(|t| !t.arn.is_empty()) {
        return Ok(task);
    }
    let text = failures
        .iter()
        .map(|(r, d)| match (r, d) {
            (Some(r), Some(d)) => format!("{r} ({d})"),
            (Some(r), None) => r.clone(),
            (None, Some(d)) => d.clone(),
            (None, None) => "unknown failure".into(),
        })
        .collect::<Vec<_>>()
        .join("; ");
    if text.is_empty() {
        return Err(other("ecs:RunTask returned no task and no failure"));
    }
    if looks_like_quota(&text) {
        return Err(quota_error(&text));
    }
    Err(other(format!("ECS could not start the task: {text}")))
}

// -------------------------------------------------------------------- wait

/// How [`wait_ready`] polls. Injectable so tests don't wait.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WaitTiming {
    pub interval: Duration,
    pub timeout: Duration,
}

impl Default for WaitTiming {
    fn default() -> Self {
        WaitTiming {
            interval: Duration::from_secs(5),
            timeout: Duration::from_secs(6 * 60),
        }
    }
}

/// Where a project's task logs go: the environment's log group and the
/// project's `awslogs-stream-prefix` (Task 4's `log_stream_prefix`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskLogs {
    pub group: String,
    pub stream_prefix: String,
}

fn describe_timeout(t: Duration) -> String {
    let secs = t.as_secs();
    if secs >= 60 && secs % 60 == 0 {
        let m = secs / 60;
        format!("{m} minute{}", if m == 1 { "" } else { "s" })
    } else {
        format!("{secs} seconds")
    }
}

/// Poll until the task is RUNNING with its exec agent up and a runtime id
/// (what an SSM session needs), calling `progress` with each look. A task
/// that stops (or is being stopped) fails with its reason and the last
/// [`LOG_TAIL_LINES`] lines of its log; one that never gets there fails
/// after `timing.timeout` with its last status. The task is left as it is
/// either way: stopping it is the caller's call.
pub async fn wait_ready<A: TaskApi>(
    api: &A,
    cluster: &str,
    arn: &str,
    logs: &TaskLogs,
    timing: WaitTiming,
    progress: impl Fn(&TaskInfo),
) -> Result<TaskInfo, AwsError> {
    let deadline = tokio::time::Instant::now() + timing.timeout;
    let wanted = [arn.to_string()];
    loop {
        let Some(t) = api
            .describe(cluster, &wanted)
            .await?
            .into_iter()
            .find(|t| t.arn == arn)
        else {
            return Err(other(format!(
                "The task {arn} is no longer known to ECS; start the project again"
            )));
        };
        progress(&t);
        if t.is_stopping() {
            return Err(stopped_error(api, logs, &t).await);
        }
        if t.is_ready() {
            return Ok(t);
        }
        if tokio::time::Instant::now() + timing.interval > deadline {
            return Err(other(format!(
                "The task did not become ready within {} (last status {}{}). It may still \
                 be starting; check the ECS console, or stop the project and start it again.",
                describe_timeout(timing.timeout),
                t.last_status,
                if t.last_status == "RUNNING" && !t.exec_agent_running {
                    ", ECS Exec agent not running yet"
                } else {
                    ""
                }
            )));
        }
        tokio::time::sleep(timing.interval).await;
    }
}

async fn stopped_error<A: TaskApi>(api: &A, logs: &TaskLogs, t: &TaskInfo) -> AwsError {
    let reason = t
        .stopped_reason
        .clone()
        .unwrap_or_else(|| "no reason given".into());
    let code = t
        .stop_code
        .as_deref()
        .map(|c| format!(" ({c})"))
        .unwrap_or_default();
    let stream = task_log_stream(&logs.stream_prefix, &t.arn);
    let tail = match api.log_tail(&logs.group, &stream, LOG_TAIL_LINES).await {
        Ok(lines) if lines.is_empty() => "\n(The task wrote no log lines.)".to_string(),
        Ok(lines) => format!("\nLast log lines:\n{}", lines.join("\n")),
        Err(e) => format!("\n(Its log could not be read: {e})"),
    };
    other(format!(
        "The task stopped before it was ready: {reason}{code}.{tail}"
    ))
}

// ----------------------------------------------------------- stop / watch

/// The entrypoint's exit status after an idle stop (`container/entrypoint.sh`,
/// `stop_keepalive`). A stop from ECS (SIGTERM) exits 0; this one is distinct
/// so an idle stop (`EssentialContainerExited`) is not mistaken for a crash.
pub const IDLE_EXIT_CODE: i32 = 75;

/// The entrypoint's exit status after the environment's lifetime cap stopped
/// the workspace (`TRIPLEC_MAX_LIFETIME_MINUTES`, enforced by the in-container
/// idle watcher — advisory, see the phase 6 constraints, Q8).
pub const LIFETIME_EXIT_CODE: i32 = 76;

/// Why a task stopped, as the app reports it (spec, "Lifecycle and status").
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StopKind {
    /// Exited on its own (the essential container ended, or AWS retired it),
    /// with ECS's stopped reason when it gave one.
    Exited(Option<String>),
    /// The workspace's own idle stop ([`IDLE_EXIT_CODE`]).
    Idle,
    /// The environment's maximum workspace lifetime ([`LIFETIME_EXIT_CODE`]).
    LifetimeCap,
    /// A Fargate Spot interruption (P8).
    Spot,
    /// Never got going: the project goes to Error with this reason.
    FailedToStart(String),
    /// Stopped by someone (the app, the console, the CLI).
    UserInitiated,
}

pub fn classify_stop(info: &TaskInfo) -> StopKind {
    match info.stop_code.as_deref() {
        Some("SpotInterruption") => StopKind::Spot,
        Some("TaskFailedToStart") => StopKind::FailedToStart(
            info.stopped_reason
                .clone()
                .filter(|r| !r.trim().is_empty())
                .unwrap_or_else(|| "The task failed to start".into()),
        ),
        Some("UserInitiated") => StopKind::UserInitiated,
        Some("EssentialContainerExited") if info.exit_code == Some(IDLE_EXIT_CODE) => StopKind::Idle,
        Some("EssentialContainerExited") if info.exit_code == Some(LIFETIME_EXIT_CODE) => {
            StopKind::LifetimeCap
        }
        _ => StopKind::Exited(info.stopped_reason.clone().filter(|r| !r.trim().is_empty())),
    }
}

/// Of `arns`, the ones that are STOPPED or that ECS no longer knows (it
/// forgets stopped tasks after about an hour), each with why it stopped (a
/// forgotten task: [`StopKind::Exited`] without a reason). A task merely
/// being stopped is not yet reported. For the AWS death watch.
pub async fn stopped_among<A: TaskApi>(
    api: &A,
    cluster: &str,
    arns: &[String],
) -> Result<Vec<(String, StopKind)>, AwsError> {
    let mut stopped = Vec::new();
    for chunk in arns.chunks(DESCRIBE_BATCH) {
        let known = api.describe(cluster, chunk).await?;
        for arn in chunk {
            match known.iter().find(|t| &t.arn == arn) {
                Some(t) if t.last_status != "STOPPED" => {}
                Some(t) => stopped.push((arn.clone(), classify_stop(t))),
                None => stopped.push((arn.clone(), StopKind::Exited(None))),
            }
        }
    }
    Ok(stopped)
}

// ---------------------------------------------------------------- real API

/// The real thing, from a connection's `SdkConfig`.
pub struct SdkTaskApi {
    ecs: aws_sdk_ecs::Client,
    logs: aws_sdk_cloudwatchlogs::Client,
}

impl SdkTaskApi {
    pub fn new(cfg: &SdkConfig) -> Self {
        SdkTaskApi {
            ecs: aws_sdk_ecs::Client::new(cfg),
            logs: aws_sdk_cloudwatchlogs::Client::new(cfg),
        }
    }
}

impl TaskApi for SdkTaskApi {
    async fn run_task(&self, req: RunTaskInput) -> Result<TaskInfo, AwsError> {
        const ACTION: &str = "ecs:RunTask";
        // The fluent builder takes no whole input; copy every field.
        let out = self
            .ecs
            .run_task()
            .set_capacity_provider_strategy(req.capacity_provider_strategy)
            .set_cluster(req.cluster)
            .set_count(req.count)
            .set_enable_ecs_managed_tags(req.enable_ecs_managed_tags)
            .set_enable_execute_command(req.enable_execute_command)
            .set_group(req.group)
            .set_launch_type(req.launch_type)
            .set_network_configuration(req.network_configuration)
            .set_overrides(req.overrides)
            .set_placement_constraints(req.placement_constraints)
            .set_placement_strategy(req.placement_strategy)
            .set_platform_version(req.platform_version)
            .set_propagate_tags(req.propagate_tags)
            .set_reference_id(req.reference_id)
            .set_started_by(req.started_by)
            .set_tags(req.tags)
            .set_task_definition(req.task_definition)
            .set_client_token(req.client_token)
            .set_volume_configurations(req.volume_configurations)
            .send()
            .await
            .map_err(|e| quota_or(classify_sdk_error(ACTION, &e)))?;
        run_task_outcome(
            out.tasks().iter().map(task_info).collect(),
            out.failures()
                .iter()
                .map(|f| (f.reason().map(String::from), f.detail().map(String::from)))
                .collect(),
        )
    }

    async fn describe(&self, cluster: &str, arns: &[String]) -> Result<Vec<TaskInfo>, AwsError> {
        const ACTION: &str = "ecs:DescribeTasks";
        let mut out = Vec::new();
        for chunk in arns.chunks(DESCRIBE_BATCH) {
            let page = self
                .ecs
                .describe_tasks()
                .cluster(cluster)
                .set_tasks(Some(chunk.to_vec()))
                .include(TaskField::Tags)
                .send()
                .await
                .map_err(|e| classify_sdk_error(ACTION, &e))?;
            out.extend(page.tasks().iter().map(task_info));
        }
        Ok(out)
    }

    async fn list_project_tasks(
        &self,
        cluster: &str,
        family: &str,
    ) -> Result<Vec<String>, AwsError> {
        const ACTION: &str = "ecs:ListTasks";
        let mut found: Vec<String> = Vec::new();
        for status in [DesiredStatus::Running, DesiredStatus::Pending] {
            let mut token: Option<String> = None;
            for _ in 0..MAX_LIST_PAGES {
                let page = self
                    .ecs
                    .list_tasks()
                    .cluster(cluster)
                    .family(family)
                    .desired_status(status.clone())
                    .set_next_token(token.take())
                    .send()
                    .await
                    .map_err(|e| classify_sdk_error(ACTION, &e))?;
                for a in page.task_arns() {
                    if !found.contains(a) {
                        found.push(a.clone());
                    }
                }
                match page.next_token() {
                    Some(t) => token = Some(t.to_string()),
                    None => break,
                }
            }
        }
        Ok(found)
    }

    async fn stop_task(&self, cluster: &str, arn: &str, reason: &str) -> Result<(), AwsError> {
        const ACTION: &str = "ecs:StopTask";
        self.ecs
            .stop_task()
            .cluster(cluster)
            .task(arn)
            .reason(reason)
            .send()
            .await
            .map_err(|e| classify_sdk_error(ACTION, &e))?;
        Ok(())
    }

    async fn log_tail(
        &self,
        group: &str,
        stream: &str,
        limit: i32,
    ) -> Result<Vec<String>, AwsError> {
        const ACTION: &str = "logs:GetLogEvents";
        let out = self
            .logs
            .get_log_events()
            .log_group_name(group)
            .log_stream_name(stream)
            .start_from_head(false)
            .limit(limit)
            .send()
            .await;
        match out {
            Ok(page) => Ok(page
                .events()
                .iter()
                .filter_map(|e| e.message())
                .map(str::to_string)
                .collect()),
            Err(e)
                if e.as_service_error()
                    .is_some_and(|s| s.is_resource_not_found_exception()) =>
            {
                Ok(Vec::new())
            }
            Err(e) => Err(classify_sdk_error(ACTION, &e)),
        }
    }
}

#[cfg(test)]
mod tests;
