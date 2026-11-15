//! A project's AWS resources, made ready before its task runs: the project
//! secret, the EFS access point (Hybrid storage) and the task definition.
//!
//! **Everything project-shaped goes in the secret (P1).** Every workspace user
//! has `ecs:Describe*` on `*`, so a task definition's `environment`, tags and
//! labels are readable by everyone in the account. The task definition holds
//! only two constants in `environment`, the three ownership tags, a
//! fingerprint label (a hash), and one `secrets` entry per payload key.
//!
//! **A task definition family is not trusted by name.** Any workspace user
//! may register a revision into any `triple-c-*` family (the policy can't
//! scope RegisterTaskDefinition to an owner's family). So a revision is reused
//! only when it carries the caller's own owner tag (which only the caller can
//! set at register) and this project's tag and the expected fingerprint, and
//! it is always named by its exact `family:revision` ARN.
//!
//! **Determinism.** Custom env vars come out of `workspace_env` in HashMap
//! order, so the payload is a `BTreeMap` and the `secrets` entries follow its
//! key order: unchanged settings give the same fingerprint, and therefore no
//! new revision, on every start. The agent token rotates every start (P2) but
//! the key set does not.

use std::collections::BTreeMap;
use std::future::Future;

use aws_config::SdkConfig;
use aws_sdk_ecs::operation::register_task_definition::RegisterTaskDefinitionInput;
use aws_sdk_ecs::types::{
    Compatibility, ContainerDefinition, CpuArchitecture, EfsAuthorizationConfig,
    EfsAuthorizationConfigIam, EfsTransitEncryption, EfsVolumeConfiguration, EphemeralStorage,
    HealthCheck, KernelCapabilities, KeyValuePair, LinuxParameters, LogConfiguration, LogDriver,
    MountPoint, NetworkMode, OsFamily, RuntimePlatform, Secret, SortOrder, Tag,
    TaskDefinitionField, TaskDefinitionStatus, Volume,
};
use aws_sdk_secretsmanager::error::ProvideErrorMetadata;
use base64::Engine;
use rand::RngCore;
use serde::Serialize;
use sha2::{Digest, Sha256};

use super::environments::EnvironmentInfo;
use super::error::{classify_sdk_error, AwsError};
use super::identity::CallerIdentity;
use super::tofu_modules::{
    access_point_root, log_stream_prefix, owner_key, project_secret_name, task_family,
    user_file_system_token, user_role_names,
};
use crate::host::ssm::AGENT_PORT;
use crate::models::project::{validate_aws_workspace, Arch, RepoSource, StorageMode};
use crate::models::Project;

pub const TAG_OWNER: &str = "triple-c:owner";
pub const TAG_PROJECT: &str = "triple-c:project";
pub const TAG_ENVIRONMENT: &str = "triple-c:environment";
/// The container's docker label carrying [`workspace_fingerprint`]. Tags
/// can't: the policy admits only the three tag keys above.
pub const FINGERPRINT_LABEL: &str = "triple-c.fingerprint";
pub const CONTAINER_NAME: &str = "workspace";
/// Where a Hybrid project's access point is mounted (P4).
pub const HOME_MOUNT_PATH: &str = "/mnt/triple-c-home";
const HOME_VOLUME: &str = "home";
/// Secrets Manager's `SecretString` limit.
pub const MAX_SECRET_BYTES: usize = 65_536;
/// At most this many of a family's newest revisions are described when
/// looking for a reusable one; past that a new one is registered.
pub const MAX_REVISIONS_CHECKED: usize = 10;
/// The container user (`claude`), owner of the access point's root.
const WORKSPACE_UID: i64 = 1000;

/// The payload's own keys. Custom env pairs of these names are dropped.
const KEY_AGENT_TOKEN: &str = "TRIPLEC_AGENT_TOKEN";
const KEY_FILES: &str = "TRIPLEC_FILES";
const KEY_REPOS: &str = "TRIPLEC_REPOS";
const KEY_WIP_PUSH: &str = "TRIPLEC_WIP_PUSH";
const KEY_IDLE: &str = "TRIPLEC_IDLE_STOP_MINUTES";
const KEY_STORAGE: &str = "TRIPLEC_STORAGE";
/// The task definition's whole `environment` (R2): constants, no project data.
const FIXED_ENVIRONMENT: [(&str, &str); 2] = [("TRIPLEC_HOST", "aws"), ("COLORTERM", "truecolor")];

fn other(message: impl Into<String>) -> AwsError {
    AwsError::Other {
        message: message.into(),
    }
}

/// The three ownership tags, in key order, and nothing else.
pub fn workspace_tags(user_id: &str, project_id: &str, env: &str) -> Vec<(String, String)> {
    vec![
        (TAG_ENVIRONMENT.to_string(), env.to_string()),
        (TAG_OWNER.to_string(), user_id.to_string()),
        (TAG_PROJECT.to_string(), project_id.to_string()),
    ]
}

