//! The state bucket, and what the account holds: environments (by their SSM
//! record and their state object), interrupted or running deployments (by
//! the S3-native `.tflock` objects) and the caller's own onboarding.
//!
//! Everything read back from AWS here is untrusted: an environment record or
//! a lock object is written by whoever can write the parameter or the
//! bucket. Records are validated field by field (reasons name the field,
//! never echo the value), lock contents are capped and sanitized, and only
//! key shapes the app itself builds (`tofu_modules::state_key`) are listed.
//!
//! Workspace users can read only the records and `iam:GetRole` (D8): a
//! denied lookup is `Unknown` (or no objects at all), never "missing".

use std::collections::BTreeMap;
use std::future::Future;

use aws_config::SdkConfig;
use aws_sdk_s3::config::http::HttpResponse;
use aws_sdk_s3::error::{ProvideErrorMetadata, SdkError};
use aws_sdk_s3::types::{
    BucketLocationConstraint, BucketVersioningStatus, CreateBucketConfiguration,
    PublicAccessBlockConfiguration, ServerSideEncryption, ServerSideEncryptionByDefault,
    ServerSideEncryptionConfiguration, ServerSideEncryptionRule, Tag, Tagging,
    VersioningConfiguration,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use super::error::{classify_sdk_error, AwsError};
use super::identity::CallerIdentity;
use super::tofu_modules::{
    environment_parameter_name, owner_key, state_bucket_name, state_key, user_role_names,
    validate_environment_name, Deployment, BOUNDARY_POLICY_NAME,
};
use crate::models::host_connection::validate_supported_region;

/// An environment record (the module's SSM parameter) or a lock object
/// larger than this is refused unread.
const MAX_UNTRUSTED_BYTES: usize = 4096;
/// A lock string shown to the user is at most this many characters.
const MAX_LOCK_STRING_CHARS: usize = 128;
/// A lock younger than this is a deploy in progress somewhere else; an older
/// one is an apply that died holding it.
pub const LIVE_LOCK_SECS: u64 = 2 * 3600;
const RECORD_SCHEMA: u64 = 1;
/// Bounds on what one scan lists, so a bucket or parameter path full of
/// junk can't make it unbounded.
const MAX_RECORDS: usize = 500;
const MAX_OBJECTS_PER_PREFIX: usize = 5000;
const MAX_LOCKS: usize = 200;
/// A state object larger than this is not read to see whether it is empty.
/// What `tofu destroy` leaves is about 180 bytes plus about 110 per
/// checkable object (OpenTofu 1.13 keeps `check_results` for each resource
/// or data source with a pre/postcondition): 453 bytes for two, measured.
/// The modules have at most a handful, so 2 KiB leaves a wide margin.
const EMPTY_STATE_MAX_BYTES: i64 = 2048;
/// The caller's onboarding state is read whole (for its outputs) only up to
/// this size; one user's module state is a few tens of KiB.
const MAX_STATE_DOCUMENT_BYTES: usize = 1024 * 1024;
/// At most this many small state objects are read per scan; the rest count
/// as deployments.
const MAX_STATE_PEEKS: usize = 200;

/// An environment's record, schema 1 (`aws/terraform/environment/main.tf`).
#[derive(Deserialize, Serialize, Clone, Debug, PartialEq)]
pub struct EnvironmentInfo {
    pub schema: u32,
    pub name: String,
    pub template_version: u32,
    pub cluster_arn: String,
    pub security_group_id: String,
    pub subnet_ids: Vec<String>,
    pub assign_public_ip: bool,
    pub kms_key_arn: String,
    pub log_group: String,
    pub network_mode: String,
    pub tenancy: String,
}

fn field<'a>(obj: &'a Map<String, Value>, name: &str) -> Result<&'a Value, String> {
    obj.get(name).ok_or_else(|| format!("{name} is missing"))
}

fn str_field<'a>(obj: &'a Map<String, Value>, name: &str) -> Result<&'a str, String> {
    field(obj, name)?
        .as_str()
        .ok_or_else(|| format!("{name} is not a string"))
}

/// `<prefix>` followed by 1 to 32 lowercase hex characters.
fn is_hex_id(s: &str, prefix: &str) -> bool {
    s.strip_prefix(prefix).is_some_and(|hex| {
        !hex.is_empty()
            && hex.len() <= 32
            && hex.bytes().all(|c| matches!(c, b'0'..=b'9' | b'a'..=b'f'))
    })
}

