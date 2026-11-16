//! A project's AWS resources, made ready before its task runs: the project
//! secret, the EFS access points (Hybrid: home; EFS: home and `/workspace`)
//! and the task definition.
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
    user_file_system_token, user_role_names, workspace_access_point_root,
};
use crate::host::ssm::AGENT_PORT;
use crate::models::project::{validate_aws_workspace, Arch, RepoSource, SecretsDelivery, StorageMode};
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
/// Where an EFS-mode project's second access point is mounted (Q1).
pub const WORKSPACE_MOUNT_PATH: &str = "/workspace";
const WORKSPACE_VOLUME: &str = "workspace";
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
const KEY_SETUP_SCRIPT: &str = "TRIPLEC_SETUP_SCRIPT";
const KEY_MAX_LIFETIME: &str = "TRIPLEC_MAX_LIFETIME_MINUTES";
/// `connect` when the credentials are copied in at connect (Q2) and so are
/// not in the secret; absent otherwise.
const KEY_CREDENTIALS: &str = "TRIPLEC_CREDENTIALS";
/// Seconds ECS waits after SIGTERM before SIGKILL (Fargate allows 2–120).
pub const STOP_TIMEOUT_SECS: i32 = 120;

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
/// not `.` or `..` (R1: the entrypoint writes it under `/tmp/.host-*/`;
/// copy-at-connect uploads it under `/dev/shm/triple-c/connect/*/`).
pub(crate) fn validate_file_key(key: &str) -> Result<(), String> {
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

/// Payload keys that come from the project's AWS settings and its
/// environment rather than from the env pairs (phase 6).
#[derive(Debug, Clone, Copy, Default)]
pub struct PayloadExtras<'a> {
    /// `AwsWorkspaceConfig::setup_script`; blank is the same as none.
    pub setup_script: Option<&'a str>,
    /// The environment's cap (`max_workspace_hours`), in hours; 0 = none.
    pub max_workspace_hours: u32,
    /// Copy-at-connect (Q2): the names of the project's custom environment
    /// variables. With it, the credentials ([`is_credential_key`]) and
    /// `TRIPLEC_FILES` stay out of the secret and `TRIPLEC_CREDENTIALS` is
    /// `connect`; the app copies them in after each connect instead.
    /// `None`: everything is in the secret (Secrets Manager delivery).
    pub connect_only: Option<&'a [String]>,
}

/// `workspace_env`'s fingerprint of the custom variables' values.
const CUSTOM_ENV_FINGERPRINT: &str = "TRIPLE_C_CUSTOM_ENV";

/// A credential, for copy-at-connect: an API key or token Triple-C manages
/// (`SECRET_ENV_KEYS`, `GIT_TOKEN` included) or any custom variable — a
/// custom value may be a key, and nothing tells which.
pub fn is_credential_key(key: &str, custom_keys: &[String]) -> bool {
    crate::docker::container::SECRET_ENV_KEYS.contains(&key) || custom_keys.iter().any(|c| c == key)
}

