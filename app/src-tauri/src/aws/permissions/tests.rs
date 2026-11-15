use std::collections::HashSet;
use std::sync::Mutex;

use super::*;
use crate::aws::policies::{checks, Check, ContextValue, Placeholders, Tier};

const USER_ID: &str = "AROAEXAMPLEID:alice@example.com";
const ROLE_ARN: &str =
    "arn:aws:iam::111122223333:role/aws-reserved/sso.amazonaws.com/us-west-2/AWSReservedSSO_Ws_abc";

fn identity() -> CallerIdentity {
    CallerIdentity {
        account: "111122223333".into(),
        arn: "arn:aws:sts::111122223333:assumed-role/AWSReservedSSO_Ws_abc/alice@example.com"
            .into(),
        user_id: USER_ID.into(),
    }
}

const ALL_PROBES: [Probe; 3] = [
    Probe::EcsListClusters,
    Probe::SsmGetParametersByPath,
    Probe::SecretsListSecrets,
];

/// A simulation call as the fake saw it.
#[derive(Debug, Clone)]
struct Simulated {
    principal: String,
    action: String,
    resource: String,
    context: Vec<ContextEntry>,
}

struct FakeApi {
    allow: HashSet<&'static str>,
    simulate_denied: bool,
    get_role_denied: bool,
    /// Returned by `resolve_principal` (after `get_role_denied`).
    resolve_error: Option<AwsError>,
    /// Returned by `simulate` for the named action.
    simulate_errors: Vec<(&'static str, AwsError)>,
    probes_ok: HashSet<Probe>,
    /// Returned by every probe not in `probes_ok` (default: AccessDenied).
    probe_error: Option<AwsError>,
    simulated: Mutex<Vec<Simulated>>,
    probed: Mutex<Vec<Probe>>,
}

impl FakeApi {
    fn allowing(tiers: &[Tier]) -> Self {
        let allow = tiers
            .iter()
            .flat_map(|t| checks(*t).iter().map(|c| c.action))
            .collect();
        FakeApi {
            allow,
            simulate_denied: false,
            get_role_denied: false,
            resolve_error: None,
            simulate_errors: Vec::new(),
            probes_ok: ALL_PROBES.into_iter().collect(),
            probe_error: None,
            simulated: Mutex::new(Vec::new()),
            probed: Mutex::new(Vec::new()),
        }
    }

    fn simulated(&self) -> Vec<Simulated> {
        self.simulated.lock().unwrap().clone()
    }
}

impl PermissionApi for FakeApi {
    async fn resolve_principal(&self, principal: PrincipalArn) -> Result<String, AwsError> {
        if self.get_role_denied {
            return Err(AwsError::AccessDenied {
                action: "iam:GetRole".into(),
                message: "not authorized to perform: iam:GetRole".into(),
            });
        }
        if let Some(e) = &self.resolve_error {
            return Err(e.clone());
        }
        Ok(match principal {
            PrincipalArn::Exact(arn) => arn,
            PrincipalArn::AssumedRole { .. } => ROLE_ARN.into(),
        })
    }

    async fn simulate(&self, request: &SimulationRequest) -> Result<bool, AwsError> {
        self.simulated.lock().unwrap().push(Simulated {
            principal: request.principal.clone(),
            action: request.action.clone(),
            resource: request.resource.clone(),
            context: request.context.clone(),
        });
        if self.simulate_denied {
            return Err(AwsError::AccessDenied {
                action: "iam:SimulatePrincipalPolicy".into(),
                message: "not authorized to perform: iam:SimulatePrincipalPolicy".into(),
            });
        }
        if let Some((_, e)) = self
            .simulate_errors
            .iter()
            .find(|(a, _)| *a == request.action)
        {
            return Err(e.clone());
        }
        Ok(self.allow.contains(request.action.as_str()))
    }

