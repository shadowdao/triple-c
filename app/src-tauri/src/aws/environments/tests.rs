use std::collections::HashMap;
use std::sync::Mutex;

use serde_json::{json, Value};

use super::*;
use crate::aws::tofu_modules::{owner_key, user_role_names};

const ACCOUNT: &str = "123456789012";
const REGION: &str = "eu-west-1";
const USER_ID: &str = "AROAEXAMPLEID:alice";

fn record(name: &str, version: u32) -> Value {
    json!({
        "schema": 1,
        "name": name,
        "template_version": version,
        "cluster_arn": format!("arn:aws:ecs:{REGION}:{ACCOUNT}:cluster/triple-c-{name}"),
        "security_group_id": "sg-0123456789abcdef0",
        "subnet_ids": ["subnet-0123456789abcdef0", "subnet-0fedcba9876543210"],
        "assign_public_ip": true,
        "kms_key_arn": format!(
            "arn:aws:kms:{REGION}:{ACCOUNT}:key/1234abcd-12ab-34cd-56ef-1234567890ab"
        ),
        "log_group": format!("/triple-c/{name}"),
        "network_mode": "public",
        "tenancy": "strict",
    })
}

fn parsed(name: &str, version: u32) -> EnvironmentInfo {
    parse_environment_record(name, &record(name, version).to_string(), ACCOUNT, REGION).unwrap()
}

fn with(name: &str, field: &str, value: Value) -> String {
    let mut r = record(name, 1);
    r[field] = value;
    r.to_string()
}

fn without(name: &str, field: &str) -> String {
    let mut r = record(name, 1);
    r.as_object_mut().unwrap().remove(field);
    r.to_string()
}

#[test]
fn the_lifetime_cap_is_read_when_present() {
    for hours in [0, 8, 168] {
        let rec = with("dev", "max_workspace_hours", json!(hours));
        let info = parse_environment_record("dev", &rec, ACCOUNT, REGION).unwrap();
        assert_eq!(info.max_workspace_hours, hours);
    }
}

#[test]
fn a_version_1_record_reads_as_update_available() {
    use crate::aws::tofu_modules::ENVIRONMENT_MODULE_VERSION;
    let v1 = parsed("dev", 1);
    assert!(v1.template_version < ENVIRONMENT_MODULE_VERSION);
}

#[test]
fn untrusted_environment_records_are_rejected() {
    let valid = parsed("dev", 1);
    assert_eq!(valid.name, "dev");
    assert_eq!(valid.template_version, 1);
    assert_eq!(valid.subnet_ids.len(), 2);
    assert!(valid.assign_public_ip);
    assert_eq!(valid.tenancy, "strict");
    assert_eq!(valid.max_workspace_hours, 0, "a v1 record has no cap");

    let other_account = "999999999999";
    let secret = "s3cr3t-value-never-echoed";
    let cases: Vec<(&str, String, &str)> = vec![
        (
            "too large",
            format!("{{\"pad\":\"{}\"}}", "x".repeat(5000)),
            "4 KiB",
        ),
        ("not json", "not json at all".into(), "JSON"),
        ("json but not an object", "[1,2]".into(), "JSON"),
        ("schema 2", with("dev", "schema", json!(2)), "schema"),
        ("schema missing", without("dev", "schema"), "schema"),
        (
            "name differs from the path",
            with("dev", "name", json!("prod")),
            "name",
        ),
        (
            "cluster in another account",
            with(
                "dev",
                "cluster_arn",
                json!(format!(
                    "arn:aws:ecs:{REGION}:{other_account}:cluster/triple-c-dev"
                )),
            ),
            "cluster_arn",
        ),
        (
            "cluster in another region",
            with(
                "dev",
                "cluster_arn",
                json!(format!(
                    "arn:aws:ecs:us-east-1:{ACCOUNT}:cluster/triple-c-dev"
                )),
            ),
            "cluster_arn",
        ),
        (
            "kms key in another account",
            with(
                "dev",
                "kms_key_arn",
                json!(format!("arn:aws:kms:{REGION}:{other_account}:key/abcd")),
            ),
            "kms_key_arn",
        ),
        (
            "kms arn of another service",
            with(
                "dev",
                "kms_key_arn",
                json!(format!("arn:aws:s3:{REGION}:{ACCOUNT}:key/abcd")),
            ),
            "kms_key_arn",
        ),
        (
            "one subnet",
            with("dev", "subnet_ids", json!(["subnet-0123abcd"])),
            "subnet_ids",
        ),
        (
            "three subnets",
            with(
                "dev",
                "subnet_ids",
                json!(["subnet-0123abcd", "subnet-0123abce", "subnet-0123abcf"]),
            ),
            "subnet_ids",
        ),
        (
            "a subnet that isn't one",
            with("dev", "subnet_ids", json!(["subnet-0123abcd", secret])),
            "subnet_ids",
        ),
        (
            "the same subnet twice",
            with(
                "dev",
                "subnet_ids",
                json!(["subnet-0123abcd", "subnet-0123abcd"]),
            ),
            "two different subnets",
        ),
        (
            "uppercase subnet hex",
            with(
                "dev",
                "subnet_ids",
                json!(["subnet-0123ABCD", "subnet-0123abcd"]),
            ),
            "subnet_ids",
        ),
        (
            "security group that isn't one",
            with("dev", "security_group_id", json!(secret)),
            "security_group_id",
        ),
        (
            "security group of the wrong type",
            with("dev", "security_group_id", json!(7)),
            "security_group_id",
        ),
        (
            "shared tenancy",
            with("dev", "tenancy", json!("shared")),
            "tenancy",
        ),
        (
            "log group outside /triple-c/",
            with("dev", "log_group", json!(secret)),
            "log_group",
        ),
        (
            "unknown network mode",
            with("dev", "network_mode", json!(secret)),
            "network_mode",
        ),
        (
            "assign_public_ip not a bool",
            with("dev", "assign_public_ip", json!("yes")),
            "assign_public_ip",
        ),
        (
            "template_version not a number",
            with("dev", "template_version", json!(secret)),
            "template_version",
        ),
        (
            "cap above a week",
            with("dev", "max_workspace_hours", json!(169)),
            "max_workspace_hours",
        ),
        (
            "cap not a number",
            with("dev", "max_workspace_hours", json!(secret)),
            "max_workspace_hours",
        ),
        (
            "negative cap",
            with("dev", "max_workspace_hours", json!(-1)),
            "max_workspace_hours",
        ),
    ];
    for (what, value, field) in cases {
        let err = parse_environment_record("dev", &value, ACCOUNT, REGION)
            .expect_err(&format!("accepted: {what}"));
        assert!(err.contains(field), "{what}: {err:?} should name {field}");
        assert!(!err.contains(secret), "{what}: {err:?} echoes the value");
        assert!(
            !err.contains(other_account),
            "{what}: {err:?} echoes the value"
        );
    }

    // The path's name is itself checked.
    assert!(
        parse_environment_record("Bad Name", &record("dev", 1).to_string(), ACCOUNT, REGION)
            .is_err()
    );
}

