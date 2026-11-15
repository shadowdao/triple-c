# Triple-C AWS policies

These are the IAM policies for running Triple-C workspaces on AWS (ECS). Attach them to the IAM
users, groups, roles or SSO permission sets of the people who use Triple-C. The app reports which
tier the signed-in identity has under **Settings → Hosts → Check**.

| File | What it is |
|---|---|
| `policies/workspace-user.json` | The **workspace user** tier: create, start, stop, connect to and delete **their own** workspaces in an existing environment |
| `policies/environment-admin.json` | The **environment admin** tier: deploy, update and delete the environment stack, and onboard users |
| `policies/permissions-boundary.json` | The **permissions boundary** every Triple-C role must carry. It is never attached to a person |

An environment admin needs **both** tier files. The admin policy adds environment management on
top of everything a workspace user can do. Explicit Deny statements in either file apply to anyone
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
    under the key policy the environment stack controls. (The permissions boundary does *not*
    stop a task role decrypting an `aws/secretsmanager` secret: that key's policy grants the
    caller directly.)
  - Every `TagResource` on a secret must carry `triple-c:owner = ${aws:userid}` in the request,
    even when it only adds `triple-c:project`.
  - A secret tagged `triple-c:component` belongs to the environment. A workspace user can never
    tag, untag, overwrite or delete one; an explicit Deny enforces this.
- **Manage their own access points.** They can create and delete EFS access points tagged with
  their owner ID, and only on the environment's file system, which is the one tagged
  `triple-c:component`.
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
- **Read environment information.** This covers ECS, CloudFormation stacks, the `/triple-c/*` SSM
  parameters, the secret list, EFS, and the `/triple-c/*` log groups. Listing is not a secret; the
  app filters by owner for display.

## Environment admin

The admin tier manages `triple-c-*` CloudFormation stacks and the resources they create:

- `triple-c-*` IAM roles;
- ECS clusters, capacity providers and task definitions;
- security groups, EFS file systems and KMS keys;
- `/triple-c/*` log groups and `/triple-c/*` SSM parameters;
- ECR repositories and pull-through cache rules.

It is fenced in the following ways.

**Roles are bounded.** The admin can create a `triple-c-*` role, or change its policies, only when
the role carries the `triple-c-permissions-boundary` managed policy as its permissions boundary.

- The boundary is the most a Triple-C role can ever hold:
  - ECR image pulls;
  - writing `/triple-c/*` logs;
  - ECS Exec channels;
  - reading `triple-c/*` secrets;
  - decrypting only through Secrets Manager, and only with environment (component-tagged) keys;
  - mounting and writing only environment (component-tagged) file systems; whether that excludes
    root access depends on access points fixing the POSIX user and root directory, which is only
    true once phase 6 enforces them (see "Enforced access-point roots" under Phase 6 gaps);
  - `sts:GetCallerIdentity`;
  - invoking Bedrock models.
- The boundary grants nothing in IAM.
- Explicit Deny statements stop the admin from:
  - removing the boundary from a `triple-c-*` role;
  - setting any other boundary on one;
  - editing or deleting the boundary policy itself.

Without these fences an admin could create a role with any permissions and become an account
administrator.

**No CloudFormation service roles.** The admin cannot create or update a `triple-c-*` stack, or
create a change set for one, with a service role (`RoleARN`).

**Destruction is tag-scoped.** Security groups, EFS file systems and KMS keys can be created only
with a `triple-c:component` tag. They can be changed or deleted only if they carry that tag.
Tagging is allowed only as part of the creating call, or on a resource that is already
Triple-C's.

**What is not scoped.** The scoping above does not cover every action. These actions apply
account-wide:

- `ecs:RegisterTaskDefinition` and `ecs:DeregisterTaskDefinition` on `*`. The admin can deregister
  *any* task definition revision in the account. `DeregisterTaskDefinition` has no resource type
  (IAM authorizes it against `*`), so it cannot be narrowed.
- `ecr:CreatePullThroughCacheRule` on `*`, which also has no resource type. The admin can add
  pull-through cache rules for any prefix. This cannot delete or overwrite anything.
- `elasticfilesystem:CreateFileSystem` on `*` (no resource type), still only with a
  `triple-c:component` request tag.
- `ec2:CreateSecurityGroup` in any VPC (the new group itself must be tagged), and `ec2:Describe*`.
- `cloudformation:*` on `triple-c-*` stacks. Whatever a template creates is limited by the actions
  above, because CloudFormation runs with the caller's own permissions (see the invariants below).