    async fn probe(&self, probe: Probe) -> Result<(), AwsError> {
        self.probed.lock().unwrap().push(probe);
        if self.probes_ok.contains(&probe) {
            Ok(())
        } else if let Some(e) = &self.probe_error {
            Err(e.clone())
        } else {
            Err(AwsError::AccessDenied {
                action: format!("{probe:?}"),
                message: "denied".into(),
            })
        }
    }
}

fn try_run(api: &FakeApi, id: &CallerIdentity) -> Result<PermissionReport, AwsError> {
    tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap()
        .block_on(check_permissions(api, id, "eu-west-1"))
}

fn run(api: &FakeApi) -> PermissionReport {
    try_run(api, &identity()).unwrap()
}

fn row<'a>(report: &'a PermissionReport, t: Tier, action: &str) -> &'a ActionResult {
    tier(report, t)
        .results
        .iter()
        .find(|r| r.action == action)
        .unwrap_or_else(|| panic!("{action} not in {t:?}"))
}

fn throttled() -> AwsError {
    AwsError::Other {
        message: "Rate exceeded (Throttling)".into(),
    }
}

fn tier(report: &PermissionReport, t: Tier) -> &TierResult {
    report.tiers.iter().find(|r| r.tier == t).unwrap()
}

fn verdict(report: &PermissionReport, t: Tier, action: &str) -> Verdict {
    tier(report, t)
        .results
        .iter()
        .find(|r| r.action == action)
        .unwrap_or_else(|| panic!("{action} not in {t:?}"))
        .verdict
        .clone()
}

#[test]
fn a_full_workspace_policy_reports_workspace_user() {
    let api = FakeApi::allowing(&[Tier::WorkspaceUser]);
    let report = run(&api);
    assert_eq!(report.method, Method::Simulated);
    assert_eq!(report.summary, Summary::WorkspaceUser);
    assert_eq!(report.region, "eu-west-1");
    assert_eq!(report.identity, identity());
    let tiers: Vec<Tier> = report.tiers.iter().map(|t| t.tier).collect();
    assert_eq!(tiers, [Tier::WorkspaceUser, Tier::EnvironmentAdmin]);
    assert!(tier(&report, Tier::WorkspaceUser)
        .results
        .iter()
        .all(|r| r.verdict == Verdict::Allowed));
    // Every check was simulated against the resolved role, not the session.
    let sims = api.simulated();
    let total = checks(Tier::WorkspaceUser).len() + checks(Tier::EnvironmentAdmin).len();
    assert_eq!(sims.len(), total);
    assert!(sims.iter().all(|s| s.principal == ROLE_ARN));
}

#[test]
fn admin_and_workspace_policies_report_environment_admin() {
    let api = FakeApi::allowing(&[Tier::WorkspaceUser, Tier::EnvironmentAdmin]);
    let report = run(&api);
    assert_eq!(report.method, Method::Simulated);
    assert_eq!(report.summary, Summary::EnvironmentAdmin);
}

#[test]
fn a_missing_action_reports_insufficient_and_names_it() {
    let mut api = FakeApi::allowing(&[Tier::WorkspaceUser, Tier::EnvironmentAdmin]);
    api.allow.remove("iam:PassRole");
    let report = run(&api);
    assert_eq!(report.summary, Summary::Insufficient);
    assert_eq!(
        verdict(&report, Tier::WorkspaceUser, "iam:PassRole"),
        Verdict::Denied
    );
    assert_eq!(
        verdict(&report, Tier::WorkspaceUser, "ecs:RunTask"),
        Verdict::Allowed
    );
}

#[test]
fn an_admin_gap_still_reports_workspace_user() {
    let mut api = FakeApi::allowing(&[Tier::WorkspaceUser, Tier::EnvironmentAdmin]);
    api.allow.remove("ec2:CreateVpc");
    let report = run(&api);
    assert_eq!(report.summary, Summary::WorkspaceUser);
    assert_eq!(
        verdict(&report, Tier::EnvironmentAdmin, "ec2:CreateVpc"),
        Verdict::Denied
    );
}

