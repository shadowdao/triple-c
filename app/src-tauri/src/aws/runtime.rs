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

use super::environments::{
    get_parameter, knowable, parse_environment_record, user_record_from, EnvironmentInfo, Knowable,
    Param, UserRecord,
};
use super::error::{classify_sdk_error, AwsError};
use super::tasks::{find_project_task, TaskApi, STOP_REASON};
use super::tofu_modules::{
    access_point_root, workspace_access_point_root, environment_cluster_name, environment_parameter_name, project_secret_name, task_family,
    is_user_ec2_cluster, user_file_system_token, user_record_parameter_name, user_role_names,
};
use super::workspace::WorkspaceApi;

/// The secret key holding the agent token (Task 4's payload).
pub const AGENT_TOKEN_KEY: &str = "TRIPLEC_AGENT_TOKEN";
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
    /// `ssm:GetParameter` of the caller's EC2 record in `env`
    /// (`user_record_parameter_name`).
    fn user_record(
        &self,
        env: &str,
        owner_key: &str,
    ) -> impl Future<Output = Result<Param, AwsError>> + Send;
    /// `iam:GetRole`.
    fn role_exists(&self, name: &str) -> impl Future<Output = Result<Knowable, AwsError>> + Send;
    /// `secretsmanager:GetSecretValue`'s `SecretString`.
    fn secret_string(&self, name: &str) -> impl Future<Output = Result<String, AwsError>> + Send;
    /// The access point's `LifeCycleState`; `None` when it is gone.
    fn access_point_state(
        &self,
        id: &str,
    ) -> impl Future<Output = Result<Option<String>, AwsError>> + Send;
    /// Delete at once (`ForceDeleteWithoutRecovery`, ruling R10): a secret
    /// left pending deletion would make the project's next start fail, and
    /// the app rebuilds it from local data. A secret that does not exist is
    /// success.
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

/// The caller's EC2 record in `env`, validated (untrusted: `parse_user_record`).
/// Absent = no EC2 capacity for NEW runs; a task already running on the
/// user's cluster may outlive the record (teardown removes it first).
pub async fn read_user_record<R: RuntimeApi>(
    rt: &R,
    env: &str,
    owner_key: &str,
    account: &str,
    region: &str,
) -> Result<UserRecord, AwsError> {
    Ok(user_record_from(rt.user_record(env, owner_key).await?, env, account, region))
}

/// The per-user EC2 cluster (ARN) a stored task ARN names, when it is one of
/// `env`'s: where a project that ran on EC2 is still looked for after its
/// user record is gone.
pub fn ec2_cluster_of_task(env: &str, task_arn: &str) -> Option<String> {
    cluster_of_task(task_arn)
        .filter(|c| is_user_ec2_cluster(env, c))
        .and_then(|_| cluster_arn_of_task(task_arn))
}