/// A fresh agent token: 48 random bytes, URL-safe base64 without padding.
pub fn new_agent_token() -> String {
    let mut bytes = [0u8; 48];
    rand::rng().fill_bytes(&mut bytes);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

// ------------------------------------------------------------------ payload

/// A name `valueFrom: <arn>:<KEY>::` can carry and a shell can export.
fn is_env_name(key: &str) -> bool {
    let b = key.as_bytes();
    !b.is_empty()
        && (b[0].is_ascii_alphabetic() || b[0] == b'_')
        && b.iter().all(|c| c.is_ascii_alphanumeric() || *c == b'_')
}

/// `ssh/<file>` or `ca/<file>`, the file name non-empty, without `/`, and
/// not `.` or `..` (R1: the entrypoint writes it under `/tmp/.host-*/`).
fn validate_file_key(key: &str) -> Result<(), String> {
    let file = key
        .strip_prefix("ssh/")
        .or_else(|| key.strip_prefix("ca/"))
        .ok_or_else(|| format!("\"{}\" is not an ssh/ or ca/ file", key.escape_debug()))?;
    if file.is_empty() || file.contains('/') || file.contains('\0') || file == "." || file == ".." {
        return Err(format!(
            "\"{}\" is not a valid file name",
            key.escape_debug()
        ));
    }
    Ok(())
}

fn payload_map(
    env_pairs: &[(String, String)],
    agent_token: &str,
    files: &[(String, Vec<u8>)],
    repos: &[RepoSource],
    wip: bool,
    idle: Option<u32>,
    storage: StorageMode,
) -> Result<BTreeMap<String, String>, String> {
    let own = [
        KEY_AGENT_TOKEN,
        KEY_FILES,
        KEY_REPOS,
        KEY_WIP_PUSH,
        KEY_IDLE,
        KEY_STORAGE,
    ];
    let mut m = BTreeMap::new();
    for (k, v) in env_pairs {
        if !is_env_name(k) {
            return Err(format!(
                "The environment variable name \"{}\" can't be passed to an AWS task: use \
                 letters, digits and _, not starting with a digit",
                k.escape_debug()
            ));
        }
        if own.contains(&k.as_str()) || FIXED_ENVIRONMENT.iter().any(|(f, _)| f == k) {
            log::warn!("Ignoring the environment variable {k}: Triple-C sets it on AWS");
            continue;
        }
        m.insert(k.clone(), v.clone());
    }
    let b64 = base64::engine::general_purpose::STANDARD;
    let mut file_map = BTreeMap::new();
    for (k, bytes) in files {
        validate_file_key(k)?;
        file_map.insert(k.clone(), b64.encode(bytes));
    }
    m.insert(KEY_AGENT_TOKEN.into(), agent_token.into());
    m.insert(KEY_FILES.into(), json(&file_map));
    m.insert(KEY_REPOS.into(), json(&repos));
    m.insert(KEY_WIP_PUSH.into(), if wip { "1" } else { "0" }.into());
    if let Some(minutes) = idle {
        m.insert(KEY_IDLE.into(), minutes.to_string());
    }
    m.insert(
        KEY_STORAGE.into(),
        match storage {
            StorageMode::Ephemeral => "ephemeral",
            StorageMode::Hybrid => "hybrid",
        }
        .into(),
    );
    Ok(m)
}

/// A nested JSON value (string keys, plain data: cannot fail).
fn json<T: Serialize + ?Sized>(v: &T) -> String {
    serde_json::to_string(v).expect("plain data serialises")
}

fn serialize_payload(m: &BTreeMap<String, String>) -> Result<String, String> {
    let json = serde_json::to_string(m).expect("a string map serialises");
    if json.len() > MAX_SECRET_BYTES {
        return Err(format!(
            "The project's settings for AWS come to {} bytes, more than the 65,536 a Secrets \
             Manager secret holds. Shorten the instructions, Claude Code settings or custom \
             environment variables, or remove files from the SSH folder.",
            json.len()
        ));
    }
    Ok(json)
}

/// The project secret's JSON (P1, R1): every env pair, then the payload's own
/// keys (`TRIPLEC_AGENT_TOKEN`, `TRIPLEC_FILES` = `{"ssh/<f>"|"ca/<f>": base64}`,
/// `TRIPLEC_REPOS`, `TRIPLEC_WIP_PUSH` = `1`/`0`, `TRIPLEC_IDLE_STOP_MINUTES`
/// when on, `TRIPLEC_STORAGE` = `ephemeral`/`hybrid`), keys sorted. Fails
/// when it would exceed 65,536 bytes.
#[cfg_attr(not(test), allow(dead_code))]
pub fn project_secret_payload(
    env_pairs: &[(String, String)],
    agent_token: &str,
    files: &[(String, Vec<u8>)],
    repos: &[RepoSource],
    wip: bool,
    idle: Option<u32>,
    storage: StorageMode,
) -> Result<String, String> {
    serialize_payload(&payload_map(
        env_pairs,
        agent_token,
        files,
        repos,
        wip,
        idle,
        storage,
    )?)
}

// ---------------------------------------------------------- task definition

/// Everything a project's task definition is built from.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct WorkspacePlan {
    pub family: String,
    pub cpu: u32,
    pub memory: u32,
    pub arch: Arch,
    pub storage: StorageMode,
    pub ephemeral_gib: u32,
    pub image: String,
    pub exec_role_arn: String,
    pub task_role_arn: String,
    pub log_group: String,
    pub log_stream_prefix: String,
    pub region: String,
    pub secret_arn: String,
    /// Sorted (the payload's key order).
    pub secret_keys: Vec<String>,
    /// Both set for Hybrid, both `None` for Ephemeral.
    pub file_system_id: Option<String>,
    pub access_point_id: Option<String>,
    pub tags: Vec<(String, String)>,
}

