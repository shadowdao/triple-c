//! The IAM policies Triple-C ships for each permission tier
//! (`aws/policies/*.json` at the repo root), the permissions boundary every
//! Triple-C-created role must carry, and the table of representative checks
//! the permission check simulates against the tier policies.
//!
//! The table and the JSON are kept in lock-step by the tests below: every
//! allowed action in a policy has a check, every check is allowed by its
//! policy under a small IAM-style evaluator, every owner-scoped check is
//! *denied* when the resource belongs to someone else, and the admin tier can
//! neither create unbounded roles nor destroy resources that aren't
//! Triple-C's.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum Tier {
    EnvironmentAdmin,
    WorkspaceUser,
}

/// The policy document an admin attaches for `tier`.
pub fn policy_json(tier: Tier) -> &'static str {
    match tier {
        Tier::EnvironmentAdmin => include_str!("../../../../aws/policies/environment-admin.json"),
        Tier::WorkspaceUser => include_str!("../../../../aws/policies/workspace-user.json"),
    }
}

/// The managed policy (no path) every role an environment admin creates must
/// carry as its permissions boundary.
#[allow(dead_code)] // Only tests read the policy text so far.
pub const PERMISSIONS_BOUNDARY_NAME: &str = "triple-c-permissions-boundary";

/// The document for [`PERMISSIONS_BOUNDARY_NAME`]: the most any
/// Triple-C-created role may ever hold.
#[allow(dead_code)] // Only tests read the policy text so far.
pub fn permissions_boundary_json() -> &'static str {
    include_str!("../../../../aws/policies/permissions-boundary.json")
}

/// A context value sent with a simulated request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextValue {
    /// The caller's `aws:userid` (the owner ID every per-user resource is
    /// tagged with).
    OwnerId,
    /// A fixed value. Placeholders are substituted exactly as in
    /// [`Check::resource`] — use [`render`].
    Literal(&'static str),
}

/// One simulated request: "may the caller do `action` on `resource`, given
/// `context`?"
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Check {
    pub action: &'static str,
    /// An ARN template ([`render`]).
    pub resource: &'static str,
    /// Context entries. A key that appears more than once is a multi-valued
    /// key (e.g. `aws:TagKeys`): whoever builds the simulation request must
    /// send all its values as one `StringList` context entry (not enforced
    /// here; the permission check's simulation builder owns it).
    pub context: &'static [(&'static str, ContextValue)],
    pub write: bool,
}

/// What a resource or literal template's placeholders stand for.
#[derive(Debug, Clone, Copy)]
pub struct Placeholders<'a> {
    /// `{region}`
    pub region: &'a str,
    /// `{account}`
    pub account: &'a str,
    /// `{principal}`: the caller's IAM user or role ARN (the simulation's
    /// `PolicySourceArn`). `{principal_name}` is its last path segment: the
    /// user or role name.
    pub principal: &'a str,
}

/// Substitutes `{region}`, `{account}`, `{principal}` and
/// `{principal_name}` in a resource or [`ContextValue::Literal`] template.
pub fn render(template: &str, p: &Placeholders) -> String {
    let name = p.principal.rsplit('/').next().unwrap_or(p.principal);
    template
        .replace("{region}", p.region)
        .replace("{account}", p.account)
        .replace("{principal_name}", name)
        .replace("{principal}", p.principal)
}

pub fn checks(tier: Tier) -> &'static [Check] {
    match tier {
        Tier::WorkspaceUser => WORKSPACE_USER_CHECKS,
        Tier::EnvironmentAdmin => ENVIRONMENT_ADMIN_CHECKS,
    }
}

use ContextValue::{Literal, OwnerId};

type Context = &'static [(&'static str, ContextValue)];

const OWNER_REQUEST_TAG: &str = "aws:RequestTag/triple-c:owner";
const OWNER_RESOURCE_TAG: &str = "aws:ResourceTag/triple-c:owner";
/// The tag SSM puts on a session; its value is the starter's `aws:userid`.
const SESSION_OWNER: &str = "ssm:resourceTag/aws:ssmmessages:session-id";

/// IAM refuses `${aws:PrincipalArn}` as a Resource (verified live), so a
/// user's own ARN is `user/${aws:username}` (or `user/*/${aws:username}`
/// with a path), resolved from this entry. A role cannot be named that way
/// (there is no role-name variable); the policies allow SSO permission-set
/// roles and `triple-c-*` roles (R18). Other roles fall back to probes.
const OWN_PRINCIPAL: Context = &[("aws:username", Literal("{principal_name}"))];
/// Role writes name the boundary; `${aws:PrincipalAccount}` resolves from
/// the second entry.
const WITH_BOUNDARY: Context = &[
    (
        "iam:PermissionsBoundary",
        Literal("arn:aws:iam::{account}:policy/triple-c-permissions-boundary"),
    ),
    ("aws:PrincipalAccount", Literal("{account}")),
];
/// Created resources carry `triple-c:component`; destroyed ones must.
const COMPONENT_REQUESTED: Context =
    &[("aws:RequestTag/triple-c:component", Literal("environment"))];
const COMPONENT_RESOURCE_TAG: &str = "aws:ResourceTag/triple-c:component";
const COMPONENT_TAGGED: Context = &[(COMPONENT_RESOURCE_TAG, Literal("environment"))];

const PROBE_CLUSTER: &str = "arn:aws:ecs:{region}:{account}:cluster/triple-c-probe";
const PROBE_TASK: &str =
    "arn:aws:ecs:{region}:{account}:task/triple-c-probe/0123456789abcdef0123456789abcdef";
const PROBE_TASK_DEFINITION: &str =
    "arn:aws:ecs:{region}:{account}:task-definition/triple-c-probe:1";
const PROBE_ROLE: &str = "arn:aws:iam::{account}:role/triple-c-probe";
const PROBE_SECRET: &str =
    "arn:aws:secretsmanager:{region}:{account}:secret:triple-c/probe/probe-AbCdEf";
const PROBE_KEY: &str = "arn:aws:kms:{region}:{account}:key/00000000-0000-0000-0000-000000000000";
const PROBE_ALIAS: &str = "arn:aws:kms:{region}:{account}:alias/triple-c-probe";
const PROBE_LOG_GROUP: &str = "arn:aws:logs:{region}:{account}:log-group:/triple-c/probe";
const PROBE_PARAMETER: &str =
    "arn:aws:ssm:{region}:{account}:parameter/triple-c/environments/probe";
const PROBE_STACK: &str = "arn:aws:cloudformation:{region}:{account}:stack/triple-c-probe/00000000-0000-0000-0000-000000000000";
const PROBE_FILE_SYSTEM: &str =
    "arn:aws:elasticfilesystem:{region}:{account}:file-system/fs-00000000";
const PROBE_ACCESS_POINT: &str =
    "arn:aws:elasticfilesystem:{region}:{account}:access-point/fsap-00000000000000000";
const PROBE_SECURITY_GROUP: &str =
    "arn:aws:ec2:{region}:{account}:security-group/sg-00000000000000000";
const PROBE_VPC: &str = "arn:aws:ec2:{region}:{account}:vpc/vpc-00000000000000000";
const PROBE_SESSION: &str = "arn:aws:ssm:{region}:{account}:session/probe-0123456789abcdef0";

const fn write(action: &'static str, resource: &'static str, context: Context) -> Check {
    Check {
        action,
        resource,
        context,
        write: true,
    }
}

const fn read(action: &'static str, resource: &'static str, context: Context) -> Check {
    Check {
        action,
        resource,
        context,
        write: false,
    }
}

