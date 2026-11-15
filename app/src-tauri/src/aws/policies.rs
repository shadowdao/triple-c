//! The IAM policies Triple-C ships for each permission tier
//! (`aws/policies/*.json` at the repo root), and the table of
//! representative checks the permission check simulates against them.
//!
//! The table and the JSON are kept in lock-step by the tests below: every
//! action in a policy has a check, every check is allowed by its policy under
//! a small IAM-style evaluator, and every owner-scoped check is *denied* when
//! the resource belongs to someone else.

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

/// A context value sent with a simulated request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextValue {
    /// The caller's `aws:userid` (the owner ID every per-user resource is
    /// tagged with).
    OwnerId,
    /// A fixed value. `{region}` and `{account}` are substituted exactly as in
    /// [`Check::resource`] — use [`render`].
    Literal(&'static str),
}

/// One simulated request: "may the caller do `action` on `resource`, given
/// `context`?"
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Check {
    pub action: &'static str,
    /// An ARN template; `{region}` and `{account}` are substituted ([`render`]).
    pub resource: &'static str,
    pub context: &'static [(&'static str, ContextValue)],
    pub write: bool,
}

/// Substitutes `{region}` and `{account}` in a resource or literal template.
pub fn render(template: &str, region: &str, account: &str) -> String {
    template
        .replace("{region}", region)
        .replace("{account}", account)
}

pub fn checks(tier: Tier) -> &'static [Check] {
    match tier {
        Tier::WorkspaceUser => WORKSPACE_USER_CHECKS,
        Tier::EnvironmentAdmin => ENVIRONMENT_ADMIN_CHECKS,
    }
}

use ContextValue::{Literal, OwnerId};

const OWNER_REQUEST_TAG: &str = "aws:RequestTag/triple-c:owner";
const OWNER_RESOURCE_TAG: &str = "aws:ResourceTag/triple-c:owner";
/// The tag SSM puts on a session; its value is the starter's `aws:userid`.
const SESSION_OWNER: &str = "ssm:resourceTag/aws:ssmmessages:session-id";

const PROBE_CLUSTER: &str = "arn:aws:ecs:{region}:{account}:cluster/triple-c-probe";
const PROBE_TASK: &str =
    "arn:aws:ecs:{region}:{account}:task/triple-c-probe/0123456789abcdef0123456789abcdef";
const PROBE_TASK_DEFINITION: &str =
    "arn:aws:ecs:{region}:{account}:task-definition/triple-c-probe:1";
const PROBE_ROLE: &str = "arn:aws:iam::{account}:role/triple-c-probe";
const PROBE_SECRET: &str =
    "arn:aws:secretsmanager:{region}:{account}:secret:triple-c/probe/probe-AbCdEf";
const PROBE_KEY: &str = "arn:aws:kms:{region}:{account}:key/00000000-0000-0000-0000-000000000000";
const PROBE_LOG_GROUP: &str = "arn:aws:logs:{region}:{account}:log-group:/triple-c/probe";
const PROBE_PARAMETER: &str =
    "arn:aws:ssm:{region}:{account}:parameter/triple-c/environments/probe";
const PROBE_STACK: &str = "arn:aws:cloudformation:{region}:{account}:stack/triple-c-probe/00000000-0000-0000-0000-000000000000";
const PROBE_FILE_SYSTEM: &str =
    "arn:aws:elasticfilesystem:{region}:{account}:file-system/fs-00000000";

const fn write(
    action: &'static str,
    resource: &'static str,
    context: &'static [(&'static str, ContextValue)],
) -> Check {
    Check {
        action,
        resource,
        context,
        write: true,
    }
}

const fn read(action: &'static str, resource: &'static str) -> Check {
    Check {
        action,
        resource,
        context: &[],
        write: false,
    }
}

