# AWS Environment + Onboarding (Phase 4, OpenTofu) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** From Settings → Hosts, a user picks a region on an AWS connection and sees the Triple-C environments there, including incomplete and outdated ones. The user can:
- deploy a new environment from the Terraform module that ships with the app;
- update or repair an existing environment;
- join an environment. Joining onboards the user automatically if they're allowed to. Otherwise it produces an exact onboarding bundle for an admin.

Strict tenancy is the only mode.

**Architecture:**
- **Modules.** Three Terraform modules (HCL, OpenTofu-compatible) ship in `aws/terraform/`: the environment, per-user onboarding, and the permissions boundary. They are compiled into the app with `include_str!`.
- **Running OpenTofu.** The app downloads a pinned OpenTofu release on first use and checks its SHA-256. It materializes a module into a working directory with a generated S3 backend and `terraform.tfvars.json`, then runs `tofu init` and `tofu apply -json`. The apply runs **detached**, so it survives the app closing. The app follows its log file and turns the JSON lines into progress events.
- **State.** State lives in one bucket per account and region, `triple-c-tfstate-<account>-<region>`. The bucket is versioned and encrypted, blocks public access, and uses S3-native locking. The app creates it before the first deploy.
- **Detection.** The app merges the SSM environment records with the state objects and locks in that bucket.
- **Commands and UI.** The Tauri commands are in `commands/environment_commands.rs`. The UI is an Environments panel on each AWS connection.
- **Proof.** Static HCL tests keep the modules and the admin policies in step. An ignored live test deploys everything as a role that holds *only* the admin policies.

**Tech Stack:**
- Rust and Tauri v2, with the existing `aws-sdk-ssm`, `aws-sdk-iam`, `sha2`, `tar` and `reqwest`, if present (otherwise the HTTP client the app already uses).
- **New dependencies:**
  - `aws-sdk-s3`, with `default-features = false` and the same features as the other SDK crates.
  - `zip` (deflate only), for the Windows release.
  - `flate2`, if it is not already a direct dependency.
  - `hcl-rs` as a **dev-dependency**, for the static module tests.
- OpenTofu itself is downloaded at runtime and is never bundled. The version is pinned in source (`TOFU_VERSION`), along with one SHA-256 per platform taken from that release's `SHA256SUMS`. The AWS provider is pinned in `versions.tf`, and the module's committed `.terraform.lock.hcl` carries hashes for every shipped platform.
- React, TypeScript and vitest.

**Spec:** `docs/superpowers/specs/2026-11-13-aws-ecs-host-design.md`. Relevant sections:
- "User flow" §2;
- "Connectivity" (network);
- "Storage";
- "Multi-user accounts" (strict tenancy);
- "Shipped AWS artefacts";
- "Phase 3: policy verification and rulings".

**Branch:** `feat/aws-environments`. A superseded CloudFormation start is parked in `git stash` ("phase4 task1 CFN WIP"). Drop that stash once Task 1 lands.

## Decisions (user, 2026-11-14)

The user chose:
- **Terraform over CloudFormation;**
- the **app runs it** itself;
- **OpenTofu**, which is MPL-2.0 and safe for the app to download and run;
- **state in an S3 bucket** in the account.

The plan adds the decisions below. Task 8 writes all of them into the spec, replacing the CloudFormation text.

- **D2. Strict tenancy gives each user their own EFS file system**, created by their onboarding module.
  - Why: EFS can't bind an access point to its owner's task role. `ClientMount` sees the access point's ARN, not its tags, and access point IDs are random.
  - The file system policy allows only that user's task role. An empty file system costs nothing.
  - The environment keeps the shared pieces: cluster, security group, KMS key, log group and network.
- **D3. No ECR in v1.** Tasks pull `ghcr.io/shadowdao/triple-c-sandbox` straight from GHCR. The spec allows this ("the default when the environment has no ECR mirror").
- **D4. Network modes** (the `network_mode` variable):
  - `public` (**default**): a new VPC with two public subnets. Tasks get a public IP, and the security group has **no inbound rules**. There is no NAT. It costs $0.
  - `private-nat`: a new VPC with two public and two private subnets and one NAT gateway. The UI says this costs about $33/month.
  - `existing`: the user supplies `vpc_id`, `subnet_ids` (exactly 2, in different AZs) and `assign_public_ip`.
- **D5. An admin request is a folder bundle.** The app writes it into a folder the user picks through a Rust-side folder dialog. It contains:
  - the module's files;
  - `backend.hcl`;
  - `terraform.tfvars.json`;
  - a `README.txt` with the exact commands: `tofu init -backend-config=backend.hcl` followed by `tofu apply`. Terraform 1.10+ works too.
- **D6. The app destroys only incomplete deployments.** That means state exists but there's no environment record, or the last apply failed. A working environment is destroyed by an admin running `tofu destroy` from a bundle, which is the irreversible case.
- **D7. The admin tier spans several policy files.** It is `environment-admin.json` and `environment-admin-network.json`, plus a third file if the live test needs one. All of them are required for the tier. `aws_policy_document` returns every file for a tier.
- **D8. CloudFormation leaves the policies**, so the `cloudformation:*` actions, checks and probes are removed. The workspace-user policy loses `ReadStacks` and gains nothing. Detection for workspace users reads only:
  - `ssm:GetParameter*` on `/triple-c/*`;
  - `iam:GetRole` on `role/triple-c-*`.

  Bucket reads (the state objects and locks) are admin-only. A workspace user sees only environments that have a record.
- **D9. One state key per deployment**, inside the per-account, per-region bucket:
  - `environments/<env>/terraform.tfstate`;
  - `users/<env>/<ownerkey>/terraform.tfstate`;
  - `boundary/terraform.tfstate`.

  The lock is the matching `.tflock` object (S3-native locking, `use_lockfile = true`). A held lock is a deploy in progress. Its JSON (`Who`, `Created`, `Operation`) is shown.
