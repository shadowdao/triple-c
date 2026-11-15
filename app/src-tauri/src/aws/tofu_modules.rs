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
pub const ENVIRONMENT_MODULE_VERSION: u32 = 1;
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

/// The environment module's `terraform.tfvars.json`. Applies the same rules
/// as the module's variable validations, so a bad value fails here first.
pub fn environment_vars(
    env: &str,
    region: &str,
    network: &NetworkChoice,
) -> Result<serde_json::Value, String> {
    let env = validate_environment_name(env)?;
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
