//! The tier policies against real IAM: `check_permissions` run against
//! throwaway roles that hold the shipped policies, and the few statements
//! whose behaviour only a real request shows (Secrets Manager's KMS calls,
//! the permissions boundary, the admin tier's role and key fences).
//!
//! Ignored; needs credentials for a **disposable** account that may create
//! and delete IAM roles and policies, KMS keys and Secrets Manager secrets:
//!
//! - `TRIPLEC_AWS_E2E_PROFILE` — a profile in the AWS config/credentials
//!   files (read the way a saved connection reads it, so `AWS_*` key
//!   variables are not used; `AWS_SHARED_CREDENTIALS_FILE` and
//!   `AWS_CONFIG_FILE` still choose the files);
//! - `TRIPLEC_AWS_E2E_REGION` — e.g. `us-west-2`.
//!
//! ```sh
//! cd app/src-tauri
//! TRIPLEC_AWS_E2E_PROFILE=… TRIPLEC_AWS_E2E_REGION=us-west-2 \
//!   cargo test aws::live_tests -- --ignored --test-threads=1
//! ```
//!
//! Every resource is named `triple-c-e2e-<id>…` (secrets
//! `triple-c/triple-c-e2e-<id>/…`, because the policies only cover
//! `triple-c/*`), tagged `triple-c-test=phase3`, and deleted when the test
//! ends, pass or fail. KMS keys can only be scheduled for deletion (7 days).
//! IAM is free; a run costs a few cents at most (secrets for minutes, keys
//! pending deletion).

use std::future::Future;
use std::panic::AssertUnwindSafe;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use aws_config::SdkConfig;
use futures_util::FutureExt;

use super::error::{classify_sdk_error, AwsError};
use super::permissions::{
    check_permissions, simulation_request, Method, PermissionApi, PermissionReport,
    SdkPermissionApi, Summary, Verdict,
};
use super::policies::{
    checks, permissions_boundary_json, policy_files, Placeholders, Tier, PERMISSIONS_BOUNDARY_NAME,
};
use super::{caller_identity, sdk_config, sdk_config_for_keys, CallerIdentity};
use crate::models::host_connection::{AwsAuth, HostConnection, HostKind};
use crate::storage::secure::AwsKeys;

const TEST_TAG: (&str, &str) = ("triple-c-test", "phase3");
const PATH: &str = "/triple-c-e2e/";
const SESSION: &str = "triple-c-e2e";

fn var(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("{name} is not set (see this file's header)"))
}

/// What one test created, for the guard to delete.
#[derive(Default)]
struct Created {
    roles: Vec<String>,
    policies: Vec<String>,
    secrets: Vec<String>,
    keys: Vec<String>,
}

struct E2e {
    cfg: SdkConfig,
    iam: aws_sdk_iam::Client,
    sts: aws_sdk_sts::Client,
    secrets: aws_sdk_secretsmanager::Client,
    kms: aws_sdk_kms::Client,
    region: String,
    account: String,
    /// `triple-c-e2e-<random>`: the prefix of everything this test creates.
    prefix: String,
    created: Mutex<Created>,
}

/// A role this test created.
struct Role {
    name: String,
    arn: String,
    /// `RoleId`: an assumed session's `aws:userid` is `<id>:<session>`.
    id: String,
}

impl Role {
    fn session_user_id(&self) -> String {
        format!("{}:{SESSION}", self.id)
    }

    /// The identity `check_permissions` sees for the role itself (not a
    /// session of it): an IAM role ARN, simulated as-is.
    fn identity(&self, account: &str) -> CallerIdentity {
        CallerIdentity {
            account: account.to_string(),
            arn: self.arn.clone(),
            user_id: self.session_user_id(),
        }
    }
}

