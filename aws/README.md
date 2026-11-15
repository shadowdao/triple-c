# Triple-C AWS policies

These are the IAM policies for running Triple-C workspaces on AWS (ECS). Attach them to the IAM
users, groups, roles or SSO permission sets of the people who use Triple-C. The app reports which
tier the signed-in identity has under **Settings → Hosts → Check**.

| File | What it is |
|---|---|
| `policies/workspace-user.json` | The **workspace user** tier: create, start, stop, connect to and delete **their own** workspaces in an existing environment |
| `policies/environment-admin.json` | The **environment admin** tier, part 1: apply and destroy the environment and onboarding modules (`terraform/`) with OpenTofu, and keep their state |
| `policies/environment-admin-network.json` | The **environment admin** tier, part 2: the network those modules create (VPC, subnets, gateways, routes, security groups) |
| `policies/permissions-boundary.json` | The **permissions boundary** every Triple-C role must carry. It is never attached to a person |

The admin tier is **two** files, because one managed policy holds only 6,144 characters; both are
required, and the app's **Copy** lists every file of a tier. An environment admin attaches all three
tier files (the workspace user's and both admin files): the admin files add environment management
on top of everything a workspace user can do. Explicit Deny statements in any file apply to anyone
who has it attached.

## Workspace user

Every per-user resource is tagged `triple-c:owner = ${aws:userid}`. That value is the `UserId`
printed by `aws sts get-caller-identity`. The policy uses this tag so that a user can only do the
following:

- **Run tasks.** The task definition must be a `triple-c-*` one tagged with the user's own owner
  ID, and the cluster must be a `triple-c-*` one. The new task is tagged with the user's owner ID.
  The user can pass only `triple-c-*` roles tagged with their owner ID, and only to ECS tasks.
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
  another user's file system is refused even though it is Triple-C's.
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
- ECS clusters (Fargate and Fargate Spot only) and task definitions;
- the network: VPCs, subnets, an internet gateway, route tables and routes, an Elastic IP and NAT
  gateway, security groups and their rules (`environment-admin-network.json`);
- EFS file systems, mount targets and file-system policies, and KMS keys and aliases;
- `/triple-c/*` log groups and `/triple-c/*` SSM parameters.

Not in v1, and so not in the policy: ECR (tasks pull from GHCR), custom ECS capacity providers,
and passing roles to EC2 (`iam:PassRole` is to `ecs-tasks.amazonaws.com` only). Phase 7's EC2
capacity adds them back.

### What each module resource needs

`app/src-tauri/src/aws/tofu_modules/tests.rs` (`MODULE_CALLS`) maps every resource type, data
source and the S3 backend the modules use to the IAM actions the AWS provider calls to create,
read, tag, update and delete it, and a test evaluates each against the admin files. It is the
**hypothesis**; a live apply and destroy is the proof, and any action that shows missing is added
to the table, the policy, the check table in `policies.rs` and this list together.

| Resource | Actions (authorized against) |
|---|---|
| `aws_ecs_cluster` | `ecs:CreateCluster`, `TagResource` (at create), `DescribeClusters`, `ListTagsForResource`, `UpdateCluster`, `UntagResource`, `DeleteCluster` (cluster `triple-c-*`) |
| `aws_ecs_cluster_capacity_providers` | `ecs:PutClusterCapacityProviders`, `DescribeClusters` (the cluster; untaggable) |
| `aws_security_group` | `ec2:CreateSecurityGroup` (group and VPC, request tag), `CreateTags` (at create), `RevokeSecurityGroupEgress` (AWS's default rule), `Authorize`/`RevokeSecurityGroupIngress`/`Egress` and `UpdateSecurityGroupRuleDescriptions*` (tagged group; new rules: `security-group-rule/*`), `CreateTags`/`DeleteTags`, `DeleteSecurityGroup` (tagged), `Describe*` |
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
| `aws_eip` | `ec2:AllocateAddress` (request tag), `CreateTags` (at create), `ReleaseAddress` (tagged), `Describe*` |
| `aws_nat_gateway` | `ec2:CreateNatGateway` (gateway: request tag; its subnet, address and VPC: tagged), `CreateTags` (at create), `DeleteNatGateway` (tagged), `Describe*` |
| `aws_efs_file_system` | `elasticfilesystem:CreateFileSystem` (`*`, request tag), `TagResource` (at create), `UpdateFileSystem`, `PutLifecycleConfiguration`, `TagResource`/`UntagResource`, `DeleteFileSystem` (tagged), `Describe*`, `ListTagsForResource` |
| `aws_efs_mount_target` | `elasticfilesystem:CreateMountTarget`, `ModifyMountTargetSecurityGroups`, `DeleteMountTarget` (the tagged file system; untaggable), and the interface EFS makes with the caller's permissions: `ec2:CreateNetworkInterface`, `ec2:DeleteNetworkInterface` (only with `aws:CalledViaLast = elasticfilesystem.amazonaws.com`; in the environment's subnets, which in `existing` mode are the user's own) |
| `aws_efs_file_system_policy` | `elasticfilesystem:PutFileSystemPolicy`, `DeleteFileSystemPolicy` (the tagged file system; untaggable), `DescribeFileSystemPolicy` |
| `aws_iam_role` | `iam:CreateRole` (with the boundary), `TagRole`, `UntagRole`, `GetRole`, `ListRolePolicies`, `ListAttachedRolePolicies`, `ListInstanceProfilesForRole`, `UpdateRole`, `UpdateRoleDescription`, `DeleteRole` (`triple-c-*`) |
| `aws_iam_role_policy` | `iam:PutRolePolicy`, `DeleteRolePolicy` (with the boundary), `GetRolePolicy` (the role; untaggable) |
| data sources | `sts:GetCallerIdentity`, `ec2:DescribeAvailabilityZones`, `ec2:DescribeSubnets`, `ssm:GetParameter` |
| S3 backend | `s3:ListBucket` (the state bucket), `GetObject`/`PutObject`/`DeleteObject` (`triple-c-tfstate-*/*`, the `.tflock` objects included), in the caller's own account |

The app also creates the state bucket: `s3:CreateBucket` with a `triple-c:component` request tag,
then `PutBucketVersioning`, `PutEncryptionConfiguration`, `PutBucketPublicAccessBlock`,
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
  - invoking Bedrock models.
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
Triple-C's. Resources that cannot carry a tag are authorized through one that does: routes and
route-table associations through the route table (and subnet), mount targets and the file-system
policy through the file system, KMS aliases through the key, role policies through the
(boundary-carrying) role. Clusters, log groups, parameters, roles and the state bucket are scoped
by name (`triple-c-*`, `/triple-c/*`, `triple-c-tfstate-*`).

**What is not scoped.** The scoping above does not cover every action. These actions apply
account-wide:

- `ecs:RegisterTaskDefinition` and `ecs:DeregisterTaskDefinition` on `*`. The admin can deregister
  *any* task definition revision in the account. `DeregisterTaskDefinition` has no resource type
  (IAM authorizes it against `*`), so it cannot be narrowed.
- `elasticfilesystem:CreateFileSystem` on `*` (no resource type), still only with a
  `triple-c:component` request tag.
- `ec2:CreateSecurityGroup` in **any** VPC, as long as the new group is tagged: an
  `existing`-network environment creates its tasks group and each user's `-efs` group in the
  user's own, untagged VPC. It is the only create that may name an untagged parent.
- `ec2:AuthorizeSecurityGroupIngress`/`Egress` on `security-group-rule/*`: authorizing names the
  new, untagged rule as well as the group, and the group must still be tagged.
- `ec2:CreateNetworkInterface` and `ec2:DeleteNetworkInterface` on `*`, but only when EFS makes
  the call for the admin (`aws:CalledViaLast = elasticfilesystem.amazonaws.com`): EFS creates and
  deletes a mount target's interface with the caller's EC2 permissions, in any subnet (an
  `existing` environment's are untagged). **Unverified:** whether EFS sets `aws:CalledVia` for
  these calls is for Task 8's live run to show; if it does not, this statement needs another
  condition.
- Reads on `*`: `ec2:Describe*`, `elasticfilesystem:Describe*`, the KMS key reads and
  `ListAliases`, `logs:DescribeLogGroups` and `ssm:DescribeParameters`.
- `s3:PutBucketPolicy` on the state bucket (only a bucket in the admin's own account, enforced by
  `s3:ResourceAccount`): an admin could open that bucket to other accounts. The state holds no
  secrets (the modules create none), but it maps the account's Triple-C resources.

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

Create the policy **without a path**. The admin policy expects exactly
`arn:aws:iam::<account>:policy/triple-c-permissions-boundary`. The `terraform/permissions-boundary`
module creates the same policy, applied by an account administrator; the admin tier can read it
(`iam:GetPolicy`, which detection uses) but never create or change it.

## Attaching the tier policies

Create each file as a **customer managed policy** and attach it. In the console, go to
**IAM → Policies → Create policy → JSON**, paste the file, and attach the policy to a user, group
or role. Inline policies do not fit: a role's inline policies share 10,240 characters and an
environment admin needs every tier file (about 14,500), and a user's share 2,048. A managed policy
may hold 6,144 characters, not counting whitespace. Without whitespace, `workspace-user.json` is
about 6,040 (roughly 100 characters of headroom), `environment-admin.json` about 5,790 (350)
and `environment-admin-network.json` about 3,280; a unit test (`every_policy_fits_a_managed_policy`)
fails before any file outgrows the limit. A tier that outgrows its files gains another file, which
the app then lists with the others. For IAM Identity Center, add the files as inline policies on the permission set
(the "up to 32,768 characters" limit is unverified) or reference the managed policies.

With the CLI:

```sh
aws iam create-policy --policy-name triple-c-workspace-user \
  --policy-document file://policies/workspace-user.json
aws iam attach-user-policy --user-name <user> \
  --policy-arn arn:aws:iam::<account>:policy/triple-c-workspace-user
```

## Known gaps

**Phase 4**

- **Per-user execution roles.** A workspace user can pass only roles tagged with their own owner
  ID, so each user's onboarding module creates their execution role as well as their task role
  (done: `terraform/user-onboard/iam.tf`).
- **The admin's deploy permissions are a hypothesis until a live apply.** The module table above
  is derived from the provider's behaviour and the Service Authorization Reference, not yet from a
  real `tofu apply`/`destroy`. In particular unverified: whether tag-on-create needs
  `logs:TagResource` / `ssm:AddTagsToResource` / `ecs:TagResource` as listed, whether EFS's
  interface calls carry `aws:CalledViaLast = elasticfilesystem.amazonaws.com`, whether
  `s3:ResourceAccount` is evaluated for `s3:CreateBucket`, and whether `s3:CreateBucket` honours
  `aws:RequestTag`. A trust-policy change on a role (`iam:UpdateAssumeRolePolicy`) is never
  allowed, so a module version that changes a role's trust must replace the role.
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
check. The phase-4 additions (the network, the state bucket, the module table above, the
owner-scoped `CreateAccessPoint`) are verified by Task 8's live apply and destroy; until then they
are unit-tested only.
