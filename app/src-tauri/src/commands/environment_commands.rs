//! Phase 4: AWS environments, deployed by the app with OpenTofu (D1–D10).
//!
//! Every command takes a connection id and a region, re-validates both, and
//! re-runs detection on this side before it changes anything: the frontend's
//! idea of what an environment allows is never trusted. Destroy runs only
//! when detection offers Destroy (no valid record, D6 / R12), Unlock only on
//! a stale lock this machine isn't holding, Repair and Update only when
//! offered.
//!
//! Applies are detached (D10). Each started or followed run gets one task
//! here that follows its log and emits [`PROGRESS_EVENT`] batches, then a
//! final `done` with the result. A deployment is followed by at most one task
//! per process ([`FOLLOWERS`]).
//!
//! Credentials: a keys connection's keys are read from the keychain here
//! and go only into [`tofu::child_env`]; they are never logged, written or
//! put in an event. The container's (or the app's) own `AWS_*` variables
//! are never used: `child_env` builds the runner's whole environment.
//!
//! The admin bundle (D5) is written into a folder the user picks in a
//! Rust-side dialog, for the same reason `file_commands::pick_save_path`
//! exists: no host path crosses IPC as an input.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use aws_config::SdkConfig;
use serde::Serialize;
use serde_json::Value;
use tauri::{AppHandle, Emitter, State};

use crate::aws::environments::{
    ensure_state_bucket, scan, DetectedEnvironment, EnvironmentAction, EnvironmentInfo,
    EnvironmentScan, EnvironmentState, Knowable, OnboardingState, SdkDetectApi,
};
use crate::aws::tofu::{
    self, backend_hcl, StartError, TofuEvent, TofuOp, TofuOutcome, Workdir,
};
use crate::aws::tofu_modules::{
    boundary_vars, environment_vars, module_files, onboarding_vars, owner_key, state_bucket_name,
    state_key, validate_environment_name, Deployment, ModuleKind, NetworkChoice,
};
use crate::aws::{caller_identity, sdk_config, AwsError, CallerIdentity};
use crate::models::{validate_supported_region, AwsAuth, HostConnection, HostKind};
use crate::storage::secure;
use crate::AppState;

/// The Tauri event carrying a run's progress ([`DeployProgress`]).
pub const PROGRESS_EVENT: &str = "aws-deploy-progress";
/// How often a follower polls the run's log.
const FOLLOW_POLL: Duration = Duration::from_millis(500);
/// At most this many events per emitted batch (a reattach replays the
/// whole log at once).
const MAX_EVENTS_PER_BATCH: usize = 200;
/// A state object read back for its outputs larger than this is refused.
const MAX_STATE_BYTES: usize = 32 * 1024 * 1024;

fn other(message: impl Into<String>) -> AwsError {
    AwsError::Other {
        message: message.into(),
    }
}

// ---------------------------------------------------------------------------
// Pure helpers (unit-tested)

/// An owned [`Deployment`]: what a state key from the frontend parses into.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DeploymentKey {
    Environment(String),
    User { env: String, owner_key: String },
    Boundary,
}

impl DeploymentKey {
    pub(crate) fn deployment(&self) -> Deployment<'_> {
        match self {
            DeploymentKey::Environment(env) => Deployment::Environment(env),
            DeploymentKey::User { env, owner_key } => Deployment::User { env, owner_key },
            DeploymentKey::Boundary => Deployment::Boundary,
        }
    }

    /// How a message names it.
    fn label(&self) -> String {
        match self {
            DeploymentKey::Environment(env) => format!("environment \"{env}\""),
            DeploymentKey::User { env, .. } => format!("the onboarding to \"{env}\""),
            DeploymentKey::Boundary => "the permissions boundary".to_string(),
        }
    }
}

fn is_owner_key(s: &str) -> bool {
    s.len() == 12 && s.bytes().all(|c| matches!(c, b'0'..=b'9' | b'a'..=b'f'))
}

