//! What the AWS host asks AWS around a task, besides running it
//! (`tasks.rs`) and preparing its resources (`workspace.rs`): the
//! environment's record, whether the caller is onboarded, the agent token of
//! a running task (P2), waiting for a new access point, and removing a
//! project's resources.
//!
//! Like the other `aws/` modules, the decisions are made by the functions
//! here over a small trait ([`RuntimeApi`]) so a fake exercises them; the SDK
//! calls are in [`SdkRuntimeApi`].

use std::future::Future;
use std::time::Duration;

use aws_config::SdkConfig;
use aws_sdk_secretsmanager::error::ProvideErrorMetadata;

use super::environments::{knowable, parse_environment_record, EnvironmentInfo, Knowable};
use super::error::{classify_sdk_error, AwsError};
use super::tasks::{find_project_task, TaskApi, STOP_REASON};
use super::tofu_modules::{
    access_point_root, environment_parameter_name, project_secret_name, task_family,
    user_file_system_token, user_role_names,
};
use super::workspace::WorkspaceApi;

/// The secret key holding the agent token (Task 4's payload).
pub const AGENT_TOKEN_KEY: &str = "TRIPLEC_AGENT_TOKEN";
/// Secrets Manager's recovery window when a project is removed.
pub const SECRET_RECOVERY_DAYS: i64 = 7;
/// A project secret is at most 64 KiB; anything bigger is not ours.
const MAX_SECRET_READ: usize = 70_000;

fn other(message: impl Into<String>) -> AwsError {
    AwsError::Other {
        message: message.into(),
    }
}

pub trait RuntimeApi: Send + Sync {
    /// `ssm:GetParameter` of an environment's record; `None` when absent.
    fn environment_record(
        &self,
        env: &str,
    ) -> impl Future<Output = Result<Option<String>, AwsError>> + Send;
    /// `iam:GetRole`.
    fn role_exists(&self, name: &str) -> impl Future<Output = Result<Knowable, AwsError>> + Send;
    /// `secretsmanager:GetSecretValue`'s `SecretString`.
    fn secret_string(&self, name: &str) -> impl Future<Output = Result<String, AwsError>> + Send;
    /// The access point's `LifeCycleState`; `None` when it is gone.
    fn access_point_state(
        &self,
        id: &str,
    ) -> impl Future<Output = Result<Option<String>, AwsError>> + Send;
    fn deregister_task_definition(
        &self,
        arn: &str,
    ) -> impl Future<Output = Result<(), AwsError>> + Send;
    fn delete_task_definitions(
        &self,
        arns: &[String],
    ) -> impl Future<Output = Result<(), AwsError>> + Send;
    /// Schedule deletion with a [`SECRET_RECOVERY_DAYS`] window. A secret
    /// that does not exist is success.
    fn delete_secret(&self, name: &str) -> impl Future<Output = Result<(), AwsError>> + Send;
    /// A missing access point is success.
    fn delete_access_point(&self, id: &str) -> impl Future<Output = Result<(), AwsError>> + Send;
}

// ------------------------------------------------------------ environment

/// The environment's record, read with one `GetParameter` and validated.
pub async fn read_environment<R: RuntimeApi>(
    rt: &R,
    env: &str,
    account: &str,
    region: &str,
) -> Result<EnvironmentInfo, AwsError> {
    let value = rt.environment_record(env).await?.ok_or_else(|| {
        other(format!(
            "The environment \"{env}\" was not found in {region}. It may have been removed; \
             check Settings → Hosts."
        ))
    })?;
    parse_environment_record(env, &value, account, region).map_err(|e| {
        other(format!(
            "The record of environment \"{env}\" in {region} is not usable: {e}"
        ))
    })
}

/// Whether the caller's task role exists in the environment: the onboarding
/// signal a workspace user can read. A denied lookup is not an answer, so
/// it is let through (RunTask will say what is wrong).
pub async fn check_onboarded<R: RuntimeApi>(
    rt: &R,
    env: &str,
    owner_key: &str,
) -> Result<(), AwsError> {
    let (task_role, _) = user_role_names(env, owner_key);
    match rt.role_exists(&task_role).await? {
        Knowable::No => Err(other(format!(
            "You have not joined the environment \"{env}\" yet. Join it in Settings → Hosts \
             (or ask an administrator to onboard you), then start the project again."
        ))),
        Knowable::Yes | Knowable::Unknown => Ok(()),
    }
}

