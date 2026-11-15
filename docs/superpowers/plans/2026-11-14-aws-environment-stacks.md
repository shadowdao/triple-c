# AWS Environment + Onboarding Stacks (Phase 4) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** From Settings → Hosts, a user picks a region on an AWS connection. They see the Triple-C environments there, including half-deployed and outdated ones. They can deploy a new environment from the CloudFormation template that ships with the app, update or repair one, and join one. Joining onboards them automatically when they are allowed to, and otherwise produces an exact onboarding request for an admin. Strict tenancy is the only mode.

**Architecture:**
- Three CloudFormation templates ship in `aws/`, compiled in with `include_str!`:
  - the **environment** template;
  - the per-user **onboarding** template;
  - a one-off **permissions boundary** template.
- `aws/templates.rs` owns names, versions and stack parameters.
- `aws/stacks.rs` creates, updates, deletes and watches stacks behind a mockable trait.
- `aws/environments.rs` detects environments by merging the SSM parameters with the tagged stacks, and also reports the boundary and the caller's onboarding state.
- Commands live in a new `commands/environment_commands.rs`. Stack progress streams to the UI as Tauri events.
- The UI is an Environments panel on each AWS connection in `HostsSettings`.
- Static tests keep the templates and the environment-admin policy in step. An ignored live test deploys everything as a role that holds *only* the admin policies, then tears it down.

**Tech Stack:**
- Rust and Tauri v2, with the existing `aws-sdk-cloudformation`, `aws-sdk-iam`, `aws-sdk-ssm` and `sha2`. No new crates. In particular, no `aws-sdk-ec2` (compile time): existing-VPC mode takes typed IDs.
- React, TypeScript and vitest.

**Spec:** `docs/superpowers/specs/2026-11-13-aws-ecs-host-design.md`. Relevant sections:
- "User flow" §2;
- "Connectivity" (network);
- "Storage";
- "Secrets and uploads";
- "Multi-user accounts" (strict tenancy);
- "Shipped AWS artefacts";
- "Phase 3: policy verification and rulings".

**Branch:** `feat/aws-environments` off `main` (3bc0e9f).

## Decisions this plan makes

These go in the spec in Task 8. Each is a deviation or a resolved gap, so the reviewer should read them first.

- **D1. Templates are JSON**: `aws/triple-c-environment.json`, `aws/triple-c-user-onboard.json` and `aws/triple-c-permissions-boundary.json`.
  - CloudFormation takes JSON natively, and the app already depends on `serde_json`.
  - There is no YAML crate in the tree, and the drift tests must parse the templates.
- **D2. Strict tenancy gives each user their own EFS file system**, created by their onboarding stack. The spec had one file system per environment.
  - In the single-file-system design, the file system policy would have to bind each access point to its owner's task role. EFS can't express that: `ClientMount` sees the access point's ARN, not its tags, and access point IDs are random.
  - With one file system per user, its policy allows only that user's task role. An empty file system costs nothing.
  - The environment keeps the shared parts: cluster, security group, KMS key, log group and network.
- **D3. No ECR in v1.** Tasks pull `ghcr.io/shadowdao/triple-c-sandbox` straight from GHCR, which the spec allows ("the default when the environment has no ECR mirror"). An ECR mirror is later work.
- **D4. Network modes** (template parameter `NetworkMode`):
  - `public` (**default**): creates a VPC with two public subnets. Tasks get a public IP behind a security group with **no inbound rules**. There is no NAT gateway, so it costs $0.
  - `private-nat`: creates a VPC with two public and two private subnets and one NAT gateway. Tasks run in the private subnets. The UI says this costs about $33/month plus data.
  - `existing`: `VpcId`, `SubnetA`, `SubnetB` (in different AZs) and `AssignPublicIp`.
  - The spec's "private subnets" becomes one option, not the only one. The no-inbound rule holds in every mode.
- **D5. An admin request is a template file plus a CLI command, not a console link.** CloudFormation quick-create links need the template at an S3 URL, and Triple-C has no public bucket.
  - The request offers to save the template, using a Rust-side save dialog like `pick_save_path`.
  - It shows the exact parameters and tags.
  - It gives a copyable `aws cloudformation deploy …` command.
- **D6. The app deletes only failed or half-deployed stacks.** Those are `ROLLBACK_COMPLETE`, `ROLLBACK_FAILED`, `CREATE_FAILED` and `DELETE_FAILED`. Deleting a working environment or onboarding stack stays in the console for v1, which is the irreversible case.
- **D7. The admin tier is two policy files**, `environment-admin.json` and `environment-admin-network.json`. Both are required for the admin tier. The VPC actions don't fit in one 6,144-character managed policy (the admin file is already about 5,530). `aws_policy_document` returns every file for a tier.
- **D8. The workspace-user policy is unchanged.** Detection uses only reads it already grants:
  - `ssm:GetParameter*` on `/triple-c/*`, which covers `GetParametersByPath` on `/triple-c/environments`;
  - `cloudformation:DescribeStacks` and `ListStacks`;
  - `iam:GetRole` on `role/triple-c-*`.

  It stays about 30 characters under the size limit. The split the phase-3 notes called for moves to phase 5, which adds actions.

## Global Constraints

