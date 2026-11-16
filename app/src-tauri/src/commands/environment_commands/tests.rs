use std::fs;
use std::time::Duration;

use serde_json::json;

use super::*;
use crate::aws::tofu::Workdir;
use crate::aws::environments::{
    EnvironmentState, Knowable, LockInfo, NamedResource, OnboardingState, Param, UserCompute,
};
use crate::aws::tofu_modules::{
    environment_vars, module_files, onboarding_vars, owner_key, state_key, ModuleKind,
};

const ACCOUNT: &str = "123456789012";
const REGION: &str = "eu-west-1";
const USER_ID: &str = "AROAEXAMPLEID12345678:alice";

fn info(name: &str, mode: &str, assign_public_ip: bool) -> EnvironmentInfo {
    EnvironmentInfo {
        schema: 1,
        name: name.into(),
        template_version: 1,
        cluster_arn: format!("arn:aws:ecs:{REGION}:{ACCOUNT}:cluster/triple-c-{name}"),
        security_group_id: "sg-0123456789abcdef0".into(),
        subnet_ids: vec![
            "subnet-0123456789abcdef0".into(),
            "subnet-0fedcba9876543210".into(),
        ],
        assign_public_ip,
        kms_key_arn: format!("arn:aws:kms:{REGION}:{ACCOUNT}:key/1234abcd-12ab"),
        log_group: format!("/triple-c/{name}"),
        network_mode: mode.into(),
        tenancy: "strict".into(),
        max_workspace_hours: 0,
    }
}

fn env(name: &str, state: EnvironmentState, actions: Vec<EnvironmentAction>) -> DetectedEnvironment {
    DetectedEnvironment {
        name: name.into(),
        template_version: None,
        state,
        actions,
        info: None,
        onboarding: None,
        onboarding_actions: Vec::new(),
        onboarded_users: Some(0),
    }
}

fn scan(boundary: Knowable, environments: Vec<DetectedEnvironment>) -> EnvironmentScan {
    EnvironmentScan {
        region: REGION.into(),
        account: ACCOUNT.into(),
        owner_id: USER_ID.into(),
        owner_key: owner_key(REGION, USER_ID),
        state_bucket: Knowable::Yes,
        boundary,
        environments,
    }
}

fn lock(age_secs: u64, local_process: bool) -> LockInfo {
    LockInfo {
        who: "someone@host".into(),
        created: String::new(),
        operation: "OperationTypeApply".into(),
        age_secs,
        local_process,
    }
}

#[test]
fn only_our_state_keys_are_accepted() {
    let ok = owner_key(REGION, USER_ID);
    let good = [
        (
            "environments/dev/terraform.tfstate".to_string(),
            DeploymentKey::Environment("dev".into()),
        ),
        (
            format!("users/dev/{ok}/terraform.tfstate"),
            DeploymentKey::User { env: "dev".into(), owner_key: ok.clone() },
        ),
        ("boundary/terraform.tfstate".to_string(), DeploymentKey::Boundary),
    ];
    for (key, want) in good {
        let parsed = parse_state_key(&key).unwrap();
        assert_eq!(parsed, want);
        assert_eq!(state_key(parsed.deployment()), key, "round trip");
    }
    let bad = [
        "",
        "environments/dev/terraform.tfstate.tflock",
        "environments/Dev/terraform.tfstate",
        "environments/dev-/terraform.tfstate",
        "environments/../terraform.tfstate",
        "environments/dev/../../boundary/terraform.tfstate",
        "/environments/dev/terraform.tfstate",
        "environments//terraform.tfstate",
        "environments/dev/x/terraform.tfstate",
        "environments/dev/terraform.tfstate/",
        "users/dev/terraform.tfstate",
        "users/dev/ABCDEF012345/terraform.tfstate",
        "users/dev/abcdef01234/terraform.tfstate",
        "users/dev/abcdef0123456/terraform.tfstate",
        "users/dev/abcdef01234g/terraform.tfstate",
        "boundary/x/terraform.tfstate",
        "boundary/terraform.tfstate ",
        "something/else",
        "environments\\dev\\terraform.tfstate",
    ];
    for key in bad {
        assert!(parse_state_key(key).is_err(), "{key:?} must be refused");
    }
}

#[test]
fn deploy_refuses_an_existing_name_and_a_missing_boundary() {
    use EnvironmentAction::*;
    let existing = vec![
        env("dev", EnvironmentState::Ready, vec![Join]),
        env("half", EnvironmentState::Incomplete { lock: None }, vec![Repair, Destroy]),
    ];
    let s = scan(Knowable::Yes, existing.clone());
    assert!(deploy_refusal(&s, "prod").is_ok());
    let e = deploy_refusal(&s, "dev").unwrap_err();
    assert!(e.contains("already exists"), "{e}");
    assert!(deploy_refusal(&s, "half").is_err(), "state without a record is taken too");
    assert!(deploy_refusal(&s, "Not Valid").is_err());

    let no_boundary = scan(Knowable::No, existing.clone());
    let e = deploy_refusal(&no_boundary, "prod").unwrap_err();
    assert!(e.contains("permissions boundary"), "{e}");
    // A caller who can't tell is let through (the apply itself would fail).
    assert!(deploy_refusal(&scan(Knowable::Unknown, existing), "prod").is_ok());
}

