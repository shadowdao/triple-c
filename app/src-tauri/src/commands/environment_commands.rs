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
//! here that follows its log from byte 0 and emits [`PROGRESS_EVENT`]
//! batches, then a final `done` with the result. The task emits nothing
//! until the command is about to return its op id ([`Gate`]), so a listener
//! registered before the invoke sees everything. Per work folder this
//! process keeps one [`Slot`]: a launch in progress (which refuses a second
//! launch or a follow) or the one follower (a re-follow replaces it).
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
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tauri::{AppHandle, Emitter, State};

use aws_sdk_s3::error::ProvideErrorMetadata;
use tokio::sync::oneshot;

use crate::aws::environments::{
    cluster_holder, ec2_name_holders, ensure_state_bucket, scan, state_bucket_policy,
    user_record_from, DetectedEnvironment, EnvironmentAction, EnvironmentInfo, EnvironmentScan,
    EnvironmentState, Knowable, LocalRun, NameHolder, OnboardApi, OnboardingState, SdkDetectApi,
    SdkOnboardApi, UserCompute, UserRecord, LIVE_LOCK_SECS,
};
use crate::aws::tofu::{
    self, backend_hcl, StartError, TofuEvent, TofuOp, TofuOutcome, Workdir,
};
use crate::aws::tofu_modules::{
    boundary_vars, environment_vars, module_files, onboarding_vars, owner_key, state_bucket_name,
    state_key, user_ec2_stem, user_record_parameter_name, validate_ec2_user_tag,
    validate_environment_name, Deployment, Ec2Compute, ModuleKind, NetworkChoice,
};
use crate::models::project::{ec2_instance_types, Ec2InstanceType, DEFAULT_EC2_INSTANCE_TYPE};
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
const MAX_STATE_BYTES: usize = 1024 * 1024;

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
            // The same checks as the apply's variables. Only the network is
            // checked here, so the lifetime cap is a valid placeholder (0).
            environment_vars(&info.name, "us-east-1", &choice, 0)?;
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
                EnvironmentAction::PushState => "Push saved state",
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