/// Validate an environment record read from the parameter for `path_name`.
/// Every field is checked against what the module writes, and every ARN
/// must be in `account` and `region`. The error names the field, never its
/// value.
pub fn parse_environment_record(
    path_name: &str,
    value: &str,
    account: &str,
    region: &str,
) -> Result<EnvironmentInfo, String> {
    let name = validate_environment_name(path_name)
        .map_err(|_| "the parameter's path is not a valid environment name".to_string())?;
    if value.len() > MAX_UNTRUSTED_BYTES {
        return Err("the record is larger than 4 KiB".into());
    }
    let parsed: Value =
        serde_json::from_str(value).map_err(|_| "the record is not JSON".to_string())?;
    let obj = parsed
        .as_object()
        .ok_or_else(|| "the record is not a JSON object".to_string())?;

    if field(obj, "schema")?.as_u64() != Some(RECORD_SCHEMA) {
        return Err("schema is not 1".into());
    }
    if str_field(obj, "name")? != name {
        return Err("name does not match the parameter's path".into());
    }
    let template_version = field(obj, "template_version")?
        .as_u64()
        .and_then(|v| u32::try_from(v).ok())
        .filter(|v| *v >= 1)
        .ok_or_else(|| "template_version is not a version number".to_string())?;

    let cluster_arn = str_field(obj, "cluster_arn")?;
    if cluster_arn
        != format!(
            "arn:aws:ecs:{region}:{account}:cluster/{}",
            super::tofu_modules::environment_cluster_name(&name)
        )
    {
        return Err(
            "cluster_arn is not this environment's cluster in this account and region".into(),
        );
    }
    let security_group_id = str_field(obj, "security_group_id")?;
    if !is_hex_id(security_group_id, "sg-") {
        return Err("security_group_id is not a security group id".into());
    }
    let subnet_ids: Vec<String> = field(obj, "subnet_ids")?
        .as_array()
        .filter(|a| a.len() == 2)
        .and_then(|a| {
            a.iter()
                .map(|s| {
                    s.as_str()
                        .filter(|s| is_hex_id(s, "subnet-"))
                        .map(String::from)
                })
                .collect::<Option<Vec<_>>>()
        })
        .ok_or_else(|| "subnet_ids is not exactly 2 subnet ids".to_string())?;
    if subnet_ids[0] == subnet_ids[1] {
        return Err("subnet_ids must be two different subnets".into());
    }
    let assign_public_ip = field(obj, "assign_public_ip")?
        .as_bool()
        .ok_or_else(|| "assign_public_ip is not true or false".to_string())?;
    let kms_key_arn = str_field(obj, "kms_key_arn")?;
    let key_prefix = format!("arn:aws:kms:{region}:{account}:key/");
    let key_ok = kms_key_arn.strip_prefix(&key_prefix).is_some_and(|id| {
        !id.is_empty()
            && id.len() <= 64
            && id
                .bytes()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
    });
    if !key_ok {
        return Err("kms_key_arn is not a KMS key in this account and region".into());
    }
    let log_group = str_field(obj, "log_group")?;
    if log_group != super::tofu_modules::environment_log_group(&name) {
        return Err("log_group is not this environment's log group".into());
    }
    let network_mode = str_field(obj, "network_mode")?;
    if !matches!(network_mode, "public" | "private-nat" | "existing") {
        return Err("network_mode is not a known network mode".into());
    }
    if str_field(obj, "tenancy")? != "strict" {
        return Err("tenancy is not strict".into());
    }

    Ok(EnvironmentInfo {
        schema: RECORD_SCHEMA as u32,
        name: name.clone(),
        template_version,
        cluster_arn: cluster_arn.to_string(),
        security_group_id: security_group_id.to_string(),
        subnet_ids,
        assign_public_ip,
        kms_key_arn: kms_key_arn.to_string(),
        log_group: log_group.to_string(),
        network_mode: network_mode.to_string(),
        tenancy: "strict".to_string(),
    })
}

/// A held state lock, as shown to the user.
#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct LockInfo {
    pub who: String,
    pub created: String,
    pub operation: String,
    /// From the lock object's `LastModified`, not its (untrusted) `Created`.
    pub age_secs: u64,
    /// This machine's runner is applying the deployment.
    pub local_process: bool,
}

/// Control characters and the invisible/bidi formatting characters that
/// could disguise text are dropped; the rest is cut to
/// [`MAX_LOCK_STRING_CHARS`].
fn sanitize_lock_string(s: &str) -> String {
    s.chars()
        .filter(|c| {
            !c.is_control()
                && !matches!(
                    *c,
                    '\u{200b}'..='\u{200f}'
                        | '\u{202a}'..='\u{202e}'
                        | '\u{2060}'..='\u{2064}'
                        | '\u{2066}'..='\u{2069}'
                        | '\u{feff}'
                )
        })
        .take(MAX_LOCK_STRING_CHARS)
        .collect()
}

/// A lock from its object's contents (S3-native locking's JSON: `ID`,
/// `Operation`, `Info`, `Who`, `Version`, `Created`, `Path`). Contents that
/// are too large or not JSON still mean a held lock, with nothing to show.
pub fn lock_info(json: &str, age_secs: u64, local_process: bool) -> LockInfo {
    let parsed = (json.len() <= MAX_UNTRUSTED_BYTES)
        .then(|| serde_json::from_str::<Value>(json).ok())
        .flatten();
    let get = |k: &str| {
        parsed
            .as_ref()
            .and_then(|v| v.get(k))
            .and_then(Value::as_str)
            .map(sanitize_lock_string)
            .unwrap_or_default()
    };
    LockInfo {
        who: get("Who"),
        created: get("Created"),
        operation: get("Operation"),
        age_secs,
        local_process,
    }
}

#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum EnvironmentState {
    /// Record and state, at the app's module version.
    Ready,
    /// The lock is held by a local runner or recently by someone else.
    Deploying { lock: Option<LockInfo> },
    /// Record and state at an older module version.
    UpdateAvailable,
    /// State without a record (a failed apply), or a stale lock.
    Incomplete { lock: Option<LockInfo> },
    /// A record whose state this caller can't see (another bucket, an admin
    /// bundle, or no bucket access).
    ExternallyManaged,
    /// The record failed validation.
    UnreadableRecord { reason: String },
}

#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum EnvironmentAction {
    Join,
    Update,
    Repair,
    Destroy,
    Follow,
    Unlock,
    /// Push the state a failed run on this machine couldn't save
    /// (`errored.tfstate`) back to the bucket.
    PushState,
}

/// The caller's onboarding to one environment.
#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum OnboardingState {
    /// Their task role exists and, where the bucket is readable, their state
    /// records a finished apply (its `onboarding_complete` output).
    Joined,
    NotJoined,
    /// A local runner, or a recent lock.
    InProgress { lock: Option<LockInfo> },
    /// The last onboarding run didn't finish: a stale lock, a failed run on
    /// this machine, or a state without the completion output. Join is a
    /// re-apply and finishes it.
    Incomplete { lock: Option<LockInfo> },
}

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct DetectedEnvironment {
    pub name: String,
    pub template_version: Option<u32>,
    pub state: EnvironmentState,
    pub actions: Vec<EnvironmentAction>,
    pub info: Option<EnvironmentInfo>,
    /// The caller's onboarding; resolved only for joinable environments.
    pub onboarding: Option<OnboardingState>,
    /// What the caller can do with their own onboarding's run (`Follow`,
    /// `Unlock`, `PushState`); Join stays in `actions`.
    pub onboarding_actions: Vec<EnvironmentAction>,
    /// How many users' onboarding states (`users/<env>/…`, non-empty) the
    /// bucket holds; `None` when the caller can't list it.
    pub onboarded_users: Option<u32>,
}

