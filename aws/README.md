# Triple-C AWS policies

IAM policies for running Triple-C workspaces on AWS (ECS). Attach them to the IAM users, groups,
roles or SSO permission sets of the people who use Triple-C. The app checks which tier the
signed-in identity has under **Settings → Hosts → Check**.

| File | Tier | Grants |
|---|---|---|
| `policies/workspace-user.json` | Workspace user | Create, start, stop, connect to and delete **their own** workspaces in an existing environment |
| `policies/environment-admin.json` | Environment admin | Deploy, update and delete the environment stack, and onboard users |

An environment admin needs **both** files: the admin policy adds environment management on top of
everything a workspace user can do.

## Workspace user

Every per-user resource is tagged `triple-c:owner = ${aws:userid}` (the `UserId` that
`aws sts get-caller-identity` prints). The policy uses that tag so a user can only:

- run tasks from `triple-c-*` task definitions on `triple-c-*` clusters, tagged with their own
  owner ID, passing only `triple-c-*` roles tagged with their owner ID, and only to ECS tasks;
- stop, `ecs execute-command` into, or open an SSM session to **their own** tasks, and end or
  resume only sessions they started;
- create, update and delete `triple-c/*` secrets tagged with their owner ID, using KMS only
  through Secrets Manager;
- read: ECS, CloudFormation stacks, the `/triple-c/*` SSM parameters, the secret list, EFS and the
  `/triple-c/*` log groups. Listing is not a secret; the app filters by owner for display.

## Environment admin

Manage `triple-c-*` CloudFormation stacks and the resources they create: `triple-c-*` IAM roles,
ECS clusters, capacity providers and task definitions, security groups, EFS file systems, KMS
keys, `/triple-c/*` log groups, ECR repositories and pull-through cache rules, and `/triple-c/*`
SSM parameters. It also grants `iam:SimulatePrincipalPolicy`, which the app's permission check
uses.

This is an account-level role: it can create and modify `triple-c-*` IAM roles, so grant it only
to people you would trust to administer the account. Consider pairing it with a permissions
boundary.

## Attaching

Console: **IAM → Policies → Create policy → JSON**, paste the file, then attach the policy to a
user, group or role. For IAM Identity Center, add it as an inline policy on the permission set.

CLI:

```sh
aws iam create-policy --policy-name triple-c-workspace-user \
  --policy-document file://policies/workspace-user.json
aws iam attach-user-policy --user-name <user> \
  --policy-arn arn:aws:iam::<account>:policy/triple-c-workspace-user
```

## Verification

The app's permission-check table (`app/src-tauri/src/aws/policies.rs`) is unit-tested against
these files: every action has a check, and the owner-scoped actions are denied for someone else's
resources. Every statement is also verified against a real account with the IAM policy simulator
by the live permission test before release. The simulator does not apply SCPs, permission
boundaries or session policies, so the first real create is the final check.