- Names. These are pure functions in `aws/templates.rs`, and nothing else builds these strings.
  - **Environment name**: `^[a-z][a-z0-9-]{0,19}$`, not ending in `-`, with no `--`.
  - **Environment stack**: `triple-c-<env>`. **Parameter**: `/triple-c/environments/<env>`.
  - **Owner key**: the first 12 hex characters of `sha256(aws:userid)`. A `UserId` contains `:`, which IAM role names reject.
  - **Onboarding stack**: `triple-c-<env>-user-<ownerkey>`.
  - **Per-user roles**: `triple-c-<env>-<ownerkey>-task` and `triple-c-<env>-<ownerkey>-exec`. The longest is 47 characters, under the 64 limit.
  - **Boundary stack**: `triple-c-permissions-boundary`. It creates the managed policy `triple-c-permissions-boundary`, with no path.
- Tags.
  - Every taggable resource in the environment template carries `triple-c:component=environment` and `triple-c:environment=<env>`.
  - In the onboarding template, every taggable resource carries `triple-c:component=user-onboarding`, `triple-c:environment=<env>` and `triple-c:owner=<OwnerId>`.
  - Stacks are tagged the same way, plus `triple-c:template-version=<n>`.
  - Workspace users never set `triple-c:component`; phase 3 depends on that.
- Every IAM role a template creates has `PermissionsBoundary` set to `arn:aws:iam::${AWS::AccountId}:policy/triple-c-permissions-boundary`. Trust is `ecs-tasks.amazonaws.com` with `aws:SourceAccount = ${AWS::AccountId}`.
- No stack uses a service role (`RoleARN`). The admin policy denies one, so CloudFormation acts with the caller's own credentials.
- The security group has **no** `SecurityGroupIngress`, inline or as a separate resource.
- The KMS key policy delegates to IAM (`arn:aws:iam::${AWS::AccountId}:root`, `kms:*`). The key has rotation on and `PendingWindowInDays: 7`.
- The environment parameter's value is JSON, `EnvironmentInfo` schema 1 (Task 1 lists the fields). It is **untrusted input**. Anyone holding `ssm:PutParameter` on `/triple-c/*` can write it, so the app validates every field before use and caps the value at 4 KiB.
- Template versions are `ENVIRONMENT_TEMPLATE_VERSION: u32 = 1` and `ONBOARD_TEMPLATE_VERSION: u32 = 1`. Each is written in three places, and a test checks they agree: the template's `Metadata.TripleC.TemplateVersion`, the stack tag, and (for the environment) the parameter JSON.
- `TemplateBody` is limited to 51,200 bytes, and a test checks it.
- AWS calls use `aws::connection::sdk_config`, never `aws_config::defaults()`.
- Every new command needs `#[tauri::command]`, a `generate_handler!` entry and an `allow-*` grant in `capabilities/default.json`. `src/test/capabilities.test.ts` must pass.
- When a policy JSON changes, its checks in `policies.rs` and `aws/README.md` change in the same commit. Every policy fits in 6,144 characters with whitespace removed.
- Live AWS spending is capped by the user at $10 for this work, and every resource is cleaned up. Live tests use the scratchpad `awsx` wrapper, never the container's own `AWS_*` env.

## Review Focus

1. **A half-deployed stack**: `CREATE_IN_PROGRESS`, `ROLLBACK_COMPLETE`, `UPDATE_ROLLBACK_COMPLETE` or `DELETE_FAILED`.
   - Show it with its state.
   - Offer Delete only for the D6 states.
   - Show an in-progress one with live progress, not as "Failed".
   - `UPDATE_ROLLBACK_COMPLETE` is usable, so offer Update.
   - Test: `half_deployed_stacks_get_the_right_actions` (Task 5).
2. **A record and its stack disagree.**
   - A parameter with no stack is shown as "Stack missing" and is not joinable.
   - A tagged stack with no parameter is shown as "Record missing — update to repair".
   - Test: `a_parameter_without_a_stack_and_a_stack_without_a_parameter_are_both_listed` (Task 5).
3. **Update when nothing changed.** CloudFormation's `ValidationError` "No updates are to be performed." means "Already up to date", not a failure. Test: `an_update_with_no_changes_is_already_up_to_date` (Task 4).
4. **A hostile or garbled environment record**: bad JSON, 64 KiB of text, an ARN in another account or region, or a name that doesn't match its path. It is reported as "Unreadable environment record" and never used to build a request. Test: `untrusted_environment_records_are_rejected` (Task 5).
5. **The app closes mid-deploy.** The stack carries on. On the next detection, the in-progress stack is shown, and its progress can be followed from its existing events without starting a second deploy. Test: `watching_a_stack_already_in_progress_reports_its_existing_events_once` (Task 4).

---

### Task 1: Environment template and `aws/templates.rs`

**Files:**
- Create: `aws/triple-c-environment.json`
- Create: `app/src-tauri/src/aws/templates.rs`, with tests in `app/src-tauri/src/aws/templates/tests.rs`
- Modify: `app/src-tauri/src/aws/mod.rs`

**Interfaces:**
- Produces, in `aws::templates`:
  ```rust
  pub const ENVIRONMENT_TEMPLATE_VERSION: u32 = 1;
  pub const ONBOARD_TEMPLATE_VERSION: u32 = 1;
  pub const BOUNDARY_POLICY_NAME: &str = "triple-c-permissions-boundary"; // re-export of policies::PERMISSIONS_BOUNDARY_NAME
  pub const BOUNDARY_STACK_NAME: &str = "triple-c-permissions-boundary";
  #[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)] #[serde(rename_all = "snake_case")]
  pub enum TemplateKind { Environment, UserOnboard, PermissionsBoundary }
  pub fn template_body(kind: TemplateKind) -> &'static str;
  pub fn template_file_name(kind: TemplateKind) -> &'static str;   // "triple-c-environment.json", …
  pub fn validate_environment_name(name: &str) -> Result<String, String>;
  pub fn environment_stack_name(env: &str) -> String;
  pub fn environment_parameter_name(env: &str) -> String;
  pub fn owner_key(user_id: &str) -> String;                        // 12 lowercase hex
  pub fn onboarding_stack_name(env: &str, owner_key: &str) -> String;
  pub fn user_role_names(env: &str, owner_key: &str) -> (String, String); // (task, exec)
  #[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)] #[serde(tag = "mode", rename_all = "kebab-case")]
  pub enum NetworkChoice { Public, PrivateNat, Existing { vpc_id: String, subnet_a: String, subnet_b: String, assign_public_ip: bool } }
  pub fn environment_parameters(env: &str, network: &NetworkChoice) -> Result<Vec<(String, String)>, String>; // validates vpc-/subnet- ids
  pub fn stack_tags(kind: TemplateKind, env: Option<&str>, owner_id: Option<&str>) -> Vec<(String, String)>;
  ```
