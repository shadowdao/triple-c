# AWS Connection + Permission Check (Phase 3) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A user can add an AWS connection in Settings → Hosts (an AWS profile, including SSO, or access keys, plus a default region). They see "Signed in as `<arn>`", and can run a permission check that reports Environment admin, Workspace user or Insufficient. For anything missing, it names the exact actions and offers the policy JSON to send to an admin.

**Architecture:**
- A new `aws/` Rust module builds SDK configs from a `HostConnection`: an explicit profile provider, or static keys read from the keyring.
- It calls `sts:GetCallerIdentity` and runs a table-driven permission check. That check uses `iam:SimulatePrincipalPolicy`, falling back to read-only probes when simulation is denied.
- The two tier policies ship as JSON in `aws/policies/` and are compiled in with `include_str!`. A test keeps the check table and the policy files identical action-for-action.
- Tauri commands in `commands/host_commands.rs`. The React UI is `components/settings/HostsSettings.tsx`.

**Tech Stack:**
- Rust: Tauri v2. Already present: `aws-config` 1.13 (`sso`, `credentials-process`), `aws-sdk-ecs`, `aws-sdk-ssm`. Added: `aws-sdk-sts`, `aws-sdk-iam`, `aws-sdk-cloudformation`, `aws-sdk-secretsmanager`, all with `default-features = false` and features `default-https-client`, `rt-tokio`, `behavior-version-latest`, the same as the existing ones.
- `keyring` through `storage/secure.rs`.
- React + TypeScript + vitest.

**Spec:** `docs/superpowers/specs/2026-11-13-aws-ecs-host-design.md`. Relevant sections: "User flow" §1 and §3, "Multi-user accounts", "Data model", "Shipped AWS artefacts", "Connection model".

## Global Constraints

- Connection model **C**: a connection is credentials plus a **default** region. Region and environment belong to a project later (phase 5), not to the connection.
- Auth options are the same as the AWS CLI's: `AwsAuth::Profile { name }` (any profile in `~/.aws/config`, SSO included) or `AwsAuth::Keys { keyring_ref }`.
- Access keys are stored in the OS keyring through `storage/secure.rs`, **never in `settings.json`**.
- `AppSettings.hosts: Vec<HostConnection>`. Local is implicit and is not stored. Existing `settings.json` files must deserialize unchanged (`#[serde(default)]`).
- The owner ID is `UserId` from `GetCallerIdentity`, i.e. `aws:userid`. Every per-user resource is tagged `triple-c:owner = ${aws:userid}`.
- Tags: `triple-c:owner`, `triple-c:project`, `triple-c:environment`, `triple-c:component`.
- Resource naming fixed by this phase, which phase 4's templates must use:
  - clusters `triple-c-*`;
  - task definition families `triple-c-*`;
  - secrets `triple-c/*`;
  - SSM parameters `/triple-c/*`;
  - stacks `triple-c-*`;
  - IAM roles `triple-c-*`.
- The simulator does not apply SCPs, permission boundaries or session policies. The UI must say so, and write actions that were only probed are shown as **"unverified — will be confirmed on first use"**, never as OK.
- AWS calls must not inherit the app process's `AWS_*` environment for a **profile** connection. Use `ProfileFileCredentialsProvider` with the explicit profile name, not the default chain.
- Every new Tauri command is registered in `lib.rs` and granted in `capabilities/default.json`. `src/test/capabilities.test.ts` must pass.
- Deferred on purpose:
  - **EFS.** The spec's `elasticfilesystem:DescribeAccessPoints` probe waits for the EFS crate, which arrives with EFS mode in phase 6. Until then, EFS read checks are `Unverified` in probe mode.
  - **Role policies.** `task-execution-role.json` and `task-role-standard.json` ship with the phase 4 templates that use them.
  - **Per-environment caching.** Results are cached per `(connection, region, environment)` and re-run on a denial. Environments arrive in phase 4, so for now the check runs on demand and the UI keeps the last result.
- No AWS spending. The permission check uses only IAM simulation, STS and read-only list/describe calls.

## Review Focus