## Invariants the templates must keep (phase 4)

The policies rely on the following. Phase 4's templates must establish them, and phase 4's
detection should flag any environment that breaks them.

1. **Every `triple-c-*` role carries `triple-c-permissions-boundary`.** The admin tier cannot
   create one without it. A role created some other way, such as by an account administrator or by
   hand, must still have the boundary. Detection should flag a `triple-c-*` role that has no
   boundary.
2. **`triple-c-*` stacks are never deployed with a CloudFormation service role.** `UpdateStack`
   and change sets on a stack that already has a service role reuse it without `iam:PassRole`,
   and that role is not under the boundary. The admin tier denies setting `RoleARN`. Detection
   should flag a `triple-c-*` stack whose `RoleARN` is set.
3. **Every environment-level `triple-c/*` secret carries `triple-c:component`.** A secret with
   neither an owner tag nor a component tag could be claimed by any workspace user.
4. **Every environment KMS key and EFS file system carries `triple-c:component`.** The boundary
   and the admin tier both rely on this tag.
5. **User secrets use the environment key.** The app must pass the environment KMS key's ARN as
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
`arn:aws:iam::<account>:policy/triple-c-permissions-boundary`. Phase 4 decides how the app offers
this step; one option is to list it with the environment stack's prerequisites.

## Attaching the tier policies

Create each file as a **customer managed policy** and attach it. In the console, go to
**IAM → Policies → Create policy → JSON**, paste the file, and attach the policy to a user, group
or role. Inline policies do not fit: a role's inline policies share 10,240 characters and an
environment admin needs both tiers (about 11,500), and a user's share 2,048. A managed policy may
hold 6,144 characters, not counting whitespace; the workspace policy is about 6,110 (roughly 30
characters of headroom, so phase 4 must split `workspace-user.json` into two policies before
adding anything to it), and a unit test (`every_policy_fits_a_managed_policy`) fails before it
outgrows that. For IAM Identity Center, add the files as inline policies on the permission set
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
  ID. Each user's onboarding stack must therefore create their execution role as well as their
  task role. The alternative is a separate statement that allows the environment's shared
  execution role.
- **The admin's full deploy permissions.** Deploying needs the following, added with the same
  boundary and tag scoping once the templates exist:
  - tag-on-create permissions for ECS, CloudWatch Logs, SSM and ECR;
  - autoscaling and instance-profile actions for EC2 capacity;
  - `cloudformation:ValidateTemplate`;
  - whatever else the templates create.
- **Admin access to user data.** The admin can retag `triple-c-*` roles (`iam:TagRole`). The
  boundary also allows every `triple-c/*` secret and every environment file system. As a result,
  isolating users *from the admin* depends on the onboarding template's per-user policies, not on
  IAM fences.
- **The workspace tier's secret Deny also applies to admins.** If the environment stack ever
  manages component-tagged secrets through the admin's credentials, that Deny will block
  `PutSecretValue`/`DeleteSecret` for them.

**Phase 5**

- **No secrets in task definitions or `RunTask` overrides.** Nothing sensitive (the agent token,
  git credentials) may travel in task-definition environment variables or in `RunTask`
  `containerOverrides`: every workspace user can read them through `ecs:Describe*` on `*`. Use
  Secrets Manager (`triple-c/*`) references instead.

**Phase 6**

- **Enforced access-point roots.** A workspace user creates their own access points. The
  `RootDirectory` and `PosixUser` must be enforced, so that an access point rooted at `/` cannot
  read other users' data. Two ways to do this: an EFS file-system policy that pins each access
  point to its owner's task role, or access points created by the onboarding stack rather than by
  the user.

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
| `cloudformation:RoleArn` | ✓ present when `RoleARN` is given; the Deny fires. The admin tier cannot pass any role to CloudFormation anyway (`PassRole` is only to ECS tasks and EC2) |
| Task definition / pull-through scoping | ✓ both authorize against `*`; they cannot be narrowed |

Still open for the phases that create the resources: `RunTask`'s tag keys and `iam:PassRole`'s
`iam:ResourceTag` on a real run, the owner tag on a real task for `StopTask`/`ExecuteCommand`/
`StartSession`, session ownership for an SSO-started session, and the EFS and security-group-rule
points above. The simulator does not apply session policies, so the first real create is the final
check.