/// A state key from the frontend, accepted only if [`state_key`] could have
/// produced it: a valid environment name, a 12-hex-character owner key, and
/// nothing else. The parse is checked by rebuilding the key.
pub(crate) fn parse_state_key(key: &str) -> Result<DeploymentKey, AwsError> {
    let refuse = || other("Not a Triple-C deployment's state key");
    let parts: Vec<&str> = key.split('/').collect();
    let parsed = match parts.as_slice() {
        ["environments", env, "terraform.tfstate"] => {
            validate_environment_name(env).map_err(|_| refuse())?;
            DeploymentKey::Environment(env.to_string())
        }
        ["users", env, ok, "terraform.tfstate"] => {
            validate_environment_name(env).map_err(|_| refuse())?;
            if !is_owner_key(ok) {
                return Err(refuse());
            }
            DeploymentKey::User {
                env: env.to_string(),
                owner_key: ok.to_string(),
            }
        }
        ["boundary", "terraform.tfstate"] => DeploymentKey::Boundary,
        _ => return Err(refuse()),
    };
    if state_key(parsed.deployment()) != key {
        return Err(refuse());
    }
    Ok(parsed)
}

fn find<'a>(scan: &'a EnvironmentScan, name: &str) -> Option<&'a DetectedEnvironment> {
    scan.environments.iter().find(|e| e.name == name)
}

/// Why a new environment named `name` can't be deployed: an invalid name,
/// a missing permissions boundary (every module role needs it), or a record
/// or state for the name already. A boundary the caller can't see is let
/// through; the apply reports it.
pub(crate) fn deploy_refusal(scan: &EnvironmentScan, name: &str) -> Result<(), String> {
    let name = validate_environment_name(name)?;
    if scan.boundary == Knowable::No {
        return Err(
            "The account has no Triple-C permissions boundary yet; deploy the permissions \
             boundary first"
                .into(),
        );
    }
    if find(scan, &name).is_some() {
        return Err(format!(
            "An environment named \"{name}\" already exists in this account and region"
        ));
    }
    Ok(())
}

/// The environment's network as deployed, from its record: an update or a
/// repair never changes it. The record has no VPC id, so an `existing`
/// network takes it from the state's `vpc_id` output.
pub(crate) fn network_from_record(
    info: &EnvironmentInfo,
    vpc_id: Option<&str>,
) -> Result<NetworkChoice, String> {
    match info.network_mode.as_str() {
        "public" => Ok(NetworkChoice::Public),
        "private-nat" => Ok(NetworkChoice::PrivateNat),
        "existing" => {
            let vpc_id = vpc_id.ok_or_else(|| {
                "The environment's VPC could not be read from its state".to_string()
            })?;
            let choice = NetworkChoice::Existing {
                vpc_id: vpc_id.to_string(),
                subnet_ids: info.subnet_ids.clone(),
                assign_public_ip: info.assign_public_ip,
            };
            // The same checks as the apply's variables.
            environment_vars(&info.name, "us-east-1", &choice)?;
            Ok(choice)
        }
        _ => Err("The environment record names an unknown network mode".into()),
    }
}

/// `outputs.vpc_id.value` of a state object, if it is a VPC id.
pub(crate) fn vpc_id_from_state(state: &[u8]) -> Option<String> {
    let v: Value = serde_json::from_slice(state).ok()?;
    let id = v.pointer("/outputs/vpc_id/value")?.as_str()?;
    let hex = id.strip_prefix("vpc-")?;
    ((hex.len() == 8 || hex.len() == 17)
        && hex.bytes().all(|c| matches!(c, b'0'..=b'9' | b'a'..=b'f')))
    .then(|| id.to_string())
}

/// `^[A-Z0-9]{16,128}(:[A-Za-z0-9+=,.@_-]{1,64})?$`: an `aws:userid` (a
/// user's unique id, or a role's id and session name). The onboarding
/// module tags with it and hashes it into role names.
pub(crate) fn validate_owner_id(id: &str) -> Result<String, String> {
    let (unique, session) = match id.split_once(':') {
        Some((u, s)) => (u, Some(s)),
        None => (id, None),
    };
    let unique_ok = (16..=128).contains(&unique.len())
        && unique
            .bytes()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit());
    let session_ok = session.is_none_or(|s| {
        (1..=64).contains(&s.len())
            && s.bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"+=,.@_-".contains(&c))
    });
    if unique_ok && session_ok {
        Ok(id.to_string())
    } else {
        Err("The owner id is not an AWS user id (aws:userid, e.g. AIDA… or AROA…:session)".into())
    }
}