const WORKSPACE_USER_CHECKS: &[Check] = &[
    write(
        "ecs:RunTask",
        PROBE_TASK_DEFINITION,
        &[
            (OWNER_REQUEST_TAG, OwnerId),
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
        "arn:aws:ssm:{region}:{account}:session/probe-0123456789abcdef0",
        &[(SESSION_OWNER, OwnerId)],
    ),
    write(
        "ssm:ResumeSession",
        "arn:aws:ssm:{region}:{account}:session/probe-0123456789abcdef0",
        &[(SESSION_OWNER, OwnerId)],
    ),
    write(
        "secretsmanager:CreateSecret",
        PROBE_SECRET,
        &[(OWNER_REQUEST_TAG, OwnerId)],
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
    write(
        "kms:Encrypt",
        PROBE_KEY,
        &[(
            "kms:ViaService",
            Literal("secretsmanager.{region}.amazonaws.com"),
        )],
    ),
    write(
        "kms:GenerateDataKey",
        PROBE_KEY,
        &[(
            "kms:ViaService",
            Literal("secretsmanager.{region}.amazonaws.com"),
        )],
    ),
    read("ecs:DescribeClusters", PROBE_CLUSTER),
    read("ecs:ListClusters", "*"),
    read("ssm:GetParametersByPath", PROBE_PARAMETER),
    read("cloudformation:DescribeStacks", "*"),
    read("cloudformation:ListStacks", "*"),
    read("secretsmanager:ListSecrets", "*"),
    read("elasticfilesystem:DescribeAccessPoints", "*"),
    read(
        "logs:GetLogEvents",
        "arn:aws:logs:{region}:{account}:log-group:/triple-c/probe:log-stream:probe",
    ),
    read("logs:FilterLogEvents", PROBE_LOG_GROUP),
    read("sts:GetCallerIdentity", "*"),
];

const ENVIRONMENT_ADMIN_CHECKS: &[Check] = &[
    write("cloudformation:CreateStack", PROBE_STACK, &[]),
    write("cloudformation:UpdateStack", PROBE_STACK, &[]),
    write("cloudformation:DeleteStack", PROBE_STACK, &[]),
    write("iam:CreateRole", PROBE_ROLE, &[]),
    write("iam:DeleteRole", PROBE_ROLE, &[]),
    write("iam:PutRolePolicy", PROBE_ROLE, &[]),
    write("iam:DeleteRolePolicy", PROBE_ROLE, &[]),
    write("iam:AttachRolePolicy", PROBE_ROLE, &[]),
    write("iam:DetachRolePolicy", PROBE_ROLE, &[]),
    write("iam:TagRole", PROBE_ROLE, &[]),
    read("iam:GetRole", PROBE_ROLE),
    write(
        "iam:PassRole",
        PROBE_ROLE,
        &[("iam:PassedToService", Literal("ecs-tasks.amazonaws.com"))],
    ),
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
    write("ec2:CreateSecurityGroup", "*", &[]),
    write("ec2:DeleteSecurityGroup", "*", &[]),
    write("ec2:AuthorizeSecurityGroupIngress", "*", &[]),
    write("ec2:RevokeSecurityGroupIngress", "*", &[]),
    write("ec2:CreateTags", "*", &[]),
    read("ec2:DescribeSecurityGroups", "*"),
    write("elasticfilesystem:CreateFileSystem", PROBE_FILE_SYSTEM, &[]),
    write("elasticfilesystem:DeleteFileSystem", PROBE_FILE_SYSTEM, &[]),
    write(
        "elasticfilesystem:CreateMountTarget",
        PROBE_FILE_SYSTEM,
        &[],
    ),
    write(
        "elasticfilesystem:DeleteMountTarget",
        PROBE_FILE_SYSTEM,
        &[],
    ),
    write(
        "elasticfilesystem:PutFileSystemPolicy",
        PROBE_FILE_SYSTEM,
        &[],
    ),
    write("elasticfilesystem:TagResource", PROBE_FILE_SYSTEM, &[]),
    write("kms:CreateKey", "*", &[]),
    write("kms:ScheduleKeyDeletion", PROBE_KEY, &[]),
    write("kms:PutKeyPolicy", PROBE_KEY, &[]),
    write("kms:TagResource", PROBE_KEY, &[]),
    // An alias call is authorized against both the alias and the key.
    write(
        "kms:CreateAlias",
        "arn:aws:kms:{region}:{account}:alias/triple-c-probe",
        &[],
    ),
    write("kms:CreateAlias", PROBE_KEY, &[]),
    write(
        "kms:DeleteAlias",
        "arn:aws:kms:{region}:{account}:alias/triple-c-probe",
        &[],
    ),
    write("kms:DeleteAlias", PROBE_KEY, &[]),
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
    read("iam:SimulatePrincipalPolicy", "*"),
];

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;
    use std::collections::BTreeSet;

    const REGION: &str = "us-west-2";
    const ACCOUNT: &str = "111122223333";
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

    fn policy(tier: Tier) -> Value {
        serde_json::from_str(policy_json(tier))
            .unwrap_or_else(|e| panic!("{tier:?} policy is not JSON: {e}"))
    }

    fn statements(tier: Tier) -> Vec<Value> {
        policy(tier)["Statement"]
            .as_array()
            .unwrap_or_else(|| panic!("{tier:?} policy has no Statement array"))
            .clone()
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
        render(check.resource, REGION, ACCOUNT)
    }

    /// Statements that name `check`'s action and one of whose resources
    /// matches `check`'s rendered resource.
    fn statements_for(tier: Tier, check: &Check) -> Vec<Value> {
        let resource = rendered(check);
        statements(tier)
            .into_iter()
            .filter(|s| actions(s).iter().any(|a| glob(a, check.action)))
            .filter(|s| resources(s).iter().any(|r| glob(r, &resource)))
            .collect()
    }

    /// A deliberately small IAM evaluator for Allow statements: the
    /// operators the shipped policies use, and nothing else (an unknown
    /// operator fails the test rather than being ignored). `caller` is the
    /// `${aws:userid}` of the principal; `owner` is what `OwnerId` context
    /// entries carry (the resource's or request's owner tag).
    fn conditions_hold(stmt: &Value, check: &Check, caller: &str, owner: &str) -> bool {
        let Some(cond) = stmt.get("Condition") else {
            return true;
        };
        let cond = cond.as_object().expect("Condition is an object");
        cond.iter().all(|(op, entries)| {
            let (base, if_exists) = match op.strip_suffix("IfExists") {
                Some(b) => (b, true),
                None => (op.as_str(), false),
            };
            let matches: fn(&str, &str) -> bool = match base {
                "StringEquals" => |p, v| p == v,
                "StringLike" | "ArnLike" => glob,
                other => panic!("{}: unsupported condition operator {other}", sid(stmt)),
            };
            entries
                .as_object()
                .expect("condition block is an object")
                .iter()
                .all(|(key, want)| {
                    let supplied =
                        check
                            .context
                            .iter()
                            .find(|(k, _)| k == key)
                            .map(|(_, v)| match v {
                                ContextValue::OwnerId => owner.to_string(),
                                ContextValue::Literal(l) => render(l, REGION, ACCOUNT),
                            });
                    match supplied {
                        None => if_exists,
                        Some(value) => strings(want)
                            .iter()
                            .map(|p| p.replace("${aws:userid}", caller))
                            .any(|p| matches(&p, &value)),
                    }
                })
        })
    }

    fn allowed(tier: Tier, check: &Check, caller: &str, owner: &str) -> bool {
        statements_for(tier, check)
            .iter()
            .any(|s| conditions_hold(s, check, caller, owner))
    }

    #[test]
    fn policies_are_valid_iam_json() {
        for tier in TIERS {
            let p = policy(tier);
            assert_eq!(p["Version"], "2012-10-17", "{tier:?}");
            let stmts = statements(tier);
            assert!(!stmts.is_empty(), "{tier:?} has no statements");
            let mut sids = BTreeSet::new();
            for s in &stmts {
                let id = s["Sid"]
                    .as_str()
                    .unwrap_or_else(|| panic!("{tier:?}: statement without Sid: {s}"));
                assert!(
                    !id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric()),
                    "{tier:?}: Sid {id:?} must be non-empty alphanumeric"
                );
                assert!(sids.insert(id.to_string()), "{tier:?}: duplicate Sid {id}");
                assert_eq!(s["Effect"], "Allow", "{tier:?} {id}");
                assert!(!actions(s).is_empty(), "{tier:?} {id}: no Action");
                assert!(!resources(s).is_empty(), "{tier:?} {id}: no Resource");
                for key in s.as_object().unwrap().keys() {
                    assert!(
                        ["Sid", "Effect", "Action", "Resource", "Condition"].contains(&key.as_str()),
                        "{tier:?} {id}: unexpected key {key} (NotAction/NotResource are not allowed)"
                    );
                }
            }
        }
    }

    #[test]
    fn every_policy_action_has_a_check_and_vice_versa() {
        for tier in TIERS {
            let policy_actions: BTreeSet<String> =
                statements(tier).iter().flat_map(actions).collect();
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
            for c in checks(tier) {
                assert!(
                    !statements_for(tier, c).is_empty(),
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
        (
            "ecs:RunTask",
            "task-definition/",
            "aws:RequestTag/triple-c:owner",
        ),
        ("ecs:TagResource", "task/", "aws:RequestTag/triple-c:owner"),
        ("ecs:StopTask", "task/", "aws:ResourceTag/triple-c:owner"),
        (
            "ecs:ExecuteCommand",
            "task/",
            "aws:ResourceTag/triple-c:owner",
        ),
        (
            "ssm:StartSession",
            "task/",
            "aws:ResourceTag/triple-c:owner",
        ),
        (
            "ssm:TerminateSession",
            "session/",
            "ssm:resourceTag/aws:ssmmessages:session-id",
        ),
        (
            "ssm:ResumeSession",
            "session/",
            "ssm:resourceTag/aws:ssmmessages:session-id",
        ),
        ("iam:PassRole", "role/", "iam:ResourceTag/triple-c:owner"),
        (
            "secretsmanager:CreateSecret",
            "secret:",
            "aws:RequestTag/triple-c:owner",
        ),
        (
            "secretsmanager:TagResource",
            "secret:",
            "aws:RequestTag/triple-c:owner",
        ),
        (
            "secretsmanager:PutSecretValue",
            "secret:",
            "aws:ResourceTag/triple-c:owner",
        ),
        (
            "secretsmanager:DeleteSecret",
            "secret:",
            "aws:ResourceTag/triple-c:owner",
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
            let stmts = statements_for(Tier::WorkspaceUser, check);
            assert!(
                stmts
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
                    "{tier:?}: {} on {} with {:?} is not allowed by any statement",
                    c.action,
                    rendered(c),
                    c.context
                );
            }
        }
    }

    /// The spec's most important rule: nobody may stop, exec into, open a
    /// session to, pass the role of, or touch the secrets of a workspace
    /// they don't own.
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
            // A secret with no owner tag yet is only tag-on-create, which
            // still needs the request tag — so every case must be denied.
            assert!(
                !allowed(Tier::WorkspaceUser, &untagged, ME, ME),
                "{} is allowed without any owner context",
                check.action
            );
        }
    }

    #[test]
    fn ecs_tag_resource_only_tags_at_run_task() {
        let tag = checks(Tier::WorkspaceUser)
            .iter()
            .find(|c| c.action == "ecs:TagResource")
            .expect("ecs:TagResource check");
        let retag = Check {
            context: &[("aws:RequestTag/triple-c:owner", ContextValue::OwnerId)],
            ..*tag
        };
        assert!(
            !allowed(Tier::WorkspaceUser, &retag, ME, ME),
            "ecs:TagResource must require ecs:CreateAction"
        );
        let other_create = Check {
            context: &[
                ("aws:RequestTag/triple-c:owner", ContextValue::OwnerId),
                ("ecs:CreateAction", ContextValue::Literal("CreateService")),
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
                ("iam:ResourceTag/triple-c:owner", ContextValue::OwnerId),
                (
                    "iam:PassedToService",
                    ContextValue::Literal("ec2.amazonaws.com"),
                ),
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
                context: &[(
                    "kms:ViaService",
                    ContextValue::Literal("s3.{region}.amazonaws.com"),
                )],
                ..*c
            };
            assert!(
                !allowed(Tier::WorkspaceUser, &via_s3, ME, ME),
                "{} allowed via S3",
                c.action
            );
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
}