/// Unlock the caller's own onboarding state: only their own owner key, and
/// only a stale lock no local runner holds (detection offers Unlock on the
/// onboarding).
pub(crate) fn onboarding_unlock_refusal(
    scan: &EnvironmentScan,
    name: &str,
    owner_key: &str,
) -> Result<(), String> {
    if owner_key != scan.owner_key {
        return Err("Only your own onboarding's state lock can be removed from the app".into());
    }
    let env = find(scan, name).ok_or_else(|| format!("No environment named \"{name}\" was found"))?;
    match &env.onboarding {
        Some(OnboardingState::Incomplete { lock: Some(l), .. })
            if !l.local_process && env.onboarding_actions.contains(&EnvironmentAction::Unlock) =>
        {
            Ok(())
        }
        _ => Err(format!(
            "Your onboarding to \"{name}\" has no stale state lock to remove"
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
    if matches!(env.onboarding, Some(OnboardingState::InProgress { .. })) {
        return Err(format!(
            "Your onboarding to \"{name}\" is already in progress"
        ));
    }
    Ok(())
}

/// The EC2 capacity an onboarding asks for: none (Fargate only), or EC2
/// with these values. A command given no choice keeps what the user's
/// record holds ([`resolve_ec2`]).
#[derive(Deserialize, Serialize, Clone, Debug, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Ec2Choice {
    None,
    Ec2(Ec2Compute),
}

/// What an onboarding apply uses: the explicit choice, or with none given
/// the recorded capacity (a re-apply keeps it unless changed). With none
/// given and a record that exists but can't be used, it refuses rather than
/// apply without EC2, which would tear the capacity down.
pub(crate) fn resolve_ec2(
    choice: Option<Ec2Choice>,
    record: &UserRecord,
) -> Result<Option<Ec2Compute>, String> {
    match (choice, record) {
        (Some(Ec2Choice::None), _) => Ok(None),
        (Some(Ec2Choice::Ec2(c)), _) => Ok(Some(c)),
        (None, UserRecord::Absent) => Ok(None),
        (None, UserRecord::Valid(r)) => Ok(Some(Ec2Compute {
            instance_type: r.instance_type.clone(),
            max_instances: r.max_instances,
            user_tag: r.user_tag.clone(),
        })),
        (None, UserRecord::Unusable(reason)) => Err(format!(
            "Your EC2 record can't be used ({reason}), so Triple-C can't keep your EC2 capacity \
             as it is; choose the EC2 capacity explicitly"
        )),
    }
}

/// R3 (amended): an apply with EC2 capacity would adopt (or fail on) a
/// per-user cluster or Auto Scaling group named `stem` that already exists.
/// Each one found must be the caller's: its `triple-c:owner` tag (the
/// module's default tags) is `owner_id`, or, failing that, the caller's
/// record names the cluster. A half-built capacity of the caller's own (a
/// failed apply that wrote no record yet) is therefore re-applied; one
/// owned by someone else, or untagged, is refused.
pub(crate) fn ec2_collision_refusal(
    stem: &str,
    holders: &[NameHolder],
    owner_id: &str,
    recorded: Option<&UserCompute>,
) -> Result<(), String> {
    let suffix = format!(":cluster/{stem}");
    let recorded_here = recorded.is_some_and(|r| r.cluster_arn.ends_with(&suffix));
    for h in holders {
        if h.owner.as_deref() == Some(owner_id) || recorded_here {
            continue;
        }
        let whose = if h.owner.is_some() {
            "belongs to another onboarding"
        } else {
            "exists but isn't tagged as any Triple-C user's"
        };
        return Err(format!(
            "An {} named {stem} already {whose}. The triple-c-user value is probably wrong: use \
             the value an admin set as your triple-c-user sign-in tag",
            h.kind
        ));
    }
    Ok(())
}

/// Everything a self-onboarding apply needs settled before it starts: the
/// caller's record, the capacity ([`resolve_ec2`]), the module variables
/// and, with EC2 capacity, the name check ([`ec2_collision_refusal`]).
/// Check-then-apply leaves a narrow race (another admin creating the same
/// names in between); only admins apply, and the module would then fail on
/// create rather than adopt.
pub(crate) async fn prepare_onboarding<A: OnboardApi>(
    api: &A,
    env: &str,
    region: &str,
    account: &str,
    owner_id: &str,
    choice: Option<Ec2Choice>,
) -> Result<Value, AwsError> {
    let okey = owner_key(region, owner_id);
    let read = api.parameter(&user_record_parameter_name(env, &okey)).await?;
    let record = user_record_from(read, env, account, region);
    let compute = resolve_ec2(choice, &record).map_err(other)?;
    let vars = onboarding_vars(env, region, owner_id, compute.as_ref()).map_err(other)?;
    if let Some(c) = &compute {
        // The caller's own triple-c-user tag can't be read (session tags
        // aren't visible to GetCallerIdentity); a wrong value fails later
        // as access denied.
        let stem = user_ec2_stem(env, &c.user_tag);
        let holders = ec2_name_holders(api, &stem).await?;
        ec2_collision_refusal(&stem, &holders, owner_id, record.valid()).map_err(other)?;
    }
    Ok(vars)
}

/// The bundle's version of the check, with what a workspace user can read
/// (the cluster, via `ecs:Describe*`, and the record; not the Auto Scaling
/// group): a warning to show, never a refusal. The admin runs the full
/// check from the README.
pub(crate) async fn bundle_ec2_warning<A: OnboardApi>(
    api: &A,
    env: &str,
    region: &str,
    account: &str,
    owner_id: &str,
    compute: &Ec2Compute,
) -> Option<String> {
    let stem = user_ec2_stem(env, &compute.user_tag);
    let checked = async {
        let holder = cluster_holder(api, &stem).await?;
        let read = api
            .parameter(&user_record_parameter_name(env, &owner_key(region, owner_id)))
            .await?;
        let record = user_record_from(read, env, account, region);
        Ok::<_, AwsError>(
            ec2_collision_refusal(&stem, &holder.into_iter().collect::<Vec<_>>(), owner_id, record.valid())
                .err(),
        )
    };
    match checked.await {
        Ok(warning) => warning,
        Err(_) => Some(format!(
            "Triple-C couldn't check whether the EC2 name {stem} is already in use; the README has \
             the admin check it"
        )),
    }
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

/// A held state lock on the environment, if detection saw one.
fn held_lock(env: &DetectedEnvironment) -> bool {
    match &env.state {
        EnvironmentState::Deploying { .. } => true,
        EnvironmentState::Incomplete { lock } => lock.is_some(),
        _ => false,
    }
}

/// Update and Repair: the action is offered and no lock is held. A re-apply
/// under a held lock would only fail inside the detached run.
pub(crate) fn reapply_refusal<'a>(
    scan: &'a EnvironmentScan,
    name: &str,
    action: EnvironmentAction,
) -> Result<&'a DetectedEnvironment, String> {
    let env = require_action(scan, name, action)?;
    if held_lock(env) {
        return Err(format!(
            "Environment \"{name}\"'s state is locked by an interrupted run; Unlock it first"
        ));
    }
    Ok(env)
}

/// Destroy from the app (D6, R12): offered by detection, no valid record
/// (a working environment is an admin's `tofu destroy`), and no held lock.
pub(crate) fn destroy_refusal<'a>(
    scan: &'a EnvironmentScan,
    name: &str,
) -> Result<&'a DetectedEnvironment, String> {
    let env = require_action(scan, name, EnvironmentAction::Destroy)?;
    // Each onboarded user's stack holds the environment's security group (a
    // mount-target group references it) and reads its record: destroying
    // the environment under them fails part-way and strands their stacks.
    match env.onboarded_users {
        Some(0) => {}
        Some(n) => {
            return Err(format!(
                "Environment \"{name}\" still has {n} onboarded user{} (state under \
                 users/{name}/); remove the onboarded users' stacks first (tofu destroy from each \
                 user's onboarding bundle)",
                if n == 1 { "" } else { "s" }
            ))
        }
        None => {
            return Err(format!(
                "The state bucket can't list users onboarded to \"{name}\", so it can't be \
                 destroyed from the app"
            ))
        }
    }
    if env.info.is_some() {
        return Err(format!(
            "Environment \"{name}\" has a valid record, so it may be working; it can't be \
             destroyed from the app (an admin destroys it from a bundle)"
        ));
    }
    if held_lock(env) {
        return Err(format!(
            "Environment \"{name}\"'s state is locked by an interrupted run; Unlock it first"
        ));
    }
    Ok(env)
}