/// The environment, if detection offers `action` on it.
pub(crate) fn require_action<'a>(
    scan: &'a EnvironmentScan,
    name: &str,
    action: EnvironmentAction,
) -> Result<&'a DetectedEnvironment, String> {
    let env = find(scan, name)
        .ok_or_else(|| format!("No environment named \"{name}\" was found"))?;
    if env.actions.contains(&action) {
        Ok(env)
    } else {
        Err(format!(
            "{} is not available for environment \"{name}\" in its current state",
            match action {
                EnvironmentAction::Join => "Joining",
                EnvironmentAction::Update => "Update",
                EnvironmentAction::Repair => "Repair",
                EnvironmentAction::Destroy => "Destroy",
                EnvironmentAction::Follow => "Follow",
                EnvironmentAction::Unlock => "Unlock",
            }
        ))
    }
}

/// Unlock only a stale lock no local runner holds: detection offers Unlock
/// and the environment is Incomplete with such a lock.
pub(crate) fn unlock_refusal(scan: &EnvironmentScan, name: &str) -> Result<(), String> {
    let env = require_action(scan, name, EnvironmentAction::Unlock)?;
    match &env.state {
        EnvironmentState::Incomplete { lock: Some(l) } if !l.local_process => Ok(()),
        _ => Err(format!(
            "Environment \"{name}\" has no stale state lock to remove"
        )),
    }
}

/// Self-onboarding needs the boundary (not known missing), an environment
/// that can be joined, and no onboarding already running.
pub(crate) fn onboard_refusal(scan: &EnvironmentScan, name: &str) -> Result<(), String> {
    if scan.boundary == Knowable::No {
        return Err("The account has no Triple-C permissions boundary yet".into());
    }
    let env = require_action(scan, name, EnvironmentAction::Join)?;
    if env.onboarding == Some(OnboardingState::InProgress) {
        return Err(format!(
            "Your onboarding to \"{name}\" is already in progress"
        ));
    }
    Ok(())
}

/// `AlreadyRunning` is told apart before it becomes an `AwsError`.
pub(crate) fn start_error(e: StartError, label: &str) -> AwsError {
    match e {
        StartError::AlreadyRunning => other(format!(
            "A deployment is already running for {label}; use Follow to watch it"
        )),
        StartError::Failed(e) => e,
    }
}

fn module_kind(d: Deployment) -> ModuleKind {
    match d {
        Deployment::Environment(_) => ModuleKind::Environment,
        Deployment::User { .. } => ModuleKind::UserOnboard,
        Deployment::Boundary => ModuleKind::PermissionsBoundary,
    }
}

const README: &str = "README.txt";
const BACKEND: &str = "backend.hcl";
const TFVARS: &str = "terraform.tfvars.json";

fn bundle_readme(d: Deployment, account: &str, region: &str, bucket: &str, key: &str) -> String {
    let what = match d {
        Deployment::Environment(env) => format!(
            "the Triple-C environment \"{env}\" (ECS cluster, security group, KMS key, log \
             group and network; the network is set in terraform.tfvars.json)"
        ),
        Deployment::User { env, .. } => format!(
            "one user's onboarding to the Triple-C environment \"{env}\" (their task and \
             execution roles and EFS file system; the user is the owner_id in \
             terraform.tfvars.json)"
        ),
        Deployment::Boundary => {
            "the account's Triple-C permissions boundary policy (triple-c-permissions-boundary)"
                .to_string()
        }
    };
    let location = if region == "us-east-1" {
        String::new()
    } else {
        format!(" --create-bucket-configuration LocationConstraint={region}")
    };
    format!(
        "Triple-C admin bundle
=====================

This folder deploys {what}
into AWS account {account}, region {region}.

OpenTofu state: s3://{bucket}/{key}
(S3-native locking: {key}.tflock is held while a run is in progress.)

Requirements
------------
- OpenTofu 1.10 or later, or Terraform 1.10 or later. Both are needed for
  S3-native state locking (use_lockfile in backend.hcl).
- Credentials for account {account} that carry the Triple-C environment-admin
  policies.
- .terraform.lock.hcl pins the AWS provider's hashes for OpenTofu
  (registry.opentofu.org). With Terraform, first run
      terraform providers lock -platform=linux_amd64 -platform=darwin_arm64 -platform=windows_amd64
  for the platforms you use (or delete .terraform.lock.hcl and let
  terraform init write a new one).

1. The state bucket
-------------------
The bucket {bucket} must exist first. Either deploy anything from
Triple-C once with this account and region (the app creates and configures
it), or create it by hand (POSIX shell syntax):

    aws s3api create-bucket --bucket {bucket} --region {region}{location}
    aws s3api put-bucket-versioning --bucket {bucket} --region {region} --expected-bucket-owner {account} --versioning-configuration Status=Enabled
    aws s3api put-bucket-encryption --bucket {bucket} --region {region} --expected-bucket-owner {account} --server-side-encryption-configuration '{{\"Rules\":[{{\"ApplyServerSideEncryptionByDefault\":{{\"SSEAlgorithm\":\"AES256\"}}}}]}}'
    aws s3api put-public-access-block --bucket {bucket} --region {region} --expected-bucket-owner {account} --public-access-block-configuration BlockPublicAcls=true,IgnorePublicAcls=true,BlockPublicPolicy=true,RestrictPublicBuckets=true
    aws s3api put-bucket-tagging --bucket {bucket} --region {region} --expected-bucket-owner {account} --tagging 'TagSet=[{{Key=triple-c:component,Value=state}}]'

2. Deploy
---------
From this folder:

    tofu init -backend-config=backend.hcl
    tofu apply

(With Terraform: terraform init -backend-config=backend.hcl, then
terraform apply.)

3. Remove
---------
From this folder, after tofu init:

    tofu destroy

This is irreversible. Remove every user's onboarding to an environment
before destroying the environment itself.
"
    )
}