- **D10. Applies are detached.** `tofu apply` runs in its own process group, with stdout and stderr going to `<workdir>/apply.log` and its PID in `<workdir>/apply.pid`. Closing the app doesn't interrupt it. On the next run, the app sees the live PID or the held lock and reattaches by following the log. The app sends an interrupt only if the user presses Cancel. OpenTofu then stops gracefully and saves state.

## Global Constraints

- **Names.** These are pure functions in `aws/tofu_modules.rs`, and nothing else builds these strings.
  - **Environment name:** `^[a-z][a-z0-9-]{0,19}$`, not ending in `-`, with no `--`.
  - **Parameter:** `/triple-c/environments/<env>`.
  - **Owner key:** the first 12 hex characters of `sha256(aws:userid)`. A `UserId` contains `:`, which IAM role names reject.
  - **Per-user roles:** `triple-c-<env>-<ownerkey>-task` and `triple-c-<env>-<ownerkey>-exec`. The longest is 47 characters, under the 64-character limit.
  - **Boundary policy:** `triple-c-permissions-boundary`, with no path.
  - **State bucket:** `triple-c-tfstate-<account>-<region>`. The longest region gives 46 characters, under the 63-character limit.
- **Tags.**
  - Each module's provider block sets `default_tags`:
    - environment: `triple-c:component=environment`, `triple-c:environment=<env>`, `triple-c:template-version=<n>`;
    - onboarding: `triple-c:component=user-onboarding`, `triple-c:environment`, `triple-c:owner=<owner_id>`, `triple-c:template-version`;
    - boundary: `triple-c:component=boundary`.
  - No resource overrides `triple-c:component`.
  - The state bucket is tagged `triple-c:component=state`.
  - Workspace users never set `triple-c:component`; phase 3 relies on that.
- **IAM roles.** Every role a module creates has `permissions_boundary` set to `arn:<partition>:iam::<account>:policy/triple-c-permissions-boundary`. It trusts `ecs-tasks.amazonaws.com`, with `aws:SourceAccount` set to the account.
- **No inbound access.** The environment security group has **no** ingress, inline or as a separate `aws_vpc_security_group_ingress_rule` or `aws_security_group_rule`.
- **KMS key.** The key policy delegates to IAM (`arn:aws:iam::<account>:root`, `kms:*`). Rotation is on and `deletion_window_in_days = 7`.
- **The environment record** is an SSM `String` parameter holding JSON, `EnvironmentInfo` schema 1 (fields listed in Task 1). It is written by the module with `jsonencode`. It is **untrusted input**: the app validates every field and caps the value at 4 KiB.
- **Versions.** `ENVIRONMENT_MODULE_VERSION: u32 = 1` and `ONBOARD_MODULE_VERSION: u32 = 1`. Each appears in the Rust constant, the module's `locals.template_version` and the `template-version` default tag, and a test checks they agree.
- **The OpenTofu child's environment.**
  - Every inherited `AWS_*` variable is removed, along with `TF_*` and `TOFU_*` except the ones the app sets.
  - A profile connection gets `AWS_PROFILE=<name>`.
  - A keys connection gets `AWS_ACCESS_KEY_ID` / `AWS_SECRET_ACCESS_KEY` / `AWS_SESSION_TOKEN` **in the child's environment only**. They never go in argv, files or logs.
  - The app sets `TF_PLUGIN_CACHE_DIR`, `TF_IN_AUTOMATION=1`, `TF_INPUT=0` and `CHECKPOINT_DISABLE=1`.
  - The provider block sets `region` explicitly. No `endpoints` override is allowed in any module.
- **Downloads.** The OpenTofu archive is verified against the pinned SHA-256 *before* extraction. On a mismatch the app deletes it and returns an error; it never runs an unverified binary. The provider is pinned by `.terraform.lock.hcl`, so `tofu init` verifies it.
- **Commands.** Every new command needs `#[tauri::command]`, a `generate_handler!` entry and an `allow-*` grant in `capabilities/default.json`. `src/test/capabilities.test.ts` must pass.
- **Policies.** When a policy JSON changes, its checks in `policies.rs` and `aws/README.md` change in the same commit. Every policy fits in 6,144 characters with whitespace removed.
- **Live AWS testing.** The user caps spending at $10, and every resource is cleaned up. Use the scratchpad `awsx` wrapper, never the container's own `AWS_*` environment.

## Review Focus

1. **An apply that was cut off.** State and the lock are present, with no live process and no record. Expected:
   - The environment shows as "Incomplete — last change was interrupted".
   - It offers Repair (re-apply) and Destroy.
   - It offers Unlock only when the lock is more than 2 hours old and isn't held by a live local process.

   Test: `interrupted_deployments_get_the_right_actions` (Task 5).
2. **A record and its state disagree.**
   - A record with no state (deployed by hand elsewhere, or with the state deleted) shows as "Managed outside this account's Triple-C state". It is joinable, but has no Update or Destroy.
   - State with no record is incomplete, as in item 1.

   Test: `a_record_without_state_and_state_without_a_record_are_both_listed` (Task 5).
3. **A hostile or garbled environment record**: bad JSON, 64 KiB, an ARN in another account or region, or a name that doesn't match its path. It is reported as "Unreadable environment record" and never used to build a request. Test: `untrusted_environment_records_are_rejected` (Task 5).
4. **The app closes mid-apply.** OpenTofu carries on. On the next detection, the deployment shows as "Deploying", and its progress replays from `apply.log` once, without starting a second apply. Test: `following_an_existing_log_reports_each_event_once` (Task 4).
5. **A tampered or truncated OpenTofu download.** Expected: deleted, an error, never executed, and a retry downloads again. Test: `a_download_with_the_wrong_hash_is_deleted_and_refused` (Task 4).

---

### Task 1: Environment module and `aws/tofu_modules.rs`