#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Knowable {
    Yes,
    No,
    /// Denied: this caller can't tell.
    Unknown,
}

#[derive(Serialize, Clone, Debug)]
pub struct EnvironmentScan {
    pub region: String,
    pub account: String,
    /// `aws:userid`.
    pub owner_id: String,
    pub owner_key: String,
    pub state_bucket: Knowable,
    pub boundary: Knowable,
    pub environments: Vec<DetectedEnvironment>,
}

/// The state bucket's listing under `environments/`, `users/` and
/// `boundary/`.
#[derive(Debug, Default)]
pub struct StateObjects {
    /// State object keys.
    pub states: Vec<String>,
    /// (`<state key>.tflock`, its contents, age in seconds from its
    /// `LastModified`).
    pub locks: Vec<(String, String, u64)>,
}

impl StateObjects {
    fn has_state(&self, key: &str) -> bool {
        self.states.iter().any(|k| k == key)
    }

    /// The lock on `state_key`, as (contents, age).
    fn lock(&self, state_key: &str) -> Option<(&str, u64)> {
        self.locks
            .iter()
            .find(|(k, _, _)| k.strip_suffix(".tflock") == Some(state_key))
            .map(|(_, json, age)| (json.as_str(), *age))
    }
}

/// The environment name of an `environments/<env>/terraform.tfstate[.tflock]`
/// key, if it is one the app could have written.
fn environment_of_key(key: &str) -> Option<&str> {
    let key = key.strip_suffix(".tflock").unwrap_or(key);
    let env = key
        .strip_prefix("environments/")?
        .strip_suffix("/terraform.tfstate")?;
    (validate_environment_name(env).is_ok()).then_some(env)
}

/// The environment name of a `users/<env>/<key>/terraform.tfstate` key.
fn user_environment_of_key(key: &str) -> Option<&str> {
    let mut parts = key.strip_prefix("users/")?.split('/');
    let (env, _, file, rest) = (parts.next()?, parts.next()?, parts.next()?, parts.next());
    (file == "terraform.tfstate" && rest.is_none() && validate_environment_name(env).is_ok())
        .then_some(env)
}

/// What this machine's work folder says of a deployment's runs.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct LocalRun {
    /// A runner here is applying it (`tofu::is_running`).
    pub running: bool,
    /// The last run here finished with an error (`tofu::last_run_failed`).
    pub failed: bool,
    /// OpenTofu left `errored.tfstate` here (`tofu::has_errored_state`).
    pub errored_state: bool,
}

/// This machine's runs, per deployment.
pub type LocalRuns<'a> = &'a (dyn Fn(Deployment) -> LocalRun + Sync);

/// One environment from its record (if any), the bucket listing (if this
/// caller can read it) and whether a local runner is applying it.
fn detect_one(
    name: &str,
    record: Option<Result<EnvironmentInfo, String>>,
    objects: Option<&StateObjects>,
    local_runs: LocalRuns,
    current_version: u32,
) -> DetectedEnvironment {
    use EnvironmentAction::*;
    let key = state_key(Deployment::Environment(name));
    let run = local_runs(Deployment::Environment(name));
    let local = run.running;
    let lock = objects
        .and_then(|o| o.lock(&key))
        .map(|(json, age)| lock_info(json, age, local));
    let has_state = objects.is_some_and(|o| o.has_state(&key));
    let (info, reason) = match record {
        Some(Ok(info)) => (Some(info), None),
        Some(Err(reason)) => (None, Some(reason)),
        None => (None, None),
    };
    let template_version = info.as_ref().map(|i| i.template_version);

    let (state, actions) = if local {
        (EnvironmentState::Deploying { lock }, vec![Follow])
    } else if lock.as_ref().is_some_and(|l| l.age_secs < LIVE_LOCK_SECS) {
        (EnvironmentState::Deploying { lock }, vec![])
    } else if lock.is_some() {
        // Stale, and not ours (a local runner was handled above). A valid
        // record with our state is a working environment whose last run
        // (an Update, a plan) died: never Destroy it from the app (D6).
        let working = info.is_some() && has_state;
        let actions = if working {
            vec![Repair, Unlock]
        } else {
            vec![Repair, Destroy, Unlock]
        };
        (EnvironmentState::Incomplete { lock }, actions)
    } else if let Some(reason) = reason {
        // A re-apply rewrites the record, so Repair is offered when the
        // state is ours. Never Destroy: the environment may be working (D6).
        let actions = if has_state { vec![Repair] } else { vec![] };
        (EnvironmentState::UnreadableRecord { reason }, actions)
    } else if let Some(info) = &info {
        if !has_state {
            (EnvironmentState::ExternallyManaged, vec![Join])
        } else if info.template_version < current_version {
            (EnvironmentState::UpdateAvailable, vec![Join, Update])
        } else {
            (EnvironmentState::Ready, vec![Join])
        }
    } else {
        // Only reachable with state and no record: a failed apply.
        (
            EnvironmentState::Incomplete { lock: None },
            vec![Repair, Destroy],
        )
    };

    // A saved state must go back before anything else runs here: nothing
    // that would re-apply or destroy is offered until it has.
    let actions = if run.errored_state && !local {
        let mut kept: Vec<_> = actions
            .into_iter()
            .filter(|a| !matches!(a, Update | Repair | Destroy))
            .collect();
        kept.insert(0, PushState);
        kept
    } else {
        actions
    };
    let onboarded_users = objects.map(|o| {
        o.states
            .iter()
            .filter(|k| user_environment_of_key(k) == Some(name))
            .count() as u32
    });

    DetectedEnvironment {
        name: name.to_string(),
        template_version,
        state,
        actions,
        info,
        onboarding: None,
        onboarding_actions: Vec::new(),
        onboarded_users,
    }
}