/// The credential pairs of `env_pairs` ([`is_credential_key`]), in order:
/// what copy-at-connect copies in and adds to the terminals' environment.
pub fn credential_pairs(env_pairs: &[(String, String)], custom_keys: &[String]) -> Vec<(String, String)> {
    env_pairs
        .iter()
        .filter(|(k, _)| is_credential_key(k, custom_keys))
        .cloned()
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn payload_map(
    env_pairs: &[(String, String)],
    agent_token: &str,
    files: &[(String, Vec<u8>)],
    repos: &[RepoSource],
    wip: bool,
    idle: Option<u32>,
    storage: StorageMode,
    extras: PayloadExtras<'_>,
) -> Result<BTreeMap<String, String>, String> {
    let own = [
        KEY_AGENT_TOKEN,
        KEY_FILES,
        KEY_REPOS,
        KEY_WIP_PUSH,
        KEY_IDLE,
        KEY_STORAGE,
        KEY_SETUP_SCRIPT,
        KEY_MAX_LIFETIME,
        KEY_CREDENTIALS,
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
        // The whole `TRIPLEC_` namespace is the entrypoint's (the Docker side
        // reserves it too: `RESERVED_ENV_PREFIXES`), not just the keys this
        // payload writes — `TRIPLEC_AGENT_ADDR`, say, would rebind the agent.
        if own.contains(&k.as_str())
            || FIXED_ENVIRONMENT.iter().any(|(f, _)| f == k)
            || k.to_ascii_uppercase().starts_with("TRIPLEC_")
        {
            log::warn!("Ignoring the environment variable {k}: Triple-C sets it on AWS");
            continue;
        }
        // Copy-at-connect: copied in after each connect, never stored. The
        // custom variables' fingerprint goes too: an unsalted hash of their
        // values would let a low-entropy one be guessed offline (and nothing
        // on AWS reads it; Docker uses it to notice a change).
        if extras
            .connect_only
            .is_some_and(|custom| is_credential_key(k, custom) || k == CUSTOM_ENV_FINGERPRINT)
        {
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
    // The agent token stays either way: it is how the app reaches the task.
    if extras.connect_only.is_some() {
        // The SSH keys and CA files arrive with the connect too.
        m.insert(KEY_CREDENTIALS.into(), "connect".into());
    } else {
        m.insert(KEY_FILES.into(), json(&file_map));
    }
    // Ruling R1 (Task 8): the LLM gateway sidecar starts before any connect,
    // so its `TRIPLEC_GATEWAY_*` keys are added as payload keys here and stay
    // in the secret even under copy-at-connect (the UI says so). They are
    // payload keys, not env pairs, so the credential filter above never
    // touches them.
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
            StorageMode::Efs => "efs",
        }
        .into(),
    );
    if let Some(script) = extras.setup_script.filter(|s| !s.trim().is_empty()) {
        m.insert(KEY_SETUP_SCRIPT.into(), script.into());
    }
    if extras.max_workspace_hours > 0 {
        m.insert(
            KEY_MAX_LIFETIME.into(),
            (u64::from(extras.max_workspace_hours) * 60).to_string(),
        );
    }
    Ok(m)
}

/// A nested JSON value (string keys, plain data: cannot fail).
fn json<T: Serialize + ?Sized>(v: &T) -> String {
    serde_json::to_string(v).expect("plain data serialises")
}

/// What a payload key is, in the size error's words. Custom variables are
/// named (names are not secret; values never appear).
fn describe_payload_key(key: &str) -> String {
    match key {
        KEY_SETUP_SCRIPT => "the setup script".into(),
        KEY_FILES => "the SSH and CA files".into(),
        KEY_REPOS => "the repository list".into(),
        "CLAUDE_INSTRUCTIONS" => "the instructions".into(),
        "CLAUDE_CODE_SETTINGS_JSON" => "the Claude Code settings".into(),
        CUSTOM_ENV_FINGERPRINT => "the custom environment variables".into(),
        k if k.starts_with("TRIPLEC_GATEWAY_") => "the LLM gateway configuration".into(),
        k => format!("the environment variable {k}"),
    }
}

/// `n` bytes in KiB, rounded up (so a part is never shown as 0 KiB).
fn kib(n: usize) -> usize {
    n.div_ceil(1024)
}

fn serialize_payload(m: &BTreeMap<String, String>) -> Result<String, String> {
    let json = serde_json::to_string(m).expect("a string map serialises");
    if json.len() > MAX_SECRET_BYTES {
        // Name what is large: the parts add up (setup script ≤ 32 KiB,
        // gateway config, instructions, files…), so the total alone does not
        // say what to shorten. A part's size is its serialised value.
        let mut parts: Vec<(usize, &str)> = m
            .iter()
            .map(|(k, v)| (serde_json::to_string(v).map_or(v.len(), |j| j.len()), k.as_str()))
            .collect();
        parts.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(b.1)));
        let largest: Vec<String> = parts
            .iter()
            .take(3)
            .filter(|(n, _)| *n >= 1024)
            .map(|(n, k)| format!("{} ({} KiB)", describe_payload_key(k), kib(*n)))
            .collect();
        let detail = if largest.is_empty() {
            String::new()
        } else {
            format!(" The largest parts: {}.", largest.join(", "))
        };
        return Err(format!(
            "The project's settings for AWS come to {} KiB ({} bytes), more than the 64 KiB \
             (65,536 bytes) a Secrets Manager secret holds.{detail} Shorten the setup script, \
             instructions, Claude Code settings or custom environment variables, or remove \
             files from the SSH folder.",
            kib(json.len()),
            json.len()
        ));
    }
    Ok(json)
}