#[test]
fn update_reuses_the_recorded_network() {
    assert_eq!(
        network_from_record(&info("dev", "public", true), None).unwrap(),
        NetworkChoice::Public
    );
    assert_eq!(
        network_from_record(&info("dev", "private-nat", false), None).unwrap(),
        NetworkChoice::PrivateNat
    );
    let existing = info("dev", "existing", false);
    assert_eq!(
        network_from_record(&existing, Some("vpc-0123456789abcdef0")).unwrap(),
        NetworkChoice::Existing {
            vpc_id: "vpc-0123456789abcdef0".into(),
            subnet_ids: existing.subnet_ids.clone(),
            assign_public_ip: false,
        }
    );
    // The VPC isn't in the record: without it from the state, refuse rather
    // than guess.
    assert!(network_from_record(&existing, None).is_err());
    assert!(network_from_record(&existing, Some("not-a-vpc")).is_err());
    assert!(network_from_record(&info("dev", "carrier-pigeon", false), None).is_err());
    // And it round-trips into the module's variables unchanged.
    let vars = environment_vars(
        "dev",
        REGION,
        &network_from_record(&existing, Some("vpc-0123456789abcdef0")).unwrap(),
        0,
    )
    .unwrap();
    assert_eq!(vars["network_mode"], "existing");
    assert_eq!(vars["subnet_ids"], json!(existing.subnet_ids));
}

#[test]
fn the_vpc_is_read_from_the_state_outputs() {
    let state = json!({
        "version": 4,
        "outputs": { "vpc_id": { "value": "vpc-0123456789abcdef0", "type": "string" } },
    })
    .to_string();
    assert_eq!(
        vpc_id_from_state(state.as_bytes()).as_deref(),
        Some("vpc-0123456789abcdef0")
    );
    assert_eq!(vpc_id_from_state(b"{}"), None);
    assert_eq!(vpc_id_from_state(b"not json"), None);
    let hostile = json!({"outputs": {"vpc_id": {"value": "vpc-1\"; rm -rf /"}}}).to_string();
    assert_eq!(vpc_id_from_state(hostile.as_bytes()), None);
}