/// Combine the records and the bucket listing into one list of
/// environments, sorted by name. `objects` is `None` when the caller can't
/// read the bucket (or there is none). Onboarding is left unresolved
/// ([`scan`] does that).
pub fn merge(
    records: Vec<(String, Result<EnvironmentInfo, String>)>,
    objects: Option<&StateObjects>,
    local_runs: LocalRuns,
    current_version: u32,
) -> Vec<DetectedEnvironment> {
    let mut by_name: BTreeMap<String, Option<Result<EnvironmentInfo, String>>> = BTreeMap::new();
    for (name, record) in records {
        by_name.insert(name, Some(record));
    }
    if let Some(o) = objects {
        let keys = o.states.iter().chain(o.locks.iter().map(|(k, _, _)| k));
        for env in keys.filter_map(|k| environment_of_key(k)) {
            by_name.entry(env.to_string()).or_insert(None);
        }
    }
    by_name
        .into_iter()
        .map(|(name, record)| detect_one(&name, record, objects, local_runs, current_version))
        .collect()
}

/// Whether an onboarding state records a finished apply: the module's
/// `onboarding_complete` output, which depends on every resource it makes,
/// so a run that failed part-way never writes it.
pub(crate) fn onboarding_complete(state: &str) -> bool {
    serde_json::from_str::<Value>(state)
        .ok()
        .and_then(|v| v.pointer("/outputs/onboarding_complete/value").cloned())
        == Some(Value::Bool(true))
}

/// The caller's onboarding as far as the lock, this machine's runs and the
/// bucket listing tell, and what they may do with its run. `None` (with
/// the actions) when the state must be read or the role looked up.
fn onboarding_from_runs(
    objects: Option<&StateObjects>,
    deployment: Deployment,
    run: LocalRun,
) -> (Option<OnboardingState>, Vec<EnvironmentAction>) {
    use EnvironmentAction::*;
    let key = state_key(deployment);
    let lock = objects
        .and_then(|o| o.lock(&key))
        .map(|(json, age)| lock_info(json, age, run.running));
    let mut actions = Vec::new();
    if run.running {
        actions.push(Follow);
    } else if run.errored_state {
        actions.push(PushState);
    }
    let state = if run.running || lock.as_ref().is_some_and(|l| l.age_secs < LIVE_LOCK_SECS) {
        Some(OnboardingState::InProgress { lock })
    } else if lock.is_some() {
        actions.push(Unlock);
        Some(OnboardingState::Incomplete { lock })
    } else if run.failed || run.errored_state {
        Some(OnboardingState::Incomplete { lock: None })
    } else {
        None
    };
    (state, actions)
}

/// What detection asks AWS. Each method returns a `Send` future so [`scan`]
/// is `Send` (a Tauri command needs that) for any implementation; an impl
/// may still write them as `async fn`. Access denied is an answer
/// (`Unknown` / `None`), not an error.
pub trait DetectApi: Send + Sync {
    /// (parameter name, value) under `/triple-c/environments/`.
    fn environment_records(
        &self,
    ) -> impl Future<Output = Result<Vec<(String, String)>, AwsError>> + Send;
    /// `None` = the bucket is missing or this caller can't list it.
    fn state_objects(
        &self,
        bucket: &str,
    ) -> impl Future<Output = Result<Option<StateObjects>, AwsError>> + Send;
    fn bucket_exists(
        &self,
        bucket: &str,
    ) -> impl Future<Output = Result<Knowable, AwsError>> + Send;
    fn boundary_exists(
        &self,
        account: &str,
    ) -> impl Future<Output = Result<Knowable, AwsError>> + Send;
    fn role_exists(&self, name: &str) -> impl Future<Output = Result<Knowable, AwsError>> + Send;
    /// A state object's contents, capped; `None` when it is missing, can't
    /// be read or is too large.
    fn state_document(
        &self,
        bucket: &str,
        key: &str,
    ) -> impl Future<Output = Result<Option<String>, AwsError>> + Send;
}

/// Everything the account holds for this caller in `region`.
pub async fn scan<D: DetectApi>(
    d: &D,
    who: &CallerIdentity,
    region: &str,
    local_runs: LocalRuns<'_>,
) -> Result<EnvironmentScan, AwsError> {
    let bucket = state_bucket_name(&who.account, region);
    scan_in_bucket(d, who, region, &bucket, local_runs).await
}

/// [`scan`] against a given state bucket instead of the account's: the live
/// tests use a throwaway bucket so they never touch a real one.
pub(crate) async fn scan_in_bucket<D: DetectApi>(
    d: &D,
    who: &CallerIdentity,
    region: &str,
    bucket: &str,
    local_runs: LocalRuns<'_>,
) -> Result<EnvironmentScan, AwsError> {
    let prefix = environment_parameter_name("");
    let records = d
        .environment_records()
        .await?
        .into_iter()
        .filter_map(|(param, value)| {
            let name = param.strip_prefix(&prefix)?;
            if validate_environment_name(name).is_err() {
                log::warn!("Ignoring an environment record under an invalid name");
                return None;
            }
            let parsed = parse_environment_record(name, &value, &who.account, region);
            Some((name.to_string(), parsed))
        })
        .collect();

    let state_bucket = d.bucket_exists(bucket).await?;
    let objects = match state_bucket {
        Knowable::Yes => d.state_objects(bucket).await?,
        _ => None,
    };
    let boundary = d.boundary_exists(&who.account).await?;

    let mut environments = merge(
        records,
        objects.as_ref(),
        local_runs,
        super::tofu_modules::ENVIRONMENT_MODULE_VERSION,
    );
    let key = owner_key(region, &who.user_id);
    for env in &mut environments {
        if !env.actions.contains(&EnvironmentAction::Join) {
            continue;
        }
        let deployment = Deployment::User {
            env: &env.name,
            owner_key: &key,
        };
        let (from_runs, actions) =
            onboarding_from_runs(objects.as_ref(), deployment, local_runs(deployment));
        let user_state = state_key(deployment);
        env.onboarding_actions = actions;
        env.onboarding = Some(match from_runs {
            Some(state) => state,
            // A state the app can read must record a finished apply: a
            // failed onboarding leaves a partial state (and maybe its task
            // role), which is not Joined.
            None if objects.as_ref().is_some_and(|o| o.has_state(&user_state)) => {
                match d.state_document(bucket, &user_state).await? {
                    Some(text) if onboarding_complete(&text) => OnboardingState::Joined,
                    _ => OnboardingState::Incomplete { lock: None },
                }
            }
            // Without a state here (a workspace user, who can't list the
            // bucket, or an onboarding managed elsewhere) the task role is
            // the only signal.
            None => {
                let (task_role, _) = user_role_names(&env.name, &key);
                match d.role_exists(&task_role).await? {
                    Knowable::Yes => OnboardingState::Joined,
                    Knowable::No | Knowable::Unknown => OnboardingState::NotJoined,
                }
            }
        });
    }

    Ok(EnvironmentScan {
        region: region.to_string(),
        account: who.account.clone(),
        owner_id: who.user_id.clone(),
        owner_key: key,
        state_bucket,
        boundary,
        environments,
    })
}