/// An action checked against several resources is one row; any denial wins.
#[test]
fn an_action_checked_twice_is_one_row_and_denied_if_either_is() {
    let api = FakeApi::allowing(&[Tier::WorkspaceUser]);
    let report = run(&api);
    let rows = tier(&report, Tier::WorkspaceUser)
        .results
        .iter()
        .filter(|r| r.action == "ssm:StartSession")
        .count();
    assert_eq!(rows, 1);
    let failed = || Outcome::Failed("Throttling".into());
    assert_eq!(
        aggregate(&[Outcome::Allowed, Outcome::Denied, failed()]),
        (Verdict::Denied, None)
    );
    assert_eq!(
        aggregate(&[Outcome::Allowed, Outcome::Unverified, failed()]),
        (Verdict::Unverified, Some("Throttling".into()))
    );
    assert_eq!(
        aggregate(&[Outcome::Allowed, Outcome::Unverified]),
        (Verdict::Unverified, None)
    );
    assert_eq!(aggregate(&[Outcome::Allowed]), (Verdict::Allowed, None));
}

fn context_value<'a>(s: &'a Simulated, key: &str) -> Option<&'a ContextEntry> {
    s.context.iter().find(|e| e.key == key)
}

#[test]
fn owner_context_is_sent_with_owner_scoped_checks() {
    let api = FakeApi::allowing(&[Tier::WorkspaceUser]);
    run(&api);
    let sims = api.simulated();
    let run_task = sims.iter().find(|s| s.action == "ecs:RunTask").unwrap();
    let owner = context_value(run_task, "aws:RequestTag/triple-c:owner").unwrap();
    assert_eq!(owner.values, [USER_ID]);
    // Literals are rendered like resources.
    let cluster = context_value(run_task, "ecs:cluster").unwrap();
    assert_eq!(
        cluster.values,
        ["arn:aws:ecs:eu-west-1:111122223333:cluster/triple-c-probe"]
    );
    assert_eq!(
        run_task.resource,
        "arn:aws:ecs:eu-west-1:111122223333:task-definition/triple-c-probe:1"
    );
    // aws:userid on every simulation.
    for s in &sims {
        let userid = context_value(s, "aws:userid")
            .unwrap_or_else(|| panic!("{} has no aws:userid", s.action));
        assert_eq!(userid.values, [USER_ID], "{}", s.action);
    }
}

#[test]
fn the_principal_placeholder_is_the_resolved_role() {
    let api = FakeApi::allowing(&[Tier::WorkspaceUser]);
    run(&api);
    let sims = api.simulated();
    let get_role = sims
        .iter()
        .find(|s| s.action == "iam:GetRole" && s.resource == ROLE_ARN)
        .expect("iam:GetRole on the caller's own role");
    // The policies name a user's own ARN by `${aws:username}`.
    assert_eq!(
        context_value(get_role, "aws:username").unwrap().values,
        [ROLE_ARN.rsplit('/').next().unwrap()]
    );
}

#[test]
fn a_repeated_context_key_is_one_string_list_entry() {
    static CTX: &[(&str, ContextValue)] = &[
        ("aws:TagKeys", ContextValue::Literal("triple-c:owner")),
        (
            "ecs:cluster",
            ContextValue::Literal("arn:{region}:{account}"),
        ),
        ("aws:TagKeys", ContextValue::Literal("triple-c:component")),
        ("aws:TagKeys", ContextValue::OwnerId),
    ];
    let check = Check {
        action: "ecs:TagResource",
        resource: "arn:aws:ecs:{region}:{account}:task/x",
        context: CTX,
        write: true,
    };
    let p = Placeholders {
        region: "eu-west-1",
        account: "111122223333",
        principal: ROLE_ARN,
    };
    let req = simulation_request(&check, &p, USER_ID);
    assert_eq!(req.principal, ROLE_ARN);
    assert_eq!(req.action, "ecs:TagResource");
    assert_eq!(req.resource, "arn:aws:ecs:eu-west-1:111122223333:task/x");
    assert_eq!(
        req.context,
        [
            ContextEntry {
                key: "aws:TagKeys".into(),
                values: vec![
                    "triple-c:owner".into(),
                    "triple-c:component".into(),
                    USER_ID.into()
                ],
            },
            ContextEntry {
                key: "ecs:cluster".into(),
                values: vec!["arn:eu-west-1:111122223333".into()],
            },
            ContextEntry {
                key: "aws:userid".into(),
                values: vec![USER_ID.into()],
            },
        ]
    );
    assert!(req.context[0].is_list());
    assert!(!req.context[1].is_list());
}

