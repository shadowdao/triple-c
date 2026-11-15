use std::fs;
use std::time::Duration;

use serde_json::json;

use super::*;
use crate::aws::tofu::Workdir;
use crate::aws::environments::{
    EnvironmentState, Knowable, LockInfo, OnboardingState,
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
    }
}

fn scan(boundary: Knowable, environments: Vec<DetectedEnvironment>) -> EnvironmentScan {
    EnvironmentScan {
        region: REGION.into(),
        account: ACCOUNT.into(),
        owner_id: USER_ID.into(),
        owner_key: owner_key(USER_ID),
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
    let ok = owner_key(USER_ID);
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
    let ok = owner_key(USER_ID);
    let d = Deployment::User { env: "dev", owner_key: &ok };
    let vars = onboarding_vars("dev", REGION, USER_ID);
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
    assert!(readme.contains(&format!("LocationConstraint={REGION}")), "{readme}");

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
    let vars = environment_vars("dev", "us-east-1", &NetworkChoice::Public).unwrap();
    write_bundle(dir.path(), Deployment::Environment("dev"), ACCOUNT, "us-east-1", &vars).unwrap();
    let readme = fs::read_to_string(dir.path().join("README.txt")).unwrap();
    assert!(readme.contains("aws s3api create-bucket"), "{readme}");
    assert!(!readme.contains("LocationConstraint"), "{readme}");
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
    busy.onboarding = Some(OnboardingState::InProgress);
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