/// The files a bundle for `d` consists of, as (name, contents).
fn bundle_files(
    d: Deployment,
    account: &str,
    region: &str,
    vars: &Value,
) -> Result<Vec<(String, String)>, String> {
    let region = validate_supported_region(region)?;
    let bucket = state_bucket_name(account, &region);
    let key = state_key(d);
    let mut files: Vec<(String, String)> = module_files(module_kind(d))
        .iter()
        .map(|(n, t)| (n.to_string(), t.to_string()))
        .collect();
    files.push((BACKEND.into(), backend_hcl(&bucket, &key, &region)));
    let json = serde_json::to_string_pretty(vars)
        .map_err(|e| format!("Could not encode the module variables: {e}"))?;
    files.push((TFVARS.into(), json + "\n"));
    files.push((README.into(), bundle_readme(d, account, &region, &bucket, &key)));
    Ok(files)
}

/// Write the D5 bundle for `d` into `dir`: the module, `backend.hcl`,
/// `terraform.tfvars.json` and `README.txt`. Refuses a folder holding
/// anything the bundle wouldn't write itself (an unrelated file, or a
/// folder such as `.terraform`), so nothing of the user's is overwritten.
/// Returns the names written.
pub(crate) fn write_bundle(
    dir: &Path,
    d: Deployment,
    account: &str,
    region: &str,
    vars: &Value,
) -> Result<Vec<String>, String> {
    let files = bundle_files(d, account, region, vars)?;
    let ours: BTreeSet<&str> = files.iter().map(|(n, _)| n.as_str()).collect();
    let io = |e: std::io::Error| format!("Could not write the bundle: {e}");
    let mut foreign = Vec::new();
    for entry in std::fs::read_dir(dir).map_err(io)? {
        let entry = entry.map_err(io)?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let is_file = entry.file_type().map_err(io)?.is_file();
        if !(is_file && ours.contains(name.as_str())) {
            foreign.push(name);
        }
    }
    if !foreign.is_empty() {
        foreign.sort();
        return Err(format!(
            "The folder already holds files that aren't part of this bundle ({}); choose an \
             empty folder",
            foreign.join(", ")
        ));
    }
    for (name, text) in &files {
        std::fs::write(dir.join(name), text).map_err(io)?;
    }
    Ok(files.into_iter().map(|(n, _)| n).collect())
}

/// One [`PROGRESS_EVENT`] payload.
#[derive(Serialize, Clone, Debug)]
pub struct DeployProgress {
    pub op_id: String,
    pub state_key: String,
    pub events: Vec<TofuEvent>,
    pub done: bool,
    /// Cancel can reach the runner (its PID is known, and not Windows).
    pub cancellable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<TofuOutcome>,
}

// ---------------------------------------------------------------------------
// Plumbing

/// Work folders being followed by this process, with their op id.
static FOLLOWERS: OnceLock<Mutex<HashMap<PathBuf, String>>> = OnceLock::new();

fn followers() -> std::sync::MutexGuard<'static, HashMap<PathBuf, String>> {
    FOLLOWERS
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|p| p.into_inner())
}