/// Runs `body` with a fresh fixture and deletes what it created afterwards,
/// whether or not `body` panicked.
async fn e2e<F, Fut>(body: F)
where
    F: FnOnce(Arc<E2e>) -> Fut,
    Fut: Future<Output = ()>,
{
    let region = var("TRIPLEC_AWS_E2E_REGION");
    let conn = HostConnection {
        id: "e2e".into(),
        name: "e2e".into(),
        kind: HostKind::Aws {
            auth: AwsAuth::Profile {
                name: var("TRIPLEC_AWS_E2E_PROFILE"),
            },
            default_region: region.clone(),
        },
    };
    let cfg = sdk_config(&conn, &region).await.expect("sdk config");
    let me = caller_identity(&cfg)
        .await
        .expect("the e2e profile's identity");
    let e = Arc::new(E2e {
        iam: aws_sdk_iam::Client::new(&cfg),
        sts: aws_sdk_sts::Client::new(&cfg),
        secrets: aws_sdk_secretsmanager::Client::new(&cfg),
        kms: aws_sdk_kms::Client::new(&cfg),
        cfg,
        region,
        account: me.account,
        prefix: format!(
            "triple-c-e2e-{}",
            &uuid::Uuid::new_v4().simple().to_string()[..12]
        ),
        created: Mutex::default(),
    });
    let result = AssertUnwindSafe(body(e.clone())).catch_unwind().await;
    e.cleanup().await;
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}

fn iam_tag(k: &str, v: &str) -> aws_sdk_iam::types::Tag {
    aws_sdk_iam::types::Tag::builder()
        .key(k)
        .value(v)
        .build()
        .unwrap()
}

fn sm_tag(k: &str, v: &str) -> aws_sdk_secretsmanager::types::Tag {
    aws_sdk_secretsmanager::types::Tag::builder()
        .key(k)
        .value(v)
        .build()
}

/// `Err(AccessDenied)` for a denied call; panics on anything else.
fn denied<T: std::fmt::Debug, E: std::error::Error + 'static>(what: &str, r: Result<T, E>) {
    match r {
        Err(e) => match classify_sdk_error(what, &e) {
            AwsError::AccessDenied { .. } => {}
            other => panic!("{what}: expected AccessDenied, got {other}"),
        },
        Ok(v) => panic!("{what}: expected AccessDenied, but it succeeded: {v:?}"),
    }
}

fn ok<T, E: std::error::Error + 'static>(what: &str, r: Result<T, E>) -> T {
    r.unwrap_or_else(|e| panic!("{what}: {}", classify_sdk_error(what, &e)))
}

impl E2e {
    fn name(&self, suffix: &str) -> String {
        format!("{}-{suffix}", self.prefix)
    }

    fn secret_name(&self, suffix: &str) -> String {
        format!("triple-c/{}/{suffix}", self.prefix)
    }

    /// A role assumable by this account, under `/triple-c-e2e/` (so its ARN
    /// has a path, as an SSO role's does), optionally bounded.
    async fn role(&self, suffix: &str, boundary: Option<&str>, tags: &[(&str, &str)]) -> Role {
        let name = self.name(suffix);
        let trust = format!(
            r#"{{"Version":"2012-10-17","Statement":[{{"Effect":"Allow","Principal":{{"AWS":"arn:aws:iam::{}:root"}},"Action":"sts:AssumeRole"}}]}}"#,
            self.account
        );
        let mut req = self
            .iam
            .create_role()
            .role_name(&name)
            .path(PATH)
            .assume_role_policy_document(trust)
            .tags(iam_tag(TEST_TAG.0, TEST_TAG.1))
            .set_permissions_boundary(boundary.map(str::to_string));
        for (k, v) in tags {
            req = req.tags(iam_tag(k, v));
        }
        let out = ok("iam:CreateRole", req.send().await);
        self.created.lock().unwrap().roles.push(name.clone());
        let r = out.role().unwrap();
        Role {
            name,
            arn: r.arn().to_string(),
            id: r.role_id().to_string(),
        }
    }

    async fn tag_role(&self, role: &Role, k: &str, v: &str) {
        ok(
            "iam:TagRole",
            self.iam
                .tag_role()
                .role_name(&role.name)
                .tags(iam_tag(k, v))
                .send()
                .await,
        );
    }

    /// A customer managed policy (the tier policies are too big to hold
    /// both inline on one role).
    async fn managed_policy(&self, suffix: &str, document: &str) -> String {
        let out = ok(
            "iam:CreatePolicy",
            self.iam
                .create_policy()
                .policy_name(self.name(suffix))
                .policy_document(document)
                .tags(iam_tag(TEST_TAG.0, TEST_TAG.1))
                .send()
                .await,
        );
        let arn = out.policy().unwrap().arn().unwrap().to_string();
        self.created.lock().unwrap().policies.push(arn.clone());
        arn
    }