fn nobody_local(_: Deployment) -> LocalRun {
    LocalRun::default()
}

fn running() -> LocalRun {
    LocalRun {
        running: true,
        ..LocalRun::default()
    }
}

fn env_key(env: &str) -> String {
    state_key(Deployment::Environment(env))
}

fn lock_json(who: &str) -> String {
    json!({
        "ID": "8a1c-uuid",
        "Operation": "OperationTypeApply",
        "Info": "",
        "Who": who,
        "Version": "1.10.6",
        "Created": "2026-11-14T10:00:00Z",
        "Path": "triple-c-tfstate/environments/x/terraform.tfstate",
    })
    .to_string()
}

fn objects(states: &[String], locks: &[(String, u64)]) -> StateObjects {
    StateObjects {
        states: states.to_vec(),
        locks: locks
            .iter()
            .map(|(k, age)| (format!("{k}.tflock"), lock_json("admin@laptop"), *age))
            .collect(),
    }
}

fn find<'a>(envs: &'a [DetectedEnvironment], name: &str) -> &'a DetectedEnvironment {
    envs.iter()
        .find(|e| e.name == name)
        .unwrap_or_else(|| panic!("{name} not listed in {envs:#?}"))
}

#[test]
fn a_record_without_state_and_state_without_a_record_are_both_listed() {
    let objs = objects(&[env_key("orphan")], &[]);
    let envs = merge(
        vec![("shared".into(), Ok(parsed("shared", 1)))],
        Some(&objs),
        &nobody_local,
        1,
    );
    assert_eq!(envs.len(), 2, "{envs:#?}");

    let shared = find(&envs, "shared");
    assert_eq!(shared.state, EnvironmentState::ExternallyManaged);
    assert_eq!(shared.actions, vec![EnvironmentAction::Join]);
    assert_eq!(shared.template_version, Some(1));
    assert!(shared.info.is_some());

    let orphan = find(&envs, "orphan");
    assert_eq!(orphan.state, EnvironmentState::Incomplete { lock: None });
    assert_eq!(
        orphan.actions,
        vec![EnvironmentAction::Repair, EnvironmentAction::Destroy]
    );
    assert_eq!(orphan.template_version, None);
    assert!(orphan.info.is_none());

    // Without bucket access (a workspace user) a record is externally
    // managed too, and nothing else is listed.
    let envs = merge(
        vec![("shared".into(), Ok(parsed("shared", 1)))],
        None,
        &nobody_local,
        1,
    );
    assert_eq!(envs.len(), 1);
    assert_eq!(envs[0].state, EnvironmentState::ExternallyManaged);

    // An unreadable record is listed with its reason.
    let envs = merge(
        vec![("bad".into(), Err("tenancy is not strict".into()))],
        None,
        &nobody_local,
        1,
    );
    assert_eq!(
        envs[0].state,
        EnvironmentState::UnreadableRecord {
            reason: "tenancy is not strict".into()
        }
    );
    assert!(envs[0].actions.is_empty());
}