#[test]
fn bundles_contain_the_module_backend_vars_and_readme() {
    let dir = tempfile::tempdir().unwrap();
    let ok = owner_key(REGION, USER_ID);
    let d = Deployment::User { env: "dev", owner_key: &ok };
    let vars = onboarding_vars("dev", REGION, USER_ID, None).unwrap();
    let written = write_bundle(dir.path(), d, ACCOUNT, REGION, &vars).unwrap();

    for (name, text) in module_files(ModuleKind::UserOnboard) {
        assert_eq!(&fs::read_to_string(dir.path().join(name)).unwrap(), text);
        assert!(written.contains(&name.to_string()));
    }
    let bucket = format!("triple-c-tfstate-{ACCOUNT}-{REGION}");
    let key = state_key(d);
    let backend = fs::read_to_string(dir.path().join("backend.hcl")).unwrap();
    assert!(backend.contains(&format!("bucket = \"{bucket}\"")), "{backend}");
    assert!(backend.contains(&format!("key = \"{key}\"")), "{backend}");
    assert!(backend.contains("use_lockfile = true"), "{backend}");
    let tfvars: Value =
        serde_json::from_str(&fs::read_to_string(dir.path().join("terraform.tfvars.json")).unwrap())
            .unwrap();
    assert_eq!(tfvars, vars);

    let readme = fs::read_to_string(dir.path().join("README.txt")).unwrap();
    for needle in [
        bucket.as_str(),
        key.as_str(),
        ACCOUNT,
        REGION,
        "tofu init -backend-config=backend.hcl",
        "tofu apply",
        "terraform providers lock",
        "registry.opentofu.org",
        "1.10",
        "aws s3api create-bucket",
    ] {
        assert!(readme.contains(needle), "README lacks {needle:?}:\n{readme}");
    }
    // Outside us-east-1 the bucket needs a location constraint.
    assert!(readme.contains(&format!("\"LocationConstraint\":\"{REGION}\"")), "{readme}");
    // The admin policy allows s3:CreateBucket only with the component tag
    // in the request, so the tag goes on the create itself; and the bucket
    // is configured as the app configures it, TLS-only policy included.
    let create = readme
        .lines()
        .find(|l| l.contains("aws s3api create-bucket"))
        .unwrap();
    assert!(
        create.contains(r#""Tags":[{"Key":"triple-c:component","Value":"state"}]"#),
        "{create}"
    );
    for needle in [
        "put-bucket-versioning",
        "put-bucket-encryption",
        "put-public-access-block",
        "put-bucket-policy",
        "aws:SecureTransport",
        "DenyInsecureTransport",
    ] {
        assert!(readme.contains(needle), "README lacks {needle:?}:\n{readme}");
    }
    assert!(!readme.contains("put-bucket-tagging"), "a later tag can't satisfy the create");

    // The EC2 name-collision check (the app does it too, Task 4): the
    // exact per-user names, and this user's record to compare against.
    let record = format!("/triple-c/users/dev/{ok}");
    for needle in [
        "BEFORE tofu apply",
        &format!("aws ecs describe-clusters --region {REGION} --clusters triple-c-dev--u-<tag>"),
        &format!(
            "aws autoscaling describe-auto-scaling-groups --region {REGION} \
             --auto-scaling-group-names triple-c-dev--u-<tag>"
        ),
        &format!("aws ssm get-parameter --region {REGION} --name {record}"),
        "cluster/triple-c-dev--u-<tag>",
        "--include TAGS",
        "triple-c:owner",
        &format!("owner_id ({USER_ID})"),
    ] {
        assert!(readme.contains(needle), "README lacks {needle:?}:\n{readme}");
    }
    // With a tag in the variables, the commands name it.
    let mut tagged = vars.clone();
    tagged["ec2_instance_type"] = "m7i.large".into();
    tagged["ec2_user_tag"] = "alice".into();
    let other = tempfile::tempdir().unwrap();
    write_bundle(other.path(), d, ACCOUNT, REGION, &tagged).unwrap();
    let readme_tagged = fs::read_to_string(other.path().join("README.txt")).unwrap();
    assert!(readme_tagged.contains("--clusters triple-c-dev--u-alice "), "{readme_tagged}");
    assert!(readme_tagged.contains("--auto-scaling-group-names triple-c-dev--u-alice "));
    assert!(!readme_tagged.contains("<tag>"));
    // The check comes before the apply it guards.
    let check = readme.find("BEFORE tofu apply").unwrap();
    let apply = readme.find("    tofu apply").unwrap();
    assert!(check < apply, "{readme}");
    // Not in the other bundles.
    let env_dir = tempfile::tempdir().unwrap();
    let env_vars = environment_vars("dev", REGION, &NetworkChoice::Public, 0).unwrap();
    write_bundle(env_dir.path(), Deployment::Environment("dev"), ACCOUNT, REGION, &env_vars).unwrap();
    let env_readme = fs::read_to_string(env_dir.path().join("README.txt")).unwrap();
    assert!(!env_readme.contains("EC2 capacity"), "{env_readme}");

    // Writing again over our own files is fine...
    write_bundle(dir.path(), d, ACCOUNT, REGION, &vars).unwrap();
    // ...but an unrelated file (or folder) means the wrong folder.
    fs::write(dir.path().join("notes.txt"), "mine").unwrap();
    let e = write_bundle(dir.path(), d, ACCOUNT, REGION, &vars).unwrap_err();
    assert!(e.contains("notes.txt"), "{e}");
    assert_eq!(fs::read_to_string(dir.path().join("notes.txt")).unwrap(), "mine");
    fs::remove_file(dir.path().join("notes.txt")).unwrap();
    fs::create_dir(dir.path().join(".terraform")).unwrap();
    assert!(write_bundle(dir.path(), d, ACCOUNT, REGION, &vars).is_err());
}

#[test]
fn a_us_east_1_bundle_creates_its_bucket_without_a_location_constraint() {
    let dir = tempfile::tempdir().unwrap();
    let vars = environment_vars("dev", "us-east-1", &NetworkChoice::Public, 0).unwrap();
    write_bundle(dir.path(), Deployment::Environment("dev"), ACCOUNT, "us-east-1", &vars).unwrap();
    let readme = fs::read_to_string(dir.path().join("README.txt")).unwrap();
    assert!(readme.contains("aws s3api create-bucket"), "{readme}");
    assert!(!readme.contains("LocationConstraint"), "{readme}");
    assert!(readme.contains(r#""Tags":[{"Key":"triple-c:component","Value":"state"}]"#), "{readme}");
    assert!(readme.contains("tofu destroy"), "{readme}");
    for (name, _) in module_files(ModuleKind::Environment) {
        assert!(dir.path().join(name).is_file(), "{name}");
    }
}

#[test]
fn owner_ids_must_look_like_an_aws_user_id() {
    for ok in ["AIDAEXAMPLEUSERID1234", "AROAEXAMPLEID12345678:alice", "AROA1234567890ABCDEF:a.b@c_d-e+f=g,h"] {
        assert_eq!(validate_owner_id(ok).unwrap(), ok);
    }
    for bad in [
        "",
        "123456789012",
        "aidaexampleuserid1234",
        "AROAEXAMPLEID12345678:",
        "AROAEXAMPLEID12345678:a b",
        "AROAEXAMPLEID12345678:a:b",
        "AROAEXAMPLEID12345678:\"",
        &format!("AROAEXAMPLEID12345678:{}", "a".repeat(65)),
        &"A".repeat(129),
    ] {
        assert!(validate_owner_id(bad).is_err(), "{bad:?}");
    }
}

#[test]
fn server_side_rechecks_follow_the_detected_actions() {
    use EnvironmentAction::*;
    let s = scan(
        Knowable::Yes,
        vec![
            env("broken", EnvironmentState::Incomplete { lock: None }, vec![Repair, Destroy]),
            env(
                "working",
                EnvironmentState::Incomplete { lock: Some(lock(9000, false)) },
                vec![Repair, Unlock],
            ),
            env("ready", EnvironmentState::Ready, vec![Join]),
            env("busy", EnvironmentState::Deploying { lock: Some(lock(10, true)) }, vec![Follow]),
        ],
    );
    assert!(require_action(&s, "broken", Destroy).is_ok());
    assert!(require_action(&s, "working", Destroy).is_err(), "never destroy a working env (D6)");
    assert!(require_action(&s, "ready", Destroy).is_err());
    assert!(require_action(&s, "missing", Destroy).is_err());
    assert!(require_action(&s, "working", Repair).is_ok());
    assert!(require_action(&s, "ready", Update).is_err());

    assert!(unlock_refusal(&s, "working").is_ok());
    assert!(unlock_refusal(&s, "broken").is_err(), "no lock to remove");
    assert!(unlock_refusal(&s, "busy").is_err(), "a local runner holds it");
    let local_stale = scan(
        Knowable::Yes,
        vec![env(
            "x",
            EnvironmentState::Incomplete { lock: Some(lock(9000, true)) },
            vec![Unlock],
        )],
    );
    assert!(unlock_refusal(&local_stale, "x").is_err());
}

#[test]
fn onboarding_needs_a_joinable_environment_and_the_boundary() {
    use EnvironmentAction::*;
    let mut ready = env("dev", EnvironmentState::Ready, vec![Join]);
    ready.onboarding = Some(OnboardingState::NotJoined);
    let mut busy = env("busy", EnvironmentState::Ready, vec![Join]);
    busy.onboarding = Some(OnboardingState::InProgress { lock: None });
    let half = env("half", EnvironmentState::Incomplete { lock: None }, vec![Repair, Destroy]);
    let s = scan(Knowable::Yes, vec![ready.clone(), busy, half]);
    assert!(onboard_refusal(&s, "dev").is_ok());
    assert!(onboard_refusal(&s, "busy").is_err());
    assert!(onboard_refusal(&s, "half").is_err());
    assert!(onboard_refusal(&s, "nope").is_err());
    assert!(onboard_refusal(&scan(Knowable::No, vec![ready]), "dev").is_err());
}

#[test]
fn progress_events_have_the_documented_shape() {
    let p = DeployProgress {
        op_id: "op".into(),
        state_key: "boundary/terraform.tfstate".into(),
        events: vec![],
        done: false,
        cancellable: true,
        result: None,
    };
    let v = serde_json::to_value(&p).unwrap();
    assert_eq!(
        v,
        json!({"op_id": "op", "state_key": "boundary/terraform.tfstate", "events": [], "done": false, "cancellable": true})
    );
    let done = DeployProgress {
        done: true,
        result: Some(TofuOutcome { ok: false, exit_code: 1, summary: Some("boom".into()) }),
        ..p
    };
    let v = serde_json::to_value(&done).unwrap();
    assert_eq!(v["result"], json!({"ok": false, "exit_code": 1, "summary": "boom"}));
}

#[test]
fn an_already_running_start_names_the_deployment_and_suggests_follow() {
    let e = start_error(StartError::AlreadyRunning, "environment \"dev\"");
    let msg = e.to_string();
    assert!(msg.contains("already running for environment \"dev\""), "{msg}");
    assert!(msg.contains("Follow"), "{msg}");
    let inner = AwsError::KeysMissing;
    assert_eq!(start_error(StartError::Failed(inner.clone()), "x"), inner);
}

#[tokio::test]
async fn a_follower_emits_nothing_until_its_gate_opens() {
    use std::sync::{Arc, Mutex};
    // A finished run: the whole replay and `done` are available at once,
    // the case where events could otherwise beat the op id to the caller.
    let dir = tempfile::tempdir().unwrap();
    let lines: String = (0..450).map(|i| format!("line {i}\n")).collect();
    fs::write(dir.path().join("apply.log"), lines).unwrap();
    fs::write(dir.path().join("apply.exit"), "0\n").unwrap();
    let wd = Workdir { dir: dir.path().to_path_buf() };
    let base = DeployProgress {
        op_id: "op-1".into(),
        state_key: "boundary/terraform.tfstate".into(),
        events: vec![],
        done: false,
        cancellable: true,
        result: None,
    };
    let seen: Arc<Mutex<Vec<DeployProgress>>> = Arc::default();
    let sink = seen.clone();
    let (gate, rx) = Gate::new();
    let task = tokio::spawn(follow_and_report(
        rx,
        wd,
        base,
        Duration::from_millis(10),
        move |p| sink.lock().unwrap().push(p.clone()),
    ));
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert!(seen.lock().unwrap().is_empty(), "emitted before the gate opened");

    gate.open();
    task.await.unwrap();
    let seen = seen.lock().unwrap();
    let batches: Vec<usize> = seen.iter().filter(|p| !p.done).map(|p| p.events.len()).collect();
    assert_eq!(batches, vec![200, 200, 50], "the whole log, in capped batches");
    assert!(seen.iter().all(|p| p.op_id == "op-1" && p.state_key == "boundary/terraform.tfstate"));
    let last = seen.last().unwrap();
    assert!(last.done && !last.cancellable);
    assert_eq!(last.result, Some(TofuOutcome { ok: true, exit_code: 0, summary: None }));
    assert_eq!(seen.iter().filter(|p| p.done).count(), 1);
}

#[tokio::test]
async fn a_dropped_gate_still_releases_the_follower() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("apply.log"), "x\n").unwrap();
    fs::write(dir.path().join("apply.exit"), "1\n").unwrap();
    let (gate, rx) = Gate::new();
    drop(gate);
    let mut n = 0;
    follow_and_report(
        rx,
        Workdir { dir: dir.path().to_path_buf() },
        DeployProgress {
            op_id: "o".into(),
            state_key: "k".into(),
            events: vec![],
            done: false,
            cancellable: false,
            result: None,
        },
        Duration::from_millis(10),
        |_| n += 1,
    )
    .await;
    assert_eq!(n, 2);
}

#[test]
fn destroy_needs_the_action_no_record_and_no_lock() {
    use EnvironmentAction::*;
    let mut stale_with_record = env(
        "rec",
        EnvironmentState::Incomplete { lock: Some(lock(9000, false)) },
        vec![Repair, Destroy, Unlock],
    );
    // Stale lock + record + no state: detection offers Destroy; the guard
    // must still refuse (the record says it may be working).
    stale_with_record.info = Some(info("rec", "public", true));
    let mut record_unlocked = env("rec2", EnvironmentState::Incomplete { lock: None }, vec![Destroy]);
    record_unlocked.info = Some(info("rec2", "public", true));
    let s = scan(
        Knowable::Yes,
        vec![
            stale_with_record,
            record_unlocked,
            env("locked", EnvironmentState::Incomplete { lock: Some(lock(9000, false)) }, vec![Repair, Destroy, Unlock]),
            env("half", EnvironmentState::Incomplete { lock: None }, vec![Repair, Destroy]),
            env("ready", EnvironmentState::Ready, vec![Join]),
        ],
    );
    let e = destroy_refusal(&s, "rec").unwrap_err();
    assert!(e.contains("valid record"), "{e}");
    assert!(destroy_refusal(&s, "rec2").unwrap_err().contains("valid record"));
    assert!(destroy_refusal(&s, "locked").unwrap_err().contains("Unlock it first"));
    assert!(destroy_refusal(&s, "ready").is_err());
    assert!(destroy_refusal(&s, "missing").is_err());
    assert_eq!(destroy_refusal(&s, "half").unwrap().name, "half");
}

#[test]
fn destroy_is_refused_while_users_are_onboarded() {
    use EnvironmentAction::*;
    let mut onboarded = env("half", EnvironmentState::Incomplete { lock: None }, vec![Repair, Destroy]);
    onboarded.onboarded_users = Some(2);
    let mut unknown = env("blind", EnvironmentState::Incomplete { lock: None }, vec![Repair, Destroy]);
    unknown.onboarded_users = None;
    let s = scan(Knowable::Yes, vec![onboarded, unknown]);
    let e = destroy_refusal(&s, "half").unwrap_err();
    assert!(e.contains("remove the onboarded users' stacks first"), "{e}");
    assert!(e.contains("users/half/"), "{e}");
    // Without a listing there's no telling, and nothing to destroy from.
    assert!(destroy_refusal(&s, "blind").unwrap_err().contains("can't list"));
}

#[test]
fn only_your_own_stale_onboarding_lock_can_be_removed() {
    use EnvironmentAction::*;
    let mut stale = env("dev", EnvironmentState::Ready, vec![Join]);
    stale.onboarding = Some(OnboardingState::Incomplete { lock: Some(lock(9000, false)), ec2: None });
    stale.onboarding_actions = vec![Unlock];
    let mut live = env("live", EnvironmentState::Ready, vec![Join]);
    live.onboarding = Some(OnboardingState::InProgress { lock: Some(lock(10, false)) });
    let mut none = env("none", EnvironmentState::Ready, vec![Join]);
    none.onboarding = Some(OnboardingState::Incomplete { lock: None, ec2: None });
    let s = scan(Knowable::Yes, vec![stale, live, none]);
    let mine = owner_key(REGION, USER_ID);
    assert!(onboarding_unlock_refusal(&s, "dev", &mine).is_ok());
    let e = onboarding_unlock_refusal(&s, "dev", "0123456789ab").unwrap_err();
    assert!(e.contains("your own"), "{e}");
    assert!(onboarding_unlock_refusal(&s, "live", &mine).is_err());
    assert!(onboarding_unlock_refusal(&s, "none", &mine).is_err());
    assert!(onboarding_unlock_refusal(&s, "missing", &mine).is_err());
}

#[test]
fn a_boundary_bundle_names_an_administrator_without_the_admin_files() {
    let dir = tempfile::tempdir().unwrap();
    write_bundle(dir.path(), Deployment::Boundary, ACCOUNT, REGION, &boundary_vars(REGION)).unwrap();
    let readme = fs::read_to_string(dir.path().join("README.txt")).unwrap();
    assert!(readme.contains("account administrator"), "{readme}");
    assert!(readme.contains("WITHOUT"), "{readme}");
    assert!(!readme.contains("that carry the Triple-C environment-admin"), "{readme}");
}

#[test]
fn repair_and_update_are_refused_under_a_held_lock() {
    use EnvironmentAction::*;
    let s = scan(
        Knowable::Yes,
        vec![
            env("locked", EnvironmentState::Incomplete { lock: Some(lock(9000, false)) }, vec![Repair, Unlock]),
            env("free", EnvironmentState::Incomplete { lock: None }, vec![Repair]),
            env("old", EnvironmentState::UpdateAvailable, vec![Join, Update]),
        ],
    );
    let e = reapply_refusal(&s, "locked", Repair).unwrap_err();
    assert!(e.contains("Unlock it first"), "{e}");
    assert!(reapply_refusal(&s, "free", Repair).is_ok());
    assert!(reapply_refusal(&s, "old", Update).is_ok());
    assert!(reapply_refusal(&s, "free", Update).is_err());
}

#[test]
fn unlock_deletes_only_a_still_stale_lock_and_reads_412_as_changed() {
    assert!(stale_lock_refusal(0).is_err());
    assert!(stale_lock_refusal(LIVE_LOCK_SECS - 1).is_err());
    assert!(stale_lock_refusal(LIVE_LOCK_SECS).is_ok());
    assert!(is_precondition_failed(Some("PreconditionFailed"), Some(412)));
    assert!(is_precondition_failed(None, Some(412)));
    assert!(!is_precondition_failed(Some("AccessDenied"), Some(403)));
    assert!(!is_precondition_failed(None, Some(404)));
}

#[test]
fn an_existing_boundary_without_state_here_is_refused() {
    assert!(boundary_refusal(&Knowable::Yes, Some(false)).unwrap_err().contains("already exists"));
    assert!(boundary_refusal(&Knowable::Yes, Some(true)).is_ok(), "re-apply our own");
    assert!(boundary_refusal(&Knowable::Yes, None).is_ok(), "can't tell");
    assert!(boundary_refusal(&Knowable::No, Some(false)).is_ok());
    assert!(boundary_refusal(&Knowable::Unknown, Some(false)).is_ok());
}

#[test]
fn a_follow_is_refused_while_a_launch_holds_the_folder() {
    let dir = Path::new("/w/env/dev");
    let mut slots: Slots<u32> = Slots::default();
    slots.begin_launch(dir).unwrap();
    let mut spawned = false;
    let r = slots.install_follower(dir, "f1", false, || {
        spawned = true;
        1
    });
    assert_eq!(r.unwrap_err(), SlotBusy::Launching);
    assert!(!spawned, "nothing is spawned for a refused follow");
    // The launch's own follower replaces its Launching slot.
    assert_eq!(slots.install_follower(dir, "l1", true, || 2).unwrap(), None);
    slots.end_launch(dir); // the guard dropping afterwards
    assert!(!slots.is_free(dir), "end_launch must not clear a follower");
}

#[test]
fn a_launch_is_refused_while_a_follower_or_launch_holds_the_folder() {
    let dir = Path::new("/w/env/dev");
    let mut slots: Slots<u32> = Slots::default();
    slots.install_follower(dir, "f1", false, || 1).unwrap();
    assert_eq!(slots.begin_launch(dir).unwrap_err(), SlotBusy::Following);
    slots.finish_follower(dir, "f1");
    slots.begin_launch(dir).unwrap();
    assert_eq!(slots.begin_launch(dir).unwrap_err(), SlotBusy::Launching);
    slots.end_launch(dir);
    assert!(slots.is_free(dir));
    // Other folders are independent.
    slots.begin_launch(dir).unwrap();
    slots.begin_launch(Path::new("/w/env/prod")).unwrap();
}

#[test]
fn a_finishing_follower_only_removes_its_own_slot() {
    let dir = Path::new("/w/env/dev");
    let mut slots: Slots<u32> = Slots::default();
    slots.install_follower(dir, "old", false, || 1).unwrap();
    // A re-follow replaces it and hands back the old task to abort.
    assert_eq!(slots.install_follower(dir, "new", false, || 2).unwrap(), Some(1));
    slots.finish_follower(dir, "old");
    assert!(!slots.is_free(dir), "the old follower must not remove the new one's slot");
    slots.finish_follower(dir, "new");
    assert!(slots.is_free(dir));
    // Nor a launch's slot.
    slots.begin_launch(dir).unwrap();
    slots.finish_follower(dir, "new");
    assert_eq!(slots.begin_launch(dir).unwrap_err(), SlotBusy::Launching);
}

#[test]
fn ending_a_launch_never_clears_a_following_slot() {
    let dir = Path::new("/w/env/dev");
    let mut slots: Slots<u32> = Slots::default();
    slots.install_follower(dir, "f", false, || 1).unwrap();
    slots.end_launch(dir);
    assert_eq!(slots.begin_launch(dir).unwrap_err(), SlotBusy::Following);
    slots.end_launch(Path::new("/w/env/none")); // nothing there: no-op
}

fn recorded(env: &str, tag: &str, max: u32) -> UserCompute {
    let stem = crate::aws::tofu_modules::user_ec2_stem(env, tag);
    UserCompute {
        instance_type: "m7g.xlarge".into(),
        max_instances: max,
        user_tag: tag.into(),
        cluster_arn: format!("arn:aws:ecs:{REGION}:{ACCOUNT}:cluster/{stem}"),
        capacity_provider: stem,
    }
}

#[test]
fn a_reapply_keeps_the_recorded_compute_unless_changed() {
    let rec = UserRecord::Valid(recorded("dev", "alice", 3));
    let kept = Ec2Compute {
        instance_type: "m7g.xlarge".into(),
        max_instances: 3,
        user_tag: "alice".into(),
    };
    // Omitted: keep what the record says (or none without one).
    assert_eq!(resolve_ec2(None, &rec), Ok(Some(kept.clone())));
    assert_eq!(resolve_ec2(None, &UserRecord::Absent), Ok(None));
    // Explicitly none: the re-apply removes it.
    assert_eq!(resolve_ec2(Some(Ec2Choice::None), &rec), Ok(None));
    // A new choice wins.
    let new = Ec2Compute {
        instance_type: "m7i.large".into(),
        max_instances: 1,
        user_tag: "alice".into(),
    };
    assert_eq!(
        resolve_ec2(Some(Ec2Choice::Ec2(new.clone())), &rec),
        Ok(Some(new.clone()))
    );
    // A record that exists but can't be used is never read as "none" (that
    // would tear the capacity down); an explicit choice still goes.
    let bad = UserRecord::Unusable("schema is not 1".into());
    let e = resolve_ec2(None, &bad).unwrap_err();
    assert!(e.contains("schema is not 1") && e.contains("explicitly"), "{e}");
    assert_eq!(resolve_ec2(Some(Ec2Choice::None), &bad), Ok(None));
    assert_eq!(resolve_ec2(Some(Ec2Choice::Ec2(new.clone())), &bad), Ok(Some(new.clone())));
    // The kept values reach the module's variables unchanged.
    let vars = onboarding_vars("dev", REGION, USER_ID, resolve_ec2(None, &rec).unwrap().as_ref()).unwrap();
    assert_eq!(vars["ec2_instance_type"], "m7g.xlarge");
    assert_eq!(vars["ec2_max_instances"], 3);
    assert_eq!(vars["ec2_user_tag"], "alice");

    // The UI's JSON for each choice.
    let none: Ec2Choice = serde_json::from_value(json!({"kind": "none"})).unwrap();
    assert_eq!(none, Ec2Choice::None);
    let ec2: Ec2Choice = serde_json::from_value(json!({
        "kind": "ec2", "instance_type": "m7i.large", "max_instances": 1, "user_tag": "alice"
    }))
    .unwrap();
    assert_eq!(ec2, Ec2Choice::Ec2(new));
}

fn held(kind: &'static str, owner: Option<&str>) -> NameHolder {
    NameHolder {
        kind,
        owner: owner.map(String::from),
    }
}

#[test]
fn ec2_names_held_by_another_owner_are_refused() {
    let stem = "triple-c-dev--u-alice";
    let me = USER_ID;
    // Free names: fine, with or without a record.
    assert!(ec2_collision_refusal(stem, &[], me, None).is_ok());
    assert!(ec2_collision_refusal(stem, &[], me, Some(&recorded("dev", "bob", 1))).is_ok());
    let cluster = |o| held("ECS cluster", o);
    let group = |o| held("Auto Scaling group", o);
    // Tagged as mine: a half-built capacity (no record yet) is re-applied.
    assert!(ec2_collision_refusal(stem, &[cluster(Some(me)), group(Some(me))], me, None).is_ok());
    // Another owner's, or untagged: refused, naming the resource.
    let e = ec2_collision_refusal(stem, &[cluster(Some("AROAOTHER:bob"))], me, None).unwrap_err();
    assert!(e.contains("ECS cluster") && e.contains(stem) && e.contains("another onboarding"), "{e}");
    assert!(e.contains("triple-c-user"), "{e}");
    let e = ec2_collision_refusal(stem, &[cluster(Some(me)), group(None)], me, None).unwrap_err();
    assert!(e.contains("Auto Scaling group") && e.contains("isn't tagged"), "{e}");
    // The record naming the cluster is the fallback for an untagged or
    // differently tagged one.
    let mine = recorded("dev", "alice", 2);
    assert!(ec2_collision_refusal(stem, &[cluster(None), group(Some("x"))], me, Some(&mine)).is_ok());
    // A record naming another cluster doesn't help.
    assert!(ec2_collision_refusal(stem, &[cluster(None)], me, Some(&recorded("dev", "bob", 1))).is_err());
    // A cluster that merely ends in the same text is not the same cluster.
    let mut lookalike = recorded("dev", "alice", 1);
    lookalike.cluster_arn = format!("arn:aws:ecs:{REGION}:{ACCOUNT}:cluster/x{stem}");
    assert!(ec2_collision_refusal(stem, &[cluster(None)], me, Some(&lookalike)).is_err());
}

/// An `OnboardApi` answering from fixed data, recording what it was asked.
#[derive(Default)]
struct FakeOnboard {
    record: Option<String>,
    record_denied: bool,
    cluster: Option<NamedResource>,
    group: Option<NamedResource>,
    group_denied: bool,
    calls: std::sync::Mutex<Vec<String>>,
}

impl FakeOnboard {
    fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }
}