/// `<data dir>/triple-c/aws/`: OpenTofu, its plugin cache and the work
/// folders.
fn work_root() -> Result<PathBuf, AwsError> {
    let root = dirs::data_dir()
        .ok_or_else(|| other("Could not find the app data folder"))?
        .join("triple-c")
        .join("aws");
    std::fs::create_dir_all(&root)
        .map_err(|e| other(format!("Could not create {}: {e}", root.display())))?;
    Ok(root)
}

/// A connection, its SDK config and caller in one validated region.
struct Ctx {
    conn: HostConnection,
    cfg: SdkConfig,
    who: CallerIdentity,
    region: String,
    root: PathBuf,
}

impl Ctx {
    fn bucket(&self) -> String {
        state_bucket_name(&self.who.account, &self.region)
    }

    fn workdir(&self, d: Deployment) -> Result<Workdir, AwsError> {
        tofu::workdir_for(&self.root, d, &self.bucket())
    }
}

async fn context(
    state: &State<'_, AppState>,
    connection_id: &str,
    region: &str,
) -> Result<Ctx, AwsError> {
    let conn = super::host_commands::aws_conn(connection_id, &state.settings_store.get())
        .map_err(other)?;
    let region = validate_supported_region(region).map_err(other)?;
    let cfg = sdk_config(&conn, &region).await?;
    let who = caller_identity(&cfg).await?;
    Ok(Ctx {
        conn,
        cfg,
        who,
        region,
        root: work_root()?,
    })
}

async fn detect(ctx: &Ctx) -> Result<EnvironmentScan, AwsError> {
    let api = SdkDetectApi::new(&ctx.cfg, &ctx.who.account);
    let (root, bucket) = (ctx.root.clone(), ctx.bucket());
    let running = move |d: Deployment| {
        tofu::workdir_for(&root, d, &bucket)
            .map(|wd| tofu::is_running(&wd))
            .unwrap_or(false)
    };
    scan(&api, &ctx.who, &ctx.region, &running).await
}

/// The environment's VPC from its state's outputs (an `existing` network's
/// VPC isn't in the record).
async fn recorded_vpc_id(ctx: &Ctx, env: &str) -> Result<Option<String>, AwsError> {
    let s3 = aws_sdk_s3::Client::new(&ctx.cfg);
    let obj = s3
        .get_object()
        .bucket(ctx.bucket())
        .key(state_key(Deployment::Environment(env)))
        .expected_bucket_owner(&ctx.who.account)
        .send()
        .await
        .map_err(|e| crate::aws::classify_sdk_error("s3:GetObject", &e))?;
    if obj
        .content_length()
        .is_some_and(|n| n < 0 || n as usize > MAX_STATE_BYTES)
    {
        return Err(other("The environment's state is implausibly large"));
    }
    let bytes = obj
        .body
        .collect()
        .await
        .map_err(|e| other(format!("Could not read the environment's state: {e}")))?
        .into_bytes();
    Ok(vpc_id_from_state(&bytes))
}

/// The network to re-apply an environment with: the recorded one.
async fn recorded_network(ctx: &Ctx, info: &EnvironmentInfo) -> Result<NetworkChoice, AwsError> {
    let vpc = if info.network_mode == "existing" {
        recorded_vpc_id(ctx, &info.name).await?
    } else {
        None
    };
    network_from_record(info, vpc.as_deref()).map_err(other)
}

fn cancellable(wd: &Workdir) -> bool {
    cfg!(unix) && tofu::runner_pid(wd).is_some()
}

fn emit_progress(app: &AppHandle, p: &DeployProgress) {
    if let Err(e) = app.emit(PROGRESS_EVENT, p) {
        log::warn!("Could not emit deploy progress: {e}");
    }
}

