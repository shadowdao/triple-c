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
  SSM session to them. They can end or resume only sessions they started.
- **Manage their own secrets.** They can create, update and delete `triple-c/*` secrets tagged with
  their owner ID, and use KMS only through Secrets Manager.
  - A secret tagged `triple-c:component` belongs to the environment. A workspace user can never
    tag, untag, overwrite or delete one; an explicit Deny enforces this.
- **Manage their own access points.** They can create and delete EFS access points tagged with
  their owner ID, and only on the environment's file system, which is the one tagged
  `triple-c:component`.
- **Inspect their own principal.** They can read it (`iam:GetRole`) and simulate it
  (`iam:SimulatePrincipalPolicy`). The app's permission check uses both.
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
  - mounting and writing only environment (component-tagged) file systems, with no root access,
    because access points fix the POSIX user;
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
  *any* task definition revision in the account. Narrowing this to `task-definition/triple-c-*`
  depends on resource-level support, which is to be verified live.
- `ecr:CreatePullThroughCacheRule` on `*`. The admin can add pull-through cache rules for any
  prefix. This cannot delete or overwrite anything.
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

In the console, go to **IAM → Policies → Create policy → JSON**, paste the file, and attach the
policy to a user, group or role. For IAM Identity Center, add it as an inline policy on the
permission set.

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

**Phase 6**

- **Enforced access-point roots.** A workspace user creates their own access points. The
  `RootDirectory` and `PosixUser` must be enforced, so that an access point rooted at `/` cannot
  read other users' data. Two ways to do this: an EFS file-system policy that pins each access
  point to its owner's task role, or access points created by the onboarding stack rather than by
  the user.

## To verify live (Task 8)

The unit tests use a small IAM-style evaluator. These points need the real IAM policy simulator or
a real account:

- **KMS key tags.** Does `CreateKey` with tags authorize `kms:TagResource` against the new,
  untagged key? If so, the tag rule on `kms:TagResource` denies it. Do **not** fix that by removing
  the condition from `TagResource`.
- **Principal variables.** Check how `${aws:PrincipalAccount}` and `${aws:PrincipalArn}` resolve.
  For an SSO session, does the role ARN include the `/aws-reserved/sso.amazonaws.com/<region>/`
  path? Does the simulator substitute them in `Resource`?
- **ECS Exec on the cluster.** `ecs:ExecuteCommand` may also be authorized on the cluster ARN.
- **ECS tag keys.** On ECS resources, check `aws:ResourceTag` against `ecs:ResourceTag`.
- **SSM sessions.** Check `ssm:StartSession` on an ECS task with `aws:ResourceTag`.
- **Session ownership for SSO callers.** Check the
  `ssm:resourceTag/aws:ssmmessages:session-id` condition when the caller signed in through SSO.
- **EC2 and EFS tagging.** Check the `ec2:CreateAction` and `elasticfilesystem:CreateAction`
  tag-on-create keys. Check whether `ec2:AuthorizeSecurityGroup*` and `RevokeSecurityGroup*` are
  also authorized against `security-group-rule` resources.
- **Secrets tagging.** Check that `CreateSecret` with tags authorizes `secretsmanager:TagResource`,
  and that the `StringEqualsIfExists` and `Null` resource-tag conditions pass at create time.
- **CloudFormation service roles.** Check that `cloudformation:RoleArn` is present in the request
  context when `RoleARN` is given.
- **Task definition scoping.** Check whether `ecs:DeregisterTaskDefinition` and
  `ecr:CreatePullThroughCacheRule` support resource-level scoping, so they can be narrowed.

The simulator does not apply SCPs, permission boundaries or session policies, so the first real
create is the final check.