#[test]
fn a_check_naming_aws_userid_itself_is_not_sent_it_twice() {
    static CTX: &[(&str, ContextValue)] = &[("aws:userid", ContextValue::OwnerId)];
    let check = Check {
        action: "sts:GetCallerIdentity",
        resource: "*",
        context: CTX,
        write: false,
    };
    let p = Placeholders {
        region: "r",
        account: "a",
        principal: "p",
    };
    let req = simulation_request(&check, &p, USER_ID);
    assert_eq!(
        req.context,
        [ContextEntry {
            key: "aws:userid".into(),
            values: vec![USER_ID.into()],
        }]
    );
}

#[test]
fn a_denied_simulation_falls_back_to_probes_and_marks_writes_unverified() {
    let mut api = FakeApi::allowing(&[Tier::WorkspaceUser, Tier::EnvironmentAdmin]);
    api.simulate_denied = true;
    let report = run(&api);
    match &report.method {
        Method::Probed { reason } => {
            assert!(reason.contains("iam:SimulatePrincipalPolicy"), "{reason}")
        }
        m => panic!("expected Probed, got {m:?}"),
    }
    // Only the first simulation was tried.
    assert_eq!(api.simulated().len(), 1);
    let mut probed = api.probed.lock().unwrap().clone();
    probed.sort_by_key(|p| format!("{p:?}"));
    let mut all = ALL_PROBES.to_vec();
    all.sort_by_key(|p| format!("{p:?}"));
    assert_eq!(probed, all);

    let ws = Tier::WorkspaceUser;
    assert_eq!(verdict(&report, ws, "ecs:RunTask"), Verdict::Unverified);
    assert_eq!(verdict(&report, ws, "iam:PassRole"), Verdict::Unverified);
    // Reads with a probe get its result.
    assert_eq!(verdict(&report, ws, "ecs:ListClusters"), Verdict::Allowed);
    assert_eq!(
        verdict(&report, ws, "ecs:DescribeClusters"),
        Verdict::Allowed
    );
    assert_eq!(
        verdict(&report, ws, "ssm:GetParametersByPath"),
        Verdict::Allowed
    );
    assert_eq!(
        verdict(&report, ws, "secretsmanager:ListSecrets"),
        Verdict::Allowed
    );
    // Reads with no probe are unverified.
    assert_eq!(
        verdict(&report, ws, "logs:FilterLogEvents"),
        Verdict::Unverified
    );
    assert_eq!(verdict(&report, ws, "iam:GetRole"), Verdict::Unverified);
    assert_eq!(
        verdict(&report, ws, "sts:GetCallerIdentity"),
        Verdict::Unverified
    );
    // The fallback itself is evidence: the caller may not simulate itself,
    // which both tiers need.
    for t in [ws, Tier::EnvironmentAdmin] {
        assert_eq!(
            verdict(&report, t, "iam:SimulatePrincipalPolicy"),
            Verdict::Denied
        );
    }
    assert_eq!(report.summary, Summary::Insufficient);
}

#[test]
fn a_denied_get_role_falls_back_to_probes() {
    let mut api = FakeApi::allowing(&[Tier::WorkspaceUser]);
    api.get_role_denied = true;
    let report = run(&api);
    match &report.method {
        Method::Probed { reason } => assert!(reason.contains("iam:GetRole"), "{reason}"),
        m => panic!("expected Probed, got {m:?}"),
    }
    assert!(api.simulated().is_empty());
    assert_eq!(api.probed.lock().unwrap().len(), ALL_PROBES.len());
    // Denied on the caller's own role: direct evidence.
    assert_eq!(
        verdict(&report, Tier::WorkspaceUser, "iam:GetRole"),
        Verdict::Denied
    );
    assert_eq!(report.summary, Summary::Insufficient);
}