1. **SSO profile with an expired token.** Expect a typed `AwsError::SsoExpired` and a "Sign in" button that runs `aws sso login --profile <name>`, not a raw SDK error. Test: `an_expired_sso_token_is_reported_as_sso_expired` (Task 3).
2. **An SSO or assumed-role caller.** `GetCallerIdentity` returns `arn:aws:sts::<acct>:assumed-role/<Role>/<session>`, but simulation needs the role ARN *with its path*. SSO roles live under `/aws-reserved/sso.amazonaws.com/<region>/`. Look the path up with `iam:GetRole`. If that is denied, fall back to probes rather than simulating a wrong ARN. Tests: `assumed_role_arn_maps_to_role_arn_with_path` and `a_denied_get_role_falls_back_to_probes` (Tasks 3 and 5).
3. **The caller can't run `iam:SimulatePrincipalPolicy`** (common for locked-down SSO roles). Expect the probe fallback: probed reads are marked allowed or denied, writes unverified, and the tier labelled "unverified". Test: `a_denied_simulation_falls_back_to_probes_and_marks_writes_unverified` (Task 5).
4. **Removing a connection whose keyring entry is already gone** (deleted by hand, or another OS user). Expect removal to still succeed, because a missing entry is not an error. Test: `removing_a_keys_connection_with_no_keyring_entry_succeeds` (Task 2).
5. **Settings import on another machine.** A `Keys` connection must arrive with its keys (exported like marketplace tokens), and a `Profile` connection arrives as-is even if that profile doesn't exist there. Checking it then says "profile not found", not a panic. Tests: `aws_connection_keys_round_trip_through_export` (Task 2) and `a_missing_profile_is_reported_by_name` (Task 3).

---

### Task 1: Data model — `HostConnection` in settings (Rust + TS)

**Files:**
- Create: `app/src-tauri/src/models/host_connection.rs`
- Modify: `app/src-tauri/src/models/mod.rs` (add `pub mod host_connection;` and re-export)
- Modify: `app/src-tauri/src/models/app_settings.rs` (add `#[serde(default)] pub hosts: Vec<HostConnection>` after `marketplaces`, plus `Default`)
- Modify: `app/src/lib/types.ts` (add `HostConnection`, `AwsAuth`; add `hosts: HostConnection[]` to `AppSettings`)

**Interfaces:**
- Produces:
  ```rust
  #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
  pub struct HostConnection { pub id: String, pub name: String, pub kind: HostKind }
  #[derive(..)] #[serde(tag = "type", rename_all = "snake_case")]
  pub enum HostKind { Aws { auth: AwsAuth, default_region: String } }
  #[derive(..)] #[serde(tag = "method", rename_all = "snake_case")]
  pub enum AwsAuth { Profile { name: String }, Keys { keyring_ref: String } }
  pub fn validate_connection_name(name: &str) -> Result<String, String>   // trimmed, 1..=64 chars, no control chars
  pub fn validate_region(region: &str) -> Result<String, String>          // ^[a-z]{2}(-[a-z]+)+-\d+$
  pub fn validate_profile_name(name: &str) -> Result<String, String>      // non-empty, ≤128, [A-Za-z0-9_.@+=,-]
  ```
  TS: `type AwsAuth = { method: "profile"; name: string } | { method: "keys"; keyring_ref: string }`; `interface HostConnection { id: string; name: string; kind: { type: "aws"; auth: AwsAuth; default_region: string } }`.

- [ ] **Step 1: Write failing tests** in `host_connection.rs` `mod tests`:
  - `settings_without_hosts_deserialize_to_empty`: a minimal `AppSettings` JSON with no `hosts` key gives `hosts == vec![]`.
  - `a_profile_connection_round_trips`: serialize then deserialize gives an equal value, and the JSON contains `"type":"aws"` and `"method":"profile"`.
  - `regions_are_validated`: accepts `us-west-2`, `eu-central-1`, `cn-north-1`, `us-gov-west-1`; rejects `""`, `US-WEST-2`, `us-west`, `us-west-2; rm`.
  - `profile_names_are_validated`: accepts `default` and `340315884407_StaffAdministratorAccess`; rejects `""`, `a b`, `x\n`, and 129 chars.
- [ ] **Step 2: Run them:** `cd app/src-tauri && cargo test host_connection`. Expected: FAIL (module missing).
- [ ] **Step 3: Implement the types and validators.** Add `hosts` to `AppSettings` and its `Default`.
- [ ] **Step 4: Run them:** `cargo test host_connection` passes. Run `cargo test` (whole crate) to catch any other `AppSettings { .. }` literal that needs the field.
- [ ] **Step 5: Add the TS types.** `cd app && npx tsc --noEmit` passes. Update any test fixture that builds an `AppSettings` with `hosts: []`.
- [ ] **Step 6: Commit** `feat(aws): HostConnection model in app settings`.

### Task 2: Keyring storage for access keys, and settings export