#[test]
fn interrupted_deployments_get_the_right_actions() {
    let stale = 3 * 3600;
    let fresh = 60;

    // A stale lock, no local process: Incomplete, with Unlock.
    let objs = objects(&[env_key("a")], &[(env_key("a"), stale)]);
    let envs = merge(Vec::new(), Some(&objs), &nobody_local, 1);
    let a = find(&envs, "a");
    let EnvironmentState::Incomplete { lock: Some(lock) } = &a.state else {
        panic!("{a:#?}");
    };
    assert_eq!(lock.who, "admin@laptop");
    assert_eq!(lock.operation, "OperationTypeApply");
    assert_eq!(lock.created, "2026-11-14T10:00:00Z");
    assert_eq!(lock.age_secs, stale);
    assert!(!lock.local_process);
    assert_eq!(
        a.actions,
        vec![
            EnvironmentAction::Repair,
            EnvironmentAction::Destroy,
            EnvironmentAction::Unlock
        ]
    );

    // A stale lock on a working environment (valid record and our state):
    // Incomplete, but never Destroy (D6) — an interrupted Update or plan.
    let envs = merge(
        vec![("a".into(), Ok(parsed("a", 1)))],
        Some(&objs),
        &nobody_local,
        1,
    );
    let a = find(&envs, "a");
    assert!(matches!(
        a.state,
        EnvironmentState::Incomplete { lock: Some(_) }
    ));
    assert_eq!(
        a.actions,
        vec![EnvironmentAction::Repair, EnvironmentAction::Unlock]
    );

    // A stale lock with an unreadable record keeps Destroy: there is no
    // valid record, so nothing says the environment works.
    let envs = merge(
        vec![("a".into(), Err("tenancy is not strict".into()))],
        Some(&objs),
        &nobody_local,
        1,
    );
    assert_eq!(
        find(&envs, "a").actions,
        vec![
            EnvironmentAction::Repair,
            EnvironmentAction::Destroy,
            EnvironmentAction::Unlock
        ]
    );

    // A stale lock with a record but no state of ours (a first apply that
    // died before writing state) keeps Destroy too.
    let no_state = objects(&[], &[(env_key("a"), stale)]);
    let envs = merge(
        vec![("a".into(), Ok(parsed("a", 1)))],
        Some(&no_state),
        &nobody_local,
        1,
    );
    assert_eq!(
        find(&envs, "a").actions,
        vec![
            EnvironmentAction::Repair,
            EnvironmentAction::Destroy,
            EnvironmentAction::Unlock
        ]
    );

    // A fresh lock (a first apply: no state object yet): Deploying, nothing
    // to do but wait.
    let objs = objects(&[], &[(env_key("b"), fresh)]);
    let envs = merge(Vec::new(), Some(&objs), &nobody_local, 1);
    let b = find(&envs, "b");
    let EnvironmentState::Deploying { lock: Some(lock) } = &b.state else {
        panic!("{b:#?}");
    };
    assert!(!lock.local_process);
    assert!(b.actions.is_empty());

    // A local process, even under an old lock: Deploying, Follow.
    let objs = objects(&[env_key("c")], &[(env_key("c"), stale)]);
    let local_c = |d: Deployment| {
        if d == Deployment::Environment("c") {
            running()
        } else {
            LocalRun::default()
        }
    };
    let envs = merge(Vec::new(), Some(&objs), &local_c, 1);
    let c = find(&envs, "c");
    let EnvironmentState::Deploying { lock: Some(lock) } = &c.state else {
        panic!("{c:#?}");
    };
    assert!(lock.local_process);
    assert_eq!(c.actions, vec![EnvironmentAction::Follow]);
}

#[test]
fn lock_contents_are_capped_and_sanitized() {
    let nasty = json!({
        "Who": format!("evil\u{1b}[31m\u{202e}name\n{}", "w".repeat(1000)),
        "Operation": "OperationTypeApply",
        "Created": "2026-11-14T10:00:00Z",
    })
    .to_string();
    let lock = lock_info(&nasty, 10, false);
    assert!(!lock.who.chars().any(|c| c.is_control()), "{:?}", lock.who);
    assert!(!lock.who.contains('\u{202e}'));
    assert!(lock.who.starts_with("evil[31mname"), "{:?}", lock.who);
    assert!(lock.who.chars().count() <= 128);

    // Oversized or unparsable contents are a held lock with nothing to say.
    let huge = json!({ "Who": "x".repeat(5000) }).to_string();
    for text in [huge.as_str(), "not json", ""] {
        let lock = lock_info(text, 7, false);
        assert_eq!(lock.who, "");
        assert_eq!(lock.age_secs, 7);
    }
}

#[test]
fn an_older_module_offers_update() {
    let objs = objects(&[env_key("old"), env_key("new")], &[]);
    let envs = merge(
        vec![
            ("old".into(), Ok(parsed("old", 1))),
            ("new".into(), Ok(parsed("new", 2))),
        ],
        Some(&objs),
        &nobody_local,
        2,
    );
    let old = find(&envs, "old");
    assert_eq!(old.state, EnvironmentState::UpdateAvailable);
    assert_eq!(
        old.actions,
        vec![EnvironmentAction::Join, EnvironmentAction::Update]
    );
    assert_eq!(old.template_version, Some(1));
    let new = find(&envs, "new");
    assert_eq!(new.state, EnvironmentState::Ready);
    assert_eq!(new.actions, vec![EnvironmentAction::Join]);
}

/// A `DetectApi` that answers from fixed data and records what it was asked.
struct FakeDetect {
    records: Vec<(String, String)>,
    objects: Option<StateObjects>,
    bucket: Knowable,
    boundary: Knowable,
    roles: HashMap<String, Knowable>,
    /// State objects' contents, by key.
    documents: HashMap<String, String>,
    /// SSM parameters (user records), by name.
    parameters: HashMap<String, String>,
    calls: Mutex<Vec<String>>,
}

impl FakeDetect {
    fn new() -> Self {
        FakeDetect {
            records: Vec::new(),
            objects: None,
            bucket: Knowable::Yes,
            boundary: Knowable::Yes,
            roles: HashMap::new(),
            documents: HashMap::new(),
            parameters: HashMap::new(),
            calls: Mutex::new(Vec::new()),
        }
    }
    fn with_record(mut self, name: &str, version: u32) -> Self {
        self.records.push((
            environment_parameter_name(name),
            record(name, version).to_string(),
        ));
        self
    }
    fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }
    fn log(&self, call: String) {
        self.calls.lock().unwrap().push(call);
    }
}

impl DetectApi for FakeDetect {
    async fn environment_records(&self) -> Result<Vec<(String, String)>, AwsError> {
        self.log("records".into());
        Ok(self.records.clone())
    }
    async fn state_objects(&self, bucket: &str) -> Result<Option<StateObjects>, AwsError> {
        self.log(format!("objects {bucket}"));
        Ok(self.objects.as_ref().map(|o| StateObjects {
            states: o.states.clone(),
            locks: o.locks.clone(),
        }))
    }
    async fn bucket_exists(&self, bucket: &str) -> Result<Knowable, AwsError> {
        self.log(format!("bucket {bucket}"));
        Ok(self.bucket.clone())
    }
    async fn boundary_exists(&self, account: &str) -> Result<Knowable, AwsError> {
        self.log(format!("boundary {account}"));
        Ok(self.boundary.clone())
    }
    async fn role_exists(&self, name: &str) -> Result<Knowable, AwsError> {
        self.log(format!("role {name}"));
        Ok(self.roles.get(name).cloned().unwrap_or(Knowable::No))
    }
    async fn parameter(&self, name: &str) -> Result<Param, AwsError> {
        self.log(format!("parameter {name}"));
        Ok(self.parameters.get(name).cloned().map_or(Param::Missing, Param::Value))
    }
    async fn state_document(&self, bucket: &str, key: &str) -> Result<Option<String>, AwsError> {
        self.log(format!("document {bucket} {key}"));
        Ok(self.documents.get(key).cloned())
    }
}