    async fn attach(&self, role: &Role, policy_arn: &str) {
        ok(
            "iam:AttachRolePolicy",
            self.iam
                .attach_role_policy()
                .role_name(&role.name)
                .policy_arn(policy_arn)
                .send()
                .await,
        );
    }

    async fn put_inline(&self, role: &Role, document: &str) {
        ok(
            "iam:PutRolePolicy",
            self.iam
                .put_role_policy()
                .role_name(&role.name)
                .policy_name("under-test")
                .policy_document(document)
                .send()
                .await,
        );
    }

    /// A role holding the given tier policies, tagged as owned by its own
    /// sessions (the strict-tenancy task role shape `iam:PassRole` checks).
    async fn tier_role(&self, suffix: &str, tiers: &[Tier]) -> Role {
        let role = self.role(suffix, None, &[]).await;
        self.tag_role(&role, "triple-c:owner", &role.session_user_id())
            .await;
        for &tier in tiers {
            let name = match tier {
                Tier::WorkspaceUser => "workspace",
                Tier::EnvironmentAdmin => "admin",
            };
            for (i, (_, json)) in policy_files(tier).iter().enumerate() {
                let arn = self
                    .managed_policy(&format!("{suffix}-{name}{i}"), json)
                    .await;
                self.attach(&role, &arn).await;
            }
        }
        role
    }

    /// An SDK config for a session of `role`, once IAM lets it be assumed
    /// and its policies have propagated.
    async fn assume(&self, role: &Role) -> SdkConfig {
        let mut last = None;
        for _ in 0..20 {
            match self
                .sts
                .assume_role()
                .role_arn(&role.arn)
                .role_session_name(SESSION)
                .send()
                .await
            {
                Ok(out) => {
                    let c = out.credentials().unwrap();
                    let keys = AwsKeys {
                        access_key_id: c.access_key_id().to_string(),
                        secret_access_key: c.secret_access_key().to_string(),
                        session_token: Some(c.session_token().to_string()),
                    };
                    // New sessions see a just-attached policy after a few
                    // seconds; denials in that window would be noise.
                    tokio::time::sleep(Duration::from_secs(10)).await;
                    return sdk_config_for_keys(&keys, &self.region);
                }
                Err(e) => {
                    last = Some(classify_sdk_error("sts:AssumeRole", &e).to_string());
                    tokio::time::sleep(Duration::from_secs(3)).await;
                }
            }
        }
        panic!("could not assume {}: {last:?}", role.arn);
    }

    /// `check_permissions` as the e2e profile, simulating `identity`'s
    /// principal. Retried once if a check failed (IAM is eventually
    /// consistent for a role created a moment ago).
    async fn check_as(&self, identity: &CallerIdentity) -> PermissionReport {
        let api = SdkPermissionApi::new(&self.cfg);
        for attempt in 0..3 {
            let report = check_permissions(&api, identity, &self.region)
                .await
                .expect("check_permissions");
            let failed = report
                .tiers
                .iter()
                .flat_map(|t| &t.results)
                .any(|r| r.note.is_some());
            if !failed || attempt == 2 {
                return report;
            }
            tokio::time::sleep(Duration::from_secs(10)).await;
        }
        unreachable!()
    }

    async fn environment_key(&self, kms: &aws_sdk_kms::Client, suffix: &str) -> String {
        let tag = |k: &str, v: &str| {
            aws_sdk_kms::types::Tag::builder()
                .tag_key(k)
                .tag_value(v)
                .build()
                .unwrap()
        };
        let r = kms
            .create_key()
            .description(self.name(suffix))
            .tags(tag("triple-c:component", "environment"))
            .tags(tag(TEST_TAG.0, TEST_TAG.1))
            .send()
            .await;
        let arn = ok("kms:CreateKey", r)
            .key_metadata()
            .unwrap()
            .arn()
            .unwrap()
            .to_string();
        self.created.lock().unwrap().keys.push(arn.clone());
        arn
    }