#[test]
fn an_unsimulatable_principal_falls_back_to_probes() {
    let api = FakeApi::allowing(&[Tier::WorkspaceUser]);
    let mut id = identity();
    id.arn = "arn:aws:iam::111122223333:root".into();
    let report = try_run(&api, &id).unwrap();
    match &report.method {
        Method::Probed { reason } => assert!(reason.contains("simulation"), "{reason}"),
        m => panic!("expected Probed, got {m:?}"),
    }
    assert!(api.simulated().is_empty());
}

/// R15: the admin tier is nearly all writes, so a probed run can't vouch
/// for it — every probe passing still caps at WorkspaceUser.
#[test]
fn a_probed_run_never_claims_environment_admin() {
    let api = FakeApi::allowing(&[Tier::WorkspaceUser, Tier::EnvironmentAdmin]);
    let mut id = identity();
    id.arn = "arn:aws:iam::111122223333:root".into();
    let report = try_run(&api, &id).unwrap();
    assert!(matches!(report.method, Method::Probed { .. }));
    assert!(tier(&report, Tier::EnvironmentAdmin)
        .results
        .iter()
        .all(|r| r.verdict != Verdict::Denied));
    assert_eq!(report.summary, Summary::WorkspaceUser);
}

#[test]
fn throttled_simulations_block_the_tier_and_carry_notes() {
    let mut api = FakeApi::allowing(&[Tier::WorkspaceUser, Tier::EnvironmentAdmin]);
    api.simulate_errors = vec![("ec2:CreateVpc", throttled())];
    let report = run(&api);
    assert_eq!(report.method, Method::Simulated);
    let r = row(&report, Tier::EnvironmentAdmin, "ec2:CreateVpc");
    assert_eq!(r.verdict, Verdict::Unverified);
    assert!(r.note.as_deref().unwrap().contains("Throttling"), "{r:?}");
    // Allowed rows carry no note.
    assert_eq!(row(&report, Tier::WorkspaceUser, "ecs:RunTask").note, None);
    assert_eq!(report.summary, Summary::WorkspaceUser);

    // In the workspace tier, it blocks everything.
    let mut api = FakeApi::allowing(&[Tier::WorkspaceUser, Tier::EnvironmentAdmin]);
    api.simulate_errors = vec![("ecs:RunTask", throttled())];
    let report = run(&api);
    assert_eq!(report.summary, Summary::Insufficient);
    assert!(row(&report, Tier::WorkspaceUser, "ecs:RunTask")
        .note
        .is_some());
}

#[test]
fn a_throttled_first_simulation_is_a_note_not_a_fallback() {
    let mut api = FakeApi::allowing(&[Tier::WorkspaceUser, Tier::EnvironmentAdmin]);
    let first = checks(Tier::WorkspaceUser)[0].action;
    api.simulate_errors = vec![(first, throttled())];
    let report = run(&api);
    assert_eq!(report.method, Method::Simulated);
    assert!(row(&report, Tier::WorkspaceUser, first).note.is_some());
    assert_eq!(report.summary, Summary::Insufficient);
}

#[test]
fn an_expired_sso_session_mid_check_is_an_error() {
    let mut api = FakeApi::allowing(&[Tier::WorkspaceUser, Tier::EnvironmentAdmin]);
    let sso = AwsError::SsoExpired {
        profile: "dev".into(),
    };
    api.simulate_errors = vec![("logs:FilterLogEvents", sso.clone())];
    assert_eq!(try_run(&api, &identity()).unwrap_err(), sso);
}

#[test]
fn credential_errors_anywhere_are_errors() {
    let bad = AwsError::InvalidCredentials {
        message: "ExpiredToken".into(),
    };
    // From resolving the principal.
    let mut api = FakeApi::allowing(&[Tier::WorkspaceUser]);
    api.resolve_error = Some(bad.clone());
    assert_eq!(try_run(&api, &identity()).unwrap_err(), bad);
    // From the first simulation.
    let mut api = FakeApi::allowing(&[Tier::WorkspaceUser]);
    api.simulate_errors = vec![(checks(Tier::WorkspaceUser)[0].action, bad.clone())];
    assert_eq!(try_run(&api, &identity()).unwrap_err(), bad);
    // From a probe.
    let mut api = FakeApi::allowing(&[Tier::WorkspaceUser]);
    api.get_role_denied = true;
    api.probes_ok.remove(&Probe::SecretsListSecrets);
    api.probe_error = Some(AwsError::KeysMissing);
    assert_eq!(
        try_run(&api, &identity()).unwrap_err(),
        AwsError::KeysMissing
    );
}