- Produces the `EnvironmentInfo` JSON shape. Task 5 parses it, and the template writes it with `Fn::Sub`:
  `{"schema":1,"name","template_version","cluster_arn","security_group_id","subnet_ids":[2],"assign_public_ip":bool,"kms_key_arn","log_group","network_mode","tenancy":"strict"}`.
- Template parameters: `EnvironmentName`, `NetworkMode` (`public|private-nat|existing`), `VpcId`, `SubnetA`, `SubnetB`, `AssignPublicIp` (`true|false`) and `LogRetentionDays` (default 30).
- Exports, which Task 2's onboarding template imports: `triple-c-<env>-ClusterArn`, `-SecurityGroupId`, `-SubnetA`, `-SubnetB`, `-KmsKeyArn` and `-LogGroupName`.
- Resources, always created: the ECS cluster (`triple-c-<env>`, `FARGATE` and `FARGATE_SPOT` capacity providers), the security group (egress all, no ingress), the KMS key and its alias `alias/triple-c-<env>`, the log group `/triple-c/<env>`, and the SSM parameter.
- Resources, conditional on network mode: the VPC, IGW, subnets, route tables, and the NAT gateway and EIP.

- [ ] **Step 1: Write the failing tests** in `templates/tests.rs`:
  - `environment_names_are_validated`: accepts `dev` and `team-a1`. Rejects `""`, `Dev`, `1dev`, `dev-`, `a--b`, a 21-character name, and `dev/x`.
  - `owner_keys_are_stable_and_role_safe`: `owner_key("AROAEXAMPLE:jdoe@example.com")` is 12 characters from `[0-9a-f]`, is the same on every call, and differs from `owner_key("AROAEXAMPLE:jdoe2@example.com")`. `user_role_names("abcdefghijklmnopqrst", &key)` are each at most 64 characters.
  - `the_environment_template_parses_and_fits`: `template_body(Environment)` parses as JSON and is under 51,200 bytes. `Metadata.TripleC.TemplateVersion == ENVIRONMENT_TEMPLATE_VERSION`.
  - `the_security_group_has_no_ingress`: there is no `AWS::EC2::SecurityGroupIngress` resource and no `SecurityGroupIngress` property.
  - `every_taggable_environment_resource_is_tagged_component_environment`: every resource with a `Tags` property has `triple-c:component=environment` and `triple-c:environment` (`Ref EnvironmentName`). The ECS cluster, KMS key, log group, security group, VPC pieces and the SSM parameter all count; resource types that can't take tags are excluded by an explicit list in the test.
  - `the_environment_record_renders_to_valid_json`: substitute fake values into the parameter's `Fn::Sub` string, with the network-mode conditions resolved each way. The result parses into the field list above, with `schema == 1` and `template_version == 1`.
  - `exports_are_named_for_the_environment`: all six export names are `Fn::Sub` of `triple-c-${EnvironmentName}-<Suffix>`.
  - `environment_parameters_cover_every_network_mode`: `Existing` with `vpc-0abc`/`subnet-1`/`subnet-2` gives all six parameters. A bad ID (`vpc-;`) is an error. `Public` sets `NetworkMode=public` and leaves the existing-VPC parameters at their template defaults.
- [ ] **Step 2: Run** `cargo test aws::templates`. Expected: FAIL (module missing).
- [ ] **Step 3: Write `aws/triple-c-environment.json` and implement `templates.rs`.**
  - Use the long-form intrinsics; JSON has no short form.
  - The record's JSON booleans come from `Fn::If` returning the literal text `true`/`false` inside the `Fn::Sub`.
  - `subnet_ids` comes from conditions selecting the created or the existing subnets.
- [ ] **Step 4: Run** `cargo test aws::templates`. Expected: PASS.
- [ ] **Step 5: Validate the template against CloudFormation.** Run `awsx aws cloudformation validate-template --template-body file://aws/triple-c-environment.json --region us-west-2`. Expected: no error. This is free and creates nothing.
- [ ] **Step 6: Commit** with the message `feat(aws): environment CloudFormation template and stack naming`.

### Task 2: Onboarding and boundary templates

**Files:**
- Create: `aws/triple-c-user-onboard.json`
- Create: `aws/triple-c-permissions-boundary.json`
- Modify: `app/src-tauri/src/aws/templates.rs`, `templates/tests.rs`

**Interfaces:**
- Consumes: Task 1's names, exports and tags.
- Produces:
  ```rust
  pub fn onboarding_parameters(env: &str, owner_id: &str) -> Vec<(String, String)>; // EnvironmentName, OwnerId, OwnerKey (= owner_key(owner_id)), BedrockOnTaskRole=false
  ```