/// Follow `wd` in a task of its own, unless this process already does; the
/// op id of whichever task follows it.
fn spawn_follower(app: AppHandle, wd: Workdir, key: String, can_cancel: bool) -> String {
    let op_id = {
        let mut map = followers();
        if let Some(existing) = map.get(&wd.dir) {
            return existing.clone();
        }
        let op_id = uuid::Uuid::new_v4().to_string();
        map.insert(wd.dir.clone(), op_id.clone());
        op_id
    };
    let id = op_id.clone();
    tauri::async_runtime::spawn(async move {
        let base = DeployProgress {
            op_id: id,
            state_key: key,
            events: Vec::new(),
            done: false,
            cancellable: can_cancel,
            result: None,
        };
        let outcome = tofu::follow(&wd, FOLLOW_POLL, |events| {
            for chunk in events.chunks(MAX_EVENTS_PER_BATCH) {
                emit_progress(
                    &app,
                    &DeployProgress {
                        events: chunk.to_vec(),
                        ..base.clone()
                    },
                );
            }
        })
        .await
        .unwrap_or_else(|e| TofuOutcome {
            ok: false,
            exit_code: -1,
            summary: Some(e.to_string()),
        });
        followers().remove(&wd.dir);
        emit_progress(
            &app,
            &DeployProgress {
                done: true,
                cancellable: false,
                result: Some(outcome),
                ..base
            },
        );
    });
    op_id
}

/// Ensure OpenTofu and the state bucket, write the work folder and start a
/// detached `init` + `op`; returns the op id its progress is emitted under.
async fn launch(
    app: AppHandle,
    ctx: &Ctx,
    key: &DeploymentKey,
    vars: &Value,
    op: TofuOp,
) -> Result<String, AwsError> {
    let d = key.deployment();
    let label = key.label();
    let wd = ctx.workdir(d)?;
    if tofu::is_running(&wd) {
        return Err(start_error(StartError::AlreadyRunning, &label));
    }
    if followers().contains_key(&wd.dir) {
        return Err(other(format!(
            "The previous run for {label} is still finishing; try again in a moment"
        )));
    }
    let tofu_bin = tofu::ensure_tofu(&ctx.root).await?;
    // StateBucketTaken is passed on as it is.
    let bucket = ensure_state_bucket(&ctx.cfg, &ctx.who.account, &ctx.region).await?;
    let wd = tofu::materialize(&ctx.root, module_kind(d), d, &bucket, &ctx.region, vars)?;

    let HostKind::Aws { auth, .. } = &ctx.conn.kind;
    let keys = match auth {
        AwsAuth::Keys { keyring_ref } => Some(
            secure::get_aws_keys(keyring_ref)
                .map_err(other)?
                .ok_or(AwsError::KeysMissing)?,
        ),
        AwsAuth::Profile { .. } => None,
    };
    let env = tofu::child_env(&ctx.conn, keys.as_ref(), &ctx.root, std::env::vars())?;
    drop(keys);

    let start_wd = wd.clone();
    let started = tokio::task::spawn_blocking(move || {
        tofu::start_detached(&tofu_bin, &start_wd, op, &env)
    })
    .await
    .map_err(|e| other(format!("The deploy could not be started: {e}")))?
    .map_err(|e| start_error(e, &label))?;
    let can_cancel = cfg!(unix) && started.pid.is_some();
    Ok(spawn_follower(app, wd, state_key(d), can_cancel))
}

// ---------------------------------------------------------------------------
// Commands

/// What the account holds in `region`: environments, the boundary, the
/// state bucket and the caller's onboarding.
#[tauri::command]
pub async fn detect_aws_environments(
    connection_id: String,
    region: String,
    state: State<'_, AppState>,
) -> Result<EnvironmentScan, AwsError> {
    let ctx = context(&state, &connection_id, &region).await?;
    detect(&ctx).await
}