**Files:**
- Modify: `app/src-tauri/src/storage/secure.rs` (new section modelled on the marketplace-token one)
- Modify: `app/src-tauri/src/models/settings_export.rs` (`ExportedSecrets.aws_connection_keys: BTreeMap<String, String>`, `#[serde(default)]`)
- Modify: `app/src-tauri/src/commands/settings_export_commands.rs` (export and restore, mirroring `exported_marketplace_tokens` / `restore_marketplace_tokens`)

**Interfaces:**
- Produces:
  ```rust
  #[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]   // Debug impl redacts the secret and token
  pub struct AwsKeys { pub access_key_id: String, pub secret_access_key: String, pub session_token: Option<String> }
  pub fn store_aws_keys(connection_id: &str, keys: &AwsKeys) -> Result<(), String>   // service "triple-c-aws-connection-<id>", JSON value
  pub fn get_aws_keys(connection_id: &str) -> Result<Option<AwsKeys>, String>
  pub fn delete_aws_keys(connection_id: &str) -> Result<(), String>                  // missing entry = Ok
  ```
  `keyring_ref` in `AwsAuth::Keys` is the connection id. Ids are validated like `marketplace_token_service`.

- [ ] **Step 1: Write failing tests:**
  - `aws_keys_debug_redacts_secrets`: `format!("{:?}", keys)` contains neither the secret nor the token.
  - `aws_key_service_names_reject_bad_ids`: rejects `"../x"`, `""`, and a 65-char id.
  - `removing_a_keys_connection_with_no_keyring_entry_succeeds`: run against the existing mock-keyring test pattern in `secure.rs` if there is one; otherwise as a unit test on `delete_entry`'s `NoEntry` → `Ok` mapping.
  - `aws_connection_keys_round_trip_through_export`, in `settings_export_commands.rs` tests: a `Keys` connection's stored keys appear in `ExportedSecrets.aws_connection_keys` (as JSON), a `Profile` connection contributes nothing, and restore calls the store fn with the same id and keys.
- [ ] **Step 2: Run** `cargo test aws_key` and `cargo test aws_connection_keys`. Expected: FAIL.
- [ ] **Step 3: Implement.** On restore, a value that fails to parse as `AwsKeys` gives a warning naming the connection id, never the value.
- [ ] **Step 4: Run** those tests, then all of `cargo test settings_export`. All pass.
- [ ] **Step 5: Commit** `feat(aws): keyring storage for AWS access keys, carried by settings export`.

### Task 3: SDK config and identity (`aws::connection`, `aws::identity`)

**Files:**
- Modify: `app/src-tauri/Cargo.toml` (add `aws-sdk-sts`, `aws-sdk-iam`, `aws-sdk-cloudformation`, `aws-sdk-secretsmanager` at the current versions matching `aws-config 1.13.x`, with the feature set above)
- Create: `app/src-tauri/src/aws/mod.rs`, `app/src-tauri/src/aws/connection.rs`, `app/src-tauri/src/aws/identity.rs`, `app/src-tauri/src/aws/error.rs`
- Modify: `app/src-tauri/src/lib.rs` (`mod aws;`)

**Interfaces:**
- Consumes: `HostConnection`, `AwsAuth` (Task 1); `secure::get_aws_keys` (Task 2).
- Produces:
  ```rust
  #[derive(Debug, Clone, Serialize, PartialEq, Eq)] #[serde(tag = "kind", rename_all = "snake_case")]
  pub enum AwsError {
      SsoExpired { profile: String },       // the UI offers "Sign in"
      ProfileNotFound { profile: String },
      KeysMissing,                          // keyring entry gone
      InvalidCredentials { message: String },
      AccessDenied { action: String, message: String },
      Other { message: String },
  }
  impl std::fmt::Display for AwsError
  pub async fn sdk_config(conn: &HostConnection, region: &str) -> Result<aws_config::SdkConfig, AwsError>
  #[derive(Debug, Clone, Serialize, PartialEq, Eq)]
  pub struct CallerIdentity { pub account: String, pub arn: String, pub user_id: String /* owner id */ }
  pub async fn caller_identity(cfg: &aws_config::SdkConfig) -> Result<CallerIdentity, AwsError>
  pub enum PrincipalArn { Exact(String), AssumedRole { account: String, role_name: String } }
  pub fn principal_for_simulation(caller_arn: &str) -> Result<PrincipalArn, AwsError>
  pub async fn resolve_principal(iam: &aws_sdk_iam::Client, p: PrincipalArn) -> Result<String, AwsError>  // GetRole for the path; denied → AccessDenied{action:"iam:GetRole"}
  pub fn classify_sdk_error(action: &str, err: &(dyn std::error::Error + 'static)) -> AwsError
  ```
  `sdk_config`:
  - `Profile` uses `ProfileFileCredentialsProvider::builder().profile_name(name)`, plus `.region(Region::new(region))`. It does not use the default chain.
  - `Keys` uses `Credentials::new(akid, secret, token, None, "triple-c-connection")`.
  - The config is built lazily, so profile problems surface on the first call and are classified there.