    fn record_secret(&self, arn: &str) {
        self.created.lock().unwrap().secrets.push(arn.to_string());
    }

    /// An admin-created secret encrypted with `key`.
    async fn admin_secret(&self, suffix: &str, key: &str, tag: Option<(&str, &str)>) -> String {
        let mut req = self
            .secrets
            .create_secret()
            .name(self.secret_name(suffix))
            .kms_key_id(key)
            .secret_string("e2e")
            .tags(sm_tag(TEST_TAG.0, TEST_TAG.1));
        if let Some((k, v)) = tag {
            req = req.tags(sm_tag(k, v));
        }
        let arn = ok("secretsmanager:CreateSecret", req.send().await)
            .arn()
            .unwrap()
            .to_string();
        self.record_secret(&arn);
        arn
    }

    /// Deletes everything recorded, logging (not failing on) errors so one
    /// failure doesn't strand the rest.
    async fn cleanup(&self) {
        let created = std::mem::take(&mut *self.created.lock().unwrap());
        let warn = |what: &str, e: &dyn std::fmt::Display| eprintln!("cleanup: {what}: {e}");
        for arn in &created.secrets {
            if let Err(e) = self
                .secrets
                .delete_secret()
                .secret_id(arn)
                .force_delete_without_recovery(true)
                .send()
                .await
            {
                let e = classify_sdk_error("secretsmanager:DeleteSecret", &e);
                if !e.to_string().contains("ResourceNotFound") {
                    warn(arn, &e);
                }
            }
        }
        for arn in &created.keys {
            if let Err(e) = self
                .kms
                .schedule_key_deletion()
                .key_id(arn)
                .pending_window_in_days(7)
                .send()
                .await
            {
                let e = classify_sdk_error("kms:ScheduleKeyDeletion", &e);
                if !e.to_string().contains("pending") {
                    warn(arn, &e);
                }
            }
        }
        for name in &created.roles {
            if let Ok(out) = self.iam.list_role_policies().role_name(name).send().await {
                for p in out.policy_names() {
                    let _ = self
                        .iam
                        .delete_role_policy()
                        .role_name(name)
                        .policy_name(p)
                        .send()
                        .await;
                }
            }
            if let Ok(out) = self
                .iam
                .list_attached_role_policies()
                .role_name(name)
                .send()
                .await
            {
                for p in out.attached_policies() {
                    let _ = self
                        .iam
                        .detach_role_policy()
                        .role_name(name)
                        .policy_arn(p.policy_arn().unwrap_or_default())
                        .send()
                        .await;
                }
            }
            if let Err(e) = self.iam.delete_role().role_name(name).send().await {
                warn(name, &classify_sdk_error("iam:DeleteRole", &e));
            }
        }
        for arn in &created.policies {
            if let Err(e) = self.iam.delete_policy().policy_arn(arn).send().await {
                warn(arn, &classify_sdk_error("iam:DeletePolicy", &e));
            }
        }
    }
}

fn verdicts(report: &PermissionReport, tier: Tier) -> Vec<(String, Verdict)> {
    report
        .tiers
        .iter()
        .find(|t| t.tier == tier)
        .unwrap()
        .results
        .iter()
        .map(|r| (r.action.clone(), r.verdict.clone()))
        .collect()
}

fn not_allowed(report: &PermissionReport, tier: Tier) -> Vec<(String, Verdict)> {
    verdicts(report, tier)
        .into_iter()
        .filter(|(_, v)| *v != Verdict::Allowed)
        .collect()
}

#[tokio::test]
#[ignore]
async fn a_role_with_the_workspace_policy_checks_as_workspace_user() {
    e2e(|e| async move {
        let role = e.tier_role("ws", &[Tier::WorkspaceUser]).await;
        let report = e.check_as(&role.identity(&e.account)).await;
        assert_eq!(report.method, Method::Simulated);
        assert_eq!(
            not_allowed(&report, Tier::WorkspaceUser),
            [],
            "every workspace check must be allowed"
        );
        assert_eq!(report.summary, Summary::WorkspaceUser);
        assert!(
            verdicts(&report, Tier::EnvironmentAdmin)
                .iter()
                .any(|(_, v)| *v == Verdict::Denied),
            "the workspace policy alone grants admin actions"
        );
    })
    .await;
}