/// An OpenTofu state that manages nothing (what `tofu destroy` leaves):
/// valid JSON whose `resources` is an empty list. Anything else, unreadable
/// contents included, counts as a deployment.
pub(crate) fn is_empty_state(text: &str) -> bool {
    serde_json::from_str::<Value>(text)
        .ok()
        .and_then(|v| v.get("resources").and_then(Value::as_array).map(Vec::is_empty))
        .unwrap_or(false)
}

/// Whether a state object of `size` bytes is read to see if it is empty:
/// only a small one, and only the first [`MAX_STATE_PEEKS`] per scan.
fn may_be_empty_state(size: i64, peeks_so_far: usize) -> bool {
    (0..=EMPTY_STATE_MAX_BYTES).contains(&size) && peeks_so_far < MAX_STATE_PEEKS
}

/// Whether a read state object still counts as a deployment. Fails closed:
/// only a state read in full and found empty is dropped (or one deleted
/// since the listing); unreadable, denied or failed reads keep it, and one
/// failed read never fails the whole scan.
fn keep_state(key: &str, read: Result<Option<String>, AwsError>) -> bool {
    match read {
        Ok(None) => false,
        Ok(Some(text)) => !is_empty_state(&text),
        Err(e) => {
            log::warn!("Could not read the state object {key} ({e}); counting it as a deployment");
            true
        }
    }
}

/// What a failed lookup means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Lookup {
    Missing,
    Denied,
    Error,
}

/// From a service error's code and HTTP status. HeadBucket has no body, so
/// its 403/404 carry no code; a 403 is also what an `expected_bucket_owner`
/// mismatch gives, which is right: a bucket of another account's is not
/// ours to know about.
fn lookup_outcome(code: Option<&str>, status: Option<u16>) -> Lookup {
    const MISSING: &[&str] = &["NotFound", "NoSuchEntity", "NoSuchBucket", "NoSuchKey"];
    const DENIED: &[&str] = &["AccessDenied", "AccessDeniedException", "Forbidden"];
    match (code, status) {
        (Some(c), _) if MISSING.contains(&c) => Lookup::Missing,
        (Some(c), _) if DENIED.contains(&c) => Lookup::Denied,
        (None, Some(404)) => Lookup::Missing,
        // A 403 with no body (HeadBucket). With a code that isn't access
        // denied (InvalidAccessKeyId, SignatureDoesNotMatch, ExpiredToken,
        // ...) it is an error the caller must see.
        (None, Some(403)) => Lookup::Denied,
        _ => Lookup::Error,
    }
}

/// Code and HTTP status of an SDK error (no status for a non-service
/// error). `SdkError` is the same type in every SDK crate.
fn error_parts<E: ProvideErrorMetadata>(
    err: &SdkError<E, HttpResponse>,
) -> (Option<String>, Option<u16>) {
    (
        err.code().map(str::to_string),
        err.raw_response().map(|r| r.status().as_u16()),
    )
}

/// Yes / No (missing) / Unknown (denied) from a lookup's result.
pub(crate) fn knowable<T, E>(
    action: &str,
    res: Result<T, SdkError<E, HttpResponse>>,
) -> Result<Knowable, AwsError>
where
    E: ProvideErrorMetadata + std::error::Error + Send + Sync + 'static,
{
    match res {
        Ok(_) => Ok(Knowable::Yes),
        Err(e) => {
            let (code, status) = error_parts(&e);
            match lookup_outcome(code.as_deref(), status) {
                Lookup::Missing => Ok(Knowable::No),
                Lookup::Denied => Ok(Knowable::Unknown),
                Lookup::Error => Err(classify_sdk_error(action, &e)),
            }
        }
    }
}

/// The real thing, from a connection's `SdkConfig`.
pub struct SdkDetectApi {
    ssm: aws_sdk_ssm::Client,
    s3: aws_sdk_s3::Client,
    iam: aws_sdk_iam::Client,
    /// `expected_bucket_owner` on every S3 call.
    account: String,
}

impl SdkDetectApi {
    pub fn new(cfg: &SdkConfig, account: &str) -> Self {
        SdkDetectApi {
            ssm: aws_sdk_ssm::Client::new(cfg),
            s3: aws_sdk_s3::Client::new(cfg),
            iam: aws_sdk_iam::Client::new(cfg),
            account: account.to_string(),
        }
    }