- The onboarding template's parameters are `EnvironmentName`, `OwnerId`, `OwnerKey` and `BedrockOnTaskRole` (`false` by default).
- It creates the following resources.
  - **Task role** `triple-c-<env>-<key>-task`. It may use:
    - `ssmmessages:CreateControlChannel/CreateDataChannel/OpenControlChannel/OpenDataChannel` on `*`;
    - `logs:CreateLogStream/PutLogEvents` on the environment's log group;
    - `elasticfilesystem:ClientMount/ClientWrite` on *this user's* file system;
    - Bedrock invoke, only under the `BedrockOnTaskRole` condition.
  - **Execution role** `triple-c-<env>-<key>-exec`. It may use:
    - `secretsmanager:GetSecretValue` on `triple-c/*` where `aws:ResourceTag/triple-c:owner` equals the `OwnerId` parameter;
    - `kms:Decrypt` on the environment key via `secretsmanager.${AWS::Region}.amazonaws.com`;
    - `logs:CreateLogStream/PutLogEvents` on the environment's log group.
  - **The user's EFS file system.** It is encrypted, `PerformanceMode: generalPurpose`, `ThroughputMode: elastic`, and has two mount targets (the environment's subnets and security group).
  - **That file system's policy:**
    - Deny `aws:SecureTransport=false`.
    - Deny any request where `elasticfilesystem:AccessPointArn` is null.
    - Allow `ClientMount` and `ClientWrite` only to the task role's ARN.
  - **The security group rule.** EFS needs inbound NFS (2049) **from the environment security group to the mount targets**. This must not open ingress on the tasks. So the onboarding stack creates its own mount-target security group `triple-c-<env>-<key>-efs`, with ingress 2049 only from the environment security group, and the environment group stays ingress-free.
- The boundary template is one `AWS::IAM::ManagedPolicy` named `triple-c-permissions-boundary`. Its `PolicyDocument` equals `aws/policies/permissions-boundary.json`.

- [ ] **Step 1: Write the failing tests:**
  - `every_onboarding_role_carries_the_boundary_and_owner_tag`: every `AWS::IAM::Role` has `PermissionsBoundary` equal to the `Fn::Sub` of the boundary ARN, and the tag `triple-c:owner = Ref OwnerId`. `RoleName` is `Fn::Sub` of `triple-c-${EnvironmentName}-${OwnerKey}-task` or `-exec`.
  - `role_trust_is_ecs_tasks_in_this_account_only`: the trust principal is `ecs-tasks.amazonaws.com` with a `StringEquals aws:SourceAccount` condition.
  - `the_users_file_system_admits_only_their_task_role_through_an_access_point`: the file system policy has the two Denies and one Allow whose principal is `Fn::GetAtt TaskRole.Arn`.
  - `the_environment_security_group_gains_no_ingress`: the only ingress anywhere in the onboarding template is on the `-efs` group, port 2049, sourced from `Fn::ImportValue triple-c-${EnvironmentName}-SecurityGroupId`.
  - `exec_role_reads_only_its_owners_secrets`: the `GetSecretValue` statement is conditioned on `aws:ResourceTag/triple-c:owner` = `Ref OwnerId`.
  - `the_boundary_template_matches_the_shipped_boundary`: parse both. The template's `PolicyDocument` equals `permissions-boundary.json` as JSON values, and `ManagedPolicyName == BOUNDARY_POLICY_NAME`.
  - `every_onboarding_role_stays_inside_the_boundary`: every action each role is granted is allowed by `permissions-boundary.json`, under the Task 3 evaluator. **Write this test in Task 3** once the evaluator is shared, and leave it as a TODO here.
- [ ] **Step 2: Run** `cargo test aws::templates`. Expected: FAIL.
- [ ] **Step 3: Write both templates and `onboarding_parameters`.**
- [ ] **Step 4: Run the tests** and `awsx aws cloudformation validate-template` on both files. Expected: PASS, and both valid.
- [ ] **Step 5: Commit** with the message `feat(aws): per-user onboarding and permissions-boundary templates`.

### Task 3: The admin policies cover the templates

**Files:**
- Create: `aws/policies/environment-admin-network.json`
- Modify: `aws/policies/environment-admin.json`
- Modify: `app/src-tauri/src/aws/policies.rs`
- Modify: `app/src-tauri/src/aws/permissions.rs` (one tier is now several files)
- Modify: `app/src-tauri/src/commands/host_commands.rs` (`aws_policy_document`)
- Modify: `app/src/lib/tauri-commands.ts`, `app/src/lib/types.ts`, `components/settings/HostsSettings.tsx` (show and copy each file)
- Modify: `aws/README.md`
- Move: the test-only IAM evaluator from `policies.rs` `mod tests` into `app/src-tauri/src/aws/policy_eval.rs` (`#[cfg(test)]`), so the template tests can use it. Its behaviour is unchanged.

**Interfaces:**
- Produces:
  ```rust
  pub fn policy_files(tier: Tier) -> &'static [(&'static str, &'static str)]; // (file name, json); admin = [environment-admin.json, environment-admin-network.json]
  #[tauri::command] pub fn aws_policy_document(tier: Tier) -> Vec<PolicyFile>; // PolicyFile { name: String, json: String }
  ```
  `policy_json` is removed. Its callers use `policy_files`, and the simulator evaluates all of a tier's files together.
- A table in `templates/tests.rs` maps each template resource type to the IAM actions CloudFormation calls to create, tag, read, update and delete it. For example, `AWS::KMS::Key` maps to `kms:CreateKey`, `kms:TagResource`, `kms:DescribeKey`, `kms:GetKeyPolicy`, `kms:PutKeyPolicy`, `kms:EnableKeyRotation`, `kms:GetKeyRotationStatus`, `kms:ListResourceTags` and `kms:ScheduleKeyDeletion`.
  - The table is the hypothesis. The Task 8 live test is the proof, and any action it finds missing goes into both the table and the policy.