#[tokio::test]
#[ignore]
async fn a_role_with_both_policies_checks_as_environment_admin() {
    e2e(|e| async move {
        let role = e
            .tier_role("admin", &[Tier::WorkspaceUser, Tier::EnvironmentAdmin])
            .await;
        let report = e.check_as(&role.identity(&e.account)).await;
        assert_eq!(report.method, Method::Simulated);
        assert_eq!(not_allowed(&report, Tier::WorkspaceUser), []);
        assert_eq!(not_allowed(&report, Tier::EnvironmentAdmin), []);
        assert_eq!(report.summary, Summary::EnvironmentAdmin);
    })
    .await;
}

#[tokio::test]
#[ignore]
async fn a_role_with_no_policy_checks_as_insufficient_and_lists_every_action() {
    e2e(|e| async move {
        let role = e.role("none", None, &[]).await;
        let report = e.check_as(&role.identity(&e.account)).await;
        assert_eq!(report.method, Method::Simulated);
        assert_eq!(report.summary, Summary::Insufficient);
        for tier in [Tier::WorkspaceUser, Tier::EnvironmentAdmin] {
            let rows = verdicts(&report, tier);
            for c in checks(tier) {
                assert!(
                    rows.iter().any(|(a, _)| a == c.action),
                    "{tier:?}: no row for {}",
                    c.action
                );
            }
            for (action, v) in rows {
                assert_eq!(v, Verdict::Denied, "{tier:?} {action}");
            }
        }
    })
    .await;
}

/// The workspace policy without `InspectOwnPrincipal`, assumed and checked
/// *as* itself: `iam:GetRole` is denied, so the check probes.
#[tokio::test]
#[ignore]
async fn a_role_without_simulate_permission_falls_back_to_probes() {
    e2e(|e| async move {
        let mut doc: serde_json::Value =
            serde_json::from_str(policy_files(Tier::WorkspaceUser)[0].1).unwrap();
        doc["Statement"]
            .as_array_mut()
            .unwrap()
            .retain(|s| s["Sid"] != "InspectOwnPrincipal");
        let role = e.role("nosim", None, &[]).await;
        let arn = e.managed_policy("nosim", &doc.to_string()).await;
        e.attach(&role, &arn).await;
        let cfg = e.assume(&role).await;
        let me = caller_identity(&cfg).await.expect("identity");
        assert_eq!(me.user_id, role.session_user_id());
        let report = check_permissions(&SdkPermissionApi::new(&cfg), &me, &e.region)
            .await
            .expect("check_permissions");
        let Method::Probed { reason } = &report.method else {
            panic!("expected probes, got {:?}", report.method);
        };
        assert!(reason.contains("iam:GetRole"), "{reason}");
        let rows = verdicts(&report, Tier::WorkspaceUser);
        let verdict = |a: &str| rows.iter().find(|(x, _)| x == a).unwrap().1.clone();
        for probed in [
            "ecs:ListClusters",
            "ssm:GetParametersByPath",
            "secretsmanager:ListSecrets",
        ] {
            assert_eq!(verdict(probed), Verdict::Allowed, "{probed}");
        }
        assert_eq!(verdict("iam:GetRole"), Verdict::Denied);
        assert_eq!(verdict("ecs:RunTask"), Verdict::Unverified);
        // A probed run never claims the admin tier, and the denied
        // own-principal check keeps it from claiming the workspace tier.
        assert_eq!(report.summary, Summary::Insufficient);
    })
    .await;
}

/// The whole workspace policy, assumed: the real `iam:GetRole` on its own
/// role (whose ARN has a path) and `SimulatePrincipalPolicy` of itself.
#[tokio::test]
#[ignore]
async fn an_assumed_workspace_role_checks_itself_by_simulation() {
    e2e(|e| async move {
        let role = e.tier_role("self", &[Tier::WorkspaceUser]).await;
        let cfg = e.assume(&role).await;
        let me = caller_identity(&cfg).await.expect("identity");
        assert!(me.arn.contains(":assumed-role/"), "{}", me.arn);
        let report = check_permissions(&SdkPermissionApi::new(&cfg), &me, &e.region)
            .await
            .expect("check_permissions");
        assert_eq!(report.method, Method::Simulated);
        assert_eq!(not_allowed(&report, Tier::WorkspaceUser), []);
        assert_eq!(report.summary, Summary::WorkspaceUser);
    })
    .await;
}