    /// Keys (and their age) under `prefix`, or `None` when listing is
    /// denied or the bucket is gone.
    async fn list(
        &self,
        bucket: &str,
        prefix: &str,
    ) -> Result<Option<Vec<(String, u64, i64)>>, AwsError> {
        const ACTION: &str = "s3:ListBucket";
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let mut out = Vec::new();
        let mut token: Option<String> = None;
        loop {
            let res = self
                .s3
                .list_objects_v2()
                .bucket(bucket)
                .prefix(prefix)
                .expected_bucket_owner(&self.account)
                .set_continuation_token(token.take())
                .send()
                .await;
            let page = match res {
                Ok(p) => p,
                Err(e) => {
                    let (code, status) = error_parts(&e);
                    return match lookup_outcome(code.as_deref(), status) {
                        Lookup::Missing | Lookup::Denied => Ok(None),
                        Lookup::Error => Err(classify_sdk_error(ACTION, &e)),
                    };
                }
            };
            for obj in page.contents() {
                let Some(key) = obj.key() else { continue };
                let age = obj
                    .last_modified()
                    .map(|t| now.saturating_sub(t.secs()).max(0) as u64)
                    .unwrap_or(0);
                out.push((key.to_string(), age, obj.size().unwrap_or(0)));
            }
            if out.len() >= MAX_OBJECTS_PER_PREFIX {
                log::warn!("The state bucket lists more than {MAX_OBJECTS_PER_PREFIX} objects under {prefix}; ignoring the rest");
                break;
            }
            match page.next_continuation_token() {
                Some(t) if page.is_truncated() == Some(true) => token = Some(t.to_string()),
                _ => break,
            }
        }
        Ok(Some(out))
    }

    /// A small object's contents (a lock, or a state that may be empty);
    /// empty when it is too large to read or can't be read, `None` when it
    /// was deleted since the listing.
    async fn lock_contents(
        &self,
        bucket: &str,
        key: &str,
        size: i64,
    ) -> Result<Option<String>, AwsError> {
        const ACTION: &str = "s3:GetObject";
        if size < 0 || size as usize > MAX_UNTRUSTED_BYTES {
            return Ok(Some(String::new()));
        }
        let res = self
            .s3
            .get_object()
            .bucket(bucket)
            .key(key)
            .expected_bucket_owner(&self.account)
            .send()
            .await;
        let obj = match res {
            Ok(o) => o,
            Err(e) => {
                let (code, status) = error_parts(&e);
                return match lookup_outcome(code.as_deref(), status) {
                    Lookup::Missing => Ok(None),
                    Lookup::Denied => Ok(Some(String::new())),
                    Lookup::Error => Err(classify_sdk_error(ACTION, &e)),
                };
            }
        };
        if obj
            .content_length()
            .is_some_and(|n| n < 0 || n as usize > MAX_UNTRUSTED_BYTES)
        {
            return Ok(Some(String::new()));
        }
        let bytes = obj
            .body
            .collect()
            .await
            .map_err(|e| AwsError::Other {
                message: format!("Could not read a state lock: {e}"),
            })?
            .into_bytes();
        if bytes.len() > MAX_UNTRUSTED_BYTES {
            return Ok(Some(String::new()));
        }
        Ok(Some(String::from_utf8_lossy(&bytes).into_owned()))
    }
}

impl DetectApi for SdkDetectApi {
    async fn environment_records(&self) -> Result<Vec<(String, String)>, AwsError> {
        const ACTION: &str = "ssm:GetParametersByPath";
        let path = environment_parameter_name("");
        let mut out = Vec::new();
        let mut token: Option<String> = None;
        loop {
            let page = self
                .ssm
                .get_parameters_by_path()
                .path(path.trim_end_matches('/'))
                .recursive(false)
                .with_decryption(false)
                .set_next_token(token.take())
                .send()
                .await
                .map_err(|e| classify_sdk_error(ACTION, &e))?;
            for p in page.parameters() {
                if let (Some(name), Some(value)) = (p.name(), p.value()) {
                    out.push((name.to_string(), value.to_string()));
                }
            }
            if out.len() >= MAX_RECORDS {
                log::warn!("More than {MAX_RECORDS} environment records; ignoring the rest");
                break;
            }
            match page.next_token() {
                Some(t) => token = Some(t.to_string()),
                None => break,
            }
        }
        Ok(out)
    }

    async fn state_objects(&self, bucket: &str) -> Result<Option<StateObjects>, AwsError> {
        let mut objects = StateObjects::default();
        let mut lock_keys = Vec::new();
        let mut peeks = 0usize;
        for prefix in ["environments/", "users/", "boundary/"] {
            let Some(listed) = self.list(bucket, prefix).await? else {
                return Ok(None);
            };
            for (key, age, size) in listed {
                if key.ends_with("/terraform.tfstate") {
                    // `tofu destroy` leaves the state object behind, empty.
                    // An empty state is no deployment: without this an
                    // environment destroyed from a bundle (or by the app's
                    // own Destroy) would read as Incomplete for ever, and a
                    // removed onboarding as Joined. Only a small object can
                    // be empty, so only those are read.
                    if may_be_empty_state(size, peeks) {
                        peeks += 1;
                        if !keep_state(&key, self.lock_contents(bucket, &key, size).await) {
                            continue;
                        }
                    }
                    objects.states.push(key);
                } else if key.ends_with("/terraform.tfstate.tflock") {
                    lock_keys.push((key, age, size));
                }
            }
        }
        if lock_keys.len() > MAX_LOCKS {
            log::warn!("More than {MAX_LOCKS} state locks; ignoring the rest");
            lock_keys.truncate(MAX_LOCKS);
        }
        for (key, age, size) in lock_keys {
            if let Some(json) = self.lock_contents(bucket, &key, size).await? {
                objects.locks.push((key, json, age));
            }
        }
        Ok(Some(objects))
    }

    async fn bucket_exists(&self, bucket: &str) -> Result<Knowable, AwsError> {
        let res = self
            .s3
            .head_bucket()
            .bucket(bucket)
            .expected_bucket_owner(&self.account)
            .send()
            .await;
        knowable("s3:ListBucket", res)
    }

    async fn boundary_exists(&self, account: &str) -> Result<Knowable, AwsError> {
        let arn = format!("arn:aws:iam::{account}:policy/{BOUNDARY_POLICY_NAME}");
        let res = self.iam.get_policy().policy_arn(arn).send().await;
        knowable("iam:GetPolicy", res)
    }