**Files:**
- Create: `aws/terraform/environment/{versions.tf,variables.tf,main.tf,network.tf,outputs.tf,.terraform.lock.hcl}`
- Create: `app/src-tauri/src/aws/tofu_modules.rs`, with tests in `aws/tofu_modules/tests.rs`
- Modify: `app/src-tauri/src/aws/mod.rs`
- Modify: `app/src-tauri/Cargo.toml`, adding `hcl-rs` under `[dev-dependencies]`

**Interfaces:**
- Produces:
  ```rust
  pub const ENVIRONMENT_MODULE_VERSION: u32 = 1;
  pub const ONBOARD_MODULE_VERSION: u32 = 1;
  pub const BOUNDARY_POLICY_NAME: &str = "triple-c-permissions-boundary"; // = policies::PERMISSIONS_BOUNDARY_NAME
  #[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Hash)] #[serde(rename_all = "snake_case")]
  pub enum ModuleKind { Environment, UserOnboard, PermissionsBoundary }
  pub fn module_files(kind: ModuleKind) -> &'static [(&'static str, &'static str)]; // (relative file name, contents), include_str!, incl. .terraform.lock.hcl
  pub fn validate_environment_name(name: &str) -> Result<String, String>;
  pub fn environment_parameter_name(env: &str) -> String;
  pub fn owner_key(user_id: &str) -> String;
  pub fn user_role_names(env: &str, owner_key: &str) -> (String, String); // (task, exec)
  pub fn state_bucket_name(account: &str, region: &str) -> String;
  pub fn state_key(kind: ModuleKind, env: Option<&str>, owner_key: Option<&str>) -> String; // D9
  #[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)] #[serde(tag = "mode", rename_all = "kebab-case")]
  pub enum NetworkChoice { Public, PrivateNat, Existing { vpc_id: String, subnet_ids: Vec<String>, assign_public_ip: bool } }
  pub fn environment_vars(env: &str, region: &str, network: &NetworkChoice) -> Result<serde_json::Value, String>; // tfvars JSON; validates vpc-/subnet- ids, exactly 2 subnets
  ```
- Variables: `environment_name`, `region`, `network_mode`, `vpc_id` (default `""`), `subnet_ids` (default `[]`), `assign_public_ip` (default `false`) and `log_retention_days` (default 30). Each has a `validation` block that matches the Rust rules.
- Resources:
  - the ECS cluster `triple-c-<env>`, with `aws_ecs_cluster_capacity_providers` for FARGATE and FARGATE_SPOT;
  - the security group `triple-c-<env>-tasks`, with egress set to all and no ingress;
  - the KMS key and the alias `alias/triple-c-<env>`;
  - the log group `/triple-c/<env>`;
  - the network resources, according to `network_mode` (`count` or `for_each`);
  - the SSM parameter.
- The record is the SSM parameter value, `jsonencode({...})` with these fields: `schema` (1), `name`, `template_version`, `cluster_arn`, `security_group_id`, `subnet_ids` (exactly 2), `assign_public_ip`, `kms_key_arn`, `log_group`, `network_mode`, and `tenancy` (always `"strict"`). Task 2's onboarding module reads it, and Task 5 parses it.
- `versions.tf` pins `required_version >= 1.10` and `hashicorp/aws` to an exact `6.x` version. Choose the current one, and write it in the report. It also declares an **empty** `backend "s3" {}`, which the app fills in with `-backend-config`.

- [ ] **Step 1: Install OpenTofu locally for development** (sudo is available), using the same pinned version Task 4 will use. Choose the latest stable 1.x release and record it in the report.
- [ ] **Step 2: Write the failing tests** in `tofu_modules/tests.rs`. Parse the module with `hcl-rs`.
  - `environment_names_are_validated`: accepts `dev` and `team-a1`. Rejects `""`, `Dev`, `1dev`, `dev-`, `a--b`, a 21-character name and `dev/x`.
  - `owner_keys_are_stable_and_role_safe`: the key is 12 characters in `[0-9a-f]` and stable. Different user IDs give different keys. Both role names are at most 64 characters for a 20-character environment name.
  - `state_bucket_and_keys_follow_d9`.
  - `the_module_versions_agree`: `locals.template_version` equals the constant, and `default_tags` carries it.
  - `default_tags_mark_the_environment_and_nothing_overrides_component`.
  - `the_security_group_has_no_ingress`: there are no `ingress` blocks, and no ingress-rule resources.
  - `no_provider_endpoint_overrides`.
  - `the_record_has_exactly_the_schema_fields`: the `jsonencode` object's keys equal the field list above.
  - `environment_vars_cover_every_network_mode`: `existing` with 1 or 3 subnets, or `vpc-;`, is an error.
- [ ] **Step 3: Run** `cargo test aws::tofu_modules`. Expected: FAIL.
- [ ] **Step 4: Write the module and implement the module.**
  - Generate `.terraform.lock.hcl` with `tofu providers lock -platform=linux_amd64 -platform=linux_arm64 -platform=darwin_amd64 -platform=darwin_arm64 -platform=windows_amd64`.
  - Run `tofu fmt -check` and `tofu init -backend=false && tofu validate` in the module directory.
  - Don't commit `.terraform/`. Add it to `.gitignore`.
- [ ] **Step 5: Run** the tests and `tofu validate`. Expected: PASS and "Success!".
- [ ] **Step 6: Commit** with the message `feat(aws): environment Terraform module and naming`.

### Task 2: Onboarding and boundary modules

**Files:**
- Create: `aws/terraform/user-onboard/*` and `aws/terraform/permissions-boundary/*`, with the same file set as Task 1
- Modify: `aws/tofu_modules.rs`, `tofu_modules/tests.rs`