#[test]
fn a_transport_error_resolving_the_principal_is_an_error() {
    let mut api = FakeApi::allowing(&[Tier::WorkspaceUser]);
    let net = AwsError::Other {
        message: "dispatch failure: connection refused".into(),
    };
    api.resolve_error = Some(net.clone());
    assert_eq!(try_run(&api, &identity()).unwrap_err(), net);
    assert!(api.probed.lock().unwrap().is_empty());
}

#[test]
fn a_failed_probe_marks_its_reads_denied() {
    let mut api = FakeApi::allowing(&[Tier::WorkspaceUser]);
    api.simulate_denied = true;
    api.probes_ok.remove(&Probe::EcsListClusters);
    let report = run(&api);
    let ws = Tier::WorkspaceUser;
    assert_eq!(verdict(&report, ws, "ecs:ListClusters"), Verdict::Denied);
    assert_eq!(
        verdict(&report, ws, "ecs:DescribeClusters"),
        Verdict::Denied
    );
    assert_eq!(
        verdict(&report, ws, "secretsmanager:ListSecrets"),
        Verdict::Allowed
    );
    assert_eq!(report.summary, Summary::Insufficient);
}

#[test]
fn a_probe_failing_for_another_reason_leaves_its_reads_unverified_and_blocks() {
    let mut api = FakeApi::allowing(&[Tier::WorkspaceUser]);
    let mut id = identity();
    id.arn = "arn:aws:iam::111122223333:root".into();
    api.probes_ok.remove(&Probe::EcsListClusters);
    api.probe_error = Some(AwsError::Other {
        message: "connection reset".into(),
    });
    let report = try_run(&api, &id).unwrap();
    let r = row(&report, Tier::WorkspaceUser, "ecs:ListClusters");
    assert_eq!(r.verdict, Verdict::Unverified);
    assert!(r.note.as_deref().unwrap().contains("connection reset"));
    // Design-level unverified rows carry no note.
    assert_eq!(row(&report, Tier::WorkspaceUser, "ecs:RunTask").note, None);
    assert_eq!(report.summary, Summary::Insufficient);
}

#[test]
fn the_report_serializes_for_the_ui() {
    let mut api = FakeApi::allowing(&[Tier::WorkspaceUser]);
    api.get_role_denied = true;
    let v = serde_json::to_value(run(&api)).unwrap();
    // Probed because iam:GetRole is denied, which the workspace tier needs.
    assert_eq!(v["summary"], "insufficient");
    assert!(v["tiers"][0]["results"][0]
        .as_object()
        .unwrap()
        .contains_key("note"));
    assert_eq!(v["tiers"][0]["tier"], "workspace_user");
    assert!(v["method"]["probed"]["reason"].is_string(), "{v}");
    assert!(["allowed", "denied", "unverified"]
        .contains(&v["tiers"][0]["results"][0]["verdict"].as_str().unwrap()));
}

/// Tauri commands need a `Send` future.
#[allow(dead_code)]
fn check_permissions_is_send() {
    fn assert_send<T: Send>(_: T) {}
    fn with<A: PermissionApi>(api: &A, id: &CallerIdentity) {
        assert_send(check_permissions(api, id, "r"));
    }
    let _ = with::<SdkPermissionApi>;
}

/// D8: CloudFormation left the policies, so nothing probes it either.
#[test]
fn no_probe_is_cloudformation() {
    assert_eq!(Probe::ALL, ALL_PROBES);
    for p in Probe::ALL {
        assert!(!p.action().starts_with("cloudformation:"), "{p:?}");
        assert_eq!(Probe::for_action(p.action()), Some(p));
    }
    assert_eq!(Probe::for_action("cloudformation:DescribeStacks"), None);
}