- [ ] **Step 1: Write the failing tests:**
  - `the_admin_tier_can_deploy_every_template_resource`: for each resource in the environment and onboarding templates, every mapped action is allowed by the admin tier's files. Use the resource's tags (`triple-c:component` present) and resource names from Task 1's naming functions.
  - `every_onboarding_role_stays_inside_the_boundary` (from Task 2).
  - The existing `every_policy_fits_a_managed_policy` covers the new file. The existing check-table drift tests now require a check for every new admin action.
  - `the_admin_still_cannot_touch_untagged_resources`: `ec2:DeleteVpc`, `ec2:DeleteSubnet` and `ec2:DeleteNatGateway` on a resource without `triple-c:component` are denied.
- [ ] **Step 2: Run** `cargo test aws::`. Expected: FAIL.
- [ ] **Step 3: Add the actions.**
  - VPC, subnet, IGW, route, route-table, EIP and NAT go in `environment-admin-network.json`.
    - Creation requires the `aws:RequestTag/triple-c:component` tag.
    - Mutation and deletion require `aws:ResourceTag/triple-c:component`.
    - `ec2:CreateTags` is allowed only with `ec2:CreateAction` set to one of those create calls.
  - Everything else goes in `environment-admin.json`. That covers the extra KMS, ECS, logs, EFS and SSM read and tag actions, plus `iam:GetPolicy` on the boundary ARN, which detection uses.
  - Add a check-table row for each new action, and update the README tier sections.
- [ ] **Step 4: Run** `cargo test aws::` and `npx vitest run src/components/settings`. Expected: PASS.
- [ ] **Step 5: Commit** with the message `feat(aws): admin policies cover the environment and onboarding stacks`.

### Task 4: Stack operations (`aws/stacks.rs`)

**Files:**
- Create: `app/src-tauri/src/aws/stacks.rs`, with tests in `aws/stacks/tests.rs`
- Modify: `app/src-tauri/src/aws/error.rs` if a new variant is needed (`AlreadyExists { name }`)

**Interfaces:**
- Produces:
  ```rust
  #[async_trait] pub trait StackApi: Send + Sync {
      async fn create(&self, name: &str, body: &str, params: &[(String, String)], tags: &[(String, String)]) -> Result<String, AwsError>; // stack id; CAPABILITY_NAMED_IAM; OnFailure=ROLLBACK
      async fn update(&self, name: &str, body: &str, keep_params: &[String], tags: &[(String, String)]) -> Result<UpdateOutcome, AwsError>; // UsePreviousValue for keep_params
      async fn delete(&self, name: &str) -> Result<(), AwsError>;
      async fn describe(&self, name: &str) -> Result<Option<StackSummary>, AwsError>; // None when it does not exist
      async fn events(&self, name: &str) -> Result<Vec<StackEvent>, AwsError>;       // newest first, as the API returns
      async fn list_tagged(&self, key: &str, value: &str) -> Result<Vec<StackSummary>, AwsError>; // DescribeStacks paged, filtered by tag client-side
  }
  pub struct SdkStackApi { … } impl SdkStackApi { pub fn new(cfg: &SdkConfig) -> Self }
  pub enum UpdateOutcome { Started, AlreadyUpToDate }
  #[derive(Serialize, Clone, Debug, PartialEq)] pub struct StackSummary { name, id, status: String, status_reason: Option<String>, tags: Vec<(String,String)>, template_version: Option<u32> } // from tag triple-c:template-version
  #[derive(Serialize, Clone, Debug, PartialEq)] pub struct StackEvent { id, timestamp: String, logical_id, resource_type, status, reason: Option<String> }
  pub fn is_terminal(status: &str) -> bool;          // *_COMPLETE or *_FAILED, except *_IN_PROGRESS
  pub fn is_success(status: &str) -> bool;           // CREATE_COMPLETE, UPDATE_COMPLETE, DELETE_COMPLETE, IMPORT_COMPLETE
  pub fn is_deletable_failure(status: &str) -> bool; // D6 list
  pub async fn watch<A: StackApi>(api: &A, name: &str, poll: Duration, deadline: Duration, on_events: impl FnMut(&[StackEvent])) -> Result<StackSummary, AwsError>;
  ```
  - `watch` reports each event once, in chronological order. Events that already existed when it started are reported in the first batch. It returns when `describe` is terminal (or `None` after a delete), and returns `Err(Timeout)` after `deadline`.
  - The real poll interval is 3 s and the deadline is 60 min. Tests pass `Duration::ZERO` and a fake clock is not needed.

- [ ] **Step 1: Write the failing tests** with a scripted fake `StackApi`:
  - `an_update_with_no_changes_is_already_up_to_date`: the SDK `ValidationError` with the message `No updates are to be performed.` maps to `UpdateOutcome::AlreadyUpToDate`. Test the mapping function `update_error_outcome(&str)` directly.
  - `creating_a_stack_that_exists_is_already_exists`: `AlreadyExistsException` maps to `AwsError::AlreadyExists { name }`.
  - `watching_a_stack_already_in_progress_reports_its_existing_events_once`: polls return events [e1], then [e1, e2], then [e1, e2, e3] with a terminal status. The callback sees e1, then e2, then e3, each exactly once and in time order.
  - `watch_ends_on_delete_complete_or_a_vanished_stack`.
  - `statuses_are_classified`: `ROLLBACK_COMPLETE` is terminal, not a success, and deletable. `UPDATE_ROLLBACK_COMPLETE` is terminal, not a success, and not deletable. `CREATE_IN_PROGRESS` is not terminal.
