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
        sdk_config_for_keys(&self.assume_keys(role).await, &self.region)
    }

    /// A session of `role` as keys (see [`E2e::assume`]).
    async fn assume_keys(&self, role: &Role) -> AwsKeys {
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
                    return keys;
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
        // `InspectOwnPrincipal` lives in the first file.
        let mut doc: serde_json::Value =
            serde_json::from_str(policy_files(Tier::WorkspaceUser)[0].1).unwrap();
        doc["Statement"]
            .as_array_mut()
            .unwrap()
            .retain(|s| s["Sid"] != "InspectOwnPrincipal");
        let role = e.role("nosim", None, &[]).await;
        let arn = e.managed_policy("nosim", &doc.to_string()).await;
        e.attach(&role, &arn).await;
        // The tasks file is attached whole.
        let tasks = e
            .managed_policy("nosim-tasks", policy_files(Tier::WorkspaceUser)[1].1)
            .await;
        e.attach(&role, &tasks).await;
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

/// The cost estimate's call (Q5), made for real: a session holding only the
/// workspace files reads the region's Fargate rates from us-east-1 (the
/// parser must find both architectures), and one holding nothing is denied.
#[tokio::test]
#[ignore]
async fn a_workspace_role_reads_fargate_prices_and_a_bare_role_cannot() {
    use super::pricing::{fetch_rates, SdkPricingApi, PRICING_REGION};
    e2e(|e| async move {
        let ws = e.tier_role("price", &[Tier::WorkspaceUser]).await;
        let cfg = sdk_config_for_keys(&e.assume_keys(&ws).await, PRICING_REGION);
        let rates = fetch_rates(&SdkPricingApi::new(&cfg), &e.region)
            .await
            .expect("workspace role reads prices");
        let x86 = rates.x86_64.expect("x86 rates");
        let arm = rates.arm64.expect("ARM rates");
        eprintln!("{}: x86 {x86:?}, ARM {arm:?}", e.region);
        assert!(x86.vcpu_hour > 0.0 && x86.gb_hour > 0.0 && x86.storage_gb_hour > 0.0);
        assert!(arm.vcpu_hour < x86.vcpu_hour, "ARM is cheaper per vCPU");
        // E10: an EC2 instance type's on-demand price, with the same rights.
        let m7i = super::pricing::fetch_ec2_price(&SdkPricingApi::new(&cfg), &e.region, "m7i.large")
            .await
            .expect("workspace role reads EC2 prices");
        eprintln!("{}: m7i.large {m7i}/hour", e.region);
        assert!(m7i > 0.0 && m7i < 1.0, "{m7i}");

        let bare = e.role("noprice", None, &[]).await;
        let cfg = sdk_config_for_keys(&e.assume_keys(&bare).await, PRICING_REGION);
        let err = fetch_rates(&SdkPricingApi::new(&cfg), &e.region)
            .await
            .expect_err("a role without pricing:GetProducts");
        assert!(err.contains("pricing:GetProducts"), "{err}");
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

        // `workspace-user-tasks.json`: the owner reads their own secret, never
        // the environment's or someone else's.
        let read = ok(
            "get own",
            as_user.get_secret_value().secret_id(&own).send().await,
        );
        assert_eq!(read.secret_string(), Some("v2"));
        for (what, arn) in [("environment", &environment), ("someone else's", &someone_elses)] {
            denied(
                &format!("get {what}"),
                as_user.get_secret_value().secret_id(arn).send().await,
            );
        }

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

/// Phase 4: OpenTofu environments, deployed for real by a role that holds
/// only the environment-admin policies.
///
/// ```sh
/// cd app/src-tauri
/// cargo build   # the runner is the app binary re-executed (see below)
/// TRIPLEC_AWS_E2E_PROFILE=… TRIPLEC_AWS_E2E_REGION=us-west-2 \
///   cargo test aws::live_tests::environment -- --ignored --test-threads=1
/// ```
///
/// What runs the way the app runs it: `ensure_tofu` (the pinned download),
/// `ensure_state_bucket`'s configuration, `materialize`, `child_env` (a keys
/// connection for the environment admin, a profile connection for the
/// account admin), the detached runner — the **built app binary**
/// (`target/debug/triple-c`, or `TRIPLEC_E2E_APP_BIN`) re-executed with
/// `--tofu-runner`, because the test harness is not the app and can't
/// re-execute itself as one — `follow`, `scan`, the refusal checks the
/// commands make, and the conditional lock delete. What doesn't: the Tauri
/// commands themselves (they need an `AppHandle` and saved settings), so the
/// slot/follower bookkeeping and the event stream are not exercised here.
///
/// Extra variables:
/// - `TRIPLEC_E2E_BUCKET_SUFFIX`: the state bucket is
///   `triple-c-tfstate-<account>-<region>-<suffix>` (default: random), so the
///   account's real one is never touched;
/// - `TRIPLEC_E2E_TOFU_ROOT`: where OpenTofu, the provider cache and the work
///   folders go (default: `<tmp>/triple-c-e2e-tofu`, kept between runs).
mod environment {
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::time::Duration;

    use aws_config::SdkConfig;

    use super::{classify_sdk_error, e2e, ok, E2e, Role};
    use futures_util::FutureExt;
    use crate::aws::environments::{
        ensure_named_state_bucket, is_empty_state, scan_in_bucket, EnvironmentAction, EnvironmentState, Knowable,
        OnboardingState, SdkDetectApi,
    };
    use crate::aws::policies::{policy_files, Tier, PERMISSIONS_BOUNDARY_NAME};
    use crate::aws::tofu::{
        self, child_env, ensure_tofu, follow, materialize, start_detached_with, TofuOp,
        TofuOutcome, Workdir,
    };
    use crate::aws::tofu_modules::{
        boundary_vars, environment_vars, onboarding_vars, owner_key, state_bucket_name, state_key,
        user_role_names, Deployment, ModuleKind, NetworkChoice,
    };
    use crate::aws::{caller_identity, sdk_config_for_keys, CallerIdentity};
    use crate::commands::environment_commands::{
        delete_lock_if_unchanged, deploy_refusal, network_from_record, onboard_refusal,
        validate_owner_id, LOCK_CHANGED,
    };
    use crate::models::host_connection::{AwsAuth, HostConnection, HostKind};
    use crate::storage::secure::AwsKeys;

    /// The environment admin: a role holding the admin tier's policy files
    /// as managed policies, and nothing else.
    struct Admin {
        role: Role,
    }

    struct Env {
        e: Arc<E2e>,
        root: PathBuf,
        tofu: PathBuf,
        app_bin: PathBuf,
        bucket: String,
        /// Deployments applied (work folder, still to destroy), newest last.
        applied: std::sync::Mutex<Vec<(Workdir, bool)>>,
        /// The test created the boundary (through its module).
        boundary_created: std::sync::Mutex<bool>,
        bucket_created: std::sync::Mutex<bool>,
    }

    fn app_bin() -> PathBuf {
        if let Ok(p) = std::env::var("TRIPLEC_E2E_APP_BIN") {
            return PathBuf::from(p);
        }
        // target/debug/deps/<test binary> -> target/debug/triple-c
        let exe = std::env::current_exe().expect("current exe");
        let dir = exe.parent().and_then(|d| d.parent()).expect("target dir");
        dir.join(format!("triple-c{}", std::env::consts::EXE_SUFFIX))
    }

    fn keys_connection(region: &str) -> HostConnection {
        HostConnection {
            id: "e2e-envadmin".into(),
            name: "e2e-envadmin".into(),
            kind: HostKind::Aws {
                auth: AwsAuth::Keys {
                    keyring_ref: "unused".into(),
                },
                default_region: region.to_string(),
            },
        }
    }

    fn profile_connection(region: &str) -> HostConnection {
        HostConnection {
            id: "e2e".into(),
            name: "e2e".into(),
            kind: HostKind::Aws {
                auth: AwsAuth::Profile {
                    name: super::var("TRIPLEC_AWS_E2E_PROFILE"),
                },
                default_region: region.to_string(),
            },
        }
    }

    /// `not authorized to perform: <action>` (and the KMS/S3 phrasings) in a
    /// log message.
    fn denied_actions(message: &str) -> Vec<String> {
        let mut out = Vec::new();
        for marker in ["not authorized to perform: ", "is not authorized to perform "] {
            let mut rest = message;
            while let Some(i) = rest.find(marker) {
                rest = &rest[i + marker.len()..];
                let action: String = rest
                    .chars()
                    .take_while(|c| c.is_ascii_alphanumeric() || *c == ':' || *c == '*')
                    .collect();
                if !action.is_empty() && !out.contains(&action) {
                    out.push(action);
                }
            }
        }
        if out.is_empty()
            && (message.contains("AccessDenied") || message.contains("UnauthorizedOperation"))
        {
            out.push("(unnamed: see the message above)".into());
        }
        out
    }

    impl Env {
        async fn new(e: Arc<E2e>) -> Arc<Env> {
            let root = std::env::var("TRIPLEC_E2E_TOFU_ROOT")
                .map(PathBuf::from)
                .unwrap_or_else(|_| std::env::temp_dir().join("triple-c-e2e-tofu"));
            std::fs::create_dir_all(&root).unwrap();
            let app_bin = app_bin();
            assert!(
                app_bin.is_file(),
                "{} is missing: run `cargo build` first (the runner is the app binary)",
                app_bin.display()
            );
            let tofu = ensure_tofu(&root).await.expect("ensure_tofu");
            let suffix = std::env::var("TRIPLEC_E2E_BUCKET_SUFFIX").unwrap_or_else(|_| {
                format!("e2e{}", &uuid::Uuid::new_v4().simple().to_string()[..8])
            });
            let bucket = format!("{}-{suffix}", state_bucket_name(&e.account, &e.region));
            assert!(bucket.len() <= 63, "{bucket} is too long for a bucket name");
            Arc::new(Env {
                e,
                root,
                tofu,
                app_bin,
                bucket,
                applied: Default::default(),
                boundary_created: Default::default(),
                bucket_created: Default::default(),
            })
        }

        /// The environment admin role (no boundary on the role itself: the
        /// boundary allows only what tasks do, so an admin under it could
        /// administer nothing).
        async fn admin(&self) -> Admin {
            let role = self.e.role("envadmin", None, &[]).await;
            for (i, (file, json)) in policy_files(Tier::EnvironmentAdmin).iter().enumerate() {
                let arn = self.e.managed_policy(&format!("envadmin{i}"), json).await;
                eprintln!("envadmin: {file} -> {arn}");
                self.e.attach(&role, &arn).await;
            }
            Admin { role }
        }

        /// A fresh session of the admin role (role chaining caps one at an
        /// hour, so each phase takes its own).
        async fn session(&self, admin: &Admin) -> (AwsKeys, SdkConfig, CallerIdentity) {
            let keys = self.e.assume_keys(&admin.role).await;
            let cfg = sdk_config_for_keys(&keys, &self.e.region);
            let who = caller_identity(&cfg).await.expect("envadmin identity");
            (keys, cfg, who)
        }

        async fn scan(&self, cfg: &SdkConfig, who: &CallerIdentity) -> crate::aws::environments::EnvironmentScan {
            let api = SdkDetectApi::new(cfg, &who.account);
            let (root, bucket) = (self.root.clone(), self.bucket.clone());
            let running = move |d: Deployment| {
                tofu::workdir_for(&root, d, &bucket)
                    .map(|wd| crate::aws::environments::LocalRun {
                        running: tofu::is_running(&wd),
                        failed: tofu::last_run_failed(&wd),
                        errored_state: tofu::has_errored_state(&wd),
                    })
                    .unwrap_or_default()
            };
            scan_in_bucket(&api, who, &self.e.region, &self.bucket, &running)
                .await
                .expect("scan")
        }

        /// Write the module and run `op` through the detached runner, printing
        /// the log and every denied action.
        async fn run(
            &self,
            kind: ModuleKind,
            d: Deployment<'_>,
            vars: &serde_json::Value,
            op: TofuOp,
            env: Vec<(String, String)>,
        ) -> (Workdir, TofuOutcome, Vec<String>) {
            let wd = materialize(&self.root, kind, d, &self.bucket, &self.e.region, vars)
                .expect("materialize");
            if op == TofuOp::Apply {
                let mut applied = self.applied.lock().unwrap();
                if !applied.iter().any(|(w, _)| *w == wd) {
                    applied.push((wd.clone(), true));
                }
            }
            let label = state_key(d);
            eprintln!("=== tofu {op:?} {label} ({})", wd.dir.display());
            let (bin, tofu_bin, start_wd) = (self.app_bin.clone(), self.tofu.clone(), wd.clone());
            let started = tokio::task::spawn_blocking(move || {
                start_detached_with(&bin, Duration::from_secs(30), &tofu_bin, &start_wd, op, &env)
            })
            .await
            .unwrap()
            .unwrap_or_else(|e| panic!("start {label}: {e}"));
            assert!(started.pid.is_some(), "the runner wrote no PID");
            let mut denied = Vec::new();
            let outcome = follow(&wd, Duration::from_millis(500), |events| {
                for ev in events {
                    let resource = ev.resource.as_deref().unwrap_or("");
                    let short: String = ev.message.chars().take(300).collect();
                    eprintln!("  [{}] {} {} {}", ev.level, ev.kind, resource, short);
                    if ev.level == "error" {
                        eprintln!("  ERROR DETAIL: {}", ev.message);
                        for a in denied_actions(&ev.message) {
                            eprintln!("  DENIED: {a} ({resource})");
                            if !denied.contains(&a) {
                                denied.push(a);
                            }
                        }
                    }
                }
            })
            .await
            .expect("follow");
            eprintln!("=== {label}: {outcome:?}");
            if op == TofuOp::Destroy && outcome.ok {
                for (w, pending) in self.applied.lock().unwrap().iter_mut() {
                    if *w == wd {
                        *pending = false;
                    }
                }
            }
            (wd, outcome, denied)
        }

        fn keys_env(&self, keys: &AwsKeys) -> Vec<(String, String)> {
            child_env(
                &keys_connection(&self.e.region),
                Some(keys),
                &self.root,
                std::env::vars(),
            )
            .expect("child env")
        }

        fn admin_env(&self) -> Vec<(String, String)> {
            child_env(&profile_connection(&self.e.region), None, &self.root, std::env::vars())
                .expect("child env")
        }

        /// `ensure_state_bucket` as the environment admin. The bucket is
        /// marked for deletion only if this call is what creates it: a bucket
        /// that already exists (a fixed `TRIPLEC_E2E_BUCKET_SUFFIX`) is never
        /// deleted by the cleanup.
        async fn ensure_bucket(&self, cfg: &SdkConfig, who: &CallerIdentity) {
            use aws_sdk_s3::error::ProvideErrorMetadata;
            let head = aws_sdk_s3::Client::new(&self.e.cfg)
                .head_bucket()
                .bucket(&self.bucket)
                .send()
                .await;
            match head {
                Ok(_) => eprintln!("{} already exists; the cleanup leaves it", self.bucket),
                Err(e)
                    if e.raw_response().map(|r| r.status().as_u16()) == Some(404)
                        || e.code() == Some("NotFound") =>
                {
                    // Set before the create, so a create that half-succeeds
                    // is still cleaned up.
                    *self.bucket_created.lock().unwrap() = true;
                }
                Err(e) => panic!("HeadBucket {}: {e:?}", self.bucket),
            }
            let b = ensure_named_state_bucket(cfg, &who.account, &self.e.region, &self.bucket)
                .await
                .unwrap_or_else(|e| panic!("ensure_state_bucket as the environment admin: {e}"));
            assert_eq!(b, self.bucket);
        }

        /// The boundary, through its module as the account admin, if the
        /// account has none.
        async fn ensure_boundary(&self) {
            let arn = format!(
                "arn:aws:iam::{}:policy/{PERMISSIONS_BOUNDARY_NAME}",
                self.e.account
            );
            match self.e.iam.get_policy().policy_arn(&arn).send().await {
                Ok(_) => {
                    eprintln!("the account already has the boundary; using it");
                    return;
                }
                Err(e) if e.as_service_error().is_some_and(|s| s.is_no_such_entity_exception()) => {}
                // Anything else is not "absent": never take ownership of a
                // boundary this run can't see.
                Err(e) => panic!("iam:GetPolicy on the boundary: {e:?}"),
            }
            // Ownership is the module's state: the cleanup removes the
            // boundary only with `tofu destroy` of that state, so a failed
            // apply (EntityAlreadyExists in a race) destroys nothing.
            *self.boundary_created.lock().unwrap() = true;
            let (_, outcome, denied) = self
                .run(
                    ModuleKind::PermissionsBoundary,
                    Deployment::Boundary,
                    &boundary_vars(&self.e.region),
                    TofuOp::Apply,
                    self.admin_env(),
                )
                .await;
            assert!(outcome.ok, "boundary apply: {outcome:?} denied {denied:?}");
        }

        /// As the account admin: destroy whatever is still applied (newest
        /// first; the boundary only through its own module's state), then the
        /// bucket with every version if this run created it. Logs rather than
        /// fails, so one error strands nothing else.
        async fn cleanup(&self) {
            let pending: Vec<Workdir> = self
                .applied
                .lock()
                .unwrap()
                .iter()
                .rev()
                .filter(|(_, p)| *p)
                .map(|(w, _)| w.clone())
                .collect();
            for wd in pending {
                eprintln!("cleanup: destroying {} as the account admin", wd.dir.display());
                let env = self.admin_env();
                let (bin, tofu_bin, w) = (self.app_bin.clone(), self.tofu.clone(), wd.clone());
                let started = tokio::task::spawn_blocking(move || {
                    start_detached_with(&bin, Duration::from_secs(30), &tofu_bin, &w, TofuOp::Destroy, &env)
                })
                .await;
                match started {
                    Ok(Ok(_)) => {
                        let outcome = follow(&wd, Duration::from_millis(500), |events| {
                            for ev in events.iter().filter(|e| e.level == "error") {
                                eprintln!("  cleanup error: {}", ev.message);
                            }
                        })
                        .await;
                        eprintln!("cleanup: destroy {}: {outcome:?}", wd.dir.display());
                    }
                    other => eprintln!("cleanup: could not start destroy: {other:?}"),
                }
            }
            // No direct delete: the boundary module's `tofu destroy` above is
            // the only thing that removes it, because its state proves this
            // run created it. If that failed, say so loudly.
            if *self.boundary_created.lock().unwrap() {
                let arn = format!(
                    "arn:aws:iam::{}:policy/{PERMISSIONS_BOUNDARY_NAME}",
                    self.e.account
                );
                if self.e.iam.get_policy().policy_arn(&arn).send().await.is_ok() {
                    eprintln!(
                        "cleanup: WARNING: {arn} still exists after its module's destroy; \
                         remove it by hand if this run created it"
                    );
                }
            }
            if *self.bucket_created.lock().unwrap() {
                self.delete_bucket().await;
            }
        }

        async fn delete_bucket(&self) {
            let s3 = aws_sdk_s3::Client::new(&self.e.cfg);
            loop {
                let page = match s3.list_object_versions().bucket(&self.bucket).send().await {
                    Ok(p) => p,
                    Err(e) => {
                        use aws_sdk_s3::error::ProvideErrorMetadata;
                        if e.code() != Some("NoSuchBucket") {
                            eprintln!("cleanup: list versions of {}: {e:?}", self.bucket);
                        }
                        return;
                    }
                };
                let mut ids = Vec::new();
                for v in page.versions() {
                    ids.push((v.key().unwrap_or_default().to_string(), v.version_id().map(str::to_string)));
                }
                for m in page.delete_markers() {
                    ids.push((m.key().unwrap_or_default().to_string(), m.version_id().map(str::to_string)));
                }
                if ids.is_empty() {
                    break;
                }
                for (key, version) in ids {
                    if let Err(e) = s3
                        .delete_object()
                        .bucket(&self.bucket)
                        .key(&key)
                        .set_version_id(version)
                        .send()
                        .await
                    {
                        eprintln!("cleanup: delete {key}: {e:?}");
                        return;
                    }
                }
            }
            match s3.delete_bucket().bucket(&self.bucket).send().await {
                Ok(_) => eprintln!("cleanup: deleted bucket {}", self.bucket),
                Err(e) => eprintln!("cleanup: delete bucket {}: {e:?}", self.bucket),
            }
        }
    }

    /// `e2e` plus the environment fixture's own cleanup, which runs first
    /// (it needs the account admin, and the roles it destroys hold the
    /// boundary).
    async fn env_e2e<F, Fut>(body: F)
    where
        F: FnOnce(Arc<Env>) -> Fut,
        Fut: std::future::Future<Output = ()>,
    {
        use futures_util::FutureExt;
        e2e(|e| async move {
            let env = Env::new(e).await;
            let result = std::panic::AssertUnwindSafe(body(env.clone()))
                .catch_unwind()
                .await;
            env.cleanup().await;
            if let Err(panic) = result {
                std::panic::resume_unwind(panic);
            }
        })
        .await;
    }

    fn assert_ok(what: &str, outcome: &TofuOutcome, denied: &[String]) {
        assert!(
            outcome.ok,
            "{what} failed: {:?}; denied actions: {denied:?}",
            outcome.summary
        );
        assert!(denied.is_empty(), "{what} reported denied actions: {denied:?}");
    }

    #[tokio::test]
    #[ignore]
    async fn an_environment_admin_can_deploy_join_update_and_remove_an_environment() {
        env_e2e(|t| async move {
            let admin = t.admin().await;
            let (keys, cfg, who) = t.session(&admin).await;
            assert!(who.arn.contains(":assumed-role/"), "{}", who.arn);

            // The state bucket, created and configured by the environment
            // admin (s3:CreateBucket with the request tag, in-account).
            t.ensure_bucket(&cfg, &who).await;
            // Idempotent.
            t.ensure_bucket(&cfg, &who).await;

            t.ensure_boundary().await;

            let name = format!("e2e{}", &uuid::Uuid::new_v4().simple().to_string()[..6]);
            let found = t.scan(&cfg, &who).await;
            assert_eq!(found.state_bucket, Knowable::Yes);
            assert_eq!(found.boundary, Knowable::Yes);
            deploy_refusal(&found, &name).expect("deploy is offered");

            // Deploy, public network, as the environment admin only.
            let vars = environment_vars(&name, &t.e.region, &NetworkChoice::Public, 0).unwrap();
            let (_, outcome, denied) = t
                .run(
                    ModuleKind::Environment,
                    Deployment::Environment(&name),
                    &vars,
                    TofuOp::Apply,
                    t.keys_env(&keys),
                )
                .await;
            assert_ok("deploy", &outcome, &denied);

            // Detected Ready with a valid record, joinable, not joined yet.
            let found = t.scan(&cfg, &who).await;
            let env = found
                .environments
                .iter()
                .find(|x| x.name == name)
                .unwrap_or_else(|| panic!("{name} not detected: {found:#?}"));
            assert_eq!(env.state, EnvironmentState::Ready, "{env:#?}");
            let info = env.info.clone().expect("a valid record");
            assert_eq!(info.network_mode, "public");
            assert!(info.assign_public_ip);
            assert_eq!(info.subnet_ids.len(), 2);
            assert!(env.actions.contains(&EnvironmentAction::Join), "{env:#?}");
            assert_eq!(env.onboarding, Some(OnboardingState::NotJoined));

            // A stale lock's conditional delete: a mismatched ETag is refused
            // (412), the current one removes it.
            {
                let s3 = aws_sdk_s3::Client::new(&cfg);
                let lock_key = format!("{}.tflock", state_key(Deployment::Environment(&name)));
                let put = ok(
                    "put a fake lock",
                    s3.put_object()
                        .bucket(&t.bucket)
                        .key(&lock_key)
                        .body(r#"{"ID":"e2e","Operation":"OperationTypeApply","Who":"e2e@test","Created":"2026-01-01T00:00:00Z"}"#.as_bytes().to_vec().into())
                        .expected_bucket_owner(&who.account)
                        .send()
                        .await,
                );
                let etag = put.e_tag().expect("etag").to_string();
                let head = ok(
                    "head the lock",
                    s3.head_object().bucket(&t.bucket).key(&lock_key).send().await,
                );
                assert_eq!(head.e_tag(), Some(etag.as_str()));
                let found = t.scan(&cfg, &who).await;
                let env = found.environments.iter().find(|x| x.name == name).unwrap();
                assert!(
                    matches!(&env.state, EnvironmentState::Deploying { lock: Some(l) } if l.who == "e2e@test"),
                    "a fresh foreign lock reads as Deploying: {env:#?}"
                );
                let wrong = delete_lock_if_unchanged(
                    &s3,
                    &t.bucket,
                    &who.account,
                    &lock_key,
                    "\"00000000000000000000000000000000\"",
                )
                .await
                .expect_err("a mismatched ETag must not delete the lock");
                assert!(wrong.to_string().contains(LOCK_CHANGED), "{wrong}");
                ok(
                    "the lock survives a mismatched delete",
                    s3.head_object().bucket(&t.bucket).key(&lock_key).send().await,
                );
                delete_lock_if_unchanged(&s3, &t.bucket, &who.account, &lock_key, &etag)
                    .await
                    .unwrap_or_else(|e| panic!("conditional delete with the current ETag: {e}"));
                let gone = s3.head_object().bucket(&t.bucket).key(&lock_key).send().await;
                assert!(gone.is_err(), "the lock is still there after the delete");
                eprintln!("conditional unlock: 412 on a mismatched ETag, deleted with the current one");
            }

            // Self-onboarding as the environment admin.
            let (keys, cfg, who) = t.session(&admin).await;
            let found = t.scan(&cfg, &who).await;
            onboard_refusal(&found, &name).expect("join is offered");
            let owner = validate_owner_id(&who.user_id).expect("owner id");
            let okey = owner_key(&t.e.region, &owner);
            let (_, outcome, denied) = t
                .run(
                    ModuleKind::UserOnboard,
                    Deployment::User { env: &name, owner_key: &okey },
                    &onboarding_vars(&name, &t.e.region, &owner, None).unwrap(),
                    TofuOp::Apply,
                    t.keys_env(&keys),
                )
                .await;
            assert_ok("onboard", &outcome, &denied);
            let found = t.scan(&cfg, &who).await;
            let env = found.environments.iter().find(|x| x.name == name).unwrap();
            // Joined from the state's onboarding_complete output, not just the role.
            assert_eq!(env.onboarding, Some(OnboardingState::Joined { ec2: None }), "{env:#?}");
            assert_eq!(env.onboarded_users, Some(1), "{env:#?}");
            let (task_role, exec_role) = user_role_names(&name, &okey);
            eprintln!("onboarded with region-scoped roles {task_role} / {exec_role}");
            for r in [&task_role, &exec_role] {
                let role = ok("get the onboarded role", t.e.iam.get_role().role_name(r).send().await);
                let boundary = role
                    .role()
                    .and_then(|r| r.permissions_boundary())
                    .and_then(|b| b.permissions_boundary_arn())
                    .unwrap_or_default()
                    .to_string();
                assert!(boundary.ends_with(&format!(":policy/{PERMISSIONS_BOUNDARY_NAME}")), "{r}: {boundary}");
            }

            // Update: the recorded network, the same module version: nothing
            // to change.
            let network = network_from_record(&info, None).expect("network from record");
            let vars = environment_vars(&name, &t.e.region, &network, 0).unwrap();
            let (_, outcome, denied) = t
                .run(
                    ModuleKind::Environment,
                    Deployment::Environment(&name),
                    &vars,
                    TofuOp::Apply,
                    t.keys_env(&keys),
                )
                .await;
            assert_ok("update", &outcome, &denied);
            let summary = outcome.summary.clone().unwrap_or_default();
            assert!(
                summary.contains("0 added, 0 changed, 0 destroyed"),
                "the update changed something: {summary}"
            );

            // Remove: the onboarding, then the environment (an admin's
            // `tofu destroy` from a bundle; the same module and state).
            let (keys, _, _) = t.session(&admin).await;
            let (_, outcome, denied) = t
                .run(
                    ModuleKind::UserOnboard,
                    Deployment::User { env: &name, owner_key: &okey },
                    &onboarding_vars(&name, &t.e.region, &owner, None).unwrap(),
                    TofuOp::Destroy,
                    t.keys_env(&keys),
                )
                .await;
            assert_ok("destroy the onboarding", &outcome, &denied);
            // The emptied onboarding state and the deleted roles: not joined.
            let found = t.scan(&cfg, &who).await;
            let env = found.environments.iter().find(|x| x.name == name).unwrap();
            assert_eq!(env.onboarding, Some(OnboardingState::NotJoined), "{env:#?}");
            assert_eq!(env.onboarded_users, Some(0), "{env:#?}");
            let (_, outcome, denied) = t
                .run(
                    ModuleKind::Environment,
                    Deployment::Environment(&name),
                    &vars,
                    TofuOp::Destroy,
                    t.keys_env(&keys),
                )
                .await;
            assert_ok("destroy the environment", &outcome, &denied);

            // `tofu destroy` leaves an empty state object, which detection
            // must read as no deployment at all.
            let state = ok(
                "read the destroyed state",
                aws_sdk_s3::Client::new(&cfg)
                    .get_object()
                    .bucket(&t.bucket)
                    .key(state_key(Deployment::Environment(&name)))
                    .send()
                    .await,
            );
            let bytes = state.body.collect().await.unwrap().into_bytes();
            assert!(
                is_empty_state(&String::from_utf8_lossy(&bytes)),
                "the destroyed state is not empty: {}",
                String::from_utf8_lossy(&bytes)
            );
            let found = t.scan(&cfg, &who).await;
            assert!(
                found.environments.iter().all(|x| x.name != name),
                "{name} still detected after destroy: {found:#?}"
            );

            // The boundary, by its module, if this test made it.
            if *t.boundary_created.lock().unwrap() {
                let (_, outcome, denied) = t
                    .run(
                        ModuleKind::PermissionsBoundary,
                        Deployment::Boundary,
                        &boundary_vars(&t.e.region),
                        TofuOp::Destroy,
                        t.admin_env(),
                    )
                    .await;
                assert_ok("destroy the boundary", &outcome, &denied);
            }
        })
        .await;
    }

    /// About $0.05: one NAT gateway and its address for a few minutes.
    #[tokio::test]
    #[ignore]
    async fn the_private_nat_mode_deploys() {
        env_e2e(|t| async move {
            let admin = t.admin().await;
            let (keys, cfg, who) = t.session(&admin).await;
            t.ensure_bucket(&cfg, &who).await;
            let name = format!("e2e{}", &uuid::Uuid::new_v4().simple().to_string()[..6]);
            let vars = environment_vars(&name, &t.e.region, &NetworkChoice::PrivateNat, 0).unwrap();
            let (_, outcome, denied) = t
                .run(
                    ModuleKind::Environment,
                    Deployment::Environment(&name),
                    &vars,
                    TofuOp::Apply,
                    t.keys_env(&keys),
                )
                .await;
            assert_ok("deploy private-nat", &outcome, &denied);
            let found = t.scan(&cfg, &who).await;
            let env = found.environments.iter().find(|x| x.name == name).unwrap();
            assert_eq!(env.state, EnvironmentState::Ready, "{env:#?}");
            let info = env.info.as_ref().unwrap();
            assert_eq!(info.network_mode, "private-nat");
            assert!(!info.assign_public_ip);
            let (keys, _, _) = t.session(&admin).await;
            let (_, outcome, denied) = t
                .run(
                    ModuleKind::Environment,
                    Deployment::Environment(&name),
                    &vars,
                    TofuOp::Destroy,
                    t.keys_env(&keys),
                )
                .await;
            assert_ok("destroy private-nat", &outcome, &denied);
        })
        .await;
    }

    // ─────────────────────────────────────────────── phase 5: a workspace

    /// What the phase-5 test made outside the modules, for its own guard
    /// (which runs before the environment fixture's destroy).
    #[derive(Default)]
    struct Phase5 {
        /// The onboarded execution role holding the test-only ECR policy.
        exec_role: Option<String>,
        cluster: Option<String>,
        /// Task definition family prefixes to retire (`triple-c-<env>-`).
        families: Vec<String>,
        /// The user's file system's creation token.
        fs_token: Option<String>,
        secrets: Vec<String>,
        /// The project whose tunnel to close.
        project_id: Option<String>,
        /// `AWS_CONFIG_FILE` before the test pointed it at its own file
        /// (outer `None`: not changed).
        config_file_before: Option<Option<std::ffi::OsString>>,
        /// `TRIPLEC_E2E_ECR_REPO`: the throwaway repository to force-delete.
        ecr_repo: Option<String>,
        /// Phase 7: the environment whose per-user EC2 capacity
        /// (`triple-c-<env>--u-*`) the guard tears down, and the user's
        /// owner key (instance role, user record).
        ec2_env: Option<String>,
        ec2_okey: Option<String>,
        /// Auto Scaling groups a probe made outside that prefix.
        probe_groups: Vec<String>,
    }

    /// The whole phase-5 body: a hang anywhere becomes a panic the guard
    /// catches, so nothing billed outlives it.
    const PHASE5_TIMEOUT: Duration = Duration::from_secs(45 * 60);
    /// One exec through the agent.
    const SH_TIMEOUT: Duration = Duration::from_secs(180);

    /// The phase-5 guard's state, even after a panic poisoned the lock.
    fn p5_lock(m: &std::sync::Mutex<Phase5>) -> std::sync::MutexGuard<'_, Phase5> {
        m.lock().unwrap_or_else(|e| e.into_inner())
    }

    const ECR_POLICY: &str = "triple-c-e2e-ecr-pull";

    /// As the account admin: stop every task in the cluster and wait for
    /// them, retire the test's task definitions, delete the user's access
    /// points and the project secret (no recovery window), and take the
    /// test-only ECR policy off the execution role, so the modules'
    /// `tofu destroy` can remove the rest. Logs, never fails.
    async fn phase5_cleanup(e: &E2e, p: &Phase5) {
        if let Some(id) = &p.project_id {
            crate::host::tunnels::close(id).await;
        }
        match &p.config_file_before {
            Some(Some(v)) => std::env::set_var("AWS_CONFIG_FILE", v),
            Some(None) => std::env::remove_var("AWS_CONFIG_FILE"),
            None => {}
        }
        if let Some(env) = &p.ec2_env {
            ec2_cleanup(e, env, p.ec2_okey.as_deref(), &p.probe_groups).await;
        }
        let ecs = aws_sdk_ecs::Client::new(&e.cfg);
        if let Some(cluster) = &p.cluster {
            let mut arns = Vec::new();
            for status in [aws_sdk_ecs::types::DesiredStatus::Running, aws_sdk_ecs::types::DesiredStatus::Pending] {
                if let Ok(out) = ecs.list_tasks().cluster(cluster).desired_status(status).send().await {
                    arns.extend(out.task_arns().iter().cloned());
                }
            }
            for arn in &arns {
                eprintln!("cleanup: stopping task {arn}");
                if let Err(err) = ecs.stop_task().cluster(cluster).task(arn).reason("triple-c-e2e cleanup").send().await {
                    eprintln!("cleanup: stop {arn}: {err:?}");
                }
            }
            // Until none is left running (a stop takes up to stopTimeout).
            for _ in 0..40 {
                let mut live = 0;
                if !arns.is_empty() {
                    if let Ok(out) = ecs.describe_tasks().cluster(cluster).set_tasks(Some(arns.clone())).send().await {
                        live = out.tasks().iter().filter(|t| t.last_status() != Some("STOPPED")).count();
                    }
                }
                if live == 0 {
                    break;
                }
                tokio::time::sleep(Duration::from_secs(10)).await;
            }
        }
        for prefix in &p.families {
            let mut defs = Vec::new();
            for status in [aws_sdk_ecs::types::TaskDefinitionStatus::Active, aws_sdk_ecs::types::TaskDefinitionStatus::Inactive] {
                let mut next = None;
                loop {
                    // No `family_prefix`: ECS matches it as a whole family
                    // name, so the test's `triple-c-<env>-` matched nothing.
                    let Ok(out) = ecs
                        .list_task_definitions()
                        .status(status.clone())
                        .set_next_token(next.take())
                        .send()
                        .await
                    else {
                        break;
                    };
                    let ours = format!(":task-definition/{prefix}");
                    defs.extend(out.task_definition_arns().iter().filter(|a| a.contains(&ours)).cloned());
                    next = out.next_token().map(str::to_string);
                    if next.is_none() {
                        break;
                    }
                }
            }
            for arn in &defs {
                let _ = ecs.deregister_task_definition().task_definition(arn).send().await;
            }
            for chunk in defs.chunks(10) {
                if let Err(err) = ecs.delete_task_definitions().set_task_definitions(Some(chunk.to_vec())).send().await {
                    eprintln!("cleanup: delete task definitions: {err:?}");
                }
            }
            if !defs.is_empty() {
                eprintln!("cleanup: retired {} task definition revision(s) of {prefix}*", defs.len());
            }
        }
        if let Some(token) = &p.fs_token {
            let efs = aws_sdk_efs::Client::new(&e.cfg);
            if let Ok(out) = efs.describe_file_systems().creation_token(token).send().await {
                for fs in out.file_systems() {
                    if let Ok(aps) = efs.describe_access_points().file_system_id(fs.file_system_id()).send().await {
                        for ap in aps.access_points() {
                            let id = ap.access_point_id().unwrap_or_default();
                            eprintln!("cleanup: deleting access point {id}");
                            if let Err(err) = efs.delete_access_point().access_point_id(id).send().await {
                                eprintln!("cleanup: delete access point {id}: {err:?}");
                            }
                        }
                    }
                }
            }
        }
        for name in &p.secrets {
            match e.secrets.delete_secret().secret_id(name).force_delete_without_recovery(true).send().await {
                Ok(_) => eprintln!("cleanup: deleted secret {name} (no recovery window)"),
                Err(err) => {
                    let err = classify_sdk_error("secretsmanager:DeleteSecret", &err);
                    if !err.to_string().contains("ResourceNotFound") {
                        eprintln!("cleanup: delete secret {name}: {err}");
                    }
                }
            }
        }
        if let Some(role) = &p.exec_role {
            match e.iam.delete_role_policy().role_name(role).policy_name(ECR_POLICY).send().await {
                Ok(_) => eprintln!("cleanup: removed {ECR_POLICY} from {role}"),
                Err(err) => {
                    if !format!("{err:?}").contains("NoSuchEntity") {
                        eprintln!("cleanup: remove {ECR_POLICY} from {role}: {err:?}");
                    }
                }
            }
        }
            if let Some(repo) = &p.ecr_repo {
            // No ECR crate in the app: the AWS CLI, as the e2e profile.
            let out = tokio::process::Command::new("aws")
                .args(["--profile", &super::var("TRIPLEC_AWS_E2E_PROFILE"), "--region", &e.region])
                .args(["ecr", "delete-repository", "--repository-name", repo, "--force"])
                .output()
                .await;
            match out {
                Ok(o) if o.status.success() => eprintln!("cleanup: deleted ECR repository {repo}"),
                Ok(o) if String::from_utf8_lossy(&o.stderr).contains("RepositoryNotFound") => {}
                other => eprintln!("cleanup: delete ECR repository {repo}: {other:?}"),
            }
        }
    }

    /// The AWS CLI as the e2e profile (`keys`: as that session instead):
    /// (success, stdout, stderr). The app has no EC2 SDK crate.
    async fn aws_cli(keys: Option<&AwsKeys>, region: &str, args: &[&str]) -> (bool, String, String) {
        let mut cmd = tokio::process::Command::new("aws");
        cmd.args(["--region", region, "--output", "json"]);
        match keys {
            None => {
                cmd.args(["--profile", &super::var("TRIPLEC_AWS_E2E_PROFILE")]);
            }
            Some(k) => {
                cmd.env_remove("AWS_PROFILE")
                    .env("AWS_ACCESS_KEY_ID", &k.access_key_id)
                    .env("AWS_SECRET_ACCESS_KEY", &k.secret_access_key);
                match &k.session_token {
                    Some(t) => cmd.env("AWS_SESSION_TOKEN", t),
                    None => cmd.env_remove("AWS_SESSION_TOKEN"),
                };
            }
        }
        cmd.args(args);
        match tokio::time::timeout(Duration::from_secs(120), cmd.output()).await {
            Ok(Ok(o)) => (
                o.status.success(),
                String::from_utf8_lossy(&o.stdout).into_owned(),
                String::from_utf8_lossy(&o.stderr).into_owned(),
            ),
            Ok(Err(err)) => (false, String::new(), format!("aws cli: {err}")),
            Err(_) => (false, String::new(), "aws cli: timed out".into()),
        }
    }

    /// Phase 7's guard, as the account admin: everything of `env`'s per-user
    /// EC2 capacity (`triple-c-<env>--u-*`) and the probes' groups, so no
    /// instance outlives a failed run. Tasks on the per-user clusters are
    /// stopped, groups force-deleted (terminating their instances) and waited
    /// for, leftover tagged instances terminated, then capacity providers,
    /// container instances, clusters, launch templates, the user record and
    /// the instance role/profile deleted. A resource already gone is fine
    /// (the modules' destroy then finds nothing). Logs, never fails.
    async fn ec2_cleanup(e: &E2e, env: &str, okey: Option<&str>, probe_groups: &[String]) {
        let prefix = format!("triple-c-{env}--u-");
        let ecs = aws_sdk_ecs::Client::new(&e.cfg);
        let asg = aws_sdk_autoscaling::Client::new(&e.cfg);
        // Tasks on the per-user clusters.
        let mut clusters = Vec::new();
        if let Ok(out) = ecs.list_clusters().send().await {
            clusters.extend(out.cluster_arns().iter().filter(|a| a.contains(&format!(":cluster/{prefix}"))).cloned());
        }
        for cluster in &clusters {
            if let Ok(out) = ecs.list_tasks().cluster(cluster).send().await {
                for arn in out.task_arns() {
                    eprintln!("cleanup: stopping EC2 task {arn}");
                    let _ = ecs.stop_task().cluster(cluster).task(arn).reason("triple-c-e2e cleanup").send().await;
                }
            }
        }
        // Groups (force: terminates their instances, ignores protection).
        let mut groups: Vec<String> = probe_groups.to_vec();
        if let Ok(out) = asg.describe_auto_scaling_groups().send().await {
            for g in out.auto_scaling_groups() {
                let n = g.auto_scaling_group_name().unwrap_or_default();
                if n.starts_with(&prefix) && !groups.iter().any(|x| x == n) {
                    groups.push(n.to_string());
                }
            }
        }
        for g in &groups {
            match asg.delete_auto_scaling_group().auto_scaling_group_name(g).force_delete(true).send().await {
                Ok(_) => eprintln!("cleanup: force-deleting Auto Scaling group {g}"),
                Err(err) => {
                    let msg = format!("{err:?}");
                    if !msg.contains("not found") && !msg.contains("ValidationError") {
                        eprintln!("cleanup: delete group {g}: {msg}");
                    }
                }
            }
        }
        if !groups.is_empty() {
            for _ in 0..60 {
                let left = asg
                    .describe_auto_scaling_groups()
                    .set_auto_scaling_group_names(Some(groups.clone()))
                    .send()
                    .await
                    .map(|o| o.auto_scaling_groups().len())
                    .unwrap_or(0);
                if left == 0 {
                    break;
                }
                tokio::time::sleep(Duration::from_secs(10)).await;
            }
        }
        // Any instance still tagged with this environment.
        let (ok_, out, err) = aws_cli(
            None,
            &e.region,
            &[
                "ec2", "describe-instances",
                "--filters", &format!("Name=tag:triple-c:environment,Values={env}"),
                "Name=instance-state-name,Values=pending,running,stopping,stopped",
                "--query", "Reservations[].Instances[].InstanceId",
            ],
        )
        .await;
        if ok_ {
            let ids: Vec<String> = serde_json::from_str(&out).unwrap_or_default();
            if !ids.is_empty() {
                let mut args = vec!["ec2", "terminate-instances", "--instance-ids"];
                args.extend(ids.iter().map(String::as_str));
                let r = aws_cli(None, &e.region, &args).await;
                eprintln!("cleanup: terminate {ids:?}: ok {}", r.0);
                let mut args = vec!["ec2", "wait", "instance-terminated", "--instance-ids"];
                args.extend(ids.iter().map(String::as_str));
                let _ = aws_cli(None, &e.region, &args).await;
            }
        } else {
            eprintln!("cleanup: describe instances: {err}");
        }
        // Capacity providers: off their clusters, then deleted.
        for cluster in &clusters {
            let _ = ecs.put_cluster_capacity_providers().cluster(cluster).set_capacity_providers(Some(vec![])).set_default_capacity_provider_strategy(Some(vec![])).send().await;
        }
        if let Ok(out) = ecs.describe_capacity_providers().send().await {
            for cp in out.capacity_providers() {
                let n = cp.name().unwrap_or_default();
                if n.starts_with(&prefix) && cp.status() != Some(&aws_sdk_ecs::types::CapacityProviderStatus::Inactive) {
                    match ecs.delete_capacity_provider().capacity_provider(n).send().await {
                        Ok(_) => eprintln!("cleanup: deleted capacity provider {n}"),
                        Err(err) => eprintln!("cleanup: delete capacity provider {n}: {err:?}"),
                    }
                }
            }
        }
        for cluster in &clusters {
            if let Ok(out) = ecs.list_container_instances().cluster(cluster).send().await {
                for ci in out.container_instance_arns() {
                    let _ = ecs.deregister_container_instance().cluster(cluster).container_instance(ci).force(true).send().await;
                }
            }
            // Busy (UpdateInProgress) while the provider detaches: retry.
            for attempt in 0..12 {
                match ecs.delete_cluster().cluster(cluster).send().await {
                    Ok(_) => {
                        eprintln!("cleanup: deleted cluster {cluster}");
                        break;
                    }
                    Err(err) if attempt < 11 && format!("{err:?}").contains("UpdateInProgress") => {
                        tokio::time::sleep(Duration::from_secs(10)).await;
                    }
                    Err(err) => {
                        eprintln!("cleanup: delete cluster {cluster}: {err:?}");
                        break;
                    }
                }
            }
        }
        // Launch templates.
        let (ok_, out, _) = aws_cli(
            None,
            &e.region,
            &["ec2", "describe-launch-templates", "--filters", &format!("Name=launch-template-name,Values={prefix}*"), "--query", "LaunchTemplates[].LaunchTemplateName"],
        )
        .await;
        if ok_ {
            let names: Vec<String> = serde_json::from_str(&out).unwrap_or_default();
            for n in names {
                let r = aws_cli(None, &e.region, &["ec2", "delete-launch-template", "--launch-template-name", &n]).await;
                eprintln!("cleanup: delete launch template {n}: ok {} {}", r.0, r.2.trim());
            }
        }
        if let Some(okey) = okey {
            let ssm = aws_sdk_ssm::Client::new(&e.cfg);
            let param = crate::aws::tofu_modules::user_record_parameter_name(env, okey);
            if ssm.delete_parameter().name(&param).send().await.is_ok() {
                eprintln!("cleanup: deleted {param}");
            }
            let role = crate::aws::tofu_modules::user_instance_role_name(env, okey);
            if e.iam.remove_role_from_instance_profile().instance_profile_name(&role).role_name(&role).send().await.is_ok() {
                eprintln!("cleanup: removed {role} from its instance profile");
            }
            if e.iam.delete_instance_profile().instance_profile_name(&role).send().await.is_ok() {
                eprintln!("cleanup: deleted instance profile {role}");
            }
            if let Ok(out) = e.iam.list_role_policies().role_name(&role).send().await {
                for pn in out.policy_names() {
                    let _ = e.iam.delete_role_policy().role_name(&role).policy_name(pn).send().await;
                }
                match e.iam.delete_role().role_name(&role).send().await {
                    Ok(_) => eprintln!("cleanup: deleted role {role}"),
                    Err(err) => eprintln!("cleanup: delete role {role}: {err:?}"),
                }
            }
        }
    }

    /// Poll `f` every `every` until it returns `Some`, or `None` after `within`.
    async fn poll<T, F, Fut>(within: Duration, every: Duration, mut f: F) -> Option<T>
    where
        F: FnMut() -> Fut,
        Fut: std::future::Future<Output = Option<T>>,
    {
        let deadline = std::time::Instant::now() + within;
        loop {
            if let Some(v) = f().await {
                return Some(v);
            }
            if std::time::Instant::now() + every > deadline {
                return None;
            }
            tokio::time::sleep(every).await;
        }
    }

    /// `sh -c script` as `user`: (output, exit code).
    async fn sh(t: &crate::host::ContainerTarget, user: &str, script: &str) -> (String, i64) {
        tokio::time::timeout(
            SH_TIMEOUT,
            t.exec_oneshot_as(user, vec!["sh".into(), "-c".into(), script.into()], Vec::new()),
        )
        .await
        .unwrap_or_else(|_| panic!("exec `{script}` did not finish in {SH_TIMEOUT:?}"))
        .unwrap_or_else(|err| panic!("exec `{script}`: {err}"))
    }

    /// What [`deploy_for_workspace_user`] set up.
    struct Deployed {
        region: String,
        /// The environment's name.
        name: String,
        /// The workspace user's `aws:userid`.
        owner: String,
        okey: String,
        fs_token: String,
        /// The workspace user's profile connection.
        conn: HostConnection,
        ws_cfg: SdkConfig,
        ws: CallerIdentity,
        /// The environment admin role (fresh sessions via `Env::session`).
        admin: Admin,
    }

    /// The workspace tests' common start: the environment (with
    /// `max_workspace_hours`) deployed by the environment admin, a role
    /// holding only the workspace-user files onboarded to it, the test-only
    /// ECR pull policy for `image`, and that role's profile connection
    /// (`AWS_CONFIG_FILE` pointed at a config file this writes). Everything
    /// made is recorded in `p5` / the fixture for the guards.
    async fn deploy_for_workspace_user(
        t: &Env,
        p5: &std::sync::Mutex<Phase5>,
        image: &str,
        max_workspace_hours: u32,
        ec2: Option<&crate::aws::tofu_modules::Ec2Compute>,
    ) -> Deployed {
        use crate::aws::tofu_modules::{environment_cluster_name, user_file_system_token};

        let region = t.e.region.clone();
        let admin = t.admin().await;
        let (keys, cfg, who) = t.session(&admin).await;
        t.ensure_bucket(&cfg, &who).await;
        t.ensure_boundary().await;

        // ── the environment, deployed by the environment admin ──
        let name = format!("e2e{}", &uuid::Uuid::new_v4().simple().to_string()[..6]);
        let (cluster, families) = (environment_cluster_name(&name), format!("triple-c-{name}-"));
        {
            let mut g = p5_lock(p5);
            g.cluster = Some(cluster);
            g.families.push(families);
        }
        let vars = environment_vars(&name, &region, &NetworkChoice::Public, max_workspace_hours).unwrap();
        let (_, outcome, denied) = t
            .run(ModuleKind::Environment, Deployment::Environment(&name), &vars, TofuOp::Apply, t.keys_env(&keys))
            .await;
        assert_ok("deploy", &outcome, &denied);

        // ── the workspace user: only the workspace files ──
        let user = t.e.tier_role("wsuser", &[Tier::WorkspaceUser]).await;
        let owner = validate_owner_id(&user.session_user_id()).expect("owner id");
        let okey = owner_key(&region, &owner);
        let fs_token = user_file_system_token(&name, &okey);
        p5_lock(p5).fs_token = Some(fs_token.clone());

        // Onboarded by the admin (an onboarding from an admin bundle). With
        // EC2 capacity (phase 7): the user's role carries the triple-c-user
        // principal tag, and the vars come from the app's own pre-apply
        // step (record, choice, name-collision check) as the admin.
        let (keys, admin_cfg, _) = t.session(&admin).await;
        let vars = match ec2 {
            None => onboarding_vars(&name, &region, &owner, None).unwrap(),
            Some(c) => {
                use crate::aws::tofu_modules::EC2_USER_TAG_KEY;
                use crate::commands::environment_commands::{prepare_onboarding, Ec2Choice};
                t.e.tag_role(&user, EC2_USER_TAG_KEY, &c.user_tag).await;
                {
                    let mut g = p5_lock(p5);
                    g.ec2_env = Some(name.clone());
                    g.ec2_okey = Some(okey.clone());
                }
                let api = crate::aws::environments::SdkOnboardApi::new(&admin_cfg);
                let t0 = std::time::Instant::now();
                let vars = prepare_onboarding(&api, &name, &region, &t.e.account, &owner, Some(Ec2Choice::Ec2(c.clone())))
                    .await
                    .unwrap_or_else(|e| panic!("the pre-apply onboarding step: {e}"));
                eprintln!("RESULT pre-apply EC2 collision check (free names) passed in {:?}: {vars}", t0.elapsed());
                vars
            }
        };
        let t0 = std::time::Instant::now();
        let (_, outcome, denied) = t
            .run(
                ModuleKind::UserOnboard,
                Deployment::User { env: &name, owner_key: &okey },
                &vars,
                TofuOp::Apply,
                t.keys_env(&keys),
            )
            .await;
        assert_ok("onboard the workspace user", &outcome, &denied);
        eprintln!("RESULT onboarding apply: {:?}", t0.elapsed());
        let (_, exec_role) = user_role_names(&name, &okey);

        // Test-only: pull from the throwaway private ECR repository.
        if let Some((registry, rest)) = image.split_once('/') {
            if registry.contains(".dkr.ecr.") {
                let repo = rest.split([':', '@']).next().unwrap();
                let doc = format!(
                    r#"{{"Version":"2012-10-17","Statement":[{{"Effect":"Allow","Action":"ecr:GetAuthorizationToken","Resource":"*"}},{{"Effect":"Allow","Action":["ecr:BatchGetImage","ecr:GetDownloadUrlForLayer","ecr:BatchCheckLayerAvailability"],"Resource":"arn:aws:ecr:{region}:{}:repository/{repo}"}}]}}"#,
                    t.e.account
                );
                p5_lock(p5).exec_role = Some(exec_role.clone());
                ok(
                    "put the test-only ECR policy",
                    t.e.iam.put_role_policy().role_name(&exec_role).policy_name(ECR_POLICY).policy_document(doc).send().await,
                );
                eprintln!("test-only: {ECR_POLICY} on {exec_role} for {repo}");
            }
        }

        // The workspace user's profile connection.
        let profile = format!("{}-wsuser", t.e.prefix);
        let cfg_file = t.root.join(format!("{}-config", t.e.prefix));
        std::fs::write(
            &cfg_file,
            format!(
                "[profile {profile}]\nrole_arn = {}\nsource_profile = {}\nrole_session_name = {}\n",
                user.arn,
                super::var("TRIPLEC_AWS_E2E_PROFILE"),
                super::SESSION
            ),
        )
        .unwrap();
        let before = std::env::var_os("AWS_CONFIG_FILE");
        p5_lock(p5).config_file_before = Some(before);
        std::env::set_var("AWS_CONFIG_FILE", &cfg_file);
        let conn = HostConnection {
            id: "e2e-wsuser".into(),
            name: "e2e-wsuser".into(),
            kind: HostKind::Aws {
                auth: AwsAuth::Profile { name: profile.clone() },
                default_region: region.clone(),
            },
        };
        let ws_cfg = crate::aws::sdk_config(&conn, &region).await.expect("workspace user config");
        let ws = caller_identity(&ws_cfg).await.expect("workspace user identity");
        assert_eq!(ws.user_id, owner, "the profile session is the onboarded owner");
        eprintln!("workspace user: {}", ws.arn);
        Deployed { region, name, owner, okey, fs_token, conn, ws_cfg, ws, admin }
    }

    /// Phase 5: a Fargate workspace's whole life through `ProjectHost::Aws`,
    /// as a role holding **only** the workspace-user files, in an
    /// environment the environment admin deployed and onboarded it to.
    ///
    /// ```sh
    /// cd app/src-tauri
    /// cargo build
    /// TRIPLEC_AWS_E2E_PROFILE=… TRIPLEC_AWS_E2E_REGION=us-west-2 \
    /// TRIPLEC_E2E_IMAGE=<account>.dkr.ecr.<region>.amazonaws.com/triple-c-e2e-sandbox:<tag> \
    ///   cargo test aws::live_tests::environment::a_fargate_workspace_round_trips \
    ///   -- --ignored --test-threads=1 --nocapture
    /// ```
    ///
    /// `TRIPLEC_E2E_IMAGE` is the image the project names as its Custom
    /// image: one built from this branch (`docker build --platform
    /// linux/amd64 ./container`) and pushed to a registry Fargate can pull.
    /// For a private ECR repository in the test account, the test gives the
    /// onboarded execution role a **test-only** inline pull policy
    /// (`triple-c-e2e-ecr-pull`; the boundary allows ECR pulls, the
    /// onboarding module grants none) and removes it before the onboarding
    /// is destroyed. `scripts/aws-e2e-workspace.sh` does it all: builds the
    /// image, creates the throwaway repository `triple-c-e2e-sandbox`,
    /// pushes, runs this test with `TRIPLEC_E2E_ECR_REPO` set (this test's
    /// guard then force-deletes that repository too) and deletes it in a
    /// trap on exit.
    ///
    /// The body runs under a 45-minute timeout and every exec under its own,
    /// so a hang becomes a panic and the guard still runs.
    ///
    /// The workspace user signs in the way a saved profile connection does:
    /// a profile (`role_arn` + `source_profile` = the e2e profile, session
    /// name `triple-c-e2e`) in a config file this test writes and points
    /// `AWS_CONFIG_FILE` at. About 30 Fargate task-minutes at 1 vCPU / 2 GB.
    #[tokio::test]
    #[ignore]
    async fn a_fargate_workspace_round_trips() {
        use crate::aws::runtime::SdkRuntimeApi;
        use crate::aws::tasks::{run_task_request, task_log_stream, SdkTaskApi, TaskApi};
        use crate::aws::tofu_modules::{
log_stream_prefix, project_secret_name, task_family};
        use crate::aws::workspace::workspace_tags;
        use crate::host::{lifecycle::ProjectHost, tunnels};
        use crate::models::app_settings::ImageSource;
        use crate::models::project::{AwsPlacement, AwsWorkspaceConfig, RepoSource, StorageMode};
        use crate::models::{AppSettings, Project};
        use crate::storage::projects_store::ProjectsStore;

        let image = super::var("TRIPLEC_E2E_IMAGE");
        let ecr_repo = std::env::var("TRIPLEC_E2E_ECR_REPO").ok().filter(|r| !r.is_empty());
        env_e2e(|t| async move {
            let p5 = std::sync::Mutex::new(Phase5 { ecr_repo, ..Default::default() });
            // Boxed: the body's state machine is far larger than a test
            // thread's 2 MiB stack once it is moved into the timeout.
            let inner = Box::pin(async {
                let Deployed { region, name, owner, okey, fs_token, conn, ws_cfg, ws, .. } =
                    deploy_for_workspace_user(&t, &p5, &image, 0, None).await;

                let mut settings = AppSettings::default();
                settings.hosts = vec![conn];
                settings.image_source = ImageSource::Custom;
                settings.custom_image_name = Some(image.clone());

                let mut project = Project::new("e2e-fargate".into(), Vec::new());
                project.host_id = Some("e2e-wsuser".into());
                project.aws_placement = Some(AwsPlacement { region: region.clone(), environment: name.clone() });
                project.aws = Some(AwsWorkspaceConfig {
                    cpu: 1024,
                    memory_mib: 2048,
                    storage: StorageMode::Hybrid,
                    repos: vec![RepoSource {
                        url: "https://github.com/octocat/Hello-World.git".into(),
                        branch: None,
                        folder: "Hello-World".into(),
                    }],
                    wip_push_on_stop: true,
                    // On from the first start: turning it on later adds a
                    // secret key, so the next start registers a new revision.
                    // The test's own execs keep task 1 busy until its stop.
                    idle_stop_minutes: Some(5),
                    ..Default::default()
                });
                let id = project.id.clone();
                let secret_name = project_secret_name(&name, &okey, &id);
                {
                    let mut g = p5_lock(&p5);
                    g.project_id = Some(id.clone());
                    g.secrets.push(secret_name);
                }
                let store_dir = tempfile::tempdir().unwrap();
                let store = ProjectsStore::over_file_for_test(store_dir.path().join("projects.json"), vec![project.clone()]);
                let host = ProjectHost::for_project(&project);
                assert_eq!(host, ProjectHost::Aws);
                let progress = |m: &str| eprintln!("  progress: {m}");
                let api = SdkTaskApi::new(&ws_cfg);
                let env = crate::aws::runtime::read_environment(&SdkRuntimeApi::new(&ws_cfg), &name, &ws.account, &region)
                    .await
                    .expect("the environment record, as the workspace user");
                let family = task_family(&name, &okey, &id);
                let stream_prefix = log_stream_prefix(&okey, &id);
                let current = |store: &ProjectsStore| store.get(&id).unwrap();

                // ── start 1 ──
                let t0 = std::time::Instant::now();
                let target = host.ensure_started(&current(&store), &settings, &store, &progress).await.expect("start 1");
                let first_start = t0.elapsed();
                let p = current(&store);
                let aws = p.aws.clone().unwrap();
                let task1 = aws.task_arn.clone().expect("the task ARN is stored");
                let revision1 = aws.task_definition_arn.clone().expect("the task definition is stored");
                eprintln!("RESULT start 1 (cold: register, access point, RunTask, pull, agent, tunnel): {first_start:?}; task {task1}; {revision1}");
                assert!(revision1.contains(&format!(":task-definition/{family}:")), "{revision1}");
                assert!(aws.access_point_id.is_some(), "Hybrid has an access point");

                // ECS's own health check (startPeriod 120 s, interval 30 s).
                let health = poll(Duration::from_secs(300), Duration::from_secs(10), || async {
                    let d = api.describe(&env.cluster_arn, std::slice::from_ref(&task1)).await.ok()?;
                    let h = d.first()?.health.clone();
                    (h.as_deref() == Some("HEALTHY")).then_some(())
                })
                .await;
                eprintln!("RESULT ECS health HEALTHY after {:?} from RunTask's start ({health:?})", t0.elapsed());
                assert!(health.is_some(), "the task never became HEALTHY");

                // Resource-level checks, as the account admin: the revision and
                // the task carry exactly the three tags.
                let ecs_admin = aws_sdk_ecs::Client::new(&t.e.cfg);
                let td = ok(
                    "describe the revision",
                    ecs_admin.describe_task_definition().task_definition(&revision1).include(aws_sdk_ecs::types::TaskDefinitionField::Tags).send().await,
                );
                let mut tags: Vec<(String, String)> = td
                    .tags()
                    .iter()
                    .map(|t| (t.key().unwrap_or_default().to_string(), t.value().unwrap_or_default().to_string()))
                    .collect();
                tags.sort();
                let mut want = workspace_tags(&owner, &id, &name);
                want.sort();
                assert_eq!(tags, want, "RegisterTaskDefinition tagged at registration");
                eprintln!("RESULT RegisterTaskDefinition as the workspace user on task-definition/triple-c-*: allowed, tagged {tags:?}");
                eprintln!("RESULT RunTask (owner-tagged revision, iam:PassRole by tag on the user's own roles): allowed");

                // ── through the agent ──
                let (out, code) = sh(&target, "claude", "echo hello-from-fargate").await;
                assert_eq!((out.trim(), code), ("hello-from-fargate", 0));
                let (id_out, _) = sh(&target, "claude", "id").await;
                eprintln!("RESULT id: {}", id_out.trim());
                assert!(id_out.contains("uid=1000(claude)"), "{id_out}");

                let cloned = poll(Duration::from_secs(300), Duration::from_secs(5), || async {
                    let (out, code) = sh(&target, "root", "test -f /run/triple-c-clone/done && cat /run/triple-c-clone/repos").await;
                    (code == 0).then_some(out)
                })
                .await
                .expect("the background clone never finished");
                eprintln!("RESULT clone status: {}", cloned.trim());
                let (head, code) = sh(&target, "claude", "git -C /workspace/Hello-World log -1 --format=%H").await;
                assert_eq!(code, 0, "{head}");
                eprintln!("RESULT cloned Hello-World at {}", head.trim());

                let (links, _) = sh(
                    &target,
                    "claude",
                    "for f in ~/.claude ~/.claude.json ~/.ssh; do printf '%s -> %s\n' \"$f\" \"$(readlink \"$f\")\"; done; \
                     grep ' /mnt/triple-c-home ' /proc/mounts; stat -c '%U:%G %a' /mnt/triple-c-home",
                )
                .await;
                eprintln!("RESULT Hybrid home:\n{}", links.trim());
                assert!(links.contains(".claude -> /mnt/triple-c-home/"), "{links}");
                let marker = uuid::Uuid::new_v4().to_string();
                let (_, code) = sh(&target, "claude", &format!("printf %s {marker} > ~/.claude/marker")).await;
                assert_eq!(code, 0);

                // Claude Code's own writes to ~/.claude.json (no login: it
                // still starts, and fails at the API call).
                let (claude, _) = sh(
                    &target,
                    "claude",
                    "cd /workspace && timeout 90 claude -p 'say ok' </dev/null 2>&1 | tail -3; \
                     test -L ~/.claude.json && echo STILL-A-LINK || echo REPLACED; ls -la ~/.claude.json",
                )
                .await;
                eprintln!("RESULT claude run:\n{}", claude.trim());

                // P9: bubblewrap's unprivileged user namespace on Fargate.
                let (bwrap, code) = sh(
                    &target,
                    "claude",
                    "bwrap --ro-bind / / --unshare-user true 2>&1; echo \"unshare-user rc=$?\"; \
                     bwrap --ro-bind / / --dev /dev --proc /proc --unshare-all --die-with-parent true 2>&1; echo \"unshare-all rc=$?\"; \
                     cat /proc/sys/user/max_user_namespaces 2>&1; cat /proc/sys/kernel/unprivileged_userns_clone 2>&1; uname -r",
                )
                .await;
                eprintln!("RESULT P9 bwrap (exit {code}):\n{}", bwrap.trim());

                // The stop guard's dry run through the agent, on a dirty repo.
                let (_, code) = sh(&target, "claude", "echo e2e >> /workspace/Hello-World/README && touch /workspace/Hello-World/e2e-new").await;
                assert_eq!(code, 0);
                let (raw, code) = sh(&target, "claude", "/usr/local/bin/triple-c-wip-push --dry-run").await;
                eprintln!("RESULT triple-c-wip-push --dry-run (exit {code}): {}", raw.trim());
                let unsaved = crate::commands::stop_guard::unsaved_work(Some(&target), crate::commands::stop_guard::DRY_RUN_TIMEOUT).await;
                eprintln!("RESULT stop guard check: {unsaved:?}");
                assert!(unsaved.checked, "{unsaved:?}");
                assert!(unsaved.repos.iter().any(|r| r.folder == "Hello-World" && r.dirty), "{unsaved:?}");

                // GetSecretValue: another owner's triple-c/* secret is denied.
                let foreign_secret = format!("triple-c/{}/someone-else", t.e.prefix);
                let created = ok(
                    "admin creates another owner's secret",
                    t.e.secrets
                        .create_secret()
                        .name(&foreign_secret)
                        .secret_string("e2e")
                        .tags(super::sm_tag("triple-c:owner", "AROAEXAMPLEOTHER:bob"))
                        .send()
                        .await,
                );
                t.e.record_secret(created.arn().unwrap());
                let ws_secrets = aws_sdk_secretsmanager::Client::new(&ws_cfg);
                super::denied(
                    "GetSecretValue on another owner's secret",
                    ws_secrets.get_secret_value().secret_id(created.arn().unwrap()).send().await,
                );
                eprintln!("RESULT GetSecretValue on another owner's secret: denied");

                // A bare TagResource on the user's own file system (no
                // CreateAction) is refused: tagging is only at creation.
                let efs_admin = aws_sdk_efs::Client::new(&t.e.cfg);
                let fs = ok(
                    "find the user's file system",
                    efs_admin.describe_file_systems().creation_token(&fs_token).send().await,
                );
                let fs_id = fs.file_systems().first().expect("the user's file system").file_system_id().to_string();
                let ws_efs = aws_sdk_efs::Client::new(&ws_cfg);
                super::denied(
                    "bare TagResource on own file system",
                    ws_efs
                        .tag_resource()
                        .resource_id(&fs_id)
                        .tags(aws_sdk_efs::types::Tag::builder().key("triple-c:owner").value(&owner).build().unwrap())
                        .send()
                        .await,
                );
                eprintln!("RESULT bare elasticfilesystem:TagResource on the user's own file system {fs_id}: denied");

                // RunTask on a revision the user does not own is denied.
                let foreign_family = format!("triple-c-{name}-e2e-foreign");
                let reg = ok(
                    "admin registers a foreign revision",
                    ecs_admin
                        .register_task_definition()
                        .family(&foreign_family)
                        .requires_compatibilities(aws_sdk_ecs::types::Compatibility::Fargate)
                        .network_mode(aws_sdk_ecs::types::NetworkMode::Awsvpc)
                        .cpu("256")
                        .memory("512")
                        .container_definitions(
                            aws_sdk_ecs::types::ContainerDefinition::builder()
                                .name("x")
                                .image("public.ecr.aws/docker/library/alpine:3")
                                .essential(true)
                                .build(),
                        )
                        .tags(aws_sdk_ecs::types::Tag::builder().key("triple-c:owner").value("AROAEXAMPLEOTHER:bob").build())
                        .send()
                        .await,
                );
                let foreign_arn = reg.task_definition().unwrap().task_definition_arn().unwrap().to_string();
                let r = api.run_task(run_task_request(&env, &foreign_arn, false, &workspace_tags(&owner, &id, &name))).await;
                match r {
                    Err(crate::aws::error::AwsError::AccessDenied { .. }) => {
                        eprintln!("RESULT RunTask on another owner's revision: denied")
                    }
                    other => panic!("RunTask on another owner's revision: expected AccessDenied, got {other:?}"),
                }

                // A second start while running: an attach, no second RunTask.
                // With the tunnel up it is reused outright; with it closed the
                // start finds the live task and reads the token back (own
                // GetSecretValue).
                let again = host.ensure_started(&current(&store), &settings, &store, &progress).await.expect("start while running (tunnel up)");
                assert_eq!(again.id(), target.id());
                tunnels::close(&id).await;
                let t1 = std::time::Instant::now();
                let attached = host.ensure_started(&current(&store), &settings, &store, &progress).await.expect("attach");
                eprintln!("RESULT attach to the running task (find, GetSecretValue own secret, tunnel): {:?}", t1.elapsed());
                assert_eq!(current(&store).aws.unwrap().task_arn.as_deref(), Some(task1.as_str()));
                let live = api.list_project_tasks(&env.cluster_arn, &family).await.unwrap();
                assert_eq!(live, vec![task1.clone()], "exactly one task: no second RunTask");
                let (out, _) = sh(&attached, "claude", "cat ~/.claude/marker").await;
                assert_eq!(out.trim(), marker);

                // ── stop 1: StopTask → SIGTERM → the entrypoint's WIP push ──
                let t2 = std::time::Instant::now();
                host.stop_project(&current(&store), &settings).await.expect("stop 1");
                let stopped = poll(Duration::from_secs(240), Duration::from_secs(5), || async {
                    let d = api.describe(&env.cluster_arn, std::slice::from_ref(&task1)).await.ok()?;
                    let t = d.into_iter().next()?;
                    (t.last_status == "STOPPED").then_some(t)
                })
                .await
                .expect("task 1 never stopped");
                eprintln!("RESULT stop 1: STOPPED after {:?}; code {:?}, reason {:?}", t2.elapsed(), stopped.stop_code, stopped.stopped_reason);
                tokio::time::sleep(Duration::from_secs(15)).await;
                let log1 = api.log_tail(&env.log_group, &task_log_stream(&stream_prefix, &task1), 60).await.unwrap();
                let tail1: Vec<&String> = log1.iter().filter(|l| l.contains("entrypoint: stopping") || l.contains("wip") || l.contains("WIP")).collect();
                eprintln!("RESULT task 1 stop log lines: {tail1:#?}");
                assert!(log1.iter().any(|l| l.contains("entrypoint: stopping (sigterm) — pushing work in progress")), "{log1:#?}");
                store.set_aws_runtime(&id, None, current(&store).aws.unwrap().access_point_id, Some(revision1.clone()), None).unwrap();

                // ── start 2, unchanged; idle stop is at its minimum ──
                let t3 = std::time::Instant::now();
                let target2 = host.ensure_started(&current(&store), &settings, &store, &progress).await.expect("start 2");
                let aws2 = current(&store).aws.unwrap();
                let task2 = aws2.task_arn.clone().unwrap();
                eprintln!("RESULT start 2 (same revision): {:?}; task {task2}", t3.elapsed());
                assert_ne!(task2, task1, "a new task");
                assert_eq!(aws2.task_definition_arn.as_deref(), Some(revision1.as_str()), "same family revision");
                let (out, _) = sh(&target2, "claude", "cat ~/.claude/marker; test -L ~/.claude.json && echo STILL-A-LINK").await;
                eprintln!("RESULT after restart: {}", out.trim());
                assert!(out.contains(&marker), "the ~/.claude marker persisted: {out}");
                poll(Duration::from_secs(300), Duration::from_secs(5), || async {
                    let (_, code) = sh(&target2, "root", "test -f /run/triple-c-clone/done").await;
                    (code == 0).then_some(())
                })
                .await
                .expect("clone 2 never finished");
                let (check, _) = sh(&target2, "root", "TRIPLEC_IDLE_STOP_MINUTES=5 /usr/local/bin/triple-c-idle-watch --check; echo idle-check=$?").await;
                eprintln!("RESULT idle --check right after use: {}", check.trim());

                // Leave it alone, the way a user does (R11): a terminal tab stays
                // open but silent (an attach held through the tunnel), and the
                // auth bridge's background poll keeps running every 2 s. Neither
                // may keep the workspace up.
                let held = target2
                    .attach(crate::host::ExecSpec::new(vec!["cat".into()]).tty(true))
                    .await
                    .expect("hold a terminal open");
                let poller = {
                    let t = target2.clone();
                    tokio::spawn(async move {
                        loop {
                            let _ = t
                                .exec_oneshot_background(
                                    "claude",
                                    vec!["/usr/bin/cat".into(), "/proc/net/tcp".into()],
                                    crate::host::PROC_NET_OUTPUT_LIMIT,
                                )
                                .await;
                            tokio::time::sleep(Duration::from_secs(2)).await;
                        }
                    })
                };
                let idle_from = std::time::Instant::now();
                let stopped2 = poll(Duration::from_secs(15 * 60), Duration::from_secs(20), || async {
                    let d = api.describe(&env.cluster_arn, std::slice::from_ref(&task2)).await.ok()?;
                    let t = d.into_iter().next()?;
                    (t.desired_status == "STOPPED" || t.last_status == "STOPPED").then_some(t)
                })
                .await
                .expect("the idle task did not stop by itself within 15 minutes (open tab + background poll)");
                poller.abort();
                drop(held);
                tunnels::close(&id).await;
                eprintln!(
                    "RESULT idle stop: desired STOPPED {:?} after the last use; code {:?}, reason {:?}",
                    idle_from.elapsed(),
                    stopped2.stop_code,
                    stopped2.stopped_reason
                );
                let final2 = poll(Duration::from_secs(240), Duration::from_secs(10), || async {
                    let d = api.describe(&env.cluster_arn, std::slice::from_ref(&task2)).await.ok()?;
                    let t = d.into_iter().next()?;
                    (t.last_status == "STOPPED").then_some(t)
                })
                .await
                .expect("task 2 never reached STOPPED");
                eprintln!(
                    "RESULT idle stop as ECS reports it: code {:?}, exit {:?}, kind {:?}",
                    final2.stop_code,
                    final2.exit_code,
                    crate::aws::tasks::classify_stop(&final2)
                );
                assert_eq!(crate::aws::tasks::classify_stop(&final2), crate::aws::tasks::StopKind::Idle);
                tokio::time::sleep(Duration::from_secs(15)).await;
                let log2 = api.log_tail(&env.log_group, &task_log_stream(&stream_prefix, &task2), 60).await.unwrap();
                let tail2: Vec<&String> = log2.iter().filter(|l| l.contains("stopping") || l.contains("idle")).collect();
                eprintln!("RESULT task 2 stop log lines: {tail2:#?}");
                assert!(log2.iter().any(|l| l.contains("entrypoint: stopping (idle)")), "{log2:#?}");

                // ── remove: the task, the secret and (data discarded) the access
                // point; the task definition is left (R8) ──
                store.set_aws_runtime(&id, None, aws2.access_point_id.clone(), aws2.task_definition_arn.clone(), aws2.workspace_access_point_id.clone()).unwrap();
                // Every step runs even when one fails (the failures are
                // reported together), so check them all before the result.
                let removed = host.remove_aws_resources(&current(&store), &settings, true).await;
                eprintln!("RESULT remove_aws_resources: {:?}", removed.as_ref().map_err(|e| e.chars().take(400).collect::<String>()));
                // R10: deleted at once, so the next start can create it again.
                // The forced delete completes asynchronously; give it a moment.
                let secret_gone = poll(Duration::from_secs(60), Duration::from_secs(5), || async {
                    match t.e.secrets.describe_secret().secret_id(project_secret_name(&name, &okey, &id)).send().await {
                        Err(e) if e.as_service_error().is_some_and(|s| s.is_resource_not_found_exception()) => Some(()),
                        _ => None,
                    }
                })
                .await;
                eprintln!("RESULT project secret after remove: gone {secret_gone:?}");
                assert!(secret_gone.is_some(), "the secret is deleted without a recovery window");
                let defs = ok(
                    "list the family",
                    ecs_admin.list_task_definitions().family_prefix(&family).send().await,
                );
                eprintln!("RESULT active revisions after remove: {:?}", defs.task_definition_arns());
                let td = ecs_admin.describe_task_definition().task_definition(&revision1).send().await;
                eprintln!(
                    "RESULT revision after remove: {:?}",
                    td.map(|o| o.task_definition().and_then(|d| d.status().cloned())).map_err(|e| format!("{e:?}").chars().take(200).collect::<String>())
                );
                let efs = aws_sdk_efs::Client::new(&t.e.cfg);
                let gone = efs.describe_access_points().access_point_id(aws2.access_point_id.clone().unwrap()).send().await;
                eprintln!("RESULT access point after remove: {:?}", gone.as_ref().map(|o| o.access_points().len()).map_err(|e| format!("{e:?}").chars().take(120).collect::<String>()));
                assert!(gone.is_err() || gone.unwrap().access_points().iter().all(|a| a.life_cycle_state() != Some(&aws_sdk_efs::types::LifeCycleState::Available)));
                removed.expect("remove the project's resources");
                // R8: the revisions stay registered for an admin to clear
                // (the guard does, as the account admin).
                assert_eq!(defs.task_definition_arns(), [revision1.clone()], "the project's revision is left ACTIVE");
            });
            let body = async {
                if tokio::time::timeout(PHASE5_TIMEOUT, inner).await.is_err() {
                    panic!("the phase-5 body did not finish in {PHASE5_TIMEOUT:?}");
                }
            };
            let result = std::panic::AssertUnwindSafe(body).catch_unwind().await;
            let guard = std::mem::take(&mut *p5_lock(&p5));
            phase5_cleanup(&t.e, &guard).await;
            if let Err(panic) = result {
                std::panic::resume_unwind(panic);
            }
        })
        .await;
    }

    // ─────────────────────────────────────────────── phase 6: one workspace, every option

    /// The keys of the project secret, as the workspace user (values are
    /// never printed), and the two gateway key values for the log check.
    async fn secret_keys(
        ws_cfg: &SdkConfig,
        secret: &str,
    ) -> (Vec<String>, serde_json::Map<String, serde_json::Value>) {
        let out = ok(
            "GetSecretValue on the project's own secret, as the workspace user",
            aws_sdk_secretsmanager::Client::new(ws_cfg).get_secret_value().secret_id(secret).send().await,
        );
        let map: serde_json::Map<String, serde_json::Value> =
            serde_json::from_str(out.secret_string().expect("a string secret")).expect("a JSON payload");
        let mut keys: Vec<String> = map.keys().cloned().collect();
        keys.sort();
        (keys, map)
    }

    /// A free loopback port on this machine.
    fn free_port() -> u16 {
        std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
    }

    /// `GET /` on 127.0.0.1:`port`: the status line.
    async fn http_get_status(port: u16) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let mut s = tokio::net::TcpStream::connect(("127.0.0.1", port)).await.expect("connect to the mapped port");
        s.write_all(b"GET / HTTP/1.0\r\nHost: localhost\r\n\r\n").await.unwrap();
        let mut buf = Vec::new();
        let _ = tokio::time::timeout(Duration::from_secs(30), s.read_to_end(&mut buf)).await;
        String::from_utf8_lossy(&buf).lines().next().unwrap_or_default().to_string()
    }

    /// Phase 6 in one project, two task starts: an environment (module v2)
    /// with `max_workspace_hours = 1`, and a project with EFS storage, a
    /// setup script, copy-at-connect credentials (a CA file, a git token and
    /// a custom variable), a port mapping and the LLM gateway sidecar (a
    /// dummy provider key). Run like the phase-5 test:
    ///
    /// ```sh
    /// TRIPLEC_E2E_TEST=aws::live_tests::environment::a_phase6_workspace_round_trips \
    /// TRIPLEC_AWS_E2E_PROFILE=… TRIPLEC_AWS_E2E_REGION=us-west-2 scripts/aws-e2e-workspace.sh
    /// ```
    ///
    /// Not exercised: the lifetime cap firing (unit-tested; 60 minutes),
    /// a v1 → v2 environment update (the fixture applies only the embedded
    /// modules), a private clone retried after connect (no credentials).
    /// About 25 Fargate task-minutes at 1 vCPU / 2 GB.
    #[tokio::test]
    #[ignore]
    async fn a_phase6_workspace_round_trips() {
        use crate::aws::runtime::SdkRuntimeApi;
        use crate::aws::tasks::{task_log_stream, SdkTaskApi, TaskApi};
        use crate::aws::tofu_modules::{log_stream_prefix, project_secret_name, task_family};
        use crate::host::aws_host::{self, connect};
        use crate::host::{lifecycle::ProjectHost, port_forwards, ExecSpec};
        use crate::models::app_settings::ImageSource;
        use crate::models::gateway_settings::{GatewayModel, GatewaySettings};
        use crate::models::project::{
            AwsPlacement, AwsWorkspaceConfig, Backend, EnvVar, OpenAiCompatibleConfig, PortMapping,
            RepoSource, SecretsDelivery, StorageMode,
        };
        use crate::models::{AppSettings, Project};
        use crate::storage::projects_store::ProjectsStore;

        let image = super::var("TRIPLEC_E2E_IMAGE");
        let ecr_repo = std::env::var("TRIPLEC_E2E_ECR_REPO").ok().filter(|r| !r.is_empty());
        env_e2e(|t| async move {
            let p5 = std::sync::Mutex::new(Phase5 { ecr_repo, ..Default::default() });
            let inner = Box::pin(async {
                // ── 1. the environment carries the lifetime cap ──
                let Deployed { region, name, okey, conn, ws_cfg, ws, .. } =
                    deploy_for_workspace_user(&t, &p5, &image, 1, None).await;
                let env = crate::aws::runtime::read_environment(&SdkRuntimeApi::new(&ws_cfg), &name, &ws.account, &region)
                    .await
                    .expect("the environment record, as the workspace user");
                eprintln!(
                    "RESULT environment record: template_version {}, max_workspace_hours {}",
                    env.template_version, env.max_workspace_hours
                );
                assert_eq!(env.template_version, crate::aws::tofu_modules::ENVIRONMENT_MODULE_VERSION);
                assert_eq!(env.max_workspace_hours, 1);

                // Dummy values: none is a real credential; each must stay out
                // of the logs and (credentials) out of the secret.
                let tag = uuid::Uuid::new_v4().simple().to_string();
                let provider_key = format!("sk-e2e-provider-{tag}");
                let git_token = format!("e2e-git-token-{tag}");
                let custom_value = format!("e2e-custom-{tag}");
                let dir = tempfile::tempdir().unwrap();
                let ca = dir.path().join("e2e-ca.pem");
                let made = std::process::Command::new("openssl")
                    .args(["req", "-x509", "-newkey", "rsa:2048", "-nodes", "-days", "1", "-subj", "/CN=triple-c-e2e-ca"])
                    .arg("-keyout")
                    .arg(dir.path().join("e2e-ca.key"))
                    .arg("-out")
                    .arg(&ca)
                    .output()
                    .expect("openssl");
                assert!(made.status.success(), "openssl: {}", String::from_utf8_lossy(&made.stderr));

                let mut settings = AppSettings::default();
                settings.hosts = vec![conn];
                settings.image_source = ImageSource::Custom;
                settings.custom_image_name = Some(image.clone());
                settings.gateway = GatewaySettings {
                    provider: "openai".into(),
                    models: vec![GatewayModel { name: "e2e-model".into(), model_id: "gpt-4o-mini".into() }],
                    ..Default::default()
                };

                let http_port: u16 = 8099;
                let host_port = free_port();
                let mut project = Project::new("e2e-phase6".into(), Vec::new());
                project.host_id = Some("e2e-wsuser".into());
                project.aws_placement = Some(AwsPlacement { region: region.clone(), environment: name.clone() });
                project.backend = Backend::OpenAiCompatible;
                project.openai_compatible_config = Some(OpenAiCompatibleConfig {
                    base_url: "https://example.invalid".into(),
                    api_key: Some(format!("e2e-oc-key-{tag}")),
                    model_id: Some("e2e-model".into()),
                    haiku_model_id: None,
                });
                project.ca_cert_path = Some(ca.to_string_lossy().into_owned());
                project.git_token = Some(git_token.clone());
                project.custom_env_vars = vec![EnvVar { key: "E2E_CUSTOM_TOKEN".into(), value: custom_value.clone() }];
                project.port_mappings = vec![PortMapping { host_port, container_port: http_port, protocol: "tcp".into() }];
                project.aws = Some(AwsWorkspaceConfig {
                    cpu: 1024,
                    memory_mib: 2048,
                    storage: StorageMode::Efs,
                    repos: vec![RepoSource {
                        url: "https://github.com/octocat/Hello-World.git".into(),
                        branch: None,
                        folder: "Hello-World".into(),
                    }],
                    wip_push_on_stop: false,
                    idle_stop_minutes: None,
                    secrets_delivery: SecretsDelivery::CopyAtConnect,
                    setup_script: Some(
                        "sudo apt-get install -y jq\njq --version\necho ok > /workspace/setup-marker\n".into(),
                    ),
                    gateway_sidecar: true,
                    ..Default::default()
                });
                assert_eq!(crate::host::port_forwards::validate_aws_port_mappings(&project.port_mappings, true), Ok(()));
                let id = project.id.clone();
                let secret_name = project_secret_name(&name, &okey, &id);
                {
                    let mut g = p5_lock(&p5);
                    g.project_id = Some(id.clone());
                    g.secrets.push(secret_name.clone());
                }
                let store_dir = tempfile::tempdir().unwrap();
                let store = ProjectsStore::over_file_for_test(store_dir.path().join("projects.json"), vec![project.clone()]);
                assert_eq!(ProjectHost::for_project(&project), ProjectHost::Aws);
                let host = ProjectHost::Aws;
                let progress = |m: &str| eprintln!("  progress: {m}");
                let api = SdkTaskApi::new(&ws_cfg);
                let family = task_family(&name, &okey, &id);
                let stream_prefix = log_stream_prefix(&okey, &id);
                let current = |store: &ProjectsStore| store.get(&id).unwrap();
                let key_reader = {
                    let k = provider_key.clone();
                    move || Ok(Some(k))
                };
                let connect_now = |target: crate::host::ContainerTarget| {
                    let (p, s) = (current(&store), settings.clone());
                    async move {
                        let bundle = connect::connect_credentials(&p, &s).expect("the connect bundle");
                        eprintln!("  connect bundle: {bundle:?}");
                        let t0 = std::time::Instant::now();
                        connect::push_connect_bundle(&target, &bundle).await.expect("push the connect bundle");
                        eprintln!("RESULT copy-at-connect push: {:?}", t0.elapsed());
                    }
                };

                // ── start 1 ──
                let t0 = std::time::Instant::now();
                let target = aws_host::ensure_started_with(&current(&store), &settings, &store, &progress, key_reader.clone())
                    .await
                    .expect("start 1");
                let aws1 = current(&store).aws.unwrap();
                let task1 = aws1.task_arn.clone().unwrap();
                eprintln!("RESULT start 1 (cold, EFS + sidecar): {:?}; task {task1}", t0.elapsed());
                assert!(aws1.access_point_id.is_some() && aws1.workspace_access_point_id.is_some(), "EFS: two access points {aws1:?}");
                assert!(aws1.task_definition_arn.as_deref().unwrap().contains(&format!(":task-definition/{family}:")));
                connect_now(target.clone()).await;

                // ── 6. the secret holds no credentials; 1. the cap reached the task ──
                let (keys, map) = secret_keys(&ws_cfg, &secret_name).await;
                eprintln!("RESULT project secret keys (copy-at-connect + sidecar): {keys:?}");
                eprintln!("RESULT TRIPLEC_MAX_LIFETIME_MINUTES in the secret: {:?}", map.get("TRIPLEC_MAX_LIFETIME_MINUTES"));
                assert_eq!(map.get("TRIPLEC_MAX_LIFETIME_MINUTES").and_then(|v| v.as_str()), Some("60"));
                for k in ["TRIPLEC_FILES", "GIT_TOKEN", "E2E_CUSTOM_TOKEN", "TRIPLE_C_CUSTOM_ENV"] {
                    assert!(!keys.iter().any(|x| x == k), "{k} is in the copy-at-connect secret");
                }
                let secret_text = serde_json::to_string(&map).unwrap();
                for (what, v) in [("git token", &git_token), ("custom value", &custom_value)] {
                    assert!(!secret_text.contains(v.as_str()), "the {what} is in the secret");
                }
                assert_eq!(map.get("TRIPLEC_GATEWAY_API_KEY").and_then(|v| v.as_str()), Some(provider_key.as_str()), "R1: the sidecar's key stays");
                let master1 = map.get("TRIPLEC_GATEWAY_MASTER_KEY").and_then(|v| v.as_str()).expect("a master key").to_string();
                let (env_line, code) = sh(
                    &target,
                    "root",
                    "tr '\\0' '\\n' < /proc/1/environ | grep '^TRIPLEC_MAX_LIFETIME_MINUTES='; \
                     ps -eo args | grep -c '[t]riple-c-idle-watch' || true",
                )
                .await;
                eprintln!("RESULT task environment (PID 1), lifetime + idle watcher count:\n{}", env_line.trim());
                assert_eq!(code, 0);
                assert!(env_line.contains("TRIPLEC_MAX_LIFETIME_MINUTES=60"), "{env_line}");

                // ── 3. clone + setup, as the app's status follower sees it ──
                let settled = poll(Duration::from_secs(600), Duration::from_secs(5), || async {
                    let s = aws_host::workspace_status(&current(&store)).await.ok().flatten()?;
                    s.settled().then_some(s)
                })
                .await
                .expect("clone + setup never settled");
                eprintln!("RESULT workspace status after start 1: {settled:?}");
                assert_eq!(settled.setup.state, aws_host::status::SetupState::Ok, "{settled:?}");
                assert!(settled.failed_repos().is_empty(), "{settled:?}");

                // ── 2. EFS /workspace, Hybrid links; 6. CA installed, ~/.ssh local ──
                let marker = uuid::Uuid::new_v4().to_string();
                let (fs, code) = sh(
                    &target,
                    "claude",
                    &format!(
                        "grep ' /workspace ' /proc/mounts; cat /workspace/setup-marker; \
                         git -C /workspace/Hello-World log -1 --format=%H; \
                         printf %s {marker} > /workspace/e2e-efs-marker && echo wrote; \
                         for f in ~/.claude ~/.claude.json ~/.ssh; do printf '%s -> %s\\n' \"$f\" \"$(readlink \"$f\")\"; done; \
                         test -L ~/.ssh && echo SSH-IS-LINK || echo ssh-local; \
                         ls /usr/local/share/ca-certificates/; \
                         for c in /usr/local/share/ca-certificates/triple-c-*.crt; do openssl x509 -in \"$c\" -noout -subject; done; \
                         git config --global --get credential.helper >/dev/null && echo git-helper-set"
                    ),
                )
                .await;
                eprintln!("RESULT start 1 workspace (exit {code}):\n{}", fs.trim());
                assert!(fs.contains(" /workspace nfs"), "{fs}");
                assert!(fs.lines().any(|l| l.trim() == "ok"), "setup marker: {fs}");
                assert!(fs.contains("wrote"), "{fs}");
                assert!(fs.contains(".claude -> /mnt/triple-c-home/"), "{fs}");
                assert!(fs.contains("ssh-local"), "R6: ~/.ssh is not on EFS under copy-at-connect: {fs}");
                assert!(fs.contains("CN = triple-c-e2e-ca") || fs.contains("CN=triple-c-e2e-ca"), "CA installed: {fs}");

                // ── 4. a port mapping ──
                let exec = target
                    .exec_detached(
                        ExecSpec::new(vec!["python3".into(), "-m".into(), "http.server".into(), http_port.to_string(), "--bind".into(), "127.0.0.1".into()])
                            .working_dir(Some("/tmp"))
                            .no_stdin(),
                    )
                    .await
                    .expect("start the HTTP server");
                eprintln!("  http.server exec {exec}");
                let up = poll(Duration::from_secs(60), Duration::from_secs(2), || async {
                    let (_, c) = sh(&target, "claude", &format!("curl -sf -o /dev/null http://127.0.0.1:{http_port}/")).await;
                    (c == 0).then_some(())
                })
                .await;
                assert!(up.is_some(), "the HTTP server never answered inside the workspace");
                let opened = port_forwards::open_for(&id, &target, &current(&store).port_mappings).await;
                assert!(opened.iter().all(|r| r.error.is_none()), "{opened:?}");
                let status_line = http_get_status(host_port).await;
                eprintln!("RESULT port mapping 127.0.0.1:{host_port} -> workspace :{http_port}: {status_line}");
                assert!(status_line.contains(" 200 "), "{status_line}");
                port_forwards::close(&id).await;

                // ── 5. the gateway sidecar ──
                let mk_env = vec![format!("MK={master1}")];
                let health = poll(Duration::from_secs(420), Duration::from_secs(10), || async {
                    let (out, _) = sh(&target, "claude", "curl -s -o /dev/null -w '%{http_code}' --max-time 5 http://127.0.0.1:4000/health/liveliness").await;
                    (out.trim() == "200").then_some(out)
                })
                .await;
                eprintln!("RESULT gateway /health/liveliness from the workspace: {health:?} after {:?} from RunTask", t0.elapsed());
                assert!(health.is_some(), "the gateway sidecar never answered");
                let exec_with = |script: &'static str| {
                    let t = target.clone();
                    let env = mk_env.clone();
                    async move {
                        tokio::time::timeout(SH_TIMEOUT, t.exec_oneshot_as("claude", vec!["sh".into(), "-c".into(), script.into()], env))
                            .await
                            .expect("exec timed out")
                            .expect("exec")
                    }
                };
                let (models, _) = exec_with(
                    "curl -s -w ' HTTP%{http_code}' -H \"Authorization: Bearer $MK\" http://127.0.0.1:4000/v1/models; echo; \
                     curl -s -o /dev/null -w 'no key: HTTP%{http_code}' http://127.0.0.1:4000/v1/models",
                )
                .await;
                eprintln!("RESULT gateway /v1/models:\n{}", models.trim());
                assert!(models.contains("e2e-model") && models.contains("HTTP200"), "{models}");
                let (chat, _) = exec_with(
                    "curl -s --max-time 60 -w ' HTTP%{http_code}' -H \"Authorization: Bearer $MK\" -H 'content-type: application/json' \
                     -d '{\"model\":\"e2e-model\",\"messages\":[{\"role\":\"user\",\"content\":\"hi\"}],\"max_tokens\":5}' \
                     http://127.0.0.1:4000/v1/chat/completions",
                )
                .await;
                let chat_short: String = chat.chars().take(400).collect();
                eprintln!("RESULT failed provider call (dummy key):\n{chat_short}");
                assert!(!chat.contains(&provider_key), "the provider key came back in the response");
                assert!(!chat.contains(" HTTP200"), "a dummy key got an answer?");
                let (claude_env, _) = sh(&target, "claude", "bash -lc 'printf \"%s\\n\" \"$ANTHROPIC_BASE_URL\"; test -n \"$ANTHROPIC_AUTH_TOKEN\" && echo token-set'").await;
                eprintln!("RESULT claude's environment: {}", claude_env.trim());

                // ── 7. a cost estimate for this configuration ──
                let pricing_cfg = crate::aws::sdk_config(&settings.hosts[0], crate::aws::pricing::PRICING_REGION).await.expect("pricing config");
                let rates = crate::aws::pricing::fetch_rates(&crate::aws::pricing::SdkPricingApi::new(&pricing_cfg), &region)
                    .await
                    .expect("fetch Fargate rates as the workspace user");
                let est = crate::aws::pricing::estimate(
                    current(&store).aws.as_ref().unwrap(),
                    &rates.for_arch(crate::models::project::Arch::X86_64).expect("x86 rates"),
                );
                eprintln!("RESULT cost estimate for 1 vCPU / 2 GiB / EFS: {est:?}");
                assert!(est.usd_per_hour > 0.0 && est.efs_billed_separately);

                // ── stop 1 ──
                host.stop_project(&current(&store), &settings).await.expect("stop 1");
                let stopped = poll(Duration::from_secs(240), Duration::from_secs(5), || async {
                    let d = api.describe(&env.cluster_arn, std::slice::from_ref(&task1)).await.ok()?;
                    let t = d.into_iter().next()?;
                    (t.last_status == "STOPPED").then_some(t)
                })
                .await
                .expect("task 1 never stopped");
                eprintln!("RESULT stop 1: code {:?}, exit {:?}, kind {:?}", stopped.stop_code, stopped.exit_code, crate::aws::tasks::classify_stop(&stopped));
                tokio::time::sleep(Duration::from_secs(20)).await;

                // ── 5. neither key in the logs ──
                let task_id = task1.rsplit('/').next().unwrap();
                let gw_stream = format!("{stream_prefix}/gateway/{task_id}");
                let gw_log = api.log_tail(&env.log_group, &gw_stream, 2000).await.unwrap();
                let ws_log = api.log_tail(&env.log_group, &task_log_stream(&stream_prefix, &task1), 2000).await.unwrap();
                eprintln!("RESULT gateway log: {} lines; workspace log: {} lines", gw_log.len(), ws_log.len());
                let short = |l: &String| l.chars().take(240).collect::<String>();
                for l in gw_log.iter().filter(|l| l.contains("rror") || l.contains("uthoriz") || l.contains("401") || l.to_lowercase().contains("header")).take(12) {
                    eprintln!("  gateway log: {}", short(l));
                }
                let prefix8: String = provider_key.chars().take(16).collect();
                let partial = gw_log.iter().filter(|l| l.contains(&prefix8)).count();
                eprintln!("RESULT gateway log lines holding the provider key's first 16 chars (masked echo): {partial}");
                for (what, v) in [("provider key", provider_key.as_str()), ("master key", master1.as_str()), ("git token", git_token.as_str()), ("custom value", custom_value.as_str())] {
                    let gw = gw_log.iter().filter(|l| l.contains(v)).count();
                    let wsn = ws_log.iter().filter(|l| l.contains(v)).count();
                    eprintln!("RESULT {what} in logs: gateway {gw} line(s), workspace {wsn} line(s)");
                    assert_eq!((gw, wsn), (0, 0), "the {what} is in the task's logs");
                }
                assert!(!gw_log.is_empty(), "the gateway stream is empty: {gw_stream}");
                store.set_aws_runtime(&id, None, aws1.access_point_id.clone(), aws1.task_definition_arn.clone(), aws1.workspace_access_point_id.clone()).unwrap();

                // ── start 2: /workspace survived ──
                let t3 = std::time::Instant::now();
                let target2 = aws_host::ensure_started_with(&current(&store), &settings, &store, &progress, key_reader.clone())
                    .await
                    .expect("start 2");
                let aws2 = current(&store).aws.unwrap();
                let task2 = aws2.task_arn.clone().unwrap();
                eprintln!("RESULT start 2: {:?}; task {task2}", t3.elapsed());
                assert_ne!(task2, task1);
                assert_eq!(aws2.workspace_access_point_id, aws1.workspace_access_point_id, "the same /workspace access point");
                connect_now(target2.clone()).await;
                let (_, map2) = secret_keys(&ws_cfg, &secret_name).await;
                let master2 = map2.get("TRIPLEC_GATEWAY_MASTER_KEY").and_then(|v| v.as_str()).unwrap_or_default();
                eprintln!("RESULT sidecar master key new per start: {}", master2 != master1);
                assert_ne!(master2, master1.as_str());
                let settled2 = poll(Duration::from_secs(600), Duration::from_secs(5), || async {
                    let s = aws_host::workspace_status(&current(&store)).await.ok().flatten()?;
                    s.settled().then_some(s)
                })
                .await
                .expect("start 2 never settled");
                eprintln!("RESULT workspace status after start 2: {settled2:?}");
                let (fs2, _) = sh(&target2, "claude", "cat /workspace/e2e-efs-marker; echo; ls /workspace; git -C /workspace/Hello-World status --short | head -3; ls /usr/local/share/ca-certificates/").await;
                eprintln!("RESULT start 2 /workspace:\n{}", fs2.trim());
                assert!(fs2.contains(&marker), "the EFS file survived the stop: {fs2}");
                assert!(settled2.clone.repos.iter().any(|r| r.folder == "Hello-World" && r.state == "exists"), "{settled2:?}");

                // ── stop 2, then remove with discard ──
                host.stop_project(&current(&store), &settings).await.expect("stop 2");
                poll(Duration::from_secs(240), Duration::from_secs(5), || async {
                    let d = api.describe(&env.cluster_arn, std::slice::from_ref(&task2)).await.ok()?;
                    (d.into_iter().next()?.last_status == "STOPPED").then_some(())
                })
                .await
                .expect("task 2 never stopped");
                store.set_aws_runtime(&id, None, aws2.access_point_id.clone(), aws2.task_definition_arn.clone(), aws2.workspace_access_point_id.clone()).unwrap();
                let removed = host.remove_aws_resources(&current(&store), &settings, true).await;
                eprintln!("RESULT remove_aws_resources (discard): {:?}", removed.as_ref().map_err(|e| e.chars().take(400).collect::<String>()));
                let efs = aws_sdk_efs::Client::new(&t.e.cfg);
                for ap in [aws2.access_point_id.clone().unwrap(), aws2.workspace_access_point_id.clone().unwrap()] {
                    let gone = efs.describe_access_points().access_point_id(&ap).send().await;
                    let live = gone.as_ref().map(|o| o.access_points().iter().any(|a| a.life_cycle_state() == Some(&aws_sdk_efs::types::LifeCycleState::Available))).unwrap_or(false);
                    eprintln!("RESULT access point {ap} after remove: available {live}");
                    assert!(!live, "{ap} is still there");
                }
                removed.expect("remove the project's resources");
            });
            let body = async {
                if tokio::time::timeout(PHASE5_TIMEOUT, inner).await.is_err() {
                    panic!("the phase-6 body did not finish in {PHASE5_TIMEOUT:?}");
                }
            };
            let result = std::panic::AssertUnwindSafe(body).catch_unwind().await;
            let guard = std::mem::take(&mut *p5_lock(&p5));
            phase5_cleanup(&t.e, &guard).await;
            if let Err(panic) = result {
                std::panic::resume_unwind(panic);
            }
        })
        .await;
    }

    // ─────────────────────────────────────────────── phase 7: ECS on EC2

    /// The phase-7 body: two cold starts, a scale-in wait (managed scaling
    /// takes ~15 minutes to remove an idle instance) and three applies.
    const PHASE7_TIMEOUT: Duration = Duration::from_secs(110 * 60);

    /// A group's (instance count, desired capacity, `id:state` list) as the
    /// account admin; `None` when it does not exist.
    async fn group_size(asg: &aws_sdk_autoscaling::Client, name: &str) -> Option<(usize, i32, Vec<String>)> {
        let out = asg.describe_auto_scaling_groups().auto_scaling_group_names(name).send().await.ok()?;
        let g = out.auto_scaling_groups().first()?;
        let ids = g
            .instances()
            .iter()
            .map(|i| format!("{}:{}", i.instance_id().unwrap_or_default(), i.lifecycle_state().map(|s| s.as_str()).unwrap_or("?")))
            .collect();
        Some((g.instances().len(), g.desired_capacity().unwrap_or(-1), ids))
    }

    /// Instances still tagged with `env` and not terminated, as the account admin.
    async fn live_env_instances(region: &str, env: &str) -> Vec<String> {
        let (ok_, out, err) = aws_cli(
            None,
            region,
            &[
                "ec2", "describe-instances",
                "--filters", &format!("Name=tag:triple-c:environment,Values={env}"),
                "Name=instance-state-name,Values=pending,running,stopping,stopped,shutting-down",
                "--query", "Reservations[].Instances[].[InstanceId,State.Name]",
            ],
        )
        .await;
        assert!(ok_, "describe-instances: {err}");
        let rows: Vec<Vec<String>> = serde_json::from_str(&out).unwrap_or_default();
        rows.into_iter().map(|r| r.join(":")).collect()
    }

    /// Phase 7: an EC2 project's whole life on the user's own capacity
    /// (ECS on EC2, bridge networking, scale to zero), as a role holding
    /// only the workspace files and carrying the `triple-c-user` principal
    /// tag (a role tag: an assumed-role session's principal tags), onboarded
    /// with `m7i.large`, at most 1 instance, by the environment admin
    /// holding all four admin files. Run with `scripts/aws-e2e-workspace.sh`
    /// and `TRIPLEC_E2E_TEST=aws::live_tests::environment::a_phase7_ec2_workspace_round_trips`.
    ///
    /// Covers: the onboarding's resources and its pre-apply collision check
    /// (and its refusal for another owner); admin-tier probes of the
    /// scaling file (`$Latest`, a pinned control, an unsupported type in a
    /// template version, a capacity provider over a non-Triple-C group);
    /// a cold start with Docker access, VPN and the sandbox on (instance
    /// launch, ECS Exec port forward into the bridge-mode task), `docker
    /// ps`, a WireGuard/TUN interface, `src_valid_mark`, bubblewrap, IMDS
    /// and the agent's introspection port from the container; RunTask on
    /// another user's cluster (the wrong-triple-c-user message); stop and
    /// the scale-in to zero; a second start; removal; re-applying the
    /// onboarding without EC2 (drain and delete with an instance still up);
    /// offboarding and the environment's destroy, all as the admin tier.
    /// Its guard (`ec2_cleanup`) force-deletes whatever EC2 capacity is
    /// left. About one m7i.large instance-hour.
    #[tokio::test]
    #[ignore]
    async fn a_phase7_ec2_workspace_round_trips() {
        use crate::aws::environments::{OnboardApi, SdkOnboardApi, UserCompute, UserRecord};
        use crate::aws::runtime::{read_user_record, SdkRuntimeApi};
        use crate::aws::tasks::{ec2_run_error, ec2_run_task_request, SdkTaskApi, TaskApi, EC2_TAG_MISMATCH};
        use crate::aws::tofu_modules::{user_ec2_stem, user_instance_role_name, Ec2Compute};
        use crate::aws::workspace::workspace_tags;
        use crate::commands::environment_commands::{prepare_onboarding, Ec2Choice};
        use crate::host::aws_host::PROGRESS_INSTANCE;
        use crate::host::lifecycle::ProjectHost;
        use crate::models::app_settings::ImageSource;
        use crate::models::project::{AwsPlacement, AwsWorkspaceConfig, LaunchType, StorageMode};
        use crate::models::{AppSettings, Project};
        use crate::storage::projects_store::ProjectsStore;

        let image = super::var("TRIPLEC_E2E_IMAGE");
        let ecr_repo = std::env::var("TRIPLEC_E2E_ECR_REPO").ok().filter(|r| !r.is_empty());
        env_e2e(|t| async move {
            let p5 = std::sync::Mutex::new(Phase5 { ecr_repo, ..Default::default() });
            let inner = Box::pin(async {
                // TRIPLEC_E2E_PHASE7_SHORT=1: the life cycle only (no probes, no
                // in-container checks or isolation, no scale-in wait; the second
                // start is then warm, on the instance still up).
                let quick = std::env::var("TRIPLEC_E2E_PHASE7_SHORT").is_ok_and(|v| v == "1");
                let short = uuid::Uuid::new_v4().simple().to_string();
                let user_tag = format!("e2e{}", &short[..6]);
                let compute = Ec2Compute { instance_type: "m7i.large".into(), max_instances: 1, user_tag: user_tag.clone() };
                let Deployed { region, name, owner, okey, conn, ws_cfg, ws, admin, .. } =
                    deploy_for_workspace_user(&t, &p5, &image, 0, Some(&compute)).await;
                let account = t.e.account.clone();
                let stem = user_ec2_stem(&name, &user_tag);
                let asg_admin = aws_sdk_autoscaling::Client::new(&t.e.cfg);
                let ecs_admin = aws_sdk_ecs::Client::new(&t.e.cfg);

                // ── 1. what the onboarding made ──
                let groups = ok("describe the group", asg_admin.describe_auto_scaling_groups().auto_scaling_group_names(&stem).send().await);
                let group = groups.auto_scaling_groups().first().expect("the user's Auto Scaling group").clone();
                let subnets = group.vpc_zone_identifier().unwrap_or_default().to_string();
                let group_tags: Vec<(String, String)> = group
                    .tags()
                    .iter()
                    .map(|t| (t.key().unwrap_or_default().to_string(), t.value().unwrap_or_default().to_string()))
                    .collect();
                eprintln!(
                    "RESULT group {stem}: min {:?} max {:?} desired {:?}, template {:?}, protect {:?}, tags {group_tags:?}",
                    group.min_size(),
                    group.max_size(),
                    group.desired_capacity(),
                    group.launch_template().map(|l| (l.launch_template_name(), l.version())),
                    group.new_instances_protected_from_scale_in()
                );
                assert_eq!((group.min_size(), group.max_size()), (Some(0), Some(1)));
                let cl = ok(
                    "describe the user's cluster",
                    ecs_admin.describe_clusters().clusters(&stem).include(aws_sdk_ecs::types::ClusterField::Tags).send().await,
                );
                let cluster = cl.clusters().first().expect("the user's cluster");
                eprintln!(
                    "RESULT cluster {stem}: {:?}, providers {:?}, default strategy {:?}, tags {:?}",
                    cluster.status(),
                    cluster.capacity_providers(),
                    cluster.default_capacity_provider_strategy().iter().map(|s| s.capacity_provider()).collect::<Vec<_>>(),
                    cluster.tags().iter().map(|t| (t.key().unwrap_or_default(), t.value().unwrap_or_default())).collect::<Vec<_>>()
                );
                let cps = ok("describe the capacity provider", ecs_admin.describe_capacity_providers().capacity_providers(&stem).send().await);
                let cp = cps.capacity_providers().first().expect("the capacity provider");
                eprintln!(
                    "RESULT capacity provider {stem}: {:?}, managed scaling {:?}, termination protection {:?}",
                    cp.status(),
                    cp.auto_scaling_group_provider().and_then(|a| a.managed_scaling()).map(|m| (m.status(), m.target_capacity())),
                    cp.auto_scaling_group_provider().map(|a| a.managed_termination_protection())
                );
                let (_, lt, _) = aws_cli(
                    None,
                    &region,
                    &["ec2", "describe-launch-template-versions", "--launch-template-name", &stem, "--query",
                      "LaunchTemplateVersions[].[VersionNumber,LaunchTemplateData.InstanceType,LaunchTemplateData.ImageId,LaunchTemplateData.MetadataOptions]"],
                )
                .await;
                eprintln!("RESULT launch template {stem}: {}", lt.split_whitespace().collect::<Vec<_>>().join(" "));
                let role_name = user_instance_role_name(&name, &okey);
                let prof = ok("get the instance profile", t.e.iam.get_instance_profile().instance_profile_name(&role_name).send().await);
                eprintln!(
                    "RESULT instance profile {role_name}: roles {:?}",
                    prof.instance_profile().map(|p| p.roles().iter().map(|r| r.role_name().to_string()).collect::<Vec<_>>())
                );
                let record = read_user_record(&SdkRuntimeApi::new(&ws_cfg), &name, &okey, &account, &region).await.expect("read the user record");
                eprintln!("RESULT user record, as the workspace user: {record:?}");
                let Some(rec) = record.valid().cloned() else { panic!("the user record is not valid: {record:?}") };
                assert_eq!((rec.instance_type.as_str(), rec.max_instances, rec.user_tag.as_str()), ("m7i.large", 1, user_tag.as_str()));
                assert!(rec.cluster_arn.ends_with(&format!(":cluster/{stem}")), "{}", rec.cluster_arn);

                // ── 1b / 6. the collision check's reads, and its refusal ──
                let (akeys, acfg, _) = t.session(&admin).await;
                let oapi = SdkOnboardApi::new(&acfg);
                let held = oapi.cluster(&stem).await.expect("DescribeClusters include=TAGS as the admin tier");
                eprintln!("RESULT DescribeClusters include=TAGS as the environment admin: {held:?}");
                let held = held.expect("the cluster is found");
                assert!(held.tags.iter().any(|(k, v)| k == "triple-c:owner" && *v == owner), "{held:?}");
                let missing = oapi.cluster(&format!("{stem}zz")).await;
                eprintln!("RESULT DescribeClusters on a missing name: {missing:?}");
                assert!(matches!(missing, Ok(None)), "{missing:?}");
                let missing_group = oapi.group(&format!("{stem}zz")).await;
                eprintln!("RESULT DescribeAutoScalingGroups on a missing name: {missing_group:?}");
                assert!(matches!(missing_group, Ok(None)), "{missing_group:?}");
                let refused = prepare_onboarding(&oapi, &name, &region, &account, "AROAEXAMPLEOTHER0000:bob", Some(Ec2Choice::Ec2(compute.clone()))).await;
                eprintln!("RESULT onboarding another owner with the same triple-c-user value: {refused:?}");
                let refused = refused.expect_err("another owner's onboarding with the same tag is refused");
                assert!(refused.to_string().contains("another onboarding"), "{refused}");

                if !quick {
                    // ── 6. the scaling file's open points, as the environment admin ──
                    let asg_env = aws_sdk_autoscaling::Client::new(&acfg);
                    let ecs_env = aws_sdk_ecs::Client::new(&acfg);
                    let probe_tags = |n: &str| -> Vec<aws_sdk_autoscaling::types::Tag> {
                        group_tags
                            .iter()
                            .filter(|(k, _)| k.starts_with("triple-c:"))
                            .map(|(k, v)| {
                                aws_sdk_autoscaling::types::Tag::builder()
                                    .key(k)
                                    .value(v)
                                    .resource_id(n)
                                    .resource_type("auto-scaling-group")
                                    .propagate_at_launch(false)
                                    .build()
                            })
                            .collect()
                    };
                    let create_group = |client: aws_sdk_autoscaling::Client, n: String, version: String, tags: Vec<aws_sdk_autoscaling::types::Tag>| {
                        let subnets = subnets.clone();
                        let lt_name = stem.clone();
                        async move {
                            client
                                .create_auto_scaling_group()
                                .auto_scaling_group_name(&n)
                                .launch_template(
                                    aws_sdk_autoscaling::types::LaunchTemplateSpecification::builder()
                                        .launch_template_name(lt_name)
                                        .version(version)
                                        .build(),
                                )
                                .min_size(0)
                                .max_size(0)
                                .desired_capacity(0)
                                .vpc_zone_identifier(subnets)
                                .set_tags(Some(tags))
                                .send()
                                .await
                                .map(|_| ())
                                .map_err(|e| classify_sdk_error("autoscaling:CreateAutoScalingGroup", &e).to_string())
                        }
                    };
                    let delete_group = |n: String| {
                        let client = asg_admin.clone();
                        async move {
                            let _ = client.delete_auto_scaling_group().auto_scaling_group_name(&n).force_delete(true).send().await;
                        }
                    };
                    // $Latest must be denied (LaunchTemplateVersionSpecified).
                    let pa = format!("{stem}-pa");
                    let r = create_group(asg_env.clone(), pa.clone(), "$Latest".into(), probe_tags(&pa)).await;
                    eprintln!("RESULT probe: CreateAutoScalingGroup with $Latest as the environment admin: {r:?}");
                    if r.is_ok() {
                        eprintln!("RESULT WARNING: $Latest was allowed");
                        delete_group(pa).await;
                    }
                    // The control: the same create pinned to version 1 is allowed
                    // (InstanceTypes filled from the template).
                    let pb = format!("{stem}-pb");
                    let r = create_group(asg_env.clone(), pb.clone(), "1".into(), probe_tags(&pb)).await;
                    eprintln!("RESULT probe: the same create pinned to version 1 (control): {r:?}");
                    if r.is_ok() {
                        let d = asg_env.delete_auto_scaling_group().auto_scaling_group_name(&pb).send().await;
                        eprintln!("RESULT probe: DeleteAutoScalingGroup of it as the environment admin: {:?}", d.map(|_| ()).map_err(|e| classify_sdk_error("autoscaling:DeleteAutoScalingGroup", &e).to_string()));
                        delete_group(pb).await;
                    }
                    // A template version naming an unsupported type, then a group pinned to it.
                    let (made, out, err) = aws_cli(
                        Some(&akeys),
                        &region,
                        &["ec2", "create-launch-template-version", "--launch-template-name", &stem, "--source-version", "1",
                          "--launch-template-data", r#"{"InstanceType":"p5.48xlarge"}"#, "--query", "LaunchTemplateVersion.VersionNumber"],
                    )
                    .await;
                    eprintln!("RESULT probe: CreateLaunchTemplateVersion with p5.48xlarge as the environment admin: ok {made} {} {}", out.trim(), err.trim());
                    if made {
                        let pc = format!("{stem}-pc");
                        let r = create_group(asg_env.clone(), pc.clone(), out.trim().to_string(), probe_tags(&pc)).await;
                        eprintln!("RESULT probe: CreateAutoScalingGroup pinned to the p5.48xlarge version: {r:?}");
                        if r.is_ok() {
                            eprintln!("RESULT WARNING: a group of an unsupported type was allowed");
                            delete_group(pc).await;
                        }
                    }
                    // A Triple-C capacity provider over a group that isn't Triple-C's.
                    let foreign_group = format!("e2e-untagged-{}", &short[..10]);
                    p5_lock(&p5).probe_groups.push(foreign_group.clone());
                    let r = create_group(asg_admin.clone(), foreign_group.clone(), "1".into(), Vec::new()).await;
                    eprintln!("RESULT probe: account admin creates the untagged group {foreign_group}: {r:?}");
                    if r.is_ok() {
                        let arn = ok("describe the untagged group", asg_admin.describe_auto_scaling_groups().auto_scaling_group_names(&foreign_group).send().await)
                            .auto_scaling_groups()
                            .first()
                            .and_then(|g| g.auto_scaling_group_arn().map(str::to_string))
                            .unwrap();
                        let pd = format!("{stem}-pd");
                        let r = ecs_env
                            .create_capacity_provider()
                            .name(&pd)
                            .auto_scaling_group_provider(
                                aws_sdk_ecs::types::AutoScalingGroupProvider::builder()
                                    .auto_scaling_group_arn(arn)
                                    .managed_termination_protection(aws_sdk_ecs::types::ManagedTerminationProtection::Disabled)
                                    .build()
                                    .unwrap(),
                            )
                            .set_tags(Some(
                                group_tags
                                    .iter()
                                    .filter(|(k, _)| k.starts_with("triple-c:"))
                                    .map(|(k, v)| aws_sdk_ecs::types::Tag::builder().key(k).value(v).build())
                                    .collect(),
                            ))
                            .send()
                            .await;
                        eprintln!(
                            "RESULT probe: CreateCapacityProvider {pd} over the untagged group, as the environment admin: {:?}",
                            r.as_ref().map(|_| ()).map_err(|e| classify_sdk_error("ecs:CreateCapacityProvider", e).to_string())
                        );
                        if r.is_ok() {
                            eprintln!("RESULT WARNING: a Triple-C capacity provider over a foreign group was allowed");
                            let _ = ecs_admin.delete_capacity_provider().capacity_provider(&pd).send().await;
                        }
                        delete_group(foreign_group).await;
                    }
                }

                // ── 2. start 1: cold, with an instance launch ──
                let mut settings = AppSettings::default();
                settings.hosts = vec![conn];
                settings.image_source = ImageSource::Custom;
                settings.custom_image_name = Some(image.clone());
                let mut project = Project::new("e2e-ec2".into(), Vec::new());
                project.host_id = Some("e2e-wsuser".into());
                project.aws_placement = Some(AwsPlacement { region: region.clone(), environment: name.clone() });
                project.allow_docker_access = true;
                project.vpn_support_enabled = true;
                project.sandbox_mode_enabled = true;
                project.aws = Some(AwsWorkspaceConfig {
                    launch_type: LaunchType::Ec2,
                    cpu: 1024,
                    memory_mib: 4096,
                    storage: StorageMode::Hybrid,
                    wip_push_on_stop: true,
                    ..Default::default()
                });
                let id = project.id.clone();
                {
                    let mut g = p5_lock(&p5);
                    g.project_id = Some(id.clone());
                    g.secrets.push(crate::aws::tofu_modules::project_secret_name(&name, &okey, &id));
                }
                let store_dir = tempfile::tempdir().unwrap();
                let store = ProjectsStore::over_file_for_test(store_dir.path().join("projects.json"), vec![project.clone()]);
                let host = ProjectHost::for_project(&project);
                let current = |store: &ProjectsStore| store.get(&id).unwrap();
                let api = SdkTaskApi::new(&ws_cfg);
                let steps: Arc<std::sync::Mutex<Vec<(Duration, String)>>> = Default::default();
                let t0 = std::sync::Mutex::new(std::time::Instant::now());
                let progress = |m: &str| {
                    let at = t0.lock().unwrap().elapsed();
                    eprintln!("  progress +{at:?}: {m}");
                    steps.lock().unwrap().push((at, m.to_string()));
                };

                let started = host.ensure_started(&current(&store), &settings, &store, &progress).await;
                if let Err(err) = &started {
                    // Debugging aid: hold the workspace for an admin to look
                    // at before the guard tears it down.
                    let pause: u64 = std::env::var("TRIPLEC_E2E_PAUSE_SECS").ok().and_then(|v| v.parse().ok()).unwrap_or(0);
                    if pause > 0 {
                        eprintln!("RESULT start 1 failed ({err}); pausing {pause}s for inspection (task {:?})", current(&store).aws.and_then(|a| a.task_arn));
                        tokio::time::sleep(Duration::from_secs(pause)).await;
                    }
                }
                let target = started.expect("EC2 start 1");
                let first_start = t0.lock().unwrap().elapsed();
                let s1: Vec<(Duration, String)> = std::mem::take(&mut *steps.lock().unwrap());
                eprintln!("RESULT EC2 start 1 (cold, instance launch): {first_start:?}; steps {s1:?}");
                assert!(s1.iter().any(|(_, m)| m == PROGRESS_INSTANCE), "no \"Starting an instance\" step: {s1:?}");
                let aws1 = current(&store).aws.unwrap();
                let task1 = aws1.task_arn.clone().expect("the task ARN is stored");
                let revision1 = aws1.task_definition_arn.clone().expect("the revision is stored");
                assert!(task1.contains(&format!(":task/{stem}/")), "the task runs on the user's cluster: {task1}");
                let d = api.describe(&rec.cluster_arn, std::slice::from_ref(&task1)).await.expect("describe task 1");
                eprintln!("RESULT task 1: {:?}", d.first().map(|t| (&t.last_status, &t.container_instance_arn)));
                eprintln!("RESULT group after start 1: {:?}", group_size(&asg_admin, &stem).await);
                let td = ok("describe revision 1", ecs_admin.describe_task_definition().task_definition(&revision1).send().await);
                let td = td.task_definition().unwrap();
                eprintln!(
                    "RESULT revision 1: network {:?}, compat {:?}, volumes {:?}",
                    td.network_mode(),
                    td.requires_compatibilities(),
                    td.volumes().iter().map(|v| v.name().unwrap_or_default()).collect::<Vec<_>>()
                );

                // ── through the agent (ECS Exec port forward into the bridge task) ──
                let (out, code) = sh(&target, "claude", "echo hello-from-ec2; id").await;
                eprintln!("RESULT agent tunnel over ECS Exec into the bridge-mode task: exit {code}: {}", out.trim());
                assert_eq!(code, 0, "{out}");
                let (mut docker_ok, mut vpn_ok, mut src_valid_mark, mut bwrap_ok) = (true, true, true, false);
                if !quick {
                    let (out, code) = sh(&target, "claude", "ls -l /var/run/docker.sock; docker ps 2>&1; docker version --format '{{.Server.Version}}' 2>&1").await;
                    eprintln!("RESULT docker ps as claude (exit {code}):\n{}", out.trim());
                    docker_ok = code == 0;
                    let (out, code) = sh(
                        &target,
                        "root",
                        "ip link add wg0 type wireguard && ip link show wg0 && ip link del wg0 && echo WG-OK; \
                         ip tuntap add tun0 mode tun && ip link show tun0 && ip link del tun0 && echo TUN-OK; \
                         ls -l /dev/net/tun; sysctl net.ipv4.conf.all.src_valid_mark; grep CapEff /proc/self/status",
                    )
                    .await;
                    eprintln!("RESULT VPN pieces as root (exit {code}):\n{}", out.trim());
                    let (sudo_out, _) = sh(&target, "claude", "sudo -n ip link add wg1 type wireguard && sudo -n ip link del wg1 && echo SUDO-WG-OK 2>&1").await;
                    eprintln!("RESULT wireguard via sudo as claude: {}", sudo_out.trim());
                    vpn_ok = out.contains("WG-OK") || out.contains("TUN-OK");
                    src_valid_mark = out.contains("src_valid_mark = 1");
                    let (bwrap, _) = sh(
                        &target,
                        "claude",
                        "bwrap --unshare-user --ro-bind / / true 2>&1; echo \"unshare-user rc=$?\"; \
                         bwrap --ro-bind / / --dev /dev --proc /proc --unshare-all --die-with-parent true 2>&1; echo \"unshare-all rc=$?\"; \
                         cat /proc/sys/user/max_user_namespaces 2>&1; uname -r",
                    )
                    .await;
                    eprintln!("RESULT E8 bwrap on EC2:\n{}", bwrap.trim());
                    bwrap_ok = bwrap.contains("unshare-user rc=0");
                    let (imds, _) = sh(
                        &target,
                        "claude",
                        "tok=$(curl -s -m 3 -X PUT http://169.254.169.254/latest/api/token -H 'X-aws-ec2-metadata-token-ttl-seconds: 60'); \
                         echo \"token rc=$? len=${#tok}\"; \
                         curl -s -m 3 -o /dev/null -w 'imdsv1 http=%{http_code}\\n' http://169.254.169.254/latest/meta-data/; \
                         gw=$(ip route | awk '/default/ {print $3}'); echo \"gateway $gw\"; \
                         for h in \"$gw\" 127.0.0.1; do curl -s -m 3 -o /dev/null -w \"introspection $h:51678 http=%{http_code}\\n\" http://$h:51678/v1/metadata; done; \
                         curl -s -m 3 http://$gw:51678/v1/metadata | head -c 300; echo; \
                         test -n \"$AWS_CONTAINER_CREDENTIALS_RELATIVE_URI\" && curl -s -m 3 http://169.254.170.2$AWS_CONTAINER_CREDENTIALS_RELATIVE_URI | grep -q AccessKeyId && echo TASK-ROLE-CREDS-OK || echo task-role-creds-missing",
                    )
                    .await;
                    eprintln!("RESULT IMDS / introspection from the container:\n{}", imds.trim());
                    assert!(imds.contains("len=0"), "the container got an IMDS token: {imds}");

                    // ── 4. isolation: RunTask on another triple-c-user's cluster ──
                    let other = format!("e2eother{}", &short[6..10]);
                    let other_cluster = user_ec2_stem(&name, &other);
                    let made = ok("admin creates another user's (empty) cluster", ecs_admin.create_cluster().cluster_name(&other_cluster).send().await);
                    let other_arn = made.cluster().and_then(|c| c.cluster_arn()).unwrap().to_string();
                    let fake = UserCompute {
                        instance_type: "m7i.large".into(),
                        max_instances: 1,
                        user_tag: other.clone(),
                        cluster_arn: other_arn.clone(),
                        capacity_provider: other_cluster.clone(),
                    };
                    let r = api.run_task(ec2_run_task_request(&fake, &revision1, &workspace_tags(&owner, &id, &name))).await.map_err(ec2_run_error);
                    eprintln!("RESULT RunTask on {other_cluster} as triple-c-user {user_tag}: {r:?}");
                    match r {
                        Err(crate::aws::error::AwsError::AccessDenied { message, .. }) => assert!(message.contains(EC2_TAG_MISMATCH), "{message}"),
                        other => panic!("RunTask on another user's cluster: expected AccessDenied, got {other:?}"),
                    }
                    let _ = ecs_admin.delete_cluster().cluster(&other_arn).send().await;
                }

                // ── 5. stop 1, then the scale-in to zero ──
                let t2 = std::time::Instant::now();
                host.stop_project(&current(&store), &settings).await.expect("stop 1");
                poll(Duration::from_secs(300), Duration::from_secs(5), || async {
                    let d = api.describe(&rec.cluster_arn, std::slice::from_ref(&task1)).await.ok()?;
                    (d.first()?.last_status == "STOPPED").then_some(())
                })
                .await
                .expect("task 1 never stopped");
                let stopped_after = t2.elapsed();
                eprintln!("RESULT stop 1: STOPPED after {stopped_after:?}");
                store.set_aws_runtime(&id, None, aws1.access_point_id.clone(), Some(revision1.clone()), None).unwrap();
                let mut scale_in = Duration::ZERO;
                if !quick {
                    let mut last: Option<(usize, i32)> = None;
                    let mut scaled = None;
                    let deadline = std::time::Instant::now() + Duration::from_secs(40 * 60);
                    while std::time::Instant::now() < deadline {
                        let now = group_size(&asg_admin, &stem).await;
                        let key = now.as_ref().map(|n| (n.0, n.1));
                        if key != last {
                            eprintln!("  group +{:?} after STOPPED: {now:?}", t2.elapsed() - stopped_after);
                            last = key;
                        }
                        if key == Some((0, 0)) {
                            scaled = Some(());
                            break;
                        }
                        tokio::time::sleep(Duration::from_secs(30)).await;
                    }
                    scale_in = t2.elapsed() - stopped_after;
                    eprintln!("RESULT scale-in to 0 instances: {scaled:?} after {:.1} min from STOPPED", scale_in.as_secs_f64() / 60.0);
                    assert!(scaled.is_some(), "the group did not scale back to zero within 40 minutes");
                }

                // ── start 2 ──
                *t0.lock().unwrap() = std::time::Instant::now();
                let target2 = host.ensure_started(&current(&store), &settings, &store, &progress).await.expect("EC2 start 2");
                let second_start = t0.lock().unwrap().elapsed();
                let s2: Vec<(Duration, String)> = std::mem::take(&mut *steps.lock().unwrap());
                eprintln!("RESULT EC2 start 2 ({}): {second_start:?}; steps {s2:?}", if quick { "warm: right after stop 1" } else { "after scale-in" });
                let (out, code) = sh(&target2, "claude", "echo again; docker ps -q | wc -l").await;
                assert_eq!(code, 0, "{out}");
                let aws2 = current(&store).aws.unwrap();
                let task2 = aws2.task_arn.clone().unwrap();
                host.stop_project(&current(&store), &settings).await.expect("stop 2");
                poll(Duration::from_secs(300), Duration::from_secs(5), || async {
                    let d = api.describe(&rec.cluster_arn, std::slice::from_ref(&task2)).await.ok()?;
                    (d.first()?.last_status == "STOPPED").then_some(())
                })
                .await
                .expect("task 2 never stopped");
                store.set_aws_runtime(&id, None, aws2.access_point_id.clone(), aws2.task_definition_arn.clone(), None).unwrap();
                let removed = host.remove_aws_resources(&current(&store), &settings, true).await;
                eprintln!("RESULT remove_aws_resources: {:?}", removed.as_ref().map_err(|e| e.chars().take(400).collect::<String>()));
                removed.expect("remove the project's resources");

                // ── 7. the onboarding re-applied without EC2, an instance still up ──
                eprintln!("RESULT group before the re-apply without EC2: {:?}", group_size(&asg_admin, &stem).await);
                let (keys, acfg, _) = t.session(&admin).await;
                let vars = prepare_onboarding(&SdkOnboardApi::new(&acfg), &name, &region, &account, &owner, Some(Ec2Choice::None))
                    .await
                    .expect("the pre-apply step without EC2");
                let t5 = std::time::Instant::now();
                let (_, outcome, denied) = t
                    .run(ModuleKind::UserOnboard, Deployment::User { env: &name, owner_key: &okey }, &vars, TofuOp::Apply, t.keys_env(&keys))
                    .await;
                eprintln!("RESULT re-apply without EC2: {:?} in {:?}", outcome.summary, t5.elapsed());
                assert_ok("re-apply the onboarding without EC2", &outcome, &denied);
                let g = group_size(&asg_admin, &stem).await;
                let inst = live_env_instances(&region, &name).await;
                let cps = ecs_admin.describe_capacity_providers().capacity_providers(&stem).send().await;
                let cl = ecs_admin.describe_clusters().clusters(&stem).send().await;
                let (_, lt, lt_err) = aws_cli(None, &region, &["ec2", "describe-launch-templates", "--launch-template-names", &stem]).await;
                let prof = t.e.iam.get_instance_profile().instance_profile_name(&role_name).send().await;
                let role = t.e.iam.get_role().role_name(&role_name).send().await;
                let record = read_user_record(&SdkRuntimeApi::new(&ws_cfg), &name, &okey, &account, &region).await;
                eprintln!(
                    "RESULT after the re-apply: group {g:?}; live instances {inst:?}; provider {:?}; cluster {:?}; template {} {}; profile gone {}; role gone {}; record {record:?}",
                    cps.as_ref().map(|o| o.capacity_providers().iter().map(|c| c.status().cloned()).collect::<Vec<_>>()),
                    cl.as_ref().map(|o| o.clusters().iter().map(|c| c.status().map(str::to_string)).collect::<Vec<_>>()),
                    lt.trim().chars().take(100).collect::<String>(),
                    lt_err.trim().chars().take(100).collect::<String>(),
                    prof.is_err(),
                    role.is_err(),
                );
                assert!(g.is_none(), "the group is gone");
                assert!(inst.is_empty(), "no instance is left: {inst:?}");
                assert!(prof.is_err() && role.is_err(), "the instance profile and role are gone");
                assert!(matches!(record, Ok(UserRecord::Absent)), "{record:?}");

                // ── offboard, then destroy the environment, as the environment admin ──
                if let Some(role) = p5_lock(&p5).exec_role.take() {
                    let _ = t.e.iam.delete_role_policy().role_name(&role).policy_name(ECR_POLICY).send().await;
                }
                let (keys, _, _) = t.session(&admin).await;
                let (_, outcome, denied) = t
                    .run(ModuleKind::UserOnboard, Deployment::User { env: &name, owner_key: &okey }, &vars, TofuOp::Destroy, t.keys_env(&keys))
                    .await;
                assert_ok("offboard", &outcome, &denied);
                let (_, outcome, denied) = t
                    .run(
                        ModuleKind::Environment,
                        Deployment::Environment(&name),
                        &environment_vars(&name, &region, &NetworkChoice::Public, 0).unwrap(),
                        TofuOp::Destroy,
                        t.keys_env(&keys),
                    )
                    .await;
                assert_ok("destroy the environment", &outcome, &denied);
                eprintln!(
                    "RESULT summary: docker {docker_ok}, vpn {vpn_ok}, src_valid_mark {src_valid_mark}, bwrap {bwrap_ok}, start1 {first_start:?}, scale-in {:.1} min, start2 {second_start:?}",
                    scale_in.as_secs_f64() / 60.0
                );
                let _ = ws;
                assert!(docker_ok, "docker ps failed as claude");
                assert!(vpn_ok && src_valid_mark, "the VPN pieces are missing");
            });
            let body = async {
                if tokio::time::timeout(PHASE7_TIMEOUT, inner).await.is_err() {
                    panic!("the phase-7 body did not finish in {PHASE7_TIMEOUT:?}");
                }
            };
            let result = std::panic::AssertUnwindSafe(body).catch_unwind().await;
            let guard = std::mem::take(&mut *p5_lock(&p5));
            phase5_cleanup(&t.e, &guard).await;
            if let Err(panic) = result {
                std::panic::resume_unwind(panic);
            }
        })
        .await;
    }
}