/// The e2e profile itself (an account administrator): every check allowed.
/// For an SSO profile this resolves the `/aws-reserved/sso.amazonaws.com/`
/// role path through `iam:GetRole`.
#[tokio::test]
#[ignore]
async fn the_e2e_profile_checks_as_environment_admin() {
    e2e(|e| async move {
        let me = caller_identity(&e.cfg).await.expect("identity");
        let report = e.check_as(&me).await;
        assert_eq!(report.method, Method::Simulated);
        let failed: Vec<_> = report
            .tiers
            .iter()
            .flat_map(|t| &t.results)
            .filter(|r| r.verdict != Verdict::Allowed)
            .collect();
        assert!(failed.is_empty(), "{failed:#?}");
        assert_eq!(report.summary, Summary::EnvironmentAdmin);
    })
    .await;
}

/// The spec's most important rule: no shell, stop or session in someone
/// else's task.
#[tokio::test]
#[ignore]
async fn the_owner_condition_denies_another_owners_task() {
    e2e(|e| async move {
        let role = e.tier_role("owner", &[Tier::WorkspaceUser]).await;
        let me = role.session_user_id();
        let api = SdkPermissionApi::new(&e.cfg);
        let p = Placeholders {
            region: &e.region,
            account: &e.account,
            principal: &role.arn,
        };
        let task_checks = checks(Tier::WorkspaceUser).iter().filter(|c| {
            ["ecs:StopTask", "ecs:ExecuteCommand", "ssm:StartSession"].contains(&c.action)
                && c.resource.contains(":task/")
        });
        let mut n = 0;
        for c in task_checks {
            n += 1;
            let mine = simulation_request(c, &p, &me);
            assert!(
                api.simulate(&mine).await.unwrap(),
                "{} on my own task",
                c.action
            );
            let mut theirs = mine.clone();
            for entry in &mut theirs.context {
                if entry.key == "aws:ResourceTag/triple-c:owner" {
                    entry.values = vec!["AROAEXAMPLEOTHER:bob@example.com".into()];
                }
            }
            assert!(
                !api.simulate(&theirs).await.unwrap(),
                "{} allowed on another owner's task",
                c.action
            );
            theirs
                .context
                .retain(|x| x.key != "aws:ResourceTag/triple-c:owner");
            assert!(
                !api.simulate(&theirs).await.unwrap(),
                "{} allowed on an untagged task",
                c.action
            );
        }
        assert_eq!(n, 3);
    })
    .await;
}

/// SimulatePrincipalPolicy *does* apply the principal's own permissions
/// boundary (it does not apply SCPs or session policies).
#[tokio::test]
#[ignore]
async fn the_simulator_applies_the_principals_permissions_boundary() {
    e2e(|e| async move {
        let boundary = e.managed_policy("boundary", permissions_boundary_json()).await;
        let role = e.role("bounded", Some(&boundary), &[]).await;
        e.put_inline(
            &role,
            r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":"secretsmanager:GetSecretValue","Resource":"*"}]}"#,
        )
        .await;
        let sim = |name: &str| {
            e.iam
                .simulate_principal_policy()
                .policy_source_arn(&role.arn)
                .action_names("secretsmanager:GetSecretValue")
                .resource_arns(format!(
                    "arn:aws:secretsmanager:{}:{}:secret:{name}-AbCdEf",
                    e.region, e.account
                ))
                .send()
        };
        let decision = |out: aws_sdk_iam::operation::simulate_principal_policy::SimulatePrincipalPolicyOutput| {
            out.evaluation_results()[0].eval_decision().as_str().to_string()
        };
        assert_eq!(decision(ok("simulate", sim("triple-c/x").await)), "allowed");
        assert_eq!(decision(ok("simulate", sim("other/x").await)), "implicitDeny");
    })
    .await;
}