/// The lock object about to be deleted must still be stale, by its own
/// `LastModified` read just now.
pub(crate) fn stale_lock_refusal(age_secs: u64) -> Result<(), String> {
    if age_secs < LIVE_LOCK_SECS {
        Err("The state lock is recent, so a deploy may be running; detect again".into())
    } else {
        Ok(())
    }
}

/// S3's answer to a conditional delete whose ETag no longer matches.
pub(crate) fn is_precondition_failed(code: Option<&str>, status: Option<u16>) -> bool {
    code == Some("PreconditionFailed") || status == Some(412)
}

/// The boundary policy is account-wide; its state is per region. A boundary
/// that exists without state in this region's bucket was deployed from
/// another region or by hand, and an apply here would fail on the name.
/// `has_state` is `None` when the caller can't tell.
pub(crate) fn boundary_refusal(boundary: &Knowable, has_state: Option<bool>) -> Result<(), String> {
    if *boundary == Knowable::Yes && has_state == Some(false) {
        Err("The permissions boundary already exists in this account (deployed from another \
             region or by hand), so it can't be deployed from here"
            .into())
    } else {
        Ok(())
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

/// The onboarding bundle's EC2 step: before applying with EC2 capacity, the
/// admin checks that the per-user names are free or already this user's
/// (the module can't: a name taken by someone else's onboarding would be
/// adopted, and a collision only fails at create). Ownership is the
/// resources' `triple-c:owner` tag, or failing that the user's record, as
/// in [`ec2_collision_refusal`]. `user_tag` is the bundle's `ec2_user_tag`
/// when set and valid; otherwise the commands show `<tag>` for the admin to
/// fill in.
fn ec2_collision_step(
    env: &str,
    owner_key: &str,
    owner_id: &str,
    region: &str,
    user_tag: Option<&str>,
) -> String {
    let stem = user_ec2_stem(env, user_tag.unwrap_or("<tag>"));
    let record = user_record_parameter_name(env, owner_key);
    format!(
        "EC2 capacity (only if ec2_instance_type is set)
------------------------------------------------
ec2_user_tag must be the value of the user's triple-c-user principal tag.
BEFORE tofu apply, check that the per-user EC2 names are not already in use
by another onboarding. Run:

    aws ecs describe-clusters --region {region} --clusters {stem} --include TAGS --query 'clusters[?status!=`INACTIVE`].[clusterName,tags[?key==`triple-c:owner`].value|[0]]' --output text
    aws autoscaling describe-auto-scaling-groups --region {region} --auto-scaling-group-names {stem} --query 'AutoScalingGroups[].[AutoScalingGroupName,Tags[?Key==`triple-c:owner`].Value|[0]]' --output text
    aws ssm get-parameter --region {region} --name {record} --query Parameter.Value --output text

If either of the first two prints {stem}, apply ONLY if the owner printed
next to it is this user's owner_id ({owner_id}) or the third (this user's
record) succeeds and its cluster_arn ends in cluster/{stem}; otherwise the
name belongs to someone else's onboarding (or to nobody's): stop and
choose the correct ec2_user_tag. (The app runs the same check before
applying; run the check right before the apply, since nothing holds the
name in between.)

"
    )
}