fn who() -> CallerIdentity {
    CallerIdentity {
        account: ACCOUNT.into(),
        arn: format!("arn:aws:sts::{ACCOUNT}:assumed-role/Admin/alice"),
        user_id: USER_ID.into(),
    }
}

fn run_scan(d: &FakeDetect, local: LocalRuns) -> EnvironmentScan {
    tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap()
        .block_on(scan(d, &who(), REGION, local))
        .unwrap()
}

fn user_key(env: &str) -> String {
    state_key(Deployment::User {
        env,
        owner_key: &owner_key(REGION, USER_ID),
    })
}

fn task_role(env: &str) -> String {
    user_role_names(env, &owner_key(REGION, USER_ID)).0
}

#[test]
fn onboarding_is_found_by_state_then_by_role() {
    let mut d = FakeDetect::new()
        .with_record("bystate", 1)
        .with_record("byrole", 1)
        .with_record("denied", 1)
        .with_record("absent", 1)
        .with_record("joining", 1);
    d.objects = Some(StateObjects {
        states: vec![
            env_key("bystate"),
            env_key("byrole"),
            env_key("denied"),
            env_key("absent"),
            env_key("joining"),
            user_key("bystate"),
        ],
        locks: vec![(
            format!("{}.tflock", user_key("joining")),
            lock_json("me"),
            30,
        )],
    });
    d.roles.insert(task_role("byrole"), Knowable::Yes);
    d.roles.insert(task_role("denied"), Knowable::Unknown);
    d.documents.insert(user_key("bystate"), completed_state());

    let scan = run_scan(&d, &nobody_local);
    assert_eq!(scan.owner_id, USER_ID);
    assert_eq!(scan.owner_key, owner_key(REGION, USER_ID));
    let onboarding = |n: &str| find(&scan.environments, n).onboarding.clone();
    assert_eq!(onboarding("bystate"), Some(OnboardingState::Joined { ec2: None }));
    assert_eq!(onboarding("byrole"), Some(OnboardingState::Joined { ec2: None }));
    assert_eq!(onboarding("denied"), Some(OnboardingState::NotJoined));
    assert_eq!(onboarding("absent"), Some(OnboardingState::NotJoined));
    assert!(matches!(
        onboarding("joining"),
        Some(OnboardingState::InProgress { lock: Some(_) })
    ));

    // The state key answered for `bystate` and the lock for `joining`, so
    // only the others cost a GetRole.
    let roles: Vec<String> = d
        .calls()
        .into_iter()
        .filter(|c| c.starts_with("role "))
        .collect();
    assert!(!roles.contains(&format!("role {}", task_role("bystate"))));
    assert!(!roles.contains(&format!("role {}", task_role("joining"))));
    assert!(roles.contains(&format!("role {}", task_role("byrole"))));
    assert_eq!(roles.len(), 3, "{roles:?}");

    // A local onboarding run is InProgress; a stale onboarding lock is
    // Incomplete.
    let mut d = FakeDetect::new().with_record("x", 1).with_record("y", 1);
    d.objects = Some(StateObjects {
        states: vec![env_key("x"), env_key("y")],
        locks: vec![(
            format!("{}.tflock", user_key("y")),
            lock_json("me"),
            3 * 3600,
        )],
    });
    let ok = owner_key(REGION, USER_ID);
    let local_x = move |dep: Deployment| {
        if dep
            == (Deployment::User {
                env: "x",
                owner_key: &ok,
            })
        {
            running()
        } else {
            LocalRun::default()
        }
    };
    let scan = run_scan(&d, &local_x);
    let x = find(&scan.environments, "x");
    assert_eq!(x.onboarding, Some(OnboardingState::InProgress { lock: None }));
    assert_eq!(x.onboarding_actions, vec![EnvironmentAction::Follow]);
    let y = find(&scan.environments, "y");
    let Some(OnboardingState::Incomplete { lock: Some(lock), ec2: None }) = &y.onboarding else {
        panic!("{y:#?}");
    };
    assert!(!lock.local_process);
    assert_eq!(y.onboarding_actions, vec![EnvironmentAction::Unlock]);
    // Join stays: it is the retry.
    assert!(y.actions.contains(&EnvironmentAction::Join));

    // Not joinable, not resolved.
    let mut d = FakeDetect::new();
    d.objects = Some(StateObjects {
        states: vec![env_key("orphan")],
        locks: Vec::new(),
    });
    let scan = run_scan(&d, &nobody_local);
    assert_eq!(find(&scan.environments, "orphan").onboarding, None);
}