**Interfaces:**
- Produces:
  ```rust
  pub fn onboarding_vars(env: &str, region: &str, owner_id: &str) -> serde_json::Value; // environment_name, region, owner_id, owner_key, bedrock_on_task_role=false
  pub fn boundary_vars(region: &str) -> serde_json::Value;                              // region, policy_json = policies::permissions_boundary_json()
  ```
- **Onboarding module.** It reads the environment through `data "aws_ssm_parameter"` and `jsondecode`, and creates:
  - **The task role** `triple-c-<env>-<key>-task`. It allows:
    - `ssmmessages:CreateControlChannel/CreateDataChannel/OpenControlChannel/OpenDataChannel` on `*`;
    - `logs:CreateLogStream/PutLogEvents` on the environment's log group;
    - `elasticfilesystem:ClientMount/ClientWrite` on *this user's* file system;
    - Bedrock invoke, only when `bedrock_on_task_role` is set.
  - **The execution role** `triple-c-<env>-<key>-exec`. It allows:
    - `secretsmanager:GetSecretValue` on `triple-c/*`, where `aws:ResourceTag/triple-c:owner` equals `var.owner_id`;
    - `kms:Decrypt` on the environment key, via `secretsmanager.<region>.amazonaws.com`;
    - the same log permissions as the task role.
  - **The user's EFS file system**: encrypted, `generalPurpose`, elastic throughput, with a mount target in each of the environment's two subnets.
  - **The file system policy:**
    - Deny when `aws:SecureTransport=false`.
    - Deny when `elasticfilesystem:AccessPointArn` is null.
    - Allow `ClientMount` and `ClientWrite` to the task role's ARN only.
  - **The mount-target security group** `triple-c-<env>-<key>-efs`. Its only ingress is TCP 2049 from the environment security group. The environment group itself never gains ingress.
- **Boundary module.** One `aws_iam_policy` named `triple-c-permissions-boundary`, whose `policy` is `var.policy_json`. The app passes the shipped JSON, so there is no second copy to drift.

- [ ] **Step 1: Write the failing tests:**
  - `every_onboarding_role_carries_the_boundary_and_owner_tag`.
  - `role_trust_is_ecs_tasks_in_this_account_only`.
  - `the_users_file_system_admits_only_their_task_role_through_an_access_point`.
  - `the_environment_security_group_gains_no_ingress`: the only ingress is the `-efs` group's port 2049 rule, sourced from the record's `security_group_id`.
  - `exec_role_reads_only_its_owners_secrets`.
  - `boundary_vars_carry_the_shipped_boundary`: `policy_json` equals `permissions-boundary.json`.
  - `every_onboarding_role_stays_inside_the_boundary`: **write this in Task 3**, using the shared evaluator.
- [ ] **Step 2: Run** `cargo test aws::tofu_modules`. Expected: FAIL.
- [ ] **Step 3: Write both modules**, plus their lock files, and run `tofu fmt`, `tofu init -backend=false` and `tofu validate`.
- [ ] **Step 4: Run** the tests. Expected: PASS.
- [ ] **Step 5: Commit** with the message `feat(aws): onboarding and permissions-boundary Terraform modules`.

### Task 3: The admin policies cover the modules; CloudFormation leaves the policies

**Files:**
- Create: `aws/policies/environment-admin-network.json`, plus a third admin file only if the size limit forces it
- Modify: `aws/policies/environment-admin.json`, `aws/policies/workspace-user.json` (remove `ReadStacks`)
- Modify: `app/src-tauri/src/aws/policies.rs`, `permissions.rs`, `permissions/tests.rs` (drop the CFN checks and probe, and evaluate every file of a tier together)
- Modify: `commands/host_commands.rs` (`aws_policy_document -> Vec<PolicyFile>`), `app/src/lib/{types,tauri-commands}.ts`, `components/settings/HostsSettings.tsx` (show and copy each file)
- Modify: `app/src-tauri/Cargo.toml`: remove `aws-sdk-cloudformation` if nothing else uses it
- Modify: `aws/README.md`
- Move: the test-only IAM evaluator from `policies.rs`'s `mod tests` to `aws/policy_eval.rs` (`#[cfg(test)]`). Its behaviour doesn't change.

**Interfaces:**
- Produces:
  ```rust
  pub fn policy_files(tier: Tier) -> &'static [(&'static str, &'static str)]; // replaces policy_json
  #[tauri::command] pub fn aws_policy_document(tier: Tier) -> Vec<PolicyFile>; // PolicyFile { name: String, json: String }
  ```
- `tofu_modules/tests.rs` gets a table mapping each **Terraform resource type** used by the modules to the IAM actions the AWS provider calls to create, read (refresh), tag, update and delete it. For example, `aws_kms_key` maps to `kms:CreateKey`, `TagResource`, `DescribeKey`, `GetKeyPolicy`, `GetKeyRotationStatus`, `ListResourceTags`, `EnableKeyRotation`, `PutKeyPolicy` and `ScheduleKeyDeletion`.
  - The table is the hypothesis. Task 8's live run is the proof, and any action it shows missing is added to the table, the policy, the check table and the README together.
- **State bucket permissions (admin):**
  - `s3:CreateBucket` with `aws:RequestTag/triple-c:component`;
  - `s3:PutBucketVersioning`, `PutEncryptionConfiguration`, `PutBucketPublicAccessBlock`, `PutBucketTagging`, `PutBucketPolicy`, `GetBucket*` and `ListBucket` on `arn:aws:s3:::triple-c-tfstate-*`;
  - `s3:GetObject`, `PutObject` and `DeleteObject` on `arn:aws:s3:::triple-c-tfstate-*/*`.
- **Other admin additions:**
  - `iam:GetPolicy` on the boundary ARN, which detection uses;
  - the `iam:` read actions the provider uses on `role/triple-c-*`;
  - the KMS, EFS, logs and SSM reads and tagging.