    async fn role_exists(&self, name: &str) -> Result<Knowable, AwsError> {
        let res = self.iam.get_role().role_name(name).send().await;
        knowable("iam:GetRole", res)
    }

    async fn state_document(&self, bucket: &str, key: &str) -> Result<Option<String>, AwsError> {
        const ACTION: &str = "s3:GetObject";
        let res = self
            .s3
            .get_object()
            .bucket(bucket)
            .key(key)
            .expected_bucket_owner(&self.account)
            .send()
            .await;
        let obj = match res {
            Ok(o) => o,
            Err(e) => {
                let (code, status) = error_parts(&e);
                return match lookup_outcome(code.as_deref(), status) {
                    Lookup::Missing | Lookup::Denied => Ok(None),
                    Lookup::Error => Err(classify_sdk_error(ACTION, &e)),
                };
            }
        };
        if obj
            .content_length()
            .is_some_and(|n| n < 0 || n as usize > MAX_STATE_DOCUMENT_BYTES)
        {
            return Ok(None);
        }
        // Capped while reading too: the length header may be absent.
        let mut body = obj.body;
        let mut bytes = Vec::new();
        while let Some(chunk) = body.try_next().await.map_err(|e| AwsError::Other {
            message: format!("Could not read a state object: {e}"),
        })? {
            if bytes.len() + chunk.len() > MAX_STATE_DOCUMENT_BYTES {
                return Ok(None);
            }
            bytes.extend_from_slice(&chunk);
        }
        Ok(Some(String::from_utf8_lossy(&bytes).into_owned()))
    }
}

/// `s3:CreateBucket`'s answer, by error code: `Some(Ok)` for a bucket this
/// account already owns, `Some(Err)` for a name another account holds,
/// `None` for an ordinary error to classify.
fn create_bucket_outcome(code: Option<&str>, bucket: &str) -> Option<Result<(), AwsError>> {
    match code {
        Some("BucketAlreadyOwnedByYou") => Some(Ok(())),
        Some("BucketAlreadyExists") => Some(Err(AwsError::StateBucketTaken {
            bucket: bucket.to_string(),
        })),
        _ => None,
    }
}

const STATE_TAG: (&str, &str) = ("triple-c:component", "state");

/// HeadBucket with `expected_bucket_owner` right after our own CreateBucket
/// succeeded (or said BucketAlreadyOwnedByYou): a 403 means the bucket of
/// that name is another account's (the credentials just worked), so it is
/// [`AwsError::StateBucketTaken`].
fn head_after_create_error(status: Option<u16>, bucket: &str) -> Option<AwsError> {
    (status == Some(403)).then(|| AwsError::StateBucketTaken {
        bucket: bucket.to_string(),
    })
}

/// The bucket's tag set with ours added, keeping every other tag, or `None`
/// when `triple-c:component=state` is already there (nothing to put). A
/// different `triple-c:component` value is replaced: no resource overrides
/// the component tag.
fn tags_with_state(existing: &[(String, String)]) -> Option<Vec<(String, String)>> {
    if existing
        .iter()
        .any(|(k, v)| k == STATE_TAG.0 && v == STATE_TAG.1)
    {
        return None;
    }
    let mut tags: Vec<(String, String)> = existing
        .iter()
        .filter(|(k, _)| k != STATE_TAG.0)
        .cloned()
        .collect();
    tags.push((STATE_TAG.0.to_string(), STATE_TAG.1.to_string()));
    Some(tags)
}

fn state_tag() -> Tag {
    Tag::builder()
        .key(STATE_TAG.0)
        .value(STATE_TAG.1)
        .build()
        .expect("key and value are set")
}

/// The component tag, and a `LocationConstraint` everywhere but
/// `us-east-1` (which refuses one).
fn create_bucket_configuration(region: &str) -> CreateBucketConfiguration {
    let b = CreateBucketConfiguration::builder().tags(state_tag());
    if region == "us-east-1" {
        b.build()
    } else {
        b.location_constraint(BucketLocationConstraint::from(region))
            .build()
    }
}

/// Deny every request to the bucket that doesn't use TLS.
pub(crate) fn state_bucket_policy(bucket: &str) -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Sid": "DenyInsecureTransport",
            "Effect": "Deny",
            "Principal": "*",
            "Action": "s3:*",
            "Resource": [
                format!("arn:aws:s3:::{bucket}"),
                format!("arn:aws:s3:::{bucket}/*"),
            ],
            "Condition": { "Bool": { "aws:SecureTransport": "false" } },
        }],
    })
    .to_string()
}

/// The bucket policy to put, given the bucket's current one: ours when it
/// has none, `None` when our TLS statement is already in it, and otherwise
/// the existing policy with our statement added (or a stale statement under
/// our Sid replaced). An organisation's own statements are never dropped.
/// Refuses a policy it can't parse rather than overwrite it.
pub(crate) fn state_bucket_policy_update(
    existing: Option<&str>,
    bucket: &str,
) -> Result<Option<String>, String> {
    let ours: Value = serde_json::from_str(&state_bucket_policy(bucket)).expect("our policy is JSON");
    let Some(existing) = existing else {
        return Ok(Some(ours.to_string()));
    };
    let ours = ours["Statement"][0].clone();
    let sid = ours["Sid"].clone();
    let mut policy: Value = serde_json::from_str(existing)
        .ok()
        .filter(Value::is_object)
        .ok_or("The state bucket's existing policy can't be read, so it was left as it is")?;
    let statements = match policy.get("Statement") {
        Some(Value::Array(a)) => a.clone(),
        Some(single @ Value::Object(_)) => vec![single.clone()],
        None => Vec::new(),
        Some(_) => {
            return Err("The state bucket's existing policy can't be read, so it was left as it is".into())
        }
    };
    if statements.contains(&ours) {
        return Ok(None);
    }
    let mut kept: Vec<Value> = statements.into_iter().filter(|s| s.get("Sid") != Some(&sid)).collect();
    kept.push(ours);
    policy["Statement"] = Value::Array(kept);
    if policy.get("Version").is_none() {
        policy["Version"] = json!("2012-10-17");
    }
    Ok(Some(policy.to_string()))
}