#[test]
fn denied_lookups_are_unknown_not_missing() {
    // A workspace user: HeadBucket and GetPolicy are denied.
    let mut d = FakeDetect::new().with_record("shared", 1);
    d.bucket = Knowable::Unknown;
    d.boundary = Knowable::Unknown;
    d.roles.insert(task_role("shared"), Knowable::Unknown);

    let scan = run_scan(&d, &nobody_local);
    assert_eq!(scan.state_bucket, Knowable::Unknown);
    assert_eq!(scan.boundary, Knowable::Unknown);
    assert_eq!(scan.account, ACCOUNT);
    assert_eq!(scan.region, REGION);
    let shared = find(&scan.environments, "shared");
    assert_eq!(shared.state, EnvironmentState::ExternallyManaged);
    assert_eq!(shared.onboarding, Some(OnboardingState::NotJoined));
    // An unknown bucket isn't listed.
    assert!(
        !d.calls().iter().any(|c| c.starts_with("objects ")),
        "{:?}",
        d.calls()
    );
    assert!(d
        .calls()
        .contains(&format!("bucket {}", state_bucket_name(ACCOUNT, REGION))));

    // A missing bucket is No, and isn't listed either.
    let mut d = FakeDetect::new().with_record("shared", 1);
    d.bucket = Knowable::No;
    d.boundary = Knowable::No;
    let scan = run_scan(&d, &nobody_local);
    assert_eq!(scan.state_bucket, Knowable::No);
    assert_eq!(scan.boundary, Knowable::No);

    // The SDK side: what each answer means.
    assert_eq!(
        lookup_outcome(Some("AccessDenied"), Some(403)),
        Lookup::Denied
    );
    assert_eq!(lookup_outcome(None, Some(403)), Lookup::Denied);
    assert_eq!(
        lookup_outcome(Some("AccessDeniedException"), Some(400)),
        Lookup::Denied
    );
    assert_eq!(lookup_outcome(Some("NotFound"), Some(404)), Lookup::Missing);
    assert_eq!(lookup_outcome(None, Some(404)), Lookup::Missing);
    assert_eq!(
        lookup_outcome(Some("NoSuchEntity"), Some(404)),
        Lookup::Missing
    );
    assert_eq!(
        lookup_outcome(Some("NoSuchBucket"), Some(404)),
        Lookup::Missing
    );
    assert_eq!(lookup_outcome(Some("Throttling"), Some(400)), Lookup::Error);
    // A 403 that is a credential failure is an error, never Unknown.
    for code in [
        "InvalidClientTokenId",
        "SignatureDoesNotMatch",
        "InvalidAccessKeyId",
        "ExpiredToken",
    ] {
        assert_eq!(
            lookup_outcome(Some(code), Some(403)),
            Lookup::Error,
            "{code}"
        );
    }
    assert_eq!(lookup_outcome(None, None), Lookup::Error);
}

#[test]
fn records_under_unexpected_paths_are_skipped() {
    let d = FakeDetect {
        records: vec![
            ("/triple-c/environments/Bad Name".into(), "{}".into()),
            ("/triple-c/environments/a/b".into(), "{}".into()),
            ("/elsewhere/dev".into(), "{}".into()),
            (
                environment_parameter_name("ok"),
                record("ok", 1).to_string(),
            ),
        ],
        ..FakeDetect::new()
    };
    let scan = run_scan(&d, &nobody_local);
    let names: Vec<&str> = scan.environments.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(names, vec!["ok"]);
}

#[test]
fn only_buckets_owned_by_you_count_as_created() {
    let bucket = state_bucket_name(ACCOUNT, REGION);
    assert_eq!(
        create_bucket_outcome(Some("BucketAlreadyOwnedByYou"), &bucket),
        Some(Ok(()))
    );
    let taken = create_bucket_outcome(Some("BucketAlreadyExists"), &bucket);
    assert_eq!(
        taken,
        Some(Err(AwsError::StateBucketTaken {
            bucket: bucket.clone()
        }))
    );
    assert!(taken.unwrap().unwrap_err().to_string().contains(&bucket));
    // Anything else is classified as an ordinary SDK error.
    assert_eq!(create_bucket_outcome(Some("AccessDenied"), &bucket), None);
    assert_eq!(create_bucket_outcome(None, &bucket), None);
}

#[test]
fn a_bucket_of_another_account_after_create_is_taken() {
    let bucket = state_bucket_name(ACCOUNT, REGION);
    assert_eq!(
        head_after_create_error(Some(403), &bucket),
        Some(AwsError::StateBucketTaken {
            bucket: bucket.clone()
        })
    );
    assert_eq!(head_after_create_error(Some(500), &bucket), None);
    assert_eq!(head_after_create_error(None, &bucket), None);
}

#[test]
fn only_a_missing_component_tag_is_put_and_other_tags_are_kept() {
    let t = |k: &str, v: &str| (k.to_string(), v.to_string());
    assert_eq!(
        tags_with_state(&[t("team", "infra"), t("triple-c:component", "state")]),
        None
    );
    assert_eq!(
        tags_with_state(&[]),
        Some(vec![t("triple-c:component", "state")])
    );
    assert_eq!(
        tags_with_state(&[t("team", "infra"), t("cost", "42")]),
        Some(vec![
            t("team", "infra"),
            t("cost", "42"),
            t("triple-c:component", "state")
        ])
    );
    // A different component value is replaced, not duplicated.
    assert_eq!(
        tags_with_state(&[t("triple-c:component", "other"), t("team", "infra")]),
        Some(vec![t("team", "infra"), t("triple-c:component", "state")])
    );
}

#[test]
fn hostile_state_keys_name_no_environment() {
    for key in [
        "environments/a/b/terraform.tfstate",
        "environments/Bad/terraform.tfstate",
        "environments/a/terraform.tfstate.tflock.x",
        "other/a/terraform.tfstate",
        "environments//terraform.tfstate",
        "environments/../terraform.tfstate",
        "environments/a/terraform.tfstate.tflock.tflock",
    ] {
        assert_eq!(environment_of_key(key), None, "{key}");
    }
    assert_eq!(
        environment_of_key("environments/dev/terraform.tfstate"),
        Some("dev")
    );
    assert_eq!(
        environment_of_key("environments/dev/terraform.tfstate.tflock"),
        Some("dev")
    );
}