- [ ] **Step 2: Run** `cargo test aws::stacks`. Expected: FAIL.
- [ ] **Step 3: Implement** `stacks.rs`, using `aws-sdk-cloudformation`. Classify errors through `classify_sdk_error`.
- [ ] **Step 4: Run** `cargo test aws::stacks`. Expected: PASS.
- [ ] **Step 5: Commit** with the message `feat(aws): CloudFormation stack operations and progress watching`.

### Task 5: Detection (`aws/environments.rs`)

**Files:**
- Create: `app/src-tauri/src/aws/environments.rs`, with tests in `aws/environments/tests.rs`

**Interfaces:**
- Consumes: `templates::*`, `stacks::{StackApi, StackSummary, is_deletable_failure}`, and `identity::CallerIdentity`.
- Produces:
  ```rust
  #[derive(Deserialize, Serialize, Clone, Debug, PartialEq)] pub struct EnvironmentInfo { schema: u32, name, template_version: u32, cluster_arn, security_group_id, subnet_ids: Vec<String>, assign_public_ip: bool, kms_key_arn, log_group, network_mode, tenancy }
  pub fn parse_environment_record(path_name: &str, value: &str, account: &str, region: &str) -> Result<EnvironmentInfo, String>;
  #[derive(Serialize, Clone, Debug, PartialEq)] #[serde(tag = "state", rename_all = "snake_case")]
  pub enum EnvironmentState { Ready, Deploying, UpdateAvailable, Failed { reason: Option<String> }, StackMissing, RecordMissing, UnreadableRecord { reason: String } }
  #[derive(Serialize, Clone, Debug, PartialEq)] pub enum EnvironmentAction { Join, Update, Delete, Watch } // snake_case
  #[derive(Serialize, Clone, Debug, PartialEq)] #[serde(tag = "state", rename_all = "snake_case")]
  pub enum OnboardingState { Joined, NotJoined, InProgress, Failed { reason: Option<String> } }
  #[derive(Serialize, Clone, Debug, PartialEq)] pub struct DetectedEnvironment { name, stack_status: Option<String>, template_version: Option<u32>, state: EnvironmentState, actions: Vec<EnvironmentAction>, info: Option<EnvironmentInfo>, onboarding: Option<OnboardingState> }
  #[derive(Serialize, Clone, Debug, PartialEq)] #[serde(rename_all = "snake_case")] pub enum BoundaryState { Present, Missing, Unknown }
  #[derive(Serialize, Clone, Debug)] pub struct EnvironmentScan { region, account, owner_id, owner_key, boundary: BoundaryState, environments: Vec<DetectedEnvironment> }
  pub fn merge(records: Vec<(String, Result<EnvironmentInfo, String>)>, stacks: Vec<StackSummary>, current_version: u32) -> Vec<DetectedEnvironment>; // sorted by name; onboarding None
  #[async_trait] pub trait DetectApi: Send + Sync { async fn environment_records(&self) -> Result<Vec<(String, String)>, AwsError>; async fn boundary_exists(&self, account: &str) -> Result<Option<bool>, AwsError>; async fn role_exists(&self, name: &str) -> Result<Option<bool>, AwsError>; } // None = denied
  pub async fn scan<D: DetectApi, S: StackApi>(d: &D, s: &S, who: &CallerIdentity, region: &str) -> Result<EnvironmentScan, AwsError>;
  ```
- Rules:
  - The `Join` action is offered only for `Ready` and `UpdateAvailable`.
  - `UpdateAvailable` means the stack's `template_version` is below the current version.
  - `UPDATE_ROLLBACK_COMPLETE` is `Ready`, with a note that the last update rolled back.
  - Onboarding is resolved only for joinable environments:
    1. `describe(onboarding_stack_name)`.
    2. If the stack doesn't exist, `role_exists(task role)`. This catches an admin who deployed it under another stack name.
    3. If that is denied, `NotJoined`.
  - The boundary state comes from `iam:GetPolicy`. If that is denied, the state is `Unknown`.
- `parse_environment_record` rejects:
  - a value over 4 KiB, or one that isn't JSON;
  - `schema != 1`;
  - a `name` different from the path's last segment;
  - any ARN not in `arn:aws:<svc>:<region>:<account>:` for this region and account;
  - `subnet_ids` that isn't exactly 2 `subnet-[0-9a-f]+` IDs, or a security group that isn't `sg-[0-9a-f]+`;
  - `tenancy != "strict"`.

  The rejection reason names the field and never echoes the value.

- [ ] **Step 1: Write the failing tests:**
  - `untrusted_environment_records_are_rejected`: a table of the rejections above, plus one valid record that parses.
  - `a_parameter_without_a_stack_and_a_stack_without_a_parameter_are_both_listed`: gives `StackMissing` with no Join, and `RecordMissing` with Update.
  - `half_deployed_stacks_get_the_right_actions`:
    - `CREATE_IN_PROGRESS` → `Deploying` [Watch];
    - `ROLLBACK_COMPLETE` → `Failed` [Delete];
    - `DELETE_FAILED` → `Failed` [Delete];
    - `UPDATE_ROLLBACK_COMPLETE` → `Ready` [Join, Update];
    - `UPDATE_IN_PROGRESS` → `Deploying` [Watch].
  - `an_older_template_offers_update`: version 0 against current version 1 gives `UpdateAvailable` [Join, Update].
  - `onboarding_is_found_by_stack_then_by_role`: the stack present at `CREATE_COMPLETE` gives `Joined`. No stack but the role present gives `Joined`. No stack and no role gives `NotJoined`. Stack at `ROLLBACK_COMPLETE` gives `Failed`.
  - `a_denied_boundary_lookup_is_unknown_not_missing`.