const WORKSPACE_USER_CHECKS: &[Check] = &[
    write(
        "ecs:RunTask",
        PROBE_TASK_DEFINITION,
        &[
            (OWNER_REQUEST_TAG, OwnerId),
            // The definition names the task role, so it must be the caller's.
            (OWNER_RESOURCE_TAG, OwnerId),
            ("ecs:cluster", Literal(PROBE_CLUSTER)),
        ],
    ),
    write(
        "ecs:TagResource",
        PROBE_TASK,
        &[
            (OWNER_REQUEST_TAG, OwnerId),
            ("ecs:CreateAction", Literal("RunTask")),
        ],
    ),
    write(
        "iam:PassRole",
        PROBE_ROLE,
        &[
            ("iam:ResourceTag/triple-c:owner", OwnerId),
            ("iam:PassedToService", Literal("ecs-tasks.amazonaws.com")),
        ],
    ),
    write("ecs:StopTask", PROBE_TASK, &[(OWNER_RESOURCE_TAG, OwnerId)]),
    write(
        "ecs:ExecuteCommand",
        PROBE_TASK,
        &[(OWNER_RESOURCE_TAG, OwnerId)],
    ),
    // ExecuteCommand is authorized against the cluster too (verified live).
    write("ecs:ExecuteCommand", PROBE_CLUSTER, &[]),
    write(
        "ssm:StartSession",
        PROBE_TASK,
        &[(OWNER_RESOURCE_TAG, OwnerId)],
    ),
    // Port forwarding needs the document as well as the target.
    write(
        "ssm:StartSession",
        "arn:aws:ssm:{region}::document/AWS-StartPortForwardingSession",
        &[],
    ),
    write(
        "ssm:TerminateSession",
        PROBE_SESSION,
        &[(SESSION_OWNER, OwnerId)],
    ),
    write(
        "ssm:ResumeSession",
        PROBE_SESSION,
        &[(SESSION_OWNER, OwnerId)],
    ),
    // The session client opens its data channel on the session.
    write(
        "ssmmessages:OpenDataChannel",
        PROBE_SESSION,
        &[(SESSION_OWNER, OwnerId)],
    ),
    // Encrypted with an environment key, never the default
    // `aws/secretsmanager` key (which can't carry `triple-c:component`).
    write(
        "secretsmanager:CreateSecret",
        PROBE_SECRET,
        &[
            (OWNER_REQUEST_TAG, OwnerId),
            ("secretsmanager:KmsKeyId", Literal(PROBE_KEY)),
        ],
    ),
    // Tag-on-create: the secret has no tags yet, so only the request tag.
    write(
        "secretsmanager:TagResource",
        PROBE_SECRET,
        &[(OWNER_REQUEST_TAG, OwnerId)],
    ),
    write(
        "secretsmanager:PutSecretValue",
        PROBE_SECRET,
        &[(OWNER_RESOURCE_TAG, OwnerId)],
    ),
    write(
        "secretsmanager:DeleteSecret",
        PROBE_SECRET,
        &[(OWNER_RESOURCE_TAG, OwnerId)],
    ),
    // Only on the environment's (component-tagged) file system.
    write(
        "elasticfilesystem:CreateAccessPoint",
        PROBE_FILE_SYSTEM,
        &[
            (OWNER_REQUEST_TAG, OwnerId),
            (COMPONENT_RESOURCE_TAG, Literal("environment")),
        ],
    ),
    write(
        "elasticfilesystem:TagResource",
        PROBE_ACCESS_POINT,
        &[
            (OWNER_REQUEST_TAG, OwnerId),
            (
                "elasticfilesystem:CreateAction",
                Literal("CreateAccessPoint"),
            ),
        ],
    ),
    write(
        "elasticfilesystem:DeleteAccessPoint",
        PROBE_ACCESS_POINT,
        &[(OWNER_RESOURCE_TAG, OwnerId)],
    ),
    write(
        "kms:Encrypt",
        PROBE_KEY,
        &[
            (
                "kms:ViaService",
                Literal("secretsmanager.{region}.amazonaws.com"),
            ),
            (COMPONENT_RESOURCE_TAG, Literal("environment")),
        ],
    ),
    write(
        "kms:GenerateDataKey",
        PROBE_KEY,
        &[
            (
                "kms:ViaService",
                Literal("secretsmanager.{region}.amazonaws.com"),
            ),
            (COMPONENT_RESOURCE_TAG, Literal("environment")),
        ],
    ),
    // CreateSecret and PutSecretValue on a customer key also need Decrypt
    // (verified live: without it, "Access to KMS is not allowed").
    write(
        "kms:Decrypt",
        PROBE_KEY,
        &[
            (
                "kms:ViaService",
                Literal("secretsmanager.{region}.amazonaws.com"),
            ),
            (COMPONENT_RESOURCE_TAG, Literal("environment")),
        ],
    ),
    read("iam:GetRole", "{principal}", OWN_PRINCIPAL),
    read("iam:SimulatePrincipalPolicy", "{principal}", OWN_PRINCIPAL),
    read("ecs:DescribeClusters", PROBE_CLUSTER, &[]),
    read("ecs:ListClusters", "*", &[]),
    read("ssm:GetParametersByPath", PROBE_PARAMETER, &[]),
    read("cloudformation:DescribeStacks", "*", &[]),
    read("cloudformation:ListStacks", "*", &[]),
    read("secretsmanager:ListSecrets", "*", &[]),
    read("elasticfilesystem:DescribeAccessPoints", "*", &[]),
    read(
        "logs:GetLogEvents",
        "arn:aws:logs:{region}:{account}:log-group:/triple-c/probe:log-stream:probe",
        &[],
    ),
    read("logs:FilterLogEvents", PROBE_LOG_GROUP, &[]),
    read("sts:GetCallerIdentity", "*", &[]),
];