/// The agent token in a project secret's JSON. The error never echoes the
/// secret.
pub fn agent_token_from_secret(json: &str) -> Result<String, String> {
    if json.len() > MAX_SECRET_READ {
        return Err("The project secret is larger than a project secret can be".into());
    }
    let v: serde_json::Value = serde_json::from_str(json)
        .map_err(|_| "The project secret is not a JSON object".to_string())?;
    v.get(AGENT_TOKEN_KEY)
        .and_then(|t| t.as_str())
        .filter(|t| !t.is_empty() && t.bytes().all(|b| b.is_ascii_graphic()))
        .map(str::to_string)
        .ok_or_else(|| "The project secret holds no agent token".to_string())
}

/// The token a running task of this project started with (P2: the caller
/// may read their own secret; `prepare_workspace` wrote it before RunTask).
pub async fn read_agent_token<R: RuntimeApi>(
    rt: &R,
    env: &str,
    owner_key: &str,
    project_id: &str,
) -> Result<String, AwsError> {
    let name = project_secret_name(env, owner_key, project_id);
    let json = rt.secret_string(&name).await?;
    agent_token_from_secret(&json).map_err(other)
}

/// How long a fresh access point may take to become `available`.
#[derive(Debug, Clone, Copy)]
pub struct PollTiming {
    pub interval: Duration,
    pub timeout: Duration,
}

impl Default for PollTiming {
    fn default() -> Self {
        PollTiming {
            interval: Duration::from_secs(2),
            timeout: Duration::from_secs(90),
        }
    }
}

/// Wait until the access point is `available` (a task mounting one that is
/// still `creating` fails to start).
pub async fn wait_access_point<R: RuntimeApi>(
    rt: &R,
    id: &str,
    timing: PollTiming,
) -> Result<(), AwsError> {
    let deadline = tokio::time::Instant::now() + timing.timeout;
    loop {
        match rt.access_point_state(id).await?.as_deref() {
            Some("available") => return Ok(()),
            Some("creating") | Some("updating") => {}
            Some(s) => {
                return Err(other(format!(
                    "The project's EFS access point {id} is {s}; start the project again"
                )))
            }
            None => {
                return Err(other(format!(
                    "The project's EFS access point {id} disappeared; start the project again"
                )))
            }
        }
        if tokio::time::Instant::now() + timing.interval > deadline {
            return Err(other(format!(
                "The project's EFS access point {id} did not become available in time; \
                 start the project again in a minute"
            )));
        }
        tokio::time::sleep(timing.interval).await;
    }
}

// ------------------------------------------------------------------ ARNs

/// The cluster *name* of `arn:...:task/<cluster>/<id>` (ECS accepts a name
/// wherever it takes a cluster).
pub fn cluster_of_task(task_arn: &str) -> Option<&str> {
    let (_, rest) = task_arn.split_once(":task/")?;
    let (cluster, id) = rest.split_once('/')?;
    (task_arn.starts_with("arn:") && !cluster.is_empty() && !id.is_empty() && !id.contains('/'))
        .then_some(cluster)
}

/// The cluster ARN of a task ARN (same partition, region and account).
pub fn cluster_arn_of_task(task_arn: &str) -> Option<String> {
    let cluster = cluster_of_task(task_arn)?;
    let (prefix, _) = task_arn.split_once(":task/")?;
    Some(format!("{prefix}:cluster/{cluster}"))
}

// ---------------------------------------------------------------- removal

/// What removing a project's AWS resources needs to know.
pub struct RemoveRequest<'a> {
    pub env: &'a str,
    pub region: &'a str,
    pub owner_user_id: &'a str,
    pub owner_key: &'a str,
    pub project_id: &'a str,
    /// The task ARN the store has, if any (stopped too, in case the listing
    /// missed it).
    pub stored_task_arn: Option<&'a str>,
    pub stored_access_point_id: Option<&'a str>,
    /// Delete the EFS access point. Its data stays on the file system until
    /// an administrator removes it; the path is not reused.
    pub discard_data: bool,
}