/// sha256 (hex) over the task definition `plan` produces, minus the label
/// that carries the fingerprint. Not project data: the request holds names,
/// ARNs and constants only.
pub fn workspace_fingerprint(plan: &WorkspacePlan) -> String {
    request_fingerprint(&build_request(plan))
}

/// sha256 (hex) over a canonical rendering of `req`: its `Debug` text, with
/// the parts `Debug` can't render faithfully taken out first and rendered
/// in order — `environment` and `repository_credentials` (which `Debug`
/// redacts, so a change to them would otherwise go unseen) and the log
/// options (a `HashMap`, whose `Debug` order is random). Docker labels are
/// dropped: the fingerprint label is the only one, and it can't cover
/// itself. Hashing the built request rather than its inputs means any
/// change to [`task_definition_request`] registers a new revision on the
/// next start, with nothing to remember to bump. (An SDK upgrade that
/// changes `Debug` output does the same, once.)
fn request_fingerprint(req: &RegisterTaskDefinitionInput) -> String {
    let mut req = req.clone();
    let mut taken = Vec::new();
    for c in req.container_definitions.iter_mut().flatten() {
        c.docker_labels = None;
        let env: Vec<(Option<String>, Option<String>)> = c
            .environment
            .take()
            .unwrap_or_default()
            .iter()
            .map(|kv| (kv.name().map(String::from), kv.value().map(String::from)))
            .collect();
        let credentials = c
            .repository_credentials
            .take()
            .map(|r| r.credentials_parameter);
        let log_options: Option<BTreeMap<String, String>> = c
            .log_configuration
            .as_mut()
            .and_then(|l| l.options.take())
            .map(|m| m.into_iter().collect());
        taken.push(format!("{env:?}|{credentials:?}|{log_options:?}"));
    }
    Sha256::digest(format!("{req:?}\n{taken:?}").as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn health_check_command() -> String {
    format!(
        "curl -sf --max-time 3 -H \"Authorization: Bearer $(cat /dev/shm/triple-c/agent-token)\" \
         http://127.0.0.1:{AGENT_PORT}/v1/health"
    )
}

/// The `RegisterTaskDefinition` request for `plan`, fingerprint label included.
pub fn task_definition_request(plan: &WorkspacePlan) -> RegisterTaskDefinitionInput {
    let mut req = build_request(plan);
    let fingerprint = request_fingerprint(&req);
    for c in req.container_definitions.iter_mut().flatten() {
        c.docker_labels = Some([(FINGERPRINT_LABEL.to_string(), fingerprint.clone())].into());
    }
    req
}

/// The request without its fingerprint label.
fn build_request(plan: &WorkspacePlan) -> RegisterTaskDefinitionInput {
    const BUILT: &str = "every required field is set";
    let hybrid = match (&plan.storage, &plan.file_system_id, &plan.access_point_id) {
        (StorageMode::Hybrid, Some(fs), Some(ap)) => Some((fs, ap)),
        _ => None,
    };

    let mut container = ContainerDefinition::builder()
        .name(CONTAINER_NAME)
        .image(&plan.image)
        .essential(true)
        .set_environment(Some(
            FIXED_ENVIRONMENT
                .iter()
                .map(|(k, v)| KeyValuePair::builder().name(*k).value(*v).build())
                .collect(),
        ))
        .set_secrets(Some(
            plan.secret_keys
                .iter()
                .map(|k| {
                    Secret::builder()
                        .name(k)
                        .value_from(format!("{}:{k}::", plan.secret_arn))
                        .build()
                        .expect(BUILT)
                })
                .collect(),
        ))
        .health_check(
            HealthCheck::builder()
                .command("CMD-SHELL")
                .command(health_check_command())
                .interval(30)
                .retries(3)
                .start_period(120)
                .build()
                .expect(BUILT),
        )
        .linux_parameters(
            LinuxParameters::builder()
                .init_process_enabled(true)
                .capabilities(KernelCapabilities::builder().add("SYS_PTRACE").build())
                .build(),
        )
        .log_configuration(
            LogConfiguration::builder()
                .log_driver(LogDriver::Awslogs)
                .options("awslogs-group", &plan.log_group)
                .options("awslogs-region", &plan.region)
                .options("awslogs-stream-prefix", &plan.log_stream_prefix)
                .build()
                .expect(BUILT),
        );
    if hybrid.is_some() {
        container = container.mount_points(
            MountPoint::builder()
                .source_volume(HOME_VOLUME)
                .container_path(HOME_MOUNT_PATH)
                .read_only(false)
                .build(),
        );
    }

    let mut req = RegisterTaskDefinitionInput::builder()
        .family(&plan.family)
        .task_role_arn(&plan.task_role_arn)
        .execution_role_arn(&plan.exec_role_arn)
        .network_mode(NetworkMode::Awsvpc)
        .requires_compatibilities(Compatibility::Fargate)
        .cpu(plan.cpu.to_string())
        .memory(plan.memory.to_string())
        .runtime_platform(
            RuntimePlatform::builder()
                .cpu_architecture(match plan.arch {
                    Arch::X86_64 => CpuArchitecture::X8664,
                    Arch::Arm64 => CpuArchitecture::Arm64,
                })
                .operating_system_family(OsFamily::Linux)
                .build(),
        )
        .ephemeral_storage(
            EphemeralStorage::builder()
                .size_in_gib(plan.ephemeral_gib as i32)
                .build(),
        )
        .container_definitions(container.build())
        .set_tags(Some(
            plan.tags
                .iter()
                .map(|(k, v)| Tag::builder().key(k).value(v).build())
                .collect(),
        ));
    if let Some((fs, ap)) = hybrid {
        req = req.volumes(
            Volume::builder()
                .name(HOME_VOLUME)
                .efs_volume_configuration(
                    EfsVolumeConfiguration::builder()
                        .file_system_id(fs)
                        .transit_encryption(EfsTransitEncryption::Enabled)
                        .authorization_config(
                            EfsAuthorizationConfig::builder()
                                .access_point_id(ap)
                                .iam(EfsAuthorizationConfigIam::Enabled)
                                .build(),
                        )
                        .build()
                        .expect(BUILT),
                )
                .build(),
        );
    }
    req.build().expect(BUILT)
}

// ---------------------------------------------------------------------- API

#[derive(Debug, Clone, PartialEq)]
pub struct FileSystemRef {
    pub id: String,
    /// Its `triple-c:owner` tag.
    pub owner: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AccessPointRef {
    pub id: String,
    pub file_system_id: String,
    pub root_path: Option<String>,
    pub owner: Option<String>,
    pub project: Option<String>,
    /// EFS `LifeCycleState` (`creating`, `available`, `updating`, ...).
    pub state: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TaskDefinitionRef {
    /// `...:task-definition/<family>:<revision>`.
    pub arn: String,
    pub family: String,
    pub owner: Option<String>,
    pub project: Option<String>,
    /// The `workspace` container's [`FINGERPRINT_LABEL`].
    pub fingerprint: Option<String>,
}

/// What preparing a workspace asks AWS. Like `DetectApi`, each method
/// returns a `Send` future; an impl may write them as `async fn`. The
/// decisions (which access point or revision may be reused) are made by the
/// callers here, so a fake exercises them.
pub trait WorkspaceApi: Send + Sync {
    /// The file system whose creation token is `token`.
    fn find_file_system(
        &self,
        token: &str,
    ) -> impl Future<Output = Result<Option<FileSystemRef>, AwsError>> + Send;
    /// The access points on `fs_id` (bounded).
    fn access_points(
        &self,
        fs_id: &str,
    ) -> impl Future<Output = Result<Vec<AccessPointRef>, AwsError>> + Send;
    /// A new access point rooted at `root` (uid/gid 1000, created 0700).
    fn create_access_point(
        &self,
        fs_id: &str,
        root: &str,
        tags: &[(String, String)],
    ) -> impl Future<Output = Result<String, AwsError>> + Send;
    /// Create the secret (encrypted with `kms_key_arn`, tagged), or put a new
    /// value into it when it exists. Returns its ARN.
    fn put_project_secret(
        &self,
        name: &str,
        kms_key_arn: &str,
        json: &str,
        tags: &[(String, String)],
    ) -> impl Future<Output = Result<String, AwsError>> + Send;
    /// The ACTIVE revisions of exactly `family`, newest first (bounded).
    fn task_definition_revisions(
        &self,
        family: &str,
    ) -> impl Future<Output = Result<Vec<String>, AwsError>> + Send;
    /// One revision with its tags; `None` when it is gone.
    fn describe_task_definition(
        &self,
        arn: &str,
    ) -> impl Future<Output = Result<Option<TaskDefinitionRef>, AwsError>> + Send;
    /// Register and return the new revision's ARN.
    fn register_task_definition(
        &self,
        req: RegisterTaskDefinitionInput,
    ) -> impl Future<Output = Result<String, AwsError>> + Send;
}

/// `...:task-definition/<family>:<digits>`.
fn is_revision_arn_of(arn: &str, family: &str) -> bool {
    arn.starts_with("arn:")
        && arn
            .rsplit_once(":task-definition/")
            .and_then(|(_, rest)| rest.rsplit_once(':'))
            .is_some_and(|(fam, rev)| {
                fam == family && !rev.is_empty() && rev.bytes().all(|c| c.is_ascii_digit())
            })
}

/// The newest of `family`'s revisions (among the first
/// [`MAX_REVISIONS_CHECKED`]) that the caller registered for this project
/// with this fingerprint, by its exact ARN. A newer revision someone else
/// registered into the family is skipped, never used.
pub async fn latest_task_definition<W: WorkspaceApi>(
    api: &W,
    family: &str,
    owner_user_id: &str,
    project_id: &str,
    fingerprint: &str,
) -> Result<Option<String>, AwsError> {
    let revisions = api.task_definition_revisions(family).await?;
    for arn in revisions.iter().take(MAX_REVISIONS_CHECKED) {
        if !is_revision_arn_of(arn, family) {
            continue;
        }
        let Some(d) = api.describe_task_definition(arn).await? else {
            continue;
        };
        if d.arn == *arn
            && d.family == family
            && d.owner.as_deref() == Some(owner_user_id)
            && d.project.as_deref() == Some(project_id)
            && d.fingerprint.as_deref() == Some(fingerprint)
        {
            return Ok(Some(d.arn));
        }
    }
    Ok(None)
}

/// The caller's access point for this project: owned by them, tagged with
/// the project, rooted where it should be, and not on its way out.
fn pick_access_point<'a>(
    points: &'a [AccessPointRef],
    fs_id: &str,
    root: &str,
    owner_user_id: &str,
    project_id: &str,
) -> Option<&'a AccessPointRef> {
    points.iter().find(|a| {
        a.file_system_id == fs_id
            && a.root_path.as_deref() == Some(root)
            && a.owner.as_deref() == Some(owner_user_id)
            && a.project.as_deref() == Some(project_id)
            && matches!(
                a.state.as_deref(),
                Some("available" | "creating" | "updating")
            )
    })
}

#[derive(Debug, Clone, PartialEq)]
pub struct PreparedWorkspace {
    pub family: String,
    /// Exact `family:revision`.
    pub task_definition_arn: String,
    pub access_point_id: Option<String>,
    /// This start's token, already in the secret.
    pub agent_token: String,
    pub secret_arn: String,
}

/// Make the project's secret, access point (Hybrid) and task definition
/// ready. Everything that can be checked locally — placement, sizes, the
/// payload's size and names — is checked before the first write.
pub async fn prepare_workspace<W: WorkspaceApi>(
    api: &W,
    who: &CallerIdentity,
    env: &EnvironmentInfo,
    project: &Project,
    env_pairs: &[(String, String)],
    files: &[(String, Vec<u8>)],
    image: &str,
) -> Result<PreparedWorkspace, AwsError> {
    let (Some(placement), Some(cfg)) = (project.aws_placement.as_ref(), project.aws.as_ref())
    else {
        return Err(other("The project is not set up to run on AWS"));
    };
    if !project.is_aws() {
        return Err(other("The project is not set up to run on AWS"));
    }
    if placement.environment != env.name {
        return Err(other(format!(
            "The project belongs to environment \"{}\", not \"{}\"",
            placement.environment, env.name
        )));
    }
    let region = placement.region.as_str();
    let mut arn_parts = env.cluster_arn.split(':');
    let partition = arn_parts.nth(1).unwrap_or("aws");
    if arn_parts.nth(1) != Some(region) {
        return Err(other(format!(
            "The environment \"{}\" is not in the project's region {region}",
            env.name
        )));
    }
    validate_aws_workspace(cfg).map_err(other)?;
    if image.trim().is_empty() {
        return Err(other("No container image is set for AWS"));
    }

    let ok = owner_key(region, &who.user_id);
    let tags = workspace_tags(&who.user_id, &project.id, &env.name);
    let agent_token = new_agent_token();
    let payload = payload_map(
        env_pairs,
        &agent_token,
        files,
        &cfg.repos,
        cfg.wip_push_on_stop,
        cfg.idle_stop_minutes,
        cfg.storage,
    )
    .map_err(other)?;
    let json = serialize_payload(&payload).map_err(other)?;

    // Hybrid: the user's own file system, from their onboarding.
    let (file_system_id, access_point_id) = match cfg.storage {
        StorageMode::Ephemeral => (None, None),
        StorageMode::Hybrid => {
            let token = user_file_system_token(&env.name, &ok);
            let fs = api
                .find_file_system(&token)
                .await?
                .filter(|fs| fs.owner.as_deref() == Some(who.user_id.as_str()))
                .ok_or_else(|| {
                    other(format!(
                        "Your file system in environment \"{}\" was not found. Hybrid storage \
                         needs it: ask an administrator to onboard you there, or use \
                         Ephemeral storage.",
                        env.name
                    ))
                })?;
            let root = access_point_root(&ok, &project.id);
            let points = api.access_points(&fs.id).await?;
            let ap = match pick_access_point(&points, &fs.id, &root, &who.user_id, &project.id) {
                Some(a) => a.id.clone(),
                None => api.create_access_point(&fs.id, &root, &tags).await?,
            };
            (Some(fs.id), Some(ap))
        }
    };

    let secret_name = project_secret_name(&env.name, &ok, &project.id);
    let secret_arn = api
        .put_project_secret(&secret_name, &env.kms_key_arn, &json, &tags)
        .await?;

    let (task_role, exec_role) = user_role_names(&env.name, &ok);
    let role_arn = |name: &str| format!("arn:{partition}:iam::{}:role/{name}", who.account);
    let plan = WorkspacePlan {
        family: task_family(&env.name, &ok, &project.id),
        cpu: cfg.cpu,
        memory: cfg.memory_mib,
        arch: cfg.arch,
        storage: cfg.storage,
        ephemeral_gib: cfg.ephemeral_gib,
        image: image.to_string(),
        exec_role_arn: role_arn(&exec_role),
        task_role_arn: role_arn(&task_role),
        log_group: env.log_group.clone(),
        log_stream_prefix: log_stream_prefix(&ok, &project.id),
        region: region.to_string(),
        secret_arn: secret_arn.clone(),
        secret_keys: payload.keys().cloned().collect(),
        file_system_id,
        access_point_id: access_point_id.clone(),
        tags,
    };
    let fingerprint = workspace_fingerprint(&plan);
    let task_definition_arn =
        match latest_task_definition(api, &plan.family, &who.user_id, &project.id, &fingerprint)
            .await?
        {
            Some(arn) => arn,
            None => {
                let arn = api
                    .register_task_definition(task_definition_request(&plan))
                    .await?;
                if !is_revision_arn_of(&arn, &plan.family) {
                    return Err(other(
                        "ecs:RegisterTaskDefinition returned an ARN that is not a revision of the \
                     project's family",
                    ));
                }
                arn
            }
        };

    Ok(PreparedWorkspace {
        family: plan.family,
        task_definition_arn,
        access_point_id,
        agent_token,
        secret_arn,
    })
}

// ---------------------------------------------------------------- real API

/// Bounds on what one lookup lists.
const MAX_ACCESS_POINTS: usize = 1000;
const MAX_LISTED_REVISIONS: i32 = 100;
/// `ListTaskDefinitions` pages read per lookup; past that, a new revision is
/// registered (what crowding a family out can cost the caller).
const MAX_REVISION_PAGES: usize = 20;

/// Secrets Manager's `InvalidRequestException` for a name whose secret is
/// scheduled for deletion ("... already scheduled for deletion").
fn is_pending_deletion(message: Option<&str>) -> bool {
    message.is_some_and(|m| m.contains("scheduled for deletion"))
}

fn pending_deletion_error(name: &str) -> AwsError {
    other(format!(
        "This project's secret ({name}) is scheduled for deletion, so it can't be written. \
         Restore it in the Secrets Manager console (or wait until it is deleted) and start \
         the project again."
    ))
}

/// The real thing, from a connection's `SdkConfig`.
pub struct SdkWorkspaceApi {
    ecs: aws_sdk_ecs::Client,
    efs: aws_sdk_efs::Client,
    secrets: aws_sdk_secretsmanager::Client,
}

impl SdkWorkspaceApi {
    pub fn new(cfg: &SdkConfig) -> Self {
        SdkWorkspaceApi {
            ecs: aws_sdk_ecs::Client::new(cfg),
            efs: aws_sdk_efs::Client::new(cfg),
            secrets: aws_sdk_secretsmanager::Client::new(cfg),
        }
    }
}

fn efs_tag(tags: &[aws_sdk_efs::types::Tag], key: &str) -> Option<String> {
    tags.iter()
        .find(|t| t.key() == key)
        .map(|t| t.value().to_string())
}

fn ecs_tag(tags: &[Tag], key: &str) -> Option<String> {
    tags.iter()
        .find(|t| t.key() == Some(key))
        .and_then(|t| t.value())
        .map(str::to_string)
}

impl WorkspaceApi for SdkWorkspaceApi {
    async fn find_file_system(&self, token: &str) -> Result<Option<FileSystemRef>, AwsError> {
        const ACTION: &str = "elasticfilesystem:DescribeFileSystems";
        let out = self
            .efs
            .describe_file_systems()
            .creation_token(token)
            .send()
            .await
            .map_err(|e| classify_sdk_error(ACTION, &e))?;
        // One at most (a creation token is unique per account and region);
        // one still being created or deleted can't be mounted.
        Ok(out
            .file_systems()
            .iter()
            .find(|fs| *fs.life_cycle_state() == aws_sdk_efs::types::LifeCycleState::Available)
            .map(|fs| FileSystemRef {
                id: fs.file_system_id().to_string(),
                owner: efs_tag(fs.tags(), TAG_OWNER),
            }))
    }

    async fn access_points(&self, fs_id: &str) -> Result<Vec<AccessPointRef>, AwsError> {
        const ACTION: &str = "elasticfilesystem:DescribeAccessPoints";
        let mut out = Vec::new();
        let mut token: Option<String> = None;
        loop {
            let page = self
                .efs
                .describe_access_points()
                .file_system_id(fs_id)
                .set_next_token(token.take())
                .send()
                .await
                .map_err(|e| classify_sdk_error(ACTION, &e))?;
            for a in page.access_points() {
                out.push(AccessPointRef {
                    id: a.access_point_id().unwrap_or_default().to_string(),
                    file_system_id: a.file_system_id().unwrap_or_default().to_string(),
                    root_path: a
                        .root_directory()
                        .and_then(|r| r.path())
                        .map(str::to_string),
                    owner: efs_tag(a.tags(), TAG_OWNER),
                    project: efs_tag(a.tags(), TAG_PROJECT),
                    state: a.life_cycle_state().map(|s| s.as_str().to_string()),
                });
            }
            match page.next_token() {
                Some(t) if out.len() < MAX_ACCESS_POINTS => token = Some(t.to_string()),
                _ => break,
            }
        }
        Ok(out)
    }

    async fn create_access_point(
        &self,
        fs_id: &str,
        root: &str,
        tags: &[(String, String)],
    ) -> Result<String, AwsError> {
        use aws_sdk_efs::types::{CreationInfo, PosixUser, RootDirectory, Tag as EfsTag};
        const ACTION: &str = "elasticfilesystem:CreateAccessPoint";
        const BUILT: &str = "every required field is set";
        let out = self
            .efs
            .create_access_point()
            .file_system_id(fs_id)
            .posix_user(
                PosixUser::builder()
                    .uid(WORKSPACE_UID)
                    .gid(WORKSPACE_UID)
                    .build()
                    .expect(BUILT),
            )
            .root_directory(
                RootDirectory::builder()
                    .path(root)
                    .creation_info(
                        CreationInfo::builder()
                            .owner_uid(WORKSPACE_UID)
                            .owner_gid(WORKSPACE_UID)
                            .permissions("0700")
                            .build()
                            .expect(BUILT),
                    )
                    .build(),
            )
            .set_tags(Some(
                tags.iter()
                    .map(|(k, v)| EfsTag::builder().key(k).value(v).build().expect(BUILT))
                    .collect(),
            ))
            .send()
            .await
            .map_err(|e| classify_sdk_error(ACTION, &e))?;
        out.access_point_id()
            .map(str::to_string)
            .ok_or_else(|| other(format!("{ACTION} returned no AccessPointId")))
    }

    async fn put_project_secret(
        &self,
        name: &str,
        kms_key_arn: &str,
        json: &str,
        tags: &[(String, String)],
    ) -> Result<String, AwsError> {
        use aws_sdk_secretsmanager::types::Tag as SmTag;
        const CREATE: &str = "secretsmanager:CreateSecret";
        const PUT: &str = "secretsmanager:PutSecretValue";
        let created = self
            .secrets
            .create_secret()
            .name(name)
            .kms_key_id(kms_key_arn)
            .secret_string(json)
            .set_tags(Some(
                tags.iter()
                    .map(|(k, v)| SmTag::builder().key(k).value(v).build())
                    .collect(),
            ))
            .send()
            .await;
        let arn = match created {
            Ok(out) => out.arn().map(str::to_string),
            Err(e)
                if e.as_service_error()
                    .is_some_and(|s| s.is_resource_exists_exception()) =>
            {
                // Only a secret tagged with the caller as owner accepts this
                // (the policy), so another user's same-named secret fails
                // here rather than receiving the payload.
                self.secrets
                    .put_secret_value()
                    .secret_id(name)
                    .secret_string(json)
                    .send()
                    .await
                    .map_err(|e| match e.as_service_error() {
                        Some(s)
                            if s.is_invalid_request_exception()
                                && is_pending_deletion(s.message()) =>
                        {
                            pending_deletion_error(name)
                        }
                        _ => classify_sdk_error(PUT, &e),
                    })?
                    .arn()
                    .map(str::to_string)
            }
            Err(e)
                if e.as_service_error().is_some_and(|s| {
                    s.is_invalid_request_exception() && is_pending_deletion(s.message())
                }) =>
            {
                return Err(pending_deletion_error(name));
            }
            Err(e) => return Err(classify_sdk_error(CREATE, &e)),
        };
        arn.ok_or_else(|| other(format!("{CREATE} returned no ARN")))
    }

    async fn task_definition_revisions(&self, family: &str) -> Result<Vec<String>, AwsError> {
        const ACTION: &str = "ecs:ListTaskDefinitions";
        // `familyPrefix` is a prefix, and sorted descending a longer family
        // (`<family>z`, anyone can register one) comes first: page on until
        // enough of exactly this family is found, or the pages run out.
        let mut found = Vec::new();
        let mut token: Option<String> = None;
        for _ in 0..MAX_REVISION_PAGES {
            let page = self
                .ecs
                .list_task_definitions()
                .family_prefix(family)
                .status(TaskDefinitionStatus::Active)
                .sort(SortOrder::Desc)
                .max_results(MAX_LISTED_REVISIONS)
                .set_next_token(token.take())
                .send()
                .await
                .map_err(|e| classify_sdk_error(ACTION, &e))?;
            found.extend(
                page.task_definition_arns()
                    .iter()
                    .filter(|a| is_revision_arn_of(a, family))
                    .cloned(),
            );
            match page.next_token() {
                Some(t) if found.len() < MAX_REVISIONS_CHECKED => token = Some(t.to_string()),
                _ => break,
            }
        }
        Ok(found)
    }

    async fn describe_task_definition(
        &self,
        arn: &str,
    ) -> Result<Option<TaskDefinitionRef>, AwsError> {
        const ACTION: &str = "ecs:DescribeTaskDefinition";
        let out = self
            .ecs
            .describe_task_definition()
            .task_definition(arn)
            .include(TaskDefinitionField::Tags)
            .send()
            .await
            .map_err(|e| classify_sdk_error(ACTION, &e))?;
        let Some(td) = out.task_definition() else {
            return Ok(None);
        };
        let fingerprint = td
            .container_definitions()
            .iter()
            .find(|c| c.name() == Some(CONTAINER_NAME))
            .and_then(|c| c.docker_labels())
            .and_then(|l| l.get(FINGERPRINT_LABEL))
            .cloned();
        Ok(Some(TaskDefinitionRef {
            arn: td.task_definition_arn().unwrap_or_default().to_string(),
            family: td.family().unwrap_or_default().to_string(),
            owner: ecs_tag(out.tags(), TAG_OWNER),
            project: ecs_tag(out.tags(), TAG_PROJECT),
            fingerprint,
        }))
    }

    async fn register_task_definition(
        &self,
        req: RegisterTaskDefinitionInput,
    ) -> Result<String, AwsError> {
        const ACTION: &str = "ecs:RegisterTaskDefinition";
        // The fluent builder takes no whole input; copy every field the
        // input has, so nothing `task_definition_request` sets is lost.
        let out = self
            .ecs
            .register_task_definition()
            .set_family(req.family)
            .set_task_role_arn(req.task_role_arn)
            .set_execution_role_arn(req.execution_role_arn)
            .set_network_mode(req.network_mode)
            .set_container_definitions(req.container_definitions)
            .set_volumes(req.volumes)
            .set_placement_constraints(req.placement_constraints)
            .set_requires_compatibilities(req.requires_compatibilities)
            .set_cpu(req.cpu)
            .set_memory(req.memory)
            .set_tags(req.tags)
            .set_pid_mode(req.pid_mode)
            .set_ipc_mode(req.ipc_mode)
            .set_proxy_configuration(req.proxy_configuration)
            .set_ephemeral_storage(req.ephemeral_storage)
            .set_runtime_platform(req.runtime_platform)
            .set_enable_fault_injection(req.enable_fault_injection)
            .set_trusted_execution_configuration(req.trusted_execution_configuration)
            .send()
            .await
            .map_err(|e| classify_sdk_error(ACTION, &e))?;
        out.task_definition()
            .and_then(|t| t.task_definition_arn())
            .map(str::to_string)
            .ok_or_else(|| other(format!("{ACTION} returned no task definition ARN")))
    }
}

#[cfg(test)]
mod tests;
