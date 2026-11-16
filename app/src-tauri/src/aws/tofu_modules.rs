//! The OpenTofu modules Triple-C ships (`aws/terraform/*` at the repo root)
//! and the names the modules and the app agree on.
//!
//! Every name a module or a phase-4 command uses — environment, parameter,
//! owner key, per-user roles, state bucket and keys — is built here and
//! nowhere else. The tests parse the shipped HCL and pin it to these
//! constants: versions, default tags, the record's fields, no inbound access
//! and no endpoint overrides.

use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::models::host_connection::validate_supported_region;

/// `locals.template_version` (and the `triple-c:template-version` default
/// tag) of `aws/terraform/environment`.
pub const ENVIRONMENT_MODULE_VERSION: u32 = 2;
/// The same for the user-onboarding module.
#[allow(dead_code)] // the onboarding module's version; checked against its HCL by a test
pub const ONBOARD_MODULE_VERSION: u32 = 1;
/// The permissions boundary every module-created role carries (no path).
/// Equal to [`super::policies::PERMISSIONS_BOUNDARY_NAME`]; a test checks it.
pub const BOUNDARY_POLICY_NAME: &str = "triple-c-permissions-boundary";

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ModuleKind {
    Environment,
    UserOnboard,
    PermissionsBoundary,
}

macro_rules! module {
    ($dir:literal: $($file:literal),+ $(,)?) => {
        &[$(($file, include_str!(concat!("../../../../aws/terraform/", $dir, "/", $file)))),+]
    };
}

const ENVIRONMENT_FILES: &[(&str, &str)] = module!("environment":
    "versions.tf",
    "variables.tf",
    "main.tf",
    "network.tf",
    "outputs.tf",
    ".terraform.lock.hcl",
);

const USER_ONBOARD_FILES: &[(&str, &str)] = module!("user-onboard":
    "versions.tf",
    "variables.tf",
    "main.tf",
    "iam.tf",
    "efs.tf",
    "outputs.tf",
    ".terraform.lock.hcl",
);

const PERMISSIONS_BOUNDARY_FILES: &[(&str, &str)] = module!("permissions-boundary":
    "versions.tf",
    "variables.tf",
    "main.tf",
    "outputs.tf",
    ".terraform.lock.hcl",
);

/// The module's files as (file name relative to the module directory,
/// contents), lock file included, for writing into a work or bundle folder.
pub fn module_files(kind: ModuleKind) -> &'static [(&'static str, &'static str)] {
    match kind {
        ModuleKind::Environment => ENVIRONMENT_FILES,
        ModuleKind::UserOnboard => USER_ONBOARD_FILES,
        ModuleKind::PermissionsBoundary => PERMISSIONS_BOUNDARY_FILES,
    }
}

/// `^[a-z][a-z0-9-]{0,19}$`, not ending in `-`, with no `--` — the same rule
/// as the module's `environment_name` validation.
pub fn validate_environment_name(name: &str) -> Result<String, String> {
    let b = name.as_bytes();
    let ok = !b.is_empty()
        && b.len() <= 20
        && b[0].is_ascii_lowercase()
        && b.iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'-')
        && !name.ends_with('-')
        && !name.contains("--");
    if ok {
        Ok(name.to_string())
    } else {
        Err(format!(
            "\"{}\" is not a valid environment name: use 1 to 20 lowercase letters, digits and \
             single hyphens, starting with a letter and not ending in a hyphen",
            name.escape_debug()
        ))
    }
}

/// An environment's ECS cluster name (the environment module's).
pub fn environment_cluster_name(env: &str) -> String {
    format!("triple-c-{env}")
}

/// An environment's CloudWatch log group (the environment module's).
pub fn environment_log_group(env: &str) -> String {
    format!("/triple-c/{env}")
}

/// The SSM parameter holding an environment's record.
pub fn environment_parameter_name(env: &str) -> String {
    format!("/triple-c/environments/{env}")
}