- **Network creation** goes in `environment-admin-network.json`:
  - creates are tag-conditioned on `aws:RequestTag/triple-c:component`;
  - mutations and deletes are conditioned on `aws:ResourceTag/triple-c:component`;
  - `ec2:CreateTags` is allowed only under `ec2:CreateAction` for those creates.

- [ ] **Step 1: Write the failing tests:**
  - `the_admin_tier_can_apply_every_module_resource`: every action mapped for every resource in the environment and onboarding modules is allowed by the admin tier's files. The test uses the `triple-c:component` tag and the names from Task 1.
  - `every_onboarding_role_stays_inside_the_boundary`.
  - `the_admin_still_cannot_touch_untagged_resources`: `ec2:DeleteVpc`, `DeleteSubnet`, `DeleteNatGateway` and `s3:DeleteBucket` on a resource without `triple-c:component`, or outside `triple-c-tfstate-*`, are denied.
  - `no_policy_mentions_cloudformation`.
  - The existing size and check-drift tests cover the new files.
- [ ] **Step 2: Run** `cargo test aws::`. Expected: FAIL.
- [ ] **Step 3: Implement it.**
- [ ] **Step 4: Run** `cargo test aws::`, then `npx vitest run src/components/settings`. Expected: PASS.
- [ ] **Step 5: Commit** with the message `feat(aws): admin policies cover the Terraform modules; drop CloudFormation`.

### Task 4: The OpenTofu runner (`aws/tofu.rs`)

**Files:**
- Create: `app/src-tauri/src/aws/tofu.rs`, with tests in `aws/tofu/tests.rs`
- Modify: `Cargo.toml` (`zip`, and `flate2` if needed)

**Interfaces:**
- Produces:
  ```rust
  pub const TOFU_VERSION: &str = "<same as Task 1>";
  pub fn release_asset(os: &str, arch: &str) -> Option<(String /*file name*/, &'static str /*sha256*/)>; // linux/darwin amd64+arm64 (.tar.gz), windows amd64 (.zip)
  pub async fn ensure_tofu(root: &Path) -> Result<PathBuf, AwsError>;   // <root>/tofu/<ver>/tofu[.exe]; downloads from github.com/opentofu/opentofu/releases/download/v<ver>/<file>, verifies, extracts, chmod 755; reuses an existing verified binary (re-hash on each call, cheap)
  pub fn verify_and_extract(archive: &Path, expected_sha256: &str, dest: &Path) -> Result<PathBuf, AwsError>; // deletes archive on mismatch
  pub struct Workdir { pub dir: PathBuf }
  pub fn materialize(root: &Path, kind: ModuleKind, state_key: &str, bucket: &str, region: &str, vars: &serde_json::Value) -> Result<Workdir, AwsError>; // <root>/work/<bucket>/<state_key without .tfstate>/; writes module_files, backend.hcl (bucket, key, region, encrypt=true, use_lockfile=true), terraform.tfvars.json (0600)
  pub fn child_env(conn: &HostConnection, keys: Option<&AwsKeys>, plugin_cache: &Path) -> Vec<(String, String)>; // the Global Constraints env, nothing inherited from AWS_/TF_/TOFU_
  pub enum TofuOp { Apply, Destroy }
  pub fn start_detached(tofu: &Path, wd: &Workdir, op: TofuOp, env: &[(String, String)]) -> Result<u32 /*pid*/, AwsError>; // runs `init -input=false -backend-config=backend.hcl` then `apply|destroy -auto-approve -json -input=false`, as one detached shell-free sequence (a tiny runner: spawn init, wait, spawn apply) writing apply.log, apply.pid, apply.exit
  pub fn is_running(wd: &Workdir) -> bool;                                 // apply.pid alive and apply.exit absent
  pub fn interrupt(wd: &Workdir) -> Result<(), AwsError>;                  // SIGINT (Windows: CTRL_BREAK to the process group)
  #[derive(Serialize, Clone, Debug, PartialEq)] pub struct TofuEvent { pub ts: String, pub level: String, pub kind: String /* "@type" */, pub resource: Option<String>, pub action: Option<String>, pub message: String }
  pub fn parse_log_line(line: &str) -> Option<TofuEvent>;                  // tofu -json lines; non-JSON lines (init output) become kind "log"
  pub async fn follow(wd: &Workdir, poll: Duration, on_events: impl FnMut(&[TofuEvent])) -> Result<TofuOutcome, AwsError>; // from byte 0, each line once, until apply.exit exists
  pub struct TofuOutcome { pub ok: bool, pub exit_code: i32, pub summary: Option<String> }
  ```
- Detaching: the runner is the app binary itself, re-executed with a hidden `--tofu-runner <workdir> <op>` argument, so no shell is involved.
  - The runner runs init and then apply, writes the exit code, and exits.
  - On Unix it is spawned with `setsid` (`pre_exec` with `libc::setsid`). On Windows it uses `CREATE_NEW_PROCESS_GROUP | DETACHED_PROCESS`.
  - Changing `main.rs` to handle the flag before Tauri starts is part of this task.
  - The runner receives `child_env` by inheriting the environment the app gives it at spawn time. **Credentials never go on argv.**

- [ ] **Step 1: Write the failing tests:**
  - `a_download_with_the_wrong_hash_is_deleted_and_refused`: write a fake archive, call `verify_and_extract` with another hash, and expect `Err`, with the archive gone and nothing extracted.
  - `the_child_environment_carries_nothing_inherited`: set `AWS_ENDPOINT_URL`, `AWS_ACCESS_KEY_ID`, `TF_LOG` and `TOFU_X` in the test's view of the env (pass the parent env in as a parameter rather than mutating the process env). Expect none of them in `child_env`, except the connection's own keys or profile. Expect the four `TF_*` and `CHECKPOINT_*` values the app sets.
  - `materialize_writes_backend_and_vars_privately`: `backend.hcl` holds the bucket, key, region, `use_lockfile = true` and `encrypt = true`. `terraform.tfvars.json` has mode `0600` on Unix. The module files are byte-identical.
  - `following_an_existing_log_reports_each_event_once`: write 3 JSON lines, then 2 more, then `apply.exit`. The callback sees 5 events, in order, once each, and the outcome is `ok`.
  - `apply_errored_and_diagnostics_surface_their_message`: parse a real `-json` `diagnostic` line (severity error) and an `apply_errored` line.
  - `release_assets_exist_for_every_shipped_platform`.