/// Where a project's task may be: `env_cluster` (the environment's), then
/// the user's EC2 cluster from their record, then the per-user cluster
/// `stored_task_arn` names (the record may be gone while that task runs),
/// without repeats.
pub fn project_task_clusters(
    env: &str,
    env_cluster: String,
    ec2_cluster: Option<String>,
    stored_task_arn: Option<&str>,
) -> Vec<String> {
    let mut out = vec![env_cluster];
    for c in [ec2_cluster, stored_task_arn.and_then(|a| ec2_cluster_of_task(env, a))]
        .into_iter()
        .flatten()
    {
        if !out.contains(&c) {
            out.push(c);
        }
    }
    out
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

/// After a start this call ran is up and its agent answered with `ours`:
/// make sure the secret still holds `ours`. A start that lost the RunTask
/// race (another machine, the same moment) rotated the token in the secret
/// before it found this task, and every later attach would read a token the
/// task never saw. Returns whether the secret had to be put back.
pub async fn ensure_agent_token<W: WorkspaceApi, R: RuntimeApi>(
    work: &W,
    rt: &R,
    env: &EnvironmentInfo,
    owner_user_id: &str,
    owner_key: &str,
    project_id: &str,
    ours: &str,
) -> Result<bool, AwsError> {
    let name = project_secret_name(&env.name, owner_key, project_id);
    let json = rt.secret_string(&name).await?;
    if agent_token_from_secret(&json).ok().as_deref() == Some(ours) {
        return Ok(false);
    }
    let mut v: serde_json::Map<String, serde_json::Value> = serde_json::from_str(&json)
        .map_err(|_| other("The project secret is not a JSON object"))?;
    v.insert(AGENT_TOKEN_KEY.to_string(), serde_json::Value::String(ours.to_string()));
    let repaired = serde_json::to_string(&v).map_err(|e| other(e.to_string()))?;
    let tags = super::workspace::workspace_tags(owner_user_id, project_id, &env.name);
    work.put_project_secret(&name, &env.kms_key_arn, &repaired, &tags).await?;
    Ok(true)
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
    /// The EFS-mode `/workspace` access point the store has (kept after a
    /// switch to Hybrid or Ephemeral, Q1).
    pub stored_workspace_access_point_id: Option<&'a str>,
    /// The user's EC2 cluster (their record's), looked in too: a project
    /// may have run there.
    pub ec2_cluster: Option<&'a str>,
    /// Delete the EFS access points, home and `/workspace` (ruling R9). The
    /// files stay on the user's own file system (only their task role can
    /// read it) until offboarding destroys it; a later start of the same
    /// project gets new access points onto the same directories.
    pub discard_data: bool,
}

/// Stop the project's task, delete its secret (no recovery window, R10) and,
/// with `discard_data`, delete its access points (not the files, R9). Task definition revisions
/// are left registered (R8). A task that can't be stopped stops the removal (nothing
/// billed may be left running); the rest is attempted in full and every
/// failure reported.
pub async fn remove_workspace_resources<T: TaskApi, W: WorkspaceApi, R: RuntimeApi>(
    tasks: &T,
    work: &W,
    rt: &R,
    req: &RemoveRequest<'_>,
) -> Result<(), AwsError> {
    let cluster = environment_cluster_name(req.env);
    let family = task_family(req.env, req.owner_key, req.project_id);

    // 1. The task: in the environment's cluster, the user's EC2 cluster (the
    // record's, and the stored task's when that is a per-user cluster of
    // this environment — the record may be gone).
    let clusters = project_task_clusters(
        req.env,
        cluster.clone(),
        req.ec2_cluster.map(str::to_string),
        req.stored_task_arn,
    );
    let mut to_stop: Vec<(String, String)> = Vec::new();
    for c in &clusters {
        if let Some(t) =
            find_project_task(tasks, c, &family, req.owner_user_id, req.project_id).await?
        {
            to_stop.push((c.clone(), t.arn));
        }
    }
    if let Some(arn) = req.stored_task_arn {
        let ours = cluster_of_task(arn) == Some(cluster.as_str())
            || ec2_cluster_of_task(req.env, arn).is_some();
        if ours && !to_stop.iter().any(|(_, a)| a == arn) {
            let c = cluster_of_task(arn).map_or_else(|| cluster.clone(), str::to_string);
            to_stop.push((c, arn.to_string()));
        }
    }
    for (c, arn) in &to_stop {
        tasks.stop_task(c, arn, STOP_REASON).await?;
    }

    let mut failures: Vec<String> = Vec::new();

    // 2. Task definition revisions are left ACTIVE (ruling R8): ECS
    // authorizes `DeregisterTaskDefinition` against `*` only, with no tag
    // condition, so the workspace tier can't retire just its own. They are
    // inert (no secret values, only the secret's ARN) and free, and
    // `RunOwnTasks` still requires the owner tag, so nobody else can run
    // them. An admin can deregister them (`aws/README.md`).
    log::info!(
        "Project {}: its task definition revisions ({family}:*) are left registered; an \
         administrator can deregister them",
        req.project_id
    );

    // 3. The secret.
    let secret = project_secret_name(req.env, req.owner_key, req.project_id);
    if let Err(e) = rt.delete_secret(&secret).await {
        failures.push(format!("secret {secret}: {e}"));
    }

    // 4. The access points (home, and EFS mode's `/workspace`), only when asked.
    if req.discard_data {
        let mut ids: Vec<String> = [req.stored_access_point_id, req.stored_workspace_access_point_id]
            .into_iter()
            .flatten()
            .map(String::from)
            .collect();
        let token = user_file_system_token(req.env, req.owner_key);
        match work.find_file_system(&token).await {
            Ok(Some(fs)) if fs.owner.as_deref() == Some(req.owner_user_id) => {
                let roots = [
                    access_point_root(req.owner_key, req.project_id),
                    workspace_access_point_root(req.owner_key, req.project_id),
                ];
                match work.access_points(&fs.id).await {
                    Ok(points) => ids.extend(
                        points
                            .into_iter()
                            .filter(|a| {
                                a.file_system_id == fs.id
                                    && a.root_path.as_ref().is_some_and(|r| roots.contains(r))
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
}

impl SdkRuntimeApi {
    pub fn new(cfg: &SdkConfig) -> Self {
        SdkRuntimeApi {
            ssm: aws_sdk_ssm::Client::new(cfg),
            iam: aws_sdk_iam::Client::new(cfg),
            secrets: aws_sdk_secretsmanager::Client::new(cfg),
            efs: aws_sdk_efs::Client::new(cfg),
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

    async fn user_record(&self, env: &str, owner_key: &str) -> Result<Param, AwsError> {
        get_parameter(&self.ssm, &user_record_parameter_name(env, owner_key)).await
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

    async fn delete_secret(&self, name: &str) -> Result<(), AwsError> {
        const ACTION: &str = "secretsmanager:DeleteSecret";
        match self
            .secrets
            .delete_secret()
            .secret_id(name)
            .force_delete_without_recovery(true)
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
            // Scheduled by an older version of the app and AWS refused the
            // forced delete: nothing more this tier may do (no RestoreSecret).
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