/// Stop the project's task, retire its task definition revisions, schedule
/// its secret for deletion (7 days) and, with `discard_data`, delete its
/// access point. A task that can't be stopped stops the removal (nothing
/// billed may be left running); the rest is attempted in full and every
/// failure reported.
pub async fn remove_workspace_resources<T: TaskApi, W: WorkspaceApi, R: RuntimeApi>(
    tasks: &T,
    work: &W,
    rt: &R,
    req: &RemoveRequest<'_>,
) -> Result<(), AwsError> {
    let cluster = format!("triple-c-{}", req.env);
    let family = task_family(req.env, req.owner_key, req.project_id);

    // 1. The task.
    let live =
        find_project_task(tasks, &cluster, &family, req.owner_user_id, req.project_id).await?;
    let mut to_stop: Vec<String> = live.into_iter().map(|t| t.arn).collect();
    if let Some(arn) = req.stored_task_arn {
        if cluster_of_task(arn) == Some(cluster.as_str()) && !to_stop.iter().any(|a| a == arn) {
            to_stop.push(arn.to_string());
        }
    }
    for arn in &to_stop {
        tasks.stop_task(&cluster, arn, STOP_REASON).await?;
    }

    let mut failures: Vec<String> = Vec::new();

    // 2. Task definition revisions the caller registered for this project.
    let mut retired = Vec::new();
    match work.task_definition_revisions(&family).await {
        Ok(revisions) => {
            for arn in revisions {
                match work.describe_task_definition(&arn).await {
                    Ok(Some(d))
                        if d.owner.as_deref() == Some(req.owner_user_id)
                            && d.project.as_deref() == Some(req.project_id) =>
                    {
                        match rt.deregister_task_definition(&d.arn).await {
                            Ok(()) => retired.push(d.arn),
                            Err(e) => failures.push(format!("task definition {}: {e}", d.arn)),
                        }
                    }
                    Ok(_) => {}
                    Err(e) => failures.push(format!("task definition {arn}: {e}")),
                }
            }
        }
        Err(e) => failures.push(format!("task definitions: {e}")),
    }
    // DeleteTaskDefinitions takes at most 10 at a time.
    for chunk in retired.chunks(10) {
        if let Err(e) = rt.delete_task_definitions(chunk).await {
            failures.push(format!("deleting retired task definitions: {e}"));
        }
    }

    // 3. The secret.
    let secret = project_secret_name(req.env, req.owner_key, req.project_id);
    if let Err(e) = rt.delete_secret(&secret).await {
        failures.push(format!("secret {secret}: {e}"));
    }

    // 4. The access point, only when asked.
    if req.discard_data {
        let mut ids: Vec<String> = req.stored_access_point_id.map(String::from).into_iter().collect();
        let token = user_file_system_token(req.env, req.owner_key);
        match work.find_file_system(&token).await {
            Ok(Some(fs)) if fs.owner.as_deref() == Some(req.owner_user_id) => {
                let root = access_point_root(req.owner_key, req.project_id);
                match work.access_points(&fs.id).await {
                    Ok(points) => ids.extend(
                        points
                            .into_iter()
                            .filter(|a| {
                                a.file_system_id == fs.id
                                    && a.root_path.as_deref() == Some(root.as_str())
                                    && a.owner.as_deref() == Some(req.owner_user_id)
                                    && a.project.as_deref() == Some(req.project_id)
                            })
                            .map(|a| a.id),
                    ),
                    Err(e) => failures.push(format!("access points: {e}")),
                }
            }
            Ok(_) => {}
            Err(e) => failures.push(format!("file system: {e}")),
        }
        ids.sort();
        ids.dedup();
        for id in ids {
            if let Err(e) = rt.delete_access_point(&id).await {
                failures.push(format!("access point {id}: {e}"));
            }
        }
    }

    if failures.is_empty() {
        Ok(())
    } else {
        Err(other(format!(
            "Some of the project's AWS resources in {} could not be removed: {}",
            req.region,
            failures.join("; ")
        )))
    }
}

// ---------------------------------------------------------------- real API

pub struct SdkRuntimeApi {
    ssm: aws_sdk_ssm::Client,
    iam: aws_sdk_iam::Client,
    secrets: aws_sdk_secretsmanager::Client,
    efs: aws_sdk_efs::Client,
    ecs: aws_sdk_ecs::Client,
}

impl SdkRuntimeApi {
    pub fn new(cfg: &SdkConfig) -> Self {
        SdkRuntimeApi {
            ssm: aws_sdk_ssm::Client::new(cfg),
            iam: aws_sdk_iam::Client::new(cfg),
            secrets: aws_sdk_secretsmanager::Client::new(cfg),
            efs: aws_sdk_efs::Client::new(cfg),
            ecs: aws_sdk_ecs::Client::new(cfg),
        }
    }
}