- [ ] **Step 2: Run** `cargo test aws::tofu`. Expected: FAIL.
- [ ] **Step 3: Implement it.** Take the hashes from the release's `SHA256SUMS` file; verify that file's signature if practical, and say in the report what you did.
- [ ] **Step 4: Run** the tests, then do a local smoke test. With `ensure_tofu` into a temp root, `materialize` the boundary module with a dummy bucket, and run only `tofu init -backend=false` plus `validate` through the same binary. Expected: PASS.
- [ ] **Step 5: Commit** with the message `feat(aws): pinned OpenTofu download and a detached apply runner`.

### Task 5: The state bucket and detection (`aws/environments.rs`)

**Files:**
- Create: `app/src-tauri/src/aws/environments.rs`, with tests in `aws/environments/tests.rs`
- Modify: `Cargo.toml` (`aws-sdk-s3`)

**Interfaces:**
- Consumes: `tofu_modules::*`, `tofu::is_running`/`Workdir` (for the local-process check), `identity::CallerIdentity`.
- Produces:
  ```rust
  #[derive(Deserialize, Serialize, Clone, Debug, PartialEq)] pub struct EnvironmentInfo { schema: u32, name, template_version: u32, cluster_arn, security_group_id, subnet_ids: Vec<String>, assign_public_ip: bool, kms_key_arn, log_group, network_mode, tenancy }
  pub fn parse_environment_record(path_name: &str, value: &str, account: &str, region: &str) -> Result<EnvironmentInfo, String>;
  #[derive(Serialize, Clone, Debug, PartialEq)] pub struct LockInfo { who: String, created: String, operation: String, age_secs: u64, local_process: bool }
  #[derive(Serialize, Clone, Debug, PartialEq)] #[serde(tag = "state", rename_all = "snake_case")]
  pub enum EnvironmentState { Ready, Deploying { lock: Option<LockInfo> }, UpdateAvailable, Incomplete { lock: Option<LockInfo> }, ExternallyManaged, UnreadableRecord { reason: String } }
  #[derive(Serialize, Clone, Debug, PartialEq)] #[serde(rename_all = "snake_case")] pub enum EnvironmentAction { Join, Update, Repair, Destroy, Follow, Unlock }
  #[derive(Serialize, Clone, Debug, PartialEq)] #[serde(tag = "state", rename_all = "snake_case")]
  pub enum OnboardingState { Joined, NotJoined, InProgress, Incomplete }
  #[derive(Serialize, Clone, Debug, PartialEq)] pub struct DetectedEnvironment { name, template_version: Option<u32>, state: EnvironmentState, actions: Vec<EnvironmentAction>, info: Option<EnvironmentInfo>, onboarding: Option<OnboardingState> }
  #[derive(Serialize, Clone, Debug, PartialEq)] #[serde(rename_all = "snake_case")] pub enum Knowable { Yes, No, Unknown }
  #[derive(Serialize, Clone, Debug)] pub struct EnvironmentScan { region, account, owner_id, owner_key, state_bucket: Knowable, boundary: Knowable, environments: Vec<DetectedEnvironment> }
  pub struct StateObjects { pub states: Vec<String> /* keys */, pub locks: Vec<(String, String /*lock json*/, u64 /*age*/)> }
  pub fn merge(records: Vec<(String, Result<EnvironmentInfo, String>)>, objects: Option<&StateObjects>, running_locally: &dyn Fn(&str) -> bool, current_version: u32) -> Vec<DetectedEnvironment>;
  #[async_trait] pub trait DetectApi: Send + Sync {
      async fn environment_records(&self) -> Result<Vec<(String, String)>, AwsError>;
      async fn state_objects(&self, bucket: &str) -> Result<Option<StateObjects>, AwsError>; // None = bucket missing or denied
      async fn bucket_exists(&self, bucket: &str) -> Result<Knowable, AwsError>;
      async fn boundary_exists(&self, account: &str) -> Result<Knowable, AwsError>;
      async fn role_exists(&self, name: &str) -> Result<Knowable, AwsError>;
  }
  pub async fn scan<D: DetectApi>(d: &D, who: &CallerIdentity, region: &str, running_locally: &dyn Fn(&str) -> bool) -> Result<EnvironmentScan, AwsError>;
  pub async fn ensure_state_bucket(cfg: &SdkConfig, account: &str, region: &str) -> Result<String, AwsError>;
  ```
- `ensure_state_bucket` creates the bucket with the `triple-c:component=state` tag. `LocationConstraint` is omitted in `us-east-1`. It then sets versioning, SSE-S3 (AES256) encryption, all four public-access blocks, and a bucket policy denying non-TLS requests. It is idempotent: `BucketAlreadyOwnedByYou` counts as success, and `BucketAlreadyExists` from another owner is an error that names the bucket.
- Rules for `merge`:
  - **Ready**: a record with state at the current version.
  - **UpdateAvailable**: a record with state at an older version.
  - **Deploying**: the lock is held and either a local process is running or the lock is less than 2 hours old. The action is Follow when the process is local.
  - **Incomplete**: state with no record and no live lock, or a stale lock. Actions are Repair and Destroy, plus Unlock when the lock is stale and not local.
  - **ExternallyManaged**: a record with no state (or no bucket access). The only action is Join.
  - Join is offered for Ready, UpdateAvailable and ExternallyManaged.
  - Onboarding is resolved for joinable environments. Check the user's state key first, then `role_exists` on the task role. If that is denied, the state is `NotJoined`.