#[test]
fn the_state_bucket_is_configured_as_specified() {
    // No LocationConstraint in us-east-1; the region everywhere else. The
    // component tag either way.
    let east = create_bucket_configuration("us-east-1");
    assert!(east.location_constraint().is_none());
    let west = create_bucket_configuration(REGION);
    assert_eq!(west.location_constraint().map(|c| c.as_str()), Some(REGION));
    for cfg in [&east, &west] {
        let tags: Vec<(&str, &str)> = cfg.tags().iter().map(|t| (t.key(), t.value())).collect();
        assert_eq!(tags, vec![("triple-c:component", "state")]);
    }

    let bucket = state_bucket_name(ACCOUNT, REGION);
    let policy: Value = serde_json::from_str(&state_bucket_policy(&bucket)).unwrap();
    let st = &policy["Statement"][0];
    assert_eq!(st["Effect"], "Deny");
    assert_eq!(st["Principal"], "*");
    assert_eq!(st["Action"], "s3:*");
    assert_eq!(
        st["Resource"],
        json!([
            format!("arn:aws:s3:::{bucket}"),
            format!("arn:aws:s3:::{bucket}/*")
        ])
    );
    assert_eq!(st["Condition"]["Bool"]["aws:SecureTransport"], "false");
}

#[test]
fn the_scan_serializes_for_the_frontend() {
    let d = FakeDetect::new().with_record("dev", 1);
    let scan = run_scan(&d, &nobody_local);
    let v = serde_json::to_value(&scan).unwrap();
    assert_eq!(v["state_bucket"], "yes");
    assert_eq!(v["environments"][0]["state"]["state"], "externally_managed");
    assert_eq!(v["environments"][0]["actions"], json!(["join"]));
    assert_eq!(v["environments"][0]["onboarding"]["state"], "not_joined");
}

#[test]
fn only_a_state_that_manages_nothing_is_empty() {
    // What `tofu destroy` leaves behind (OpenTofu 1.13).
    let destroyed = r#"{"version":4,"terraform_version":"1.13.9","serial":31,"lineage":"0b6e3a0e-6f0c-4c35-a0f3-1d0f3f6c9a11","outputs":{},"resources":[],"check_results":null}"#;
    assert!(is_empty_state(destroyed));
    let live = r#"{"version":4,"serial":3,"outputs":{},"resources":[{"mode":"managed","type":"aws_iam_policy","name":"boundary","instances":[]}]}"#;
    assert!(!is_empty_state(live));
    // Unreadable or odd contents are a deployment, not nothing.
    for odd in ["", "not json", "{}", r#"{"resources":null}"#, r#"{"resources":{}}"#, "[]"] {
        assert!(!is_empty_state(odd), "{odd:?}");
    }
}

#[test]
fn only_small_states_are_read_and_only_up_to_the_cap() {
    assert!(may_be_empty_state(0, 0));
    assert!(may_be_empty_state(453, 0));
    assert!(may_be_empty_state(EMPTY_STATE_MAX_BYTES, MAX_STATE_PEEKS - 1));
    assert!(!may_be_empty_state(EMPTY_STATE_MAX_BYTES + 1, 0));
    assert!(!may_be_empty_state(-1, 0));
    assert!(!may_be_empty_state(100, MAX_STATE_PEEKS));
}