- [ ] **Step 2: Run** `cargo test aws::environments`. Expected: FAIL.
- [ ] **Step 3: Implement** it, with `SdkDetectApi`: `GetParametersByPath` on `/triple-c/environments` (non-recursive, paged), and `iam:GetPolicy`/`GetRole`. An `AccessDenied` maps to `None`.
- [ ] **Step 4: Run** the tests. Expected: PASS.
- [ ] **Step 5: Commit** with the message `feat(aws): detect environments, the boundary and onboarding`.

### Task 6: Commands, progress events and admin requests

**Files:**
- Create: `app/src-tauri/src/commands/environment_commands.rs`
- Modify: `commands/mod.rs`, `lib.rs` (`generate_handler!`), `capabilities/default.json`
- Modify: `app/src/lib/types.ts`, `app/src/lib/tauri-commands.ts`

**Interfaces:**
- Produces these commands. Each takes `connection_id` and `region` (validated with `validate_supported_region`). An `op_id` is a new UUID.
  ```rust
  detect_aws_environments(connection_id, region) -> Result<EnvironmentScan, AwsError>
  deploy_aws_environment(connection_id, region, name, network: NetworkChoice, app) -> Result<String /*op_id*/, AwsError>   // validates name; refuses if boundary Missing
  update_aws_environment(connection_id, region, name, app) -> Result<String, AwsError>     // keep_params = all template params
  delete_failed_aws_stack(connection_id, region, stack_name, app) -> Result<String, AwsError> // only if describe() status is_deletable_failure and stack name is one of ours
  watch_aws_stack(connection_id, region, stack_name, app) -> Result<String, AwsError>        // attach to an in-progress stack
  onboard_aws_user(connection_id, region, environment, app) -> Result<String, AwsError>      // self-onboarding with the caller's own owner id
  deploy_aws_permissions_boundary(connection_id, region, app) -> Result<String, AwsError>
  aws_admin_request(connection_id, region, kind: TemplateKind, environment: Option<String>, owner_id: Option<String>) -> Result<AdminRequest, AwsError>
  save_aws_template(kind: TemplateKind, window) -> Result<Option<String>, String> // Rust-side save dialog (pattern: file_commands::pick_save_path); writes the embedded body
  ```
  - `AdminRequest { stack_name, template_file_name, parameters: Vec<(String,String)>, tags: Vec<(String,String)>, capabilities: Vec<String>, cli_command: String }`.
  - `cli_command` is `aws cloudformation deploy --region <r> --stack-name <n> --template-file <file> --capabilities CAPABILITY_NAMED_IAM --parameter-overrides K=V … --tags K=V …`, with every value shell-quoted by `commands::terminal_commands::shell_quote_arg`. Make that function `pub(crate)`; don't copy it.
  - Every mutating command spawns `stacks::watch` and returns at once.
  - Progress is emitted on the Tauri event `aws-stack-progress` as `{ op_id, stack_name, events: StackEvent[], done: bool, result?: { ok: bool, status, message? } }`.
  - "Already up to date" is emitted as `done` with `ok: true` and `status: "NO_CHANGES"`.
- Stack names passed in from the frontend are checked to have the form `triple-c-*`, and to be one this app's naming functions can produce. The frontend can't name an arbitrary stack.

- [ ] **Step 1: Write failing unit tests** in the module, against the pure helpers:
  - `admin_requests_are_exact_and_quoted`: an onboarding request for env `dev` and owner `AROAX:o'brien@example.com` contains `OwnerId='AROAX:o'"'"'brien@example.com'`, or whatever the helper's quoting is (assert that it round-trips through `sh -c 'printf %s …'`). Its stack name is `triple-c-dev-user-<key>`.
  - `only_our_stacks_can_be_deleted_or_watched`: `triple-c-dev` and `triple-c-dev-user-0123456789ab` are accepted. `other-stack`, `triple-c-` and `triple-c-dev; x` are rejected.
  - `deploy_refuses_when_the_boundary_is_missing`, using a fake that reports `Missing`.
- [ ] **Step 2: Run** `cargo test environment_commands`. Expected: FAIL.
- [ ] **Step 3: Implement** the commands, register them and grant them. Add the TS types and wrappers: `EnvironmentScan`, `DetectedEnvironment`, `NetworkChoice`, `AdminRequest`, `StackProgress`, `TemplateKind`.
- [ ] **Step 4: Run** `cargo test`, `cargo check` and `npx vitest run src/test/capabilities.test.ts`. Expected: PASS.
- [ ] **Step 5: Commit** with the message `feat(aws): environment commands with streamed stack progress`.

### Task 7: Environments UI

**Files:**
- Create: `app/src/components/settings/hosts/EnvironmentsPanel.tsx`, `DeployEnvironmentModal.tsx`, `AdminRequestModal.tsx`, `StackProgress.tsx`, plus a `*.test.tsx` for each of the first three
- Modify: `app/src/components/settings/HostsSettings.tsx`, which renders `<EnvironmentsPanel connection={c} tier={lastReport?.summary} />` under each connection's permission check

**Interfaces:**
- Consumes: the Task 6 wrappers and the `aws-stack-progress` event.