/// The secret statements and the KMS rules behind them, with real calls:
/// tag-on-create, the environment key by ARN (and `kms:Decrypt` with it),
/// the explicit Deny on the AWS-managed key, owner and component fences,
/// and the permissions boundary reading `aws:ResourceTag` of the key.
#[tokio::test]
#[ignore]
async fn secrets_use_the_environment_key_and_only_their_owner_can_change_them() {
    e2e(|e| async move {
        let key = e.environment_key(&e.kms, "env-key").await;
        let aws_managed = ok(
            "kms:DescribeKey",
            e.kms.describe_key().key_id("alias/aws/secretsmanager").send().await,
        )
        .key_metadata()
        .unwrap()
        .arn()
        .unwrap()
        .to_string();
        let user = e.tier_role("secrets-user", &[Tier::WorkspaceUser]).await;
        let boundary = e.managed_policy("secrets-boundary", permissions_boundary_json()).await;
        let task = e.role("secrets-task", Some(&boundary), &[]).await;
        e.put_inline(
            &task,
            r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":["secretsmanager:GetSecretValue","kms:Decrypt"],"Resource":"*"}]}"#,
        )
        .await;
        let environment = e
            .admin_secret("environment", &key, Some(("triple-c:component", "environment")))
            .await;
        let someone_elses = e
            .admin_secret("someone-else", &key, Some(("triple-c:owner", "AROAEXAMPLEOTHER:bob")))
            .await;

        let as_user = aws_sdk_secretsmanager::Client::new(&e.assume(&user).await);
        let me = user.session_user_id();
        let create = |suffix: &str, key: Option<&str>| {
            as_user
                .create_secret()
                .name(e.secret_name(suffix))
                .set_kms_key_id(key.map(str::to_string))
                .secret_string("v1")
                .tags(sm_tag("triple-c:owner", &me))
                .send()
        };

        // Own secret on the environment key, named by ARN.
        let own = ok("create own secret", create("own", Some(&key)).await)
            .arn()
            .unwrap()
            .to_string();
        e.record_secret(&own);
        ok(
            "tag for cleanup",
            e.secrets
                .tag_resource()
                .secret_id(&own)
                .tags(sm_tag(TEST_TAG.0, TEST_TAG.1))
                .send()
                .await,
        );
        ok(
            "put own",
            as_user.put_secret_value().secret_id(&own).secret_string("v2").send().await,
        );

        // Any other key is refused.
        for (i, (what, k)) in [("no KmsKeyId", None), ("aws/secretsmanager", Some(aws_managed.as_str()))]
            .into_iter()
            .enumerate()
        {
            let r = create(&format!("refused-{i}"), k).await;
            if let Ok(out) = &r {
                e.record_secret(out.arn().unwrap());
            }
            denied(what, r);
        }
        // Only the ordinary tag keys, and never claiming or changing others'.
        denied(
            "tag own with triple-c:component",
            as_user
                .tag_resource()
                .secret_id(&own)
                .tags(sm_tag("triple-c:owner", &me))
                .tags(sm_tag("triple-c:component", "x"))
                .send()
                .await,
        );
        for (what, arn) in [("environment", &environment), ("someone else's", &someone_elses)] {
            denied(
                &format!("put {what}"),
                as_user.put_secret_value().secret_id(arn).secret_string("x").send().await,
            );
            denied(
                &format!("delete {what}"),
                as_user.delete_secret().secret_id(arn).send().await,
            );
            denied(
                &format!("claim {what}"),
                as_user
                    .tag_resource()
                    .secret_id(arn)
                    .tags(sm_tag("triple-c:owner", &me))
                    .send()
                    .await,
            );
        }

        // A task role under the boundary decrypts through Secrets Manager
        // only with the component-tagged key.
        let as_task = aws_sdk_secretsmanager::Client::new(&e.assume(&task).await);
        let value = ok(
            "task reads own",
            as_task.get_secret_value().secret_id(&own).send().await,
        );
        assert_eq!(value.secret_string(), Some("v2"));

        ok(
            "delete own",
            as_user
                .delete_secret()
                .secret_id(&own)
                .force_delete_without_recovery(true)
                .send()
                .await,
        );
    })
    .await;
}

