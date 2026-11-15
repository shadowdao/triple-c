# Triple-C AWS policies

IAM policies for running Triple-C workspaces on AWS (ECS). Attach them to the IAM users, groups,
roles or SSO permission sets of the people who use Triple-C. The app checks which tier the
signed-in identity has under **Settings → Hosts → Check**.

| File | What it is |
|---|---|
| `policies/workspace-user.json` | **Workspace user** tier: create, start, stop, connect to and delete **their own** workspaces in an existing environment |
| `policies/environment-admin.json` | **Environment admin** tier: deploy, update and delete the environment stack, and onboard users |
| `policies/permissions-boundary.json` | The **permissions boundary** every role Triple-C creates must carry. It is not attached to people |

An environment admin needs **both** tier files: the admin policy adds environment management on
top of everything a workspace user can do.

## Workspace user

Every per-user resource is tagged `triple-c:owner = ${aws:userid}` (the `UserId` that
`aws sts get-caller-identity` prints). The policy uses that tag so that a user can only do the
following.

- **Run tasks:**
  - from `triple-c-*` task definitions tagged with their own owner ID, on `triple-c-*` clusters;
  - with the new task tagged with their own owner ID;
  - passing only `triple-c-*` roles tagged with their owner ID, and only to ECS tasks.
- **Their own tasks:** stop them, `ecs execute-command` into them, or open an SSM session to
  them. They can end or resume only sessions they started.
- **Secrets:** create, update and delete `triple-c/*` secrets tagged with their owner ID, using
  KMS only through Secrets Manager.
- **EFS access points:** create and delete access points tagged with their owner ID.
- **Their own principal:** read it (`iam:GetRole`) and simulate it
  (`iam:SimulatePrincipalPolicy`). The app's permission check uses both.
- **Read:** ECS, CloudFormation stacks, the `/triple-c/*` SSM parameters, the secret list, EFS and
  the `/triple-c/*` log groups. Listing is not a secret; the app filters by owner for display.

## Environment admin

The admin tier manages `triple-c-*` CloudFormation stacks and the resources they create:

- `triple-c-*` IAM roles;
- ECS clusters, capacity providers and task definitions;
- security groups, EFS file systems and KMS keys;
- `/triple-c/*` log groups and `/triple-c/*` SSM parameters;
- ECR repositories and pull-through cache rules.

It is fenced in two ways.

**Roles are bounded.** The admin can create a `triple-c-*` role or change its policies only if the
role carries the `triple-c-permissions-boundary` managed policy as its permissions boundary. The
boundary is the most a Triple-C role can ever hold:

- ECR image pulls;
- writing `/triple-c/*` logs;
- ECS Exec channels;
- reading `triple-c/*` secrets, decrypting only through Secrets Manager;
- mounting EFS;
- `sts:GetCallerIdentity`;
- invoking Bedrock models.

It grants nothing in IAM. Explicit Deny statements stop the admin from removing the boundary from
a role, and from editing or deleting the boundary policy itself. Without these fences, an admin
could create a role with any permissions and become an account administrator.

**Destruction is tag-scoped.** Security groups, EFS file systems and KMS keys can be created only
with a `triple-c:component` tag. They can be modified or deleted only if they carry one, so the
admin tier cannot touch resources that aren't Triple-C's. Tagging is allowed only as part of the
creating call, or on a resource that is already Triple-C's.

## One-time setup: the boundary policy

Before anyone can deploy an environment, an **account administrator** creates the boundary policy
once. The environment-admin tier deliberately cannot do this.

```sh
aws iam create-policy --policy-name triple-c-permissions-boundary \
  --policy-document file://policies/permissions-boundary.json
```

Create it **without a path**. The admin policy expects exactly
`arn:aws:iam::<account>:policy/triple-c-permissions-boundary`. Phase 4 decides how the app offers
this step (for example, as part of the environment stack's prerequisites).

## Attaching the tier policies

Console: **IAM → Policies → Create policy → JSON**, paste the file, then attach the policy to a
user, group or role. For IAM Identity Center, add it as an inline policy on the permission set.

CLI:

```sh
aws iam create-policy --policy-name triple-c-workspace-user \
  --policy-document file://policies/workspace-user.json
aws iam attach-user-policy --user-name <user> \
  --policy-arn arn:aws:iam::<account>:policy/triple-c-workspace-user
```

## Known gaps (phase 4)

These arrive with the environment and onboarding templates.

- **Per-user execution roles.** A workspace user can pass only roles tagged with their own owner
  ID, so each user's onboarding stack must create their execution role as well as their task
  role. Alternatively, a separate statement can allow the environment's shared execution role.
- **The admin's full deploy permissions.** Deploying a stack will also need:
  - tag-on-create permissions for ECS, CloudWatch Logs, SSM and ECR;
  - autoscaling and instance-profile actions for EC2 capacity;
  - `cloudformation:ValidateTemplate`;
  - whatever else the templates create.

  These will be added, with the same boundary and tag scoping, once the templates exist.

## Verification

The app's permission-check table (`app/src-tauri/src/aws/policies.rs`) is unit-tested against
these files with a small IAM-style evaluator. The tests check that:

- every allowed action has a check;
- owner-scoped actions are denied for someone else's resources;
- every role write requires the boundary;
- destructive admin actions need the `triple-c:component` tag.

Before release, every statement is also verified against a real account with the IAM policy
simulator, by the live permission test. The simulator does not apply SCPs, permission boundaries
or session policies, so the first real create is the final check.