/// Deploy a new environment. Refused for a name that has a record or
/// state, and when the permissions boundary is missing.
#[tauri::command]
pub async fn deploy_aws_environment(
    connection_id: String,
    region: String,
    name: String,
    network: NetworkChoice,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<String, AwsError> {
    let ctx = context(&state, &connection_id, &region).await?;
    let found = detect(&ctx).await?;
    deploy_refusal(&found, &name).map_err(other)?;
    let vars = environment_vars(&name, &ctx.region, &network).map_err(other)?;
    let key = DeploymentKey::Environment(name);
    launch(app, &ctx, &key, &vars, TofuOp::Apply).await
}

/// Re-apply an environment at the app's module version, with the network
/// read back from its record (an update never changes the network).
#[tauri::command]
pub async fn update_aws_environment(
    connection_id: String,
    region: String,
    name: String,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<String, AwsError> {
    let ctx = context(&state, &connection_id, &region).await?;
    let found = detect(&ctx).await?;
    let env = require_action(&found, &name, EnvironmentAction::Update).map_err(other)?;
    let info = env
        .info
        .as_ref()
        .ok_or_else(|| other("The environment has no readable record"))?;
    let network = recorded_network(&ctx, info).await?;
    let vars = environment_vars(&env.name, &ctx.region, &network).map_err(other)?;
    let key = DeploymentKey::Environment(env.name.clone());
    launch(app, &ctx, &key, &vars, TofuOp::Apply).await
}

/// Re-apply an Incomplete environment. With a valid record its network is
/// the recorded one (`network` is ignored); without one, `network` is
/// required.
#[tauri::command]
pub async fn repair_aws_environment(
    connection_id: String,
    region: String,
    name: String,
    network: Option<NetworkChoice>,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<String, AwsError> {
    let ctx = context(&state, &connection_id, &region).await?;
    let found = detect(&ctx).await?;
    let env = require_action(&found, &name, EnvironmentAction::Repair).map_err(other)?;
    let network = match (&env.info, network) {
        (Some(info), _) => recorded_network(&ctx, info).await?,
        (None, Some(n)) => n,
        (None, None) => {
            return Err(other(
                "The environment has no record to take its network from; choose one",
            ))
        }
    };
    let vars = environment_vars(&env.name, &ctx.region, &network).map_err(other)?;
    let key = DeploymentKey::Environment(env.name.clone());
    launch(app, &ctx, &key, &vars, TofuOp::Apply).await
}

/// Destroy an incomplete deployment: only when detection offers Destroy,
/// i.e. there is no valid record (D6). A working environment is destroyed
/// by an admin from a bundle.
#[tauri::command]
pub async fn destroy_incomplete_aws_environment(
    connection_id: String,
    region: String,
    name: String,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<String, AwsError> {
    let ctx = context(&state, &connection_id, &region).await?;
    let found = detect(&ctx).await?;
    let env = require_action(&found, &name, EnvironmentAction::Destroy).map_err(other)?;
    if env.info.is_some() {
        return Err(other("A working environment can't be destroyed from the app"));
    }
    // A destroy plans the removal of everything in state whatever the
    // network variables say; they only have to be valid.
    let vars = environment_vars(&env.name, &ctx.region, &NetworkChoice::Public).map_err(other)?;
    let key = DeploymentKey::Environment(env.name.clone());
    launch(app, &ctx, &key, &vars, TofuOp::Destroy).await
}

/// Delete an environment's stale `.tflock`. Only an environment's lock,
/// only when detection offers Unlock, the lock is stale and no runner on
/// this machine holds the deployment.
#[tauri::command]
pub async fn unlock_aws_state(
    connection_id: String,
    region: String,
    state_key: String,
    state: State<'_, AppState>,
) -> Result<(), AwsError> {
    let key = parse_state_key(&state_key)?;
    let DeploymentKey::Environment(name) = &key else {
        return Err(other("Only an environment's state lock can be removed from the app"));
    };
    let ctx = context(&state, &connection_id, &region).await?;
    let found = detect(&ctx).await?;
    unlock_refusal(&found, name).map_err(other)?;
    if tofu::is_running(&ctx.workdir(key.deployment())?) {
        return Err(other("A deploy on this machine holds the lock"));
    }
    let lock_key = format!("{state_key}.tflock");
    aws_sdk_s3::Client::new(&ctx.cfg)
        .delete_object()
        .bucket(ctx.bucket())
        .key(&lock_key)
        .expected_bucket_owner(&ctx.who.account)
        .send()
        .await
        .map_err(|e| crate::aws::classify_sdk_error("s3:DeleteObject", &e))?;
    log::info!("Removed the stale state lock {lock_key}");
    Ok(())
}

/// Reattach to a run on this machine (in progress, or finished with its
/// log still there): replays its log as progress. Returns the op id; a run
/// already followed keeps its follower and op id.
#[tauri::command]
pub async fn follow_aws_deployment(
    connection_id: String,
    region: String,
    state_key: String,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<String, AwsError> {
    let key = parse_state_key(&state_key)?;
    let ctx = context(&state, &connection_id, &region).await?;
    let wd = ctx.workdir(key.deployment())?;
    if !tofu::has_run(&wd) {
        return Err(other(format!(
            "There is no deploy of {} on this machine to follow",
            key.label()
        )));
    }
    let can_cancel = tofu::is_running(&wd) && cancellable(&wd);
    Ok(spawn_follower(app, wd, state_key, can_cancel))
}

/// Cancel: interrupt the runner, which stops OpenTofu gracefully.
#[tauri::command]
pub async fn cancel_aws_deployment(
    connection_id: String,
    region: String,
    state_key: String,
    state: State<'_, AppState>,
) -> Result<(), AwsError> {
    let key = parse_state_key(&state_key)?;
    let ctx = context(&state, &connection_id, &region).await?;
    tofu::interrupt(&ctx.workdir(key.deployment())?)
}

/// Onboard the caller to `environment`, as the owner their own
/// `aws:userid` names.
#[tauri::command]
pub async fn onboard_aws_user(
    connection_id: String,
    region: String,
    environment: String,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<String, AwsError> {
    let env = validate_environment_name(&environment).map_err(other)?;
    let ctx = context(&state, &connection_id, &region).await?;
    let owner = validate_owner_id(&ctx.who.user_id).map_err(other)?;
    let found = detect(&ctx).await?;
    onboard_refusal(&found, &env).map_err(other)?;
    let vars = onboarding_vars(&env, &ctx.region, &owner);
    let key = DeploymentKey::User {
        owner_key: owner_key(&owner),
        env,
    };
    launch(app, &ctx, &key, &vars, TofuOp::Apply).await
}

/// Deploy (or re-apply) the account's permissions boundary.
#[tauri::command]
pub async fn deploy_aws_permissions_boundary(
    connection_id: String,
    region: String,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<String, AwsError> {
    let ctx = context(&state, &connection_id, &region).await?;
    let vars = boundary_vars(&ctx.region);
    launch(app, &ctx, &DeploymentKey::Boundary, &vars, TofuOp::Apply).await
}

/// Ask for a folder from Rust (see `file_commands::pick_save_path`).
async fn pick_folder(window: &tauri::Window) -> Option<PathBuf> {
    use tauri_plugin_dialog::DialogExt;
    let (tx, rx) = tokio::sync::oneshot::channel();
    window
        .dialog()
        .file()
        .set_parent(window)
        .set_title("Save the admin bundle into a folder")
        .pick_folder(move |picked| {
            let _ = tx.send(picked);
        });
    rx.await.ok().flatten().and_then(|p| p.into_path().ok())
}

/// Write an admin bundle (D5) for `kind` into a folder the user picks:
/// `environment` names the environment (environment and onboarding),
/// `owner_id` the user to onboard (an `aws:userid`), `network` an
/// environment's network (public when omitted). `None` when the dialog was
/// dismissed; otherwise the folder written.
#[allow(clippy::too_many_arguments)]
#[tauri::command]
pub async fn save_aws_admin_bundle(
    connection_id: String,
    region: String,
    kind: ModuleKind,
    environment: Option<String>,
    owner_id: Option<String>,
    network: Option<NetworkChoice>,
    window: tauri::Window,
    state: State<'_, AppState>,
) -> Result<Option<String>, AwsError> {
    let need_env = || {
        environment
            .as_deref()
            .ok_or_else(|| other("An environment name is required"))
            .and_then(|e| validate_environment_name(e).map_err(other))
    };
    // Validate everything before any AWS call or dialog.
    let region_ok = validate_supported_region(&region).map_err(other)?;
    let (key, vars) = match kind {
        ModuleKind::Environment => {
            let env = need_env()?;
            let vars = environment_vars(&env, &region_ok, &network.unwrap_or(NetworkChoice::Public))
                .map_err(other)?;
            (DeploymentKey::Environment(env), vars)
        }
        ModuleKind::UserOnboard => {
            let env = need_env()?;
            let owner = owner_id
                .as_deref()
                .map(str::trim)
                .ok_or_else(|| other("The user's owner id (aws:userid) is required"))
                .and_then(|o| validate_owner_id(o).map_err(other))?;
            let vars = onboarding_vars(&env, &region_ok, &owner);
            (
                DeploymentKey::User {
                    owner_key: owner_key(&owner),
                    env,
                },
                vars,
            )
        }
        ModuleKind::PermissionsBoundary => (DeploymentKey::Boundary, boundary_vars(&region_ok)),
    };
    let ctx = context(&state, &connection_id, &region).await?;
    let Some(dir) = pick_folder(&window).await else {
        return Ok(None);
    };
    write_bundle(&dir, key.deployment(), &ctx.who.account, &ctx.region, &vars).map_err(other)?;
    Ok(Some(dir.to_string_lossy().into_owned()))
}

#[cfg(test)]
mod tests;