/// The first 12 hex characters of `sha256("<region>:<aws:userid>")`. A
/// `UserId` holds a `:`, which IAM role names reject, so per-user names use
/// this instead. The region is part of it because role names are
/// account-global while environments are regional: the same user in the same
/// environment name in two regions gets two sets of roles.
pub fn owner_key(region: &str, user_id: &str) -> String {
    Sha256::digest(format!("{region}:{user_id}").as_bytes())
        .iter()
        .take(6)
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// A user's (task role, execution role) names. At most 47 characters.
pub fn user_role_names(env: &str, owner_key: &str) -> (String, String) {
    (
        format!("triple-c-{env}-{owner_key}-task"),
        format!("triple-c-{env}-{owner_key}-exec"),
    )
}

/// Whether a project id is used in names as it is: 1 to 64 characters of
/// `[A-Za-z0-9-]`. Every UUID (how the app makes ids) is.
fn is_clean_project_id(id: &str) -> bool {
    (1..=64).contains(&id.len()) && id.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-')
}

/// 16 hex characters of `sha256(id)`.
fn project_id_hash(id: &str) -> String {
    Sha256::digest(id.as_bytes())
        .iter()
        .take(8)
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// A project id as a name component, safe in a secret name, an EFS path and
/// a log stream. A clean id (see [`is_clean_project_id`]) is used as is.
/// Anything else — only a hand-edited `projects.json` has one — becomes its
/// first 40 characters with everything outside `[A-Za-z0-9-]` replaced by
/// `-`, then `_` and a hash of the whole id. Clean ids never contain `_`, so
/// the two forms can't collide, and ids that sanitise alike (`a/b`, `a:b`)
/// still differ by the hash.
fn project_component(project_id: &str) -> String {
    if is_clean_project_id(project_id) {
        return project_id.to_string();
    }
    let prefix: String = project_id
        .chars()
        .take(40)
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect();
    format!("{prefix}_{}", project_id_hash(project_id))
}

/// A project's ECS task definition family:
/// `triple-c-<env>-<owner_key>-<first 12 of the project id>` (an id that
/// isn't clean contributes `_` and 11 hex of its hash instead). At most 55
/// characters of `[A-Za-z0-9_-]`. Other users can register revisions into
/// any `triple-c-*` family, so a family is never trusted by name alone (see
/// `aws::workspace`), and two of a user's projects could share 12 leading
/// characters, so a family's tasks and revisions are told apart by their
/// `triple-c:project` tag.
pub fn task_family(env: &str, owner_key: &str, project_id: &str) -> String {
    let id: String = if is_clean_project_id(project_id) {
        project_id.chars().take(12).collect()
    } else {
        format!("_{}", &project_id_hash(project_id)[..11])
    };
    format!("triple-c-{env}-{owner_key}-{id}")
}

/// The project's Secrets Manager secret (P1): `triple-c/<env>/<owner_key>/<project id>`.
pub fn project_secret_name(env: &str, owner_key: &str, project_id: &str) -> String {
    format!(
        "triple-c/{env}/{owner_key}/{}",
        project_component(project_id)
    )
}

/// The root of the project's EFS access point (P4), at most 90 characters.
pub fn access_point_root(owner_key: &str, project_id: &str) -> String {
    format!("/triple-c/{owner_key}/{}", project_component(project_id))
}

/// The root of the project's EFS-mode `/workspace` access point (Q1):
/// `/triple-c-workspaces/<owner_key>/<project>`, a sibling tree of the home
/// roots (under [`access_point_root`] it would sit inside the home access
/// point). At most 100 characters.
pub fn workspace_access_point_root(owner_key: &str, project_id: &str) -> String {
    format!(
        "/triple-c-workspaces/{owner_key}/{}",
        project_component(project_id)
    )
}

/// The container's `awslogs-stream-prefix`; ECS names the stream
/// `<prefix>/workspace/<task id>`.
pub fn log_stream_prefix(owner_key: &str, project_id: &str) -> String {
    format!("{owner_key}/{}", project_component(project_id))
}

/// The awslogs stream of a task's `workspace` container:
/// `<prefix>/<container>/<task-id>`, the task id being the last segment of
/// its ARN.
pub fn task_log_stream(stream_prefix: &str, task_arn: &str) -> String {
    let task_id = task_arn.rsplit('/').next().unwrap_or(task_arn);
    format!("{stream_prefix}/workspace/{task_id}")
}

/// The `creation_token` of the user's file system (the onboarding module's
/// `local.name`).
pub fn user_file_system_token(env: &str, owner_key: &str) -> String {
    format!("triple-c-{env}-{owner_key}")
}

/// The per-account, per-region state bucket.
pub fn state_bucket_name(account: &str, region: &str) -> String {
    format!("triple-c-tfstate-{account}-{region}")
}

/// One deployment, i.e. one state object in the bucket (D9).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Deployment<'a> {
    /// The environment module for this environment name.
    Environment(&'a str),
    /// A user's onboarding module in an environment.
    User { env: &'a str, owner_key: &'a str },
    /// The account's permissions-boundary module.
    Boundary,
}

/// A deployment's state object in [`state_bucket_name`] (D9); the lock is the
/// same key plus `.tflock`.
pub fn state_key(d: Deployment) -> String {
    match d {
        Deployment::Environment(env) => format!("environments/{env}/terraform.tfstate"),
        Deployment::User { env, owner_key } => {
            format!("users/{env}/{owner_key}/terraform.tfstate")
        }
        Deployment::Boundary => "boundary/terraform.tfstate".to_string(),
    }
}

/// How an environment's tasks reach the network (D4).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "mode", rename_all = "kebab-case")]
pub enum NetworkChoice {
    /// A new VPC with two public subnets; tasks get a public IP. No NAT, $0.
    Public,
    /// A new VPC with private subnets behind one NAT gateway (~$33/month).
    PrivateNat,
    /// The user's own VPC and exactly two subnets.
    Existing {
        vpc_id: String,
        subnet_ids: Vec<String>,
        assign_public_ip: bool,
    },
}

/// `vpc-` / `subnet-` followed by 8 or 17 lowercase hex characters.
fn is_aws_id(id: &str, prefix: &str) -> bool {
    id.strip_prefix(prefix).is_some_and(|hex| {
        (hex.len() == 8 || hex.len() == 17)
            && hex.bytes().all(|c| matches!(c, b'0'..=b'9' | b'a'..=b'f'))
    })
}

/// The longest lifetime cap an environment can carry, in hours (a week).
pub const MAX_WORKSPACE_HOURS: u32 = 168;

/// The environment module's `terraform.tfvars.json`. Applies the same rules
/// as the module's variable validations, so a bad value fails here first.
pub fn environment_vars(
    env: &str,
    region: &str,
    network: &NetworkChoice,
    max_workspace_hours: u32,
) -> Result<serde_json::Value, String> {
    let env = validate_environment_name(env)?;
    if max_workspace_hours > MAX_WORKSPACE_HOURS {
        return Err(format!(
            "The maximum workspace lifetime is at most {MAX_WORKSPACE_HOURS} hours (0 = no cap)"
        ));
    }
    let region = validate_supported_region(region)?;
    let (mode, vpc_id, subnet_ids, assign_public_ip) = match network {
        NetworkChoice::Public => ("public", String::new(), Vec::new(), false),
        NetworkChoice::PrivateNat => ("private-nat", String::new(), Vec::new(), false),
        NetworkChoice::Existing {
            vpc_id,
            subnet_ids,
            assign_public_ip,
        } => {
            if !is_aws_id(vpc_id, "vpc-") {
                return Err(format!("\"{}\" is not a VPC id", vpc_id.escape_debug()));
            }
            if subnet_ids.len() != 2 {
                return Err(format!(
                    "An existing network needs exactly 2 subnets (in different availability \
                     zones), not {}",
                    subnet_ids.len()
                ));
            }
            if let Some(bad) = subnet_ids.iter().find(|s| !is_aws_id(s, "subnet-")) {
                return Err(format!("\"{}\" is not a subnet id", bad.escape_debug()));
            }
            if subnet_ids[0] == subnet_ids[1] {
                return Err("The two subnets must be different".into());
            }
            (
                "existing",
                vpc_id.clone(),
                subnet_ids.clone(),
                *assign_public_ip,
            )
        }
    };
    Ok(json!({
        "environment_name": env,
        "region": region,
        "network_mode": mode,
        "vpc_id": vpc_id,
        "subnet_ids": subnet_ids,
        "assign_public_ip": assign_public_ip,
        "max_workspace_hours": max_workspace_hours,
    }))
}

/// The onboarding module's `terraform.tfvars.json` for the user whose
/// `aws:userid` is `owner_id`. Bedrock stays on the workspace user's own
/// credentials unless a later setting opts the task role in.
pub fn onboarding_vars(env: &str, region: &str, owner_id: &str) -> serde_json::Value {
    json!({
        "environment_name": env,
        "region": region,
        "owner_id": owner_id,
        "owner_key": owner_key(region, owner_id),
        "bedrock_on_task_role": false,
    })
}

/// The boundary module's `terraform.tfvars.json`: the shipped boundary
/// document itself, so the module holds no second copy.
pub fn boundary_vars(region: &str) -> serde_json::Value {
    json!({
        "region": region,
        "policy_json": super::policies::permissions_boundary_json(),
    })
}

#[cfg(test)]
mod tests;