**Behaviour** (spec §2's table, with phase-3 conventions: `StatusIndicator`, `Modal`, `Button`, `Field`, design tokens):
- **Region select.** Defaults to the connection's `default_region`. Detection runs when the panel opens and when the region changes. A "Detect again" button reruns it.
- **One row per environment**, showing its state, the template version and actions:
  - **Join**:
    - If `onboarding` is `Joined`, it shows "Joined" instead.
    - Otherwise, an environment admin gets `onboardAwsUser` with progress.
    - Anyone else gets the onboarding `AdminRequestModal`. The row then reads "Waiting for an admin to onboard you", and the request is regenerated each time the panel opens.
  - **Update**: runs `updateAwsEnvironment` with progress. "Already up to date" is a success.
  - **Delete**: a typed confirmation of the stack name, then `deleteFailedAwsStack`.
  - **Watch**: `watchAwsStack`.
- **"Deploy a new environment."**
  - It is the primary action when nothing is found and the tier is admin. It is secondary when environments exist.
  - If the tier is not admin and nothing is found, show the explanation, the policy files from `awsPolicyDocument("environment_admin")`, and the environment `AdminRequestModal`.
- **`DeployEnvironmentModal`:**
  - Name, defaulting to `main`, validated with the same rule as Rust (mirror the regex in TS; Rust is the boundary).
  - Network, as a `SegmentedControl` with the D4 modes. Each mode states its cost: `public` "$0, tasks get a public IP, no inbound access"; `private-nat` "about $33/month for the NAT gateway"; `existing` shows three ID fields and the public-IP toggle.
  - If the boundary is `Missing`, the modal first shows a step: "Create the permissions boundary (needs an account administrator)". That step offers `deployAwsPermissionsBoundary`. If that fails with access denied, it shows the boundary `AdminRequestModal`.
  - If the boundary is `Unknown`, deploying is allowed, with a note that it fails if the boundary is missing.
- **`StackProgress`** shows events oldest first, with resource, status and reason. Failure reasons are shown in full. When done, it shows a result line and triggers re-detection.
- **`AdminRequestModal`:** "Save template…" (`saveAwsTemplate`), the parameters and tags table, and the CLI command with a Copy button.

- [ ] **Step 1: Write failing component tests**, mocking `tauri-commands`:
  - `shows deploy as primary when nothing is found and the user is an admin`.
  - `offers an admin request instead of deploy for a workspace user with nothing found`.
  - `a failed stack offers delete only after typing its name`.
  - `join onboards an admin and shows progress until done, then re-detects`.
  - `join for a workspace user opens the onboarding request with the exact command`.
  - `deploy modal blocks on a missing boundary and offers to create it`.
  - `the public network mode is the default and says it costs nothing`.
- [ ] **Step 2: Run** `npx vitest run src/components/settings/hosts`. Expected: FAIL.
- [ ] **Step 3: Implement** the components.
- [ ] **Step 4: Run** `npx vitest run` and `npx tsc --noEmit`. Expected: PASS.
- [ ] **Step 5: Commit** with the message `feat(aws): environments panel — detect, deploy, update, join`.

### Task 8: Live verification, docs and spec

**Files:**
- Modify: `app/src-tauri/src/aws/live_tests.rs`, adding the `environment` tests below
- Modify: `aws/README.md` (an "Environment and onboarding stacks" section, plus results in "Live verification"), `CLAUDE.md` (the `aws/` paragraph: templates, stacks, environments, D2), and the spec (phase 4 done, decisions D1–D8, the EFS isolation change, "To verify" updates)

**Live tests** (ignored; they read `TRIPLEC_AWS_E2E_PROFILE` and `TRIPLEC_AWS_E2E_REGION`; run with `awsx cargo test aws::live_tests::environment -- --ignored --test-threads=1`):
- `an_environment_admin_can_deploy_join_update_and_remove_an_environment`:
  1. As the admin credentials, ensure the boundary exists. If it doesn't, deploy the boundary stack and record that it was created, so it can be removed at the end.
  2. Create the role `triple-c-e2e-envadmin`, trusted by the caller's account. Attach the admin tier's two policies as `triple-c-e2e-*` managed policies, and give it the boundary.
  3. Assume that role. Every following step runs as it, *only*.
  4. Deploy `triple-c-e2e<rand>` in `public` mode, and watch it to `CREATE_COMPLETE`.
  5. `scan` finds it `Ready` with a valid record.
  6. Self-onboard, and see `Joined`.
  7. Update, and get `AlreadyUpToDate`.
  8. Delete the onboarding stack, then the environment stack.
  9. Finally, as admin, delete the e2e role and policies, and the boundary if this test created it.
  - Cleanup runs on failure too, through a guard.
  - Every access-denied error is printed with its action, so a missing policy action shows up as a precise failure.
- `the_private_nat_mode_deploys` (separately ignored): an admin deploy in `private-nat` mode, then `CREATE_COMPLETE`, then delete. This costs about $0.05.

- [ ] **Step 1: Write the tests.**
- [ ] **Step 2: Run them** with the e2e environment and fix any denied action. Each fix goes in the Task 3 policy files, the resource-action table, the check table and the README together, and is re-run until green. Record each action IAM required beyond the table in the README's "Live verification".
- [ ] **Step 3: Confirm cleanup.** List `triple-c-*` stacks, roles, policies and log groups, `triple-c` VPCs, EIPs and EFS file systems, and `alias/triple-c-*` KMS aliases in the region. Expected: none left, with KMS keys only pending deletion. Record the spend.
- [ ] **Step 4: Write the docs and spec updates.** Then run the full `cargo test`, `npx vitest run` and `npx tsc --noEmit`. Expected: PASS.
- [ ] **Step 5: Commit** with the message `test(aws): live environment lifecycle as an environment admin; docs`.