fn bundle_readme(
    d: Deployment,
    account: &str,
    region: &str,
    bucket: &str,
    key: &str,
    vars: &Value,
) -> String {
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
    // The admin policy allows s3:CreateBucket only with the component tag in
    // the request (aws:RequestTag), so it goes on the create; a later
    // put-bucket-tagging can't satisfy that. us-east-1 refuses a
    // LocationConstraint.
    let tags = r#""Tags":[{"Key":"triple-c:component","Value":"state"}]"#;
    let create_config = if region == "us-east-1" {
        format!("{{{tags}}}")
    } else {
        format!(r#"{{"LocationConstraint":"{region}",{tags}}}"#)
    };
    let policy = state_bucket_policy(bucket);
    let ec2_step = match d {
        Deployment::User { env, owner_key } => {
            let tag = vars["ec2_user_tag"]
                .as_str()
                .filter(|t| validate_ec2_user_tag(t).is_ok());
            let owner_id = vars["owner_id"].as_str().unwrap_or("<owner_id>");
            ec2_collision_step(env, owner_key, owner_id, region, tag)
        }
        _ => String::new(),
    };
    let credentials = match d {
        Deployment::Boundary => format!(
            "- Credentials of an account administrator for account {account} WITHOUT
  the Triple-C environment-admin policies attached: those policies deny
  changing or deleting the boundary policy to anyone holding them, account
  administrators included. (This bundle needs iam:CreatePolicy and, to
  remove it, iam:DeletePolicy on triple-c-permissions-boundary.)"
        ),
        _ => format!(
            "- Credentials for account {account} that carry the Triple-C environment-admin
  policies."
        ),
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
{credentials}
- .terraform.lock.hcl pins the AWS provider's hashes for OpenTofu
  (registry.opentofu.org). With Terraform, first run
      terraform providers lock -platform=linux_amd64 -platform=darwin_arm64 -platform=windows_amd64
  for the platforms you use (or delete .terraform.lock.hcl and let
  terraform init write a new one).

1. The state bucket
-------------------
The bucket {bucket} must exist first. Either deploy anything from
Triple-C once with this account and region (the app creates and configures
it), or create it by hand exactly as the app does (POSIX shell syntax; an
AWS CLI v2 recent enough that create-bucket accepts Tags, e.g. 2.39):

    aws s3api create-bucket --bucket {bucket} --region {region} --create-bucket-configuration '{create_config}'
    aws s3api put-bucket-versioning --bucket {bucket} --region {region} --expected-bucket-owner {account} --versioning-configuration Status=Enabled
    aws s3api put-bucket-encryption --bucket {bucket} --region {region} --expected-bucket-owner {account} --server-side-encryption-configuration '{{\"Rules\":[{{\"ApplyServerSideEncryptionByDefault\":{{\"SSEAlgorithm\":\"AES256\"}}}}]}}'
    aws s3api put-public-access-block --bucket {bucket} --region {region} --expected-bucket-owner {account} --public-access-block-configuration BlockPublicAcls=true,IgnorePublicAcls=true,BlockPublicPolicy=true,RestrictPublicBuckets=true
    aws s3api put-bucket-policy --bucket {bucket} --region {region} --expected-bucket-owner {account} --policy '{policy}'

{ec2_step}2. Deploy
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
    files.push((README.into(), bundle_readme(d, account, &region, &bucket, &key, vars)));
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

/// Holds a follower's first emit back until the command that started it is
/// about to return its op id, so no event (a finished run's whole replay
/// and its `done` included) can arrive before the caller knows the op id.
/// Opening it, or dropping it, releases the follower.
pub(crate) struct Gate(oneshot::Sender<()>);

impl Gate {
    pub(crate) fn new() -> (Gate, oneshot::Receiver<()>) {
        let (tx, rx) = oneshot::channel();
        (Gate(tx), rx)
    }

    /// Call immediately before the command's final `Ok(op_id)`.
    pub(crate) fn open(self) {
        let _ = self.0.send(());
    }
}

/// A follower: wait for the gate, follow the log from byte 0 emitting
/// batches of at most [`MAX_EVENTS_PER_BATCH`], then emit `done` with the
/// result (a follow error is a failed result).
pub(crate) async fn follow_and_report(
    gate: oneshot::Receiver<()>,
    wd: Workdir,
    base: DeployProgress,
    poll: Duration,
    mut emit: impl FnMut(&DeployProgress),
) {
    // A dropped gate means the command returned without opening it.
    let _ = gate.await;
    let outcome = tofu::follow(&wd, poll, |events| {
        for chunk in events.chunks(MAX_EVENTS_PER_BATCH) {
            emit(&DeployProgress {
                events: chunk.to_vec(),
                ..base.clone()
            });
        }
    })
    .await
    .unwrap_or_else(|e| TofuOutcome {
        ok: false,
        exit_code: -1,
        summary: Some(e.to_string()),
    });
    emit(&DeployProgress {
        done: true,
        cancellable: false,
        result: Some(outcome),
        ..base
    });
}

// ---------------------------------------------------------------------------
// Plumbing

/// What this process is doing with a work folder. `T` is the follower
/// task's handle (generic so the registry is unit-testable).
pub(crate) enum Slot<T> {
    /// A launch is between its checks and its start; nothing else may
    /// materialize, start or follow the folder.
    Launching,
    /// One task follows the folder's run under `op_id`.
    Following { op_id: String, task: T },
}

/// Why a folder's slot refused.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum SlotBusy {
    Launching,
    Following,
}

impl SlotBusy {
    fn error(&self, label: &str) -> AwsError {
        other(match self {
            SlotBusy::Launching => format!("A deployment is starting for {label}; wait for it"),
            SlotBusy::Following => format!(
                "The run for {label} is still being followed; wait for it to finish"
            ),
        })
    }
}

/// Per work folder: a launch in progress or the one follower. Every
/// check-and-change happens in one method call, i.e. under one acquisition
/// of the registry's lock.
pub(crate) struct Slots<T>(HashMap<PathBuf, Slot<T>>);

impl<T> Default for Slots<T> {
    fn default() -> Self {
        Slots(HashMap::new())
    }
}

impl<T> Slots<T> {
    /// Claim `dir` for a launch: refused if anything holds it.
    pub(crate) fn begin_launch(&mut self, dir: &Path) -> Result<(), SlotBusy> {
        match self.0.get(dir) {
            Some(Slot::Launching) => Err(SlotBusy::Launching),
            Some(Slot::Following { .. }) => Err(SlotBusy::Following),
            None => {
                self.0.insert(dir.to_path_buf(), Slot::Launching);
                Ok(())
            }
        }
    }

    /// A launch is over without a follower: clear `Launching`, never a
    /// follower's slot.
    pub(crate) fn end_launch(&mut self, dir: &Path) {
        if matches!(self.0.get(dir), Some(Slot::Launching)) {
            self.0.remove(dir);
        }
    }

    /// Install a follower made by `spawn`. From a launch (`from_launch`, the
    /// caller holds `Launching`) it replaces the launch's slot; from a
    /// follow it is refused while a launch holds the folder, and otherwise
    /// replaces any earlier follower, whose handle is returned to abort.
    /// `spawn` runs only once the check has passed, under the same lock.
    pub(crate) fn install_follower(
        &mut self,
        dir: &Path,
        op_id: &str,
        from_launch: bool,
        spawn: impl FnOnce() -> T,
    ) -> Result<Option<T>, SlotBusy> {
        if !from_launch && matches!(self.0.get(dir), Some(Slot::Launching)) {
            return Err(SlotBusy::Launching);
        }
        let task = spawn();
        let old = self.0.insert(
            dir.to_path_buf(),
            Slot::Following {
                op_id: op_id.to_string(),
                task,
            },
        );
        Ok(match old {
            Some(Slot::Following { task, .. }) => Some(task),
            _ => None,
        })
    }

    /// A follower ended: remove the slot only if it is still that one's.
    pub(crate) fn finish_follower(&mut self, dir: &Path, op_id: &str) {
        if matches!(self.0.get(dir), Some(Slot::Following { op_id: id, .. }) if id == op_id) {
            self.0.remove(dir);
        }
    }

    #[cfg(test)]
    pub(crate) fn is_free(&self, dir: &Path) -> bool {
        !self.0.contains_key(dir)
    }
}

type FollowerTask = tauri::async_runtime::JoinHandle<()>;

static SLOTS: OnceLock<Mutex<Slots<FollowerTask>>> = OnceLock::new();

fn slots() -> std::sync::MutexGuard<'static, Slots<FollowerTask>> {
    SLOTS
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|p| p.into_inner())
}