/// The admin tier's fences, for real: roles only with the Triple-C
/// boundary, the boundary never removed or swapped or edited, and keys
/// created only with `triple-c:component` (tag-on-create passes the
/// `kms:TagResource` rule) and destroyed only if they carry it.
///
/// Needs the exact `triple-c-permissions-boundary` ARN: the test creates
/// that policy and deletes it again, unless the account already has one, in
/// which case it uses it and leaves it alone.
#[tokio::test]
#[ignore]
async fn the_admin_tier_creates_only_bounded_roles_and_tagged_keys() {
    e2e(|e| async move {
        let boundary = format!("arn:aws:iam::{}:policy/{PERMISSIONS_BOUNDARY_NAME}", e.account);
        if e.iam.get_policy().policy_arn(&boundary).send().await.is_err() {
            ok(
                "create the boundary",
                e.iam
                    .create_policy()
                    .policy_name(PERMISSIONS_BOUNDARY_NAME)
                    .policy_document(permissions_boundary_json())
                    .tags(iam_tag(TEST_TAG.0, TEST_TAG.1))
                    .send()
                    .await,
            );
            e.created.lock().unwrap().policies.push(boundary.clone());
        }
        let other_boundary = e.managed_policy("other-boundary", permissions_boundary_json()).await;
        let admin = e
            .tier_role("fence", &[Tier::WorkspaceUser, Tier::EnvironmentAdmin])
            .await;
        let cfg = e.assume(&admin).await;
        let iam = aws_sdk_iam::Client::new(&cfg);
        let kms = aws_sdk_kms::Client::new(&cfg);
        let trust = format!(
            r#"{{"Version":"2012-10-17","Statement":[{{"Effect":"Allow","Principal":{{"AWS":"arn:aws:iam::{}:root"}},"Action":"sts:AssumeRole"}}]}}"#,
            e.account
        );
        let create = |suffix: &str, b: Option<&str>| {
            iam.create_role()
                .role_name(e.name(suffix))
                .assume_role_policy_document(&trust)
                .tags(iam_tag(TEST_TAG.0, TEST_TAG.1))
                .set_permissions_boundary(b.map(str::to_string))
                .send()
        };
        for (suffix, b) in [("unbounded", None), ("other-bounded", Some(other_boundary.as_str()))] {
            let r = create(suffix, b).await;
            if r.is_ok() {
                e.created.lock().unwrap().roles.push(e.name(suffix));
            }
            denied(&format!("create {suffix} role"), r);
        }
        let child = e.name("bounded");
        ok("create bounded role", create("bounded", Some(&boundary)).await);
        e.created.lock().unwrap().roles.push(child.clone());
        ok(
            "put a policy on the bounded role",
            iam.put_role_policy()
                .role_name(&child)
                .policy_name("p")
                .policy_document(r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":"sts:GetCallerIdentity","Resource":"*"}]}"#)
                .send()
                .await,
        );
        denied(
            "remove the boundary",
            iam.delete_role_permissions_boundary().role_name(&child).send().await,
        );
        denied(
            "swap the boundary",
            iam.put_role_permissions_boundary()
                .role_name(&child)
                .permissions_boundary(&other_boundary)
                .send()
                .await,
        );
        denied(
            "edit the boundary",
            iam.create_policy_version()
                .policy_arn(&boundary)
                .policy_document(permissions_boundary_json())
                .send()
                .await,
        );

        // Keys: tagged create passes (tag-on-create authorizes
        // kms:TagResource against the new key's requested tags).
        let untagged = kms.create_key().description(e.name("untagged")).send().await;
        if let Ok(out) = &untagged {
            e.created
                .lock()
                .unwrap()
                .keys
                .push(out.key_metadata().unwrap().arn().unwrap().to_string());
        }
        denied("create an untagged key", untagged);
        let key = e.environment_key(&kms, "admin-key").await;
        ok(
            "schedule the environment key's deletion",
            kms.schedule_key_deletion()
                .key_id(&key)
                .pending_window_in_days(7)
                .send()
                .await,
        );
    })
    .await;
}
