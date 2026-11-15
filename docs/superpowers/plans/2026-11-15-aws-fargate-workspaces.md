# AWS Fargate Workspaces Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A project can run on an AWS environment as a Fargate task: start, connect (terminals, files, auth bridge through the agent), stop with a guard that pushes unsaved work, idle auto-stop, logs, and it outlives the app.

**Architecture:** `ProjectHost::Aws` registers a per-project task definition (fingerprint-tagged), keeps every project value in one per-project Secrets Manager secret, ensures a per-project EFS access point for Hybrid storage, runs the task, and opens the phase 2b `PortForward`, whose `AgentEndpoint` becomes the project's `ContainerTarget::Agent`. A tunnel registry in `host/` lets `ContainerTarget::for_project` find it. `ExitSource::Aws` polls `DescribeTasks`. The image's entrypoint gains an `aws` mode (secret files, repo clone, Hybrid persistence, WIP push on SIGTERM, idle watcher).

**Tech Stack:** Rust (aws-sdk-ecs, -secretsmanager, + new aws-sdk-efs, aws-sdk-cloudwatchlogs at the workspace's pinned versions), bash entrypoint, React/TS.

**Spec:** `docs/superpowers/specs/2026-11-13-aws-ecs-host-design.md` (sections Storage, Secrets and uploads, Lifecycle and status, Image, Data model, Fit with existing conventions). Phase 5 scope: "Ephemeral and Hybrid storage, Secrets Manager delivery, git bootstrap, start/stop/status, stop guard with WIP push on by default, idle auto-stop, logs."

## Decisions this plan makes (rulings; the spec doesn't settle them)

- **P1. One secret per project holds everything project-shaped.** `triple-c/<env>/<owner_key>/<project-id>` (SecretString = JSON object). The task definition references keys with `valueFrom: <secret-arn>:<KEY>::`. *Why:* every workspace user has `ecs:Describe*` on `*`, so anything in a task definition's `environment` is readable by every other user in the account — not just credentials but instructions, settings JSON, repo URLs, custom env. The task definition carries only fixed constants (`TRIPLEC_HOST=aws`, `COLORTERM`, `TRIPLEC_SECRET_KEYS`). The spec's `<owner>` in secret names becomes `owner_key` (`aws:userid` contains `:`, which secret names, log streams and EFS paths reject). 64 KB cap enforced with a clear error.
- **P2. The agent token rotates every start** (written into the project secret before `RunTask`) and the user may `secretsmanager:GetSecretValue` their **own** secrets (owner tag), so a second machine can attach to a running task. The task role still has no Secrets Manager permission.
- **P3. Workspace users register task definitions.** New policy file `aws/policies/workspace-user-tasks.json` (the current file has ~34 characters of headroom): `ecs:RegisterTaskDefinition` with request tag owner = `${aws:userid}` and the three-key tag set, `ecs:TagResource` with `ecs:CreateAction=RegisterTaskDefinition`, `ecs:DeregisterTaskDefinition` + `ecs:DeleteTaskDefinitions` on owner-tagged `task-definition/triple-c-*`, and `secretsmanager:GetSecretValue` on owner-tagged `secret:triple-c/*`. Family: `triple-c-<env>-<owner_key>-<project-id-12>`. Whether RegisterTaskDefinition honours resource-level `task-definition/triple-c-*` must be checked live; fallback is `Resource: "*"` with the request-tag condition (recorded in README either way).
- **P4. Hybrid persists by symlink, not by masking `/home/claude`.** EFS (access point root `/triple-c/<owner_key>/<project-id>`, POSIX 1000:1000, created 0700) mounts at `/mnt/triple-c-home`; the entrypoint links `~/.claude`, `~/.claude.json` and `~/.ssh` into it (seeding from the image's copy on first use). *Why:* EFS has no Docker-volume "copy the image's contents on first mount", so mounting over `/home/claude` would hide Claude Code, cargo and uv; this way the image's tooling stays upgradable and only user state persists. The scheduler's state under `~/.claude` comes along.
- **P5. AWS uses a registry image only.** `LocalBuild` maps to the registry image; `Custom` must name a registry reference. Note for release: `:latest` must contain the agent (true once the next release rebuilds the image from its tag).
- **P6. Not in phase 5:** EFS storage mode, Copy-at-connect, setup scripts, cost estimate (phase 6); port mappings, LLM gateway sidecar, Docker access, VPN, the admin's maximum lifetime (no field in the environment record yet). The Config UI shows these disabled with the reason (`HostCaps` idea from the spec). Ollama/llama.cpp are allowed with a warning that the URL must be reachable from AWS.
- **P7. Idle = no authenticated agent activity for N minutes, no `claude` process, no running scheduler job.** Default off; 5–1440 minutes when on.
- **P8. Spot** uses capacity provider `FARGATE_SPOT`; stop code `SpotInterruption` → Stopped with a "Spot" reason.
- **P9. Bubblewrap on Fargate:** verified live in Task 9. Until then the AWS host forces `sandbox_mode_enabled` off in the generated settings with a notice.

## Global Constraints

- Never use `aws_config::defaults().load()`; SDK configs come from `aws::connection::sdk_config(conn, region)` only.
- Every per-user resource is tagged `triple-c:owner=<aws:userid>`, `triple-c:project=<project-id>`, `triple-c:environment=<env>`, and nothing else (the policy's `ForAllValues:StringEquals aws:TagKeys` set).
- Names built only in `aws/tofu_modules.rs` (or a new `aws/workspace_names.rs` it re-exports): family, secret name, access point root, log stream prefix. All use `owner_key`, never `aws:userid`.
- No project data in a task definition's `environment`, tags or `overrides` (P1). Secrets only by `valueFrom` ARN.
- Container name `workspace`; agent port `host::ssm::AGENT_PORT` (47900); task runs with `enableExecuteCommand: true`, `initProcessEnabled: true`, cap add `SYS_PTRACE` only.
- Health check exactly as the spec: `curl -sf --max-time 3 -H "Authorization: Bearer $(cat /dev/shm/triple-c/agent-token)" http://127.0.0.1:47900/v1/health`, interval 30, retries 3, startPeriod 120.
- Fargate CPU/memory pairs are AWS's valid table (256: 512–2048; 512: 1024–4096; 1024: 2048–8192; 2048: 4096–16384 in 1024 steps; 4096: 8192–30720 in 1024 steps; 8192: 16384–61440 in 4096 steps; 16384: 32768–122880 in 8192 steps). Ephemeral 21–200 GiB.
- Boundary tests in `host/mod.rs` still pass: no bollard or Docker lifecycle outside `host/`/`docker/`.
- **Change a policy JSON, its checks and `aws/README.md` together**; every policy ≤ 6,144 characters; re-run the live tests after a policy change.
- New Tauri commands: `#[tauri::command]` + `generate_handler!` + `allow-*` in `capabilities/default.json`.
- New `Project` fields: serde defaults; `wip_push_on_stop` defaults **true** (the `default_full_permissions` pattern); TS types mirror Rust; existing `projects.json` loads unchanged.
- Live AWS: only the scratchpad `awsx` wrapper; spend cap $10 total; every live test cleans up.

## Review Focus

1. A project whose task was stopped outside the app (console, Spot, idle stop) while the app was closed: reopening shows Stopped, not Running, and Start works.
2. Two app instances (two machines) on one project: the second attaches to the running task (P2) instead of starting another; Start never runs a second task for a project with one RUNNING/PENDING.
3. A repo folder that already exists on restart (Hybrid keeps nothing in `/workspace`, but a re-run entrypoint in one task must not re-clone) and a clone that fails (bad URL, private repo without token) — the task still becomes ready and the failure is in the start log.
4. Stop guard when the agent is unreachable (tunnel down): the app says it cannot check, offering Stop anyway / Cancel, never silently stopping.
5. Project secret over 64 KB (large CA bundle + instructions): clear error naming the cause before any AWS write.

---

### Task 1: Project model for the AWS host

**Files:** Modify `app/src-tauri/src/models/project.rs`, `app/src/lib/types.ts`, `app/src-tauri/src/commands/project_commands.rs` (`restore_store_owned_fields`, `Project::new`), store setter in `storage/projects_store.rs`. Test in `models/project.rs` tests.

**Interfaces — Produces:**
- `Project.host_id: Option<String>` (None = Local), `Project.aws_placement: Option<AwsPlacement { region: String, environment: String }>`, `Project.aws: Option<AwsWorkspaceConfig>`.
- `AwsWorkspaceConfig { cpu: u32, memory_mib: u32, arch: Arch /*X86_64|Arm64*/, storage: StorageMode /*Ephemeral|Hybrid (default)*/, ephemeral_gib: u32 /*21*/, spot: bool, idle_stop_minutes: Option<u32>, repos: Vec<RepoSource>, wip_push_on_stop: bool /*default true*/, task_arn: Option<String>, access_point_id: Option<String>, task_definition_arn: Option<String> }`; `RepoSource { url: String, branch: Option<String>, folder: String }`. Defaults: 1024 CPU / 4096 MiB / X86_64 / Hybrid / 21.
- `pub fn validate_aws_workspace(cfg: &AwsWorkspaceConfig) -> Result<(), String>` (CPU/memory table, ephemeral range, idle 5–1440, folder `^[A-Za-z0-9._-]{1,64}$` not `.`/`..`, unique folders, URL is `https://…` or `git@host:path` or `ssh://…`, branch passes `git check-ref-format --branch` rules, ≤ 20 repos).
- `Project::is_aws(&self) -> bool` (`host_id.is_some()`).
- `task_arn`, `access_point_id`, `task_definition_arn` are **store-owned** (restored in `restore_store_owned_fields`); setter `ProjectsStore::set_aws_runtime(id, task_arn, access_point_id, task_definition_arn)`.
- `update_project` refuses changing `host_id`/`aws_placement` while the project is not Stopped, and refuses `host_id` naming a connection that doesn't exist.

- [ ] Tests: old `projects.json` fixture without the fields loads with `host_id == None`; JSON with `aws: {}` gets `wip_push_on_stop == true`; each validation rule has a pass and a fail case (incl. 256/4096 rejected, 1024/2048 accepted, 16384/122880 accepted, ephemeral 20 and 201 rejected, duplicate folder rejected, `..` rejected); store-owned fields survive an `update_project` that tries to change them.
- [ ] Implement; mirror in `types.ts`; `cargo test models::project` and `npx tsc --noEmit` pass.
- [ ] Commit `feat(aws): project model for the AWS host`.

### Task 2: One env builder for both hosts

**Files:** Modify `app/src-tauri/src/docker/container.rs` (lines ~1231–1625 of `create_container`), create `app/src-tauri/src/docker/workspace_env.rs` (or a `pub(crate)` fn in container.rs).

**Interfaces — Produces:** `pub(crate) struct WorkspaceEnvInputs<'a>` (every `create_container` parameter the env block reads: globals, timezone, defaults, instructions, cc settings) and `pub(crate) fn workspace_env(project: &Project, inputs: &WorkspaceEnvInputs) -> Vec<(String, String)>` returning exactly the `KEY=VALUE` pairs `create_container` pushes today **except** `HOST_UID`/`HOST_GID` (host-specific, stay in `create_container`). `create_container` becomes: host pairs + `workspace_env(..)` formatted as `KEY=VALUE`. No behaviour change on Docker.

- [ ] Test: a table of representative projects (Anthropic shared token, Bedrock keys, Bedrock profile, Ollama, OpenAI-compatible, custom env, instructions, cc settings, VPN on) — the env `create_container` would build before the change (capture via a test-only seam that returns the vector instead of calling Docker) equals the new one, order included. Add the seam first, record the vectors, then refactor.
- [ ] Refactor; all existing `docker::container` tests pass unchanged.
- [ ] Commit `refactor(docker): build container env in one host-neutral function`.

### Task 3: Workspace-user task policy (P2, P3)

**Files:** Create `aws/policies/workspace-user-tasks.json`; modify `app/src-tauri/src/aws/policies.rs` (`policy_files(Tier::WorkspaceUser)` lists both files; new checks in `WORKSPACE_USER_CHECKS`; `OWNER_SCOPED`), `aws/README.md`, `app/src/components/settings/hosts/PolicyFiles.tsx` if it hardcodes names (it shouldn't), `aws/live_tests.rs` (workspace-user role gets both files).

**Interfaces — Produces:** statements `RegisterOwnTaskDefinitions`, `TagOwnTaskDefinitionsAtRegister`, `RetireOwnTaskDefinitions`, `ReadOwnSecrets` as in P3.

- [ ] Tests (existing lock-step machinery): every new action has a check; checks allowed for the owner and denied for another owner; RegisterTaskDefinition denied without the owner request tag and with an extra tag key; GetSecretValue denied on a secret tagged `triple-c:component`; both workspace files ≤ 6,144.
- [ ] Implement; README's policy table + "Still open" (resource-level support of RegisterTaskDefinition to verify live).
- [ ] Commit `feat(aws): workspace users register their own task definitions`.

### Task 4: Workspace resources: names, secret, access point, task definition

**Files:** Create `app/src-tauri/src/aws/workspace.rs` (+ `workspace/tests.rs`); modify `aws/mod.rs`, `aws/tofu_modules.rs` (names), `Cargo.toml` (+ `aws-sdk-efs`, `aws-sdk-cloudwatchlogs`, same feature set and version line as the other aws crates; regenerate `Cargo.lock` with `--locked`-compatible update).

**Interfaces — Produces:**
- Names: `task_family(env, owner_key, project_id) -> String` (`triple-c-<env>-<owner_key>-<first 12 of project id sanitised>`, ≤ 255, `[A-Za-z0-9_-]`), `project_secret_name(env, owner_key, project_id)`, `access_point_root(owner_key, project_id)`, `log_stream_prefix(owner_key, project_id)`, `user_file_system_token(env, owner_key)` (= the module's `creation_token`, `triple-c-<env>-<owner_key>`).
- `pub struct WorkspacePlan { family, cpu, memory, arch, storage, ephemeral_gib, image, exec_role_arn, task_role_arn, log_group, region, secret_arn, secret_keys: Vec<String>, file_system_id: Option<String>, access_point_id: Option<String>, tags }` and `pub fn task_definition_request(plan: &WorkspacePlan) -> RegisterTaskDefinitionInput` (Fargate, awsvpc, runtimePlatform, ephemeralStorage, container `workspace` essential, `secrets` = one entry per key with `valueFrom: <arn>:<KEY>::`, `environment` = only `TRIPLEC_HOST=aws`, `COLORTERM=truecolor`, `TRIPLEC_SECRET_KEYS=<comma list>`, health check per Global Constraints, `linuxParameters { initProcessEnabled: true, capabilities.add: [SYS_PTRACE] }`, awslogs to `log_group` with prefix, Hybrid → volume `home` with `efsVolumeConfiguration { fileSystemId, transitEncryption: ENABLED, authorizationConfig { accessPointId, iam: ENABLED } }` mounted at `/mnt/triple-c-home`), and the container `dockerLabels` `triple-c.fingerprint` (a sha256, not project data; tags can't carry it because the policy allows only the three tag keys).
- `pub fn workspace_fingerprint(plan: &WorkspacePlan) -> String` (sha256 over everything in the request except the fingerprint label).
- `pub fn project_secret_payload(env_pairs: &[(String,String)], agent_token: &str, files: &[(String, Vec<u8>)], repos: &[RepoSource], wip: bool, idle: Option<u32>, storage) -> Result<String /*JSON*/, String>` — files as `TRIPLEC_FILE_<NAME>` base64 (SSH key dir entries, CA cert), repos as `TRIPLEC_REPOS` (JSON), `TRIPLEC_AGENT_TOKEN`, `TRIPLEC_WIP_PUSH`, `TRIPLEC_IDLE_STOP_MINUTES`, `TRIPLEC_STORAGE`; error if serialized > 65,536 bytes (Review Focus 5).
- `#[async_trait] pub trait WorkspaceApi` (fake-able, like `DetectApi`): `find_file_system(token) -> Option<(id, arn)>`, `find_access_point(fs_id, project_id, owner) -> Option<String>`, `create_access_point(fs_id, root, tags) -> String`, `put_project_secret(name, kms_key_arn, json, tags) -> arn` (create, or put value if exists), `latest_task_definition(family) -> Option<(arn, fingerprint)>`, `register_task_definition(req) -> arn`. `SdkWorkspaceApi::new(&SdkConfig)`.
- `pub async fn prepare_workspace<W: WorkspaceApi>(api, who: &CallerIdentity, env: &EnvironmentInfo, project: &Project, env_pairs, files, image) -> Result<PreparedWorkspace { task_definition_arn, access_point_id, agent_token, secret_arn }, AwsError>` — agent token = 48 random bytes, URL-safe base64.

- [ ] Tests (fake API): request has no project data in `environment` (assert the exact three keys) and every secret key appears as a `valueFrom`; Ephemeral has no volume; Hybrid mounts the access point with IAM + TLS; Arm64 sets `runtimePlatform.cpuArchitecture=ARM64`; unchanged inputs don't register a new revision (fingerprint match) but a changed CPU does; existing access point is reused; payload over 64 KB fails before any API write; names use `owner_key` and never contain `:`.
- [ ] Implement; `cargo test aws::workspace`.
- [ ] Commit `feat(aws): per-project secret, access point and task definition`.

### Task 5: Run, find, stop and watch tasks

**Files:** Create `app/src-tauri/src/aws/tasks.rs` (+ tests); modify `host/watch.rs` (`ExitSource::Aws`), `container_watch.rs`.

**Interfaces — Produces:**
- `#[async_trait] pub trait TaskApi`: `run_task(RunTaskInput) -> TaskInfo`, `describe(cluster, arns) -> Vec<TaskInfo>`, `list_project_tasks(cluster, family) -> Vec<String>` (ListTasks by family, RUNNING and PENDING), `stop_task(cluster, arn, reason)`, `log_tail(group, stream, limit) -> Vec<String>`.
- `TaskInfo { arn, last_status, desired_status, health: Option<String>, stop_code: Option<String>, stopped_reason: Option<String>, task_definition_arn, runtime_id: Option<String>, exec_agent_running: bool }`.
- `pub fn run_task_request(env: &EnvironmentInfo, task_def_arn, spot: bool, tags) -> RunTaskInput` (cluster, `FARGATE` or capacity provider `FARGATE_SPOT`, awsvpc subnets/SG/`assignPublicIp` from the record, `enableExecuteCommand: true`, the three tags given explicitly).
- `pub async fn find_or_run(api, env, project, task_def_arn, spot) -> Result<TaskInfo, AwsError>` — if `list_project_tasks` finds a RUNNING/PENDING task of the family, return it (Review Focus 2); else run.
- `pub async fn wait_ready(api, cluster, arn, progress) -> Result<TaskInfo, AwsError>` — poll every 5 s up to 6 min until RUNNING and `exec_agent_running` and `runtime_id`; on STOPPED, error with `stopped_reason` plus the last 50 log lines.
- `pub fn classify_stop(info: &TaskInfo) -> StopKind { Exited, Spot, FailedToStart(String), UserInitiated }` per spec's mapping.
- `pub fn is_quota_error(&AwsError) -> bool` — RunTask quota failures surface as such, no retry.
- `ExitSource::Aws` → `deaths()` polls every 15 s the RUNNING AWS projects' task ARNs (grouped per connection+region) and yields targets whose task is STOPPED; `ContainerTarget::has_exited` for an Agent target consults the tunnel registry's task (Task 6) instead of returning false.

- [ ] Tests (fake API): existing RUNNING task is reused, no RunTask; PENDING reused; Spot uses capacity provider strategy and no launch type; STOPPED during wait returns the reason and log lines; quota error message; classify_stop table.
- [ ] Implement; `cargo test aws::tasks host::watch container_watch`.
- [ ] Commit `feat(aws): run, find, stop and watch workspace tasks`.

### Task 6: `ProjectHost::Aws` and the tunnel registry

**Files:** Modify `host/lifecycle.rs`, `host/mod.rs` (drop `#[allow(dead_code)]` on ssm; `ContainerTarget::for_project`), create `host/aws_host.rs` (or `host/tunnels.rs` + lifecycle arm), modify `commands/project_commands.rs` (start/stop/reconcile/remove), `lib.rs` (shutdown closes tunnels; startup reconcile re-attaches).

**Interfaces — Produces:**
- `host::tunnels`: global registry `project_id -> LiveTunnel { task: EcsTask, task_arn, cluster_arn, connection_id, region, forward: PortForward, endpoint: AgentEndpoint }`; `register`, `get_endpoint(project_id) -> Option<AgentEndpoint>`, `task_of(&AgentEndpoint) -> Option<(conn, region, cluster, task_arn)>`, `close(project_id)`, `close_all()` (called from the `Lifecycle` shutdown path within its budget).
- `ContainerTarget::for_project(p)`: AWS project → `tunnels::get_endpoint(&p.id).map(ContainerTarget::Agent)`; Local unchanged.
- `ProjectHost::for_project(p)` → `Aws` when `p.is_aws()`. `ensure_started`: connection → `sdk_config` → `caller_identity` → environment record (single `GetParameter` read, `parse_environment_record`) → check onboarding (task role exists) → build env via Task 2 + files (SSH dir, CA) → `prepare_workspace` → `find_or_run` → `wait_ready` → `PortForward::open` → agent health through the tunnel → register → `set_aws_runtime`. Progress strings: "Reading environment…", "Preparing workspace…", "Starting task…", "Waiting for the task (about 2 minutes)…", "Connecting…". When `find_or_run` returned an existing task, the agent token is read back with `GetSecretValue` (P2).
- `stop(target)`: Aws → `StopTask` with reason "Stopped from Triple-C", close tunnel. `remove`: deregister + delete the family's revisions, delete the project secret (`ForceDeleteWithoutRecovery` false, 7-day window), and delete the access point only when the caller passes `discard_data: true`.
- `reconcile_project_statuses`: an AWS project marked Running → describe its `task_arn`; RUNNING → re-open tunnel in the background (status stays Running; terminals wait for it); otherwise Stopped (Review Focus 1).
- Boundary tests unchanged and passing.

- [ ] Tests: `for_project` returns None for an AWS project with no tunnel and the registered endpoint when present; `ProjectHost::for_project` picks Aws; reconcile marks a STOPPED task's project Stopped (fake TaskApi seam); the existing Docker live test still compiles. Live coverage in Task 9.
- [ ] Implement; full `cargo test --locked`.
- [ ] Commit `feat(aws): start, attach and stop a project on its AWS environment`.

### Task 7: Entrypoint `aws` mode, WIP push, idle stop

**Files:** Modify `container/entrypoint.sh`, `container/Dockerfile` (COPY two scripts), create `container/triple-c-wip-push`, `container/triple-c-idle-watch`; tests where the existing marked-block shell tests live (`docker/image.rs` pattern: `# >>> name` / `# <<< name` blocks run under bash in a temp dir).

**Interfaces — Produces (environment contract from Task 4's payload):**
- When `TRIPLEC_HOST=aws` (before the agent-token block): for each `TRIPLEC_FILE_<NAME>` write base64-decoded content to its mapped path (`SSH_<file>` → `/tmp/.host-ssh/<file>`, `CA_<file>` → `/tmp/.host-ca/<file>`), mode 600, then unset the var; skip `remap_uid_gid` and the Docker-socket step; Hybrid (`TRIPLEC_STORAGE=hybrid`) → `link_persistent_home` (block): for each of `.claude`, `.claude.json`, `.ssh`: if missing under `/mnt/triple-c-home`, move the image's copy there (or create empty), then replace with a symlink; owned by claude.
- `clone_repos` (block): parse `TRIPLEC_REPOS` with `python3 -I`, for each repo whose `/workspace/<folder>` is not a git repo, `git clone [--branch b] url folder` as `claude` with `GIT_TERMINAL_PROMPT=0`, 10-minute timeout each; failures printed (`Triple-C: clone of <folder> failed: …`) and never fatal (Review Focus 3).
- `triple-c-wip-push [--dry-run] [--reason idle|stop|sigterm]`, run as `claude`, for each git repo directly under `/workspace`: if the tree is dirty or `@{u}..HEAD` non-empty (or no upstream and commits exist), build a commit of the full working tree using a temporary `GIT_INDEX_FILE` (`git add -A` into it, `write-tree`, `commit-tree -p HEAD`), never touching the user's index, HEAD or working tree; push it to `refs/heads/triple-c/wip/<UTC yyyymmdd-HHMMSS>`. `--dry-run` prints one JSON line per repo `{"folder","dirty":bool,"unpushed":n,"branch"}` and changes nothing. Exit 0 if all pushes succeeded or nothing to do.
- SIGTERM trap in aws mode: if `TRIPLEC_WIP_PUSH=1`, `timeout 90 triple-c-wip-push --reason sigterm` before `stop_keepalive`.
- `triple-c-idle-watch` (started in aws mode when `TRIPLEC_IDLE_STOP_MINUTES` is set, as root, alongside the agent): every 60 s reads `/v1/activity` with the token file; idle when `now - last_activity_unix ≥ N*60`, `active_execs == 0`, `active_tunnels == 0`, no `pgrep -x claude`, no running scheduler job (`triple-c-scheduler status` reports none). Then WIP push if enabled, then `kill -TERM 1`.

- [ ] Tests (bash, temp dirs): file vars decode to the right paths with mode 600 and are unset; `link_persistent_home` seeds on first run and is idempotent on second; `clone_repos` skips an existing repo and reports a bad URL without failing; `triple-c-wip-push` on a repo with an untracked file, a staged change and an unpushed commit pushes a branch to a local bare remote whose tree contains all three, and leaves `git status --porcelain` and `HEAD` exactly as before; `--dry-run` JSON shape; clean repo → nothing pushed; idle-watch decision as a pure function `is_idle now last execs tunnels claude_running jobs_running minutes` (block test).
- [ ] Implement; `docker build` of the image succeeds locally (amd64).
- [ ] Commit `feat(container): aws mode — secret files, repo clone, Hybrid home, WIP push, idle stop`.

### Task 8: Stop guard and the AWS project UI

**Files:** Modify `commands/project_commands.rs` (new commands), `lib.rs`, `capabilities/default.json`, `app/src/lib/tauri-commands.ts`, `types.ts`; create `app/src/components/projects/home/config/HostSection.tsx`, `app/src/components/projects/StopGuardModal.tsx`; modify `ConfigTab.tsx`, `AddProjectDialog.tsx`, `ProjectHome.tsx`, `ProjectRow.tsx`, `hooks/useProjects.ts`, sections that must disable options on AWS (`RuntimeSection`, `AccessSection`, `WorkspaceSection`).

**Interfaces — Produces:**
- `check_unsaved_work(project_id) -> UnsavedWork { checked: bool, repos: Vec<RepoState{folder, dirty, unpushed, branch}>, error: Option<String> }` — runs `triple-c-wip-push --dry-run` through the agent (`exec_oneshot_as`, user claude); `checked:false` + error when the agent is unreachable (Review Focus 4).
- `stop_project_container(project_id, wip: Option<WipChoice>)` where `WipChoice = Push | Discard` (Local ignores it): `Push` runs `triple-c-wip-push --reason stop` first and aborts the stop with its output if it fails.
- `aws_task_log(project_id, limit) -> Vec<String>` (last lines of the current/last task's stream).
- UI: AddProjectDialog gets a host choice (Local | each AWS connection); AWS → region (default region preselected) and environment (from `detectAwsEnvironments`, only `Ready`/`UpdateAvailable` where the user is Joined; otherwise a line pointing at Settings → Hosts), repos instead of folders. HostSection in Config: CPU/memory (valid pairs only), arch, storage (Ephemeral / Hybrid; EFS disabled "coming later"), ephemeral size, Spot, idle stop, WIP push, repos editor; Ephemeral shows "Claude login and history are lost when the workspace stops". On AWS, Docker access, VPN, port mappings and the gateway backend show disabled with "Not available on AWS (Fargate)". Stop on an AWS project calls `check_unsaved_work` first; if anything is dirty/unpushed or `checked:false`, StopGuardModal offers Push to WIP branch (preselected; absent when `checked:false`), Stop anyway, Cancel. Start failure shows `aws_task_log` lines.
- [ ] Tests (vitest + RTL): modal default is Push; unreachable agent hides Push and says why; AWS project hides/locks Local-only options; AddProjectDialog sends `host_id`/`aws_placement`/`aws`; Config saves a valid CPU/memory pair and refuses an invalid one; Rust tests for the command's `WipChoice` handling with a fake target; capability census passes.
- [ ] Implement; `npm run build`, `npx vitest run`, `cargo test --locked`.
- [ ] Commit `feat(aws): AWS project settings and the stop guard`.

### Task 9: Live verification (needs refreshed credentials)

**Files:** Extend `app/src-tauri/src/aws/live_tests.rs` (ignored), `aws/README.md` ("Live verification"), spec "To verify".

- [ ] Using a deployed throwaway environment (public network mode) and a role holding **only the workspace-user files** (onboarded by the admin role first), the ignored test: creates an AWS project (Hybrid, 1 vCPU/2 GB, one public repo), starts it through `ProjectHost::Aws`, runs `echo` and `id` through the agent target, checks the repo was cloned, writes `~/.claude/marker`, stops, starts again (new task, same family revision), finds the marker, verifies a second `ensure_started` while running reuses the task (no second RunTask), sets idle stop to 5 minutes and observes the task stop by itself, checks `bwrap --unshare-user true` (P9 outcome recorded), then removes the project (secret scheduled for deletion, task definitions deleted, access point deleted) and destroys the environment.
- [ ] Record: RegisterTaskDefinition resource-level support (P3), PassRole-by-tag on RunTask (spec's open item), time to HEALTHY, Spot not exercised unless cheap.
- [ ] Spend estimate before running (≈ 2 task-hours ≈ $0.10 + NAT none); cleanup inventory after.
- [ ] Commit `test(aws): live Fargate workspace lifecycle`.

### Task 10: Docs

**Files:** spec (phase 5 done, rulings P1–P9, Data model updated: `task_definition_arn`, no `launch_type`/`secrets_delivery` yet), `CLAUDE.md` (`aws/` and `host/` paragraphs: tunnel registry, project secret rule, entrypoint aws mode), `aws/README.md`.

- [ ] Commit `docs(aws): phase 5 Fargate workspaces`.