/// Create (or adopt) `triple-c-tfstate-<account>-<region>` and configure it:
/// the component tag, versioning, SSE-S3, all four public-access blocks and
/// a TLS-only policy. Idempotent. Every call after the create carries
/// `expected_bucket_owner`, so a bucket of another account's is never
/// configured or trusted.
pub async fn ensure_state_bucket(
    cfg: &SdkConfig,
    account: &str,
    region: &str,
) -> Result<String, AwsError> {
    let region =
        validate_supported_region(region).map_err(|message| AwsError::Other { message })?;
    let bucket = state_bucket_name(account, &region);
    ensure_named_state_bucket(cfg, account, &region, &bucket).await
}

/// [`ensure_state_bucket`] for a given bucket name (the live tests' own
/// throwaway bucket); `region` is already validated.
pub(crate) async fn ensure_named_state_bucket(
    cfg: &SdkConfig,
    account: &str,
    region: &str,
    bucket: &str,
) -> Result<String, AwsError> {
    let region = region.to_string();
    let bucket = bucket.to_string();
    let s3 = aws_sdk_s3::Client::new(cfg);

    if let Err(e) = s3
        .create_bucket()
        .bucket(&bucket)
        .create_bucket_configuration(create_bucket_configuration(&region))
        .send()
        .await
    {
        match create_bucket_outcome(e.code(), &bucket) {
            Some(Ok(())) => {}
            Some(Err(taken)) => return Err(taken),
            None => return Err(classify_sdk_error("s3:CreateBucket", &e)),
        }
    }
    // us-east-1 answers a create of a bucket this account owns with success,
    // not BucketAlreadyOwnedByYou: confirm ownership before configuring.
    if let Err(e) = s3
        .head_bucket()
        .bucket(&bucket)
        .expected_bucket_owner(account)
        .send()
        .await
    {
        let (_, status) = error_parts(&e);
        return Err(head_after_create_error(status, &bucket)
            .unwrap_or_else(|| classify_sdk_error("s3:ListBucket", &e)));
    }

    let existing = match s3
        .get_bucket_tagging()
        .bucket(&bucket)
        .expected_bucket_owner(account)
        .send()
        .await
    {
        Ok(out) => out
            .tag_set()
            .iter()
            .map(|t| (t.key().to_string(), t.value().to_string()))
            .collect(),
        Err(e) if e.code() == Some("NoSuchTagSet") => Vec::new(),
        Err(e) => return Err(classify_sdk_error("s3:GetBucketTagging", &e)),
    };
    if let Some(tags) = tags_with_state(&existing) {
        let tag_set = tags
            .into_iter()
            .map(|(k, v)| Tag::builder().key(k).value(v).build())
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| AwsError::Other {
                message: format!("Could not build the state bucket's tags: {e}"),
            })?;
        s3.put_bucket_tagging()
            .bucket(&bucket)
            .expected_bucket_owner(account)
            .tagging(
                Tagging::builder()
                    .set_tag_set(Some(tag_set))
                    .build()
                    .expect("tag set is set"),
            )
            .send()
            .await
            .map_err(|e| classify_sdk_error("s3:PutBucketTagging", &e))?;
    }
    s3.put_bucket_versioning()
        .bucket(&bucket)
        .expected_bucket_owner(account)
        .versioning_configuration(
            VersioningConfiguration::builder()
                .status(BucketVersioningStatus::Enabled)
                .build(),
        )
        .send()
        .await
        .map_err(|e| classify_sdk_error("s3:PutBucketVersioning", &e))?;
    s3.put_bucket_encryption()
        .bucket(&bucket)
        .expected_bucket_owner(account)
        .server_side_encryption_configuration(
            ServerSideEncryptionConfiguration::builder()
                .rules(
                    ServerSideEncryptionRule::builder()
                        .apply_server_side_encryption_by_default(
                            ServerSideEncryptionByDefault::builder()
                                .sse_algorithm(ServerSideEncryption::Aes256)
                                .build()
                                .expect("algorithm is set"),
                        )
                        .build(),
                )
                .build()
                .expect("rules are set"),
        )
        .send()
        .await
        .map_err(|e| classify_sdk_error("s3:PutEncryptionConfiguration", &e))?;
    s3.put_public_access_block()
        .bucket(&bucket)
        .expected_bucket_owner(account)
        .public_access_block_configuration(
            PublicAccessBlockConfiguration::builder()
                .block_public_acls(true)
                .ignore_public_acls(true)
                .block_public_policy(true)
                .restrict_public_buckets(true)
                .build(),
        )
        .send()
        .await
        .map_err(|e| classify_sdk_error("s3:PutBucketPublicAccessBlock", &e))?;
    // Only when the TLS statement is missing, and then added to whatever
    // policy the bucket has (an organisation may have its own statements).
    let existing = match s3
        .get_bucket_policy()
        .bucket(&bucket)
        .expected_bucket_owner(account)
        .send()
        .await
    {
        Ok(out) => out.policy().map(str::to_string),
        Err(e) if e.code() == Some("NoSuchBucketPolicy") => None,
        Err(e) => return Err(classify_sdk_error("s3:GetBucketPolicy", &e)),
    };
    if let Some(policy) = state_bucket_policy_update(existing.as_deref(), &bucket)
        .map_err(|message| AwsError::Other { message })?
    {
        s3.put_bucket_policy()
            .bucket(&bucket)
            .expected_bucket_owner(account)
            .policy(policy)
            .send()
            .await
            .map_err(|e| classify_sdk_error("s3:PutBucketPolicy", &e))?;
    }
    Ok(bucket)
}

#[cfg(test)]
mod tests;