- `parse_environment_record` rejects:
  - more than 4 KiB, or not JSON;
  - `schema` other than 1;
  - a name that doesn't match the path;
  - any ARN not in this account and region;
  - anything other than exactly 2 `subnet-[0-9a-f]+` IDs;
  - a security group that isn't `sg-[0-9a-f]+`;
  - `tenancy` other than `strict`.

  The reason names the field and never echoes the value.

- [ ] **Step 1: Write the failing tests:**
  - `untrusted_environment_records_are_rejected`: a table of those cases, plus one valid record.
  - `a_record_without_state_and_state_without_a_record_are_both_listed`.
  - `interrupted_deployments_get_the_right_actions`:
    - a stale lock with no local process gives `Incomplete` with [Repair, Destroy, Unlock];
    - a fresh lock gives `Deploying` with no actions;
    - a local process gives `Deploying` with [Follow].
  - `an_older_module_offers_update`.
  - `onboarding_is_found_by_state_then_by_role`.
  - `denied_lookups_are_unknown_not_missing`.
- [ ] **Step 2: Run** `cargo test aws::environments`. Expected: FAIL.
- [ ] **Step 3: Implement it**, including the SDK `DetectApi`: SSM `GetParametersByPath`, S3 `ListObjectsV2` with the prefixes `environments/`, `users/` and `boundary/` plus `GetObject` on `*.tflock`, and IAM `GetPolicy`/`GetRole`. Access denied maps to `Unknown` or `None`.
- [ ] **Step 4: Run** the tests. Expected: PASS.
- [ ] **Step 5: Commit** with the message `feat(aws): state bucket, and detection of environments, locks and onboarding`.

### Task 6: Commands, progress events and admin bundles

**Files:**
- Create: `app/src-tauri/src/commands/environment_commands.rs`
- Modify: `commands/mod.rs`, `lib.rs`, `capabilities/default.json`, `app/src/lib/{types,tauri-commands}.ts`

**Interfaces:**
- Produces these commands. Each takes `connection_id` and `region` (validated). The work root is `dirs::data_dir()/triple-c/aws/`. An `op_id` is a UUID.
  ```rust
  detect_aws_environments(connection_id, region) -> Result<EnvironmentScan, AwsError>
  deploy_aws_environment(connection_id, region, name, network: NetworkChoice, app) -> Result<String, AwsError>  // ensure_tofu, ensure_state_bucket, refuse if boundary == No, refuse if a record or state for `name` exists
  update_aws_environment(connection_id, region, name, app) -> Result<String, AwsError>   // re-apply with vars read back from the record (network from record; never change network on update)
  repair_aws_environment(connection_id, region, name, network: Option<NetworkChoice>, app) -> Result<String, AwsError> // re-apply an Incomplete one; network required if no record
  destroy_incomplete_aws_environment(connection_id, region, name, app) -> Result<String, AwsError> // only when detection says Incomplete
  unlock_aws_state(connection_id, region, state_key) -> Result<(), AwsError>             // only for a stale, non-local lock (re-checked server-side), deletes the .tflock object
  follow_aws_deployment(connection_id, region, state_key, app) -> Result<String, AwsError>
  cancel_aws_deployment(connection_id, region, state_key) -> Result<(), AwsError>        // tofu::interrupt
  onboard_aws_user(connection_id, region, environment, app) -> Result<String, AwsError>  // self-onboarding with the caller's own owner id
  deploy_aws_permissions_boundary(connection_id, region, app) -> Result<String, AwsError>
  save_aws_admin_bundle(connection_id, region, kind: ModuleKind, environment: Option<String>, owner_id: Option<String>, window) -> Result<Option<String>, AwsError> // Rust-side folder dialog; writes D5 bundle; returns the folder path
  ```
  - Progress is emitted on the Tauri event `aws-deploy-progress` as `{ op_id, state_key, events: TofuEvent[], done: bool, result?: { ok, exit_code, summary? } }`.
  - State keys passed in from the frontend must equal a key `state_key()` can produce. Anything else is refused.
  - The bundle's `README.txt` gives the exact commands for this account, region and bucket. Its vars come from the same `*_vars` functions the app applies with. The bucket must exist first, so the README says to deploy from the app once or to create the bucket by hand, and gives the CLI for that.

- [ ] **Step 1: Write the failing tests** against the pure helpers:
  - `bundles_contain_the_module_backend_vars_and_readme`: `write_bundle(dir, …)` writes every `module_files` entry, plus `backend.hcl`, `terraform.tfvars.json` and a README naming the bucket and key.
  - `only_our_state_keys_are_accepted`.
  - `deploy_refuses_an_existing_name_and_a_missing_boundary`.
  - `update_reuses_the_recorded_network`.
- [ ] **Step 2: Run** `cargo test environment_commands`. Expected: FAIL.
- [ ] **Step 3: Implement** the commands, register them, grant them, and add the TS types and wrappers.
- [ ] **Step 4: Run** `cargo test`, `cargo check` and `npx vitest run src/test/capabilities.test.ts`. Expected: PASS.
- [ ] **Step 5: Commit** with the message `feat(aws): environment commands with streamed OpenTofu progress`.

### Task 7: Environments UI

**Files:**
- Create: `app/src/components/settings/hosts/{EnvironmentsPanel,DeployEnvironmentModal,AdminBundleModal,DeployProgress}.tsx`, plus tests for the first three
- Modify: `HostsSettings.tsx`, which renders `<EnvironmentsPanel connection={c} tier={lastReport?.summary} />`

