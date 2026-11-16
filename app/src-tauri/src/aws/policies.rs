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

/// The policy documents an admin attaches for `tier`, as (file name under
/// `aws/policies/`, contents). Every file of a tier is required: the admin
/// tier is split across files only because a managed policy holds 6,144
/// characters (D7).
pub fn policy_files(tier: Tier) -> &'static [(&'static str, &'static str)] {
    match tier {
        Tier::EnvironmentAdmin => &[
            (
                "environment-admin.json",
                include_str!("../../../../aws/policies/environment-admin.json"),
            ),
            (
                "environment-admin-network.json",
                include_str!("../../../../aws/policies/environment-admin-network.json"),
            ),
            (
                "environment-admin-compute.json",
                include_str!("../../../../aws/policies/environment-admin-compute.json"),
            ),
        ],
        Tier::WorkspaceUser => &[
            (
                "workspace-user.json",
                include_str!("../../../../aws/policies/workspace-user.json"),
            ),
            (
                "workspace-user-tasks.json",
                include_str!("../../../../aws/policies/workspace-user-tasks.json"),
            ),
        ],
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
/// A call EFS makes on the caller's behalf (a mount target's interface).
const VIA_EFS: Context = &[(
    "aws:CalledViaLast",
    Literal("elasticfilesystem.amazonaws.com"),
)];
/// Created inside a Triple-C (component-tagged) VPC, subnet or address.
const REQUESTED_IN_TRIPLE_C_PARENT: Context = &[
    ("aws:RequestTag/triple-c:component", Literal("environment")),
    (COMPONENT_RESOURCE_TAG, Literal("environment")),
];
/// The state bucket is in the caller's own account.
const IN_MY_ACCOUNT: Context = &[
    ("s3:ResourceAccount", Literal("{account}")),
    ("aws:PrincipalAccount", Literal("{account}")),
];
/// The state bucket is created tagged `triple-c:component=state`.
const STATE_REQUESTED: Context = &[
    ("aws:RequestTag/triple-c:component", Literal("state")),
    ("s3:ResourceAccount", Literal("{account}")),
    ("aws:PrincipalAccount", Literal("{account}")),
];

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
const PROBE_FILE_SYSTEM: &str =
    "arn:aws:elasticfilesystem:{region}:{account}:file-system/fs-00000000";
const PROBE_ACCESS_POINT: &str =
    "arn:aws:elasticfilesystem:{region}:{account}:access-point/fsap-00000000000000000";
const PROBE_SECURITY_GROUP: &str =
    "arn:aws:ec2:{region}:{account}:security-group/sg-00000000000000000";
const PROBE_VPC: &str = "arn:aws:ec2:{region}:{account}:vpc/vpc-00000000000000000";
const PROBE_SESSION: &str = "arn:aws:ssm:{region}:{account}:session/probe-0123456789abcdef0";
const PROBE_SUBNET: &str = "arn:aws:ec2:{region}:{account}:subnet/subnet-00000000000000000";
const PROBE_INTERNET_GATEWAY: &str =
    "arn:aws:ec2:{region}:{account}:internet-gateway/igw-00000000000000000";
const PROBE_ROUTE_TABLE: &str = "arn:aws:ec2:{region}:{account}:route-table/rtb-00000000000000000";
const PROBE_ELASTIC_IP: &str =
    "arn:aws:ec2:{region}:{account}:elastic-ip/eipalloc-00000000000000000";
const PROBE_NAT_GATEWAY: &str = "arn:aws:ec2:{region}:{account}:natgateway/nat-00000000000000000";
/// What EC2 names when a call's resources no longer exist (an address
/// association that went with its NAT gateway): a literal `*/*`.
const PROBE_GONE_ASSOCIATION: &str = "arn:aws:ec2:{region}:{account}:*/*";
const PROBE_NETWORK_INTERFACE: &str =
    "arn:aws:ec2:{region}:{account}:network-interface/eni-00000000000000000";
const PROBE_SECURITY_GROUP_RULE: &str =
    "arn:aws:ec2:{region}:{account}:security-group-rule/sgr-00000000000000000";
/// [`super::tofu_modules::state_bucket_name`]'s bucket.
const PROBE_STATE_BUCKET: &str = "arn:aws:s3:::triple-c-tfstate-{account}-{region}";
const PROBE_STATE_OBJECT: &str =
    "arn:aws:s3:::triple-c-tfstate-{account}-{region}/environments/probe/terraform.tfstate";
const PROBE_BOUNDARY: &str = "arn:aws:iam::{account}:policy/triple-c-permissions-boundary";
// Per-user EC2 capacity (phase 7, `environment-admin-compute.json`).
const PROBE_CAPACITY_PROVIDER: &str =
    "arn:aws:ecs:{region}:{account}:capacity-provider/triple-c-probe";
const PROBE_LAUNCH_TEMPLATE: &str =
    "arn:aws:ec2:{region}:{account}:launch-template/lt-00000000000000000";
const PROBE_INSTANCE: &str = "arn:aws:ec2:{region}:{account}:instance/i-00000000000000000";
const PROBE_VOLUME: &str = "arn:aws:ec2:{region}:{account}:volume/vol-00000000000000000";
/// Public images have no account in their ARN.
const PROBE_IMAGE: &str = "arn:aws:ec2:{region}::image/ami-00000000000000000";
const PROBE_AUTO_SCALING_GROUP: &str = "arn:aws:autoscaling:{region}:{account}:autoScalingGroup:\
     00000000-0000-0000-0000-000000000000:autoScalingGroupName/triple-c-probe";
const PROBE_INSTANCE_PROFILE: &str = "arn:aws:iam::{account}:instance-profile/triple-c-probe";
/// An instance role is `triple-c-<env>-<owner key>-instance`.
const PROBE_INSTANCE_ROLE: &str = "arn:aws:iam::{account}:role/triple-c-probe-instance";
const PROBE_AUTO_SCALING_SERVICE_ROLE: &str = "arn:aws:iam::{account}:role/aws-service-role/\
     autoscaling.amazonaws.com/AWSServiceRoleForAutoScaling";
const PROBE_ECS_SERVICE_ROLE: &str =
    "arn:aws:iam::{account}:role/aws-service-role/ecs.amazonaws.com/AWSServiceRoleForECS";
/// The public parameter the onboarding module resolves the ECS-optimized
/// AMI from (no account in its ARN).
const PROBE_AMI_PARAMETER: &str = "arn:aws:ssm:{region}::parameter/aws/service/ecs/\
     optimized-ami/amazon-linux-2023/recommended/image_id";
/// A user's compute record (`/triple-c/users/<env>/<owner key>`, E11).
const PROBE_USER_RECORD: &str =
    "arn:aws:ssm:{region}:{account}:parameter/triple-c/users/probe/0123456789ab";
/// RunInstances names the launch template the instance comes from.
const FROM_LAUNCH_TEMPLATE: Context = &[(
    "ec2:LaunchTemplate",
    Literal("arn:aws:ec2:{region}:{account}:launch-template/lt-00000000000000000"),
)];
/// ...and the image is Amazon's.
const AMAZON_IMAGE_FROM_LAUNCH_TEMPLATE: Context = &[
    ("ec2:Owner", Literal("amazon")),
    (
        "ec2:LaunchTemplate",
        Literal("arn:aws:ec2:{region}:{account}:launch-template/lt-00000000000000000"),
    ),
];
const PASSED_TO_EC2: Context = &[("iam:PassedToService", Literal("ec2.amazonaws.com"))];

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
    // `workspace-user-tasks.json`: a user registers and tags their own task
    // definitions, and reads their own secrets. Retiring them is the admin's
    // (R8): ECS authorizes `DeregisterTaskDefinition` against `*` only, with
    // no tag condition, so it can't be confined to a user's own revisions.
    // Residual risk the policy cannot close: the family carries the owner
    // key, which IAM can't condition on, so any user may register a revision
    // into another owner's family. The app's mitigation: it runs only the
    // exact `family:revision` ARN its own register call returned, and when it
    // looks up an existing revision it accepts only one tagged with the
    // caller's own owner ID (never "latest of the family").
    write(
        "ecs:RegisterTaskDefinition",
        PROBE_TASK_DEFINITION,
        &[(OWNER_REQUEST_TAG, OwnerId)],
    ),
    write(
        "ecs:TagResource",
        PROBE_TASK_DEFINITION,
        &[
            (OWNER_REQUEST_TAG, OwnerId),
            ("ecs:CreateAction", Literal("RegisterTaskDefinition")),
        ],
    ),
    read(
        "secretsmanager:GetSecretValue",
        PROBE_SECRET,
        &[(OWNER_RESOURCE_TAG, OwnerId)],
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
    // Only on the user's own file system: Triple-C's (component-tagged by
    // their onboarding module) and tagged with their owner ID (D2).
    write(
        "elasticfilesystem:CreateAccessPoint",
        PROBE_FILE_SYSTEM,
        &[
            (OWNER_REQUEST_TAG, OwnerId),
            (OWNER_RESOURCE_TAG, OwnerId),
            (COMPONENT_RESOURCE_TAG, Literal("user-onboarding")),
        ],
    ),
    // Authorized against the parent file system, not the new access point
    // (verified live, phase 5).
    write(
        "elasticfilesystem:TagResource",
        PROBE_FILE_SYSTEM,
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
    read("secretsmanager:ListSecrets", "*", &[]),
    read("elasticfilesystem:DescribeAccessPoints", "*", &[]),
    read(
        "logs:GetLogEvents",
        "arn:aws:logs:{region}:{account}:log-group:/triple-c/probe:log-stream:probe",
        &[],
    ),
    read("logs:FilterLogEvents", PROBE_LOG_GROUP, &[]),
    read("sts:GetCallerIdentity", "*", &[]),
    // `workspace-user-tasks.json`: the hourly cost estimate (Q5). The Price
    // List API has no resource-level permissions, and prices are public.
    read("pricing:GetProducts", "*", &[]),
];

/// The admin tier's checks, across all its files. `tofu_modules`' tests hold
/// the per-resource table these are drawn from.
const ENVIRONMENT_ADMIN_CHECKS: &[Check] = &[
    // Roles: bounded writes, by name.
    write("iam:CreateRole", PROBE_ROLE, WITH_BOUNDARY),
    write("iam:PutRolePolicy", PROBE_ROLE, WITH_BOUNDARY),
    write("iam:DeleteRolePolicy", PROBE_ROLE, WITH_BOUNDARY),
    write("iam:AttachRolePolicy", PROBE_ROLE, WITH_BOUNDARY),
    write("iam:DetachRolePolicy", PROBE_ROLE, WITH_BOUNDARY),
    write("iam:DeleteRole", PROBE_ROLE, &[]),
    write("iam:TagRole", PROBE_ROLE, &[]),
    write("iam:UntagRole", PROBE_ROLE, &[]),
    write("iam:UpdateRole", PROBE_ROLE, &[]),
    write("iam:UpdateRoleDescription", PROBE_ROLE, &[]),
    read("iam:GetRole", PROBE_ROLE, &[]),
    read("iam:GetRolePolicy", PROBE_ROLE, &[]),
    read("iam:ListRolePolicies", PROBE_ROLE, &[]),
    read("iam:ListAttachedRolePolicies", PROBE_ROLE, &[]),
    read("iam:ListInstanceProfilesForRole", PROBE_ROLE, &[]),
    write(
        "iam:PassRole",
        PROBE_ROLE,
        &[("iam:PassedToService", Literal("ecs-tasks.amazonaws.com"))],
    ),
    read("iam:GetRole", "{principal}", OWN_PRINCIPAL),
    read("iam:SimulatePrincipalPolicy", "{principal}", OWN_PRINCIPAL),
    // Detection reads the boundary policy.
    read("iam:GetPolicy", PROBE_BOUNDARY, &[]),
    // ECS, by name.
    write("ecs:CreateCluster", PROBE_CLUSTER, COMPONENT_REQUESTED),
    write("ecs:DeleteCluster", PROBE_CLUSTER, &[]),
    write("ecs:UpdateCluster", PROBE_CLUSTER, &[]),
    write("ecs:PutClusterCapacityProviders", PROBE_CLUSTER, &[]),
    write(
        "ecs:TagResource",
        PROBE_CLUSTER,
        &[("ecs:CreateAction", Literal("CreateCluster"))],
    ),
    write("ecs:UntagResource", PROBE_CLUSTER, &[]),
    read("ecs:DescribeClusters", PROBE_CLUSTER, &[]),
    read("ecs:ListTagsForResource", PROBE_CLUSTER, &[]),
    write("ecs:RegisterTaskDefinition", "*", &[]),
    write("ecs:DeregisterTaskDefinition", "*", &[]),
    // The network (environment-admin-network.json). Creates carry the
    // component request tag and are authorized against the new resource and
    // its parents; tags go on only through the creating call.
    write("ec2:CreateVpc", PROBE_VPC, COMPONENT_REQUESTED),
    write("ec2:CreateSubnet", PROBE_SUBNET, COMPONENT_REQUESTED),
    write("ec2:CreateSubnet", PROBE_VPC, REQUESTED_IN_TRIPLE_C_PARENT),
    write(
        "ec2:CreateInternetGateway",
        PROBE_INTERNET_GATEWAY,
        COMPONENT_REQUESTED,
    ),
    write(
        "ec2:CreateRouteTable",
        PROBE_ROUTE_TABLE,
        COMPONENT_REQUESTED,
    ),
    write(
        "ec2:CreateRouteTable",
        PROBE_VPC,
        REQUESTED_IN_TRIPLE_C_PARENT,
    ),
    write("ec2:AllocateAddress", PROBE_ELASTIC_IP, COMPONENT_REQUESTED),
    write(
        "ec2:CreateNatGateway",
        PROBE_NAT_GATEWAY,
        COMPONENT_REQUESTED,
    ),
    write(
        "ec2:CreateNatGateway",
        PROBE_SUBNET,
        REQUESTED_IN_TRIPLE_C_PARENT,
    ),
    write(
        "ec2:CreateNatGateway",
        PROBE_ELASTIC_IP,
        REQUESTED_IN_TRIPLE_C_PARENT,
    ),
    write(
        "ec2:CreateSecurityGroup",
        PROBE_SECURITY_GROUP,
        COMPONENT_REQUESTED,
    ),
    write(
        "ec2:CreateNatGateway",
        PROBE_VPC,
        REQUESTED_IN_TRIPLE_C_PARENT,
    ),
    // The one create that may name an untagged (existing-network) VPC.
    // The VPC side of the request carries no request tag (verified live),
    // so only the group's side is tag-checked.
    write("ec2:CreateSecurityGroup", PROBE_VPC, &[]),
    write(
        "ec2:CreateTags",
        PROBE_SECURITY_GROUP,
        &[("ec2:CreateAction", Literal("CreateSecurityGroup"))],
    ),
    write(
        "ec2:CreateTags",
        PROBE_VPC,
        &[("ec2:CreateAction", Literal("CreateVpc"))],
    ),
    write(
        "ec2:CreateTags",
        PROBE_NAT_GATEWAY,
        &[("ec2:CreateAction", Literal("CreateNatGateway"))],
    ),
    // Changes and deletes need the component tag on the resource.
    write("ec2:CreateTags", PROBE_SECURITY_GROUP, COMPONENT_TAGGED),
    write("ec2:DeleteTags", PROBE_SUBNET, COMPONENT_TAGGED),
    write("ec2:ModifyVpcAttribute", PROBE_VPC, COMPONENT_TAGGED),
    write("ec2:DeleteVpc", PROBE_VPC, COMPONENT_TAGGED),
    write("ec2:ModifySubnetAttribute", PROBE_SUBNET, COMPONENT_TAGGED),
    write("ec2:DeleteSubnet", PROBE_SUBNET, COMPONENT_TAGGED),
    write(
        "ec2:AttachInternetGateway",
        PROBE_INTERNET_GATEWAY,
        COMPONENT_TAGGED,
    ),
    write("ec2:AttachInternetGateway", PROBE_VPC, COMPONENT_TAGGED),
    write(
        "ec2:DetachInternetGateway",
        PROBE_INTERNET_GATEWAY,
        COMPONENT_TAGGED,
    ),
    write(
        "ec2:DeleteInternetGateway",
        PROBE_INTERNET_GATEWAY,
        COMPONENT_TAGGED,
    ),
    write("ec2:DeleteRouteTable", PROBE_ROUTE_TABLE, COMPONENT_TAGGED),
    // Routes and associations can't be tagged: their route table (and
    // subnet) can.
    write("ec2:CreateRoute", PROBE_ROUTE_TABLE, COMPONENT_TAGGED),
    write("ec2:ReplaceRoute", PROBE_ROUTE_TABLE, COMPONENT_TAGGED),
    write("ec2:DeleteRoute", PROBE_ROUTE_TABLE, COMPONENT_TAGGED),
    write(
        "ec2:AssociateRouteTable",
        PROBE_ROUTE_TABLE,
        COMPONENT_TAGGED,
    ),
    write("ec2:AssociateRouteTable", PROBE_SUBNET, COMPONENT_TAGGED),
    write(
        "ec2:DisassociateRouteTable",
        PROBE_ROUTE_TABLE,
        COMPONENT_TAGGED,
    ),
    write("ec2:DisassociateRouteTable", PROBE_SUBNET, COMPONENT_TAGGED),
    write("ec2:ReleaseAddress", PROBE_ELASTIC_IP, COMPONENT_TAGGED),
    // Destroying the NAT gateway's address disassociates it first; the NAT
    // gateway is gone by then, so EC2 authorizes the call against the
    // literal `*/*` (verified live), which is all the policy allows. That a
    // live association never matches it is expected, not verified.
    write("ec2:DisassociateAddress", PROBE_GONE_ASSOCIATION, &[]),
    write("ec2:DeleteNatGateway", PROBE_NAT_GATEWAY, COMPONENT_TAGGED),
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
    // Authorizing also names the new (untagged) rule.
    write(
        "ec2:AuthorizeSecurityGroupIngress",
        PROBE_SECURITY_GROUP_RULE,
        &[],
    ),
    write(
        "ec2:AuthorizeSecurityGroupEgress",
        PROBE_SECURITY_GROUP,
        COMPONENT_TAGGED,
    ),
    write(
        "ec2:AuthorizeSecurityGroupEgress",
        PROBE_SECURITY_GROUP_RULE,
        &[],
    ),
    write(
        "ec2:RevokeSecurityGroupIngress",
        PROBE_SECURITY_GROUP,
        COMPONENT_TAGGED,
    ),
    // The provider revokes AWS's default egress rule after creating a group.
    write(
        "ec2:RevokeSecurityGroupEgress",
        PROBE_SECURITY_GROUP,
        COMPONENT_TAGGED,
    ),
    write(
        "ec2:UpdateSecurityGroupRuleDescriptionsIngress",
        PROBE_SECURITY_GROUP,
        COMPONENT_TAGGED,
    ),
    write(
        "ec2:UpdateSecurityGroupRuleDescriptionsEgress",
        PROBE_SECURITY_GROUP,
        COMPONENT_TAGGED,
    ),
    // EFS creates and deletes a mount target's interface with the caller's
    // EC2 permissions.
    write(
        "ec2:CreateNetworkInterface",
        PROBE_NETWORK_INTERFACE,
        VIA_EFS,
    ),
    write("ec2:CreateNetworkInterface", PROBE_SUBNET, VIA_EFS),
    write(
        "ec2:DeleteNetworkInterface",
        PROBE_NETWORK_INTERFACE,
        VIA_EFS,
    ),
    read("ec2:DescribeSecurityGroups", "*", &[]),
    read("ec2:DescribeVpcs", "*", &[]),
    // EFS. CreateFileSystem has no resource type: IAM authorizes it against
    // `*` (verified live; a `file-system/*` Resource never matches).
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
        "elasticfilesystem:UntagResource",
        PROBE_FILE_SYSTEM,
        COMPONENT_TAGGED,
    ),
    write(
        "elasticfilesystem:DeleteFileSystem",
        PROBE_FILE_SYSTEM,
        COMPONENT_TAGGED,
    ),
    write(
        "elasticfilesystem:UpdateFileSystem",
        PROBE_FILE_SYSTEM,
        COMPONENT_TAGGED,
    ),
    write(
        "elasticfilesystem:PutLifecycleConfiguration",
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
        "elasticfilesystem:ModifyMountTargetSecurityGroups",
        PROBE_FILE_SYSTEM,
        COMPONENT_TAGGED,
    ),
    write(
        "elasticfilesystem:PutFileSystemPolicy",
        PROBE_FILE_SYSTEM,
        COMPONENT_TAGGED,
    ),
    write(
        "elasticfilesystem:DeleteFileSystemPolicy",
        PROBE_FILE_SYSTEM,
        COMPONENT_TAGGED,
    ),
    read(
        "elasticfilesystem:DescribeFileSystems",
        PROBE_FILE_SYSTEM,
        &[],
    ),
    read(
        "elasticfilesystem:ListTagsForResource",
        PROBE_FILE_SYSTEM,
        &[],
    ),
    // KMS.
    write("kms:CreateKey", "*", COMPONENT_REQUESTED),
    write("kms:ScheduleKeyDeletion", PROBE_KEY, COMPONENT_TAGGED),
    write("kms:PutKeyPolicy", PROBE_KEY, COMPONENT_TAGGED),
    write("kms:EnableKeyRotation", PROBE_KEY, COMPONENT_TAGGED),
    write("kms:UpdateKeyDescription", PROBE_KEY, COMPONENT_TAGGED),
    write("kms:TagResource", PROBE_KEY, COMPONENT_TAGGED),
    write("kms:UntagResource", PROBE_KEY, COMPONENT_TAGGED),
    // An alias call is authorized against both the alias and the key.
    write("kms:CreateAlias", PROBE_ALIAS, &[]),
    write("kms:CreateAlias", PROBE_KEY, COMPONENT_TAGGED),
    write("kms:UpdateAlias", PROBE_ALIAS, &[]),
    write("kms:UpdateAlias", PROBE_KEY, COMPONENT_TAGGED),
    write("kms:DeleteAlias", PROBE_ALIAS, &[]),
    write("kms:DeleteAlias", PROBE_KEY, COMPONENT_TAGGED),
    read("kms:DescribeKey", PROBE_KEY, &[]),
    read("kms:GetKeyPolicy", PROBE_KEY, &[]),
    read("kms:GetKeyRotationStatus", PROBE_KEY, &[]),
    read("kms:ListResourceTags", PROBE_KEY, &[]),
    read("kms:ListAliases", "*", &[]),
    // Logs, SSM, ECR, by name.
    write("logs:CreateLogGroup", PROBE_LOG_GROUP, COMPONENT_REQUESTED),
    write("logs:TagResource", PROBE_LOG_GROUP, COMPONENT_REQUESTED),
    write("logs:UntagResource", PROBE_LOG_GROUP, &[]),
    write("logs:DeleteLogGroup", PROBE_LOG_GROUP, &[]),
    write("logs:PutRetentionPolicy", PROBE_LOG_GROUP, &[]),
    write("logs:DeleteRetentionPolicy", PROBE_LOG_GROUP, &[]),
    read("logs:ListTagsForResource", PROBE_LOG_GROUP, &[]),
    read("logs:DescribeLogGroups", "*", &[]),
    write("ssm:PutParameter", PROBE_PARAMETER, COMPONENT_REQUESTED),
    write(
        "ssm:AddTagsToResource",
        PROBE_PARAMETER,
        COMPONENT_REQUESTED,
    ),
    write("ssm:RemoveTagsFromResource", PROBE_PARAMETER, &[]),
    write("ssm:DeleteParameter", PROBE_PARAMETER, &[]),
    read("ssm:GetParameter", PROBE_PARAMETER, &[]),
    read("ssm:ListTagsForResource", PROBE_PARAMETER, &[]),
    read("ssm:DescribeParameters", "*", &[]),
    read("sts:GetCallerIdentity", "*", &[]),
    // The state bucket (the app creates it; OpenTofu's S3 backend reads and
    // writes the state and lock objects).
    write("s3:CreateBucket", PROBE_STATE_BUCKET, STATE_REQUESTED),
    // CreateBucket with tags also authorizes s3:TagResource (verified live).
    write("s3:TagResource", PROBE_STATE_BUCKET, STATE_REQUESTED),
    write("s3:PutBucketVersioning", PROBE_STATE_BUCKET, IN_MY_ACCOUNT),
    write(
        "s3:PutEncryptionConfiguration",
        PROBE_STATE_BUCKET,
        IN_MY_ACCOUNT,
    ),
    write(
        "s3:PutBucketPublicAccessBlock",
        PROBE_STATE_BUCKET,
        IN_MY_ACCOUNT,
    ),
    write("s3:PutBucketTagging", PROBE_STATE_BUCKET, IN_MY_ACCOUNT),
    write("s3:PutBucketPolicy", PROBE_STATE_BUCKET, IN_MY_ACCOUNT),
    read("s3:GetBucketVersioning", PROBE_STATE_BUCKET, IN_MY_ACCOUNT),
    read("s3:ListBucket", PROBE_STATE_BUCKET, IN_MY_ACCOUNT),
    read("s3:GetObject", PROBE_STATE_OBJECT, IN_MY_ACCOUNT),
    write("s3:PutObject", PROBE_STATE_OBJECT, IN_MY_ACCOUNT),
    write("s3:DeleteObject", PROBE_STATE_OBJECT, IN_MY_ACCOUNT),
    // Per-user EC2 capacity (environment-admin-compute.json, phase 7): the
    // user's cluster is a `triple-c-*` cluster (above); its capacity
    // provider is scoped by name, like the cluster.
    write(
        "ecs:CreateCapacityProvider",
        PROBE_CAPACITY_PROVIDER,
        COMPONENT_REQUESTED,
    ),
    write(
        "ecs:TagResource",
        PROBE_CAPACITY_PROVIDER,
        &[("ecs:CreateAction", Literal("CreateCapacityProvider"))],
    ),
    write("ecs:UpdateCapacityProvider", PROBE_CAPACITY_PROVIDER, &[]),
    write("ecs:DeleteCapacityProvider", PROBE_CAPACITY_PROVIDER, &[]),
    write(
        "ecs:PutClusterCapacityProviders",
        PROBE_CAPACITY_PROVIDER,
        &[],
    ),
    write("ecs:UntagResource", PROBE_CAPACITY_PROVIDER, &[]),
    read("ecs:ListTagsForResource", PROBE_CAPACITY_PROVIDER, &[]),
    read("ecs:DescribeCapacityProviders", "*", &[]),
    // Launch templates: created with the component tag, changed, used and
    // deleted only when they carry it.
    write(
        "ec2:CreateLaunchTemplate",
        PROBE_LAUNCH_TEMPLATE,
        COMPONENT_REQUESTED,
    ),
    write(
        "ec2:CreateTags",
        PROBE_LAUNCH_TEMPLATE,
        &[("ec2:CreateAction", Literal("CreateLaunchTemplate"))],
    ),
    write(
        "ec2:CreateLaunchTemplateVersion",
        PROBE_LAUNCH_TEMPLATE,
        COMPONENT_TAGGED,
    ),
    write(
        "ec2:ModifyLaunchTemplate",
        PROBE_LAUNCH_TEMPLATE,
        COMPONENT_TAGGED,
    ),
    write(
        "ec2:DeleteLaunchTemplate",
        PROBE_LAUNCH_TEMPLATE,
        COMPONENT_TAGGED,
    ),
    write("ec2:CreateTags", PROBE_LAUNCH_TEMPLATE, COMPONENT_TAGGED),
    write("ec2:DeleteTags", PROBE_LAUNCH_TEMPLATE, COMPONENT_TAGGED),
    // Creating an Auto Scaling group checks that the caller could launch
    // its template: RunInstances, only from a Triple-C launch template and
    // only Amazon's images, and the tags the launch puts on.
    write("ec2:RunInstances", PROBE_LAUNCH_TEMPLATE, COMPONENT_TAGGED),
    write("ec2:RunInstances", PROBE_INSTANCE, FROM_LAUNCH_TEMPLATE),
    write("ec2:RunInstances", PROBE_VOLUME, FROM_LAUNCH_TEMPLATE),
    write(
        "ec2:RunInstances",
        PROBE_NETWORK_INTERFACE,
        FROM_LAUNCH_TEMPLATE,
    ),
    write("ec2:RunInstances", PROBE_SUBNET, FROM_LAUNCH_TEMPLATE),
    write(
        "ec2:RunInstances",
        PROBE_SECURITY_GROUP,
        FROM_LAUNCH_TEMPLATE,
    ),
    write(
        "ec2:RunInstances",
        PROBE_IMAGE,
        AMAZON_IMAGE_FROM_LAUNCH_TEMPLATE,
    ),
    write(
        "ec2:CreateTags",
        PROBE_INSTANCE,
        &[("ec2:CreateAction", Literal("RunInstances"))],
    ),
    write(
        "ec2:CreateTags",
        PROBE_VOLUME,
        &[("ec2:CreateAction", Literal("RunInstances"))],
    ),
    write(
        "ec2:CreateTags",
        PROBE_NETWORK_INTERFACE,
        &[("ec2:CreateAction", Literal("RunInstances"))],
    ),
    // Auto Scaling groups: `triple-c-*`, created with the component tag,
    // changed and deleted only when they carry it.
    write(
        "autoscaling:CreateAutoScalingGroup",
        PROBE_AUTO_SCALING_GROUP,
        COMPONENT_REQUESTED,
    ),
    write(
        "autoscaling:CreateOrUpdateTags",
        PROBE_AUTO_SCALING_GROUP,
        COMPONENT_REQUESTED,
    ),
    write(
        "autoscaling:CreateOrUpdateTags",
        PROBE_AUTO_SCALING_GROUP,
        COMPONENT_TAGGED,
    ),
    write(
        "autoscaling:UpdateAutoScalingGroup",
        PROBE_AUTO_SCALING_GROUP,
        COMPONENT_TAGGED,
    ),
    write(
        "autoscaling:SetInstanceProtection",
        PROBE_AUTO_SCALING_GROUP,
        COMPONENT_TAGGED,
    ),
    write(
        "autoscaling:DeleteTags",
        PROBE_AUTO_SCALING_GROUP,
        COMPONENT_TAGGED,
    ),
    write(
        "autoscaling:DeleteAutoScalingGroup",
        PROBE_AUTO_SCALING_GROUP,
        COMPONENT_TAGGED,
    ),
    read("autoscaling:DescribeAutoScalingGroups", "*", &[]),
    read("autoscaling:DescribeScalingActivities", "*", &[]),
    // The service-linked roles Auto Scaling and ECS (managed scaling) use,
    // created on first use; AWS fixes their policies.
    write(
        "iam:CreateServiceLinkedRole",
        PROBE_AUTO_SCALING_SERVICE_ROLE,
        &[("iam:AWSServiceName", Literal("autoscaling.amazonaws.com"))],
    ),
    write(
        "iam:CreateServiceLinkedRole",
        PROBE_ECS_SERVICE_ROLE,
        &[("iam:AWSServiceName", Literal("ecs.amazonaws.com"))],
    ),
    // The instance role (a bounded `triple-c-*` role, above) and its
    // instance profile, scoped by name.
    write("iam:CreateInstanceProfile", PROBE_INSTANCE_PROFILE, &[]),
    write("iam:TagInstanceProfile", PROBE_INSTANCE_PROFILE, &[]),
    write("iam:UntagInstanceProfile", PROBE_INSTANCE_PROFILE, &[]),
    write("iam:AddRoleToInstanceProfile", PROBE_INSTANCE_PROFILE, &[]),
    write(
        "iam:RemoveRoleFromInstanceProfile",
        PROBE_INSTANCE_PROFILE,
        &[],
    ),
    write("iam:DeleteInstanceProfile", PROBE_INSTANCE_PROFILE, &[]),
    read("iam:GetInstanceProfile", PROBE_INSTANCE_PROFILE, &[]),
    read("iam:ListInstanceProfileTags", PROBE_INSTANCE_PROFILE, &[]),
    write("iam:PassRole", PROBE_INSTANCE_ROLE, PASSED_TO_EC2),
    // The ECS-optimized AMI's public parameter, and the user's compute
    // record (`/triple-c/*`, above).
    read("ssm:GetParameter", PROBE_AMI_PARAMETER, &[]),
    read("ssm:GetParameters", PROBE_AMI_PARAMETER, &[]),
    write("ssm:PutParameter", PROBE_USER_RECORD, COMPONENT_REQUESTED),
    write(
        "ssm:AddTagsToResource",
        PROBE_USER_RECORD,
        COMPONENT_REQUESTED,
    ),
    write("ssm:DeleteParameter", PROBE_USER_RECORD, &[]),
    read("ssm:GetParameter", PROBE_USER_RECORD, &[]),
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::aws::policy_eval::{
        actions, conditions_hold, effect, evaluate_in, glob, matching, parse, rendered, resources,
        sid, statements_of, strings, Request, ME, SOMEONE_ELSE,
    };
    use serde_json::Value;
    use std::collections::BTreeSet;

    const TIERS: [Tier; 2] = [Tier::WorkspaceUser, Tier::EnvironmentAdmin];

    /// Every file of `tier`, parsed: (file name, document).
    fn policies(tier: Tier) -> Vec<(&'static str, Value)> {
        policy_files(tier)
            .iter()
            .map(|(name, json)| (*name, parse(name, json)))
            .collect()
    }

    fn boundary() -> Value {
        parse("permissions boundary", permissions_boundary_json())
    }

    /// The statements of every file of `tier`, which IAM evaluates together.
    fn statements(tier: Tier) -> Vec<Value> {
        policies(tier)
            .iter()
            .flat_map(|(_, doc)| statements_of(doc))
            .collect()
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

    fn evaluate(tier: Tier, req: Request) -> bool {
        evaluate_in(&statements(tier), req)
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
            for (name, doc) in policies(tier) {
                validate_document(name, &doc);
            }
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
        (
            "ecs:RegisterTaskDefinition",
            "task-definition/",
            OWNER_REQUEST_TAG,
        ),
        ("ecs:TagResource", "task-definition/", OWNER_REQUEST_TAG),
        (
            "secretsmanager:GetSecretValue",
            "secret:",
            OWNER_RESOURCE_TAG,
        ),
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
            "elasticfilesystem:CreateAccessPoint",
            "file-system/",
            OWNER_RESOURCE_TAG,
        ),
        (
            "elasticfilesystem:TagResource",
            "file-system/",
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

    /// The three tag keys a workspace user may put on anything they create.
    const OWN_TAG_KEYS: Context = &[
        ("aws:TagKeys", Literal("triple-c:owner")),
        ("aws:TagKeys", Literal("triple-c:project")),
        ("aws:TagKeys", Literal("triple-c:environment")),
    ];

    fn ws_check(action: &str, resource_part: &str) -> &'static Check {
        checks(Tier::WorkspaceUser)
            .iter()
            .find(|c| c.action == action && c.resource.contains(resource_part))
            .unwrap_or_else(|| panic!("no workspace-user check for {action} on {resource_part}"))
    }

    #[test]
    fn registering_a_task_definition_needs_the_owner_tag_and_only_the_three_keys() {
        let reg = ws_check("ecs:RegisterTaskDefinition", "task-definition/");
        const THREE_KEYS: Context = &[
            (OWNER_REQUEST_TAG, OwnerId),
            ("aws:TagKeys", Literal("triple-c:owner")),
            ("aws:TagKeys", Literal("triple-c:project")),
            ("aws:TagKeys", Literal("triple-c:environment")),
        ];
        const FOUR_KEYS: Context = &[
            (OWNER_REQUEST_TAG, OwnerId),
            ("aws:TagKeys", Literal("triple-c:owner")),
            ("aws:TagKeys", Literal("triple-c:project")),
            ("aws:TagKeys", Literal("triple-c:environment")),
            ("aws:TagKeys", Literal("extra")),
        ];
        let with_keys = |context: Context| Check { context, ..*reg };
        assert!(allowed(Tier::WorkspaceUser, &with_keys(THREE_KEYS), ME, ME));
        assert!(
            !allowed(Tier::WorkspaceUser, &with_keys(FOUR_KEYS), ME, ME),
            "an extra tag key must be refused"
        );
        let no_owner = Check {
            context: OWN_TAG_KEYS,
            ..*reg
        };
        assert!(
            !allowed(Tier::WorkspaceUser, &no_owner, ME, ME),
            "no owner request tag"
        );
        let someone_elses = Check {
            context: &[(OWNER_REQUEST_TAG, Literal(SOMEONE_ELSE))],
            ..*reg
        };
        assert!(!allowed(Tier::WorkspaceUser, &someone_elses, ME, ME));
        // Only Triple-C's families.
        let other_family = Check {
            resource: "arn:aws:ecs:{region}:{account}:task-definition/production:1",
            ..*reg
        };
        assert!(!allowed(Tier::WorkspaceUser, &other_family, ME, ME));
    }

    #[test]
    fn task_definitions_are_tagged_only_at_register_and_only_with_the_three_keys() {
        let tag = ws_check("ecs:TagResource", "task-definition/");
        assert!(allowed(Tier::WorkspaceUser, tag, ME, ME));
        let retag = Check {
            context: &[(OWNER_REQUEST_TAG, OwnerId)],
            ..*tag
        };
        assert!(!allowed(Tier::WorkspaceUser, &retag, ME, ME));
        let other_create = Check {
            context: &[
                (OWNER_REQUEST_TAG, OwnerId),
                ("ecs:CreateAction", Literal("RunTask")),
            ],
            ..*tag
        };
        assert!(!allowed(Tier::WorkspaceUser, &other_create, ME, ME));
        let extra_key = Check {
            context: &[
                (OWNER_REQUEST_TAG, OwnerId),
                ("ecs:CreateAction", Literal("RegisterTaskDefinition")),
                ("aws:TagKeys", Literal("triple-c:owner")),
                ("aws:TagKeys", Literal("triple-c:component")),
            ],
            ..*tag
        };
        assert!(
            !allowed(Tier::WorkspaceUser, &extra_key, ME, ME),
            "a task definition must not be tagged triple-c:component"
        );
    }

    /// R8: the workspace tier retires no task definition, not even its own
    /// (`DeregisterTaskDefinition` is authorized against `*` only).
    #[test]
    fn the_workspace_tier_never_retires_a_task_definition() {
        let register = ws_check("ecs:RegisterTaskDefinition", "task-definition/");
        for action in ["ecs:DeregisterTaskDefinition", "ecs:DeleteTaskDefinitions"] {
            assert!(
                !checks(Tier::WorkspaceUser).iter().any(|c| c.action == action),
                "{action} is checked for the workspace tier"
            );
            for resource in ["*", register.resource] {
                let own = Check {
                    action,
                    resource,
                    context: &[(OWNER_RESOURCE_TAG, OwnerId)],
                    ..*register
                };
                assert!(!allowed(Tier::WorkspaceUser, &own, ME, ME), "{action} on {resource}");
            }
        }
    }

    /// The cost estimate's one call (Q5), in the second workspace file: only
    /// `pricing:GetProducts`, on `*` (the API has no resource types), with no
    /// other Pricing action riding along.
    #[test]
    fn the_workspace_tier_reads_prices_and_nothing_else_of_pricing() {
        let (_, doc) = policies(Tier::WorkspaceUser)
            .into_iter()
            .find(|(name, _)| *name == "workspace-user-tasks.json")
            .unwrap();
        let pricing: Vec<Value> = statements_of(&doc)
            .into_iter()
            .filter(|s| actions(s).iter().any(|a| a.starts_with("pricing:")))
            .collect();
        assert_eq!(pricing.len(), 1);
        assert_eq!(actions(&pricing[0]), ["pricing:GetProducts"]);
        assert_eq!(resources(&pricing[0]), ["*"]);
        assert!(pricing[0].get("Condition").is_none());
        let c = ws_check("pricing:GetProducts", "*");
        assert!(!c.write);
        assert!(allowed(Tier::WorkspaceUser, c, ME, SOMEONE_ELSE));
        for action in ["pricing:DescribeServices", "pricing:GetPriceListFileUrl"] {
            let other = Check { action, ..*c };
            assert!(!allowed(Tier::WorkspaceUser, &other, ME, ME), "{action}");
        }
    }

    /// RegisterTaskDefinition with a task or execution role also authorizes
    /// `iam:PassRole` on each; `PassOwnTaskRoles` (owner-tagged, to ECS
    /// tasks, `triple-c-*` roles) must cover both and nothing else.
    #[test]
    fn register_task_definition_can_pass_the_users_task_and_execution_roles() {
        let pass = ws_check("iam:PassRole", "role/");
        for role in [
            "arn:aws:iam::{account}:role/triple-c-prod-0123456789ab-task",
            "arn:aws:iam::{account}:role/triple-c-prod-0123456789ab-exec",
        ] {
            let c = Check { resource: role, ..*pass };
            assert!(allowed(Tier::WorkspaceUser, &c, ME, ME), "{role}");
            assert!(!allowed(Tier::WorkspaceUser, &c, ME, SOMEONE_ELSE), "{role}");
        }
    }

    #[test]
    fn secrets_are_readable_only_by_their_owner_and_never_the_environments() {
        let get = ws_check("secretsmanager:GetSecretValue", "secret:");
        assert!(allowed(Tier::WorkspaceUser, get, ME, ME));
        assert!(!allowed(Tier::WorkspaceUser, get, ME, SOMEONE_ELSE));
        // Even tagged with the caller's owner ID, a component-tagged secret
        // (the environment's) is off limits: the Deny holds on its own.
        let environments = Check {
            context: &[
                (OWNER_RESOURCE_TAG, OwnerId),
                (COMPONENT_RESOURCE_TAG, Literal("environment")),
            ],
            ..*get
        };
        assert!(!allowed(Tier::WorkspaceUser, &environments, ME, ME));
        let component_only = Check {
            context: &[(COMPONENT_RESOURCE_TAG, Literal("environment"))],
            ..*get
        };
        let req = Request {
            check: &component_only,
            caller: ME,
            owner: ME,
        };
        let stmts = denies(Tier::WorkspaceUser);
        assert!(
            matching(&stmts, &req).iter().any(|s| conditions_hold(s, &req)),
            "missing a Deny of GetSecretValue on component-tagged secrets"
        );
        // Not outside Triple-C's secrets.
        let other = Check {
            resource: "arn:aws:secretsmanager:{region}:{account}:secret:prod/db-AbCdEf",
            ..*get
        };
        assert!(!allowed(Tier::WorkspaceUser, &other, ME, ME));
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
        let mut docs: Vec<(&str, Value)> = TIERS.into_iter().flat_map(policies).collect();
        docs.push(("permissions-boundary.json", boundary()));
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
    /// environment admin needs every file of both tiers, which together
    /// exceed a role's 10,240-character inline budget, so they ship as
    /// managed policies, the admin tier in several (D7).
    #[test]
    fn every_policy_fits_a_managed_policy() {
        let mut files: Vec<(&str, &str)> = TIERS
            .into_iter()
            .flat_map(|t| policy_files(t).iter().copied())
            .collect();
        files.push(("permissions-boundary.json", permissions_boundary_json()));
        for (name, text) in files {
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
            // The ECS agent on a user's instance (E4).
            "ecs:RegisterContainerInstance",
            "ecs:DeregisterContainerInstance",
            "ecs:DiscoverPollEndpoint",
            "ecs:Poll",
            "ecs:StartTelemetrySession",
            "ecs:SubmitTaskStateChange",
            "ecs:SubmitContainerStateChange",
            "ecs:SubmitAttachmentStateChanges",
            "ecs:UpdateContainerInstancesState",
            "ecs:TagResource",
            // Session Manager on the instance.
            "ssm:UpdateInstanceInformation",
            "ec2messages:GetMessages",
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

    /// The instance role's ECS agent (E4) reaches only Triple-C clusters and
    /// their container instances, and tags only the instance it registers.
    #[test]
    fn the_boundary_keeps_the_ecs_agent_on_triple_c_clusters() {
        let stmts = statements_of(&boundary());
        let within = |action: &'static str, resource: &'static str, context: Context| {
            evaluate_in(
                &stmts,
                Request {
                    check: &write(action, resource, context),
                    caller: ME,
                    owner: ME,
                },
            )
        };
        const INSTANCE: &str = "arn:aws:ecs:{region}:{account}:container-instance/\
             triple-c-probe-0123456789ab/0123456789abcdef0123456789abcdef";
        const OTHER_CLUSTER: &str = "arn:aws:ecs:{region}:{account}:cluster/prod";
        const OTHER_INSTANCE: &str = "arn:aws:ecs:{region}:{account}:container-instance/\
             prod/0123456789abcdef0123456789abcdef";
        const AT_REGISTER: Context = &[("ecs:CreateAction", Literal("RegisterContainerInstance"))];
        for action in [
            "ecs:RegisterContainerInstance",
            "ecs:DeregisterContainerInstance",
            "ecs:SubmitTaskStateChange",
        ] {
            assert!(within(action, PROBE_CLUSTER, &[]), "{action}");
            assert!(!within(action, OTHER_CLUSTER, &[]), "{action} elsewhere");
        }
        for action in [
            "ecs:Poll",
            "ecs:StartTelemetrySession",
            "ecs:UpdateContainerInstancesState",
        ] {
            assert!(within(action, INSTANCE, &[]), "{action}");
            assert!(!within(action, OTHER_INSTANCE, &[]), "{action} elsewhere");
        }
        assert!(within("ecs:TagResource", INSTANCE, AT_REGISTER));
        assert!(!within("ecs:TagResource", OTHER_INSTANCE, AT_REGISTER));
        for (resource, context) in [
            (INSTANCE, &[][..]),
            (PROBE_CLUSTER, AT_REGISTER),
            (PROBE_TASK, &[][..]),
        ] {
            assert!(
                !within("ecs:TagResource", resource, context),
                "ecs:TagResource on {resource} with {context:?}"
            );
        }
        // The agent never runs, stops or changes tasks or clusters itself.
        for action in [
            "ecs:RunTask",
            "ecs:StartTask",
            "ecs:StopTask",
            "ecs:CreateCluster",
            "ecs:DeleteCluster",
            "ecs:ExecuteCommand",
        ] {
            assert!(!within(action, PROBE_CLUSTER, &[]), "{action}");
            assert!(!within(action, PROBE_TASK, &[]), "{action}");
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

    /// Strict tenancy (D2): each user has their own file system, tagged
    /// with their owner ID by their onboarding module, and may create access
    /// points only there.
    #[test]
    fn access_points_only_on_the_users_own_file_system() {
        let c = checks(Tier::WorkspaceUser)
            .iter()
            .find(|c| c.action == "elasticfilesystem:CreateAccessPoint")
            .unwrap();
        assert!(allowed(Tier::WorkspaceUser, c, ME, ME));
        let foreign_fs = Check {
            context: &[(OWNER_REQUEST_TAG, OwnerId)],
            ..*c
        };
        assert!(!allowed(Tier::WorkspaceUser, &foreign_fs, ME, ME));
        // Someone else's onboarding file system, with my owner tag requested.
        let someone_elses_fs = Check {
            context: &[
                (OWNER_REQUEST_TAG, OwnerId),
                (OWNER_RESOURCE_TAG, Literal(SOMEONE_ELSE)),
                (COMPONENT_RESOURCE_TAG, Literal("user-onboarding")),
            ],
            ..*c
        };
        assert!(!allowed(Tier::WorkspaceUser, &someone_elses_fs, ME, ME));
        // An owner-tagged file system that isn't Triple-C's.
        let not_triple_c = Check {
            context: &[(OWNER_REQUEST_TAG, OwnerId), (OWNER_RESOURCE_TAG, OwnerId)],
            ..*c
        };
        assert!(!allowed(Tier::WorkspaceUser, &not_triple_c, ME, ME));
    }

    #[test]
    fn the_admin_tier_spans_its_files() {
        let names = |t: Tier| policy_files(t).iter().map(|(n, _)| *n).collect::<Vec<_>>();
        assert_eq!(
            names(Tier::EnvironmentAdmin),
            [
                "environment-admin.json",
                "environment-admin-network.json",
                "environment-admin-compute.json"
            ]
        );
        assert_eq!(
            names(Tier::WorkspaceUser),
            ["workspace-user.json", "workspace-user-tasks.json"]
        );
    }

    /// D8: CloudFormation leaves the policies, the checks and the probes.
    #[test]
    fn no_policy_mentions_cloudformation() {
        let mut texts: Vec<(&str, &str)> = TIERS
            .into_iter()
            .flat_map(|t| policy_files(t).iter().copied())
            .collect();
        texts.push(("permissions-boundary.json", permissions_boundary_json()));
        for (name, text) in texts {
            assert!(
                !text.to_ascii_lowercase().contains("cloudformation"),
                "{name} mentions CloudFormation"
            );
        }
        for tier in TIERS {
            for c in checks(tier) {
                assert!(
                    !c.action.starts_with("cloudformation:")
                        && !c.resource.contains("cloudformation"),
                    "{tier:?}: check {} on {}",
                    c.action,
                    c.resource
                );
            }
        }
    }

    /// The network the admin creates is tag-scoped like everything else: an
    /// untagged VPC, subnet, gateway or route table is someone else's, and
    /// the only bucket the admin may touch is the state bucket.
    #[test]
    fn the_admin_still_cannot_touch_untagged_resources() {
        let tier = Tier::EnvironmentAdmin;
        for (action, resource) in [
            ("ec2:DeleteVpc", PROBE_VPC),
            ("ec2:ModifyVpcAttribute", PROBE_VPC),
            ("ec2:DeleteSubnet", PROBE_SUBNET),
            ("ec2:DeleteInternetGateway", PROBE_INTERNET_GATEWAY),
            ("ec2:DetachInternetGateway", PROBE_INTERNET_GATEWAY),
            ("ec2:DeleteRouteTable", PROBE_ROUTE_TABLE),
            ("ec2:CreateRoute", PROBE_ROUTE_TABLE),
            ("ec2:DeleteRoute", PROBE_ROUTE_TABLE),
            ("ec2:AssociateRouteTable", PROBE_ROUTE_TABLE),
            ("ec2:DeleteNatGateway", PROBE_NAT_GATEWAY),
            ("ec2:ReleaseAddress", PROBE_ELASTIC_IP),
            ("ec2:DeleteSecurityGroup", PROBE_SECURITY_GROUP),
            ("ec2:CreateTags", PROBE_VPC),
            ("ec2:DeleteTags", PROBE_SUBNET),
        ] {
            let tagged = write(action, resource, COMPONENT_TAGGED);
            assert!(
                allowed(tier, &tagged, ME, ME),
                "{action} on a tagged {resource}"
            );
            let untagged = write(action, resource, &[]);
            assert!(
                !allowed(tier, &untagged, ME, ME),
                "{action} on an untagged {resource}"
            );
        }
        // Creates need the component request tag.
        for (action, resource) in [
            ("ec2:CreateVpc", PROBE_VPC),
            ("ec2:CreateSubnet", PROBE_SUBNET),
            ("ec2:CreateInternetGateway", PROBE_INTERNET_GATEWAY),
            ("ec2:CreateRouteTable", PROBE_ROUTE_TABLE),
            ("ec2:AllocateAddress", PROBE_ELASTIC_IP),
            ("ec2:CreateNatGateway", PROBE_NAT_GATEWAY),
        ] {
            assert!(
                allowed(tier, &write(action, resource, COMPONENT_REQUESTED), ME, ME),
                "{action}"
            );
            assert!(
                !allowed(tier, &write(action, resource, &[]), ME, ME),
                "untagged {action}"
            );
        }
        // Tags go on at create only through the creating call.
        let tag_at = |create: &'static str| -> Check {
            let ctx: Context =
                Box::leak(vec![("ec2:CreateAction", Literal(create))].into_boxed_slice());
            write("ec2:CreateTags", PROBE_VPC, ctx)
        };
        assert!(allowed(tier, &tag_at("CreateVpc"), ME, ME));
        assert!(!allowed(tier, &tag_at("RunInstances"), ME, ME));
        // A mount target's interface is EFS's doing, never a direct call, nor
        // another service's.
        for action in ["ec2:CreateNetworkInterface", "ec2:DeleteNetworkInterface"] {
            assert!(
                !allowed(tier, &write(action, PROBE_NETWORK_INTERFACE, &[]), ME, ME),
                "{action}"
            );
            let via_lambda: Context = &[("aws:CalledViaLast", Literal("lambda.amazonaws.com"))];
            assert!(
                !allowed(
                    tier,
                    &write(action, PROBE_NETWORK_INTERFACE, via_lambda),
                    ME,
                    ME
                ),
                "{action} via another service"
            );
        }
        // Only the state bucket is managed, and only in the caller's account
        // (the name is predictable, so another account could pre-create it).
        const OTHER_BUCKET: &str = "arn:aws:s3:::someone-elses-bucket";
        const OTHER_ACCOUNT: Context = &[
            ("s3:ResourceAccount", Literal("999999999999")),
            ("aws:PrincipalAccount", Literal("{account}")),
        ];
        const OTHER_ACCOUNT_REQUESTED: Context = &[
            ("aws:RequestTag/triple-c:component", Literal("state")),
            ("s3:ResourceAccount", Literal("999999999999")),
            ("aws:PrincipalAccount", Literal("{account}")),
        ];
        // No bucket may be deleted.
        for bucket in [PROBE_STATE_BUCKET, OTHER_BUCKET] {
            for context in [&[][..], COMPONENT_TAGGED, IN_MY_ACCOUNT] {
                assert!(
                    !allowed(tier, &write("s3:DeleteBucket", bucket, context), ME, ME),
                    "s3:DeleteBucket on {bucket}"
                );
            }
        }
        let state_calls = [
            ("s3:PutBucketPolicy", PROBE_STATE_BUCKET),
            ("s3:PutBucketVersioning", PROBE_STATE_BUCKET),
            ("s3:PutEncryptionConfiguration", PROBE_STATE_BUCKET),
            ("s3:PutBucketPublicAccessBlock", PROBE_STATE_BUCKET),
            ("s3:PutBucketTagging", PROBE_STATE_BUCKET),
            ("s3:ListBucket", PROBE_STATE_BUCKET),
            ("s3:GetObject", PROBE_STATE_OBJECT),
            ("s3:PutObject", PROBE_STATE_OBJECT),
            ("s3:DeleteObject", PROBE_STATE_OBJECT),
        ];
        for (action, resource) in state_calls {
            assert!(
                allowed(tier, &write(action, resource, IN_MY_ACCOUNT), ME, ME),
                "{action}"
            );
            for context in [&[][..], OTHER_ACCOUNT] {
                assert!(
                    !allowed(tier, &write(action, resource, context), ME, ME),
                    "{action} on a state bucket with {context:?}"
                );
            }
            let elsewhere = resource.replace(
                "triple-c-tfstate-{account}-{region}",
                "someone-elses-bucket",
            );
            let elsewhere: &'static str = Box::leak(elsewhere.into_boxed_str());
            assert!(
                !allowed(tier, &write(action, elsewhere, IN_MY_ACCOUNT), ME, ME),
                "{action} on {elsewhere}"
            );
        }
        for action in ["s3:CreateBucket", "s3:TagResource"] {
            assert!(
                allowed(tier, &write(action, PROBE_STATE_BUCKET, STATE_REQUESTED), ME, ME),
                "{action}"
            );
            for (bucket, context) in [
                (PROBE_STATE_BUCKET, IN_MY_ACCOUNT),
                (PROBE_STATE_BUCKET, OTHER_ACCOUNT_REQUESTED),
                (OTHER_BUCKET, STATE_REQUESTED),
            ] {
                assert!(
                    !allowed(tier, &write(action, bucket, context), ME, ME),
                    "{action} {bucket} with {context:?}"
                );
            }
        }
    }

    /// Network resources are created only inside the environment's own
    /// (component-tagged) VPC, subnets and addresses: a tagged subnet,
    /// route table, gateway or NAT gateway in someone else's VPC would
    /// re-route it. The one exception is a security group, which an
    /// `existing`-network environment creates in the user's own VPC.
    #[test]
    fn network_creates_need_a_triple_c_parent() {
        let tier = Tier::EnvironmentAdmin;
        for (action, parent) in [
            ("ec2:CreateSubnet", PROBE_VPC),
            ("ec2:CreateRouteTable", PROBE_VPC),
            ("ec2:CreateNatGateway", PROBE_VPC),
            ("ec2:CreateNatGateway", PROBE_SUBNET),
            ("ec2:CreateNatGateway", PROBE_ELASTIC_IP),
        ] {
            assert!(
                allowed(
                    tier,
                    &write(action, parent, REQUESTED_IN_TRIPLE_C_PARENT),
                    ME,
                    ME
                ),
                "{action} in a Triple-C {parent}"
            );
            assert!(
                !allowed(tier, &write(action, parent, COMPONENT_REQUESTED), ME, ME),
                "{action} in an untagged {parent}"
            );
        }
        // Attaching a gateway (and routing through it) needs the VPC tagged.
        for (action, resource) in [
            ("ec2:AttachInternetGateway", PROBE_VPC),
            ("ec2:CreateRoute", PROBE_ROUTE_TABLE),
            ("ec2:AssociateRouteTable", PROBE_ROUTE_TABLE),
            ("ec2:AssociateRouteTable", PROBE_SUBNET),
        ] {
            assert!(
                !allowed(tier, &write(action, resource, COMPONENT_REQUESTED), ME, ME),
                "{action} on an untagged {resource}"
            );
        }
        // A group may be created in any VPC (EC2 shows the VPC no request
        // tag), but the group itself must carry the component tag.
        assert!(allowed(
            tier,
            &write("ec2:CreateSecurityGroup", PROBE_VPC, &[]),
            ME,
            ME
        ));
        assert!(!allowed(
            tier,
            &write("ec2:CreateSecurityGroup", PROBE_SECURITY_GROUP, &[]),
            ME,
            ME
        ));
        // Only the literal `*/*` of an association that is already gone. That
        // a live association is authorized against its address and interface
        // ARNs (so never matches) is expected, NOT live-verified: see "Still
        // open" in aws/README.md for the probe.
        assert!(allowed(
            tier,
            &write("ec2:DisassociateAddress", PROBE_GONE_ASSOCIATION, &[]),
            ME,
            ME
        ));
        for resource in [PROBE_ELASTIC_IP, PROBE_NETWORK_INTERFACE] {
            for context in [&[][..], COMPONENT_TAGGED] {
                assert!(
                    !allowed(tier, &write("ec2:DisassociateAddress", resource, context), ME, ME),
                    "ec2:DisassociateAddress on {resource} with {context:?}"
                );
            }
        }
        // A VPC's main route table association names no subnet, so swapping
        // it would re-route a VPC by its route table alone: never allowed.
        assert!(!allowed(
            tier,
            &write(
                "ec2:ReplaceRouteTableAssociation",
                PROBE_ROUTE_TABLE,
                COMPONENT_TAGGED
            ),
            ME,
            ME
        ));
        // Only an instance role is passed to EC2 (phase 7); task and
        // execution roles go to ECS tasks only.
        assert!(!allowed(
            tier,
            &write("iam:PassRole", PROBE_ROLE, PASSED_TO_EC2),
            ME,
            ME
        ));
    }

    /// The new compute checks of the admin tier.
    fn compute_checks() -> Vec<&'static Check> {
        let compute = parse(
            "environment-admin-compute.json",
            policy_files(Tier::EnvironmentAdmin)
                .iter()
                .find(|(n, _)| *n == "environment-admin-compute.json")
                .expect("the compute file")
                .1,
        );
        let stmts = statements_of(&compute);
        let found: Vec<_> = checks(Tier::EnvironmentAdmin)
            .iter()
            .filter(|c| {
                let req = Request {
                    check: c,
                    caller: ME,
                    owner: ME,
                };
                !matching(&stmts, &req).is_empty()
            })
            .collect();
        assert!(found.len() >= 30, "only {} compute checks", found.len());
        found
    }

    /// Per-user EC2 capacity (E5): what the admin creates carries the
    /// component tag, and what it changes, uses or deletes must carry it;
    /// another (untagged, or not `triple-c-*`) launch template, Auto Scaling
    /// group, capacity provider or instance profile is out of reach.
    #[test]
    fn per_user_compute_is_tag_and_name_scoped() {
        let tier = Tier::EnvironmentAdmin;
        for (action, resource) in [
            ("ec2:CreateLaunchTemplate", PROBE_LAUNCH_TEMPLATE),
            (
                "autoscaling:CreateAutoScalingGroup",
                PROBE_AUTO_SCALING_GROUP,
            ),
        ] {
            assert!(allowed(
                tier,
                &write(action, resource, COMPONENT_REQUESTED),
                ME,
                ME
            ));
            assert!(
                !allowed(tier, &write(action, resource, &[]), ME, ME),
                "untagged {action}"
            );
        }
        for (action, resource) in [
            ("ec2:CreateLaunchTemplateVersion", PROBE_LAUNCH_TEMPLATE),
            ("ec2:ModifyLaunchTemplate", PROBE_LAUNCH_TEMPLATE),
            ("ec2:DeleteLaunchTemplate", PROBE_LAUNCH_TEMPLATE),
            ("ec2:CreateTags", PROBE_LAUNCH_TEMPLATE),
            ("ec2:DeleteTags", PROBE_LAUNCH_TEMPLATE),
            ("ec2:RunInstances", PROBE_LAUNCH_TEMPLATE),
            (
                "autoscaling:UpdateAutoScalingGroup",
                PROBE_AUTO_SCALING_GROUP,
            ),
            (
                "autoscaling:SetInstanceProtection",
                PROBE_AUTO_SCALING_GROUP,
            ),
            ("autoscaling:CreateOrUpdateTags", PROBE_AUTO_SCALING_GROUP),
            ("autoscaling:DeleteTags", PROBE_AUTO_SCALING_GROUP),
            (
                "autoscaling:DeleteAutoScalingGroup",
                PROBE_AUTO_SCALING_GROUP,
            ),
        ] {
            assert!(
                allowed(tier, &write(action, resource, COMPONENT_TAGGED), ME, ME),
                "{action} on a tagged {resource}"
            );
            assert!(
                !allowed(tier, &write(action, resource, &[]), ME, ME),
                "{action} on an untagged {resource}"
            );
        }
        // Named resources: someone else's names are out of reach, tagged or not.
        let others = [
            (
                "autoscaling:DeleteAutoScalingGroup",
                "arn:aws:autoscaling:{region}:{account}:autoScalingGroup:\
                 00000000-0000-0000-0000-000000000000:autoScalingGroupName/prod-web",
            ),
            (
                "ecs:DeleteCapacityProvider",
                "arn:aws:ecs:{region}:{account}:capacity-provider/prod-web",
            ),
            (
                "ecs:UpdateCapacityProvider",
                "arn:aws:ecs:{region}:{account}:capacity-provider/prod-web",
            ),
            (
                "ecs:PutClusterCapacityProviders",
                "arn:aws:ecs:{region}:{account}:cluster/prod",
            ),
            (
                "iam:DeleteInstanceProfile",
                "arn:aws:iam::{account}:instance-profile/prod-web",
            ),
            (
                "iam:AddRoleToInstanceProfile",
                "arn:aws:iam::{account}:instance-profile/prod-web",
            ),
            (
                "iam:RemoveRoleFromInstanceProfile",
                "arn:aws:iam::{account}:instance-profile/prod-web",
            ),
        ];
        for (action, resource) in others {
            for context in [&[][..], COMPONENT_TAGGED] {
                assert!(
                    !allowed(tier, &write(action, resource, context), ME, ME),
                    "{action} on {resource}"
                );
            }
        }
        // Tags go on at create only through the creating call.
        for resource in [PROBE_LAUNCH_TEMPLATE, PROBE_INSTANCE, PROBE_VOLUME] {
            let other: Context = &[("ec2:CreateAction", Literal("CreateVolume"))];
            assert!(
                !allowed(tier, &write("ec2:CreateTags", resource, other), ME, ME),
                "CreateTags on {resource} through CreateVolume"
            );
        }
        // The user's record lives under /triple-c/; nothing else's does.
        assert!(!allowed(
            tier,
            &write(
                "ssm:DeleteParameter",
                "arn:aws:ssm:{region}:{account}:parameter/prod/users/x",
                &[]
            ),
            ME,
            ME
        ));
    }

    /// RunInstances (which creating an Auto Scaling group checks the caller
    /// for) works only from a Triple-C launch template, with Amazon's
    /// images: never a bare launch, nor someone else's template or image.
    #[test]
    fn instances_launch_only_from_triple_c_launch_templates() {
        let tier = Tier::EnvironmentAdmin;
        for resource in [
            PROBE_INSTANCE,
            PROBE_VOLUME,
            PROBE_NETWORK_INTERFACE,
            PROBE_SUBNET,
            PROBE_SECURITY_GROUP,
        ] {
            assert!(allowed(
                tier,
                &write("ec2:RunInstances", resource, FROM_LAUNCH_TEMPLATE),
                ME,
                ME
            ));
            for context in [&[][..], COMPONENT_REQUESTED, COMPONENT_TAGGED] {
                assert!(
                    !allowed(tier, &write("ec2:RunInstances", resource, context), ME, ME),
                    "RunInstances on {resource} without a launch template ({context:?})"
                );
            }
        }
        assert!(!allowed(
            tier,
            &write("ec2:RunInstances", PROBE_LAUNCH_TEMPLATE, &[]),
            ME,
            ME
        ));
        let someone_elses_image: Context = &[
            ("ec2:Owner", Literal("999999999999")),
            (
                "ec2:LaunchTemplate",
                Literal("arn:aws:ec2:{region}:{account}:launch-template/lt-00000000000000000"),
            ),
        ];
        for context in [&[][..], FROM_LAUNCH_TEMPLATE, someone_elses_image] {
            assert!(
                !allowed(
                    tier,
                    &write("ec2:RunInstances", PROBE_IMAGE, context),
                    ME,
                    ME
                ),
                "RunInstances with an image and {context:?}"
            );
        }
        // Other resource types a launch could name (a key pair, an
        // instance-store snapshot, ...) are not granted at all.
        for resource in [
            "arn:aws:ec2:{region}:{account}:key-pair/k",
            "arn:aws:ec2:{region}::snapshot/snap-00000000000000000",
        ] {
            assert!(!allowed(
                tier,
                &write("ec2:RunInstances", resource, FROM_LAUNCH_TEMPLATE),
                ME,
                ME
            ));
        }
    }

    /// Roles go to EC2 only as an instance role (`triple-c-*-instance`),
    /// and never anything else's; service-linked roles only for Auto
    /// Scaling and ECS.
    #[test]
    fn instance_roles_are_the_only_roles_passed_to_ec2() {
        let tier = Tier::EnvironmentAdmin;
        assert!(allowed(
            tier,
            &write("iam:PassRole", PROBE_INSTANCE_ROLE, PASSED_TO_EC2),
            ME,
            ME
        ));
        for role in [
            PROBE_ROLE,
            "arn:aws:iam::{account}:role/triple-c-probe-0123456789ab-task",
            "arn:aws:iam::{account}:role/admin-instance",
            "arn:aws:iam::{account}:role/OrganizationAccountAccessRole",
        ] {
            assert!(
                !allowed(tier, &write("iam:PassRole", role, PASSED_TO_EC2), ME, ME),
                "{role} passed to EC2"
            );
        }
        for service in ["lambda.amazonaws.com", "ecs-tasks.amazonaws.com"] {
            let ctx: Context =
                Box::leak(vec![("iam:PassedToService", Literal(service))].into_boxed_slice());
            let instance_to_other = write("iam:PassRole", PROBE_INSTANCE_ROLE, ctx);
            // An instance role is still a `triple-c-*` role, which the
            // phase-5 statement lets the admin pass to ECS tasks; nothing
            // else.
            assert_eq!(
                allowed(tier, &instance_to_other, ME, ME),
                service == "ecs-tasks.amazonaws.com",
                "{service}"
            );
        }
        // (A service's role ARN follows from its name, so one statement
        // covers both services.)
        for (role, service) in [
            (
                "arn:aws:iam::{account}:role/aws-service-role/\
                 elasticbeanstalk.amazonaws.com/AWSServiceRoleForElasticBeanstalk",
                "elasticbeanstalk.amazonaws.com",
            ),
            (PROBE_ROLE, "autoscaling.amazonaws.com"),
        ] {
            let ctx: Context =
                Box::leak(vec![("iam:AWSServiceName", Literal(service))].into_boxed_slice());
            assert!(
                !allowed(
                    tier,
                    &write("iam:CreateServiceLinkedRole", role, ctx),
                    ME,
                    ME
                ),
                "service-linked {role} for {service}"
            );
        }
    }

    /// A workspace user changes none of the compute (they may read, as
    /// they read all of ECS): their EC2 capacity is the admin's onboarding
    /// module's to create.
    #[test]
    fn the_workspace_tier_cannot_manage_compute() {
        for c in compute_checks().into_iter().filter(|c| c.write) {
            assert!(
                !allowed(Tier::WorkspaceUser, c, ME, ME),
                "workspace user: {} on {}",
                c.action,
                rendered(c)
            );
        }
    }

    const OTHER_BOUNDARY: Context = &[
        (
            "iam:PermissionsBoundary",
            Literal("arn:aws:iam::{account}:policy/SomethingElse"),
        ),
        ("aws:PrincipalAccount", Literal("{account}")),
    ];

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