/// The project secret's JSON (P1, R1): every env pair (under copy-at-connect,
/// every one but the credentials), then the payload's own keys
/// (`TRIPLEC_AGENT_TOKEN`, `TRIPLEC_FILES` = `{"ssh/<f>"|"ca/<f>": base64}`
/// — or, under copy-at-connect, `TRIPLEC_CREDENTIALS` = `connect` instead —
/// `TRIPLEC_REPOS`, `TRIPLEC_WIP_PUSH` = `1`/`0`, `TRIPLEC_IDLE_STOP_MINUTES`
/// when on, `TRIPLEC_STORAGE` = `ephemeral`/`hybrid`/`efs`,
/// `TRIPLEC_SETUP_SCRIPT` when set, `TRIPLEC_MAX_LIFETIME_MINUTES` when the
/// environment caps it), keys sorted. Fails when it would exceed 65,536 bytes.
#[cfg_attr(not(test), allow(dead_code))]
#[allow(clippy::too_many_arguments)]
pub fn project_secret_payload_with(
    env_pairs: &[(String, String)],
    agent_token: &str,
    files: &[(String, Vec<u8>)],
    repos: &[RepoSource],
    wip: bool,
    idle: Option<u32>,
    storage: StorageMode,
    extras: PayloadExtras<'_>,
) -> Result<String, String> {
    serialize_payload(&payload_map(
        env_pairs,
        agent_token,
        files,
        repos,
        wip,
        idle,
        storage,
        extras,
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
    /// Both set for Hybrid and EFS, both `None` for Ephemeral.
    pub file_system_id: Option<String>,
    pub access_point_id: Option<String>,
    /// The `/workspace` access point; used only when `storage` is EFS.
    pub workspace_access_point_id: Option<String>,
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
        (StorageMode::Hybrid | StorageMode::Efs, Some(fs), Some(ap)) => Some((fs, ap)),
        _ => None,
    };
    // EFS = Hybrid + `/workspace` on its own access point. Hybrid and
    // Ephemeral requests are exactly what they were before EFS mode (a
    // test pins their fingerprints).
    let workspace = match (&plan.storage, hybrid, &plan.workspace_access_point_id) {
        (StorageMode::Efs, Some((fs, _)), Some(ap)) => Some((fs, ap)),
        _ => None,
    };

    let mut container = ContainerDefinition::builder()
        .name(CONTAINER_NAME)
        .image(&plan.image)
        .essential(true)
        // SIGTERM → SIGKILL grace. The entrypoint's TERM trap pushes unsaved
        // work (`triple-c-wip-push`, bounded at 90 s) before it exits; ECS's
        // default 30 s would kill most pushes part-way. 120 is Fargate's max.
        .stop_timeout(STOP_TIMEOUT_SECS)
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
    if workspace.is_some() {
        container = container.mount_points(
            MountPoint::builder()
                .source_volume(WORKSPACE_VOLUME)
                .container_path(WORKSPACE_MOUNT_PATH)
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
    for (name, (fs, ap)) in [(HOME_VOLUME, hybrid), (WORKSPACE_VOLUME, workspace)]
        .into_iter()
        .filter_map(|(n, v)| v.map(|v| (n, v)))
    {
        req = req.volumes(efs_volume(name, fs, ap));
    }
    req.build().expect(BUILT)
}

/// An EFS volume through an access point, IAM-authorised, TLS in transit.
fn efs_volume(name: &str, fs: &str, ap: &str) -> Volume {
    Volume::builder()
        .name(name)
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
                .expect("every required field is set"),
        )
        .build()
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
    /// The secret's current `SecretString`; `None` when there is no secret.
    fn project_secret_value(
        &self,
        name: &str,
    ) -> impl Future<Output = Result<Option<String>, AwsError>> + Send;
    /// Delete the secret now, every version with it
    /// (`ForceDeleteWithoutRecovery`); no secret is not an error.
    fn delete_project_secret(&self, name: &str) -> impl Future<Output = Result<(), AwsError>> + Send;
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
    /// EFS storage's `/workspace` access point.
    pub workspace_access_point_id: Option<String>,
    /// This start's token, already in the secret.
    pub agent_token: String,
    pub secret_arn: String,
}

/// Make the project's secret, access points (Hybrid, EFS) and task definition
/// ready. Everything that can be checked locally — placement, sizes, the
/// payload's size and names — is checked before the first write.
#[allow(clippy::too_many_arguments)]
pub async fn prepare_workspace<W: WorkspaceApi>(
    api: &W,
    who: &CallerIdentity,
    env: &EnvironmentInfo,
    project: &Project,
    env_pairs: &[(String, String)],
    files: &[(String, Vec<u8>)],
    custom_env_keys: &[String],
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
        PayloadExtras {
            setup_script: cfg.setup_script.as_deref(),
            max_workspace_hours: env.max_workspace_hours,
            connect_only: (cfg.secrets_delivery == SecretsDelivery::CopyAtConnect)
                .then_some(custom_env_keys),
        },
    )
    .map_err(other)?;
    let json = serialize_payload(&payload).map_err(other)?;

    // Hybrid and EFS: the user's own file system, from their onboarding.
    let (file_system_id, access_point_id, workspace_access_point_id) = match cfg.storage {
        StorageMode::Ephemeral => (None, None, None),
        StorageMode::Hybrid | StorageMode::Efs => {
            let mode = if cfg.storage == StorageMode::Efs { "EFS" } else { "Hybrid" };
            let token = user_file_system_token(&env.name, &ok);
            let fs = api
                .find_file_system(&token)
                .await?
                .filter(|fs| fs.owner.as_deref() == Some(who.user_id.as_str()))
                .ok_or_else(|| {
                    other(format!(
                        "Your file system in environment \"{}\" was not found. {mode} storage \
                         needs it: ask an administrator to onboard you there, or use \
                         Ephemeral storage.",
                        env.name
                    ))
                })?;
            let points = api.access_points(&fs.id).await?;
            let ensure = |root: String| {
                let found = pick_access_point(&points, &fs.id, &root, &who.user_id, &project.id)
                    .map(|a| a.id.clone());
                let (fs_id, tags) = (&fs.id, &tags);
                async move {
                    match found {
                        Some(id) => Ok(id),
                        None => api.create_access_point(fs_id, &root, tags).await,
                    }
                }
            };
            let ap = ensure(access_point_root(&ok, &project.id)).await?;
            let workspace_ap = if cfg.storage == StorageMode::Efs {
                Some(ensure(workspace_access_point_root(&ok, &project.id)).await?)
            } else {
                None
            };
            (Some(fs.id), Some(ap), workspace_ap)
        }
    };

    let secret_name = project_secret_name(&env.name, &ok, &project.id);
    let secret_arn = match extras_custom(cfg, custom_env_keys) {
        // Copy-at-connect over a secret that still holds credentials (it was
        // Secrets Manager before, or a custom variable has gone): a new value
        // would leave them in the secret's version history (AWSPREVIOUS), so
        // the secret is deleted outright and made again (ruling R7).
        Some(custom) => {
            let old = api.project_secret_value(&secret_name).await?;
            if old.as_deref().is_some_and(|o| holds_credentials(o, &payload, custom)) {
                log::info!(
                    "Project {}: its secret still held credentials; replacing it so no version keeps them",
                    project.id
                );
                replace_project_secret(
                    api,
                    &secret_name,
                    &env.kms_key_arn,
                    &json,
                    &tags,
                    RECREATE_TIMING,
                )
                .await?
            } else {
                api.put_project_secret(&secret_name, &env.kms_key_arn, &json, &tags).await?
            }
        }
        None => api.put_project_secret(&secret_name, &env.kms_key_arn, &json, &tags).await?,
    };

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
        workspace_access_point_id: workspace_access_point_id.clone(),
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
        workspace_access_point_id,
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
/// Copy-at-connect's custom variable names, or `None` for Secrets Manager.
fn extras_custom<'a>(
    cfg: &crate::models::project::AwsWorkspaceConfig,
    custom_env_keys: &'a [String],
) -> Option<&'a [String]> {
    (cfg.secrets_delivery == SecretsDelivery::CopyAtConnect).then_some(custom_env_keys)
}

