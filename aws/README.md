# Triple-C AWS policies

These are the IAM policies for running Triple-C workspaces on AWS (ECS). Attach them to the IAM
users, groups, roles or SSO permission sets of the people who use Triple-C. The app reports which
tier the signed-in identity has under **Settings → Hosts → Check**.

| File | What it is |
|---|---|
| `policies/workspace-user.json` | The **workspace user** tier, part 1: create, start, stop, connect to and delete **their own** workspaces in an existing environment |
| `policies/workspace-user-tasks.json` | The **workspace user** tier, part 2: register and tag **their own** ECS task definitions, read **their own** secrets, and read Fargate prices for the cost estimate (`workspace-user.json` has no room left) |
| `policies/environment-admin.json` | The **environment admin** tier, part 1: apply and destroy the environment and onboarding modules (`terraform/`) with OpenTofu, and keep their state |
| `policies/environment-admin-network.json` | The **environment admin** tier, part 2: the network those modules create (VPC, subnets, gateways, routes, security groups) |
| `policies/environment-admin-compute.json` | The **environment admin** tier, part 3: per-user EC2 capacity the onboarding module creates (phase 7): capacity providers, launch templates, Auto Scaling groups, instance profiles, the instance role passed to EC2 |
| `policies/permissions-boundary.json` | The **permissions boundary** every Triple-C role must carry. It is never attached to a person |

The workspace tier is **two** files and the admin tier **three**, because one managed policy
holds only 6,144 characters; every file of a tier is required, and the app's **Copy** lists every
file of a tier. An environment admin attaches all five tier files (both workspace user files and
the three admin files): the admin files add environment management on top of everything a
workspace user can do. Explicit Deny statements in any file apply to anyone
who has it attached.

## Workspace user

Every per-user resource is tagged `triple-c:owner = ${aws:userid}`. That value is the `UserId`
printed by `aws sts get-caller-identity`. The policy uses this tag so that a user can only do the
following:

- **Run tasks.** The task definition must be a `triple-c-*` one tagged with the user's own owner
  ID, and the cluster must be a `triple-c-*` one. The new task is tagged with the user's owner ID.
  The user can pass only `triple-c-*` roles tagged with their owner ID, and only to ECS tasks.