- [ ] **Step 1: Write failing tests** (pure, no network):
  - `iam_user_arn_is_used_as_is`: `arn:aws:iam::111122223333:user/alice` gives `Exact`.
  - `assumed_role_arn_maps_to_role_name`: `arn:aws:sts::111122223333:assumed-role/AWSReservedSSO_Admin_abc/alice@x.com` gives `AssumedRole{account:"111122223333", role_name:"AWSReservedSSO_Admin_abc"}`.
  - `root_and_federated_arns_are_rejected_for_simulation`: `arn:aws:iam::1:root` and `arn:aws:sts::1:federated-user/bob` give `Err(AwsError::Other{..})`, with a message saying simulation isn't possible for this principal (the caller then falls back to probes).
  - `assumed_role_arn_maps_to_role_arn_with_path`: `resolve_principal` against a fake IAM endpoint. Use an `aws_smithy_runtime` `StaticReplayClient`, or a local `tokio` HTTP listener as in `host/ssm/tests.rs` `a_dropped_open_still_ends_its_session`. The fake returns GetRole XML with `<Path>/aws-reserved/sso.amazonaws.com/us-west-2/</Path>` and `<Arn>`; expect that `Arn`.
  - `an_expired_sso_token_is_reported_as_sso_expired`: `classify_sdk_error` on an error whose display chain contains "The SSO session associated with this profile has expired" or "Token has expired" gives `SsoExpired`.
  - `a_missing_profile_is_reported_by_name`: `sdk_config` + `caller_identity` with `AWS_CONFIG_FILE` and `AWS_SHARED_CREDENTIALS_FILE` pointing at an empty temp file gives `ProfileNotFound{profile}`. Use the provider's `ProfileFileCredentialsProvider` profile-files builder to point at the temp files, not process env, so tests don't race.
- [ ] **Step 2: Run** `cargo test aws::`. Expected: FAIL.
- [ ] **Step 3: Implement.** Keep `classify_sdk_error` substring-based on `DisplayErrorContext` text, plus the SDK error code (`AccessDenied`, `AccessDeniedException`, `ExpiredToken`, `InvalidClientTokenId`, `SignatureDoesNotMatch`).
- [ ] **Step 4: Run** `cargo test aws::`. Passes. Run `cargo tree -i rustls@0.21` and confirm it is still absent: no legacy TLS stack.
- [ ] **Step 5: Commit** `feat(aws): SDK config from a connection, caller identity, principal resolution`.

### Task 4: Shipped tier policies and the check table

**Files:**
- Create: `aws/policies/workspace-user.json`, `aws/policies/environment-admin.json` (repo root)
- Create: `aws/README.md` (short: what each file grants and how to attach it)
- Create: `app/src-tauri/src/aws/policies.rs`