/// Whether a secret's current value `old` holds anything copy-at-connect
/// keeps out: the files, the custom variables' fingerprint, a credential
/// key, or an environment variable the new payload no longer has (a custom
/// variable since removed). `TRIPLEC_*` keys are the payload's own. A value
/// that is not a JSON object counts as holding them.
pub fn holds_credentials(old: &str, new: &BTreeMap<String, String>, custom: &[String]) -> bool {
    let Ok(old) = serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(old) else {
        return true;
    };
    old.keys().any(|k| {
        k == KEY_FILES
            || k == CUSTOM_ENV_FINGERPRINT
            || is_credential_key(k, custom)
            || (!k.starts_with("TRIPLEC_") && !new.contains_key(k))
    })
}

/// How long a replaced secret's name may stay taken by its deletion.
#[derive(Debug, Clone, Copy)]
pub struct RecreateTiming {
    pub interval: std::time::Duration,
    pub limit: std::time::Duration,
}

const RECREATE_TIMING: RecreateTiming = RecreateTiming {
    interval: std::time::Duration::from_secs(2),
    limit: std::time::Duration::from_secs(30),
};

fn is_pending_deletion_error(e: &AwsError) -> bool {
    matches!(e, AwsError::Other { message } if message.contains("is scheduled for deletion"))
}