impl OnboardApi for FakeOnboard {
    async fn parameter(&self, name: &str) -> Result<Param, AwsError> {
        self.calls.lock().unwrap().push(format!("parameter {name}"));
        Ok(match (&self.record, self.record_denied) {
            (_, true) => Param::Denied,
            (Some(v), _) => Param::Value(v.clone()),
            (None, _) => Param::Missing,
        })
    }
    async fn cluster(&self, name: &str) -> Result<Option<NamedResource>, AwsError> {
        self.calls.lock().unwrap().push(format!("cluster {name}"));
        Ok(self.cluster.clone())
    }
    async fn group(&self, name: &str) -> Result<Option<NamedResource>, AwsError> {
        self.calls.lock().unwrap().push(format!("group {name}"));
        if self.group_denied {
            return Err(AwsError::AccessDenied {
                action: "autoscaling:DescribeAutoScalingGroups".into(),
                message: "denied".into(),
            });
        }
        Ok(self.group.clone())
    }
}

fn named(status: Option<&str>, owner: Option<&str>) -> NamedResource {
    NamedResource {
        status: status.map(String::from),
        tags: owner
            .map(|o| vec![("triple-c:owner".to_string(), o.to_string())])
            .unwrap_or_default(),
    }
}

fn record_json(env: &str, tag: &str) -> String {
    let r = recorded(env, tag, 2);
    json!({
        "schema": 1, "instance_type": r.instance_type, "cluster_arn": r.cluster_arn,
        "capacity_provider": r.capacity_provider, "user_tag": r.user_tag, "max_instances": 2,
    })
    .to_string()
}