- **Register their own task definitions** (`workspace-user-tasks.json`). `ecs:RegisterTaskDefinition`
  on `triple-c-*` families needs `triple-c:owner = ${aws:userid}` in the request and only the three
  ordinary tag keys. `ecs:TagResource` on a task definition is allowed only at registration
  (`ecs:CreateAction = RegisterTaskDefinition`, same owner and tag-key conditions), so a
  definition can never be retagged. Users **cannot retire** task definitions, not even their own
  (ruling R8): ECS authorizes `ecs:DeregisterTaskDefinition` against `*` only, with no tag
  condition, so allowing it would let any user deregister any revision in the account. Removing a
  project leaves its revisions registered (they hold no secret values and cost nothing, and
  `RunOwnTasks` still needs the owner tag, so nobody else can run them); an admin clears them
  (see "Clearing workspace task definitions"). A task definition that names a task or
  execution role is also authorized with `iam:PassRole` on it, which `PassOwnTaskRoles` already
  covers (owner-tagged `triple-c-*` roles, to ECS tasks only; a unit test checks both roles).
  `RunOwnTasks` still requires the definition's owner tag, so a user can run only definitions they
  registered. The `*` in `task-definition/triple-c-*` matches `family:revision` too.
  - **Residual risk: polluting another owner's family.** The family name carries the owner key,
    which IAM cannot condition on, so any workspace user can register a *revision* into another
    owner's `triple-c-*` family and make it the family's latest. It cannot run or steal anything
    (`RunOwnTasks` needs the owner tag, and the new revision carries the attacker's), and the
    policy cannot close this. The app's mitigation: it always runs the exact `family:revision`
    ARN its own register call returned, and when it looks up an existing revision it accepts
    only one tagged with the caller's own owner ID, never "latest of the family".
- **Read their own secrets.** `secretsmanager:GetSecretValue` on `triple-c/*` secrets tagged with
  their owner ID, so a second machine can read the agent token and attach to a running task
  (`ReadOwnSecrets`). An explicit Deny (`NeverReadEnvironmentSecrets`) refuses it on any secret
  tagged `triple-c:component`, even one that also carries their owner tag. (`NeverTouchEnvironmentSecrets`
  in `workspace-user.json` covers writes; it has no room for another action.) Reading also
  needs `kms:Decrypt` through Secrets Manager on the environment key, which the first file grants.
- **Work with their own tasks.** They can stop them, `ecs execute-command` into them, and open an
  SSM session to them. They can end or resume only sessions they started, and open data channels
  (`ssmmessages:OpenDataChannel`) only on those.
  - `ecs:ExecuteCommand` is authorized against the **cluster** as well as the task, so the policy
    also allows it on `triple-c-*` clusters with no condition. The owner rule is on the task, and
    both must pass.
- **Manage their own secrets.** They can create, update and delete `triple-c/*` secrets tagged with
  their owner ID.
  - A secret must be encrypted with an **environment KMS key**, meaning one tagged
    `triple-c:component`. `CreateSecret` must name the key by its full ARN in `KmsKeyId` (a key
    ID, an alias or no key at all is refused), and the user can use KMS
    (`Encrypt`/`GenerateDataKey`/`Decrypt`) only through Secrets Manager and only with environment
    keys. Secrets Manager needs `kms:Decrypt` as well as `GenerateDataKey` to create or update a
    secret on a customer key.
  - A secret on any other key is refused by an **explicit Deny**: `kms:Encrypt` and
    `kms:GenerateDataKey*` through Secrets Manager on a key without `triple-c:component`. The Deny
    is needed because the AWS-managed `aws/secretsmanager` key, or any customer key whose key
    policy grants account principals directly, needs no identity-policy Allow; without the Deny
    such a `CreateSecret` succeeds. Keeping every user secret on the environment key keeps it
    under the key policy the environment module controls. (The permissions boundary does *not*
    stop a task role decrypting an `aws/secretsmanager` secret: that key's policy grants the
    caller directly.)
  - Every `TagResource` on a secret must carry `triple-c:owner = ${aws:userid}` in the request,
    even when it only adds `triple-c:project`.
  - A secret tagged `triple-c:component` belongs to the environment. A workspace user can never
    tag, untag, overwrite or delete one; an explicit Deny enforces this.
- **Manage their own access points.** They can create and delete EFS access points tagged with
  their owner ID, and create them only on **their own** file system: one tagged
  `triple-c:component` (their onboarding module tags it `user-onboarding`) *and*
  `triple-c:owner = ${aws:userid}`. Each user has their own file system (strict tenancy), so
  another user's file system is refused even though it is Triple-C's. The `TagResource` that a
  tagged `CreateAccessPoint` needs is authorized against the **file system**, not the new access
  point (verified live, phase 5), so `TagOwnAccessPointsAtCreate` names `file-system/*` and is
  confined by `elasticfilesystem:CreateAction = CreateAccessPoint`.
- **What removing a project removes.** Removing a project (or only its AWS resources) stops its
  task and deletes its secret **at once** (`ForceDeleteWithoutRecovery`, ruling R10: the app
  rebuilds it from local data, and a secret left pending deletion would make the next start's
  `CreateSecret` fail). "Also remove the workspace's file-system access point" deletes only the
  **access point** (ruling R9): the files under it (the Claude login, history, SSH keys) stay on
  the user's own EFS file system, which only their own task role can read, until the user is
  offboarded (destroying their onboarding module removes the file system). Starting the same
  project on AWS again creates a new access point onto the same directory. Task definition
  revisions stay registered (R8).
- **Set only the ordinary tag keys.** Every request that tags something may set only
  `triple-c:owner`, `triple-c:project` and `triple-c:environment` (`aws:TagKeys`). This applies to
  `RunTask`, the ECS and EFS tag-on-create calls, `CreateSecret`, and secret `TagResource`. A
  workspace user can never set `triple-c:component`, which marks environment resources and which
  the Denies and phase-4 detection trust.
- **Inspect their own principal, as far as IAM can say so.** `iam:GetRole` and
  `iam:SimulatePrincipalPolicy`, which the app's permission check uses on the caller's own
  principal. An IAM user may inspect only itself (`user/${aws:username}`, with or without a path).
  "My own role" cannot be written: IAM refuses `${aws:PrincipalArn}` as a Resource (verified live,
  "must be in ARN format or \"*\"") and has no role-name variable. So the `InspectOwnPrincipal`
  statement names the roles people actually sign in with:
  - `role/aws-reserved/sso.amazonaws.com/*`, the IAM Identity Center permission-set roles;
  - `role/triple-c-*`.

  A caller on any other role cannot read or simulate its role, so the permission check falls back
  to probes and reports **Insufficient**, naming `iam:GetRole`. If your users sign in through other
  roles, add those roles' ARNs or path prefixes (for example `arn:aws:iam::*:role/engineers/*`)
  to `InspectOwnPrincipal` in both tier policies. Everything matched there can be read and
  simulated by every holder of the policy, so keep it to the sign-in roles.
- **Read Fargate prices** (`workspace-user-tasks.json`, `ReadFargatePrices`). `pricing:GetProducts`
  on `*`: the Price List API has no resource types or condition keys, and prices are public. The
  app asks it (in us-east-1, service `AmazonECS`) for the workspace region's on-demand Fargate
  vCPU, memory and ephemeral-storage rates, caches them for a day and shows an estimated hourly
  cost in the project's Host settings and Overview. Without this permission the estimate reads
  "Estimate unavailable"; nothing else depends on it.
- **Read environment information.** This covers ECS, the `/triple-c/*` SSM parameters (the
  environment records), the secret list, EFS, and the `/triple-c/*` log groups. Listing is not a
  secret; the app filters by owner for display. A workspace user sees only environments that have
  a record: the OpenTofu state bucket is admin-only.

## Environment admin

The admin tier applies (and destroys) the OpenTofu modules in `terraform/` with the admin's own
credentials: the provider calls AWS as the admin, so the policy must allow every call it makes,
and nothing runs with more than the admin holds. It covers:

- the per-account state bucket `triple-c-tfstate-<account>-<region>` (created by the app, tagged
  `triple-c:component=state`, then read and written by OpenTofu's S3 backend, lock files included);
- `triple-c-*` IAM roles, and reading the `triple-c-permissions-boundary` policy;
- ECS clusters (the environment's: Fargate and Fargate Spot; a user's: their own EC2 capacity
  provider) and task definitions;
- the network: VPCs, subnets, an internet gateway, route tables and routes, an Elastic IP and NAT
  gateway, security groups and their rules (`environment-admin-network.json`);
- EFS file systems, mount targets and file-system policies, and KMS keys and aliases;
- `/triple-c/*` log groups and `/triple-c/*` SSM parameters (including each user's compute record
  `/triple-c/users/<env>/<owner key>`);
- per-user EC2 capacity (`environment-admin-compute.json`, phase 7): the user's own
  `triple-c-<env>-<owner key>` cluster and capacity provider, a component-tagged launch template
  and `triple-c-*` Auto Scaling group (pinned to a template version, no load balancer, supported
  instance types only), the `triple-c-*` instance profile, passing `triple-c-*-instance` roles to
  EC2, reading the ECS-optimized AMI's public SSM parameter, and the Auto Scaling and ECS
  service-linked roles.

**Supported instance types.** Launches and groups are limited to the types the app sizes
(`ec2_instance_spec` in `models/project.rs`, pinned to the policy by a test): `m7i`, `m7g`, `c7i`,
`c7g`, `r7i`, `r7g` in `large` to `4xlarge`, and `t3`, `t4g` in `large` to `2xlarge`. No GPU,
accelerator or metal types. An onboarding with any other type is denied at apply; supporting one
means adding it to that table and to the three lists in the compute file together.

**Clearing workspace task definitions.** Removing a project leaves its revisions registered
(family `triple-c-<env>-<owner key>-<project id, 12 chars>`). The admin tier may deregister any
revision (`ecs:DeregisterTaskDefinition` on `*`), so an admin clears an environment's with:

```sh
aws ecs list-task-definitions --family-prefix triple-c-<env>- --query 'taskDefinitionArns[]' --output text \
  | xargs -r -n1 aws ecs deregister-task-definition --task-definition >/dev/null
```

Deregistered revisions become INACTIVE; ECS keeps them until they are deleted with
`ecs:DeleteTaskDefinitions`, which the admin tier does not grant (an account administrator can).
A task still running from a revision keeps running.

Not in the policy: ECR (tasks pull from GHCR). Phase 7's EC2 capacity added custom ECS capacity
providers and passing roles to EC2. The `triple-c-*-instance` name on that PassRole keeps a
user's task and execution roles from being handed to an instance by mistake; it is **not** a
fence against the admin, who can create a bounded role under any `triple-c-*` name. What fences
an instance role is the permissions boundary, as for every Triple-C role.

### What each module resource needs

`app/src-tauri/src/aws/tofu_modules/tests.rs` (`MODULE_CALLS`) maps every resource type, data
source and the S3 backend the modules use to the IAM actions the AWS provider calls to create,
read, tag, update and delete it, and a test evaluates each against the admin files. A live apply
and destroy of every module as a role holding only the admin files (see "Live verification") is
the proof; an action it shows missing is added to the table, the policy, the check table in
`policies.rs` and this list together.

| Resource | Actions (authorized against) |
|---|---|
| `aws_ecs_cluster` | `ecs:CreateCluster`, `TagResource` (at create), `DescribeClusters`, `ListTagsForResource`, `UpdateCluster`, `UntagResource`, `DeleteCluster` (cluster `triple-c-*`) |
| `aws_ecs_cluster_capacity_providers` | `ecs:PutClusterCapacityProviders`, `DescribeClusters` (the cluster; untaggable) |
| `aws_security_group` | `ec2:CreateSecurityGroup` (group: request tag; VPC: any, since EC2 shows the VPC no request tag), `CreateTags` (at create), `RevokeSecurityGroupEgress` (AWS's default rule), `Authorize`/`RevokeSecurityGroupIngress`/`Egress` and `UpdateSecurityGroupRuleDescriptions*` (tagged group; new rules: `security-group-rule/*`), `CreateTags`/`DeleteTags`, `DeleteSecurityGroup` (tagged), `Describe*` |
| `aws_kms_key` | `kms:CreateKey` (request tag), `TagResource`, `DescribeKey`, `GetKeyPolicy`, `GetKeyRotationStatus`, `ListResourceTags`, `EnableKeyRotation`, `PutKeyPolicy`, `UpdateKeyDescription`, `UntagResource`, `ScheduleKeyDeletion` (tagged key) |
| `aws_kms_alias` | `kms:CreateAlias`/`UpdateAlias`/`DeleteAlias` (alias `triple-c-*` and the tagged key; untaggable), `ListAliases` |
| `aws_cloudwatch_log_group` | `logs:CreateLogGroup`, `TagResource`, `PutRetentionPolicy`, `ListTagsForResource`, `UntagResource`, `DeleteRetentionPolicy`, `DeleteLogGroup` (`/triple-c/*`), `DescribeLogGroups` (`*`) |
| `aws_ssm_parameter` | `ssm:PutParameter`, `AddTagsToResource`, `GetParameter`, `ListTagsForResource`, `RemoveTagsFromResource`, `DeleteParameter` (`/triple-c/*`), `DescribeParameters` (`*`) |
| `aws_vpc` | `ec2:CreateVpc` (request tag), `CreateTags` (at create), `ModifyVpcAttribute` (DNS hostnames), `CreateTags`/`DeleteTags`, `DeleteVpc` (tagged), `Describe*` |
| `aws_internet_gateway` | `ec2:CreateInternetGateway`, `CreateTags` (at create), `Attach`/`DetachInternetGateway` (tagged gateway and VPC), `DeleteInternetGateway`, `Describe*` |
| `aws_subnet` | `ec2:CreateSubnet` (subnet: request tag; VPC: tagged), `CreateTags` (at create), `ModifySubnetAttribute`, `DeleteSubnet` (tagged), `Describe*` |
| `aws_route_table` | `ec2:CreateRouteTable` (table: request tag; VPC: tagged), `CreateTags` (at create), `DeleteRouteTable` (tagged), `Describe*` |
| `aws_route` | `ec2:CreateRoute`, `ReplaceRoute`, `DeleteRoute` (the tagged route table; untaggable) |
| `aws_route_table_association` | `ec2:AssociateRouteTable`, `DisassociateRouteTable` (the tagged route table and subnet; untaggable). Never `ReplaceRouteTableAssociation`: it names no subnet when it swaps a VPC's main route table, so it could re-route a VPC that isn't Triple-C's; the modules never change an association's table |
| `aws_eip` | `ec2:AllocateAddress` (request tag), `CreateTags` (at create), `ReleaseAddress` (tagged), `DisassociateAddress` (only the literal `*/*` EC2 names once the NAT gateway holding the association is gone), `Describe*` |
| `aws_nat_gateway` | `ec2:CreateNatGateway` (gateway: request tag; its subnet, address and VPC: tagged), `CreateTags` (at create), `DeleteNatGateway` (tagged), `Describe*` |
| `aws_efs_file_system` | `elasticfilesystem:CreateFileSystem` (`*`, request tag), `TagResource` (at create), `UpdateFileSystem`, `PutLifecycleConfiguration`, `TagResource`/`UntagResource`, `DeleteFileSystem` (tagged), `Describe*`, `ListTagsForResource` |
| `aws_efs_mount_target` | `elasticfilesystem:CreateMountTarget`, `ModifyMountTargetSecurityGroups`, `DeleteMountTarget` (the tagged file system; untaggable), and the interface EFS makes with the caller's permissions: `ec2:CreateNetworkInterface`, `ec2:DeleteNetworkInterface` (only with `aws:CalledViaLast = elasticfilesystem.amazonaws.com`, verified live; in the environment's subnets, which in `existing` mode are the user's own) |
| `aws_efs_file_system_policy` | `elasticfilesystem:PutFileSystemPolicy`, `DeleteFileSystemPolicy` (the tagged file system; untaggable), `DescribeFileSystemPolicy` |
| `aws_iam_role` | `iam:CreateRole` (with the boundary), `TagRole`, `UntagRole`, `GetRole`, `ListRolePolicies`, `ListAttachedRolePolicies`, `ListInstanceProfilesForRole`, `UpdateRole`, `UpdateRoleDescription`, `DeleteRole` (`triple-c-*`) |
| `aws_iam_role_policy` | `iam:PutRolePolicy`, `DeleteRolePolicy` (with the boundary), `GetRolePolicy` (the role; untaggable) |
| data sources | `sts:GetCallerIdentity`, `ec2:DescribeAvailabilityZones`, `ec2:DescribeSubnets`, `ssm:GetParameter` |
| *Phase 7, per-user EC2 (onboarding module, expected; `EC2_COMPUTE_CALLS` until the resources exist)* | |
| `aws_ecs_cluster` (the user's) | as the environment's cluster, on `cluster/triple-c-<env>-<owner key>` |
| `aws_ecs_capacity_provider` | `ecs:CreateCapacityProvider`, `TagResource` (at create), `ListTagsForResource`, `UpdateCapacityProvider`, `UntagResource`, `DeleteCapacityProvider` (`capacity-provider/triple-c-*`), `DescribeCapacityProviders` (`*`); `iam:CreateServiceLinkedRole` for `ecs.amazonaws.com` (managed scaling runs as ECS's service-linked role) |
| `aws_ecs_cluster_capacity_providers` (the user's) | `ecs:PutClusterCapacityProviders` (the cluster and, expected, the capacity provider) |
| `aws_launch_template` | `ec2:CreateLaunchTemplate` (request tag), `CreateTags` (at create), `CreateLaunchTemplateVersion`, `CreateTags`/`DeleteTags`, `DeleteLaunchTemplate` (tagged), `Describe*`. Never `ModifyLaunchTemplate`: the default version never moves (the module must not set `update_default_version` or `default_version`) |
| `aws_autoscaling_group` | `autoscaling:CreateAutoScalingGroup` (`triple-c-*`, request tag, `autoscaling:LaunchTemplateVersionSpecified = true`, no `TargetGroupARNs`/`LoadBalancerNames`, `autoscaling:InstanceTypes` supported), `UpdateAutoScalingGroup` (tagged; the same, except that an update which leaves the template alone passes the version check), `SetInstanceProtection`, `CreateOrUpdateTags`/`DeleteTags`, `DeleteAutoScalingGroup` (tagged), `Describe*` (`*`); `iam:CreateServiceLinkedRole` for `autoscaling.amazonaws.com`. Creating a group checks that the caller could launch its template: `ec2:RunInstances` on the tagged launch template; on the instance and its volume only with the component request tag, `ec2:LaunchTemplate` set and (instance) a supported `ec2:InstanceType`; on the network interface and subnet with `ec2:LaunchTemplate`; on the security group only the template's own (`ec2:IsLaunchTemplateResource`) component-tagged group; on images only Amazon's, named by the template; `ec2:CreateTags` at `RunInstances`; `iam:PassRole` of the instance role to `ec2.amazonaws.com`. ASG tags are **not** the provider's `default_tags`: the module must set the component tag on the group itself |
| `aws_iam_role` (instance), `aws_iam_role_policy` | as the other per-user roles (bounded), on `triple-c-<env>-<owner key>-instance`, tagged `triple-c:cluster = triple-c-<env>-<owner key>` (`iam:TagRole`) |
| `aws_iam_instance_profile` | `iam:CreateInstanceProfile`, `TagInstanceProfile`, `AddRoleToInstanceProfile` (expected to need `iam:PassRole` of the role too), `GetInstanceProfile`, `ListInstanceProfileTags`, `UntagInstanceProfile`, `RemoveRoleFromInstanceProfile`, `DeleteInstanceProfile` (`instance-profile/triple-c-*`) |
| `data.aws_ssm_parameter` (AMI) | `ssm:GetParameter`/`GetParameters` on the public `parameter/aws/service/ecs/optimized-ami/*` |
| `aws_ssm_parameter` (user record) | as the environment record, on `/triple-c/users/<env>/<owner key>` |
| S3 backend | `s3:ListBucket` (the state bucket), `GetObject`/`PutObject`/`DeleteObject` (`triple-c-tfstate-*/*`, the `.tflock` objects included), in the caller's own account |

The app also creates the state bucket: `s3:CreateBucket` with a `triple-c:component` request tag
(and `s3:TagResource`, which a create with tags also needs, under the same conditions), then `PutBucketVersioning`, `PutEncryptionConfiguration`, `PutBucketPublicAccessBlock`,
`PutBucketTagging`, `PutBucketPolicy`, `GetBucket*` and `ListBucket`, all on
`triple-c-tfstate-*` only. No bucket can be deleted.

Every state-bucket statement also requires `s3:ResourceAccount = ${aws:PrincipalAccount}`: the
bucket name is predictable, so another account could create `triple-c-tfstate-<your account>-…`
first and collect (or feed) your state. With the condition, the admin's calls on a bucket owned by
any other account are denied, so the app fails rather than writes state there. (The Service
Authorization Reference lists `s3:ResourceAccount` for `CreateBucket` as well as the bucket and
object actions; Task 8's live run confirms it.)

It is fenced in the following ways.

**Roles are bounded.** The admin can create a `triple-c-*` role, or change its policies, only when
the role carries the `triple-c-permissions-boundary` managed policy as its permissions boundary.

- The boundary is the most a Triple-C role can ever hold:
  - ECR image pulls;
  - writing `/triple-c/*` logs;
  - ECS Exec channels;
  - reading `triple-c/*` secrets;
  - decrypting only through Secrets Manager, and only with environment (component-tagged) keys;
  - mounting and writing only Triple-C (component-tagged) file systems, which are each user's own
    (tagged `user-onboarding` by their onboarding module); the file system's own policy admits
    only its owner's task role, through an access point;
  - `sts:GetCallerIdentity`;
  - invoking Bedrock models;
  - for a user's EC2 instance role (phase 7, E4): the ECS agent's calls
    (`RegisterContainerInstance`, `DeregisterContainerInstance`, `Poll`, `StartTelemetrySession`,
    `Submit*`, `UpdateContainerInstancesState`) only on **the role's own cluster**, the one its
    `triple-c:cluster` tag names (`cluster/${aws:PrincipalTag/triple-c:cluster}` and that
    cluster's container instances; the tag must match `triple-c-*`), `ecs:TagResource` only on
    such a container instance as it registers (`ecs:CreateAction = RegisterContainerInstance`),
    and `ecs:DiscoverPollEndpoint`. A user with Docker access is root on their own instance and
    so holds its role's credentials; the tag keeps them from registering that instance into
    another user's cluster (and so having that user's tasks placed on it). An untagged instance
    role can do nothing with ECS. The admin sets the tag when it creates the role (the admin
    tier already tags `triple-c-*` roles freely, and a workspace user cannot tag roles at all).
    **No Session Manager on the instance** (plan E4 amended in fix round 1): nobody is granted
    host sessions, and ECS Exec into an EC2 task uses the task role, as on Fargate.
- The boundary grants nothing in IAM.
- Explicit Deny statements stop the admin from:
  - removing the boundary from a `triple-c-*` role;
  - setting any other boundary on one;
  - editing or deleting the boundary policy itself.

Without these fences an admin could create a role with any permissions and become an account
administrator.

**Destruction is tag-scoped.** VPCs, subnets, internet gateways, route tables, Elastic IPs, NAT
gateways, security groups, EFS file systems and KMS keys can be created only with a
`triple-c:component` tag (the modules' `default_tags` put it on every create request). They can be
changed or deleted only if they carry that tag. Subnets, route tables and NAT gateways can be
created only *inside* a Triple-C (tagged) VPC, subnet and address, and an internet gateway
attached, a route added or a route table associated only on tagged ones, so the admin cannot
re-route a VPC that isn't Triple-C's. Tagging is allowed only as part of the creating
call (`ec2:CreateAction`, `elasticfilesystem:CreateAction`), or on a resource that is already
Triple-C's. Launch templates (phase 7) are created only with the component tag and versioned,
launched from or deleted only when they carry it; their default version never changes. Auto
Scaling groups must be named `triple-c-*` and also carry it (request tag to create; resource tag
to change, protect, tag or delete, so an existing untagged `triple-c-*` group can't be claimed by
tagging it). A group must name its template's version explicitly: a group on `$Latest` or
`$Default` would launch any later version, whose instance profile nobody checked, as Auto
Scaling's service-linked role. Resources
that cannot carry a tag are authorized through one that does: routes and
route-table associations through the route table (and subnet), mount targets and the file-system
policy through the file system, KMS aliases through the key, role policies through the
(boundary-carrying) role. Clusters, capacity providers, log groups, parameters, roles, instance
profiles and the state bucket are scoped by name (`triple-c-*`, `/triple-c/*`,
`triple-c-tfstate-*`).

**What is not scoped.** The scoping above does not cover every action. These actions apply
account-wide:

- `ecs:RegisterTaskDefinition` and `ecs:DeregisterTaskDefinition` on `*`. The admin can deregister
  *any* task definition revision in the account. `DeregisterTaskDefinition` has no resource type
  (IAM authorizes it against `*`), so it cannot be narrowed.
- `elasticfilesystem:CreateFileSystem` on `*` (no resource type), still only with a
  `triple-c:component` request tag.
- `ec2:CreateSecurityGroup` in **any** VPC, as long as the new group is tagged: an
  `existing`-network environment creates its tasks group and each user's `-efs` group in the
  user's own, untagged VPC. It is the only create that may name an untagged parent. The VPC side
  of the grant has no condition at all: EC2 does not show the VPC the request's tags (a request-tag
  condition there denied every create, verified live), so the group's own side carries the tag
  rule.
- `ec2:DisassociateAddress` on `arn:aws:ec2:*:*:${*}/${*}`, i.e. the literal resource `*/*` and
  nothing else (`${*}` is IAM's escaped `*`). Destroying the NAT gateway's address disassociates
  it first; by then the NAT gateway and its association are gone and EC2 authorizes the call
  against `*/*` (verified live: denied without this, then `InvalidAssociationID.NotFound`, which
  the provider ignores). **Expected, not live-verified:** a live association should be
  authorized against its address and interface ARNs, which `${*}/${*}` does not match, so no real
  address should be disassociable through this statement. Only the gone-association case was
  observed; the probe that would settle it is listed under "Still open" below.
- `ec2:AuthorizeSecurityGroupIngress`/`Egress` on `security-group-rule/*`: authorizing names the
  new, untagged rule as well as the group, and the group must still be tagged.
- `ec2:CreateNetworkInterface` and `ec2:DeleteNetworkInterface` on `*`, but only when EFS makes
  the call for the admin (`aws:CalledViaLast = elasticfilesystem.amazonaws.com`): EFS creates and
  deletes a mount target's interface with the caller's EC2 permissions, in any subnet (an
  `existing` environment's are untagged). Verified live: EFS sets `aws:CalledViaLast` for both,
  and mount targets are created and deleted under this statement alone.
- Reads on `*`: `ec2:Describe*`, `elasticfilesystem:Describe*`, the KMS key reads and
  `ListAliases`, `logs:DescribeLogGroups` and `ssm:DescribeParameters`.
- `ec2:RunInstances`, directly as well as through a group: only from a component-tagged launch
  template, the instance and its volume carrying the component tag, a supported instance type,
  Amazon's image and the environment's (tagged) security group, both as the template names them
  (`ec2:IsLaunchTemplateResource`), and only a `triple-c-*-instance` role passed. The network
  interface and **subnet are not tag-checked**: the group supplies the subnets (not the template),
  and an `existing`-network environment's subnets are the user's own, untagged. The modules never
  call `RunInstances` themselves: creating a group checks that the caller could, and the group's
  launches run as Auto Scaling's service-linked role.
- `iam:CreateServiceLinkedRole`, only for `autoscaling.amazonaws.com` and `ecs.amazonaws.com`
  (AWS fixes those roles' policies).
- Reads on `*`: `autoscaling:Describe*`, `ecs:DescribeCapacityProviders`; and the public
  ECS-optimized AMI parameters.
- `s3:PutBucketPolicy` on the state bucket (only a bucket in the admin's own account, enforced by
  `s3:ResourceAccount`): an admin could open that bucket to other accounts. The state holds no
  secrets (the modules create none), but it maps the account's Triple-C resources.

## Environments (OpenTofu)

An environment is deployed with [OpenTofu](https://opentofu.org) from the modules in `terraform/`.
The app runs them itself (it downloads OpenTofu 1.13.9, verified against a pinned SHA-256 before
it is unpacked), and an admin can run the very same files by hand from a bundle.

| Module | Creates | State key |
|---|---|---|
| `terraform/permissions-boundary` | the account's `triple-c-permissions-boundary` policy (an account administrator applies it; the admin tier cannot) | `boundary/terraform.tfstate` |
| `terraform/environment` | what every user shares: ECS cluster (Fargate, Fargate Spot), the tasks security group (no inbound rules), KMS key and alias, log group `/triple-c/<env>`, the network (`network_mode`: `public`, `private-nat`, `existing`) and, last, the record `/triple-c/environments/<env>` (which carries the advisory `max_workspace_hours` lifetime cap, 0 = none, at most 168) | `environments/<env>/terraform.tfstate` |
| `terraform/user-onboard` | one user's task and execution roles (with the boundary) and their own EFS file system, its `-efs` security group, two mount targets and a file-system policy that admits only their task role | `users/<env>/<owner key>/terraform.tfstate` |

**Workspace lifetime cap (advisory).** `max_workspace_hours` (module variable, 0 to 168, default 0 =
no cap) is stored in the environment record and passed to every workspace as
`TRIPLEC_MAX_LIFETIME_MINUTES`; the workspace's idle watcher then pushes work in progress (best
effort, 90 s) and stops the task, which ECS reports as exit code 76. It is **not a security
control**: a workspace user has sudo inside their own task and can switch the watcher off. To
enforce a limit, stop old tasks from outside (for example a scheduled Lambda; not built). Changing
the cap is a module update (template version 2), so environments deployed before it show "Update
available" in the app.

The **owner key** is the first 12 hex characters of `sha256("<region>:<aws:userid>")`; it names
the user's roles, `triple-c-<env>-<owner key>-task` and `-exec` (a `UserId` holds a `:`, which
role names reject). The region is in it because IAM role names are account-wide while
environments are regional: the same user in an environment of the same name in two regions gets
two sets of roles. The module checks the key against `region` and `owner_id`.

Every module pins the AWS provider (`hashicorp/aws` 6.73.0, hashes in `.terraform.lock.hcl`),
sets `region` explicitly, never overrides an endpoint, and tags everything through `default_tags`
(`triple-c:component`, plus the environment, owner and template version). The onboarding module
reads the environment only from its record, never from its state.

**The state bucket** is `triple-c-tfstate-<account>-<region>`, one per account and region. The app
creates it on the first deploy (versioning, SSE-S3, all four public-access blocks, a TLS-only
bucket policy, tagged `triple-c:component=state`; the tag goes on the create itself, which the
admin policy requires). An existing bucket policy is kept: the TLS-only statement is added to it
when missing, never put over an organisation's own statements. Every call carries `expected_bucket_owner`, so a
bucket of that name in another account is refused, never used. Locking is S3-native
(`use_lockfile = true`): a run holds `<state key>.tflock`, and a held lock is what "deploying"
means to every other copy of the app. The app removes a stale lock (older than two hours: an
environment's, or the caller's own onboarding's) with a conditional delete on the lock's ETag, so a lock taken since is never
removed. `tofu destroy` leaves an *empty* state object behind; detection reads an empty state as
no deployment at all.

**How the app runs a deployment.** It writes the module into a work folder
(`<app data>/triple-c/aws/work/<bucket>/<state key>/`) with `backend.hcl` and
`terraform.tfvars.json`, then starts a detached runner (the app binary re-executed with
`--tofu-runner`) that runs `tofu init` and `tofu apply` (or `destroy`) in its own session,
logging to `apply.log`. Closing the app does not stop it; the next start reattaches by following the
log. Cancel interrupts OpenTofu, which stops gracefully and saves its state. Cancel is not
available on Windows yet (a run there goes on to the end). If OpenTofu cannot save the state after
a run it keeps it in `errored.tfstate` in the work folder; the run ends with an error saying so,
nothing else is started for that deployment until it is pushed back, and **Push saved state** on
the row runs `tofu state push errored.tfstate` through the same runner (by hand: `tofu init
-backend-config=backend.hcl`, then `tofu state push errored.tfstate`, from the work folder). OpenTofu's environment
is built from scratch: no inherited `AWS_*`, `TF_*` or `TOFU_*` variables, the connection's profile
name or its keys (in the environment only, never in arguments or files), and the app's own CLI
config, so a `~/.tofurc` cannot redirect provider downloads. The app destroys only an incomplete
deployment (state without a record, or a failed apply), and never while any user's onboarding
state for it remains; a working environment is removed by an admin with `tofu destroy` from a
bundle.

**Onboarding detection.** A user is *Joined* when their task role exists and, where the bucket is
readable, their state holds the module's `onboarding_complete` output, which depends on every
resource the module creates, so a run that failed part-way never writes it. A partial state, a
failed (or errored) run on this machine, or a stale lock reads as *Incomplete*, and Join (an
idempotent re-apply) stays offered to finish it; Follow, Unlock and Push saved state work on the
user's own onboarding as on an environment.

**Admin bundles.** When the signed-in identity cannot deploy (or the admin prefers to), the app
saves a folder with the module, `backend.hcl`, `terraform.tfvars.json` and a `README.txt` holding
the exact commands. By hand, with credentials carrying the admin files (OpenTofu or Terraform 1.10
or later); the permissions-boundary bundle instead needs an account administrator **without** the
admin files attached, since their Deny on the boundary policy applies to anyone holding them:

```sh
cd <bundle folder>
tofu init -backend-config=backend.hcl
tofu apply      # or: tofu destroy
```

Remove every user's onboarding before destroying their environment. With Terraform, first run
`terraform providers lock` for your platforms (the lock file carries OpenTofu's registry hashes).
The bundle's `README.txt` also shows how to create the state bucket by hand if the app never has,
exactly as the app configures it: `create-bucket` with the `triple-c:component=state` tag in its
`--create-bucket-configuration` (the admin policy allows the create only with that tag in the
request, so a later `put-bucket-tagging` would not do; this needs an AWS CLI v2 whose
`create-bucket` accepts `Tags`), then versioning, SSE-S3, the public-access block and the TLS-only
bucket policy.

## Invariants the modules must keep (phase 4)

The policies rely on the following. The modules in `terraform/` establish them (their tests pin
it), and detection should flag any environment that breaks them.

1. **Every `triple-c-*` role carries `triple-c-permissions-boundary`.** The admin tier cannot
   create one without it. A role created some other way, such as by an account administrator or by
   hand, must still have the boundary. Detection should flag a `triple-c-*` role that has no
   boundary.
2. **Every environment-level `triple-c/*` secret carries `triple-c:component`.** A secret with
   neither an owner tag nor a component tag could be claimed by any workspace user.
3. **Every environment KMS key and EFS file system carries `triple-c:component`,** and every
   user's file system also carries `triple-c:owner`. The boundary, the admin tier and the
   workspace user's access-point rule rely on these tags.
4. **User secrets use the environment key.** The app must pass the environment KMS key's ARN as
   `KmsKeyId` when it creates a secret. The workspace policy explicitly denies Secrets Manager
   encryption with any key that lacks `triple-c:component`, including `aws/secretsmanager`.

## One-time setup: the boundary policy

An **account administrator** creates the boundary policy once, before anyone deploys an
environment. The environment-admin tier deliberately cannot do this.

```sh
aws iam create-policy --policy-name triple-c-permissions-boundary \
  --policy-document file://policies/permissions-boundary.json
```

**Phase 7 changed the boundary** (version 2: the instance role's ECS agent, on the cluster its
`triple-c:cluster` tag names). An account administrator re-applies it once (the boundary module from the app, or
`aws iam create-policy-version --policy-arn arn:aws:iam::<account>:policy/triple-c-permissions-boundary
--policy-document file://policies/permissions-boundary.json --set-as-default`) before anyone
onboards with EC2 capacity; Fargate users are unaffected. The module tags the policy
`triple-c:template-version = 2`.

Create the policy **without a path**. The admin policy expects exactly
`arn:aws:iam::<account>:policy/triple-c-permissions-boundary`. The `terraform/permissions-boundary`
module creates the same policy, applied by an account administrator; the admin tier can read it
(`iam:GetPolicy`, which detection uses) but never create or change it.

## Attaching the tier policies

Create each file as a **customer managed policy** and attach it. In the console, go to
**IAM → Policies → Create policy → JSON**, paste the file, and attach the policy to a user, group
or role. Inline policies do not fit: a role's inline policies share 10,240 characters and an
environment admin needs every tier file (about 20,200), and a user's share 2,048. A managed policy
may hold 6,144 characters, not counting whitespace. Measured 2026-11-16 without whitespace, `workspace-user.json` is
6,036 (108 characters of headroom), `workspace-user-tasks.json` 1,293 (including `ReadFargatePrices`),
`environment-admin.json` 5,805 (339), `environment-admin-network.json` 3,340,
`environment-admin-compute.json` 6,020 (124) and `permissions-boundary.json` 2,651; a unit test (`every_policy_fits_a_managed_policy`)
fails before any file outgrows the limit. A tier that outgrows its files gains another file, which
the app then lists with the others. For IAM Identity Center, add the files as inline policies on the permission set
(the "up to 32,768 characters" limit is unverified) or reference the managed policies.

With the CLI:

```sh
aws iam create-policy --policy-name triple-c-workspace-user \
  --policy-document file://policies/workspace-user.json
aws iam create-policy --policy-name triple-c-workspace-user-tasks \
  --policy-document file://policies/workspace-user-tasks.json
aws iam attach-user-policy --user-name <user> \
  --policy-arn arn:aws:iam::<account>:policy/triple-c-workspace-user
aws iam attach-user-policy --user-name <user> \
  --policy-arn arn:aws:iam::<account>:policy/triple-c-workspace-user-tasks
```

## Known gaps

**Phase 4**

- **Per-user execution roles.** A workspace user can pass only roles tagged with their own owner
  ID, so each user's onboarding module creates their execution role as well as their task role
  (done: `terraform/user-onboard/iam.tf`).
- **The admin's deploy permissions are verified for `public` and `private-nat`, not `existing`.**
  A live apply, update and destroy of every module as a role holding only the admin files passed
  (see "Live verification"). The `existing` network mode (a user's own, untagged VPC and subnets)
  was not deployed live; its extra grants (`CreateSecurityGroup` in any VPC, mount-target
  interfaces in untagged subnets) are the same statements the other modes exercised. Which
  tag-on-create actions are strictly *needed* was not tested one by one: the listed ones are
  sufficient, and only `s3:TagResource` turned out missing. A trust-policy change on a role
  (`iam:UpdateAssumeRolePolicy`) is never allowed, so a module version that changes a role's trust
  must replace the role.
- **Admin access to user data.** The admin can retag `triple-c-*` roles (`iam:TagRole`). The
  boundary also allows every `triple-c/*` secret and every Triple-C file system. As a result,
  isolating users *from the admin* depends on the onboarding module's per-user policies, not on
  IAM fences.
- **The workspace tier's secret Deny also applies to admins.** If the environment module ever
  manages component-tagged secrets through the admin's credentials, that Deny will block
  `PutSecretValue`/`DeleteSecret` for them.

**Phase 5**

- **No secrets in task definitions or `RunTask` overrides.** Nothing sensitive (the agent token,
  git credentials) may travel in task-definition environment variables or in `RunTask`
  `containerOverrides`: every workspace user can read them through `ecs:Describe*` on `*`. Use
  Secrets Manager (`triple-c/*`) references instead.

**Phase 7 (per-user EC2 capacity): still open until the live run (Task 7)**

The unit tests' evaluator is not IAM; these points of the compute file and the boundary are
expected, not verified:

- **Creating an Auto Scaling group checks the caller's launch permissions.** Expected: a dry run
  of `ec2:RunInstances` from the launch template as the admin, with `ec2:LaunchTemplate` set on
  every resource, the template's own tags visible, `ec2:Owner = amazon` on the image, and
  `iam:PassRole` of the instance role to `ec2.amazonaws.com`. If the dry run omits
  `ec2:LaunchTemplate` on some resource, the group's create is denied and that statement needs
  another condition.
- **`iam:AddRoleToInstanceProfile`** is expected to need `iam:PassRole` of the role; which
  `iam:PassedToService` IAM sends then (if any) is unknown, and only `ec2.amazonaws.com` is allowed.
- **`ecs:PutClusterCapacityProviders`** may be authorized against the capacity provider as well as
  the cluster (both are allowed); **`ecs:CreateCapacityProvider`** may check the caller's
  `autoscaling:Describe*` on the group (allowed) or create ECS's service-linked role (allowed).
- **Service-linked roles.** The Auto Scaling and ECS roles exist in most accounts already;
  creating them is allowed only for those two services. Whether creating a group in an account
  without one creates it as the caller (needing this grant) is not verified.
- **The instance role's agent calls** (`ecs:Poll`, `StartTelemetrySession`,
  `UpdateContainerInstancesState`) are expected to be authorized against
  `container-instance/<cluster>/<id>` and the rest against the cluster; the boundary allows both.
- **The public AMI parameter's ARN** has no account (`arn:aws:ssm:<region>::parameter/aws/…`);
  `arn:aws:ssm:*::parameter/aws/service/ecs/optimized-ami/*` is expected to match it.
- **Version pinning.** `autoscaling:LaunchTemplateVersionSpecified` is expected to be `true` for a
  numeric version and `false` for `$Latest`/`$Default`, and absent on an update that names no
  template (allowed by `BoolIfExists`, so the drain before destroy works). Probe: create a group
  with `$Latest` as the admin; it must be denied.
- **`ec2:IsLaunchTemplateResource`** is expected to be `true` on the image and security group the
  template names, and `aws:RequestTag` to carry the template's tag specifications for the instance
  and volume. If the group's launch check doesn't send either, that statement denies the create.
- **`autoscaling:InstanceTypes`** is expected to be absent (vacuously allowed) unless a request
  names instance types (a mixed-instances override); the instance type itself is enforced at
  `RunInstances`.
- **Tags at group create.** If `CreateAutoScalingGroup` with tags also needs
  `autoscaling:CreateOrUpdateTags`, it is denied (that is allowed only on an already-tagged
  group); then add it to the create statement with the same conditions.
- **A `triple-c-*` capacity provider pointing at someone else's group.** Whether
  `CreateCapacityProvider` checks the caller's rights on the named Auto Scaling group is unknown;
  if it does not, an admin could put a non-Triple-C group under a Triple-C capacity provider (and
  ECS's managed scaling would then scale it). Probe: create one naming an untagged group.
- **`aws:PrincipalTag` for the instance role**: the role's session is expected to carry its role
  tags, so `${aws:PrincipalTag/triple-c:cluster}` resolves for the agent's calls.
- **IMDS and Docker access.** Tasks use `awsvpc`, with `ECS_AWSVPC_BLOCK_IMDS=true` and a hop
  limit of 1 so a task can't read the instance role. A project with Docker access mounts the
  host's socket, so its user can start a host-network container and read IMDS (and so the
  instance role): accepted, since Docker access is root on the user's own instance; the boundary
  keeps that role on the user's own cluster.

**Phase 6**

- **Access-point roots.** A workspace user creates their own access points, only on their own file
  system (strict tenancy: one file system per user, whose policy admits only their task role). An
  access point rooted at `/` therefore reaches only that user's own data. Pinning `RootDirectory`
  and `PosixUser` is still worth doing if file systems are ever shared again.

## Live verification (2026-11-14)

Every point the unit tests' small evaluator could not settle was checked against real IAM in a
disposable account (us-west-2): with the policy simulator, with calls made as throwaway roles that
hold the policies, and with deny probes (a call made as a role with no permissions names the
resource it was authorized against, and creates nothing). The repeatable part is
`app/src-tauri/src/aws/live_tests.rs`:

```sh
cd app/src-tauri
TRIPLEC_AWS_E2E_PROFILE=<admin profile of a disposable account> TRIPLEC_AWS_E2E_REGION=us-west-2 \
  cargo test aws::live_tests -- --ignored --test-threads=1
```

It creates `triple-c-e2e-<id>-*` roles and policies, `triple-c/triple-c-e2e-<id>/*` secrets and
`triple-c-e2e-<id>` KMS keys, all tagged `triple-c-test=phase3`, and deletes them when each test
ends (keys are scheduled for deletion, 7 days). One test creates the real
`triple-c-permissions-boundary` policy if the account has none, and deletes it again.

✓ verified as written · ✗→fixed: the policy changed · — could not verify, and why.

| Point | Result |
|---|---|
| `${aws:PrincipalArn}` as a Resource | ✗→fixed. IAM refuses it ("must be in ARN format or \"*\""), so neither tier policy could be attached at all. Now SSO permission-set roles, `triple-c-*` roles and `user/${aws:username}`, `user/*/${aws:username}` (see the workspace user's own principal) |
| SSO role path | ✓ `iam:GetRole` returns `role/aws-reserved/sso.amazonaws.com/<region>/AWSReservedSSO_…`, and the check simulates that ARN. The SSO admin session checks as Environment admin, Simulated |
| `${aws:PrincipalAccount}` in `iam:PermissionsBoundary` | ✓ `CreateRole` with `triple-c-permissions-boundary` succeeds as the admin tier; without a boundary, or with another one, it is denied |
| Boundary fences | ✓ removing, replacing or editing the boundary is an explicit deny |
| Simulator and context | ✓ the simulator substitutes `${aws:userid}` and `${aws:username}` from context entries. It **does** apply the simulated principal's own permissions boundary (verified); it does not apply session policies. SCPs: not verifiable here |
| Simulator rate limit | ✗→fixed (app). `SimulatePrincipalPolicy` throttles ("Rate exceeded") within a full check; the check now uses adaptive retries and two requests at a time |
| KMS `CreateKey` with tags | ✓ allowed: tag-on-create passes the tag rule on `kms:TagResource`. Untagged `CreateKey`, and `CreateAlias`/`TagResource`/`ScheduleKeyDeletion` on an untagged key, are denied |
| ECS Exec on the cluster | ✗→fixed. `ecs:ExecuteCommand` is authorized against the cluster, then the task. Added `ExecOnTripleCClusters`; with only the cluster allowed, the call is denied on the task, so the owner rule still decides |
| ECS tag keys | ✓ (service reference) `aws:ResourceTag` is supported on tasks, clusters and task definitions. Live: — needs phase 4/5 ECS resources |
| SSM `StartSession` to a task | ✓ authorized against `arn:aws:ecs:<region>:<account>:task/<cluster>/<task-id>`, which `task/triple-c-*/*` matches. The tag condition on a real task: — needs phase 5 |
| SSO session ownership | ✓ (AWS docs) the `aws:ssmmessages:session-id` tag is the starter's `aws:userid` for users and role sessions alike. Live: — needs a session. Added `ssmmessages:OpenDataChannel` on own sessions, which AWS's current examples require |
| EC2 tag-on-create | ✓ `CreateSecurityGroup` (DryRun) is allowed with a `triple-c:component` tag and denied without one or with only other tags. `security-group-rule` resources on `AuthorizeSecurityGroup*`: — needs a security group |
| EFS | ✗→fixed: `CreateFileSystem` has no resource type, so `Resource` is now `*`. ✓ a tagged `CreateFileSystem`/`CreateAccessPoint` authorizes `elasticfilesystem:TagResource` first. `elasticfilesystem:CreateAction`, `CreateAccessPoint` against the file system's tags, `ClientMount`/`ClientWrite`: — need phase 6 file systems |
| Secrets tag-on-create | ✓ `CreateSecret` with tags authorizes `secretsmanager:TagResource` separately (refused without `TagOwnSecrets`), and that statement's `StringEqualsIfExists`/`Null` conditions pass at create |
| `secretsmanager:KmsKeyId` | ✓ carries the request's value: a key ARN passes, a bare key ID, an alias or no key is refused |
| Secrets Manager's KMS calls | ✓ made as the caller with `kms:ViaService`. ✗→fixed: `CreateSecret`/`PutSecretValue` on a customer key also need `kms:Decrypt`, which the policy lacked ("Access to KMS is not allowed"). `PutSecretValue` on an own secret now passes |
| `aws/secretsmanager` key | ✓ denied, and the Deny is what denies it: the same policy without `NeverEncryptSecretsWithOtherKeys` creates the secret |
| Secret ownership | ✓ another owner's or a component-tagged secret cannot be written, deleted or claimed; `triple-c:component` cannot be set. An untagged `triple-c/*` secret *can* be claimed (invariant 3) |
| `aws:TagKeys` | ✓ for `CreateSecret` and secret `TagResource`. `RunTask` (including `aws:ecs:*` and propagated tags), `CreateAccessPoint`, ECS/EFS tag-on-create: — need phase 4/5/6 resources |
| Boundary and key tags | ✓ a role under the boundary decrypts through Secrets Manager with a component-tagged key and is refused with an untagged customer key, so KMS supplies the key's `aws:ResourceTag` |
| `cloudformation:RoleArn` | ✓ (phase 3) present when `RoleARN` is given; the Deny fired. CloudFormation has since left the policies (phase 4, D8), and the Deny with it |
| Task definition / pull-through scoping | ✓ both authorize against `*`; they cannot be narrowed |

Still open for the phases that create the resources: `RunTask`'s tag keys and `iam:PassRole`'s
`iam:ResourceTag` on a real run, the owner tag on a real task for `StopTask`/`ExecuteCommand`/
`StartSession`, session ownership for an SSO-started session, and the EFS and security-group-rule
points above. The simulator does not apply session policies, so the first real create is the final
check. The phase-4 additions (the network, the state bucket, the module table above) are
verified below; the owner-scoped `CreateAccessPoint` waits for phase 6's access points.

### Phase 4: the modules, applied for real (2026-11-14)

```sh
cd app/src-tauri
cargo build    # the runner is the app binary, re-executed with --tofu-runner
TRIPLEC_AWS_E2E_PROFILE=<admin profile of a disposable account> TRIPLEC_AWS_E2E_REGION=us-west-2 \
  cargo test aws::live_tests::environment -- --ignored --test-threads=1
```

`an_environment_admin_can_deploy_join_update_and_remove_an_environment` creates a role holding
**only** the admin files (as `triple-c-e2e-<id>-envadmin*` managed policies) and does everything as
a session of it, passing its keys to OpenTofu as a keys connection does: `ensure_state_bucket` on a
throwaway `triple-c-tfstate-<account>-<region>-<suffix>` bucket (`TRIPLEC_E2E_BUCKET_SUFFIX`, random
by default), deploy a `public` environment through the real detached runner, `scan` (Ready, a valid
record, joinable), self-onboard (Joined, both roles carry the boundary), update (no changes),
destroy the onboarding, then the environment. If the account has no boundary, the account admin
applies the boundary module first and destroys it at the end. `the_private_nat_mode_deploys` does
the same for `private-nat` (about $0.05: one NAT gateway for a few minutes). A guard destroys
whatever is still applied, as the account admin (the boundary only through its module's state, so
a boundary the run did not create is never touched), and deletes the bucket with every version if
the run created it, on failure too; every denied action is printed by name. The test drives the same functions as the
commands (`ensure_tofu`, `materialize`, `child_env`, the runner, `follow`, `scan`, the refusal
checks, the conditional lock delete); the Tauri commands' own bookkeeping (launch slots, the
progress events) is not exercised.

| Point | Result |
|---|---|
| `s3:CreateBucket` with a request tag and `s3:ResourceAccount` | ✓ both conditions are evaluated and pass for the admin's own account. ✗→fixed: a create with tags also authorizes **`s3:TagResource`**, which the policy lacked (added to `CreateTheStateBucket`, same conditions) |
| `ec2:CreateSecurityGroup` in a VPC | ✗→fixed: denied on the VPC resource, which EC2 does not show the request's tags, so `CreateSecurityGroupsInAnyVpc` lost its request-tag condition. The group itself still needs the tag |
| Tag-on-create (ECS, KMS, Logs, SSM, EFS, EC2, IAM) | ✓ every create passed with the listed actions; none was missing. Not tested one by one whether each is strictly needed |
| EFS mount-target interfaces | ✓ EFS sets `aws:CalledViaLast = elasticfilesystem.amazonaws.com`: both mount targets were created and deleted with `MountTargetInterfacesThroughEfs` as the only grant of `ec2:CreateNetworkInterface`/`DeleteNetworkInterface`. Ruling R9's fallback was not needed |
| EFS file-system policy naming a just-created role | ✓ no invalid-principal error in any onboarding apply (two applies, each created in 4 s, right after the role; whether the provider retried internally is not visible without its debug log); no module change. If it ever fails, applying the onboarding again completes it |
| NAT address on destroy | ✗→fixed: the provider disassociates the address before releasing it, after the NAT gateway (and the association) are gone, and EC2 authorizes that call against the literal `*/*`. `DisassociateGoneAddresses` allows exactly `arn:aws:ec2:*:*:${*}/${*}`; CloudTrail shows the retried call reaching `InvalidAssociationID.NotFound` |
| Conditional unlock | ✓ on the versioned state bucket, `DeleteObject` with `If-Match` and a mismatched ETag gets 412 (the app's "lock changed" answer) and leaves the lock; with the current ETag it deletes it. A fresh foreign lock scans as Deploying with its `Who` |
| Update | ✓ re-applying with the network and lifetime cap read back from the record: "0 added, 0 changed, 0 destroyed" |
| After `tofu destroy` | ✗→fixed (app): the state object stays, empty, so a destroyed environment kept scanning as Incomplete (and a removed onboarding as Joined). Detection now reads small state objects (up to 2 KiB, at most 200 per scan) and treats one with no resources as absent; a read that fails counts as a deployment |
| Cleanup | ✓ nothing `triple-c` left in us-west-2 (roles, policies, log groups, clusters, VPCs, subnets, gateways, NAT gateways, addresses, security groups, file systems, parameters, aliases, buckets); five environment KMS keys pending deletion (7 days); the boundary, which the account did not have, deleted again |

The environment-admin role in these tests carries **no permissions boundary**, deliberately: the
boundary is the ceiling for the task and execution roles the modules create, not for admins (it
allows only what a task does, so an admin under it could administer nothing). The test instead
asserts that every role the admin creates carries the boundary.

Still open:

- the `existing` network mode against a user's own VPC, and an onboarding by a workspace user
  from an admin bundle (the same module, applied with admin credentials);
- `DisassociateGoneAddresses` cannot touch a **live** association. Probe: allocate an untagged
  Elastic IP, associate it with a test ENI, and call `ec2:DisassociateAddress` on that association
  as the environment admin; it must be denied (then release both).

### Phase 5: a Fargate workspace (2026-11-15)

```sh
TRIPLEC_AWS_E2E_PROFILE=<admin profile> TRIPLEC_AWS_E2E_REGION=us-west-2 scripts/aws-e2e-workspace.sh
```

The script first unsets every `AWS_*` variable of the caller, so only the named profile's
credentials are used; `TRIPLEC_AWS_E2E_CREDENTIALS_FILE` / `TRIPLEC_AWS_E2E_CONFIG_FILE` (default:
the caller's `AWS_SHARED_CREDENTIALS_FILE` / `AWS_CONFIG_FILE`, else `~/.aws`) say where that
profile lives. It builds this checkout's `container/` (`--platform linux/amd64`), creates a throwaway
private ECR repository `triple-c-e2e-sandbox`, pushes, runs the test
(`aws::live_tests::environment::a_fargate_workspace_round_trips`, with `TRIPLEC_E2E_IMAGE` and
`TRIPLEC_E2E_ECR_REPO`) and deletes the repository in an exit trap — always attempted, a
`RepositoryNotFoundException` is fine; Ctrl-C exits 130 and a TERM 143 after the same cleanup. The
test's guard deletes it as well. The test body runs under a 45-minute timeout, every exec under its own.
**Test only:** the onboarding grants the execution role no ECR permissions (the boundary allows
pulls), so the test puts an inline `triple-c-e2e-ecr-pull` policy on it and removes it before the
onboarding is destroyed. The test deploys a `public` environment as the environment admin, onboards
a role that holds **only** the workspace files, and drives `ProjectHost::Aws` as that role through
a profile connection (`role_arn` + `source_profile`): Hybrid, 1 vCPU / 2 GB, one public repo, idle
stop 5 minutes. Its guard stops every task, retires the test's revisions as the account admin, deletes the access points
and the secret and removes the ECR policy; then the fixture destroys the modules.

| Point | Result |
|---|---|
| `RegisterTaskDefinition` on `task-definition/triple-c-*` | ✓ allowed for the workspace user with the owner request tag and the three keys; the revision carries exactly those tags (`TagResource` with `ecs:CreateAction`) |
| `RunTask`: owner tag on the revision, `iam:PassRole` by `iam:ResourceTag` | ✓ allowed on the user's revision with their own roles; on a revision tagged with another owner, denied |
| `DeregisterTaskDefinition` | ✗→decided (R8): ECS authorizes it against `*` only. `task-definition/triple-c-*`, and `*` with `aws:ResourceTag/triple-c:owner`, are both denied; only an unconditional `*` works (probe). The workspace tier no longer has it (`RetireOwnTaskDefinitions` dropped); removing a project leaves its revisions registered for an admin to clear |
| `DeleteTaskDefinitions` | (probe) resource-level: authorized against the revision's ARN with `aws:ResourceTag` (the owner's tag passes, another owner's is denied), but it only deletes INACTIVE revisions, so without deregistration it is useless to a user. Dropped from the workspace tier with it (R8) |
| `CreateAccessPoint`'s `TagResource` | ✗→fixed: authorized against the **file system**, not the access point (probe: `access-point/*` denied; `file-system/*` allowed; `CreateAction`, `aws:RequestTag` and `aws:TagKeys` all present) |
| Bare `TagResource` on the user's own file system | ✓ denied (no `elasticfilesystem:CreateAction`): `TagOwnAccessPointsAtCreate` tags only at access-point creation |
| `GetSecretValue` | ✓ own secret (an attach reads the token back); another owner's `triple-c/*` secret denied |
| EFS access point with the task role (IAM + TLS) | ✓ mounted at `/mnt/triple-c-home` (`127.0.0.1:/ … nfs4`, the TLS tunnel), `claude:claude 700`; `~/.claude`, `~/.claude.json` and `~/.ssh` link into it |
| Times | first start (register, access point, RunTask, pull of ~1.2 GB, agent, tunnel) 94–104 s; ECS `HEALTHY` 135 s after the start began; attach to a running task 2 s; StopTask to `STOPPED` 42 s; restart 86–92 s |
| Attach | ✓ a second start while running reuses the task (one task in the family, same ARN, no RunTask) |
| Restart | ✓ new task, same revision; the `~/.claude` marker survived |
| Repo clone | ✓ the background clone finished (`/run/triple-c-clone/repos`: `Hello-World cloned`) |
| Stop guard and WIP push | ✓ `triple-c-wip-push --dry-run` reports the dirty repo; after StopTask the log shows `entrypoint: stopping (sigterm) — pushing work in progress...` and the push failing for lack of GitHub credentials. No push with real credentials was tried |
| Idle stop (5 min) | ✓ with a terminal held open and the auth bridge's background poll running, the task stopped itself 365 s after the last use (`triple-c-idle-watch: idle for 5 minutes`, `entrypoint: stopping (idle) — work in progress already handled by the idle watcher`); ECS reports `EssentialContainerExited` with exit code 75, which the app classifies as an idle stop (re-run 2026-11-15 after the PR #82 fixes) |
| `~/.claude.json` after Claude Code runs | ✓ still a link (Claude Code started without a login: "Not logged in") |
| Bubblewrap (P9) | ✗ `bwrap --unshare-user` fails: "No permissions to create new namespace" (kernel 6.1, `max_user_namespaces` 16384, no `CAP_SYS_ADMIN` on Fargate). The sandbox stays off on AWS |
| A revision per configuration | turning idle stop on adds a secret key, so the next start registers a new revision (by design) |
| Removal (R8) | ✓ final run: `remove_aws_resources` succeeds; the secret is scheduled for deletion, the access point deleted, the revision left ACTIVE. Since R10 the secret is deleted without a recovery window (the test now asserts it is gone) |
| Cleanup | ✓ nothing `triple-c` left in us-west-2; environment KMS keys pending deletion |

### Phase 6: the cost estimate's price reads (2026-11-16)

`cargo test aws::live_tests::a_ -- --ignored --test-threads=1` (the four permission-check tests
plus `a_workspace_role_reads_fargate_prices_and_a_bare_role_cannot`), us-west-2, all passed.

| Point | Result |
|---|---|
| `pricing:GetProducts` on `*` (`ReadFargatePrices`) | ✓ a role holding only the workspace files reads us-west-2's rates from the us-east-1 endpoint (`AmazonECS`; `TERM_MATCH regionCode`, `TERM_MATCH termType=OnDemand`, `CONTAINS usagetype=Fargate`: eight rows, one page). x86 $0.04048/vCPU-h, $0.004445/GB-h; ARM $0.03238, $0.00356; ephemeral storage $0.000111/GB-h |
| Without the permission | ✓ a role with no policy is denied (`AccessDeniedException` naming `pricing:GetProducts`); the app shows "Estimate unavailable" |
| The permission check | ✓ the simulator allows the new check for the workspace tier and denies it for a role with no policy |

### Phase 6: one workspace with every option (2026-11-16)

```sh
TRIPLEC_E2E_TEST=aws::live_tests::environment::a_phase6_workspace_round_trips \
TRIPLEC_AWS_E2E_PROFILE=<admin profile> TRIPLEC_AWS_E2E_REGION=us-west-2 scripts/aws-e2e-workspace.sh
```

Same fixture as phase 5 (`deploy_for_workspace_user`: environment admin deploys, a role holding
only the workspace files is onboarded and drives the app as a profile connection), with the
environment's `max_workspace_hours = 1`. One project, two task starts, 1 vCPU / 2 GiB x86: EFS
storage, one public repo, a setup script, copy-at-connect (a CA file, a git token and a custom
variable — all dummies), a port mapping 127.0.0.1:<free port> → 8099, the gateway sidecar
(OpenAI-compatible backend, one gateway model, a dummy provider key handed in instead of the
keychain via `aws_host::ensure_started_with`), idle stop off, WIP push off. Passed in 600 s,
spend under $0.05.

| Point | Result |
|---|---|
| Environment module v2 + cap | ✓ the record reads `template_version 2`, `max_workspace_hours 1` (as the workspace user); the secret has `TRIPLEC_MAX_LIFETIME_MINUTES=60`, PID 1's environment has it (root exec, that one line grepped), one `triple-c-idle-watch` runs with idle stop off |
| v1 → v2 Update | not exercised: the fixture applies only the embedded modules (unit-tested: an old record shows "Update available") |
| Lifetime cap firing | not exercised live (60 minutes; Task 4's unit tests) |
| Times | cold start (two access points, register, RunTask, pull of both images, agent, tunnel) 115 s; restart 112 s; copy-at-connect push 3 s; gateway `/health/liveliness` 200 at 126 s after the start began |
| EFS `/workspace` | ✓ `127.0.0.1:/ /workspace nfs4` (TLS tunnel); Hello-World cloned onto it; a marker written in task 1 was there in task 2; the clone reported `exists` the second time; both access point ids kept in the store across the restart |
| Hybrid links under EFS | ✓ `~/.claude` and `~/.claude.json` → `/mnt/triple-c-home/…`; `~/.ssh` is a local directory (R6, copy-at-connect) |
| Setup script | ✓ `sudo apt-get install -y jq` (already in the image: "jq is already the newest version"), `jq --version`, marker on `/workspace`; `aws_workspace_status`'s read (`triple-c-workspace-status` through the tunnel) settled with clone `cloned` + setup `ok` (code 0, log tail); same again on the restart |
| Copy-at-connect | ✓ secret keys listed (values never printed): no `TRIPLEC_FILES`, `GIT_TOKEN`, custom variable or `TRIPLE_C_CUSTOM_ENV`, `TRIPLEC_CREDENTIALS` present; neither dummy value anywhere in the secret; after the push `/usr/local/share/ca-certificates/triple-c-e2e-ca.crt` (`subject=CN = triple-c-e2e-ca`) and a git credential helper are set |
| Port mapping | ✓ `python3 -m http.server 8099 --bind 127.0.0.1` (detached exec); `port_forwards::open_for` → `GET /` on the host's loopback port: `HTTP/1.0 200 OK` |
| Gateway sidecar | ✓ from the workspace: `/health/liveliness` 200; `/v1/models` with the master key 200 listing `e2e-model`, without it 401; claude's login shell has `ANTHROPIC_BASE_URL=http://127.0.0.1:4000` and a token; the sidecar's `TRIPLEC_GATEWAY_*` stay in the secret (R1); a new master key each start |
| Keys in the logs | ✓ after a failed `/v1/chat/completions` (dummy key → OpenAI 401) and the successful calls: the provider key, the master key, the git token and the custom value are in **0** lines of the gateway stream (`<prefix>/gateway/<task-id>`, 107 lines) and of the workspace stream. LiteLLM logs the exception and traceback, not request headers; the provider's own error text (also in the HTTP response) carries the key **masked** by OpenAI (`sk-e2e-p…1249`: first 8 and last 4 characters) |
| Cost estimate | ✓ `fetch_rates` as the workspace user + `estimate`: $0.0495/hour for 1 vCPU / 2 GiB / 21 GiB x86, `efs_billed_separately` |
| Removal with discard | ✓ `remove_aws_resources(…, true)` Ok; both access points gone; the secret deleted |
| Cleanup | ✓ nothing `triple-c` left in us-west-2 (the run's environment KMS key pending deletion). The guard's task-definition retirement had never matched anything (ECS's `familyPrefix` is a whole family name); fixed, and 13 revisions earlier runs had left ACTIVE were deregistered and deleted |