/// Delete the secret with every version, then create it with `json`,
/// retrying while AWS finishes the (asynchronous) delete.
pub async fn replace_project_secret<W: WorkspaceApi>(
    api: &W,
    name: &str,
    kms_key_arn: &str,
    json: &str,
    tags: &[(String, String)],
    timing: RecreateTiming,
) -> Result<String, AwsError> {
    api.delete_project_secret(name).await?;
    let deadline = tokio::time::Instant::now() + timing.limit;
    loop {
        match api.put_project_secret(name, kms_key_arn, json, tags).await {
            Err(e) if is_pending_deletion_error(&e) => {
                if tokio::time::Instant::now() + timing.interval > deadline {
                    return Err(other(format!(
                        "The project's secret ({name}) was deleted to remove the credentials it                          held, but AWS has not finished deleting it after {} s. Start the project                          again in a minute.",
                        timing.limit.as_secs()
                    )));
                }
                tokio::time::sleep(timing.interval).await;
            }
            r => return r,
        }
    }
}

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

    async fn project_secret_value(&self, name: &str) -> Result<Option<String>, AwsError> {
        const ACTION: &str = "secretsmanager:GetSecretValue";
        match self.secrets.get_secret_value().secret_id(name).send().await {
            Ok(out) => Ok(out.secret_string().map(str::to_string)),
            Err(e)
                if e.as_service_error()
                    .is_some_and(|s| s.is_resource_not_found_exception()) =>
            {
                Ok(None)
            }
            Err(e) => Err(classify_sdk_error(ACTION, &e)),
        }
    }

    async fn delete_project_secret(&self, name: &str) -> Result<(), AwsError> {
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
            Err(e) => Err(classify_sdk_error(ACTION, &e)),
        }
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