**Interfaces:**
- Produces:
  ```rust
  #[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)] #[serde(rename_all = "snake_case")]
  pub enum Tier { EnvironmentAdmin, WorkspaceUser }
  pub fn policy_json(tier: Tier) -> &'static str          // include_str!("../../../../aws/policies/<file>")
  pub struct Check {
      pub action: &'static str,                            // "ecs:RunTask"
      pub resource: &'static str,                          // template; {region} {account} substituted
      pub context: &'static [(&'static str, ContextValue)],// e.g. ("aws:RequestTag/triple-c:owner", OwnerId)
      pub write: bool,
  }
  pub enum ContextValue { OwnerId, Literal(&'static str) }
  pub fn checks(tier: Tier) -> &'static [Check]
  ```

  **Workspace-user policy.** These statements follow spec "Multi-user accounts":
  - `ecs:RunTask` on `arn:aws:ecs:*:*:task-definition/triple-c-*`, with `aws:RequestTag/triple-c:owner = ${aws:userid}`;
  - `ecs:TagResource` on create;
  - `iam:PassRole` on `arn:aws:iam::*:role/triple-c-*`, with `iam:ResourceTag/triple-c:owner = ${aws:userid}` and `iam:PassedToService = ecs-tasks.amazonaws.com`;
  - `ecs:StopTask` and `ecs:ExecuteCommand` on `arn:aws:ecs:*:*:task/triple-c-*/*`, with `aws:ResourceTag/triple-c:owner = ${aws:userid}`;
  - `ssm:StartSession` on `arn:aws:ecs:*:*:task/triple-c-*/*` (with the same tag) and on `arn:aws:ssm:*::document/AWS-StartPortForwardingSession`;
  - `ssm:TerminateSession` and `ssm:ResumeSession` on `arn:aws:ssm:*:*:session/*` with `ssm:resourceTag/aws:ssmmessages:session-id = ${aws:userid}` (the AWS-documented owner condition; session IDs are prefixed by the role session name, not `aws:userid`, so an ARN pattern can't scope them);
  - `secretsmanager:CreateSecret`, `TagResource`, `PutSecretValue` and `DeleteSecret` on `arn:aws:secretsmanager:*:*:secret:triple-c/*`, with the request tag (create) or resource tag (others);
  - `kms:Encrypt` and `kms:GenerateDataKey` with `kms:ViaService = secretsmanager.*.amazonaws.com`;
  - read-only: `ecs:Describe*`, `ecs:List*`, `ssm:GetParameter*` on `arn:aws:ssm:*:*:parameter/triple-c/*`, `cloudformation:DescribeStacks`, `cloudformation:ListStacks`, `secretsmanager:ListSecrets`, `elasticfilesystem:Describe*`, `logs:GetLogEvents` and `logs:FilterLogEvents` on `log-group:/triple-c/*`, and `sts:GetCallerIdentity`.

  **Environment-admin policy:**
  - `cloudformation:*` on `arn:aws:cloudformation:*:*:stack/triple-c-*/*`;
  - `iam:CreateRole`, `DeleteRole`, `PutRolePolicy`, `DeleteRolePolicy`, `AttachRolePolicy`, `DetachRolePolicy`, `TagRole`, `GetRole` and `PassRole` on `role/triple-c-*`;
  - `ecs:CreateCluster`, `DeleteCluster`, `RegisterTaskDefinition`, `DeregisterTaskDefinition`, `PutClusterCapacityProviders`, `CreateCapacityProvider` and `DeleteCapacityProvider`;
  - `ec2:CreateSecurityGroup`, `DeleteSecurityGroup`, `AuthorizeSecurityGroup*`, `RevokeSecurityGroup*`, `CreateTags` and `Describe*`;
  - `elasticfilesystem:CreateFileSystem`, `DeleteFileSystem`, `CreateMountTarget`, `DeleteMountTarget`, `PutFileSystemPolicy` and `TagResource`;
  - `kms:CreateKey`, `ScheduleKeyDeletion`, `CreateAlias`, `DeleteAlias`, `PutKeyPolicy` and `TagResource`;
  - `logs:CreateLogGroup`, `DeleteLogGroup` and `PutRetentionPolicy` on `/triple-c/*`;
  - `ecr:CreateRepository`, `DeleteRepository` and `CreatePullThroughCacheRule`;
  - `ssm:PutParameter` and `DeleteParameter` on `parameter/triple-c/*`;
  - `iam:SimulatePrincipalPolicy` on `*`.

  The check table holds one representative `Check` per action. Each resource template is a concrete name inside the pattern, for example `arn:aws:ecs:{region}:{account}:task-definition/triple-c-probe:1`. Wildcard actions like `ecs:Describe*` are checked as one concrete action, `ecs:DescribeClusters`.

- [ ] **Step 1: Write failing tests** in `policies.rs`:
  - `policies_are_valid_iam_json`: each file parses. `Version == "2012-10-17"`, and every statement has `Sid`, `Effect: "Allow"`, `Action` and `Resource`.
  - `every_policy_action_has_a_check_and_vice_versa`: the set of actions in the JSON (wildcard actions matched by glob against the check's action) equals the set covered by `checks(tier)`. This is the drift guard.
  - `every_check_resource_matches_a_statement_resource`: after substituting `{region}`=`us-west-2` and `{account}`=`111122223333`, each check's resource glob-matches one of the `Resource` patterns of a statement that contains its action.
  - `owner_scoped_actions_carry_the_owner_context`: `ecs:RunTask`, `ecs:StopTask`, `ecs:ExecuteCommand`, `ssm:StartSession` (task resource), `iam:PassRole` and `secretsmanager:CreateSecret` each have an `OwnerId` context entry with the key their statement's condition uses.
- [ ] **Step 2: Run** `cargo test aws::policies`. Expected: FAIL.
- [ ] **Step 3: Write the two JSON files and the table.** Write `aws/README.md`, and state there that statements are verified against a real account by Task 7's live test.
- [ ] **Step 4: Run** `cargo test aws::policies`. Passes.
- [ ] **Step 5: Commit** `feat(aws): shipped tier policies and the permission check table`.

### Task 5: The permission check

**Files:**
- Create: `app/src-tauri/src/aws/permissions.rs`, `app/src-tauri/src/aws/permissions/tests.rs`

**Interfaces:**
- Consumes: `Tier`, `checks`, `Check` (Task 4); `CallerIdentity`, `PrincipalArn`, `resolve_principal`, `AwsError` (Task 3).
- Produces:
  ```rust
  #[async_trait::async_trait]   // or native async fn in trait if the crate already avoids async_trait
  pub trait PermissionApi: Send + Sync {
      async fn resolve_principal(&self, caller_arn: &str) -> Result<String, AwsError>;
      async fn simulate(&self, principal: &str, action: &str, resource: &str, context: &[(String, String)]) -> Result<bool, AwsError>;
      async fn probe(&self, probe: Probe) -> Result<(), AwsError>;
  }
  pub enum Probe { EcsListClusters, SsmGetParametersByPath, CfnDescribeStacks, SecretsListSecrets }
  pub struct SdkPermissionApi { /* iam, ecs, ssm, cfn, secrets clients */ }
  impl SdkPermissionApi { pub fn new(cfg: &aws_config::SdkConfig) -> Self }

  #[derive(Debug, Clone, Serialize, PartialEq, Eq)] #[serde(rename_all = "snake_case")]
  pub enum Verdict { Allowed, Denied, Unverified }
  #[derive(Debug, Clone, Serialize)]
  pub struct ActionResult { pub action: String, pub verdict: Verdict }
  #[derive(Debug, Clone, Serialize)]
  pub struct TierResult { pub tier: Tier, pub results: Vec<ActionResult> }
  #[derive(Debug, Clone, Serialize, PartialEq, Eq)] #[serde(rename_all = "snake_case")]
  pub enum Method { Simulated, Probed { reason: String } }
  #[derive(Debug, Clone, Serialize, PartialEq, Eq)] #[serde(rename_all = "snake_case")]
  pub enum Summary { EnvironmentAdmin, WorkspaceUser, Insufficient }
  #[derive(Debug, Clone, Serialize)]
  pub struct PermissionReport {
      pub identity: CallerIdentity, pub region: String, pub method: Method,
      pub summary: Summary, pub tiers: Vec<TierResult>,   // [WorkspaceUser, EnvironmentAdmin]
  }
  pub async fn check_permissions(api: &dyn PermissionApi, identity: &CallerIdentity, region: &str) -> PermissionReport
  ```
  Rules:
  - **Simulated:** one `simulate` per check. Context values: `OwnerId` → `identity.user_id`, plus `aws:userid` → `identity.user_id` on every call. `EvalDecision == "allowed"` → Allowed, anything else → Denied.
  - **Fallback to Probed:** if `resolve_principal` or the *first* `simulate` returns `AccessDenied`, or `principal_for_simulation` errors, run all four probes.
    - A read check whose service has a probe gets that probe's result.
    - A read check with no probe, and every `write: true` check, is `Unverified`.
  - **Summary:** WorkspaceUser needs no `Denied` in the workspace tier. EnvironmentAdmin additionally needs no `Denied` in the admin tier. `Unverified` doesn't block a summary; the UI shows the method.
  - Simulations run with bounded concurrency (4).

- [ ] **Step 1: Write failing tests** against a `FakeApi { allow: HashSet<&str>, simulate_denied: bool, get_role_denied: bool, probes_ok: HashSet<Probe> }`:
  - `a_full_workspace_policy_reports_workspace_user`.
  - `admin_and_workspace_policies_report_environment_admin`.
  - `a_missing_action_reports_insufficient_and_names_it`: drop `iam:PassRole`. The summary is `Insufficient`, and the workspace tier lists `iam:PassRole` as `Denied`.
  - `owner_context_is_sent_with_owner_scoped_checks`: the fake records contexts. `ecs:RunTask` was simulated with `aws:RequestTag/triple-c:owner` = identity.user_id.
  - `a_denied_simulation_falls_back_to_probes_and_marks_writes_unverified`.
  - `a_denied_get_role_falls_back_to_probes`: the method is `Probed`, and its reason mentions `iam:GetRole`.
  - `a_failed_probe_marks_its_reads_denied`.
- [ ] **Step 2: Run** `cargo test aws::permissions`. Expected: FAIL.
- [ ] **Step 3: Implement** `check_permissions` and `SdkPermissionApi`.
  - `SimulatePrincipalPolicy`: set `policy_source_arn`, `action_names` = [action], `resource_arns` = [resource], and `context_entries` with `ContextKeyTypeEnum::String`. Read `evaluation_results[0].eval_decision`.
  - Probes:
    - `ecs list_clusters` with `max_results(1)`;
    - `ssm get_parameters_by_path("/triple-c/environments")` with `max_results(1)`;
    - `cfn describe_stacks` with no name, first page only;
    - `secretsmanager list_secrets` with `max_results(1)`.
- [ ] **Step 4: Run** `cargo test aws::`. All pass.
- [ ] **Step 5: Commit** `feat(aws): tiered permission check with simulation and probe fallback`.

### Task 6: Tauri commands

**Files:**
- Create: `app/src-tauri/src/commands/host_commands.rs`
- Modify: `app/src-tauri/src/commands/mod.rs`, `app/src-tauri/src/lib.rs` (`generate_handler!`), `app/src-tauri/capabilities/default.json` (or the census location CLAUDE.md names), and `app/src/lib/tauri-commands.ts`, plus the `PermissionReport`, `CallerIdentity` and `AwsError` TS types in `types.ts`

**Interfaces:**
- Consumes: Tasks 1–5.
- Produces these commands. TS wrappers are camelCase with the same arguments.
  ```rust
  add_aws_profile_connection(name: String, profile: String, default_region: String) -> Result<HostConnection, AwsError>
  add_aws_keys_connection(name: String, access_key_id: String, secret_access_key: String, session_token: Option<String>, default_region: String) -> Result<HostConnection, AwsError>
  update_aws_connection(id: String, name: String, default_region: String) -> Result<HostConnection, AwsError>
  remove_host_connection(id: String) -> Result<AppSettings, String>
  aws_connection_identity(id: String) -> Result<CallerIdentity, AwsError>
  aws_connection_sso_login(id: String) -> Result<(), String>          // Profile only; wraps aws_commands::run_sso_login
  check_aws_permissions(id: String, region: Option<String>) -> Result<PermissionReport, AwsError>   // None → default_region
  aws_policy_document(tier: Tier) -> String
  ```
  - **Add (both kinds):** validate → call `GetCallerIdentity` with the new credentials → only on success store the keys and save settings. Return `AwsError`, and save nothing, if the identity call fails.
    - Exception: `SsoExpired` for a profile still saves. The UI then shows "Sign in".
  - **Keys:** if saving settings fails after storing the keys, delete the keyring entry.
  - **Remove:** delete the keyring entry first; a missing entry is fine. Then drop the connection from settings.

- [ ] **Step 1: Write failing tests** for the pure parts, in `host_commands.rs` tests:
  - `adding_saves_nothing_when_identity_fails`: inject an identity fn returning `InvalidCredentials`. Afterwards there are no settings changes and no store call. Structure the commands as thin wrappers over `fn add_connection_with(…, identity: impl AsyncFn, store: impl Fn, settings: &SettingsStore)` so this is testable without AWS.
  - `an_sso_expired_profile_is_still_saved`.
  - `keys_are_removed_if_saving_settings_fails`.
  - `capabilities.test.ts` (existing) still passes with the new commands granted.
- [ ] **Step 2: Run them.** Expected: FAIL.
- [ ] **Step 3: Implement and register the commands.** Add the TS wrappers and types.
- [ ] **Step 4: Run** `cargo test host_commands`, `cd app && npx vitest run src/test/capabilities.test.ts`, and `npx tsc --noEmit`. All pass.
- [ ] **Step 5: Commit** `feat(aws): Tauri commands for AWS connections and permission checks`.

### Task 7: Settings → Hosts UI

**Files:**
- Create: `app/src/components/settings/HostsSettings.tsx`, `app/src/components/settings/HostsSettings.test.tsx`
- Modify: `app/src/components/settings/SettingsPanel.tsx` (new `<AccordionSection id="hosts" title="Hosts" defaultOpen={false}>` after "Backends")

**Interfaces:**
- Consumes: the Task 6 TS wrappers and types; `listAwsProfiles` (existing); `useSettings().setAppSettings`.

Behaviour:
- **List.** "Local Docker" is always first and can't be removed. Each AWS connection shows its name, `Profile: <name>` or `Access keys`, and its default region, with **Check**, **Edit** and **Remove** buttons.
- **Add AWS connection form:**
  - name;
  - auth (segmented: Profile | Access keys). Profile is a dropdown from `listAwsProfiles()`; Access keys are three password-type inputs, the session token optional;
  - default region (a text input with a datalist of common regions).
  - Saving shows "Signed in as `<arn>`" on success, or the error. `sso_expired` shows a **Sign in** button that calls `awsConnectionSsoLogin` and then retries.
- **Check.** Runs `checkAwsPermissions`, then shows:
  - the summary as a badge (Environment admin / Workspace user / Insufficient);
  - when the method is `probed`, a note: "Simulation not permitted (<reason>). Read access was probed; write actions are unverified and will be confirmed on first use.";
  - a collapsible per-tier list of the denied and unverified actions;
  - **Copy workspace-user policy** and **Copy admin policy** buttons (`awsPolicyDocument` → clipboard);
  - a one-line caveat that the simulation doesn't account for SCPs, permission boundaries or session policies.
- **Remove** asks for confirmation first.

- [ ] **Step 1: Write failing tests** in `HostsSettings.test.tsx`, mocking `@tauri-apps/api/core` `invoke` as the existing `MarketplaceSettings.test.tsx` does:
  - `shows Local Docker and existing AWS connections`.
  - `adds a profile connection and shows the signed-in identity`.
  - `offers Sign in when the profile's SSO session expired`.
  - `access key inputs are password fields and the session token is optional`.
  - `check shows Insufficient with the denied actions and the copy-policy buttons`.
  - `a probed check says writes are unverified`.
- [ ] **Step 2: Run** `cd app && npx vitest run src/components/settings/HostsSettings.test.tsx`. Expected: FAIL.
- [ ] **Step 3: Implement**, following `MarketplaceSettings.tsx` for structure and styling.
- [ ] **Step 4: Run** `npx vitest run` (the whole suite) and `npx tsc --noEmit`. Both pass.
- [ ] **Step 5: Commit** `feat(aws): Settings → Hosts with AWS connections and permission check`.

### Task 8: Live verification against a real account, and docs

**Files:**
- Create: `app/src-tauri/src/aws/live_tests.rs` (`#[ignore]`, env-gated like `host/ssm/live_tests.rs`)
- Modify: `CLAUDE.md` (a new `aws/` module paragraph next to `host/`), `docs/superpowers/specs/2026-11-13-aws-ecs-host-design.md` (mark phase 3 done and record the verification results)

The live tests need `TRIPLEC_AWS_E2E_PROFILE` (admin creds for a disposable account) and `TRIPLEC_AWS_E2E_REGION`. Each test creates IAM roles named `triple-c-e2e-<uuid>`, each assumable by the caller's account, with the policy under test inline, and deletes them in a guard on exit. IAM is free.

- [ ] **Step 1: Write the ignored tests:**
  - `a_role_with_the_workspace_policy_checks_as_workspace_user`: create the role with `workspace-user.json`, tag it `triple-c:owner=<simulated userid>`, and run `check_permissions` with an `SdkPermissionApi` whose principal is that role ARN. Expect `WorkspaceUser`, with no `Denied` in the workspace tier.
  - `a_role_with_both_policies_checks_as_environment_admin`.
  - `a_role_with_no_policy_checks_as_insufficient_and_lists_every_action`.
  - `a_role_without_simulate_permission_falls_back_to_probes`: assume the role (STS `AssumeRole`) and run the full `check_permissions` *as* it.
  - `the_owner_condition_denies_another_owners_task`: simulate `ecs:StopTask` with `aws:ResourceTag/triple-c:owner` = a different id, and expect Denied. This is the spec's "most important rule".
- [ ] **Step 2: Run them** with fresh credentials from the user: `cargo test aws::live_tests -- --ignored --test-threads=1`. All pass. Fix the policy JSON if any statement doesn't evaluate as the spec intends, and re-run Task 4's tests.
- [ ] **Step 3: Confirm no `triple-c-e2e-*` roles remain:** `aws iam list-roles --query "Roles[?starts_with(RoleName,'triple-c-e2e-')]"` returns `[]`.
- [ ] **Step 4: Update the docs** and commit `test(aws): live IAM verification of the tier policies; docs`.