#[test]
fn a_state_that_cannot_be_read_still_counts() {
    let empty = r#"{"version":4,"outputs":{},"resources":[],"check_results":[{"object_kind":"resource","config_addr":"terraform_data.b","status":"unknown","objects":null}]}"#;
    assert!(!keep_state("k", Ok(Some(empty.into()))));
    // Deleted since the listing.
    assert!(!keep_state("k", Ok(None)));
    // Denied reads arrive as Some("") from lock_contents.
    assert!(keep_state("k", Ok(Some(String::new()))));
    assert!(keep_state("k", Ok(Some(r#"{"resources":[{"type":"x"}]}"#.into()))));
    // An S3 error on one object degrades that entry, not the scan.
    assert!(keep_state("k", Err(AwsError::Other { message: "boom".into() })));
}

fn completed_state() -> String {
    json!({
        "version": 4,
        "resources": [{"type": "aws_iam_role", "name": "task"}],
        "outputs": {"onboarding_complete": {"value": true, "type": "bool"}},
    })
    .to_string()
}

#[test]
fn a_failed_onboarding_is_incomplete_not_joined() {
    // A partial state (the task role made, the file system not): the role
    // exists, but the state has no completion output.
    let mut d = FakeDetect::new()
        .with_record("partial", 1)
        .with_record("done", 1)
        .with_record("failedhere", 1)
        .with_record("errored", 1);
    let partial = json!({
        "version": 4,
        "resources": [{"type": "aws_iam_role", "name": "task"}],
        "outputs": {"task_role_arn": {"value": "arn:aws:iam::1:role/x", "type": "string"}},
    })
    .to_string();
    d.objects = Some(StateObjects {
        states: ["partial", "done", "failedhere", "errored"]
            .iter()
            .flat_map(|e| [env_key(e), user_key(e)])
            .collect(),
        locks: Vec::new(),
    });
    d.documents.insert(user_key("partial"), partial);
    d.documents.insert(user_key("done"), completed_state());
    d.documents.insert(user_key("failedhere"), completed_state());
    d.documents.insert(user_key("errored"), completed_state());
    for e in ["partial", "done", "failedhere", "errored"] {
        d.roles.insert(task_role(e), Knowable::Yes);
    }
    let ok = owner_key(REGION, USER_ID);
    let local = move |dep: Deployment| match dep {
        Deployment::User { env: "failedhere", owner_key } if owner_key == ok => LocalRun {
            failed: true,
            ..LocalRun::default()
        },
        Deployment::User { env: "errored", owner_key } if owner_key == ok => LocalRun {
            failed: true,
            errored_state: true,
            ..LocalRun::default()
        },
        _ => LocalRun::default(),
    };
    let scan = run_scan(&d, &local);
    let row = |n: &str| find(&scan.environments, n).clone();

    assert_eq!(row("done").onboarding, Some(OnboardingState::Joined { ec2: None }));
    for name in ["partial", "failedhere", "errored"] {
        let r = row(name);
        assert_eq!(
            r.onboarding,
            Some(OnboardingState::Incomplete { lock: None, ec2: None }),
            "{name}"
        );
        // Join is kept: an idempotent re-apply finishes it.
        assert!(r.actions.contains(&EnvironmentAction::Join), "{name}");
    }
    assert!(row("partial").onboarding_actions.is_empty());
    // The saved state has to go back first.
    assert_eq!(
        row("errored").onboarding_actions,
        vec![EnvironmentAction::PushState]
    );
    // Neither the local failures nor the lock-free ones cost a GetRole.
    assert!(!d.calls().iter().any(|c| c.starts_with("role ")), "{:?}", d.calls());
    assert!(onboarding_complete(&completed_state()));
    assert!(!onboarding_complete("{}"));
    assert!(!onboarding_complete(
        &json!({"outputs": {"onboarding_complete": {"value": "true"}}}).to_string()
    ));
}

#[test]
fn an_errored_environment_state_offers_only_the_push() {
    let objs = objects(&[env_key("e")], &[]);
    let errored = |d: Deployment| LocalRun {
        failed: true,
        errored_state: d == Deployment::Environment("e"),
        ..LocalRun::default()
    };
    let envs = merge(Vec::new(), Some(&objs), &errored, 1);
    // State without a record: normally Repair and Destroy.
    assert_eq!(find(&envs, "e").actions, vec![EnvironmentAction::PushState]);
}

#[test]
fn onboarded_users_are_counted_per_environment() {
    let ok = owner_key(REGION, USER_ID);
    let objs = objects(
        &[
            env_key("a"),
            env_key("b"),
            state_key(Deployment::User { env: "a", owner_key: &ok }),
            state_key(Deployment::User { env: "a", owner_key: "0123456789ab" }),
            "users/a/x/y/terraform.tfstate".to_string(),
        ],
        &[],
    );
    let envs = merge(Vec::new(), Some(&objs), &nobody_local, 1);
    assert_eq!(find(&envs, "a").onboarded_users, Some(2));
    assert_eq!(find(&envs, "b").onboarded_users, Some(0));
    let envs = merge(vec![("c".into(), Ok(parsed("c", 1)))], None, &nobody_local, 1);
    assert_eq!(find(&envs, "c").onboarded_users, None);
}

#[test]
fn the_tls_statement_is_added_to_a_bucket_policy_never_replacing_it() {
    let bucket = state_bucket_name(ACCOUNT, REGION);
    let ours: Value = serde_json::from_str(&state_bucket_policy(&bucket)).unwrap();
    let tls = ours["Statement"][0].clone();

    // No policy: ours.
    let new: Value =
        serde_json::from_str(&state_bucket_policy_update(None, &bucket).unwrap().unwrap()).unwrap();
    assert_eq!(new, ours);

    // Ours already there (statement order or key order aside): nothing to put.
    let same = json!({"Statement": [tls.clone()], "Version": "2012-10-17"}).to_string();
    assert_eq!(state_bucket_policy_update(Some(&same), &bucket).unwrap(), None);

    // An organisation's statements are kept; ours is added once.
    let org = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Sid": "OrgDenyOutsideVpce",
            "Effect": "Deny",
            "Principal": "*",
            "Action": "s3:*",
            "Resource": format!("arn:aws:s3:::{bucket}/*"),
            "Condition": {"StringNotEquals": {"aws:SourceVpce": "vpce-1234"}},
        }],
    });
    let merged: Value = serde_json::from_str(
        &state_bucket_policy_update(Some(&org.to_string()), &bucket)
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    let st = merged["Statement"].as_array().unwrap();
    assert_eq!(st.len(), 2);
    assert_eq!(st[0], org["Statement"][0]);
    assert_eq!(st[1], tls);
    assert_eq!(merged["Version"], "2012-10-17");

    // A single statement object (not a list) is kept too.
    let single = json!({"Version": "2012-10-17", "Statement": org["Statement"][0].clone()});
    let merged: Value = serde_json::from_str(
        &state_bucket_policy_update(Some(&single.to_string()), &bucket)
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(merged["Statement"].as_array().unwrap().len(), 2);

    // A stale statement under our Sid is replaced, not duplicated.
    let mut old = tls.clone();
    old["Resource"] = json!("arn:aws:s3:::elsewhere");
    let stale = json!({"Version": "2012-10-17", "Statement": [org["Statement"][0].clone(), old]});
    let merged: Value = serde_json::from_str(
        &state_bucket_policy_update(Some(&stale.to_string()), &bucket)
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    let st = merged["Statement"].as_array().unwrap();
    assert_eq!(st.len(), 2);
    assert_eq!(st[1], tls);

    // Something that isn't a policy is never overwritten.
    assert!(state_bucket_policy_update(Some("not json"), &bucket).is_err());
}

fn user_record(env: &str, tag: &str) -> Value {
    let stem = crate::aws::tofu_modules::user_ec2_stem(env, tag);
    json!({
        "schema": 1,
        "instance_type": "m7g.large",
        "cluster_arn": format!("arn:aws:ecs:{REGION}:{ACCOUNT}:cluster/{stem}"),
        "capacity_provider": stem,
        "user_tag": tag,
        "max_instances": 3,
    })
}

#[test]
fn a_user_record_reads_as_the_users_compute() {
    let rec = user_record("dev", "alice");
    let c = parse_user_record("dev", &rec.to_string(), ACCOUNT, REGION).unwrap();
    assert_eq!(
        c,
        UserCompute {
            instance_type: "m7g.large".into(),
            max_instances: 3,
            user_tag: "alice".into(),
            cluster_arn: format!("arn:aws:ecs:{REGION}:{ACCOUNT}:cluster/triple-c-dev--u-alice"),
            capacity_provider: "triple-c-dev--u-alice".into(),
        }
    );
    // A record written before max_instances existed: the module's default.
    let mut old = rec.clone();
    old.as_object_mut().unwrap().remove("max_instances");
    let c = parse_user_record("dev", &old.to_string(), ACCOUNT, REGION).unwrap();
    assert_eq!(c.max_instances, 1);
}

#[test]
fn a_user_record_is_untrusted() {
    let bad = |field: &str, value: Value, why: &str| {
        let mut rec = user_record("dev", "alice");
        rec[field] = value;
        let err = parse_user_record("dev", &rec.to_string(), ACCOUNT, REGION).unwrap_err();
        assert!(err.contains(why), "{field}: {err}");
        assert!(!err.contains("evil"), "{err}");
    };
    bad("schema", json!(2), "schema");
    bad("instance_type", json!("p5.48xlarge"), "instance_type");
    bad("instance_type", json!(7), "instance_type");
    bad("max_instances", json!(0), "max_instances");
    bad("max_instances", json!(5), "max_instances");
    bad("max_instances", json!("2"), "max_instances");
    bad("user_tag", json!("evil--tag"), "user_tag");
    // Another user's cluster, or one in another account or region.
    bad(
        "cluster_arn",
        json!(format!("arn:aws:ecs:{REGION}:{ACCOUNT}:cluster/triple-c-dev--u-bob")),
        "cluster_arn",
    );
    bad(
        "cluster_arn",
        json!(format!("arn:aws:ecs:us-east-1:{ACCOUNT}:cluster/triple-c-dev--u-alice")),
        "cluster_arn",
    );
    bad(
        "cluster_arn",
        json!(format!("arn:aws:ecs:{REGION}:999999999999:cluster/triple-c-dev--u-alice")),
        "cluster_arn",
    );
    bad("capacity_provider", json!("evil"), "capacity_provider");
    // Another environment's record.
    let rec = user_record("prod", "alice").to_string();
    assert!(parse_user_record("dev", &rec, ACCOUNT, REGION).is_err());
    for text in ["", "not json", "[]", &"x".repeat(5000)] {
        assert!(parse_user_record("dev", text, ACCOUNT, REGION).is_err());
    }
}

fn compute_of(s: &OnboardingState) -> Option<&UserCompute> {
    match s {
        OnboardingState::Joined { ec2 } | OnboardingState::Incomplete { ec2, .. } => ec2.as_ref(),
        _ => None,
    }
}

#[test]
fn onboarding_carries_the_users_compute_from_their_record() {
    use crate::aws::tofu_modules::user_record_parameter_name;
    let ok = owner_key(REGION, USER_ID);
    let mut d = FakeDetect::new()
        .with_record("joined", 1)
        .with_record("fargate", 1)
        .with_record("broken", 1)
        .with_record("partial", 1)
        .with_record("absent", 1);
    d.roles.insert(task_role("joined"), Knowable::Yes);
    d.roles.insert(task_role("fargate"), Knowable::Yes);
    d.roles.insert(task_role("broken"), Knowable::Yes);
    d.objects = Some(StateObjects {
        states: vec![
            env_key("joined"),
            env_key("fargate"),
            env_key("broken"),
            env_key("partial"),
            env_key("absent"),
            user_key("partial"),
        ],
        locks: vec![],
    });
    d.documents.insert(user_key("partial"), "{\"resources\":[{}]}".into());
    for env in ["joined", "partial", "absent"] {
        d.parameters.insert(
            user_record_parameter_name(env, &ok),
            user_record(env, "alice").to_string(),
        );
    }
    d.parameters.insert(
        user_record_parameter_name("broken", &ok),
        user_record("other", "alice").to_string(),
    );
    // `joined`'s state is not readable here: found by role.
    d.objects.as_mut().unwrap().states.retain(|k| *k != user_key("joined"));

    let scan = run_scan(&d, &nobody_local);
    let onboarding = |n: &str| find(&scan.environments, n).onboarding.clone().unwrap();
    let joined = onboarding("joined");
    let compute = compute_of(&joined).expect("joined has compute");
    assert_eq!(compute.instance_type, "m7g.large");
    assert_eq!(compute.max_instances, 3);
    assert_eq!(compute.user_tag, "alice");
    assert_eq!(onboarding("fargate"), OnboardingState::Joined { ec2: None });
    // A record that doesn't validate is no compute.
    assert_eq!(onboarding("broken"), OnboardingState::Joined { ec2: None });
    // An incomplete onboarding keeps what its record still says.
    let partial = onboarding("partial");
    assert!(matches!(partial, OnboardingState::Incomplete { .. }), "{partial:?}");
    assert_eq!(compute_of(&partial).map(|c| c.max_instances), Some(3));
    // Not joined: the record isn't even read.
    assert_eq!(onboarding("absent"), OnboardingState::NotJoined);
    assert!(!d
        .calls()
        .contains(&format!("parameter {}", user_record_parameter_name("absent", &ok))));

    // Serialized for the UI: the state tag plus `ec2`.
    let v = serde_json::to_value(&joined).unwrap();
    assert_eq!(v["state"], "joined");
    assert_eq!(v["ec2"]["capacity_provider"], "triple-c-joined--u-alice");
    assert_eq!(
        serde_json::to_value(OnboardingState::Joined { ec2: None }).unwrap(),
        json!({"state": "joined", "ec2": null})
    );
}

#[test]
fn a_user_record_read_is_absent_valid_or_unusable() {
    let ok = user_record("dev", "alice").to_string();
    assert_eq!(user_record_from(Param::Missing, "dev", ACCOUNT, REGION), UserRecord::Absent);
    assert!(matches!(
        user_record_from(Param::Value(ok), "dev", ACCOUNT, REGION),
        UserRecord::Valid(_)
    ));
    // Denied or invalid: there may be capacity, so it is not "absent".
    assert!(matches!(
        user_record_from(Param::Denied, "dev", ACCOUNT, REGION),
        UserRecord::Unusable(_)
    ));
    assert!(matches!(
        user_record_from(Param::Value("{}".into()), "dev", ACCOUNT, REGION),
        UserRecord::Unusable(_)
    ));
}