fn prepare(api: &FakeOnboard, choice: Option<Ec2Choice>) -> Result<Value, AwsError> {
    tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap()
        .block_on(prepare_onboarding(api, "dev", REGION, ACCOUNT, USER_ID, choice))
}

fn alice() -> Option<Ec2Choice> {
    Some(Ec2Choice::Ec2(Ec2Compute {
        instance_type: "m7i.large".into(),
        max_instances: 1,
        user_tag: "alice".into(),
    }))
}

#[test]
fn onboarding_checks_the_ec2_names_before_it_applies() {
    let stem = "triple-c-dev--u-alice";
    // Fargate only: no name lookups at all.
    let api = FakeOnboard::default();
    let vars = prepare(&api, Some(Ec2Choice::None)).unwrap();
    assert_eq!(vars["ec2_instance_type"], "");
    assert!(api.calls().iter().all(|c| c.starts_with("parameter ")), "{:?}", api.calls());

    // Free names: the vars come back.
    let api = FakeOnboard::default();
    let vars = prepare(&api, alice()).unwrap();
    assert_eq!(vars["ec2_user_tag"], "alice");
    assert!(api.calls().contains(&format!("cluster {stem}")));
    assert!(api.calls().contains(&format!("group {stem}")));

    // Another owner's cluster: refused, so nothing is launched.
    let api = FakeOnboard {
        cluster: Some(named(Some("ACTIVE"), Some("AROAOTHER:bob"))),
        ..Default::default()
    };
    let e = prepare(&api, alice()).unwrap_err();
    assert!(format!("{e:?}").contains("another onboarding"), "{e:?}");

    // My own half-built cluster and group (a failed apply, no record yet).
    let api = FakeOnboard {
        cluster: Some(named(Some("PROVISIONING"), Some(USER_ID))),
        group: Some(named(None, Some(USER_ID))),
        ..Default::default()
    };
    assert!(prepare(&api, alice()).is_ok());

    // Untagged: refused unless my record names it.
    let untagged = || FakeOnboard {
        cluster: Some(named(Some("ACTIVE"), None)),
        ..Default::default()
    };
    assert!(prepare(&untagged(), alice()).is_err());
    let api = FakeOnboard {
        record: Some(record_json("dev", "alice")),
        ..untagged()
    };
    assert!(prepare(&api, alice()).is_ok());

    // A deleted (INACTIVE) cluster doesn't hold the name; every other
    // status does.
    let api = FakeOnboard {
        cluster: Some(named(Some("INACTIVE"), None)),
        ..Default::default()
    };
    assert!(prepare(&api, alice()).is_ok());
    for status in ["ACTIVE", "PROVISIONING", "DEPROVISIONING", "FAILED"] {
        let api = FakeOnboard {
            cluster: Some(named(Some(status), Some("AROAOTHER:bob"))),
            ..Default::default()
        };
        assert!(prepare(&api, alice()).is_err(), "{status}");
    }

    // An Auto Scaling group alone is enough to refuse.
    let api = FakeOnboard {
        group: Some(named(None, Some("AROAOTHER:bob"))),
        ..Default::default()
    };
    assert!(prepare(&api, alice()).is_err());

    // A denied group lookup is an error, never "free".
    let api = FakeOnboard {
        group_denied: true,
        ..Default::default()
    };
    assert!(prepare(&api, alice()).is_err());
}