/// The folder's `Launching` slot, released on every exit path unless a
/// follower has replaced it.
struct LaunchGuard(PathBuf);

impl LaunchGuard {
    fn take(dir: &Path, label: &str) -> Result<LaunchGuard, AwsError> {
        slots().begin_launch(dir).map_err(|b| b.error(label))?;
        Ok(LaunchGuard(dir.to_path_buf()))
    }
}

impl Drop for LaunchGuard {
    fn drop(&mut self) {
        slots().end_launch(&self.0);
    }
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
            .map(|wd| LocalRun {
                running: tofu::is_running(&wd),
                failed: tofu::last_run_failed(&wd),
                errored_state: tofu::has_errored_state(&wd),
            })
            .unwrap_or_default()
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
    let too_large = || other("The environment's state is implausibly large");
    if obj
        .content_length()
        .is_some_and(|n| n < 0 || n as usize > MAX_STATE_BYTES)
    {
        return Err(too_large());
    }
    // Capped while reading too: the length header may be absent.
    let mut body = obj.body;
    let mut bytes = Vec::new();
    while let Some(chunk) = body
        .try_next()
        .await
        .map_err(|e| other(format!("Could not read the environment's state: {e}")))?
    {
        if bytes.len() + chunk.len() > MAX_STATE_BYTES {
            return Err(too_large());
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(vpc_id_from_state(&bytes))
}

/// Whether this region's bucket holds the boundary's state; `None` when
/// the caller may not look.
async fn boundary_state_exists(ctx: &Ctx) -> Result<Option<bool>, AwsError> {
    let res = aws_sdk_s3::Client::new(&ctx.cfg)
        .head_object()
        .bucket(ctx.bucket())
        .key(state_key(Deployment::Boundary))
        .expected_bucket_owner(&ctx.who.account)
        .send()
        .await;
    match res {
        Ok(_) => Ok(Some(true)),
        Err(e) => match e.raw_response().map(|r| r.status().as_u16()) {
            Some(404) => Ok(Some(false)),
            Some(403) => Ok(None),
            _ => Err(crate::aws::classify_sdk_error("s3:GetObject (HeadObject)", &e)),
        },
    }
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

/// Only the main window shows deploys; no other window gets the events.
fn emit_progress(app: &AppHandle, p: &DeployProgress) {
    if let Err(e) = app.emit_to("main", PROGRESS_EVENT, p) {
        log::warn!("Could not emit deploy progress: {e}");
    }
}

/// Start the folder's follower under a new op id, replacing (aborting) any
/// follower this process already has for it, so a re-follow (a reloaded
/// window) replays the whole log. The follower emits nothing until the
/// returned [`Gate`] is opened. A follow (`from_launch == false`) is refused
/// while a launch holds the folder; the check, the spawn and the slot change
/// happen under one lock acquisition, so no launch can slip in between.
fn install_follower(
    app: AppHandle,
    wd: Workdir,
    key: String,
    label: &str,
    can_cancel: bool,
    from_launch: bool,
) -> Result<(String, Gate), AwsError> {
    let op_id = uuid::Uuid::new_v4().to_string();
    let (gate, rx) = Gate::new();
    let base = DeployProgress {
        op_id: op_id.clone(),
        state_key: key,
        events: Vec::new(),
        done: false,
        cancellable: can_cancel,
        result: None,
    };
    let dir = wd.dir.clone();
    let id = op_id.clone();
    // The task's own clean-up takes the same lock, so it can't run before
    // its slot is in place.
    let old = slots()
        .install_follower(&dir, &op_id, from_launch, move || {
            tauri::async_runtime::spawn(async move {
                let dir = wd.dir.clone();
                follow_and_report(rx, wd, base, FOLLOW_POLL, |p| emit_progress(&app, p)).await;
                slots().finish_follower(&dir, &id);
            })
        })
        .map_err(|b| b.error(label))?;
    if let Some(old) = old {
        old.abort();
    }
    Ok((op_id, gate))
}

/// The runner's whole environment ([`tofu::child_env`]): a keys
/// connection's keys come from the keychain and go nowhere else.
fn runner_env(ctx: &Ctx) -> Result<Vec<(String, String)>, AwsError> {
    let HostKind::Aws { auth, .. } = &ctx.conn.kind;
    let keys = match auth {
        AwsAuth::Keys { keyring_ref } => Some(
            secure::get_aws_keys(keyring_ref)
                .map_err(other)?
                .ok_or(AwsError::KeysMissing)?,
        ),
        AwsAuth::Profile { .. } => None,
    };
    tofu::child_env(&ctx.conn, keys.as_ref(), &ctx.root, std::env::vars())
}

/// Ensure OpenTofu and the state bucket, write the work folder and start a
/// detached `init` + `op`. Returns the op id its progress is emitted under
/// and the gate the command opens just before returning it.
async fn launch(
    app: AppHandle,
    ctx: &Ctx,
    key: &DeploymentKey,
    vars: &Value,
    op: TofuOp,
) -> Result<(String, Gate), AwsError> {
    let d = key.deployment();
    let label = key.label();
    let wd = ctx.workdir(d)?;
    if tofu::is_running(&wd) {
        return Err(start_error(StartError::AlreadyRunning, &label));
    }
    // Held until a follower replaces it, or released on any error below.
    let _launching = LaunchGuard::take(&wd.dir, &label)?;
    let tofu_bin = tofu::ensure_tofu(&ctx.root).await?;
    // StateBucketTaken is passed on as it is.
    let bucket = ensure_state_bucket(&ctx.cfg, &ctx.who.account, &ctx.region).await?;
    let wd = tofu::materialize(&ctx.root, module_kind(d), d, &bucket, &ctx.region, vars)?;

    let env = runner_env(ctx)?;

    let start_wd = wd.clone();
    let started = tokio::task::spawn_blocking(move || {
        tofu::start_detached(&tofu_bin, &start_wd, op, &env)
    })
    .await
    .map_err(|e| other(format!("The deploy could not be started: {e}")))?
    .map_err(|e| start_error(e, &label))?;
    let can_cancel = cfg!(unix) && started.pid.is_some();
    install_follower(app, wd, state_key(d), &label, can_cancel, true)
}

/// Open a launch's gate and hand back its op id: the last thing a command
/// does, so its events follow the op id to the caller.
fn opened(launched: Result<(String, Gate), AwsError>) -> Result<String, AwsError> {
    let (op_id, gate) = launched?;
    gate.open();
    Ok(op_id)
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
    max_workspace_hours: Option<u32>,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<String, AwsError> {
    let ctx = context(&state, &connection_id, &region).await?;
    let found = detect(&ctx).await?;
    deploy_refusal(&found, &name).map_err(other)?;
    let vars = environment_vars(
        &name,
        &ctx.region,
        &network,
        max_workspace_hours.unwrap_or(0),
    )
    .map_err(other)?;
    let key = DeploymentKey::Environment(name);
    opened(launch(app, &ctx, &key, &vars, TofuOp::Apply).await)
}

/// Re-apply an environment at the app's module version, with the network
/// read back from its record (an update never changes the network).
#[tauri::command]
pub async fn update_aws_environment(
    connection_id: String,
    region: String,
    name: String,
    max_workspace_hours: Option<u32>,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<String, AwsError> {
    let ctx = context(&state, &connection_id, &region).await?;
    let found = detect(&ctx).await?;
    let env = reapply_refusal(&found, &name, EnvironmentAction::Update).map_err(other)?;
    let info = env
        .info
        .as_ref()
        .ok_or_else(|| other("The environment has no readable record"))?;
    let network = recorded_network(&ctx, info).await?;
    // The recorded cap stays unless the caller changes it.
    let hours = max_workspace_hours.unwrap_or(info.max_workspace_hours);
    let vars = environment_vars(&env.name, &ctx.region, &network, hours).map_err(other)?;
    let key = DeploymentKey::Environment(env.name.clone());
    opened(launch(app, &ctx, &key, &vars, TofuOp::Apply).await)
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
    max_workspace_hours: Option<u32>,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<String, AwsError> {
    let ctx = context(&state, &connection_id, &region).await?;
    let found = detect(&ctx).await?;
    let env = reapply_refusal(&found, &name, EnvironmentAction::Repair).map_err(other)?;
    let network = match (&env.info, network) {
        (Some(info), _) => recorded_network(&ctx, info).await?,
        (None, Some(n)) => n,
        (None, None) => {
            return Err(other(
                "The environment has no record to take its network from; choose one",
            ))
        }
    };
    // A valid record's cap stays unless the caller changes it.
    let hours = max_workspace_hours
        .or(env.info.as_ref().map(|i| i.max_workspace_hours))
        .unwrap_or(0);
    let vars = environment_vars(&env.name, &ctx.region, &network, hours).map_err(other)?;
    let key = DeploymentKey::Environment(env.name.clone());
    opened(launch(app, &ctx, &key, &vars, TofuOp::Apply).await)
}

/// Destroy an incomplete deployment: only when detection offers Destroy,
/// there is no valid record (D6) and no lock is held ([`destroy_refusal`]).
/// A working environment is destroyed by an admin from a bundle.
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
    let env = destroy_refusal(&found, &name).map_err(other)?;
    // A destroy plans the removal of everything in state whatever the
    // network variables say; they only have to be valid.
    let vars =
        environment_vars(&env.name, &ctx.region, &NetworkChoice::Public, 0).map_err(other)?;
    let key = DeploymentKey::Environment(env.name.clone());
    opened(launch(app, &ctx, &key, &vars, TofuOp::Destroy).await)
}

/// Delete a stale `.tflock`: an environment's, or the caller's own
/// onboarding's (`users/<env>/<their key>/…`), only when detection offers
/// Unlock on it and no runner on this machine holds the deployment. The delete is conditional on the ETag of the lock object
/// as just re-read (and re-checked stale), so a lock taken since is never
/// removed; there is no unconditional fallback.
#[tauri::command]
pub async fn unlock_aws_state(
    connection_id: String,
    region: String,
    state_key: String,
    state: State<'_, AppState>,
) -> Result<(), AwsError> {
    let key = parse_state_key(&state_key)?;
    if key == DeploymentKey::Boundary {
        return Err(other(
            "The permissions boundary's state lock can't be removed from the app",
        ));
    }
    let ctx = context(&state, &connection_id, &region).await?;
    let found = detect(&ctx).await?;
    match &key {
        DeploymentKey::Environment(name) => unlock_refusal(&found, name),
        DeploymentKey::User { env, owner_key } => onboarding_unlock_refusal(&found, env, owner_key),
        DeploymentKey::Boundary => unreachable!("refused above"),
    }
    .map_err(other)?;
    if tofu::is_running(&ctx.workdir(key.deployment())?) {
        return Err(other("A deploy on this machine holds the lock"));
    }
    let lock_key = format!("{state_key}.tflock");
    let s3 = aws_sdk_s3::Client::new(&ctx.cfg);
    let head = match s3
        .head_object()
        .bucket(ctx.bucket())
        .key(&lock_key)
        .expected_bucket_owner(&ctx.who.account)
        .send()
        .await
    {
        Ok(h) => h,
        Err(e) if e.raw_response().map(|r| r.status().as_u16()) == Some(404) => {
            return Err(other("The state lock is already gone; detect again"));
        }
        // HeadObject is authorised by s3:GetObject.
        Err(e) => return Err(crate::aws::classify_sdk_error("s3:GetObject (HeadObject)", &e)),
    };
    let etag = head
        .e_tag()
        .filter(|t| !t.is_empty())
        .ok_or_else(|| other("The state lock has no ETag, so it can't be removed safely"))?
        .to_string();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let age = head
        .last_modified()
        .map(|t| now.saturating_sub(t.secs()).max(0) as u64)
        .ok_or_else(|| other("The state lock's age is unknown, so it can't be removed safely"))?;
    stale_lock_refusal(age).map_err(other)?;
    delete_lock_if_unchanged(&s3, &ctx.bucket(), &ctx.who.account, &lock_key, &etag).await?;
    log::info!("Removed the stale state lock {lock_key}");
    Ok(())
}

/// The lock was replaced since it was read: its ETag no longer matches.
pub(crate) const LOCK_CHANGED: &str = "The state lock changed since it was checked; detect again";

/// Delete `lock_key` only if its ETag is still `etag` (S3 conditional
/// delete, `If-Match`); [`LOCK_CHANGED`] when it is not.
pub(crate) async fn delete_lock_if_unchanged(
    s3: &aws_sdk_s3::Client,
    bucket: &str,
    account: &str,
    lock_key: &str,
    etag: &str,
) -> Result<(), AwsError> {
    if let Err(e) = s3
        .delete_object()
        .bucket(bucket)
        .key(lock_key)
        .if_match(etag)
        .expected_bucket_owner(account)
        .send()
        .await
    {
        let status = e.raw_response().map(|r| r.status().as_u16());
        if is_precondition_failed(e.code(), status) {
            return Err(other(LOCK_CHANGED));
        }
        return Err(crate::aws::classify_sdk_error("s3:DeleteObject", &e));
    }
    Ok(())
}

/// Reattach to a run on this machine (in progress, or finished with its
/// log still there): replays its whole log as progress under a new op id.
/// A follower this process already had for it is replaced, so a reloaded
/// window gets the full history. Refused while a launch is starting there.
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
    // Refused inside install_follower, under the registry lock, while a
    // launch holds the folder.
    let (op_id, gate) = install_follower(app, wd, state_key, &key.label(), can_cancel, false)?;
    gate.open();
    Ok(op_id)
}

/// Push the state a failed run on this machine couldn't save
/// (`errored.tfstate`) back to the bucket: `tofu state push errored.tfstate`
/// through the same detached runner and environment as a deploy, followed
/// like one. Once pushed the file is set aside and the deployment can be
/// re-applied (until then [`tofu::materialize`] refuses).
#[tauri::command]
pub async fn push_aws_saved_state(
    connection_id: String,
    region: String,
    state_key: String,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<String, AwsError> {
    let key = parse_state_key(&state_key)?;
    let ctx = context(&state, &connection_id, &region).await?;
    opened(launch_push(app, &ctx, &key).await)
}

async fn launch_push(app: AppHandle, ctx: &Ctx, key: &DeploymentKey) -> Result<(String, Gate), AwsError> {
    let d = key.deployment();
    let label = key.label();
    let wd = ctx.workdir(d)?;
    if tofu::is_running(&wd) {
        return Err(start_error(StartError::AlreadyRunning, &label));
    }
    if !tofu::has_errored_state(&wd) {
        return Err(other(format!(
            "There is no saved state of {label} on this machine to push"
        )));
    }
    let _launching = LaunchGuard::take(&wd.dir, &label)?;
    let tofu_bin = tofu::ensure_tofu(&ctx.root).await?;
    let env = runner_env(ctx)?;
    let start_wd = wd.clone();
    let started = tokio::task::spawn_blocking(move || {
        tofu::start_detached(&tofu_bin, &start_wd, TofuOp::PushState, &env)
    })
    .await
    .map_err(|e| other(format!("The push could not be started: {e}")))?
    .map_err(|e| start_error(e, &label))?;
    let can_cancel = cfg!(unix) && started.pid.is_some();
    install_follower(app, wd, state_key(d), &label, can_cancel, true)
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
/// `aws:userid` names, with the EC2 capacity `ec2` asks for (omitted: keep
/// what their user record holds, none without one). With EC2 capacity the
/// per-user names must be free or already theirs (R3).
#[tauri::command]
pub async fn onboard_aws_user(
    connection_id: String,
    region: String,
    environment: String,
    ec2: Option<Ec2Choice>,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<String, AwsError> {
    let env = validate_environment_name(&environment).map_err(other)?;
    let ctx = context(&state, &connection_id, &region).await?;
    let owner = validate_owner_id(&ctx.who.user_id).map_err(other)?;
    let found = detect(&ctx).await?;
    onboard_refusal(&found, &env).map_err(other)?;
    let vars = prepare_onboarding(
        &SdkOnboardApi::new(&ctx.cfg),
        &env,
        &ctx.region,
        &ctx.who.account,
        &owner,
        ec2,
    )
    .await?;
    let key = DeploymentKey::User {
        owner_key: owner_key(&ctx.region, &owner),
        env,
    };
    opened(launch(app, &ctx, &key, &vars, TofuOp::Apply).await)
}

/// The EC2 instance types onboarding offers (the app's built-in table) and
/// the one it offers first.
#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct Ec2InstanceTable {
    pub default_type: &'static str,
    pub types: Vec<Ec2InstanceType>,
}

#[tauri::command]
pub fn list_ec2_instance_types() -> Ec2InstanceTable {
    Ec2InstanceTable {
        default_type: DEFAULT_EC2_INSTANCE_TYPE,
        types: ec2_instance_types(),
    }
}

/// Deploy (or re-apply) the account's permissions boundary. Refused when
/// the boundary exists but this region's bucket has no state for it.
#[tauri::command]
pub async fn deploy_aws_permissions_boundary(
    connection_id: String,
    region: String,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<String, AwsError> {
    let ctx = context(&state, &connection_id, &region).await?;
    let found = detect(&ctx).await?;
    let has_state = match found.state_bucket {
        Knowable::Yes => boundary_state_exists(&ctx).await?,
        Knowable::No => Some(false),
        Knowable::Unknown => None,
    };
    boundary_refusal(&found.boundary, has_state).map_err(other)?;
    let vars = boundary_vars(&ctx.region);
    opened(launch(app, &ctx, &DeploymentKey::Boundary, &vars, TofuOp::Apply).await)
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
/// environment's network (public when omitted), `ec2` an onboarding's EC2
/// capacity (none when omitted; the bundle's README has the admin check
/// its names first). `None` when the dialog was dismissed; otherwise the
/// folder written and any warning.
#[allow(clippy::too_many_arguments)]
#[tauri::command]
pub async fn save_aws_admin_bundle(
    connection_id: String,
    region: String,
    kind: ModuleKind,
    environment: Option<String>,
    owner_id: Option<String>,
    network: Option<NetworkChoice>,
    max_workspace_hours: Option<u32>,
    ec2: Option<Ec2Compute>,
    window: tauri::Window,
    state: State<'_, AppState>,
) -> Result<Option<SavedBundle>, AwsError> {
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
            let vars = environment_vars(
                &env,
                &region_ok,
                &network.unwrap_or(NetworkChoice::Public),
                max_workspace_hours.unwrap_or(0),
            )
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
            let vars = onboarding_vars(&env, &region_ok, &owner, ec2.as_ref()).map_err(other)?;
            (
                DeploymentKey::User {
                    owner_key: owner_key(&region_ok, &owner),
                    env,
                },
                vars,
            )
        }
        ModuleKind::PermissionsBoundary => (DeploymentKey::Boundary, boundary_vars(&region_ok)),
    };
    let ctx = context(&state, &connection_id, &region).await?;
    let warning = match (&key, &ec2, &owner_id) {
        (DeploymentKey::User { env, .. }, Some(c), Some(owner)) => {
            let api = SdkOnboardApi::new(&ctx.cfg);
            bundle_ec2_warning(&api, env, &ctx.region, &ctx.who.account, owner.trim(), c).await
        }
        _ => None,
    };
    let Some(dir) = pick_folder(&window).await else {
        return Ok(None);
    };
    write_bundle(&dir, key.deployment(), &ctx.who.account, &ctx.region, &vars).map_err(other)?;
    Ok(Some(SavedBundle {
        folder: dir.to_string_lossy().into_owned(),
        warning,
    }))
}

/// A bundle written: its folder and, for an onboarding with EC2 capacity,
/// a warning when its EC2 name looks taken ([`bundle_ec2_warning`]).
#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct SavedBundle {
    pub folder: String,
    pub warning: Option<String>,
}

#[cfg(test)]
mod tests;