impl RuntimeApi for SdkRuntimeApi {
    async fn environment_record(&self, env: &str) -> Result<Option<String>, AwsError> {
        const ACTION: &str = "ssm:GetParameter";
        match self
            .ssm
            .get_parameter()
            .name(environment_parameter_name(env))
            .with_decryption(false)
            .send()
            .await
        {
            Ok(out) => Ok(out.parameter().and_then(|p| p.value()).map(str::to_string)),
            Err(e)
                if e.as_service_error()
                    .is_some_and(|s| s.is_parameter_not_found()) =>
            {
                Ok(None)
            }
            Err(e) => Err(classify_sdk_error(ACTION, &e)),
        }
    }

    async fn role_exists(&self, name: &str) -> Result<Knowable, AwsError> {
        let res = self.iam.get_role().role_name(name).send().await;
        knowable("iam:GetRole", res)
    }

    async fn secret_string(&self, name: &str) -> Result<String, AwsError> {
        const ACTION: &str = "secretsmanager:GetSecretValue";
        let out = self
            .secrets
            .get_secret_value()
            .secret_id(name)
            .send()
            .await
            .map_err(|e| classify_sdk_error(ACTION, &e))?;
        out.secret_string()
            .map(str::to_string)
            .ok_or_else(|| other(format!("{ACTION} returned no SecretString")))
    }

    async fn access_point_state(&self, id: &str) -> Result<Option<String>, AwsError> {
        const ACTION: &str = "elasticfilesystem:DescribeAccessPoints";
        match self.efs.describe_access_points().access_point_id(id).send().await {
            Ok(out) => Ok(out
                .access_points()
                .iter()
                .find(|a| a.access_point_id() == Some(id))
                .and_then(|a| a.life_cycle_state())
                .map(|s| s.as_str().to_string())),
            Err(e)
                if e.as_service_error()
                    .is_some_and(|s| s.is_access_point_not_found()) =>
            {
                Ok(None)
            }
            Err(e) => Err(classify_sdk_error(ACTION, &e)),
        }
    }

    async fn deregister_task_definition(&self, arn: &str) -> Result<(), AwsError> {
        const ACTION: &str = "ecs:DeregisterTaskDefinition";
        self.ecs
            .deregister_task_definition()
            .task_definition(arn)
            .send()
            .await
            .map_err(|e| classify_sdk_error(ACTION, &e))?;
        Ok(())
    }

    async fn delete_task_definitions(&self, arns: &[String]) -> Result<(), AwsError> {
        const ACTION: &str = "ecs:DeleteTaskDefinitions";
        let out = self
            .ecs
            .delete_task_definitions()
            .set_task_definitions(Some(arns.to_vec()))
            .send()
            .await
            .map_err(|e| classify_sdk_error(ACTION, &e))?;
        let failed: Vec<String> = out
            .failures()
            .iter()
            .map(|f| {
                format!(
                    "{} ({})",
                    f.arn().unwrap_or("?"),
                    f.reason().unwrap_or("no reason given")
                )
            })
            .collect();
        if failed.is_empty() {
            Ok(())
        } else {
            Err(other(format!("{ACTION} failed for {}", failed.join(", "))))
        }
    }

    async fn delete_secret(&self, name: &str) -> Result<(), AwsError> {
        const ACTION: &str = "secretsmanager:DeleteSecret";
        match self
            .secrets
            .delete_secret()
            .secret_id(name)
            .force_delete_without_recovery(false)
            .recovery_window_in_days(SECRET_RECOVERY_DAYS)
            .send()
            .await
        {
            Ok(_) => Ok(()),
            Err(e)
                if e.as_service_error()
                    .is_some_and(|s| s.is_resource_not_found_exception()) =>
            {
                Ok(())
            }
            // Already scheduled: the goal.
            Err(e)
                if e.as_service_error().is_some_and(|s| {
                    s.is_invalid_request_exception()
                        && s.message().is_some_and(|m| m.contains("scheduled for deletion"))
                }) =>
            {
                Ok(())
            }
            Err(e) => Err(classify_sdk_error(ACTION, &e)),
        }
    }

    async fn delete_access_point(&self, id: &str) -> Result<(), AwsError> {
        const ACTION: &str = "elasticfilesystem:DeleteAccessPoint";
        match self.efs.delete_access_point().access_point_id(id).send().await {
            Ok(_) => Ok(()),
            Err(e)
                if e.as_service_error()
                    .is_some_and(|s| s.is_access_point_not_found()) =>
            {
                Ok(())
            }
            Err(e) => Err(classify_sdk_error(ACTION, &e)),
        }
    }
}

#[cfg(test)]
mod tests;