const ENVIRONMENT_ADMIN_CHECKS: &[Check] = &[
    write("cloudformation:CreateStack", PROBE_STACK, &[]),
    write("cloudformation:UpdateStack", PROBE_STACK, &[]),
    write("cloudformation:DeleteStack", PROBE_STACK, &[]),
    write("iam:CreateRole", PROBE_ROLE, WITH_BOUNDARY),
    write("iam:PutRolePolicy", PROBE_ROLE, WITH_BOUNDARY),
    write("iam:DeleteRolePolicy", PROBE_ROLE, WITH_BOUNDARY),
    write("iam:AttachRolePolicy", PROBE_ROLE, WITH_BOUNDARY),
    write("iam:DetachRolePolicy", PROBE_ROLE, WITH_BOUNDARY),
    write("iam:DeleteRole", PROBE_ROLE, &[]),
    write("iam:TagRole", PROBE_ROLE, &[]),
    read("iam:GetRole", PROBE_ROLE, &[]),
    write(
        "iam:PassRole",
        PROBE_ROLE,
        &[("iam:PassedToService", Literal("ecs-tasks.amazonaws.com"))],
    ),
    read("iam:GetRole", "{principal}", OWN_PRINCIPAL),
    read("iam:SimulatePrincipalPolicy", "{principal}", OWN_PRINCIPAL),
    write("ecs:CreateCluster", PROBE_CLUSTER, &[]),
    write("ecs:DeleteCluster", PROBE_CLUSTER, &[]),
    write("ecs:PutClusterCapacityProviders", PROBE_CLUSTER, &[]),
    write(
        "ecs:CreateCapacityProvider",
        "arn:aws:ecs:{region}:{account}:capacity-provider/triple-c-probe",
        &[],
    ),
    write(
        "ecs:DeleteCapacityProvider",
        "arn:aws:ecs:{region}:{account}:capacity-provider/triple-c-probe",
        &[],
    ),
    write("ecs:RegisterTaskDefinition", "*", &[]),
    write("ecs:DeregisterTaskDefinition", "*", &[]),
    // CreateSecurityGroup is authorized against the new group and the VPC.
    write(
        "ec2:CreateSecurityGroup",
        PROBE_SECURITY_GROUP,
        COMPONENT_REQUESTED,
    ),
    write("ec2:CreateSecurityGroup", PROBE_VPC, &[]),
    write(
        "ec2:CreateTags",
        PROBE_SECURITY_GROUP,
        &[("ec2:CreateAction", Literal("CreateSecurityGroup"))],
    ),
    write("ec2:CreateTags", PROBE_SECURITY_GROUP, COMPONENT_TAGGED),
    write(
        "ec2:DeleteSecurityGroup",
        PROBE_SECURITY_GROUP,
        COMPONENT_TAGGED,
    ),
    write(
        "ec2:AuthorizeSecurityGroupIngress",
        PROBE_SECURITY_GROUP,
        COMPONENT_TAGGED,
    ),
    write(
        "ec2:RevokeSecurityGroupIngress",
        PROBE_SECURITY_GROUP,
        COMPONENT_TAGGED,
    ),
    read("ec2:DescribeSecurityGroups", "*", &[]),
    // CreateFileSystem has no resource type: IAM authorizes it against `*`
    // (verified live; a `file-system/*` Resource never matches).
    write(
        "elasticfilesystem:CreateFileSystem",
        "*",
        COMPONENT_REQUESTED,
    ),
    write(
        "elasticfilesystem:TagResource",
        PROBE_FILE_SYSTEM,
        &[(
            "elasticfilesystem:CreateAction",
            Literal("CreateFileSystem"),
        )],
    ),
    write(
        "elasticfilesystem:TagResource",
        PROBE_FILE_SYSTEM,
        COMPONENT_TAGGED,
    ),
    write(
        "elasticfilesystem:DeleteFileSystem",
        PROBE_FILE_SYSTEM,
        COMPONENT_TAGGED,
    ),
    write(
        "elasticfilesystem:CreateMountTarget",
        PROBE_FILE_SYSTEM,
        COMPONENT_TAGGED,
    ),
    write(
        "elasticfilesystem:DeleteMountTarget",
        PROBE_FILE_SYSTEM,
        COMPONENT_TAGGED,
    ),
    write(
        "elasticfilesystem:PutFileSystemPolicy",
        PROBE_FILE_SYSTEM,
        COMPONENT_TAGGED,
    ),
    write("kms:CreateKey", "*", COMPONENT_REQUESTED),
    write("kms:ScheduleKeyDeletion", PROBE_KEY, COMPONENT_TAGGED),
    write("kms:PutKeyPolicy", PROBE_KEY, COMPONENT_TAGGED),
    write("kms:TagResource", PROBE_KEY, COMPONENT_TAGGED),
    // An alias call is authorized against both the alias and the key.
    write("kms:CreateAlias", PROBE_ALIAS, &[]),
    write("kms:CreateAlias", PROBE_KEY, COMPONENT_TAGGED),
    write("kms:DeleteAlias", PROBE_ALIAS, &[]),
    write("kms:DeleteAlias", PROBE_KEY, COMPONENT_TAGGED),
    write("logs:CreateLogGroup", PROBE_LOG_GROUP, &[]),
    write("logs:DeleteLogGroup", PROBE_LOG_GROUP, &[]),
    write("logs:PutRetentionPolicy", PROBE_LOG_GROUP, &[]),
    write(
        "ecr:CreateRepository",
        "arn:aws:ecr:{region}:{account}:repository/triple-c/probe",
        &[],
    ),
    write(
        "ecr:DeleteRepository",
        "arn:aws:ecr:{region}:{account}:repository/triple-c/probe",
        &[],
    ),
    write("ecr:CreatePullThroughCacheRule", "*", &[]),
    write("ssm:PutParameter", PROBE_PARAMETER, &[]),
    write("ssm:DeleteParameter", PROBE_PARAMETER, &[]),
];

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;
    use std::collections::BTreeSet;

    const AT: Placeholders = Placeholders {
        region: "us-west-2",
        account: "111122223333",
        principal: "arn:aws:iam::111122223333:role/aws-reserved/sso.amazonaws.com/us-west-2/AWSReservedSSO_Dev_0123456789abcdef",
    };
    const ME: &str = "AROAEXAMPLEME:alice@example.com";
    const SOMEONE_ELSE: &str = "AROAEXAMPLEOTHER:bob@example.com";

    const TIERS: [Tier; 2] = [Tier::WorkspaceUser, Tier::EnvironmentAdmin];

    /// IAM wildcard match: `*` is any run of characters (including `:` and
    /// `/`), `?` is exactly one.
    fn glob(pattern: &str, text: &str) -> bool {
        let p: Vec<char> = pattern.chars().collect();
        let t: Vec<char> = text.chars().collect();
        let (mut pi, mut ti) = (0, 0);
        let (mut star, mut mark) = (None, 0);
        while ti < t.len() {
            if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
                pi += 1;
                ti += 1;
            } else if pi < p.len() && p[pi] == '*' {
                star = Some(pi);
                mark = ti;
                pi += 1;
            } else if let Some(s) = star {
                pi = s + 1;
                mark += 1;
                ti = mark;
            } else {
                return false;
            }
        }
        while pi < p.len() && p[pi] == '*' {
            pi += 1;
        }
        pi == p.len()
    }

    #[test]
    fn glob_behaves_like_iam_wildcards() {
        assert!(glob("ecs:Describe*", "ecs:DescribeClusters"));
        assert!(!glob("ecs:Describe*", "ecs:ListClusters"));
        assert!(glob(
            "arn:aws:ecs:*:*:task/triple-c-*/*",
            "arn:aws:ecs:us-west-2:1:task/triple-c-x/abc"
        ));
        assert!(!glob(
            "arn:aws:ecs:*:*:task/triple-c-*/*",
            "arn:aws:ecs:us-west-2:1:task/other/abc"
        ));
        assert!(glob("a?c", "abc"));
        assert!(!glob("a?c", "ac"));
    }

    fn parse(name: &str, json: &str) -> Value {
        serde_json::from_str(json).unwrap_or_else(|e| panic!("{name} is not JSON: {e}"))
    }

    fn policy(tier: Tier) -> Value {
        parse(&format!("{tier:?} policy"), policy_json(tier))
    }

    fn boundary() -> Value {
        parse("permissions boundary", permissions_boundary_json())
    }

    fn statements_of(doc: &Value) -> Vec<Value> {
        doc["Statement"]
            .as_array()
            .expect("policy has no Statement array")
            .clone()
    }

    fn statements(tier: Tier) -> Vec<Value> {
        statements_of(&policy(tier))
    }

    fn effect(stmt: &Value) -> &str {
        stmt["Effect"].as_str().unwrap_or("<no Effect>")
    }

    fn allows(tier: Tier) -> Vec<Value> {
        statements(tier)
            .into_iter()
            .filter(|s| effect(s) == "Allow")
            .collect()
    }

    fn denies(tier: Tier) -> Vec<Value> {
        statements(tier)
            .into_iter()
            .filter(|s| effect(s) == "Deny")
            .collect()
    }

    /// `Action` / `Resource` / condition values: a string or an array of them.
    fn strings(v: &Value) -> Vec<String> {
        match v {
            Value::String(s) => vec![s.clone()],
            Value::Array(a) => a
                .iter()
                .map(|x| x.as_str().expect("expected a string").to_string())
                .collect(),
            other => panic!("expected a string or array, got {other}"),
        }
    }

    fn actions(stmt: &Value) -> Vec<String> {
        strings(&stmt["Action"])
    }

    fn resources(stmt: &Value) -> Vec<String> {
        strings(&stmt["Resource"])
    }

    fn sid(stmt: &Value) -> &str {
        stmt["Sid"].as_str().unwrap_or("<no Sid>")
    }

    fn rendered(check: &Check) -> String {
        render(check.resource, &AT)
    }

    /// The request: who calls, and whose the resource / request tag is.
    #[derive(Clone, Copy)]
    struct Request<'a> {
        check: &'a Check,
        /// `${aws:userid}` of the caller.
        caller: &'a str,
        /// What `OwnerId` context entries carry.
        owner: &'a str,
    }

    impl Request<'_> {
        fn context_value(&self, key: &str) -> Option<String> {
            self.context_values(key).into_iter().next()
        }

        /// All values of a (possibly multi-valued) key.
        fn context_values(&self, key: &str) -> Vec<String> {
            self.check
                .context
                .iter()
                .filter(|(k, _)| *k == key)
                .map(|(_, v)| match v {
                    ContextValue::OwnerId => self.owner.to_string(),
                    ContextValue::Literal(l) => render(l, &AT),
                })
                .collect()
        }

        /// Resolves `${…}` policy variables the way the simulator does: from
        /// the request context. An unresolvable variable makes the string
        /// match nothing (`None`).
        fn resolve(&self, s: &str) -> Option<String> {
            let mut out = String::new();
            let mut rest = s;
            while let Some(start) = rest.find("${") {
                out.push_str(&rest[..start]);
                let end = rest[start..].find('}').expect("unterminated ${") + start;
                let name = &rest[start + 2..end];
                let value = if name == "aws:userid" {
                    Some(self.caller.to_string())
                } else {
                    self.context_value(name)
                };
                out.push_str(&value?);
                rest = &rest[end + 1..];
            }
            out.push_str(rest);
            Some(out)
        }
    }

    /// Statements that name the action and one of whose resources matches
    /// the rendered resource.
    fn matching<'s>(stmts: &'s [Value], req: &Request) -> Vec<&'s Value> {
        let resource = rendered(req.check);
        stmts
            .iter()
            .filter(|s| actions(s).iter().any(|a| glob(a, req.check.action)))
            .filter(|s| {
                resources(s)
                    .iter()
                    .filter_map(|r| req.resolve(r))
                    .any(|r| glob(&r, &resource))
            })
            .collect()
    }

    /// A deliberately small IAM condition evaluator: the operators the
    /// shipped policies use, and nothing else (an unknown operator fails the
    /// test rather than being ignored).
    fn conditions_hold(stmt: &Value, req: &Request) -> bool {
        let Some(cond) = stmt.get("Condition") else {
            return true;
        };
        let cond = cond.as_object().expect("Condition is an object");
        cond.iter().all(|(op, entries)| {
            let entries = entries.as_object().expect("condition block is an object");
            // ForAllValues: every value of the (multi-valued) key matches
            // one of the policy's values. Vacuously true when the key is
            // absent, which is why statements pair it with a required key.
            if let Some(base) = op.strip_prefix("ForAllValues:") {
                assert_eq!(
                    base,
                    "StringEquals",
                    "{}: unsupported ForAllValues operator",
                    sid(stmt)
                );
                return entries.iter().all(|(key, want)| {
                    let allowed: Vec<String> = strings(want)
                        .iter()
                        .filter_map(|p| req.resolve(p))
                        .collect();
                    req.context_values(key).iter().all(|v| allowed.contains(v))
                });
            }
            if op == "Null" {
                return entries.iter().all(|(key, want)| {
                    let absent = req.context_value(key).is_none();
                    match want.as_str() {
                        Some("true") => absent,
                        Some("false") => !absent,
                        _ => panic!("{}: Null takes \"true\" or \"false\"", sid(stmt)),
                    }
                });
            }
            let (base, if_exists) = match op.strip_suffix("IfExists") {
                Some(b) => (b, true),
                None => (op.as_str(), false),
            };
            // A negated operator holds when the key is absent, and
            // otherwise when no value matches.
            let (matches, negated): (fn(&str, &str) -> bool, bool) = match base {
                "StringEquals" => (|p, v| p == v, false),
                "StringLike" | "ArnLike" => (glob, false),
                "StringNotEquals" => (|p, v| p == v, true),
                "StringNotLike" => (glob, true),
                other => panic!("{}: unsupported condition operator {other}", sid(stmt)),
            };
            entries
                .iter()
                .all(|(key, want)| match req.context_value(key) {
                    None => if_exists || negated,
                    Some(value) => {
                        let any = strings(want)
                            .iter()
                            .filter_map(|p| req.resolve(p))
                            .any(|p| matches(&p, &value));
                        any != negated
                    }
                })
        })
    }

    fn evaluate(tier: Tier, req: Request) -> bool {
        evaluate_in(&statements(tier), req)
    }

    /// Allowed by some Allow and by no Deny among `stmts`.
    fn evaluate_in(stmts: &[Value], req: Request) -> bool {
        let applies = |s: &&Value| conditions_hold(s, &req);
        let allowed = matching(stmts, &req)
            .into_iter()
            .filter(|s| effect(s) == "Allow")
            .any(|s| applies(&s));
        let denied = matching(stmts, &req)
            .into_iter()
            .filter(|s| effect(s) == "Deny")
            .any(|s| applies(&s));
        allowed && !denied
    }

    fn allowed(tier: Tier, check: &Check, caller: &str, owner: &str) -> bool {
        evaluate(
            tier,
            Request {
                check,
                caller,
                owner,
            },
        )
    }

    fn validate_document(name: &str, doc: &Value) {
        assert_eq!(doc["Version"], "2012-10-17", "{name}");
        let stmts = statements_of(doc);
        assert!(!stmts.is_empty(), "{name} has no statements");
        let mut sids = BTreeSet::new();
        for s in &stmts {
            let id = s["Sid"]
                .as_str()
                .unwrap_or_else(|| panic!("{name}: statement without Sid: {s}"));
            assert!(
                !id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric()),
                "{name}: Sid {id:?} must be non-empty alphanumeric"
            );
            assert!(sids.insert(id.to_string()), "{name}: duplicate Sid {id}");
            assert!(
                ["Allow", "Deny"].contains(&effect(s)),
                "{name} {id}: Effect must be Allow or Deny"
            );
            assert!(!actions(s).is_empty(), "{name} {id}: no Action");
            assert!(!resources(s).is_empty(), "{name} {id}: no Resource");
            for key in s.as_object().unwrap().keys() {
                assert!(
                    ["Sid", "Effect", "Action", "Resource", "Condition"].contains(&key.as_str()),
                    "{name} {id}: unexpected key {key} (NotAction/NotResource are not allowed)"
                );
            }
        }
    }

    #[test]
    fn policies_are_valid_iam_json() {
        for tier in TIERS {
            validate_document(&format!("{tier:?}"), &policy(tier));
            assert!(!allows(tier).is_empty(), "{tier:?} allows nothing");
        }
        validate_document("permissions boundary", &boundary());
    }

    #[test]
    fn every_policy_action_has_a_check_and_vice_versa() {
        for tier in TIERS {
            let policy_actions: BTreeSet<String> = allows(tier).iter().flat_map(actions).collect();
            for a in &policy_actions {
                assert!(
                    checks(tier).iter().any(|c| glob(a, c.action)),
                    "{tier:?}: policy action {a} has no check"
                );
            }
            for c in checks(tier) {
                assert!(
                    policy_actions.iter().any(|a| glob(a, c.action)),
                    "{tier:?}: check {} is for an action the policy does not grant",
                    c.action
                );
                assert!(
                    !c.action.contains('*'),
                    "{tier:?}: check action {} must be concrete",
                    c.action
                );
            }
        }
    }

    #[test]
    fn every_check_resource_matches_a_statement_resource() {
        for tier in TIERS {
            let stmts = allows(tier);
            for c in checks(tier) {
                let req = Request {
                    check: c,
                    caller: ME,
                    owner: ME,
                };
                assert!(
                    !matching(&stmts, &req).is_empty(),
                    "{tier:?}: {} on {} matches no statement resource",
                    c.action,
                    rendered(c)
                );
                assert!(
                    !rendered(c).contains('{'),
                    "{tier:?}: unsubstituted placeholder in {}",
                    c.resource
                );
            }
        }
    }

    /// The owner-scoped actions, and the condition key each statement must
    /// carry the owner in.
    const OWNER_SCOPED: &[(&str, &str, &str)] = &[
        ("ecs:RunTask", "task-definition/", OWNER_REQUEST_TAG),
        ("ecs:RunTask", "task-definition/", OWNER_RESOURCE_TAG),
        ("ecs:TagResource", "task/", OWNER_REQUEST_TAG),
        ("ecs:StopTask", "task/", OWNER_RESOURCE_TAG),
        ("ecs:ExecuteCommand", "task/", OWNER_RESOURCE_TAG),
        ("ssm:StartSession", "task/", OWNER_RESOURCE_TAG),
        ("ssm:TerminateSession", "session/", SESSION_OWNER),
        ("ssm:ResumeSession", "session/", SESSION_OWNER),
        ("iam:PassRole", "role/", "iam:ResourceTag/triple-c:owner"),
        ("secretsmanager:CreateSecret", "secret:", OWNER_REQUEST_TAG),
        ("secretsmanager:TagResource", "secret:", OWNER_REQUEST_TAG),
        (
            "secretsmanager:PutSecretValue",
            "secret:",
            OWNER_RESOURCE_TAG,
        ),
        ("secretsmanager:DeleteSecret", "secret:", OWNER_RESOURCE_TAG),
        (
            "elasticfilesystem:CreateAccessPoint",
            "file-system/",
            OWNER_REQUEST_TAG,
        ),
        (
            "elasticfilesystem:TagResource",
            "access-point/",
            OWNER_REQUEST_TAG,
        ),
        (
            "elasticfilesystem:DeleteAccessPoint",
            "access-point/",
            OWNER_RESOURCE_TAG,
        ),
    ];

    fn owner_scoped_checks() -> Vec<(&'static Check, &'static str)> {
        OWNER_SCOPED
            .iter()
            .map(|(action, res, key)| {
                let check = checks(Tier::WorkspaceUser)
                    .iter()
                    .find(|c| c.action == *action && c.resource.contains(res))
                    .unwrap_or_else(|| {
                        panic!("no workspace-user check for {action} on a {res} resource")
                    });
                (check, *key)
            })
            .collect()
    }

    #[test]
    fn owner_scoped_actions_carry_the_owner_context() {
        for (check, key) in owner_scoped_checks() {
            assert!(
                check.context.contains(&(key, ContextValue::OwnerId)),
                "{} must send ({key}, OwnerId); has {:?}",
                check.action,
                check.context
            );
            // ...and that is the key its statement's condition uses.
            let stmts = allows(Tier::WorkspaceUser);
            let req = Request {
                check,
                caller: ME,
                owner: ME,
            };
            let found = matching(&stmts, &req);
            assert!(!found.is_empty(), "{}: no statement", check.action);
            assert!(
                found
                    .iter()
                    .all(|s| s["Condition"]
                        .as_object()
                        .is_some_and(|ops| ops.values().any(|e| e
                            .get(key)
                            .is_some_and(|v| strings(v).contains(&"${aws:userid}".to_string()))))),
                "{}: every matching statement must condition {key} on ${{aws:userid}}",
                check.action
            );
        }
    }

    #[test]
    fn every_check_is_allowed_by_its_policy() {
        for tier in TIERS {
            for c in checks(tier) {
                assert!(
                    allowed(tier, c, ME, ME),
                    "{tier:?}: {} on {} with {:?} is not allowed",
                    c.action,
                    rendered(c),
                    c.context
                );
            }
        }
    }

    /// The spec's most important rule: nobody may stop, exec into, open a
    /// session to, pass the role of, or touch the secrets or access points
    /// of a workspace they don't own.
    #[test]
    fn owner_scoped_checks_are_denied_for_someone_elses_resource() {
        for (check, _) in owner_scoped_checks() {
            assert!(
                !allowed(Tier::WorkspaceUser, check, ME, SOMEONE_ELSE),
                "{} on {} is allowed for a resource owned by someone else",
                check.action,
                rendered(check)
            );
        }
    }

    #[test]
    fn owner_scoped_checks_are_denied_without_an_owner_tag() {
        for (check, _) in owner_scoped_checks() {
            let untagged = Check {
                context: &[],
                ..*check
            };
            assert!(
                !allowed(Tier::WorkspaceUser, &untagged, ME, ME),
                "{} is allowed without any owner context",
                check.action
            );
        }
    }

    #[test]
    fn run_task_needs_an_owned_task_definition() {
        let run = checks(Tier::WorkspaceUser)
            .iter()
            .find(|c| c.action == "ecs:RunTask")
            .expect("ecs:RunTask check");
        let someone_elses_definition = Check {
            context: &[
                (OWNER_REQUEST_TAG, OwnerId),
                (OWNER_RESOURCE_TAG, Literal(SOMEONE_ELSE)),
                ("ecs:cluster", Literal(PROBE_CLUSTER)),
            ],
            ..*run
        };
        assert!(!allowed(
            Tier::WorkspaceUser,
            &someone_elses_definition,
            ME,
            ME
        ));
        let other_cluster = Check {
            context: &[
                (OWNER_REQUEST_TAG, OwnerId),
                (OWNER_RESOURCE_TAG, OwnerId),
                (
                    "ecs:cluster",
                    Literal("arn:aws:ecs:{region}:{account}:cluster/production"),
                ),
            ],
            ..*run
        };
        assert!(!allowed(Tier::WorkspaceUser, &other_cluster, ME, ME));
    }

    #[test]
    fn ecs_tag_resource_only_tags_at_run_task() {
        let tag = checks(Tier::WorkspaceUser)
            .iter()
            .find(|c| c.action == "ecs:TagResource")
            .expect("ecs:TagResource check");
        let retag = Check {
            context: &[(OWNER_REQUEST_TAG, OwnerId)],
            ..*tag
        };
        assert!(
            !allowed(Tier::WorkspaceUser, &retag, ME, ME),
            "ecs:TagResource must require ecs:CreateAction"
        );
        let other_create = Check {
            context: &[
                (OWNER_REQUEST_TAG, OwnerId),
                ("ecs:CreateAction", Literal("CreateService")),
            ],
            ..*tag
        };
        assert!(!allowed(Tier::WorkspaceUser, &other_create, ME, ME));
    }

    #[test]
    fn workspace_pass_role_is_only_to_ecs_tasks() {
        let pass = checks(Tier::WorkspaceUser)
            .iter()
            .find(|c| c.action == "iam:PassRole")
            .expect("iam:PassRole check");
        let to_ec2 = Check {
            context: &[
                ("iam:ResourceTag/triple-c:owner", OwnerId),
                ("iam:PassedToService", Literal("ec2.amazonaws.com")),
            ],
            ..*pass
        };
        assert!(!allowed(Tier::WorkspaceUser, &to_ec2, ME, ME));
        let not_triple_c = Check {
            resource: "arn:aws:iam::{account}:role/Admin",
            ..*pass
        };
        assert!(!allowed(Tier::WorkspaceUser, &not_triple_c, ME, ME));
    }

    #[test]
    fn workspace_kms_is_only_through_secrets_manager() {
        for c in checks(Tier::WorkspaceUser)
            .iter()
            .filter(|c| c.action.starts_with("kms:"))
        {
            let direct = Check { context: &[], ..*c };
            assert!(
                !allowed(Tier::WorkspaceUser, &direct, ME, ME),
                "{} allowed without kms:ViaService",
                c.action
            );
            let via_s3 = Check {
                context: &[("kms:ViaService", Literal("s3.{region}.amazonaws.com"))],
                ..*c
            };
            assert!(
                !allowed(Tier::WorkspaceUser, &via_s3, ME, ME),
                "{} allowed via S3",
                c.action
            );
        }
    }

    /// GetRole and SimulatePrincipalPolicy: an IAM user only on itself. IAM
    /// has no role-name variable and refuses `${aws:PrincipalArn}` as a
    /// Resource (verified live), so roles are limited by path and name
    /// instead (R18): SSO permission-set roles and `triple-c-*` roles only.
    #[test]
    fn principal_inspection_is_of_the_caller_sso_roles_or_triple_c_roles() {
        for tier in TIERS {
            for action in ["iam:GetRole", "iam:SimulatePrincipalPolicy"] {
                let own = checks(tier)
                    .iter()
                    .find(|c| c.action == action && c.resource == "{principal}")
                    .unwrap_or_else(|| panic!("{tier:?}: no own-principal check for {action}"));
                assert!(allowed(tier, own, ME, ME));
                let as_alice: Context = &[("aws:username", Literal("alice"))];
                let at = |resource: &'static str| Check {
                    resource,
                    context: as_alice,
                    ..*own
                };
                for mine in [
                    "arn:aws:iam::{account}:user/alice",
                    "arn:aws:iam::{account}:user/team/alice",
                    "arn:aws:iam::{account}:role/aws-reserved/sso.amazonaws.com/us-west-2/AWSReservedSSO_Dev_0123",
                    "arn:aws:iam::{account}:role/triple-c-task-alice",
                ] {
                    assert!(
                        allowed(tier, &at(mine), ME, ME),
                        "{tier:?}: {action} on {mine}"
                    );
                }
                for theirs in [
                    "arn:aws:iam::{account}:role/SomeoneElsesRole",
                    "arn:aws:iam::{account}:role/ops/deployer",
                    "arn:aws:iam::{account}:role/aws-service-role/ecs.amazonaws.com/AWSServiceRoleForECS",
                    "arn:aws:iam::{account}:user/bob",
                    "arn:aws:iam::{account}:user/team/xalice",
                    "arn:aws:iam::{account}:group/alice",
                ] {
                    assert!(
                        !allowed(tier, &at(theirs), ME, ME),
                        "{tier:?}: {action} allowed on {theirs}"
                    );
                }
            }
        }
    }

    /// IAM rejects a Resource that is not `*` or an ARN, even one that is a
    /// policy variable resolving to an ARN (`${aws:PrincipalArn}`, verified
    /// live: "Resource ${aws:PrincipalArn} must be in ARN format or \"*\"").
    #[test]
    fn every_resource_is_an_arn_or_star() {
        let docs = [
            ("workspace-user", policy(Tier::WorkspaceUser)),
            ("environment-admin", policy(Tier::EnvironmentAdmin)),
            (
                "permissions-boundary",
                serde_json::from_str(permissions_boundary_json()).unwrap(),
            ),
        ];
        for (name, doc) in &docs {
            for s in statements_of(doc) {
                for r in resources(&s) {
                    assert!(
                        r == "*" || r.starts_with("arn:"),
                        "{name} {}: Resource {r} is not an ARN",
                        sid(&s)
                    );
                }
            }
        }
    }

    /// A customer managed policy may hold 6,144 characters, not counting
    /// whitespace (verified live: the workspace policy is close). An
    /// environment admin needs both tier policies, which together exceed a
    /// role's 10,240-character inline budget, so they ship as managed
    /// policies.
    #[test]
    fn every_policy_fits_a_managed_policy() {
        for (name, text) in [
            ("workspace-user", policy_json(Tier::WorkspaceUser)),
            ("environment-admin", policy_json(Tier::EnvironmentAdmin)),
            ("permissions-boundary", permissions_boundary_json()),
        ] {
            let size = text.chars().filter(|c| !c.is_whitespace()).count();
            assert!(
                size <= 6144,
                "{name} is {size} characters without whitespace (max 6144)"
            );
        }
    }

    const ROLE_WRITES: &[&str] = &[
        "iam:CreateRole",
        "iam:PutRolePolicy",
        "iam:AttachRolePolicy",
        "iam:DetachRolePolicy",
        "iam:DeleteRolePolicy",
    ];
    /// Role and policy changes no tier may make at all.
    const NEVER_ALLOWED: &[&str] = &[
        "iam:PutRolePermissionsBoundary",
        "iam:DeleteRolePermissionsBoundary",
        "iam:UpdateAssumeRolePolicy",
        "iam:CreatePolicy",
        "iam:CreatePolicyVersion",
        "iam:DeletePolicy",
        "iam:DeletePolicyVersion",
        "iam:SetDefaultPolicyVersion",
        "iam:CreateUser",
        "iam:AttachUserPolicy",
        "iam:PutUserPolicy",
    ];

    #[test]
    fn admin_role_management_requires_the_boundary() {
        let tier = Tier::EnvironmentAdmin;
        let boundary_arn =
            format!("arn:aws:iam::${{aws:PrincipalAccount}}:policy/{PERMISSIONS_BOUNDARY_NAME}");
        // Every statement granting a role write pins the boundary.
        for s in allows(tier) {
            if !actions(&s)
                .iter()
                .any(|a| ROLE_WRITES.iter().any(|w| glob(a, w)))
            {
                continue;
            }
            let pinned = s["Condition"]["StringEquals"]["iam:PermissionsBoundary"].clone();
            assert_eq!(
                pinned,
                Value::String(boundary_arn.clone()),
                "{}: role writes must require iam:PermissionsBoundary = {boundary_arn}",
                sid(&s)
            );
        }
        for action in ROLE_WRITES {
            let c = checks(tier)
                .iter()
                .find(|c| c.action == *action)
                .unwrap_or_else(|| panic!("no check for {action}"));
            assert!(
                c.context
                    .iter()
                    .any(|(k, _)| *k == "iam:PermissionsBoundary"),
                "{action} check must send iam:PermissionsBoundary"
            );
            for context in [
                &[][..],
                &[
                    (
                        "iam:PermissionsBoundary",
                        Literal("arn:aws:iam::{account}:policy/SomethingElse"),
                    ),
                    ("aws:PrincipalAccount", Literal("{account}")),
                ][..],
                &[
                    (
                        "iam:PermissionsBoundary",
                        Literal("arn:aws:iam::999999999999:policy/triple-c-permissions-boundary"),
                    ),
                    ("aws:PrincipalAccount", Literal("{account}")),
                ][..],
            ] {
                let unbounded = Check { context, ..*c };
                assert!(
                    !allowed(tier, &unbounded, ME, ME),
                    "{action} allowed with boundary context {context:?}"
                );
            }
        }
        // Nothing grants the escape hatches...
        for t in TIERS {
            for s in allows(t) {
                for a in actions(&s) {
                    for never in NEVER_ALLOWED {
                        assert!(!glob(&a, never), "{t:?} {}: {a} grants {never}", sid(&s));
                    }
                }
            }
        }
        // ...and explicit Denies hold even if another policy grants them.
        let denied = |action: &'static str, resource: &'static str| {
            let c = Check {
                action,
                resource,
                context: &[],
                write: true,
            };
            let stmts = denies(tier);
            let req = Request {
                check: &c,
                caller: ME,
                owner: ME,
            };
            matching(&stmts, &req)
                .iter()
                .any(|s| conditions_hold(s, &req))
        };
        assert!(
            denied("iam:DeleteRolePermissionsBoundary", PROBE_ROLE),
            "missing Deny of iam:DeleteRolePermissionsBoundary on triple-c roles"
        );
        for action in [
            "iam:CreatePolicyVersion",
            "iam:DeletePolicy",
            "iam:DeletePolicyVersion",
            "iam:SetDefaultPolicyVersion",
        ] {
            assert!(
                denied(
                    action,
                    "arn:aws:iam::{account}:policy/triple-c-permissions-boundary"
                ),
                "missing Deny of {action} on the boundary policy"
            );
        }
    }

    #[test]
    fn destructive_admin_actions_are_tag_scoped() {
        let tier = Tier::EnvironmentAdmin;
        const CREATES: &[(&str, &str)] = &[
            ("kms:CreateKey", "*"),
            ("ec2:CreateSecurityGroup", PROBE_SECURITY_GROUP),
            ("elasticfilesystem:CreateFileSystem", "*"),
        ];
        const DESTROYS: &[(&str, &str)] = &[
            ("kms:ScheduleKeyDeletion", PROBE_KEY),
            ("kms:PutKeyPolicy", PROBE_KEY),
            ("kms:TagResource", PROBE_KEY),
            ("kms:CreateAlias", PROBE_KEY),
            ("kms:DeleteAlias", PROBE_KEY),
            ("ec2:DeleteSecurityGroup", PROBE_SECURITY_GROUP),
            ("ec2:AuthorizeSecurityGroupIngress", PROBE_SECURITY_GROUP),
            ("ec2:RevokeSecurityGroupIngress", PROBE_SECURITY_GROUP),
            ("ec2:CreateTags", PROBE_SECURITY_GROUP),
            ("elasticfilesystem:DeleteFileSystem", PROBE_FILE_SYSTEM),
            ("elasticfilesystem:PutFileSystemPolicy", PROBE_FILE_SYSTEM),
            ("elasticfilesystem:CreateMountTarget", PROBE_FILE_SYSTEM),
            ("elasticfilesystem:DeleteMountTarget", PROBE_FILE_SYSTEM),
            ("elasticfilesystem:TagResource", PROBE_FILE_SYSTEM),
        ];
        for (list, context) in [(CREATES, COMPONENT_REQUESTED), (DESTROYS, COMPONENT_TAGGED)] {
            for (action, resource) in list {
                let c = checks(tier)
                    .iter()
                    .find(|c| c.action == *action && c.resource == *resource)
                    .unwrap_or_else(|| panic!("no admin check for {action} on {resource}"));
                if c.context == context {
                    assert!(allowed(tier, c, ME, ME), "{action} with {context:?}");
                }
                // Without the component tag, it's someone else's resource.
                let untagged = Check { context: &[], ..*c };
                assert!(
                    !allowed(tier, &untagged, ME, ME),
                    "{action} on {resource} is allowed without a triple-c:component tag"
                );
            }
        }
        // Tagging is allowed on create only through the creating call.
        for (action, key, wrong) in [
            ("ec2:CreateTags", "ec2:CreateAction", "RunInstances"),
            (
                "elasticfilesystem:TagResource",
                "elasticfilesystem:CreateAction",
                "CreateAccessPoint",
            ),
        ] {
            let c = checks(tier)
                .iter()
                .find(|c| c.action == action && c.context.iter().any(|(k, _)| *k == key))
                .unwrap_or_else(|| panic!("no tag-on-create check for {action}"));
            let ctx: Context = match action {
                "ec2:CreateTags" => &[("ec2:CreateAction", Literal("RunInstances"))],
                _ => &[(
                    "elasticfilesystem:CreateAction",
                    Literal("CreateAccessPoint"),
                )],
            };
            let other_create = Check { context: ctx, ..*c };
            assert!(
                !allowed(tier, &other_create, ME, ME),
                "{action} allowed with {key} = {wrong}"
            );
        }
    }

    #[test]
    fn the_boundary_grants_what_tasks_need_and_nothing_iam() {
        let doc = boundary();
        let stmts = statements_of(&doc);
        let granted: Vec<String> = stmts
            .iter()
            .filter(|s| effect(s) == "Allow")
            .flat_map(actions)
            .collect();
        for a in &granted {
            assert!(
                !a.starts_with("iam:") && a != "*" && !a.ends_with(":*"),
                "boundary grants {a}"
            );
            assert!(!glob(a, "sts:AssumeRole"), "boundary grants {a}");
        }
        for need in [
            "ecr:GetAuthorizationToken",
            "ecr:BatchGetImage",
            "ecr:GetDownloadUrlForLayer",
            "logs:CreateLogStream",
            "logs:PutLogEvents",
            "ssmmessages:CreateControlChannel",
            "ssmmessages:CreateDataChannel",
            "ssmmessages:OpenControlChannel",
            "ssmmessages:OpenDataChannel",
            "secretsmanager:GetSecretValue",
            "kms:Decrypt",
            "elasticfilesystem:ClientMount",
            "elasticfilesystem:ClientWrite",
            "sts:GetCallerIdentity",
            "bedrock:InvokeModel",
            "bedrock:InvokeModelWithResponseStream",
        ] {
            assert!(
                granted.iter().any(|a| glob(a, need)),
                "boundary does not grant {need}"
            );
        }
        // Access points fix the POSIX user; nothing needs root on EFS.
        assert!(
            !granted
                .iter()
                .any(|a| glob(a, "elasticfilesystem:ClientRootAccess")),
            "boundary grants elasticfilesystem:ClientRootAccess"
        );
        // Secrets only through Secrets Manager, and only Triple-C's; keys
        // and file systems only the environment's (component-tagged).
        for s in &stmts {
            let acts = actions(s);
            if acts.iter().any(|a| glob(a, "kms:Decrypt")) {
                assert_eq!(
                    s["Condition"]["StringLike"]["kms:ViaService"],
                    "secretsmanager.*.amazonaws.com",
                    "{}",
                    sid(s)
                );
            }
            if acts
                .iter()
                .any(|a| glob(a, "kms:Decrypt") || glob("elasticfilesystem:Client*", a))
            {
                assert_eq!(
                    s["Condition"]["Null"][COMPONENT_RESOURCE_TAG],
                    "false",
                    "{}: must require the triple-c:component resource tag",
                    sid(s)
                );
            }
            if acts
                .iter()
                .any(|a| glob(a, "secretsmanager:GetSecretValue"))
            {
                assert_eq!(
                    resources(s),
                    vec!["arn:aws:secretsmanager:*:*:secret:triple-c/*".to_string()],
                    "{}",
                    sid(s)
                );
            }
        }
    }

    #[test]
    fn writes_are_marked_and_reads_are_not() {
        const READ_PREFIXES: &[&str] = &["Describe", "List", "Get", "Filter", "Simulate"];
        for tier in TIERS {
            for c in checks(tier) {
                let verb = c.action.split(':').nth(1).unwrap();
                let read = READ_PREFIXES.iter().any(|p| verb.starts_with(p));
                assert_eq!(c.write, !read, "{tier:?}: {} write flag", c.action);
            }
        }
    }

    /// A secret owned by the environment (component-tagged) or by another
    /// user can't be claimed, overwritten or deleted by a workspace user.
    #[test]
    fn workspace_users_cannot_claim_secrets_they_dont_own() {
        let tier = Tier::WorkspaceUser;
        const ENVIRONMENT_SECRET: Context = &[
            (OWNER_REQUEST_TAG, OwnerId),
            (OWNER_RESOURCE_TAG, OwnerId),
            (COMPONENT_RESOURCE_TAG, Literal("environment")),
        ];
        const ENVIRONMENT_SECRET_UNOWNED: Context = &[
            (OWNER_REQUEST_TAG, OwnerId),
            (COMPONENT_RESOURCE_TAG, Literal("environment")),
        ];
        for action in [
            "secretsmanager:TagResource",
            "secretsmanager:UntagResource",
            "secretsmanager:PutSecretValue",
            "secretsmanager:DeleteSecret",
        ] {
            for context in [ENVIRONMENT_SECRET, ENVIRONMENT_SECRET_UNOWNED] {
                let c = write(action, PROBE_SECRET, context);
                assert!(
                    !allowed(tier, &c, ME, ME),
                    "{action} allowed on a component-tagged secret with {context:?}"
                );
            }
        }
        // The Allow alone already refuses to let a component-tagged secret
        // be claimed; the Deny is defense in depth.
        let claim = write(
            "secretsmanager:TagResource",
            PROBE_SECRET,
            ENVIRONMENT_SECRET_UNOWNED,
        );
        assert!(
            !evaluate_in(
                &allows(tier),
                Request {
                    check: &claim,
                    caller: ME,
                    owner: ME
                }
            ),
            "TagOwnSecrets alone allows claiming an environment secret"
        );
        // Retagging another user's secret to yourself.
        let steal = write(
            "secretsmanager:TagResource",
            PROBE_SECRET,
            &[
                (OWNER_REQUEST_TAG, OwnerId),
                (OWNER_RESOURCE_TAG, Literal(SOMEONE_ELSE)),
            ],
        );
        assert!(!allowed(tier, &steal, ME, ME));
        // Tag-on-create (no resource tags yet) still works.
        let create = checks(tier)
            .iter()
            .find(|c| c.action == "secretsmanager:TagResource")
            .unwrap();
        assert!(allowed(tier, create, ME, ME));
    }

    #[test]
    fn access_points_only_on_the_environments_file_system() {
        let c = checks(Tier::WorkspaceUser)
            .iter()
            .find(|c| c.action == "elasticfilesystem:CreateAccessPoint")
            .unwrap();
        let foreign_fs = Check {
            context: &[(OWNER_REQUEST_TAG, OwnerId)],
            ..*c
        };
        assert!(!allowed(Tier::WorkspaceUser, &foreign_fs, ME, ME));
    }

    const CFN_WRITES: &[&str] = &[
        "cloudformation:CreateStack",
        "cloudformation:UpdateStack",
        "cloudformation:CreateChangeSet",
    ];
    const WITH_SERVICE_ROLE: Context = &[(
        "cloudformation:RoleArn",
        Literal("arn:aws:iam::{account}:role/AnyServiceRole"),
    )];
    const OTHER_BOUNDARY: Context = &[
        (
            "iam:PermissionsBoundary",
            Literal("arn:aws:iam::{account}:policy/SomethingElse"),
        ),
        ("aws:PrincipalAccount", Literal("{account}")),
    ];

    #[test]
    fn triple_c_stacks_cannot_use_a_service_role() {
        for action in CFN_WRITES {
            let with_role = write(action, PROBE_STACK, WITH_SERVICE_ROLE);
            assert!(
                !allowed(Tier::EnvironmentAdmin, &with_role, ME, ME),
                "{action} allowed with a CloudFormation service role"
            );
            let plain = write(action, PROBE_STACK, &[]);
            assert!(allowed(Tier::EnvironmentAdmin, &plain, ME, ME), "{action}");
        }
    }

    /// True if some Deny statement in `tier` applies to `check`.
    fn explicitly_denied(tier: Tier, check: &Check) -> bool {
        let stmts = denies(tier);
        let req = Request {
            check,
            caller: ME,
            owner: ME,
        };
        matching(&stmts, &req)
            .iter()
            .any(|s| conditions_hold(s, &req))
    }

    #[test]
    fn a_triple_c_role_boundary_can_only_be_set_to_the_triple_c_boundary() {
        let put = |context| write("iam:PutRolePermissionsBoundary", PROBE_ROLE, context);
        assert!(explicitly_denied(
            Tier::EnvironmentAdmin,
            &put(OTHER_BOUNDARY)
        ));
        assert!(explicitly_denied(Tier::EnvironmentAdmin, &put(&[])));
        assert!(!explicitly_denied(
            Tier::EnvironmentAdmin,
            &put(WITH_BOUNDARY)
        ));
    }

    /// Admins attach both tiers; together they must still never yield a
    /// `triple-c-*` role without the boundary.
    #[test]
    fn both_tiers_together_cannot_obtain_an_unbounded_role() {
        let mut both = statements(Tier::WorkspaceUser);
        both.extend(statements(Tier::EnvironmentAdmin));
        let denied = |c: &Check| {
            !evaluate_in(
                &both,
                Request {
                    check: c,
                    caller: ME,
                    owner: ME,
                },
            )
        };
        for action in ROLE_WRITES {
            for context in [&[][..], OTHER_BOUNDARY] {
                assert!(
                    denied(&write(action, PROBE_ROLE, context)),
                    "{action} with {context:?}"
                );
            }
            assert!(
                !denied(&write(action, PROBE_ROLE, WITH_BOUNDARY)),
                "{action}"
            );
        }
        for action in NEVER_ALLOWED {
            for resource in [
                PROBE_ROLE,
                "arn:aws:iam::{account}:policy/triple-c-permissions-boundary",
                "*",
            ] {
                for context in [&[][..], WITH_BOUNDARY, OTHER_BOUNDARY] {
                    assert!(
                        denied(&write(action, resource, context)),
                        "{action} on {resource} with {context:?}"
                    );
                }
            }
        }
        // A stack deployed with a service role would act outside the
        // boundary.
        for action in CFN_WRITES {
            assert!(denied(&write(action, PROBE_STACK, WITH_SERVICE_ROLE)));
        }
    }

    const ALLOWED_TAG_KEYS: [&str; 3] =
        ["triple-c:owner", "triple-c:project", "triple-c:environment"];

    /// A check like `c` whose request also carries these tag keys.
    fn with_tag_keys(c: &Check, keys: &[&'static str]) -> Check {
        let mut context: Vec<(&'static str, ContextValue)> = c.context.to_vec();
        context.extend(keys.iter().map(|k| ("aws:TagKeys", Literal(k))));
        Check {
            context: Box::leak(context.into_boxed_slice()),
            ..*c
        }
    }

    /// The workspace checks that set tags: those carrying the owner request tag.
    fn tagging_checks() -> Vec<&'static Check> {
        let found: Vec<_> = checks(Tier::WorkspaceUser)
            .iter()
            .filter(|c| c.context.contains(&(OWNER_REQUEST_TAG, OwnerId)))
            .collect();
        assert!(
            found.len() >= 6,
            "expected the six tagging checks, got {found:?}"
        );
        found
    }

    /// Every workspace statement that lets a user set the owner tag also
    /// limits which tag keys they may set.
    #[test]
    fn workspace_tagging_statements_restrict_tag_keys() {
        for s in allows(Tier::WorkspaceUser) {
            let sets_owner = s["Condition"]
                .as_object()
                .is_some_and(|ops| ops.values().any(|e| e.get(OWNER_REQUEST_TAG).is_some()));
            if !sets_owner {
                continue;
            }
            let mut got = strings(&s["Condition"]["ForAllValues:StringEquals"]["aws:TagKeys"]);
            got.sort();
            let mut want: Vec<String> = ALLOWED_TAG_KEYS.iter().map(|k| k.to_string()).collect();
            want.sort();
            assert_eq!(got, want, "{}: aws:TagKeys", sid(&s));
        }
    }

    /// `triple-c:component` marks environment resources, which detection
    /// and the Denies trust; a workspace user must never be able to set it.
    #[test]
    fn workspace_users_cannot_set_the_component_tag() {
        let tier = Tier::WorkspaceUser;
        for c in tagging_checks() {
            let spoofed = with_tag_keys(c, &["triple-c:owner", "triple-c:component"]);
            assert!(
                !allowed(tier, &spoofed, ME, ME),
                "{} on {} allowed with a triple-c:component tag",
                c.action,
                c.resource
            );
            let ordinary = with_tag_keys(c, &["triple-c:owner", "triple-c:project"]);
            assert!(
                allowed(tier, &ordinary, ME, ME),
                "{} with owner and project tags",
                c.action
            );
            // The key restriction is not what grants: without the owner
            // request tag, allowed keys alone are still denied.
            let ownerless = Check {
                context: Box::leak(
                    ordinary
                        .context
                        .iter()
                        .filter(|(k, _)| *k != OWNER_REQUEST_TAG)
                        .copied()
                        .collect::<Vec<_>>()
                        .into_boxed_slice(),
                ),
                ..ordinary
            };
            assert!(
                !allowed(tier, &ownerless, ME, ME),
                "{} allowed without the owner request tag",
                c.action
            );
        }
    }

    #[test]
    fn user_secrets_use_an_environment_key() {
        let tier = Tier::WorkspaceUser;
        let create = checks(tier)
            .iter()
            .find(|c| c.action == "secretsmanager:CreateSecret")
            .unwrap();
        for context in [
            &[(OWNER_REQUEST_TAG, OwnerId)][..],
            &[
                (OWNER_REQUEST_TAG, OwnerId),
                (
                    "secretsmanager:KmsKeyId",
                    Literal("alias/aws/secretsmanager"),
                ),
            ][..],
        ] {
            let c = Check { context, ..*create };
            assert!(
                !allowed(tier, &c, ME, ME),
                "CreateSecret allowed with {context:?}"
            );
        }
        for c in checks(tier).iter().filter(|c| c.action.starts_with("kms:")) {
            let untagged_key = Check {
                context: &[(
                    "kms:ViaService",
                    Literal("secretsmanager.{region}.amazonaws.com"),
                )],
                ..*c
            };
            assert!(
                !allowed(tier, &untagged_key, ME, ME),
                "{} allowed on a key without triple-c:component",
                c.action
            );
        }
    }

    #[test]
    fn for_all_values_is_vacuous_on_an_absent_key() {
        let stmt: Value = serde_json::json!({
            "Sid": "T", "Effect": "Allow", "Action": "x:Y", "Resource": "*",
            "Condition": { "ForAllValues:StringEquals": { "aws:TagKeys": ["a", "b"] } }
        });
        let check = |context| Check {
            action: "x:Y",
            resource: "*",
            context,
            write: true,
        };
        let holds = |c: &Check| {
            conditions_hold(
                &stmt,
                &Request {
                    check: c,
                    caller: ME,
                    owner: ME,
                },
            )
        };
        assert!(holds(&check(&[])));
        assert!(holds(&check(&[
            ("aws:TagKeys", Literal("a")),
            ("aws:TagKeys", Literal("b"))
        ])));
        assert!(!holds(&check(&[
            ("aws:TagKeys", Literal("a")),
            ("aws:TagKeys", Literal("c"))
        ])));
    }

    /// A key whose *key policy* grants account principals through Secrets
    /// Manager (the AWS-managed `aws/secretsmanager` key, or any such customer
    /// key) needs no identity-policy Allow, so only an explicit Deny keeps a
    /// workspace user's secrets on environment keys.
    #[test]
    fn untagged_keys_are_explicitly_denied_through_secrets_manager() {
        let tier = Tier::WorkspaceUser;
        const VIA_SECRETS_MANAGER: Context = &[(
            "kms:ViaService",
            Literal("secretsmanager.{region}.amazonaws.com"),
        )];
        const VIA_SECRETS_MANAGER_ENVIRONMENT_KEY: Context = &[
            (
                "kms:ViaService",
                Literal("secretsmanager.{region}.amazonaws.com"),
            ),
            (COMPONENT_RESOURCE_TAG, Literal("environment")),
        ];
        for action in [
            "kms:Encrypt",
            "kms:GenerateDataKey",
            "kms:GenerateDataKeyWithoutPlaintext",
        ] {
            assert!(
                explicitly_denied(tier, &write(action, PROBE_KEY, VIA_SECRETS_MANAGER)),
                "{action} on an untagged key via Secrets Manager is not explicitly denied"
            );
            assert!(
                !explicitly_denied(
                    tier,
                    &write(action, PROBE_KEY, VIA_SECRETS_MANAGER_ENVIRONMENT_KEY)
                ),
                "{action} on an environment key must not be denied"
            );
        }
        // The checks themselves (environment key) stay allowed.
        for c in checks(tier).iter().filter(|c| c.action.starts_with("kms:")) {
            assert!(allowed(tier, c, ME, ME), "{}", c.action);
        }
    }
}