**Behaviour.** This follows spec §2's table. It uses `StatusIndicator`, `Modal`, `Button`, `Field`, `SegmentedControl` and the design tokens.
- **Detection.** A region select defaults to `default_region`. Detection runs when the panel opens and whenever the region changes. There is also a "Detect again" button.
- **Environment rows.** Each row shows the state (Lock owner and age are shown when present), the version, and its actions:
  - Join;
  - Update;
  - Repair, which asks for the network if there is no record;
  - Destroy, which requires typing the name;
  - Follow;
  - Unlock, which shows who holds the lock and since when, and needs a typed confirmation;
  - Cancel, during a local deploy.
- **Joining.**
  - When the user is already onboarded, the row reads "Joined".
  - For an admin, Join runs `onboardAwsUser` and shows progress.
  - Anyone else gets the `AdminBundleModal` for onboarding, and the row reads "Waiting for an admin to onboard you".
- **Deploying.**
  - "Deploy a new environment" is the primary action when nothing is found and the user is an admin. It is secondary when environments exist.
  - A non-admin with nothing found sees an explanation, the policy files (`awsPolicyDocument("environment_admin")`), and the environment `AdminBundleModal`.
- **`DeployEnvironmentModal`:**
  - Name: defaults to `main`, and is validated with the Rust rule mirrored in TS.
  - Network: the D4 modes, each with its cost line. `public` reads "$0 — tasks get a public IP, no inbound access". `private-nat` reads "about $33/month for the NAT gateway". `existing` has a VPC ID, two subnet IDs and a public-IP toggle.
  - If `boundary` is `no`, the modal first offers a step: "Create the permissions boundary (needs an account administrator)". If that is denied, it opens the boundary bundle.
  - A first-run note says "First deploy downloads OpenTofu (~30 MB) and the AWS provider (~150 MB)."
- **`DeployProgress`.** It lists events in order: resource, action and message. Errors are shown in full. A final result line triggers re-detection. Cancel calls `cancelAwsDeployment` after a confirmation.
- **`AdminBundleModal`.** It explains the bundle and has a "Save bundle to folder…" button (`saveAwsAdminBundle`). After saving, it shows the saved path and the two commands, with a Copy button.

- [ ] **Step 1: Write the failing tests:**
  - `shows deploy as primary when nothing is found and the user is an admin`.
  - `offers an admin bundle instead of deploy for a workspace user with nothing found`.
  - `an incomplete environment offers repair and destroy, destroy only after typing its name`.
  - `a stale lock offers unlock with its owner and age`.
  - `join onboards an admin and shows progress until done, then re-detects`.
  - `deploy modal blocks on a missing boundary and offers to create it`.
  - `the public network mode is the default and says it costs nothing`.
- [ ] **Step 2: Run** `npx vitest run src/components/settings/hosts`. Expected: FAIL.
- [ ] **Step 3: Implement it.**
- [ ] **Step 4: Run** `npx vitest run` and `npx tsc --noEmit`. Expected: PASS.
- [ ] **Step 5: Commit** with the message `feat(aws): environments panel — detect, deploy, update, repair, join`.

### Task 8: Live verification, docs and spec

**Files:**
- Modify: `app/src-tauri/src/aws/live_tests.rs`, adding the `environment` tests
- Modify: `aws/README.md`. Add an "Environments (OpenTofu)" section covering the modules, the state bucket, bundles and running them by hand. Add the results to "Live verification".
- Modify: `CLAUDE.md`'s `aws/` paragraph, covering `tofu_modules`, `tofu`, `environments`, D2 and D10.
- Modify: the spec. Phase 4 is done. Replace the CloudFormation design text (§2 "Deploy uses the CloudFormation template…", stack detection, "Shipped AWS artefacts") with the OpenTofu design and decisions D2–D10, and update "To verify".

**Live tests** (ignored; they read `TRIPLEC_AWS_E2E_PROFILE` and `TRIPLEC_AWS_E2E_REGION`; run with `awsx cargo test aws::live_tests::environment -- --ignored --test-threads=1`):
- `an_environment_admin_can_deploy_join_update_and_remove_an_environment`:
  1. As admin, make sure the boundary exists. If it doesn't, apply the boundary module and remember to remove it at the end.
  2. Create the role `triple-c-e2e-envadmin` with the admin tier's policies attached as `triple-c-e2e-*` managed policies, and with the boundary set.
  3. Assume that role. Every step from here runs as it **only**. Pass its temporary keys to the child as a keys connection.
  4. `ensure_state_bucket` on a test-unique bucket. Allow a `TRIPLEC_E2E_BUCKET_SUFFIX` so the test doesn't touch a real one.
  5. Deploy `e2e<rand>` in `public` mode through the real runner and follow it to `ok`.
  6. `scan` finds it Ready with a valid record.
  7. Self-onboard and see `Joined`.
  8. Update. The apply shows "No changes".
  9. Destroy the onboarding, then the environment.
  10. As admin, delete the role, the policies, the bucket (every version), and the boundary if this test created it.
  - A guard runs cleanup on failure too.
  - Every denied action is printed with its name.
- `the_private_nat_mode_deploys` (ignored separately): deploy in `private-nat` mode, check it reaches `ok`, then destroy. It costs about $0.05.

- [ ] **Step 1: Write the tests.**
- [ ] **Step 2: Run them and fix any denied action.** Each fix goes into the Task 3 policies, the resource-action table, the check table and the README together. Re-run until green, and record what IAM needed beyond the table.
- [ ] **Step 3: Confirm cleanup.** Look for leftover `triple-c-*` roles, policies and log groups, any `triple-c` VPCs, EIPs, EFS file systems, `triple-c-tfstate-*` buckets, and `alias/triple-c-*` aliases. Expected: none, apart from KMS keys pending deletion. Record the spend.
- [ ] **Step 4: Write the docs and the spec**, then run the full `cargo test`, `npx vitest run` and `npx tsc --noEmit`. Expected: PASS.
- [ ] **Step 5: Commit** with the message `test(aws): live environment lifecycle with OpenTofu as an environment admin; docs`.
