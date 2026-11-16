# Triple-C on AWS (ECS on EC2 / Fargate) — design

Date: 2026-11-13
Status: **draft for review** (user decisions recorded 2026-11-13 and 2026-11-14; phases 1-5 done). The user set the direction (local or AWS, login like the AWS CLI,
permission check, hosting options, secrets pushed automatically, shared accounts). Everything else
here is a proposal.

## Goal

A project can run its container on **AWS ECS** instead of the local Docker daemon, so it keeps
running for days without the user's machine. The user picks the host per project. Local Docker
keeps working exactly as it does today.

The AWS flow, in the user's words:

1. Pick **Local Docker** or **AWS** as the host.
2. AWS starts with a **login**, by SSO or access keys, the same way the AWS CLI does.
3. Triple-C **checks it has the permissions** it needs to set up the workspace.
4. The user picks **how it is hosted**: ECS on EC2 or Fargate, ephemeral storage or EFS.
5. SSH keys, certificates and similar uploads go to **Secrets Manager or straight to the
   instance**, without manual steps.
6. **A shared AWS account used by several people is the normal case**, not an edge case.

### Decisions (user, 2026-11-13)

| Topic | Decision |
|---|---|
| Who provisions | **Not necessarily an admin.** Triple-C **detects** existing Triple-C infrastructure in the account and offers **join it** or **deploy a new one**. Many users have no admin, so deploying is a first-class path, not an admin-only extra |
| WIP push before stop | **On by default** (per project, can be turned off) |
| Claude Code login | **Unchanged.** Anthropic login (`claude login`, OAuth through the auth bridge, or the shared token) or Bedrock, exactly as for local projects. The AWS host doesn't change how Claude authenticates |
| Tenancy | **Strict from v1, and the default.** Most people will set this up in their own accounts. Strict costs a single user nothing extra (they onboard themselves automatically), and it means the setup is already safe the day a second person joins |
| Connection model | **C, hybrid.** A connection is a login (profile or keys) plus a **default region**. Environments are detected per region when needed; each project records `(connection, region, environment)`. See [Connection model](#connection-model) |

### Decisions (user, 2026-11-14): environments are OpenTofu

The user chose **Terraform over CloudFormation**, the **app runs it** itself, with **OpenTofu**
(MPL-2.0, safe for the app to download and run), and **state in an S3 bucket** in the account.
Phase 4 added these decisions:

- **D2. Strict tenancy gives each user their own EFS file system**, created by their onboarding
  module. EFS can't bind an access point to its owner's task role (`ClientMount` sees the access
  point's ARN, not its tags, and access-point IDs are random), so the file-system policy admits
  only that user's task role. An empty file system costs nothing. The environment keeps the shared
  pieces: cluster, security group, KMS key, log group and network.
- **D3. No ECR in v1.** Tasks pull `ghcr.io/shadowdao/triple-c-sandbox` straight from GHCR (the
  default when an environment has no ECR mirror).
- **D4. Network modes** (`network_mode`): `public` (**default**: a new VPC, two public subnets,
  tasks get a public IP, the security group has no inbound rules, no NAT, $0); `private-nat` (a new
  VPC with two public and two private subnets and one NAT gateway; the UI says about $33/month);
  `existing` (the user's `vpc_id`, exactly two `subnet_ids` in different AZs, and
  `assign_public_ip`).
- **D5. An admin request is a folder bundle**, written into a folder picked through a Rust-side
  dialog: the module's files, `backend.hcl`, `terraform.tfvars.json` and a `README.txt` with the
  exact commands (`tofu init -backend-config=backend.hcl`, then `tofu apply`; Terraform 1.10+
  works too).
- **D6. The app destroys only incomplete deployments** (state without an environment record, or
  a failed apply), and never while any user's onboarding state for the environment remains. A working environment is destroyed by an admin with `tofu destroy` from a
  bundle, which is the irreversible case.
- **D7. The admin tier spans several policy files** (`environment-admin.json` and
  `environment-admin-network.json`), all required; `aws_policy_document` returns every file of a
  tier.
- **D8. CloudFormation left the policies**, with its actions, checks and probes. Detection for a
  workspace user reads only `ssm:GetParameter*` on `/triple-c/*` and `iam:GetRole` on
  `role/triple-c-*`; the bucket (state objects and locks) is admin-only, so a workspace user sees
  only environments that have a record.
- **D9. One state key per deployment**, in the per-account, per-region bucket
  `triple-c-tfstate-<account>-<region>`: `environments/<env>/terraform.tfstate`,
  `users/<env>/<ownerkey>/terraform.tfstate`, `boundary/terraform.tfstate`. The owner key is the
  first 12 hex characters of `sha256("<region>:<aws:userid>")` and also names the user's roles
  (`triple-c-<env>-<ownerkey>-task|exec`): role names are account-global and environments
  regional, so the region is part of it (the same user and environment name in two regions must
  not collide). The lock is the
  matching `.tflock` object (S3-native locking, `use_lockfile = true`); a held lock is a deploy in
  progress, and its `Who`/`Created`/`Operation` are shown.
- **D10. Applies are detached.** `tofu apply` runs in its own session, output to
  `<workdir>/apply.log`, PID in `<workdir>/apply.pid` (liveness is the runner's `apply.lock`). A
  run that can't save its state leaves `errored.tfstate`; nothing else starts there until it is
  pushed back (Push saved state: `tofu state push errored.tfstate` through the same runner).
  Closing the app doesn't interrupt it; on the next run the app sees the live runner or the held
  lock and reattaches by following the log. It interrupts only when the user presses Cancel (not
  on Windows yet), and OpenTofu then stops gracefully and saves state.

### Non-goals (v1)

- Kubernetes/EKS, other clouds, or remote Docker hosts. The host abstraction below allows them
  later.
- Per-project snapshot images (`docker commit`) on AWS. See [Snapshots](#snapshots).
- Moving an existing local project's state to AWS. A project can be switched to AWS only if its
  folders are git repos with remotes, and it starts fresh there.
- Running the speech-to-text container on AWS. The microphone is on the user's machine, so STT
  stays local.
- Docker Sandboxes, gVisor or Kata. That is a separate design.

## Terminology

`Project.backend` (`models/project.rs`) already means the **LLM provider** (Anthropic, Bedrock,
Ollama, llama.cpp, OpenAI-compatible). To avoid two meanings of "backend", this design uses:

| Term | Meaning |
|---|---|
| **Host** | Where a project's container runs: `Local` (Docker today) or `Aws` |
| **AWS connection** | Saved credentials and region (an SSO profile or access keys), stored in app settings |
| **Environment** | Shared infrastructure in one AWS account and region: cluster, VPC wiring, ECR, EFS, KMS key, IAM roles, log group. It is provisioned once, often by an admin, and used by many people |
| **Workspace** | One user's project inside an environment: task definition, EFS access point, secrets, and the running task |
| **Owner** | The AWS principal that created a workspace. Every per-user resource is tagged with it |

Environment vs. workspace is the central decision. It is what makes shared accounts work: admins
own the environment, users only ever create things inside their own workspaces.

## User flow

### 1. Settings → Hosts → Add AWS connection

- **Auth method** (same options as the AWS CLI):
  - **AWS profile**: any profile in `~/.aws/config`, including SSO profiles. SSO login reuses
    `commands/aws_commands.rs` (`is_sso_profile`, `check_sso_session`, `run_sso_login`), which
    already does this for Bedrock.
  - **Access keys**: access key ID, secret, and an optional session token. Stored in the OS
    keyring through `storage/secure.rs`, never in `settings.json`.
  - Later: the SSO device flow through the SDK (`aws-sdk-ssooidc`), so SSO works without the
    AWS CLI installed.
- **Default region.** Used for detection and preselected for new projects. Other regions are
  scanned only when the user picks one for a project.
- When saved, call `sts:GetCallerIdentity` and show **"Signed in as `<arn>`"**. The identity
  determines the owner ID (see [Multi-user accounts](#multi-user-accounts)).

AWS API calls go through the Rust SDK (`aws-config` plus the service crates). That way, access-key
users don't need the AWS CLI. Only SSO login needs it, until the device flow lands.

### 2. Detect, then join or deploy

Triple-C looks for existing Triple-C infrastructure in the region in two ways, so it finds an
environment even if one of them is missing:

- the SSM parameters `/triple-c/environments/*`, written last by the environment module, whose
  JSON value (`EnvironmentInfo`, schema 1) lists the environment's ARNs and IDs. The record is
  untrusted input: every field is validated and the value capped at 4 KiB;
- the OpenTofu state objects and `.tflock` locks in the state bucket (D9; admins only, D8). This
  catches a deployment whose record is missing (a failed or interrupted apply) or one in progress.
  A state without a record, a failed apply or a stale lock (over two hours old) is shown as
  **Incomplete** with Repair, Unlock or, without a record, Destroy (D6), instead of being silently
  ignored; a fresh lock is **Deploying** (Follow when it is this machine's run). An empty state
  (what `tofu destroy` leaves behind) counts as no deployment.

The record carries the module version it was deployed from, so an environment deployed by an older
Triple-C shows **"Update available"**.

What the user sees depends on what was found and what they're allowed to do:

| Found | Can deploy (admin tier) | Offered |
|---|---|---|
| Nothing | Yes | **Deploy a new environment** (the default path for a personal account) |
| Nothing | No | Explanation, plus the policy JSON and an admin bundle (D5) to send to whoever owns the account |
| One or more | Yes | **Join** `<name>` (default) or **Deploy a new one** (for example a separate region or cost boundary) |
| One or more | No | **Join** `<name>` |

**Deploy** applies the **OpenTofu module that ships with the app** (`aws/terraform/environment`)
with the caller's own credentials. The app downloads a pinned OpenTofu (checked against its SHA-256
before it is unpacked), creates the state bucket if needed, writes a work folder and starts a
detached `tofu init` + `tofu apply` (D10); progress streams from OpenTofu's JSON log into the
deploy view. An admin can apply the same files without Triple-C from a bundle (D5), and
`tofu destroy` removes everything the module created. The app never creates environment resources
one API call at a time.

**Joining in strict tenancy** (the default) also needs the user's **onboarding**
(`aws/terraform/user-onboard`): their own task and execution roles (with the permissions
boundary) and their own EFS file system (D2), and later, if they use EC2, a dedicated capacity
provider. The app checks whether the caller already has one (their state object, or their task
role):

- **The caller can deploy it** (an environment admin; always true in their own account): Triple-C
  applies the onboarding module for them as part of joining. For the person who just deployed the
  environment, this happens in the same flow, so a single user never sees "onboarding" as a step.
- **The caller can't**: Triple-C produces an onboarding request for whoever runs the environment —
  an admin bundle (D5) with the user's `aws:userid` filled in — and the user's "Join" stays
  pending until their roles exist. The app re-checks whenever the user opens the dialog.

### 3. Permission check

There are three tiers, and the check reports which one the user is in:

| Tier | Can do |
|---|---|
| **Environment admin** | Deploy or update an environment (the OpenTofu modules), onboard users (strict tenancy) |
| **Workspace user** | Create, start, stop, connect to and delete **their own** workspaces in an existing environment |
| **Insufficient** | Lists exactly which actions are missing, and offers the IAM policy JSON to send to an admin |

How the check works:

1. Call `iam:SimulatePrincipalPolicy` for the caller's role or user ARN, against the action list
   for each tier, using the environment's real resource ARNs and the tag conditions a request
   would carry.
2. If the user can't run the simulation (common for locked-down SSO roles), fall back to read-only
   probes (`ecs:DescribeClusters`, `ssm:GetParameter`, `secretsmanager:ListSecrets` filtered by
   tag, `elasticfilesystem:DescribeAccessPoints`). Write actions are then marked **"unverified —
   will be confirmed on first use"**, not "OK".
3. Be honest about the gaps: the simulation does not apply SCPs or session policies (it does
   apply the principal's own permissions boundary — verified 2026-11-14). The first real create is
   the final check, and its error must name the denied action.
4. A probed run never claims more than **Workspace user**: probes see reads, and the admin tier is
   nearly all writes.

The exact policies for each tier ship as files with the app (`aws/policies/*.json`). Settings
links to them so an admin can grant them without reading this doc.

### 4. Hosting options (per project, defaults per environment)

| Option | Choices | Notes |
|---|---|---|
| Launch type | **Fargate** (default), **ECS on EC2** | EC2 choices are limited by the environment's tenancy mode, see below |
| CPU / memory | Fargate sizes (0.25–16 vCPU) or EC2 reservations | The admin can cap these per environment |
| Architecture | x86_64 / ARM64 (Graviton) | The image is already built for `linux/amd64` and `linux/arm64` |
| Storage | **Hybrid** (default), **Ephemeral**, **EFS** | See [Storage](#storage) |
| Ephemeral size | 21–200 GiB (Fargate) | On EC2, the instance volume |
| Fargate Spot | off / on | Cheaper. Can be interrupted with 2 minutes' warning |
| Idle auto-stop | off / N hours with no terminal or agent activity | The admin can set a maximum |
| Repos | git URL + branch + folder name, one per `ProjectPath` | Replaces host paths on AWS |
| Secrets delivery | **Secrets Manager** (default), **Copy at connect** (built in phase 6)

- With `secrets_delivery: CopyAtConnect` the project secret still carries settings, repos, the
  agent token and the gateway sidecar's keys (R1), but no `TRIPLEC_FILES` and no credential
  variable (`TRIPLEC_CREDENTIALS=connect` marks it). After every connect (start, attach,
  background re-attach) the app uploads the files and an env file as root into a per-connect
  directory on tmpfs under `/dev/shm/triple-c/connect/` through the agent, and runs
  `triple-c-apply-connect`: CA install, SSH merge and git credential helper (the entrypoint's own
  functions, shared through `entrypoint-lib.sh`), then retries repos whose clone failed.
- Credentials never enter the agent's environment snapshot, so cron and scheduled jobs never get
  them, and neither does the setup script. Terminals the app opens add them to their own env.
  The first clone of a private repo waits up to 5 minutes for the first connect.
- `~/.ssh` is not linked to EFS under this mode (R6), and a secret that held credentials is
  recreated when the user switches to it (R7).
- The "persist to EFS" opt-in this section once described was **not built**; the UI says
  scheduled jobs don't get credentials while the app is closed.

**Claude Code login works the same as locally (decided).** The project's LLM backend setting
doesn't change on the AWS host:

- **Anthropic login**: `claude login` in a terminal. The OAuth callback reaches the container
  through the auth bridge over the agent tunnel. The credential persists in `~/.claude`, which is
  on EFS in Hybrid and EFS modes, and lost on stop in Ephemeral mode, where the UI says so. The
  shared auth token (`use_shared_auth_token` → `CLAUDE_CODE_OAUTH_TOKEN`) is delivered as a
  Secrets Manager secret instead of an env var.
- **Bedrock**: the same Bedrock options as today (static keys, profile, bearer token). Profiles
  and SSO are delivered as described above, `triple-c-sso-refresh` keeps working, and the
  existing post-start `sync_bedrock_credentials` moves onto the agent. One optional extra,
  **off by default**: "Use this workspace's task role for Bedrock". Under strict tenancy the task
  role is the user's own, so this is safe and needs no credentials in the container. It is an
  extra choice, not a replacement.

### Multi-user accounts

**Owner ID.** `aws:userid` from `GetCallerIdentity`:

- for an IAM user, the user ID;
- for SSO and assumed roles, `AROAxxxx:<session-name>`, where the session name is the SSO
  username.

It is stable per person and available as a policy variable, `${aws:userid}`. Every per-user
resource is created with `triple-c:owner = ${aws:userid}`.

**Control-plane isolation (ABAC).** The workspace-user policy only allows:

- `ecs:RunTask`, with the request tag `triple-c:owner` set to `${aws:userid}`, on the
  environment's cluster and task definition family prefix, plus `iam:PassRole` **only** for the
  environment's execution and task roles;
- `ecs:StopTask`, `ecs:ExecuteCommand`, `ssm:StartSession` where the resource tag
  `triple-c:owner` is `${aws:userid}`. Without this, any user could open a shell in anyone's
  task, so it's the most important rule;
- `secretsmanager:CreateSecret`, `PutSecretValue` and `DeleteSecret` with matching request or
  resource tags, on `triple-c/<env>/*`;
- `elasticfilesystem:CreateAccessPoint` and `DeleteAccessPoint` with a matching tag;
  `kms:Encrypt` and `GenerateDataKey` through Secrets Manager only (`kms:ViaService`);
- read-only `Describe*` / `List*`. The app filters results by owner tag for display. Listing is
  not a secret.

Wildcard `${aws:userid}` conditions on ARNs and tags behave differently across ECS, EFS and
Secrets Manager (`ecs:ResourceTag` vs `aws:ResourceTag`, and support for tag-on-create). **Every
statement in `aws/policies/workspace-user.json` needs an integration test against a real account
before release.**

**Data-plane isolation: tenancy modes.** These are set on the environment by the admin.

| Mode | For | Task role | EC2 options | Isolation |
|---|---|---|---|---|
| **Strict** (**default, v1**) | Everyone, from a solo account to a team | **Per-user** task role, created by the user's onboarding module (self-deployed, or by an admin) | Privileged and Docker allowed **only on a per-user capacity provider** (dedicated EC2 instances, never shared with another owner) | Per-user IAM all the way down. Each user has their own EFS file system (D2), whose policy admits only that user's task role |
| **Standard** (opt-in, later) | Large teams that want fewer IAM roles | Shared, minimal (logs, SSM messages) | **Fargate only**, or EC2 with **no** privileged mode, Docker socket or `NET_ADMIN` | Containers are unprivileged and can't mount NFS, can't read secrets, and can't reach each other over the network |

There is no separate "personal" mode. A solo user on strict tenancy is simply the only onboarded
user, and the account is already safe to share.

Strict tenancy's per-user `iam:PassRole` is scoped by a tag on the role
(`iam:ResourceTag/triple-c:owner = ${aws:userid}`), not by role name, because `aws:userid`
contains a `:`, which role names don't allow. That `iam:PassRole` evaluates `iam:ResourceTag` this
way is verified in the simulator (phase 3); confirm it on the first real `RunTask` (phase 5).

Why EC2 privileged mode needs dedicated capacity: a privileged container, or one with the Docker
socket, has root on its EC2 host, and so access to every other tenant's task on that host
(including their injected secrets in `/proc/*/environ`). In standard tenancy the app
**doesn't offer** Docker access or VPN for EC2 projects. In strict tenancy it offers them only on
the user's dedicated capacity provider.

**Cost visibility.** All resources carry `triple-c:owner`, `triple-c:project` and
`triple-c:environment`. The admin activates them as cost-allocation tags. The app shows an
estimated hourly cost per running workspace (Fargate vCPU and GB-hour rates for the region, from
the Pricing API, cached).

**Quotas.** Fargate vCPU and EFS access-point limits are per account and region. When `RunTask`
fails because of a quota, the app says so and doesn't retry.

### ECS on EC2 vs Fargate

| | Fargate | ECS on EC2 |
|---|---|---|
| Start time | About 30–90 s (image pull every time; SOCI lazy-loading **to evaluate**) | Fast when the image is cached on a warm instance |
| Docker access / privileged / VPN | No | Yes, on dedicated capacity |
| GPU | No | Yes |
| Ops | None | ASG + capacity provider, defined in the environment module. The app only picks a capacity provider |
| Cost when idle | 0 (task stopped) | Instances keep running unless the ASG scales in |
| Max size | 16 vCPU / 120 GB | Instance type |

ECS on EC2 uses `awsvpc` network mode too, so security groups and the no-inbound rule work the same
way.

### Lifecycle and status

- `container_watch.rs` listens to Docker `die` events. For AWS, `AwsHost::events()` polls
  `DescribeTasks` for the owner's running tasks every 15 s while the app is open, and maps
  `stopCode` / `stoppedReason`:
  - `EssentialContainerExited` → Stopped (exited on its own);
  - `SpotInterruption` → Stopped (Spot);
  - `TaskFailedToStart` → Error, with the reason;
  - user-initiated → Stopped.

  EventBridge → SQS for push notifications is a later improvement.
- **Idle auto-stop** runs **inside** the task: the agent tracks activity (terminal I/O and agent
  calls; an open but silent terminal or tunnel is not activity, and the app's background polls
  are marked `X-Triple-C-Background` and never count — ruling R11), the watcher adds running
  `claude` processes, scheduler jobs and unsaved git work, and stops the task by exiting the
  entrypoint with status 75, which the app reads back from `DescribeTasks` to say "stopped after
  being idle". It doesn't depend on the app being open. Because a running scheduler job counts as
  activity, scheduled tasks keep a workspace alive. The admin's maximum lifetime is a hard cap
  either way.
- **Health:** the task definition carries an ECS container health check against the agent, run
  inside the container with the token from its tmpfs file, so the token never leaves the task:
  `curl -sf --max-time 3 -H "Authorization: Bearer $(cat /dev/shm/triple-c/agent-token)"
  http://127.0.0.1:47900/v1/health` (interval 30 s, retries 3, `startPeriod` 120 s). ECS reports it
  as the container's and the task's `healthStatus` in `DescribeTasks`, so the same 15 s poll
  shows *Starting* (`UNKNOWN`), *Ready* (`HEALTHY`) or *Unhealthy* (`UNHEALTHY`) with no
  connection open. Measured 2026-11-14: `HEALTHY` 105 s after `RunTask`. The app still checks the
  agent through its own tunnel before it calls a project connected.
- **A workspace outlives the app.** Nothing about a task depends on the app or its tunnel:
  - Quitting or crashing the app ends only the SSM session. Idle auto-stop and the admin's
    lifetime cap are what stop the task.
  - The next app (the same machine or another) resolves the task and opens a new tunnel.
  - Measured 2026-11-14: a background job started through one tunnel kept writing every 5 s,
    with no gap, after that tunnel's processes were killed with `-9`. A fresh tunnel found it
    still running, and the agent still reported its exec as running.
  - What does **not** survive is an attached exec. As with `docker exec`, the agent sends
    SIGHUP to a tty's process group when its client disconnects. So a terminal tab's `claude`
    ends with the app, and comes back through `claude --resume`, whose transcripts live on the
    workspace's storage. Keeping live terminals across an app restart (a detached pty the agent
    holds for re-attach) is a possible later feature, not v1.
  - If the app dies without closing its tunnels, the plugin child can outlive it. Its session
    then carries no keepalive (that lives in the app) and SSM ends it within its 20-minute idle
    timeout. The host still closes every tunnel on a normal exit.
- Logs: `awslogs` driver to the environment's log group, stream `<owner>/<project>/<task>`. The
  app shows start logs during startup and when a start fails.

### Feature compatibility (AWS host)

| Feature | Fargate | EC2 (shared) | EC2 (dedicated) |
|---|---|---|---|
| Terminals, web terminal, file viewer | Yes (agent) | Yes | Yes |
| Auth bridge (OAuth callbacks) | Yes (agent tunnel) | Yes | Yes |
| Browser view (Playwright) | Yes (agent tunnel) | Yes | Yes |
| Port mappings | Yes, as local forwards through the agent | Yes | Yes |
| Speech-to-text | Yes (local STT, audio over the agent) | Yes | Yes |
| Scheduler, Mission Control | Yes, and they keep running with the app closed | Yes | Yes |
| Marketplace installs | Yes (files copied through the agent) | Yes | Yes |
| LLM gateway (LiteLLM) | Sidecar container in the task definition | Sidecar | Sidecar |
| Ollama / llama.cpp on the user's machine | **No**, unless the URL is reachable from AWS | No | No |
| Claude Code sandbox (bubblewrap) | Off (bubblewrap fails on Fargate, P9) | Weaker nested mode | Full |
| Docker access | No | No | Yes |
| VPN support | No | No | Yes |
| Snapshots | Setup script instead | Setup script | Setup script |

## Fit with existing conventions (CLAUDE.md)

- **Detecting that a container needs recreating.** Locally, `container_needs_recreation()`
  compares `triple-c.*` labels. On AWS, the equivalent is the **task definition revision**: the
  app computes a fingerprint over every setting that shapes the task, stores it as a task
  definition tag, and registers a new revision when it changes. A running task on an old revision
  gets the same "restart to apply" signal. The CLAUDE.md rule "never put a secret in a label"
  applies equally to tags, task definition env vars and `overrides`. Secrets go only through
  `secrets` ARNs.
- **Env vars are visible on EC2.** Injected secrets become container env vars, which anyone with
  `docker inspect` or `/proc` on the instance can read. This is fine for Fargate and dedicated EC2
  capacity, and is exactly why standard tenancy bans privileged containers on shared EC2
  capacity.
- **`/home/claude` stays seed-only** in Hybrid and EFS modes: it is persistent storage, the same
  semantics as the home volume, so the existing "seed at runtime from `/opt` or
  `/usr/local/bin`" rule carries over unchanged.
- **Marketplace sync and Bedrock credential sync** run "after every container start". They move
  onto `ProjectHost::write_file` / `exec`. Marketplace tokens still never leave the host: sync
  sends the same pinned tar.
- **Settings export/import.** `HostConnection` access keys live in the keychain, so they go into
  `ExportedSecrets` under the same carve-out as the other keychain secrets. Importing an AWS
  connection or environment is shown as its own preview warning, alongside the custom-image
  warning, because it changes **where code runs and who is billed**.
- **New IPC commands** (`aws_connect`, `aws_check_permissions`, `aws_list_environments`,
  `aws_deploy_environment`, …) each need a `generate_handler!` entry and an `allow-*` grant in
  `capabilities/default.json`. `build.rs` enforces this.
- **UI.** The AWS connection goes in `components/settings/` beside the existing AWS panel. The
  per-project hosting options go in Project Home's Config tab, not a modal. Login and permission
  check results use the existing `StatusIndicator` (glyph + word).

## Data model

```rust
// AppSettings
pub hosts: Vec<HostConnection>,           // Local is implicit and always present

pub struct HostConnection {
    pub id: String,
    pub name: String,
    pub kind: HostKind,                    // Aws { auth: AwsAuth, default_region: String }
}
pub enum AwsAuth { Profile { name: String }, Keys { keyring_ref: String } }

// Project
pub host_id: Option<String>,               // None = Local (all existing projects)
pub aws_placement: Option<AwsPlacement>,   // { region, environment } — set when host_id is an AWS connection
pub aws: Option<AwsWorkspaceConfig>,

pub struct AwsWorkspaceConfig {
    pub cpu: u32, pub memory_mib: u32, pub arch: Arch,   // X86_64 | Arm64 (Fargate's valid pairs)
    pub storage: StorageMode,              // Ephemeral | Hybrid (default) | Efs (Hybrid + /workspace on EFS)
    pub ephemeral_gib: u32,                // 21-200
    pub spot: bool,
    pub idle_stop_minutes: Option<u32>,
    pub repos: Vec<RepoSource>,            // { url, branch: Option, folder } — parallel to ProjectPath
    pub wip_push_on_stop: bool,            // default true
    pub secrets_delivery: SecretsDelivery, // SecretsManager (default) | CopyAtConnect
    pub setup_script: Option<String>,      // <= 32 KiB, runs as claude after the clone
    pub gateway_sidecar: bool,             // LiteLLM as a second container
    // runtime state (store-owned, not user-edited)
    pub task_arn: Option<String>, pub access_point_id: Option<String>,
    pub task_definition_arn: Option<String>,   // the exact family:revision last run
    pub workspace_access_point_id: Option<String>, // the EFS-mode /workspace access point
}
// EnvironmentInfo (the SSM record, parsed): max_workspace_hours: u32 (0 = no cap).
// Not built: launch_type (Fargate only; EC2 is phase 7).
```

Existing `projects.json` files deserialize unchanged (`host_id` missing → Local).
`Project.container_id` stays Docker-only. AWS uses `aws.task_arn`.

### Phase 5: rulings (2026-11-15)

- **P1** One secret per project (`triple-c/<env>/<owner_key>/<project-id>`) holds everything project-shaped; task definitions carry only constants. Every user has `ecs:Describe*` on `*`, so anything in a task definition is public to the account. `owner_key` replaces `<owner>` (`aws:userid` contains `:`).
- **P2** The agent token rotates on every start; users may read their own secrets so a second machine can attach to a running task. The task role has no Secrets Manager access.
- **P3** Workspace users register their own task definitions (`workspace-user-tasks.json`, owner request tag + the three tag keys); family `triple-c-<env>-<owner_key>-<project-id-12>`. Anyone can register into any `triple-c-*` family, so revisions and tasks are trusted by owner tag and run by exact `family:revision` ARN.
- **P4** Hybrid persists by symlinking `~/.claude`, `~/.claude.json`, `~/.ssh` into the EFS mount (`/mnt/triple-c-home`), not by mounting over `/home/claude`: EFS can't seed from the image, and masking would hide the image's tooling.
- **P5** AWS runs a registry image only; `LocalBuild` maps to the registry image and `Custom` must name a registry reference.
- **P6** Left for phase 6+: EFS mode, Copy-at-connect, setup scripts, cost estimate, port mappings, the LLM gateway sidecar, Docker access, VPN, the admin's maximum lifetime (all but Docker access and VPN were built in phase 6). The UI shows them disabled with the reason.
- **P7** Idle means no authenticated agent activity for N minutes, no `claude` process and no running scheduler job; default off, 5-1440 minutes when on.
- **P8** Spot uses `FARGATE_SPOT`; stop code `SpotInterruption` shows as Stopped with a Spot reason.
- **P9** Bubblewrap fails on Fargate (measured live), so the AWS host forces Claude Code's sandbox off in the generated settings, with a notice.
- **R1** Files travel as one secret key `TRIPLEC_FILES` (JSON of base64), decoded by the entrypoint, then unset.
- **R2** The task definition's `environment` is exactly `TRIPLEC_HOST=aws` and `COLORTERM=truecolor` (no key list).
- **R3/R4** The tunnel registry feeds `has_exited` (the exit source takes an injected project-to-task lookup); the sandbox is forced off in settings (P9).
- **R5** The live test pushes the image to a throwaway private ECR repo and grants the execution role a test-only pull policy, because Fargate can't pull a local image.
- **R6** Repos clone in the background after the agent starts, so the 30 s health window holds; cost: a terminal may open before a repo exists.
- **R7** OpenAI Compatible is allowed on AWS with a warning that the URL must be reachable from AWS; only the local LiteLLM sidecar is unavailable.
- **R8** Removal leaves task-definition revisions ACTIVE for an admin to clear: `ecs:DeregisterTaskDefinition` is only grantable on `*`, so granting it would let any user break others' workspaces. Cost: stale revisions accumulate (free).

### Phase 6: rulings (2026-11-15/16)

Plan decisions:

- **Q1** EFS mode adds a second access point mounted at `/workspace` (root `/triple-c-workspaces/<owner_key>/<project>`, 1000:1000, 0700, on the user's own file system): the home access point's root would otherwise contain it. EFS = Hybrid + `/workspace`; leaving EFS keeps the access point until "Remove AWS resources".
- **Q2** Copy-at-connect keeps settings in the secret and moves credentials (files and every credential variable) to a push after each connect, as above. Why: "nothing stored in AWS" has to hold for keys and tokens.
- **Q3** The setup script is AWS-only, runs as `claude` after the clone with a 30-minute timeout, and reports `running | ok | failed <code>`; the idle stop treats it like a running clone.
- **Q4** The start reports clone and setup progress from one agent-side status read (`triple-c-workspace-status`); a failed clone or setup is a warning, never a failed start. (Amended by R5.)
- **Q5** The cost estimate comes from the Pricing API (`pricing:GetProducts`, `*`-only in IAM), cached a day per region; spot is shown as on-demand with a note; EFS is not estimated; any failure reads "Estimate unavailable".
- **Q6** Port mappings reuse the auth bridge's socat-over-attach tunnel: loopback bind only, TCP only, opened after connect. They reach the container's loopback, so the agent port and (with the sidecar) 4000 are refused.
- **Q7** The LLM gateway is an opt-in `essential: false` sidecar on 127.0.0.1:4000 using the pinned LiteLLM image; its config and keys arrive as `TRIPLEC_GATEWAY_*` payload keys with keys by `os.environ/` reference. Needs the OpenAI-compatible backend and at least 1 vCPU / 2 GiB.
- **Q8** The lifetime cap is an environment setting (`max_workspace_hours`, module v2) enforced inside the workspace by the idle watcher: advisory, since the user has sudo in their task. Exit status 76 identifies it. Server-side enforcement is a follow-up.
- **Q9** One WIP branch per task run, updated in place, so repeated stops don't pile up branches; unchanged trees are skipped.

Execution rulings:

- **R1** Gateway keys stay in the secret even under copy-at-connect: the sidecar starts before any connect. Cost: a provider key in AWS for those users; the UI says so.
- **R2** If the AWS credentials expired before the live run, finish the docs and review and leave the live run for the user. (They were renewed; the live run happened.)
- **R3** Clones go to a scratch folder and are renamed into place atomically; nothing at a final path is ever deleted. Why: on EFS the folder outlives the task and may hold work.
- **R4** The WIP branch is per run and per folder (`triple-c/wip/<run-id>-<folder>`): repos sharing a remote would otherwise overwrite each other's branch.
- **R5** The start ends at "Connecting"; clone and setup are followed by a cancellable background follower, and reconcile also covers Stopped projects holding a task ARN. Why: a start holding the project lock for up to 35 minutes blocks Stop.
- **R6** Copy-at-connect projects don't link `~/.ssh` to EFS, so keys vanish with the task. Cost: SSH keys made inside the container don't persist.
- **R7** Switching to copy-at-connect recreates the project secret (force delete, then create), because a new secret version would keep the old credentials as AWSPREVIOUS. Cost: a short delay on the first start after the switch.
- **R8** The sidecar's master key is random per start, so the local Docker gateway's keychain key never goes to AWS.

## Shipped AWS artefacts

```
aws/
  README.md                        # the policies, the modules, running a bundle by hand, live results
  terraform/                       # OpenTofu modules (hashicorp/aws pinned, .terraform.lock.hcl)
    permissions-boundary/          # the account's triple-c-permissions-boundary policy
    environment/                   # cluster, tasks SG (no ingress), KMS key + alias, log group,
                                   # network (public | private-nat | existing), SSM record (last)
    user-onboard/                  # per user: task + exec roles (bounded), own EFS file system,
                                   # its SG, two mount targets, file-system policy (D2)
  policies/
    workspace-user.json
    environment-admin.json         # the admin tier, part 1 (D7)
    environment-admin-network.json # the admin tier, part 2: the network
    permissions-boundary.json      # every triple-c-* role carries it
```

State lives in `triple-c-tfstate-<account>-<region>` (versioned, SSE-S3, public access blocked,
TLS only, tagged `triple-c:component=state`), one object per deployment (D9).

## Phases

1. **Host abstraction refactor.** `ProjectHost` trait, `DockerHost`, every call site moved. No
   behaviour change, and the existing tests pass.
2. **Agent.** `triple-c-agent` in the image. Optionally use it locally too behind a flag, to
   harden it before AWS depends on it.
3. **AWS connection + permission check.** Settings UI, SDK wiring, `GetCallerIdentity`, the tiered
   simulation and fallback probes, policy files. **Done 2026-11-14**; the policies were verified
   against real IAM (see "Phase 3: policy verification and rulings").
4. **Environment + onboarding modules.** OpenTofu modules for both and the boundary, run by the
   app (detached) or by an admin from a bundle; detection (SSM records + state objects and locks,
   including incomplete, in-progress and outdated ones), join or deploy, self-onboarding or an
   admin request, **strict tenancy**. **Done 2026-11-14**; deployed, joined, updated and destroyed
   for real as a role holding only the admin policies (see "Phase 4: live verification").
5. **Fargate workspaces.** Ephemeral and Hybrid storage, Secrets Manager delivery, git bootstrap,
   start/stop/status, stop guard with WIP push on by default, idle auto-stop, logs. **Done 2026-11-15**; verified live on Fargate:
   cold start about 94-104 s, ECS `HEALTHY` about 135 s, attach to a running task about 2 s, idle
   stop verified (see "Phase 5: rulings" and `aws/README.md` "Phase 5").
6. **EFS mode, Copy-at-connect, setup scripts, cost estimate.** Also port mappings, the LLM
   gateway sidecar and the environment's (advisory) workspace lifetime cap. **Done 2026-11-16**
   (rulings: "Phase 6: rulings"). Verified live in one workspace (Task 10; `aws/README.md`
   "Phase 6: one workspace with every option"): EFS persistence, the setup script, port mapping,
   the sidecar, copy-at-connect and the cost estimate. Not exercised live: the lifetime cap firing
   (unit-tested), a v1 → v2 environment update, a private clone retried after connect.
7. **ECS on EC2.** Per-user capacity providers in the onboarding module, privileged/Docker/VPN on
   dedicated capacity.
8. **Standard tenancy** (opt-in), only if demand appears.

## To verify before or during implementation

- ~~Whether Fargate allows bubblewrap's unprivileged user namespaces, and what Claude Code's
  sandbox does there.~~ **Measured 2026-11-15:** it doesn't. `bwrap --unshare-user` fails with "No
  permissions to create new namespace" (no `CAP_SYS_ADMIN`), so the AWS host keeps Claude Code's
  sandbox off.
- ABAC condition keys for each action: ECS `RunTask` tag-on-create, `ExecuteCommand`, and
  `ssm:StartSession` against ECS targets; EFS `CreateAccessPoint` with request tags; Secrets
  Manager name-prefix plus tag conditions with `${aws:userid}` for SSO sessions.
  **Phase 5 (2026-11-15, `aws/README.md` "Phase 5"):** `RunTask` with the owner's revision and
  roles, `RegisterTaskDefinition` on `task-definition/triple-c-*`, `CreateAccessPoint` with request
  tags and `GetSecretValue` on one's own secret work for a role session; another owner's revision
  or secret is denied. `CreateAccessPoint`'s `TagResource` is authorized against the file system
  (policy fixed). `DeregisterTaskDefinition` supports only `*` with no tag condition, so (ruling R8)
  workspace users don't retire task definitions; removal leaves them for an admin. SSO sessions themselves were not used.
- ~~That `session-manager-plugin` port forwarding to an ECS target is stable for hours-long
  sessions, and how quickly it reconnects.~~ **Measured 2026-11-14** (phase 2b, us-west-2,
  plugin 1.2.896, Fargate 1 vCPU / 2 GB, `host::ssm::live_tests`):
  - Opening the port forward takes 0.6-0.7 s. A one-shot exec through it averages 35 ms, against
    about 1 s for a fresh `execute-command`. A file moves at about 0.75 MB/s each way (4 MiB in
    5.5 s), which is SSM's own throughput.
  - When the plugin is killed, the supervisor reopens the session on the same local port in
    about 0.55 s.
  - **Session Manager's idle timeout applies to port forwards.** A tunnel with no traffic was cut
    at 19.7 minutes ("timed out due to inactivity"), and every connection inside it went with it,
    including a held-open attach (the test's stand-in for an idle terminal tab). The supervisor
    reconnected at once and requests resumed, but open streams do not survive a reconnect. So an
    Up tunnel sends an unauthenticated `GET /v1/health` every 5 minutes. The agent answers `401`
    before it counts activity, so this doesn't defeat idle auto-stop. With it, a 44-minute hold
    with 22 silent minutes had no reconnect at all, and the held-open attach survived to the end
    (without it, the same attach died at the 20-minute cut).
  - Over 60 minutes, no drop happened other than the idle timeout.
- Fargate image-pull time for the sandbox image, and whether SOCI indexes help. **Measured
  2026-11-14:** the 1.1 GB (compressed) amd64 image pulled from public GHCR in 55 s, and the task
  reached RUNNING about 75 s after `RunTask`. **The entrypoint's `chown -R /home/claude` then took
  about 5.5 minutes.** On ECS, `/home/claude` is the image's own copy (no volume), so it copied the
  whole tree up, also costing about 2 GB of ephemeral storage. It now chowns only what isn't
  already `claude`'s, and the agent answers 83 s after `RunTask`, 4 s after the container starts.
  SOCI was not tried.
- ~~Whether the gateway sidecar logs the provider or master key.~~ **Measured 2026-11-16:** neither
  appears in the gateway or workspace log streams after a failed and a successful call; LiteLLM
  logs the provider's error text, in which OpenAI masks the key (first 8 + last 4 characters).
- ~~That a cold start with EFS `/workspace` and the sidecar stays close to phase 5's.~~
  **Measured 2026-11-16:** 115 s (two access points, two images); restart 112 s; sidecar healthy
  126 s after the start began.
- That GHCR pull-through cache works for the public image.
- That the `SIGTERM` deadline on Fargate retirement and Spot leaves enough time for a WIP push.

### Phase 3: policy verification and rulings (2026-11-14)

Rulings made while writing the tier policies, which phase 4 must build on:

- **Environment admins are fenced by a permissions boundary.** They create or change `triple-c-*`
  roles only with the `triple-c-permissions-boundary` managed policy as the boundary, and cannot
  remove, replace or edit it. An account administrator creates the boundary policy once.
- **Destructive admin actions are tag-scoped.** Security groups, EFS file systems and KMS keys are
  created only with `triple-c:component`, and changed or deleted only if they carry it.
- **A probed permission check caps at Workspace user** (above).
- **Strict tenancy needs per-user execution roles**, not only task roles: a workspace user can pass
  only roles tagged with their own owner ID.
- **Environment-level secrets carry `triple-c:component`**; workspace users can never write,
  delete or retag those, and an untagged `triple-c/*` secret could be claimed by any of them.
- **User secrets use the environment KMS key, named by ARN** in `KmsKeyId`; any other key,
  including `aws/secretsmanager`, is explicitly denied.

`aws/README.md` ("Live verification") has the per-statement results, and
`app/src-tauri/src/aws/live_tests.rs` repeats them. What reality changed:

- IAM refuses `${aws:PrincipalArn}` as a Resource, so self-inspection is `user/${aws:username}`
  for IAM users and, for role sessions (no role-name variable exists), SSO permission-set roles
  and `triple-c-*` roles only (ruling R18). Callers on other roles are probed and reported
  Insufficient; an admin can add their roles to `InspectOwnPrincipal`.
- `ecs:ExecuteCommand` is also authorized against the cluster; the owner rule stays on the task.
- Secrets Manager needs `kms:Decrypt` as well as `GenerateDataKey` on a customer key.
- `elasticfilesystem:CreateFileSystem` has no resource type (`Resource: "*"`).
- The session client needs `ssmmessages:OpenDataChannel` on its own session (AWS docs).
- The tier policies must be managed policies (both together exceed a role's inline budget); the
  workspace policy is close to the 6,144-character limit (about 6,110), and a unit test guards it.
- `SimulatePrincipalPolicy` throttles within one check; the app retries adaptively.
- The `aws/secretsmanager` Deny is load-bearing, but not for the reason first given: a bounded task
  role *can* decrypt such a secret, because that key's policy grants the caller directly.

Left for the phases with real ECS/EFS/EC2 resources: `RunTask`'s `aws:TagKeys` (and any
`aws:ecs:*` or propagated tags), `iam:PassRole` with `iam:ResourceTag` on a real run, the owner
tag on a real task for stop/exec/session, session ownership for an SSO-started session, EFS
`CreateAction`/`CreateAccessPoint`/`ClientMount`, and `security-group-rule` authorization.
(Phase 4 has since created security groups and rules for real; see below.)

### Phase 4: live verification (2026-11-14)

`app/src-tauri/src/aws/live_tests.rs` (`environment`) applies the modules for real, in us-west-2,
as a role holding **only** the admin files: state bucket, a `public` environment through the real
detached runner, detection, self-onboarding, an update ("No changes"), destroy; and a
`private-nat` environment. What reality changed (details in `aws/README.md`):

- `s3:CreateBucket` with tags also needs `s3:TagResource`. Its `aws:RequestTag` and
  `s3:ResourceAccount` conditions are evaluated and pass.
- `ec2:CreateSecurityGroup` is authorized against the VPC without the request's tags, so the VPC
  side of the grant is unconditioned (the group still needs the tag).
- Destroying the NAT gateway's address disassociates it after the association is gone; EC2
  authorizes that against the literal `*/*`, which is all the policy grants (`${*}/${*}`).
- `tofu destroy` leaves an empty state object; detection treats it as no deployment (otherwise a
  destroyed environment read as Incomplete for ever).
- Confirmed as designed: EFS sets `aws:CalledViaLast` for mount-target interfaces; the EFS
  file-system policy accepted the just-created task role; conditional `DeleteObject` (`If-Match`)
  on the versioned bucket returns 412 on a stale ETag.

Still to verify: the `existing` network mode against a user's own VPC. Fargate tasks in the
deployed environment were run in phase 5 (`aws/README.md`).

## Connection model

**Decided: C** (user, 2026-11-13). The comparison below is kept for the record.

The options:

- **A. One connection per environment.** Credentials + region + environment, bound together.
- **B. One connection per identity.** Credentials only (a profile or keys, like an AWS CLI
  profile). Each project picks a region and environment.
- **C. Hybrid.** A connection is credentials plus a **default** region. Environments are detected
  per region when needed, and each project records `(connection, region, environment)`.

| | A. Per environment | B. Per identity | C. Hybrid |
|---|---|---|---|
| Mental model | Simplest: "my AWS dev box" | Matches the AWS CLI (a profile is an identity; region is a parameter) | Matches the AWS CLI, with a sensible default |
| Setup for a solo user, one region | One step | One step, plus picking a region for each project | One step |
| Several regions or environments, same login | Duplicate connections with the same credentials. Keys are stored twice; SSO shares a token cache, so it's only clutter | One connection | One connection |
| Permission check | Once per connection, cached | Per environment, run lazily the first time a project uses it | Per environment, run lazily |
| Detection cost | One region | Every region the user might use (`DescribeRegions` + a scan, slow) or a hand-kept list | Default region straight away; other regions only when the user picks one |
| Risk of deploying to the wrong place | Low: the place is in the connection's name | Higher: an easy region-picker mistake leaves an environment in a region nobody looks at | Low: default region preselected, others must be chosen deliberately |
| Moving a project to another region or environment | Re-point the project to another connection | Change a field | Change a field |
| Settings export/import | Each connection is self-contained | Projects reference environments that may not exist on the importing machine | Same as B |
| Data model / UI | Smallest | Largest (environment picker and per-environment status everywhere) | Medium |
| Multiple AWS **accounts** | One connection each | One connection each (credentials define the account) | One connection each |

**Why C:**

- It costs the solo user, one-region case nothing, because the default region carries it.
- It avoids storing the same keys repeatedly the moment someone uses a second region.
- It keeps detection cheap and deploying deliberate.

A is fine as a v1 shortcut if we want the smallest UI. Moving from A to C later is mechanical:
connections that share credentials get merged.

Consequences for implementation:

- Permission check results are cached per `(connection, region, environment)` and re-run if a
  call is denied.
- The project's Config tab shows the region and environment with the default region preselected.
  Choosing another region triggers detection there, and offers join or deploy as in step 2.
- Settings export carries connections. Projects aren't exported (unchanged scope), so a dangling
  `aws_placement` can't arrive through an import.

## Remaining open questions

None. Everything in "To verify" is an implementation-time check, not a product decision.
