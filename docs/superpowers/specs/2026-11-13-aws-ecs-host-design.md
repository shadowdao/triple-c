# Triple-C on AWS (ECS on EC2 / Fargate) — design

Date: 2026-11-13
Status: **draft for review** (user decisions recorded 2026-11-13). The user set the direction (local or AWS, login like the AWS CLI,
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

- the SSM parameters `/triple-c/environments/*`, written by the environment stack, whose JSON
  value lists the environment's ARNs and IDs;
- CloudFormation stacks tagged `triple-c:component=environment`. This catches a stack whose
  parameter was deleted, or one that is half-deployed (`*_IN_PROGRESS`, `*_FAILED`). Those are
  shown with their state and a "Repair or delete" action instead of being silently ignored.

Detection also reads the template version each stack was deployed from, so an environment deployed
by an older Triple-C shows **"Update available"**.

What the user sees depends on what was found and what they're allowed to do:

| Found | Can deploy (stack permissions) | Offered |
|---|---|---|
| Nothing | Yes | **Deploy a new environment** (the default path for a personal account) |
| Nothing | No | Explanation, plus the policy JSON and a pre-filled CloudFormation console link to send to whoever owns the account |
| One or more | Yes | **Join** `<name>` (default) or **Deploy a new one** (for example a separate region or cost boundary) |
| One or more | No | **Join** `<name>` |

**Deploy** uses the **CloudFormation template that ships with the app**
(`aws/triple-c-environment.yaml`). A CloudFormation stack is auditable, an admin can deploy it
without Triple-C, and deleting the stack removes everything. The app never creates environment
resources one API call at a time. Deploy progress streams from the stack events into the same
progress view used for local image pulls.

**Joining in strict tenancy** (the default) also needs the user's **onboarding stack**: their own
task role, EFS access-point policy entry and, if they use EC2, a dedicated capacity provider. The
app checks whether the caller already has one:

- **The caller can deploy it** (always true in their own account): Triple-C deploys
  `triple-c-user-onboard.yaml` for them as part of joining. For the person who just deployed the
  environment, this happens in the same flow, so a single user never sees "onboarding" as a step.
- **The caller can't**: Triple-C produces an onboarding request for whoever runs the environment —
  a pre-filled CloudFormation console link plus the exact parameters — and the user's "Join" stays
  pending until the stack exists. The app re-checks whenever the user opens the dialog.

### 3. Permission check

There are three tiers, and the check reports which one the user is in:

| Tier | Can do |
|---|---|
| **Environment admin** | Deploy or update the environment stack, onboard users (strict tenancy) |
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
3. Be honest about the gaps: the simulation does not apply SCPs, permission boundaries or session
   policies. The first real create is the final check, and its error must name the denied action.

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
| Secrets delivery | **Secrets Manager** (default), **Copy at connect** | See [Secrets and uploads](#secrets-and-uploads) |

### 5. Start, connect, stop

- **Start** calls `ecs:RunTask` (a standalone task, not a service), tags the task with the owner,
  and polls `DescribeTasks` until the task is `RUNNING` and Triple-C's in-container agent (see
  [Connectivity](#connectivity)) reports ready. Progress appears in the sidebar the same way as for
  local projects.
- **Connect**: terminals, file viewer, browser view, auth bridge and port mappings all work through
  the agent tunnel.
- **Closing the app does not stop the task.** That is the point of this feature: the scheduler,
  long agent runs and so on keep going. Reopening the app reconnects to the running tasks.
- **Stop** first checks for unsaved work (see [Storage](#storage)), then calls `ecs:StopTask`.
- **Delete** removes the task definition revisions, the secrets, and (after confirmation) the
  workspace's EFS data.

## Architecture

### Host abstraction

The app currently talks to Docker directly: `bollard` via `docker/client.rs`, about 38 exec call
sites across terminals, file viewer, auth bridge, browser view, migration and marketplace,
`container_watch.rs` on the Docker event stream, and `commit_container_snapshot`. Introduce a
`ProjectHost` trait in `app/src-tauri/src/host/` with two implementations:

```rust
#[async_trait]
trait ProjectHost {
    async fn start(&self, p: &Project) -> Result<HostHandle>;
    async fn stop(&self, h: &HostHandle) -> Result<()>;
    async fn status(&self, h: &HostHandle) -> Result<HostStatus>;
    async fn exec(&self, h: &HostHandle, cmd: ExecSpec) -> Result<ExecStream>;     // pty or piped
    async fn read_file(&self, h: &HostHandle, path: &str) -> Result<Bytes>;
    async fn write_file(&self, h: &HostHandle, path: &str, data: Bytes) -> Result<()>;
    async fn tunnel(&self, h: &HostHandle, port: u16) -> Result<DuplexStream>;      // auth bridge, browser view, ports
    fn events(&self) -> BoxStream<HostEvent>;                                         // replaces container_watch's die events
    fn capabilities(&self) -> HostCaps;                                               // see compatibility matrix
}
```

- `DockerHost` wraps today's code, with no behaviour change.
- `AwsHost` implements the trait with the ECS/EC2/EFS/Secrets Manager SDK calls plus the agent.
- **Phase 1 is a pure refactor**: every exec, file and tunnel call site moves onto the trait, with
  local only. The AWS work then goes behind a stable interface.

`HostCaps` drives the UI. Options a host can't support (VPN, Docker access, snapshots…) are shown
disabled with the reason, instead of failing at start.

### Connectivity

ECS Exec (SSM Session Manager) gives a shell in a Fargate task with no inbound ports, no public IP
and no SSH. Fargate injects the SSM agent, so the image doesn't need it. Two problems with using it
directly:

- Each `execute-command` opens a new SSM session, which takes about 1 second. That is fine for a
  terminal, but too slow for the file viewer's polling, the auth bridge's `/proc/net` polling and
  per-connection tunnels.
- The session protocol is a custom websocket framing that the Rust SDK doesn't implement.

**Design:** a small **in-container agent**, `triple-c-agent`, a static musl Rust binary added to
the image.

- It listens on `127.0.0.1:<port>` inside the task.
- It multiplexes, over one authenticated connection: pty exec, piped exec, file
  read/write/stat/list/watch, TCP tunnels to in-container loopback ports, and liveness/activity
  for idle auto-stop.
- The app reaches it through **one SSM port-forwarding session** (`AWS-StartPortForwardingSession`,
  target `ecs:<cluster>_<taskId>_<runtimeId>`), opened when the project connects and reopened if
  it drops.
- Authentication uses a random per-task token, delivered as a secret at task start. The SSM
  session's IAM checks (`ssm:StartSession` and `ecs:ExecuteCommand`, both scoped by the owner tag)
  are the outer gate; the token is defence in depth.
- v1 runs AWS's `session-manager-plugin` as a child process for the port-forward (it is
  Apache-2.0, so Triple-C can bundle it). A native Rust client for the SSM protocol could replace
  it later.

This one channel replaces every `docker exec … socat` tunnel (auth bridge, browser view, port
mappings) and every exec call. The web terminal and audio bridge work unchanged, because they sit
on top of the terminal and exec layer.

Network: tasks run in **private subnets** under a security group with **no inbound rules at all**.
Outbound access goes through a NAT gateway, or VPC endpoints in restricted environments (ECR, S3,
Logs, SSM, Secrets Manager, EFS, Bedrock). Two tasks in the same environment cannot reach each
other.

### Image

**Same image, same Dockerfile.** No AWS-specific image. Changes:

1. Add `triple-c-agent` to the image.
2. The entrypoint gets an `aws` mode (`TRIPLEC_HOST=aws`) that runs before the existing steps:
   - **Write secrets to files**: each `TRIPLEC_FILE_<name>` env var injected from Secrets Manager
     is written to the path the entrypoint **already reads** (`/tmp/.host-ssh/…`,
     `/tmp/.host-ca/…`, `/tmp/.host-aws/…`). After that, `sync_host_ssh`,
     `install_corporate_ca` and the AWS config merge run unchanged.
   - **Clone repos**: clone each repo into `/workspace/<folder>` if it isn't already there
     (it's already there on EFS after a restart). Git credentials come from the injected git token
     or SSH key.
   - **Skip** the UID/GID remap (there are no bind mounts; `claude` stays UID 1000, which the EFS
     access point matches) and the Docker-socket group setup (no socket).
   - Start `triple-c-agent` as root, outside the `claude` user's reach, and keep the existing
     unkillable keepalive.
3. **Fargate restrictions** are handled by the host, not the image: no `privileged`, and no added
   capabilities except `SYS_PTRACE`. So VPN (`NET_ADMIN`) and Docker access are unavailable. The
   bubblewrap sandbox likely can't create user namespaces on Fargate (**verify**), so the generated
   Claude Code settings use `enableWeakerNestedSandbox` or turn the sandbox off, with a visible
   notice.

**Getting the image into the account:**

- ECR pull-through cache of `ghcr.io/shadowdao/triple-c-sandbox` (GHCR as an upstream needs a
  GitHub token stored in Secrets Manager), or
- the environment stack's ECR repo, filled by an admin or CI with `crane copy`.

Pulling straight from GHCR over NAT also works, and is the default when the environment has no ECR mirror. Image
updates use the existing "new image available" flow: the next start uses the new digest.

### Storage

| Mode | `/workspace` (repos) | `/home/claude` + `.claude` | Survives stop | Speed | Cost |
|---|---|---|---|---|---|
| **Ephemeral** | task storage | task storage | **No** | Fast | Included (beyond 20 GiB is billed) |
| **Hybrid** (default) | task storage | EFS | Settings, sessions, memory, history: yes. Working tree: only what's pushed | Fast builds, fast git | Small EFS footprint |
| **EFS** | EFS | EFS | Everything | git, `node_modules` and builds are noticeably slower over NFS | Grows with repo and dependency size |

- **One EFS file system per environment, one access point per workspace.** Access point root:
  `/triple-c/<owner>/<project-id>`, with POSIX user and group 1000. The access point is created by
  the user and tagged with the owner (see policies).
- **WIP push is on by default** (`wip_push_on_stop: true`, with a serde default per the
  `default_full_permissions` pattern) and can be turned off per project.
- **Ephemeral and Hybrid have a stop guard.** Before stopping, the app asks the agent for
  `git status --porcelain` and any unpushed commits in each repo. If anything would be lost, it
  offers: **Push to a `triple-c/wip/<date>` branch** (the preselected default), **Stop anyway**, or
  **Cancel**. The same check runs before idle auto-stop. If anything is dirty, idle auto-stop
  pushes a WIP branch instead of throwing work away.
- **Fargate task retirement and Spot interruption** send `SIGTERM` with a short deadline (2 minutes
  for Spot). The entrypoint's `SIGTERM` trap runs the same WIP push, best-effort, when the project
  has it enabled.

#### Snapshots

`docker commit` doesn't exist on ECS. On AWS:

- Anything installed outside `/home/claude` (apt packages, for example) is **lost on restart**.
- A per-project **setup script** (stored with the project, run by the entrypoint on every start)
  replaces snapshotting for the AWS host. Its output streams to the start-progress view.
- Later, the app could build a per-workspace image from base image + setup script with CodeBuild
  and push it to ECR. Out of scope for v1.

### Secrets and uploads

Things that are host files or keyring values today: SSH key (`ssh_key_path`), CA cert
(`ca_cert_path`), git token, AWS config for Bedrock, `CLAUDE_CODE_OAUTH_TOKEN` / API keys, custom
env vars marked secret, the agent token.

**Secrets Manager (default)**

- When the user picks an SSH key or cert for an AWS project, the app reads it on the host and
  creates or updates a secret named `triple-c/<env>/<owner>/<project-id>/<name>`. The secret is
  encrypted with the environment's KMS key and tagged `triple-c:owner`, `triple-c:project`,
  `triple-c:environment`.
- The task definition references the secrets in `containerDefinitions[].secrets`, so **the ECS
  task execution role** fetches them at start and injects them as env vars. The entrypoint writes
  them to files as described above.
- **The container's own task role has no Secrets Manager permission.** Code running inside a task
  cannot read another workspace's secrets, even though tasks share a task role in standard
  tenancy.
- Updating a key in the app updates the secret. The next start picks it up, and a running task
  gets a "restart to apply" prompt.
- Secrets Manager's 64 KB per-secret limit covers keys and cert bundles. Anything bigger is
  rejected with a clear message.

**Copy at connect**

- Nothing is stored in AWS. When the app connects, it pushes the files into the running container
  through the agent, the same way Triple-C's file upload works today, writing to tmpfs paths that
  the entrypoint re-syncs from.
- The task can't start a fresh clone of a **private** repo until the app connects, so the clone is
  deferred until the first connect. Scheduled jobs can't use the credentials while the app is
  closed, unless the files were persisted to EFS (an extra opt-in, warned about).
- For users or orgs that don't want keys in AWS at all.

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
| **Strict** (**default, v1**) | Everyone, from a solo account to a team | **Per-user** task role, created by the user's onboarding stack (self-deployed, or by an admin) | Privileged and Docker allowed **only on a per-user capacity provider** (dedicated EC2 instances, never shared with another owner) | Per-user IAM all the way down. EFS file system policy allows each access point only to its owner's task role |
| **Standard** (opt-in, later) | Large teams that want fewer IAM roles | Shared, minimal (logs, SSM messages) | **Fargate only**, or EC2 with **no** privileged mode, Docker socket or `NET_ADMIN` | Containers are unprivileged and can't mount NFS, can't read secrets, and can't reach each other over the network |

There is no separate "personal" mode. A solo user on strict tenancy is simply the only onboarded
user, and the account is already safe to share.

Strict tenancy's per-user `iam:PassRole` is scoped by a tag on the role
(`iam:ResourceTag/triple-c:owner = ${aws:userid}`), not by role name, because `aws:userid`
contains a `:`, which role names don't allow. **Verify** that `iam:PassRole` evaluates
`iam:ResourceTag` as expected before release.

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
| Ops | None | ASG + capacity provider, defined in the environment stack. The app only picks a capacity provider |
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
- **Idle auto-stop** runs **inside** the task: the agent tracks activity (terminal I/O, agent
  calls, running `claude` processes and scheduler jobs) and stops the task by exiting the
  entrypoint. It doesn't depend on the app being open. Because a running scheduler job counts as
  activity, scheduled tasks keep a workspace alive. The admin's maximum lifetime is a hard cap
  either way.
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
| Claude Code sandbox (bubblewrap) | Weaker nested mode or off (**verify**) | Weaker nested mode | Full |
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
    pub launch_type: LaunchType,           // Fargate | Ec2 { capacity_provider }
    pub cpu: u32, pub memory_mib: u32, pub arch: Arch,
    pub storage: StorageMode,              // Ephemeral | Hybrid | Efs
    pub ephemeral_gib: u32,
    pub spot: bool,
    pub idle_stop_minutes: Option<u32>,
    pub repos: Vec<RepoSource>,            // { url, branch, folder } — parallel to ProjectPath
    pub secrets_delivery: SecretsDelivery, // SecretsManager | CopyAtConnect
    pub setup_script: Option<String>,
    pub wip_push_on_stop: bool,
    // runtime state (not user-edited)
    pub task_arn: Option<String>, pub access_point_id: Option<String>,
}
```

Existing `projects.json` files deserialize unchanged (`host_id` missing → Local).
`Project.container_id` stays Docker-only. AWS uses `aws.task_arn`.

## Shipped AWS artefacts

```
aws/
  triple-c-environment.yaml        # CloudFormation: cluster, capacity providers, SG, EFS + policy,
                                   # KMS key, ECR repo / pull-through rule, log group, exec+task roles,
                                   # SSM environment parameter. Params: VPC/subnets (or create),
                                   # tenancy mode, limits, Bedrock-on-task-role.
  triple-c-user-onboard.yaml       # strict tenancy: per-user task role + capacity provider
  policies/
    environment-admin.json
    workspace-user.json
    task-execution-role.json
    task-role-standard.json
```

## Phases

1. **Host abstraction refactor.** `ProjectHost` trait, `DockerHost`, every call site moved. No
   behaviour change, and the existing tests pass.
2. **Agent.** `triple-c-agent` in the image. Optionally use it locally too behind a flag, to
   harden it before AWS depends on it.
3. **AWS connection + permission check.** Settings UI, SDK wiring, `GetCallerIdentity`, the tiered
   simulation and fallback probes, policy files.
4. **Environment + onboarding stacks.** CloudFormation templates for both, detection (SSM
   parameters + tagged stacks, including half-deployed and outdated ones), join or deploy,
   self-onboarding or an admin request, **strict tenancy**.
5. **Fargate workspaces.** Ephemeral and Hybrid storage, Secrets Manager delivery, git bootstrap,
   start/stop/status, stop guard with WIP push on by default, idle auto-stop, logs.
6. **EFS mode, Copy-at-connect, setup scripts, cost estimate.**
7. **ECS on EC2.** Per-user capacity providers in the onboarding stack, privileged/Docker/VPN on
   dedicated capacity.
8. **Standard tenancy** (opt-in), only if demand appears.

## To verify before or during implementation

- Whether Fargate allows bubblewrap's unprivileged user namespaces, and what Claude Code's sandbox
  does there.
- ABAC condition keys for each action: ECS `RunTask` tag-on-create, `ExecuteCommand`, and
  `ssm:StartSession` against ECS targets; EFS `CreateAccessPoint` with request tags; Secrets
  Manager name-prefix plus tag conditions with `${aws:userid}` for SSO sessions.
- That `session-manager-plugin` port forwarding to an ECS target is stable for hours-long
  sessions, and how quickly it reconnects.
- Fargate image-pull time for the sandbox image, and whether SOCI indexes help.
- That GHCR pull-through cache works for the public image.
- That the `SIGTERM` deadline on Fargate retirement and Spot leaves enough time for a WIP push.

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
| Risk of deploying to the wrong place | Low: the place is in the connection's name | Higher: an easy region-picker mistake leaves a stack in a region nobody looks at | Low: default region preselected, others must be chosen deliberately |
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