#[test]
fn capacity_kept_from_the_record_is_checked_too() {
    let stem = "triple-c-dev--u-alice";
    // Omitted choice + a record: the record's capacity, and its names are
    // checked (here another owner's group appeared under it).
    let api = FakeOnboard {
        record: Some(record_json("dev", "alice")),
        cluster: Some(named(Some("ACTIVE"), Some(USER_ID))),
        group: Some(named(None, Some(USER_ID))),
        ..Default::default()
    };
    let vars = prepare(&api, None).unwrap();
    assert_eq!(vars["ec2_instance_type"], "m7g.xlarge");
    assert_eq!(vars["ec2_max_instances"], 2);
    assert!(api.calls().contains(&format!("group {stem}")));

    // A record that can't be used (unreadable or invalid) and no choice:
    // refused before any name lookup or launch.
    for api in [
        FakeOnboard {
            record_denied: true,
            ..Default::default()
        },
        FakeOnboard {
            record: Some("{\"schema\": 9}".into()),
            ..Default::default()
        },
    ] {
        let e = prepare(&api, None).unwrap_err();
        assert!(format!("{e:?}").contains("explicitly"), "{e:?}");
        assert!(api.calls().iter().all(|c| c.starts_with("parameter ")));
    }
}

#[test]
fn a_bundle_warns_when_its_cluster_name_looks_taken() {
    let run = |api: &FakeOnboard| {
        let c = Ec2Compute {
            instance_type: "m7i.large".into(),
            max_instances: 1,
            user_tag: "alice".into(),
        };
        tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
            .block_on(bundle_ec2_warning(api, "dev", REGION, ACCOUNT, USER_ID, &c))
    };
    assert_eq!(run(&FakeOnboard::default()), None);
    let api = FakeOnboard {
        cluster: Some(named(Some("ACTIVE"), Some("AROAOTHER:bob"))),
        ..Default::default()
    };
    assert!(run(&api).unwrap().contains("another onboarding"));
    // Only the cluster half: a workspace user can't read Auto Scaling.
    assert!(!api.calls().iter().any(|c| c.starts_with("group ")));
    let api = FakeOnboard {
        cluster: Some(named(Some("ACTIVE"), Some(USER_ID))),
        ..Default::default()
    };
    assert_eq!(run(&api), None);
}

#[test]
fn the_ui_gets_the_apps_instance_table() {
    let table = list_ec2_instance_types();
    assert_eq!(table.types, crate::models::project::ec2_instance_types());
    assert_eq!(table.types.len(), 30);
    assert_eq!(table.default_type, "m7i.large");
    assert!(table.types.iter().any(|t| t.instance_type == table.default_type));
}
