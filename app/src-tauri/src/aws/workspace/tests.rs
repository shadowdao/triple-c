use std::collections::BTreeMap;
use std::sync::Mutex;

use aws_sdk_ecs::operation::register_task_definition::RegisterTaskDefinitionInput;
use aws_sdk_ecs::types::{CpuArchitecture, EfsAuthorizationConfigIam, EfsTransitEncryption};

use super::*;
use crate::aws::environments::EnvironmentInfo;
use crate::aws::identity::CallerIdentity;
use crate::aws::tofu_modules::{
    access_point_root, log_stream_prefix, owner_key, project_secret_name, task_family,
    user_file_system_token, workspace_access_point_root,
};
use crate::models::project::{Arch, AwsPlacement, AwsWorkspaceConfig, RepoSource, StorageMode};
use crate::models::Project;

const ACCOUNT: &str = "111122223333";
const REGION: &str = "eu-west-1";
const USER: &str = "AROAEXAMPLEID:alice@example.com";
const OTHER: &str = "AROAOTHERID:mallory";
const PROJECT_ID: &str = "0f1e2d3c-4b5a-6978-8796-a5b4c3d2e1f0";
const SECRET_ARN: &str =
    "arn:aws:secretsmanager:eu-west-1:111122223333:secret:triple-c/dev/abc/p-AbCdEf";

fn who() -> CallerIdentity {
    CallerIdentity {
        account: ACCOUNT.into(),
        arn: format!("arn:aws:sts::{ACCOUNT}:assumed-role/Dev/alice"),
        user_id: USER.into(),
    }
}

fn env_info() -> EnvironmentInfo {
    EnvironmentInfo {
        schema: 1,
        name: "dev".into(),
        template_version: 1,
        cluster_arn: format!("arn:aws:ecs:{REGION}:{ACCOUNT}:cluster/triple-c-dev"),
        security_group_id: "sg-0123456789abcdef0".into(),
        subnet_ids: vec!["subnet-0a".into(), "subnet-0b".into()],
        assign_public_ip: true,
        kms_key_arn: format!(
            "arn:aws:kms:{REGION}:{ACCOUNT}:key/1234abcd-12ab-34cd-56ef-1234567890ab"
        ),
        log_group: "/triple-c/dev".into(),
        network_mode: "public".into(),
        tenancy: "strict".into(),
        max_workspace_hours: 0,
    }
}

fn project(storage: StorageMode) -> Project {
    let mut p = Project::new("p".into(), vec![]);
    p.id = PROJECT_ID.into();
    p.host_id = Some("conn-1".into());
    p.aws_placement = Some(AwsPlacement {
        region: REGION.into(),
        environment: "dev".into(),
    });
    p.aws = Some(AwsWorkspaceConfig {
        storage,
        repos: vec![RepoSource {
            url: "https://github.com/example/repo.git".into(),
            branch: Some("main".into()),
            folder: "repo".into(),
        }],
        ..Default::default()
    });
    p
}

fn pairs(kv: &[(&str, &str)]) -> Vec<(String, String)> {
    kv.iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

fn sample_plan(storage: StorageMode) -> WorkspacePlan {
    let ok = owner_key(REGION, USER);
    WorkspacePlan {
        family: task_family("dev", &ok, PROJECT_ID),
        cpu: 1024,
        memory: 4096,
        arch: Arch::X86_64,
        storage,
        ephemeral_gib: 21,
        image: "ghcr.io/example/triple-c:latest".into(),
        exec_role_arn: format!("arn:aws:iam::{ACCOUNT}:role/triple-c-dev-{ok}-exec"),
        task_role_arn: format!("arn:aws:iam::{ACCOUNT}:role/triple-c-dev-{ok}-task"),
        log_group: "/triple-c/dev".into(),
        log_stream_prefix: log_stream_prefix(&ok, PROJECT_ID),
        region: REGION.into(),
        secret_arn: SECRET_ARN.into(),
        secret_keys: vec![
            "A_KEY".into(),
            "GIT_TOKEN".into(),
            "TRIPLEC_AGENT_TOKEN".into(),
        ],
        file_system_id: (storage != StorageMode::Ephemeral).then(|| "fs-0123abcd".to_string()),
        access_point_id: (storage != StorageMode::Ephemeral).then(|| "fsap-0123abcd".to_string()),
        workspace_access_point_id: (storage == StorageMode::Efs)
            .then(|| "fsap-4567efab".to_string()),
        tags: workspace_tags(USER, PROJECT_ID, "dev"),
        gateway_image: None,
        ec2: None,
    }
}

// ---------------------------------------------------------------- names

#[test]
fn names_use_owner_key_and_never_contain_a_colon() {
    let ok = owner_key(REGION, USER);
    let names = [
        task_family("dev", &ok, PROJECT_ID),
        project_secret_name("dev", &ok, PROJECT_ID),
        access_point_root(&ok, PROJECT_ID),
        log_stream_prefix(&ok, PROJECT_ID),
        user_file_system_token("dev", &ok),
        workspace_access_point_root(&ok, PROJECT_ID),
    ];
    for n in &names {
        assert!(n.contains(&ok), "{n} lacks the owner key");
        assert!(!n.contains(':'), "{n} contains a colon");
        assert!(!n.contains("AROA"), "{n} contains the raw user id");
    }
    assert_eq!(names[0], format!("triple-c-dev-{ok}-0f1e2d3c-4b5"));
    assert_eq!(names[1], format!("triple-c/dev/{ok}/{PROJECT_ID}"));
    assert_eq!(names[2], format!("/triple-c/{ok}/{PROJECT_ID}"));
    assert_eq!(names[3], format!("{ok}/{PROJECT_ID}"));
    // The onboarding module's `creation_token` (local.name).
    assert_eq!(names[4], format!("triple-c-dev-{ok}"));
    // A sibling of the home roots, never inside one (Q1).
    assert_eq!(names[5], format!("/triple-c-workspaces/{ok}/{PROJECT_ID}"));
    assert!(!names[5].starts_with(&format!("{}/", names[2])));
}

#[test]
fn names_sanitise_a_hostile_project_id() {
    let ok = owner_key(REGION, USER);
    for id in ["../../etc:x", "a/b", "..", ".", "", "x y*z"] {
        let fam = task_family("dev", &ok, id);
        assert!(fam.len() <= 255);
        assert!(
            fam.bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_'),
            "{fam}"
        );
        let root = access_point_root(&ok, id);
        let tail = root.strip_prefix(&format!("/triple-c/{ok}/")).unwrap();
        assert!(
            !tail.is_empty() && !tail.contains('/') && tail != "." && tail != "..",
            "{root}"
        );
        let wroot = workspace_access_point_root(&ok, id);
        assert_eq!(
            wroot.strip_prefix(&format!("/triple-c-workspaces/{ok}/")),
            Some(tail),
            "{wroot}"
        );
        let secret = project_secret_name("dev", &ok, id);
        assert!(!secret.contains(':') && !secret.contains(".."), "{secret}");
        assert_eq!(secret.matches('/').count(), 3, "{secret}");
    }
    let long = "x".repeat(500);
    assert!(task_family("abcdefghijklmnopqrst", &ok, &long).len() <= 255);
    assert!(access_point_root(&ok, &long).len() <= 100);
    assert!(workspace_access_point_root(&ok, &long).len() <= 100);
}

// ------------------------------------------------------- task definition

fn env_names(req: &RegisterTaskDefinitionInput) -> BTreeMap<String, String> {
    req.container_definitions()[0]
        .environment()
        .iter()
        .map(|kv| {
            (
                kv.name().unwrap().to_string(),
                kv.value().unwrap().to_string(),
            )
        })
        .collect()
}

#[test]
fn request_environment_is_only_the_constants_and_every_key_is_a_secret() {
    let plan = sample_plan(StorageMode::Ephemeral);
    let req = task_definition_request(&plan);
    assert_eq!(
        env_names(&req),
        BTreeMap::from([
            ("COLORTERM".to_string(), "truecolor".to_string()),
            ("TRIPLEC_HOST".to_string(), "aws".to_string()),
        ])
    );
    let c = &req.container_definitions()[0];
    let secrets: Vec<(String, String)> = c
        .secrets()
        .iter()
        .map(|s| (s.name().to_string(), s.value_from().to_string()))
        .collect();
    let expected: Vec<(String, String)> = plan
        .secret_keys
        .iter()
        .map(|k| (k.clone(), format!("{SECRET_ARN}:{k}::")))
        .collect();
    assert_eq!(secrets, expected);
}

#[test]
fn request_shape_matches_the_global_constraints() {
    let plan = sample_plan(StorageMode::Ephemeral);
    let req = task_definition_request(&plan);
    assert_eq!(req.family(), Some(plan.family.as_str()));
    assert_eq!(req.cpu(), Some("1024"));
    assert_eq!(req.memory(), Some("4096"));
    assert_eq!(
        req.network_mode(),
        Some(&aws_sdk_ecs::types::NetworkMode::Awsvpc)
    );
    assert_eq!(
        req.requires_compatibilities(),
        &[aws_sdk_ecs::types::Compatibility::Fargate]
    );
    assert_eq!(req.task_role_arn(), Some(plan.task_role_arn.as_str()));
    assert_eq!(req.execution_role_arn(), Some(plan.exec_role_arn.as_str()));
    assert_eq!(req.ephemeral_storage().unwrap().size_in_gib(), 21);
    // Room for the TERM trap's WIP push (90 s) before SIGKILL.
    assert_eq!(req.container_definitions()[0].stop_timeout(), Some(120));
    let rp = req.runtime_platform().unwrap();
    assert_eq!(rp.cpu_architecture(), Some(&CpuArchitecture::X8664));
    assert_eq!(
        rp.operating_system_family(),
        Some(&aws_sdk_ecs::types::OsFamily::Linux)
    );

    let tags: BTreeMap<_, _> = req
        .tags()
        .iter()
        .map(|t| (t.key().unwrap(), t.value().unwrap()))
        .collect();
    assert_eq!(
        tags,
        BTreeMap::from([
            ("triple-c:environment", "dev"),
            ("triple-c:owner", USER),
            ("triple-c:project", PROJECT_ID),
        ])
    );

    assert_eq!(req.container_definitions().len(), 1);
    let c = &req.container_definitions()[0];
    assert_eq!(c.name(), Some("workspace"));
    assert_eq!(c.essential(), Some(true));
    assert_eq!(c.image(), Some(plan.image.as_str()));
    let hc = c.health_check().unwrap();
    assert_eq!(
        hc.command(),
        &[
            "CMD-SHELL".to_string(),
            "curl -sf --max-time 3 -H \"Authorization: Bearer $(cat /dev/shm/triple-c/agent-token)\" http://127.0.0.1:47900/v1/health".to_string(),
        ]
    );
    assert_eq!(
        (hc.interval(), hc.retries(), hc.start_period()),
        (Some(30), Some(3), Some(120))
    );
    let lp = c.linux_parameters().unwrap();
    assert_eq!(lp.init_process_enabled(), Some(true));
    assert_eq!(
        lp.capabilities().unwrap().add(),
        &["SYS_PTRACE".to_string()]
    );
    assert!(lp.capabilities().unwrap().drop().is_empty());
    let log = c.log_configuration().unwrap();
    assert_eq!(log.log_driver(), &aws_sdk_ecs::types::LogDriver::Awslogs);
    let opts = log.options().unwrap();
    assert_eq!(
        opts.get("awslogs-group").map(String::as_str),
        Some("/triple-c/dev")
    );
    assert_eq!(opts.get("awslogs-region").map(String::as_str), Some(REGION));
    assert_eq!(
        opts.get("awslogs-stream-prefix"),
        Some(&plan.log_stream_prefix)
    );
    assert_eq!(opts.len(), 3);
    let labels = c.docker_labels().unwrap();
    assert_eq!(labels.len(), 1);
    assert_eq!(
        labels.get(FINGERPRINT_LABEL),
        Some(&workspace_fingerprint(&plan))
    );
    assert!(c.port_mappings().is_empty());
}

#[test]
fn ephemeral_has_no_volume_or_mount() {
    let req = task_definition_request(&sample_plan(StorageMode::Ephemeral));
    assert!(req.volumes().is_empty());
    assert!(req.container_definitions()[0].mount_points().is_empty());
}

#[test]
fn hybrid_mounts_the_access_point_with_iam_and_tls() {
    let req = task_definition_request(&sample_plan(StorageMode::Hybrid));
    assert_eq!(req.volumes().len(), 1);
    let v = &req.volumes()[0];
    assert_eq!(v.name(), Some("home"));
    let efs = v.efs_volume_configuration().unwrap();
    assert_eq!(efs.file_system_id(), "fs-0123abcd");
    assert_eq!(
        efs.transit_encryption(),
        Some(&EfsTransitEncryption::Enabled)
    );
    let auth = efs.authorization_config().unwrap();
    assert_eq!(auth.access_point_id(), Some("fsap-0123abcd"));
    assert_eq!(auth.iam(), Some(&EfsAuthorizationConfigIam::Enabled));
    // An access point sets the root; the volume must not set its own.
    assert!(efs.root_directory().is_none() || efs.root_directory() == Some("/"));
    let mounts = req.container_definitions()[0].mount_points();
    assert_eq!(mounts.len(), 1);
    assert_eq!(mounts[0].source_volume(), Some("home"));
    assert_eq!(mounts[0].container_path(), Some("/mnt/triple-c-home"));
    assert_eq!(mounts[0].read_only(), Some(false));
}

/// Captured before EFS mode existed: Hybrid and Ephemeral requests must not
/// change (a changed fingerprint registers a new revision for every project).
#[test]
fn hybrid_and_ephemeral_requests_are_unchanged_by_efs_mode() {
    assert_eq!(
        workspace_fingerprint(&sample_plan(StorageMode::Hybrid)),
        "d6d00b3f3489060c40ff732be8eb8a10e723e26dd5f1ff58345e4c2a307f862d"
    );
    assert_eq!(
        workspace_fingerprint(&sample_plan(StorageMode::Ephemeral)),
        "00f248ed315167fe7a1adc0ea5c8bcee328e8a0739a1c4f9ce9ac10270851786"
    );
    // A stray workspace access point (kept after switching EFS → Hybrid)
    // does not reach a Hybrid or Ephemeral request.
    for storage in [StorageMode::Hybrid, StorageMode::Ephemeral] {
        let mut p = sample_plan(storage);
        p.workspace_access_point_id = Some("fsap-stray".into());
        assert_eq!(
            workspace_fingerprint(&p),
            workspace_fingerprint(&sample_plan(storage))
        );
    }
}

#[test]
fn efs_mounts_home_and_workspace_access_points() {
    let req = task_definition_request(&sample_plan(StorageMode::Efs));
    let vols = req.volumes();
    assert_eq!(vols.len(), 2);
    let by_name = |n: &str| vols.iter().find(|v| v.name() == Some(n)).unwrap();
    for (name, ap) in [("home", "fsap-0123abcd"), ("workspace", "fsap-4567efab")] {
        let efs = by_name(name).efs_volume_configuration().unwrap();
        assert_eq!(efs.file_system_id(), "fs-0123abcd");
        assert_eq!(efs.transit_encryption(), Some(&EfsTransitEncryption::Enabled));
        let auth = efs.authorization_config().unwrap();
        assert_eq!(auth.access_point_id(), Some(ap));
        assert_eq!(auth.iam(), Some(&EfsAuthorizationConfigIam::Enabled));
        assert!(efs.root_directory().is_none() || efs.root_directory() == Some("/"));
    }
    let mounts: Vec<(&str, &str, Option<bool>)> = req.container_definitions()[0]
        .mount_points()
        .iter()
        .map(|m| {
            (
                m.source_volume().unwrap(),
                m.container_path().unwrap(),
                m.read_only(),
            )
        })
        .collect();
    assert_eq!(
        mounts,
        vec![
            ("home", "/mnt/triple-c-home", Some(false)),
            ("workspace", "/workspace", Some(false)),
        ]
    );
    // Its own fingerprint, and the workspace access point is covered.
    let fp = workspace_fingerprint(&sample_plan(StorageMode::Efs));
    assert_ne!(fp, workspace_fingerprint(&sample_plan(StorageMode::Hybrid)));
    let mut other = sample_plan(StorageMode::Efs);
    other.workspace_access_point_id = Some("fsap-other".into());
    assert_ne!(workspace_fingerprint(&other), fp);
}

#[test]
fn efs_without_its_workspace_access_point_mounts_nothing_at_workspace() {
    // Not a plan `prepare_workspace` makes; the builder must not invent one.
    let mut p = sample_plan(StorageMode::Efs);
    p.workspace_access_point_id = None;
    let req = task_definition_request(&p);
    assert!(req
        .container_definitions()[0]
        .mount_points()
        .iter()
        .all(|m| m.container_path() != Some("/workspace")));
}

#[test]
fn arm64_sets_the_runtime_platform() {
    let mut plan = sample_plan(StorageMode::Ephemeral);
    plan.arch = Arch::Arm64;
    let req = task_definition_request(&plan);
    assert_eq!(
        req.runtime_platform().unwrap().cpu_architecture(),
        Some(&CpuArchitecture::Arm64)
    );
}

#[test]
fn fingerprint_is_stable_and_covers_every_field() {
    let base = sample_plan(StorageMode::Hybrid);
    assert_eq!(
        workspace_fingerprint(&base),
        workspace_fingerprint(&base.clone())
    );
    let fp = workspace_fingerprint(&base);
    assert_eq!(fp.len(), 64);
    assert!(fp.bytes().all(|c| c.is_ascii_hexdigit()));

    type Change = Box<dyn Fn(&mut WorkspacePlan)>;
    let changes: Vec<Change> = vec![
        Box::new(|p| p.family.push('x')),
        Box::new(|p| p.cpu = 2048),
        Box::new(|p| p.memory = 8192),
        Box::new(|p| p.arch = Arch::Arm64),
        Box::new(|p| p.storage = StorageMode::Ephemeral),
        Box::new(|p| p.ephemeral_gib = 30),
        Box::new(|p| p.image.push('2')),
        Box::new(|p| p.exec_role_arn.push('x')),
        Box::new(|p| p.task_role_arn.push('x')),
        Box::new(|p| p.log_group.push('x')),
        Box::new(|p| p.log_stream_prefix.push('x')),
        Box::new(|p| p.region = "us-east-1".into()),
        Box::new(|p| p.secret_arn.push('x')),
        Box::new(|p| p.secret_keys.push("NEW".into())),
        Box::new(|p| p.file_system_id = Some("fs-other".into())),
        Box::new(|p| p.access_point_id = Some("fsap-other".into())),
        Box::new(|p| p.tags[0].1.push('x')),
    ];
    for (i, change) in changes.iter().enumerate() {
        let mut p = base.clone();
        change(&mut p);
        assert_ne!(
            workspace_fingerprint(&p),
            fp,
            "change {i} kept the fingerprint"
        );
    }
}

// --------------------------------------------------------- secret payload

fn payload_map(json: &str) -> BTreeMap<String, String> {
    serde_json::from_str(json).unwrap()
}

#[test]
fn payload_holds_all_project_data_with_sorted_keys() {
    let env = pairs(&[("ZED", "1"), ("GIT_TOKEN", "ghp_secret"), ("ALPHA", "a")]);
    let files = vec![
        ("ssh/id_ed25519".to_string(), b"KEY".to_vec()),
        ("ca/corp.pem".to_string(), b"PEM".to_vec()),
    ];
    let repos = project(StorageMode::Hybrid).aws.unwrap().repos;
    let json = project_secret_payload_with(
        &env,
        "tok",
        &files,
        &repos,
        true,
        Some(30),
        StorageMode::Hybrid,
        PayloadExtras::default(),
    )
    .unwrap();
    let m = payload_map(&json);
    let keys: Vec<&str> = m.keys().map(String::as_str).collect();
    assert_eq!(
        keys,
        [
            "ALPHA",
            "GIT_TOKEN",
            "TRIPLEC_AGENT_TOKEN",
            "TRIPLEC_FILES",
            "TRIPLEC_IDLE_STOP_MINUTES",
            "TRIPLEC_REPOS",
            "TRIPLEC_STORAGE",
            "TRIPLEC_WIP_PUSH",
            "ZED",
        ]
    );
    // Serialised in key order, so the secret value does not churn.
    let positions: Vec<usize> = m
        .keys()
        .map(|k| json.find(&format!("\"{k}\"")).unwrap())
        .collect();
    assert!(positions.windows(2).all(|w| w[0] < w[1]));
    assert_eq!(m["TRIPLEC_AGENT_TOKEN"], "tok");
    assert_eq!(m["TRIPLEC_WIP_PUSH"], "1");
    assert_eq!(m["TRIPLEC_IDLE_STOP_MINUTES"], "30");
    assert_eq!(m["TRIPLEC_STORAGE"], "hybrid");
    let files: BTreeMap<String, String> = serde_json::from_str(&m["TRIPLEC_FILES"]).unwrap();
    assert_eq!(
        files,
        BTreeMap::from([
            ("ca/corp.pem".to_string(), "UEVN".to_string()),
            ("ssh/id_ed25519".to_string(), "S0VZ".to_string()),
        ])
    );
    let repos: serde_json::Value = serde_json::from_str(&m["TRIPLEC_REPOS"]).unwrap();
    assert_eq!(
        repos,
        serde_json::json!([{"url": "https://github.com/example/repo.git", "branch": "main", "folder": "repo"}])
    );
    assert!(!m.keys().any(|k| k.starts_with("TRIPLEC_FILE_")));
    assert!(!m.contains_key("TRIPLEC_SECRET_KEYS"));
}

#[test]
fn payload_off_values() {
    let json =
        project_secret_payload_with(
            &[],
            "t",
            &[],
            &[],
            false,
            None,
            StorageMode::Ephemeral,
            PayloadExtras::default(),
        ).unwrap();
    let m = payload_map(&json);
    assert_eq!(m["TRIPLEC_WIP_PUSH"], "0");
    assert_eq!(m["TRIPLEC_STORAGE"], "ephemeral");
    assert!(!m.contains_key("TRIPLEC_IDLE_STOP_MINUTES"));
    assert_eq!(m["TRIPLEC_FILES"], "{}");
    assert_eq!(m["TRIPLEC_REPOS"], "[]");
}

#[test]
fn payload_is_independent_of_env_pair_order() {
    let a = pairs(&[("B", "2"), ("A", "1"), ("C", "3")]);
    let b = pairs(&[("C", "3"), ("A", "1"), ("B", "2")]);
    let pa = project_secret_payload_with(
        &a,
        "t",
        &[],
        &[],
        true,
        None,
        StorageMode::Hybrid,
        PayloadExtras::default(),
    ).unwrap();
    let pb = project_secret_payload_with(
        &b,
        "t",
        &[],
        &[],
        true,
        None,
        StorageMode::Hybrid,
        PayloadExtras::default(),
    ).unwrap();
    assert_eq!(pa, pb);
}

#[test]
fn payload_rejects_bad_file_names() {
    for name in [
        "ssh/", "ssh/.", "ssh/..", "ssh/a/b", "other/x", "x", "ca/../k", "ssh/a:b",
    ] {
        let files = vec![(name.to_string(), b"x".to_vec())];
        let r = project_secret_payload_with(
            &[],
            "t",
            &files,
            &[],
            true,
            None,
            StorageMode::Hybrid,
            PayloadExtras::default(),
        );
        if name == "ssh/a:b" {
            // A colon is a fine file name; only the secret's *keys* are restricted.
            assert!(r.is_ok());
        } else {
            assert!(r.is_err(), "{name} accepted");
        }
    }
}

#[test]
fn payload_rejects_env_names_a_secret_reference_cannot_carry() {
    for bad in ["A:B", "", "1ABC", "A-B", "A B"] {
        let env = pairs(&[(bad, "SENTINEL-VALUE-7f3a")]);
        let r = project_secret_payload_with(
            &env,
            "t",
            &[],
            &[],
            true,
            None,
            StorageMode::Hybrid,
            PayloadExtras::default(),
        );
        assert!(r.is_err(), "{bad:?} accepted");
        // The error never echoes a value.
        assert!(!r.unwrap_err().contains("SENTINEL-VALUE-7f3a"));
    }
}

#[test]
fn payload_keeps_its_own_keys_over_custom_env() {
    let env = pairs(&[
        ("TRIPLEC_AGENT_TOKEN", "forged"),
        ("TRIPLEC_HOST", "local"),
        ("TRIPLEC_AGENT_ADDR", "0.0.0.0:47900"),
        ("triplec_other", "x"),
        ("COLORTERM", "16"),
        ("OK", "1"),
    ]);
    let m = payload_map(
        &project_secret_payload_with(
            &env,
            "real",
            &[],
            &[],
            true,
            None,
            StorageMode::Hybrid,
            PayloadExtras::default(),
        ).unwrap(),
    );
    assert_eq!(m["TRIPLEC_AGENT_TOKEN"], "real");
    // The task definition's fixed environment owns these two names.
    assert!(!m.contains_key("TRIPLEC_HOST"));
    assert!(!m.contains_key("COLORTERM"));
    // The rest of the TRIPLEC_ namespace is the entrypoint's too.
    assert!(!m.contains_key("TRIPLEC_AGENT_ADDR"));
    assert!(!m.contains_key("triplec_other"));
    assert_eq!(m["OK"], "1");
}

#[test]
fn payload_over_64_kib_fails() {
    let env = pairs(&[("BIG", &"x".repeat(65_536))]);
    let err =
        project_secret_payload_with(
            &env,
            "t",
            &[],
            &[],
            true,
            None,
            StorageMode::Hybrid,
            PayloadExtras::default(),
        ).unwrap_err();
    assert!(err.contains("65,536"), "{err}");
}

#[test]
fn payload_carries_setup_script_and_lifetime_when_set() {
    let json = project_secret_payload_with(
        &[],
        "t",
        &[],
        &[],
        true,
        None,
        StorageMode::Hybrid,
        PayloadExtras { setup_script: Some("apt-get install -y jq\n"), max_workspace_hours: 8, ..Default::default() },
    )
    .unwrap();
    let m = payload_map(&json);
    assert_eq!(m["TRIPLEC_SETUP_SCRIPT"], "apt-get install -y jq\n");
    assert_eq!(m["TRIPLEC_MAX_LIFETIME_MINUTES"], "480");
}

#[test]
fn payload_omits_setup_script_and_lifetime_when_unset() {
    for script in [None, Some(""), Some("  \n\t")] {
        let json = project_secret_payload_with(
            &[],
            "t",
            &[],
            &[],
            true,
            None,
            StorageMode::Hybrid,
            PayloadExtras { setup_script: script, ..Default::default() },
        )
        .unwrap();
        let m = payload_map(&json);
        assert!(!m.contains_key("TRIPLEC_SETUP_SCRIPT"), "{script:?}");
        assert!(!m.contains_key("TRIPLEC_MAX_LIFETIME_MINUTES"));
    }
}

#[test]
fn payload_setup_script_and_lifetime_cannot_be_forged_by_custom_env() {
    let env = pairs(&[("TRIPLEC_SETUP_SCRIPT", "rm -rf /"), ("TRIPLEC_MAX_LIFETIME_MINUTES", "1")]);
    let m = payload_map(
        &project_secret_payload_with(
            &env,
            "t",
            &[],
            &[],
            true,
            None,
            StorageMode::Hybrid,
            PayloadExtras::default(),
        ).unwrap(),
    );
    assert!(!m.contains_key("TRIPLEC_SETUP_SCRIPT"));
    assert!(!m.contains_key("TRIPLEC_MAX_LIFETIME_MINUTES"));
}

#[test]
fn payload_size_error_names_the_setup_script_when_it_is_the_large_part() {
    let env = pairs(&[("CLAUDE_INSTRUCTIONS", &"i".repeat(20 * 1024)), ("SMALL", "x")]);
    let script = "s".repeat(31 * 1024);
    let files = vec![("ssh/id_rsa".to_string(), vec![b'k'; 12 * 1024])];
    let err = project_secret_payload_with(
        &env,
        "t",
        &files,
        &[],
        true,
        None,
        StorageMode::Hybrid,
        PayloadExtras { setup_script: Some(&script), ..Default::default() },
    )
    .unwrap_err();
    assert!(err.contains("65,536"), "{err}");
    // Largest first: the script, then the instructions, then the files.
    let at = |needle: &str| err.find(needle).unwrap_or_else(|| panic!("{needle} missing: {err}"));
    assert!(at("the setup script (32 KiB)") < at("the instructions (21 KiB)"), "{err}");
    assert!(at("the instructions") < at("the SSH and CA files (17 KiB)"), "{err}");
    assert!(!err.contains("SMALL"), "{err}");
    // Values never appear.
    assert!(!err.contains("sss"), "{err}");
}

#[test]
fn payload_size_error_names_a_large_custom_variable_by_name_only() {
    let env = pairs(&[("BIG_BLOB", &"SENTINEL".repeat(9000))]);
    let err =
        project_secret_payload_with(
            &env,
            "t",
            &[],
            &[],
            true,
            None,
            StorageMode::Hybrid,
            PayloadExtras::default(),
        ).unwrap_err();
    assert!(err.contains("the environment variable BIG_BLOB (71 KiB)"), "{err}");
    assert!(!err.contains("SENTINEL"), "{err}");
}

#[test]
fn agent_tokens_are_64_url_safe_characters_and_fresh() {
    let a = new_agent_token();
    let b = new_agent_token();
    assert_ne!(a, b);
    assert_eq!(a.len(), 64);
    assert!(a
        .bytes()
        .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_'));
}

// ------------------------------------------------------- prepare (fake API)

type Tags = Vec<(String, String)>;

#[derive(Default)]
struct Fake {
    file_system: Option<FileSystemRef>,
    access_points: Mutex<Vec<AccessPointRef>>,
    created_access_points: Mutex<Vec<(String, String, Tags)>>,
    secrets: Mutex<Vec<(String, String, String, Tags)>>,
    /// Newest first, like `ListTaskDefinitions` sorted DESC.
    revisions: Mutex<Vec<TaskDefinitionRef>>,
    registered: Mutex<Vec<RegisterTaskDefinitionInput>>,
    describes: Mutex<usize>,
    /// The secret's value before this start (`project_secret_value`).
    existing_secret: Mutex<Option<String>>,
    /// Names force-deleted.
    deleted_secrets: Mutex<Vec<String>>,
    /// Calls `project_secret_value` got.
    secret_reads: Mutex<usize>,
    /// Puts that fail as "scheduled for deletion" before one succeeds.
    pending_puts: Mutex<usize>,
    /// Reads that fail as "marked for deletion" before one succeeds.
    pending_reads: Mutex<usize>,
    /// Calls `create_project_secret` got.
    creates: Mutex<usize>,
}

impl Fake {
    fn onboarded() -> Self {
        Fake {
            file_system: Some(FileSystemRef {
                id: "fs-0123abcd".into(),
                owner: Some(USER.into()),
            }),
            ..Default::default()
        }
    }
    fn writes(&self) -> usize {
        self.created_access_points.lock().unwrap().len()
            + self.secrets.lock().unwrap().len()
            + self.registered.lock().unwrap().len()
    }
}

impl WorkspaceApi for Fake {
    async fn find_file_system(&self, token: &str) -> Result<Option<FileSystemRef>, AwsError> {
        let ok = owner_key(REGION, USER);
        assert_eq!(token, user_file_system_token("dev", &ok));
        Ok(self.file_system.clone())
    }
    async fn access_points(&self, fs_id: &str) -> Result<Vec<AccessPointRef>, AwsError> {
        Ok(self
            .access_points
            .lock()
            .unwrap()
            .iter()
            .filter(|a| a.file_system_id == fs_id)
            .cloned()
            .collect())
    }
    async fn create_access_point(
        &self,
        fs_id: &str,
        root: &str,
        tags: &[(String, String)],
    ) -> Result<String, AwsError> {
        let mut c = self.created_access_points.lock().unwrap();
        c.push((fs_id.into(), root.into(), tags.to_vec()));
        Ok(format!("fsap-new{}", c.len()))
    }
    async fn project_secret_value(&self, name: &str) -> Result<Option<String>, AwsError> {
        *self.secret_reads.lock().unwrap() += 1;
        {
            let mut pending = self.pending_reads.lock().unwrap();
            if *pending > 0 {
                *pending -= 1;
                return Err(pending_deletion_error(name));
            }
        }
        Ok(self.existing_secret.lock().unwrap().clone())
    }
    async fn delete_project_secret(&self, name: &str) -> Result<(), AwsError> {
        self.deleted_secrets.lock().unwrap().push(name.into());
        *self.existing_secret.lock().unwrap() = None;
        Ok(())
    }
    async fn create_project_secret(
        &self,
        name: &str,
        kms_key_arn: &str,
        json: &str,
        tags: &[(String, String)],
    ) -> Result<Option<String>, AwsError> {
        *self.creates.lock().unwrap() += 1;
        if self.existing_secret.lock().unwrap().is_some() {
            return Ok(None);
        }
        self.put_project_secret(name, kms_key_arn, json, tags).await.map(Some)
    }
    async fn put_project_secret(
        &self,
        name: &str,
        kms_key_arn: &str,
        json: &str,
        tags: &[(String, String)],
    ) -> Result<String, AwsError> {
        {
            let mut pending = self.pending_puts.lock().unwrap();
            if *pending > 0 {
                *pending -= 1;
                return Err(pending_deletion_error(name));
            }
        }
        self.secrets.lock().unwrap().push((
            name.into(),
            kms_key_arn.into(),
            json.into(),
            tags.to_vec(),
        ));
        Ok(SECRET_ARN.into())
    }
    async fn task_definition_revisions(&self, family: &str) -> Result<Vec<String>, AwsError> {
        Ok(self
            .revisions
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r.family == family)
            .map(|r| r.arn.clone())
            .collect())
    }
    async fn describe_task_definition(
        &self,
        arn: &str,
    ) -> Result<Option<TaskDefinitionRef>, AwsError> {
        *self.describes.lock().unwrap() += 1;
        Ok(self
            .revisions
            .lock()
            .unwrap()
            .iter()
            .find(|r| r.arn == arn)
            .cloned())
    }
    async fn register_task_definition(
        &self,
        req: RegisterTaskDefinitionInput,
    ) -> Result<String, AwsError> {
        let family = req.family().unwrap().to_string();
        let mut revs = self.revisions.lock().unwrap();
        let n = revs.iter().filter(|r| r.family == family).count() + 1;
        let arn = format!("arn:aws:ecs:{REGION}:{ACCOUNT}:task-definition/{family}:{n}");
        let tag = |k: &str| {
            req.tags()
                .iter()
                .find(|t| t.key() == Some(k))
                .and_then(|t| t.value())
                .map(String::from)
        };
        revs.insert(
            0,
            TaskDefinitionRef {
                arn: arn.clone(),
                family: family.clone(),
                owner: tag(TAG_OWNER),
                project: tag(TAG_PROJECT),
                fingerprint: req.container_definitions()[0]
                    .docker_labels()
                    .and_then(|l| l.get(FINGERPRINT_LABEL).cloned()),
            },
        );
        self.registered.lock().unwrap().push(req);
        Ok(arn)
    }
}

const IMAGE: &str = "ghcr.io/example/triple-c:latest";

async fn prepare(
    fake: &Fake,
    p: &Project,
    env: &[(String, String)],
) -> Result<PreparedWorkspace, AwsError> {
    prepare_workspace(fake, &who(), &env_info(), p, env, &[], &[], IMAGE, None, None).await
}

#[tokio::test]
async fn prepare_puts_the_setup_script_and_the_environments_cap_in_the_secret() {
    let fake = Fake::onboarded();
    let mut p = project(StorageMode::Ephemeral);
    p.aws.as_mut().unwrap().setup_script = Some("sudo apt-get install -y jq".into());
    let mut env = env_info();
    env.max_workspace_hours = 12;
    prepare_workspace(&fake, &who(), &env, &p, &[], &[], &[], IMAGE, None, None).await.unwrap();
    let m = payload_map(&fake.secrets.lock().unwrap()[0].2);
    assert_eq!(m["TRIPLEC_SETUP_SCRIPT"], "sudo apt-get install -y jq");
    assert_eq!(m["TRIPLEC_MAX_LIFETIME_MINUTES"], "720");

    // No script, no cap: neither key.
    let fake = Fake::onboarded();
    prepare(&fake, &project(StorageMode::Ephemeral), &[]).await.unwrap();
    let m = payload_map(&fake.secrets.lock().unwrap()[0].2);
    assert!(!m.contains_key("TRIPLEC_SETUP_SCRIPT"));
    assert!(!m.contains_key("TRIPLEC_MAX_LIFETIME_MINUTES"));
}

#[tokio::test]
async fn first_start_creates_everything_with_the_three_tags() {
    let fake = Fake::onboarded();
    let p = project(StorageMode::Hybrid);
    let env = pairs(&[
        ("GIT_TOKEN", "ghp_secret"),
        ("CLAUDE_INSTRUCTIONS", "be nice"),
    ]);
    let out = prepare(&fake, &p, &env).await.unwrap();
    let ok = owner_key(REGION, USER);
    let tags = workspace_tags(USER, PROJECT_ID, "dev");

    let aps = fake.created_access_points.lock().unwrap().clone();
    assert_eq!(
        aps,
        vec![(
            "fs-0123abcd".to_string(),
            access_point_root(&ok, PROJECT_ID),
            tags.clone()
        )]
    );
    assert_eq!(out.access_point_id.as_deref(), Some("fsap-new1"));

    let secrets = fake.secrets.lock().unwrap().clone();
    assert_eq!(secrets.len(), 1);
    assert_eq!(secrets[0].0, project_secret_name("dev", &ok, PROJECT_ID));
    assert_eq!(secrets[0].1, env_info().kms_key_arn);
    assert_eq!(secrets[0].3, tags);
    let m = payload_map(&secrets[0].2);
    assert_eq!(m["TRIPLEC_AGENT_TOKEN"], out.agent_token);
    assert_eq!(m["GIT_TOKEN"], "ghp_secret");
    assert_eq!(out.secret_arn, SECRET_ARN);

    let reg = fake.registered.lock().unwrap();
    assert_eq!(reg.len(), 1);
    let req = &reg[0];
    assert_eq!(
        req.family(),
        Some(task_family("dev", &ok, PROJECT_ID).as_str())
    );
    assert_eq!(
        req.execution_role_arn(),
        Some(format!("arn:aws:iam::{ACCOUNT}:role/triple-c-dev-{ok}-exec").as_str())
    );
    assert_eq!(
        req.task_role_arn(),
        Some(format!("arn:aws:iam::{ACCOUNT}:role/triple-c-dev-{ok}-task").as_str())
    );
    // No project data outside `secrets`: every payload value is absent from
    // the environment, labels and tags.
    // (`Debug` redacts `environment`, so its pairs are rendered by hand.)
    let env_text: Vec<String> = req.container_definitions()[0]
        .environment()
        .iter()
        .map(|kv| format!("{}={}", kv.name().unwrap(), kv.value().unwrap()))
        .collect();
    let rendered = format!(
        "{env_text:?}{:?}{:?}",
        req.container_definitions()[0].docker_labels(),
        req.tags()
    );
    for v in [
        "ghp_secret",
        "be nice",
        out.agent_token.as_str(),
        "github.com/example",
    ] {
        assert!(!rendered.contains(v), "{v} leaked outside secrets");
    }
    // Every payload key, and only those, is a secret reference.
    let secret_names: Vec<&str> = req.container_definitions()[0]
        .secrets()
        .iter()
        .map(|s| s.name())
        .collect();
    assert_eq!(
        secret_names,
        m.keys().map(String::as_str).collect::<Vec<_>>()
    );
    assert!(out.task_definition_arn.ends_with(":1"));
    assert_eq!(out.family, task_family("dev", &ok, PROJECT_ID));
}

#[tokio::test]
async fn unchanged_inputs_reuse_the_revision_and_a_changed_cpu_registers_one() {
    let fake = Fake::onboarded();
    let mut p = project(StorageMode::Hybrid);
    let env = pairs(&[("A", "1")]);
    let first = prepare(&fake, &p, &env).await.unwrap();
    // The created access point now exists for the second start.
    fake.access_points.lock().unwrap().push(AccessPointRef {
        id: first.access_point_id.clone().unwrap(),
        file_system_id: "fs-0123abcd".into(),
        root_path: Some(access_point_root(&owner_key(REGION, USER), PROJECT_ID)),
        owner: Some(USER.into()),
        project: Some(PROJECT_ID.into()),
        state: Some("available".into()),
    });
    let second = prepare(&fake, &p, &env).await.unwrap();
    assert_eq!(
        fake.registered.lock().unwrap().len(),
        1,
        "unchanged inputs re-registered"
    );
    assert_eq!(second.task_definition_arn, first.task_definition_arn);
    assert_ne!(
        second.agent_token, first.agent_token,
        "the agent token rotates every start"
    );
    assert_eq!(
        fake.secrets.lock().unwrap().len(),
        2,
        "the secret is rewritten every start"
    );

    p.aws.as_mut().unwrap().cpu = 2048;
    p.aws.as_mut().unwrap().memory_mib = 8192;
    let third = prepare(&fake, &p, &env).await.unwrap();
    assert_eq!(fake.registered.lock().unwrap().len(), 2);
    assert!(third.task_definition_arn.ends_with(":2"));
}

#[tokio::test]
async fn env_order_does_not_register_a_new_revision() {
    let fake = Fake::onboarded();
    let p = project(StorageMode::Ephemeral);
    prepare(&fake, &p, &pairs(&[("B", "2"), ("A", "1")]))
        .await
        .unwrap();
    prepare(&fake, &p, &pairs(&[("A", "1"), ("B", "2")]))
        .await
        .unwrap();
    assert_eq!(fake.registered.lock().unwrap().len(), 1);
    let reg = fake.registered.lock().unwrap();
    let names: Vec<&str> = reg[0].container_definitions()[0]
        .secrets()
        .iter()
        .map(|s| s.name())
        .collect();
    let mut sorted = names.clone();
    sorted.sort();
    assert_eq!(names, sorted);
}

#[tokio::test]
async fn a_foreign_newest_revision_is_never_used() {
    let fake = Fake::onboarded();
    let p = project(StorageMode::Ephemeral);
    let first = prepare(&fake, &p, &[]).await.unwrap();
    let own = fake.revisions.lock().unwrap()[0].clone();
    let ok = owner_key(REGION, USER);
    let family = task_family("dev", &ok, PROJECT_ID);
    // Another user registers a newer revision into this family, copying the
    // fingerprint label and claiming the project.
    fake.revisions.lock().unwrap().insert(
        0,
        TaskDefinitionRef {
            arn: format!("arn:aws:ecs:{REGION}:{ACCOUNT}:task-definition/{family}:2"),
            family: family.clone(),
            owner: Some(OTHER.into()),
            project: Some(PROJECT_ID.into()),
            fingerprint: own.fingerprint.clone(),
        },
    );
    let second = prepare(&fake, &p, &[]).await.unwrap();
    // The caller's own older revision, by its exact family:revision ARN.
    assert_eq!(second.task_definition_arn, first.task_definition_arn);
    assert!(second.task_definition_arn.ends_with(&format!("{family}:1")));
    assert_eq!(fake.registered.lock().unwrap().len(), 1);

    // With only foreign revisions, the caller registers its own.
    let fake = Fake::onboarded();
    fake.revisions.lock().unwrap().push(TaskDefinitionRef {
        owner: Some(OTHER.into()),
        arn: format!("arn:aws:ecs:{REGION}:{ACCOUNT}:task-definition/{family}:1"),
        ..own.clone()
    });
    let out = prepare(&fake, &p, &[]).await.unwrap();
    assert_eq!(fake.registered.lock().unwrap().len(), 1);
    assert!(out.task_definition_arn.ends_with(&format!("{family}:2")));
}

#[tokio::test]
async fn a_revision_for_another_project_or_family_is_not_reused() {
    let fake = Fake::onboarded();
    let p = project(StorageMode::Ephemeral);
    prepare(&fake, &p, &[]).await.unwrap();
    // Same owner and fingerprint label, but another project's tag.
    fake.revisions.lock().unwrap()[0].project = Some("someone-else".into());
    prepare(&fake, &p, &[]).await.unwrap();
    assert_eq!(fake.registered.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn an_existing_access_point_is_reused() {
    let fake = Fake::onboarded();
    let ok = owner_key(REGION, USER);
    let root = access_point_root(&ok, PROJECT_ID);
    let good = AccessPointRef {
        id: "fsap-existing".into(),
        file_system_id: "fs-0123abcd".into(),
        root_path: Some(root.clone()),
        owner: Some(USER.into()),
        project: Some(PROJECT_ID.into()),
        state: Some("available".into()),
    };
    fake.access_points.lock().unwrap().extend([
        // Decoys: wrong owner, wrong root, being deleted.
        AccessPointRef {
            id: "fsap-foreign".into(),
            owner: Some(OTHER.into()),
            ..good.clone()
        },
        AccessPointRef {
            id: "fsap-elsewhere".into(),
            root_path: Some("/".into()),
            ..good.clone()
        },
        AccessPointRef {
            id: "fsap-deleting".into(),
            state: Some("deleting".into()),
            ..good.clone()
        },
        good.clone(),
    ]);
    let out = prepare(&fake, &project(StorageMode::Hybrid), &[])
        .await
        .unwrap();
    assert_eq!(out.access_point_id.as_deref(), Some("fsap-existing"));
    assert!(fake.created_access_points.lock().unwrap().is_empty());
    {
        let reg = fake.registered.lock().unwrap();
        let efs = reg[0].volumes()[0].efs_volume_configuration().unwrap();
        assert_eq!(
            efs.authorization_config().unwrap().access_point_id(),
            Some("fsap-existing")
        );
    }

    // Only decoys: a new one is created.
    let fake2 = Fake::onboarded();
    fake2.access_points.lock().unwrap().push(AccessPointRef {
        id: "fsap-foreign".into(),
        owner: Some(OTHER.into()),
        ..good
    });
    prepare(&fake2, &project(StorageMode::Hybrid), &[])
        .await
        .unwrap();
    assert_eq!(fake2.created_access_points.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn efs_creates_both_access_points_then_reuses_them_by_root_owner_and_project() {
    let fake = Fake::onboarded();
    let ok = owner_key(REGION, USER);
    let tags = workspace_tags(USER, PROJECT_ID, "dev");
    let p = project(StorageMode::Efs);
    let first = prepare(&fake, &p, &[]).await.unwrap();
    let created = fake.created_access_points.lock().unwrap().clone();
    assert_eq!(
        created,
        vec![
            ("fs-0123abcd".to_string(), access_point_root(&ok, PROJECT_ID), tags.clone()),
            (
                "fs-0123abcd".to_string(),
                workspace_access_point_root(&ok, PROJECT_ID),
                tags.clone()
            ),
        ]
    );
    assert_eq!(first.access_point_id.as_deref(), Some("fsap-new1"));
    assert_eq!(first.workspace_access_point_id.as_deref(), Some("fsap-new2"));
    {
        let reg = fake.registered.lock().unwrap();
        let aps: Vec<Option<&str>> = reg[0]
            .volumes()
            .iter()
            .map(|v| {
                v.efs_volume_configuration()
                    .unwrap()
                    .authorization_config()
                    .unwrap()
                    .access_point_id()
            })
            .collect();
        assert_eq!(aps, vec![Some("fsap-new1"), Some("fsap-new2")]);
        let secrets = fake.secrets.lock().unwrap();
        assert_eq!(payload_map(&secrets[0].2)["TRIPLEC_STORAGE"], "efs");
    }

    // The second start finds both (and is not fooled by decoys: another
    // owner's, another project's, the home root, one being deleted).
    let good = AccessPointRef {
        id: "fsap-ws".into(),
        file_system_id: "fs-0123abcd".into(),
        root_path: Some(workspace_access_point_root(&ok, PROJECT_ID)),
        owner: Some(USER.into()),
        project: Some(PROJECT_ID.into()),
        state: Some("available".into()),
    };
    fake.access_points.lock().unwrap().extend([
        AccessPointRef { id: "fsap-foreign".into(), owner: Some(OTHER.into()), ..good.clone() },
        AccessPointRef {
            id: "fsap-other-project".into(),
            project: Some("another".into()),
            ..good.clone()
        },
        AccessPointRef { id: "fsap-deleting".into(), state: Some("deleting".into()), ..good.clone() },
        AccessPointRef {
            id: "fsap-home".into(),
            root_path: Some(access_point_root(&ok, PROJECT_ID)),
            ..good.clone()
        },
        good,
    ]);
    let second = prepare(&fake, &p, &[]).await.unwrap();
    assert_eq!(fake.created_access_points.lock().unwrap().len(), 2);
    assert_eq!(second.access_point_id.as_deref(), Some("fsap-home"));
    assert_eq!(second.workspace_access_point_id.as_deref(), Some("fsap-ws"));
}

#[tokio::test]
async fn hybrid_never_creates_or_returns_a_workspace_access_point() {
    let fake = Fake::onboarded();
    let out = prepare(&fake, &project(StorageMode::Hybrid), &[]).await.unwrap();
    assert!(out.workspace_access_point_id.is_none());
    assert_eq!(fake.created_access_points.lock().unwrap().len(), 1);
    assert_eq!(fake.registered.lock().unwrap()[0].volumes().len(), 1);
}

#[tokio::test]
async fn efs_without_the_users_file_system_fails_before_any_write() {
    let fake = Fake::default();
    let err = prepare(&fake, &project(StorageMode::Efs), &[]).await.unwrap_err();
    assert!(err.to_string().contains("EFS storage"), "{err}");
    assert_eq!(fake.writes(), 0);
}

#[tokio::test]
async fn ephemeral_touches_no_file_system() {
    let fake = Fake::default(); // no file system at all
    let out = prepare(&fake, &project(StorageMode::Ephemeral), &[])
        .await
        .unwrap();
    assert!(out.access_point_id.is_none());
    assert!(fake.created_access_points.lock().unwrap().is_empty());
    assert!(fake.registered.lock().unwrap()[0].volumes().is_empty());
}

#[tokio::test]
async fn an_oversized_payload_fails_before_any_write() {
    let fake = Fake::onboarded();
    let env = pairs(&[("BIG", &"x".repeat(70_000))]);
    let err = prepare(&fake, &project(StorageMode::Hybrid), &env)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("65,536"), "{err}");
    assert_eq!(fake.writes(), 0);
}

#[tokio::test]
async fn hybrid_without_the_users_file_system_fails_before_any_write() {
    let fake = Fake::default();
    let err = prepare(&fake, &project(StorageMode::Hybrid), &[])
        .await
        .unwrap_err();
    assert!(err.to_string().contains("file system"), "{err}");
    assert_eq!(fake.writes(), 0);

    let mut foreign = Fake::onboarded();
    foreign.file_system.as_mut().unwrap().owner = Some(OTHER.into());
    let err = prepare(&foreign, &project(StorageMode::Hybrid), &[])
        .await
        .unwrap_err();
    assert!(err.to_string().contains("file system"), "{err}");
    assert_eq!(foreign.writes(), 0);
}

#[tokio::test]
async fn refuses_a_project_that_is_not_placed_in_this_environment() {
    let fake = Fake::onboarded();
    let mut local = project(StorageMode::Hybrid);
    local.host_id = None;
    local.aws = None;
    local.aws_placement = None;
    assert!(prepare(&fake, &local, &[]).await.is_err());

    let mut elsewhere = project(StorageMode::Hybrid);
    elsewhere.aws_placement.as_mut().unwrap().environment = "prod".into();
    assert!(prepare(&fake, &elsewhere, &[]).await.is_err());

    let mut region = project(StorageMode::Hybrid);
    region.aws_placement.as_mut().unwrap().region = "us-east-1".into();
    assert!(prepare(&fake, &region, &[]).await.is_err());
    assert_eq!(fake.writes(), 0);
}

#[tokio::test]
async fn files_travel_in_the_secret_only() {
    let fake = Fake::onboarded();
    let files = vec![("ssh/id_ed25519".to_string(), b"PRIVATE KEY".to_vec())];
    prepare_workspace(
        &fake,
        &who(),
        &env_info(),
        &project(StorageMode::Hybrid),
        &[],
        &files,
        &[],
        IMAGE,
        None,
        None,
    )
    .await
    .unwrap();
    let m = payload_map(&fake.secrets.lock().unwrap()[0].2);
    assert!(m["TRIPLEC_FILES"].contains("ssh/id_ed25519"));
}

#[tokio::test]
async fn revision_lookup_is_bounded() {
    let fake = Fake::onboarded();
    let ok = owner_key(REGION, USER);
    let family = task_family("dev", &ok, PROJECT_ID);
    for n in (1..=50).rev() {
        fake.revisions.lock().unwrap().push(TaskDefinitionRef {
            arn: format!("arn:aws:ecs:{REGION}:{ACCOUNT}:task-definition/{family}:{n}"),
            family: family.clone(),
            owner: Some(OTHER.into()),
            project: Some(PROJECT_ID.into()),
            fingerprint: None,
        });
    }
    prepare(&fake, &project(StorageMode::Ephemeral), &[])
        .await
        .unwrap();
    assert!(*fake.describes.lock().unwrap() <= MAX_REVISIONS_CHECKED);
    assert_eq!(fake.registered.lock().unwrap().len(), 1);
}

// ------------------------------------------------------- fix round 1

#[test]
fn ids_that_sanitise_alike_get_different_names() {
    let ok = owner_key(REGION, USER);
    for (a, b) in [("a/b", "a:b"), ("", "_"), ("x y", "x*y")] {
        assert_ne!(
            project_secret_name("dev", &ok, a),
            project_secret_name("dev", &ok, b)
        );
        assert_ne!(access_point_root(&ok, a), access_point_root(&ok, b));
        assert_ne!(log_stream_prefix(&ok, a), log_stream_prefix(&ok, b));
        assert_ne!(task_family("dev", &ok, a), task_family("dev", &ok, b));
    }
    // A sanitised id can't take a clean id's name: clean ids have no `_`.
    let hashed = project_secret_name("dev", &ok, "a/b");
    assert!(hashed.rsplit('/').next().unwrap().contains('_'), "{hashed}");
    // Ids that differ only past 64 characters still differ.
    let long_a = format!("{}a", "x".repeat(70));
    let long_b = format!("{}b", "x".repeat(70));
    assert_ne!(
        project_secret_name("dev", &ok, &long_a),
        project_secret_name("dev", &ok, &long_b)
    );
    assert!(task_family("dev", &ok, "a/b").len() <= 255);
    // A UUID passes unchanged.
    assert!(project_secret_name("dev", &ok, PROJECT_ID).ends_with(PROJECT_ID));
}

#[test]
fn the_fingerprint_follows_the_built_request_and_not_its_label() {
    let plan = sample_plan(StorageMode::Hybrid);
    let req = task_definition_request(&plan);
    let label = req.container_definitions()[0]
        .docker_labels()
        .unwrap()
        .get(FINGERPRINT_LABEL)
        .unwrap()
        .clone();
    // The label is the fingerprint of the request itself, label excluded.
    assert_eq!(request_fingerprint(&req), label);
    assert_eq!(workspace_fingerprint(&plan), label);

    // Any change the builder could make shows: the health check, a log
    // option, the (Debug-redacted) environment, a mount.
    type Edit = Box<dyn Fn(&mut RegisterTaskDefinitionInput)>;
    let edits: Vec<Edit> = vec![
        Box::new(|r| {
            let c = &mut r.container_definitions.as_mut().unwrap()[0];
            c.health_check.as_mut().unwrap().interval = Some(31);
        }),
        Box::new(|r| {
            let c = &mut r.container_definitions.as_mut().unwrap()[0];
            let opts = c
                .log_configuration
                .as_mut()
                .unwrap()
                .options
                .as_mut()
                .unwrap();
            opts.insert("mode".into(), "non-blocking".into());
        }),
        Box::new(|r| {
            let c = &mut r.container_definitions.as_mut().unwrap()[0];
            c.environment = Some(vec![aws_sdk_ecs::types::KeyValuePair::builder()
                .name("TRIPLEC_HOST")
                .value("aws2")
                .build()]);
        }),
        Box::new(|r| {
            let c = &mut r.container_definitions.as_mut().unwrap()[0];
            c.mount_points.as_mut().unwrap()[0].read_only = Some(true);
        }),
        Box::new(|r| r.cpu = Some("2048".into())),
    ];
    for (i, edit) in edits.iter().enumerate() {
        let mut changed = req.clone();
        edit(&mut changed);
        assert_ne!(
            request_fingerprint(&changed),
            label,
            "edit {i} kept the fingerprint"
        );
    }
    // Another label value does not change it.
    let mut relabelled = req.clone();
    relabelled.container_definitions.as_mut().unwrap()[0].docker_labels =
        Some([(FINGERPRINT_LABEL.to_string(), "other".to_string())].into());
    assert_eq!(request_fingerprint(&relabelled), label);
}

#[test]
fn the_fingerprint_is_stable_across_builds() {
    // Each build makes fresh HashMaps (log options), each with its own
    // random iteration order.
    let plan = sample_plan(StorageMode::Hybrid);
    let first = workspace_fingerprint(&plan);
    for _ in 0..50 {
        assert_eq!(workspace_fingerprint(&plan), first);
    }
}

#[test]
fn a_secret_pending_deletion_is_recognised() {
    assert!(is_pending_deletion(Some(
        "You can't create this secret because a secret with this name is already scheduled for deletion."
    )));
    assert!(!is_pending_deletion(Some("Some other invalid request")));
    assert!(!is_pending_deletion(None));
    let msg = pending_deletion_error("triple-c/dev/abc/p").to_string();
    assert!(msg.contains("still being deleted") && msg.contains("again in a minute"), "{msg}");
    assert!(!msg.contains("Restore"), "{msg}");
    assert!(is_pending_deletion_error(&pending_deletion_error("n")));
    assert!(msg.contains("triple-c/dev/abc/p"), "{msg}");
}


// ------------------------------------------------- copy-at-connect (Q2)

#[test]
fn a_copy_at_connect_payload_keeps_settings_and_the_token_but_no_credentials() {
    let env = pairs(&[
        ("ANTHROPIC_API_KEY", "sk-ant"),
        ("GIT_TOKEN", "ghp_secret"),
        ("AWS_SECRET_ACCESS_KEY", "aws-secret"),
        ("MY_CUSTOM", "custom-value"),
        ("ANTHROPIC_MODEL", "opus"),
        ("CLAUDE_INSTRUCTIONS", "be nice"),
        ("GIT_USER_NAME", "Dev"),
    ]);
    let files = vec![
        ("ssh/id_ed25519".to_string(), b"PRIVATE KEY".to_vec()),
        ("ca/corp.pem".to_string(), b"PEM".to_vec()),
    ];
    let repos = project(StorageMode::Hybrid).aws.unwrap().repos;
    let custom = vec!["MY_CUSTOM".to_string()];
    let json = project_secret_payload_with(
        &env,
        "tok",
        &files,
        &repos,
        true,
        Some(30),
        StorageMode::Hybrid,
        PayloadExtras {
            setup_script: Some("make"),
            connect_only: Some(&custom),
            ..Default::default()
        },
    )
    .unwrap();
    for value in ["sk-ant", "ghp_secret", "aws-secret", "custom-value", "PRIVATE KEY", "UFJJVkFURSBLRVk="] {
        assert!(!json.contains(value), "{value} must not be stored: {json}");
    }
    let m = payload_map(&json);
    assert!(!m.contains_key("TRIPLEC_FILES"));
    for key in crate::docker::container::SECRET_ENV_KEYS.iter().chain(["MY_CUSTOM"].iter()) {
        assert!(!m.contains_key(*key), "{key}");
    }
    assert_eq!(m["TRIPLEC_CREDENTIALS"], "connect");
    // Settings and the agent token stay.
    assert_eq!(m["TRIPLEC_AGENT_TOKEN"], "tok");
    assert_eq!(m["ANTHROPIC_MODEL"], "opus");
    assert_eq!(m["CLAUDE_INSTRUCTIONS"], "be nice");
    assert_eq!(m["GIT_USER_NAME"], "Dev");
    assert_eq!(m["TRIPLEC_SETUP_SCRIPT"], "make");
    assert_eq!(m["TRIPLEC_IDLE_STOP_MINUTES"], "30");
    assert!(m["TRIPLEC_REPOS"].contains("\"folder\""));

    // Secrets Manager delivery: all of it, and no TRIPLEC_CREDENTIALS.
    let m = payload_map(
        &project_secret_payload_with(
            &env,
            "tok",
            &files,
            &repos,
            true,
            Some(30),
            StorageMode::Hybrid,
            PayloadExtras::default(),
        )
        .unwrap(),
    );
    assert_eq!(m["GIT_TOKEN"], "ghp_secret");
    assert_eq!(m["MY_CUSTOM"], "custom-value");
    assert!(m["TRIPLEC_FILES"].contains("ssh/id_ed25519"));
    assert!(!m.contains_key("TRIPLEC_CREDENTIALS"));
}

#[test]
fn credential_pairs_are_the_managed_keys_and_every_custom_variable() {
    let env = pairs(&[
        ("ANTHROPIC_API_KEY", "sk"),
        ("TZ", "UTC"),
        ("MY_CUSTOM", "v"),
        ("GIT_TOKEN", ""),
        ("CLAUDE_CODE_OAUTH_TOKEN", "o"),
    ]);
    let got = credential_pairs(&env, &["MY_CUSTOM".to_string()]);
    assert_eq!(
        got,
        pairs(&[("ANTHROPIC_API_KEY", "sk"), ("MY_CUSTOM", "v"), ("GIT_TOKEN", ""), ("CLAUDE_CODE_OAUTH_TOKEN", "o")])
    );
    assert!(!is_credential_key("TZ", &[]));
}

#[tokio::test]
async fn prepare_leaves_the_credentials_out_of_a_copy_at_connect_secret() {
    let fake = Fake::onboarded();
    let mut p = project(StorageMode::Hybrid);
    p.aws.as_mut().unwrap().secrets_delivery = crate::models::project::SecretsDelivery::CopyAtConnect;
    let files = vec![("ssh/id_ed25519".to_string(), b"PRIVATE KEY".to_vec())];
    let env = pairs(&[("GIT_TOKEN", "ghp_secret"), ("MINE", "x"), ("TZ", "UTC")]);
    prepare_workspace(&fake, &who(), &env_info(), &p, &env, &files, &["MINE".to_string()], IMAGE, None, None)
        .await
        .unwrap();
    let m = payload_map(&fake.secrets.lock().unwrap()[0].2);
    assert!(!m.contains_key("GIT_TOKEN") && !m.contains_key("MINE") && !m.contains_key("TRIPLEC_FILES"));
    assert_eq!(m["TZ"], "UTC");
    assert_eq!(m["TRIPLEC_CREDENTIALS"], "connect");
    assert!(m.contains_key("TRIPLEC_AGENT_TOKEN"));
}

fn copy_at_connect_project() -> Project {
    let mut p = project(StorageMode::Hybrid);
    p.aws.as_mut().unwrap().secrets_delivery = crate::models::project::SecretsDelivery::CopyAtConnect;
    p
}

#[tokio::test]
async fn switching_to_copy_at_connect_replaces_a_secret_that_held_credentials() {
    // The secret from a Secrets Manager period: replaced, not overwritten.
    let fake = Fake::onboarded();
    *fake.existing_secret.lock().unwrap() =
        Some(r#"{"GIT_TOKEN":"ghp_old","TRIPLEC_AGENT_TOKEN":"t","TZ":"UTC"}"#.into());
    *fake.pending_puts.lock().unwrap() = 2; // AWS still finishing the delete
    let env = pairs(&[("GIT_TOKEN", "ghp_new"), ("TZ", "UTC")]);
    prepare_workspace(&fake, &who(), &env_info(), &copy_at_connect_project(), &env, &[], &[], IMAGE, None, None)
        .await
        .unwrap();
    assert_eq!(*fake.secret_reads.lock().unwrap(), 1, "read only once it exists");
    assert_eq!(fake.deleted_secrets.lock().unwrap().len(), 1);
    let m = payload_map(&fake.secrets.lock().unwrap()[0].2);
    assert!(!m.contains_key("GIT_TOKEN"));

    // Already clean: a plain put, nothing deleted.
    let fake = Fake::onboarded();
    *fake.existing_secret.lock().unwrap() = Some(
        r#"{"TRIPLEC_AGENT_TOKEN":"t","TRIPLEC_CREDENTIALS":"connect","TZ":"UTC","TRIPLEC_REPOS":"[]"}"#.into(),
    );
    prepare_workspace(&fake, &who(), &env_info(), &copy_at_connect_project(), &env, &[], &[], IMAGE, None, None)
        .await
        .unwrap();
    assert!(fake.deleted_secrets.lock().unwrap().is_empty());
    assert_eq!(fake.secrets.lock().unwrap().len(), 1);

    // No secret yet (a first start): created, never read — a read of a
    // secret that doesn't exist may be refused — nothing deleted.
    let fake = Fake::onboarded();
    prepare_workspace(&fake, &who(), &env_info(), &copy_at_connect_project(), &env, &[], &[], IMAGE, None, None)
        .await
        .unwrap();
    assert_eq!(*fake.creates.lock().unwrap(), 1);
    assert_eq!(*fake.secret_reads.lock().unwrap(), 0);
    assert!(fake.deleted_secrets.lock().unwrap().is_empty());
    assert!(!payload_map(&fake.secrets.lock().unwrap()[0].2).contains_key("GIT_TOKEN"));

    // Secrets Manager never reads or deletes.
    let fake = Fake::onboarded();
    *fake.existing_secret.lock().unwrap() = Some(r#"{"GIT_TOKEN":"x"}"#.into());
    prepare(&fake, &project(StorageMode::Hybrid), &env).await.unwrap();
    assert_eq!(*fake.secret_reads.lock().unwrap(), 0);
    assert!(fake.deleted_secrets.lock().unwrap().is_empty());
}

#[test]
fn what_counts_as_credentials_left_in_a_secret() {
    let new: BTreeMap<String, String> =
        [("TZ", "UTC"), ("TRIPLEC_AGENT_TOKEN", "t")].iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
    let custom = vec!["MINE".to_string()];
    let holds = |old: &str| holds_credentials(old, &new, &custom);
    assert!(holds(r#"{"TRIPLEC_FILES":"{}"}"#));
    assert!(holds(r#"{"TRIPLE_C_CUSTOM_ENV":"abc"}"#));
    assert!(holds(r#"{"ANTHROPIC_API_KEY":""}"#));
    assert!(holds(r#"{"MINE":"v"}"#));
    assert!(holds(r#"{"GONE_CUSTOM":"v","TZ":"UTC"}"#), "a variable the payload no longer has");
    assert!(holds("not json"));
    assert!(!holds(r#"{"TZ":"UTC","TRIPLEC_AGENT_TOKEN":"old","TRIPLEC_SETUP_SCRIPT":"x"}"#));
}

#[tokio::test]
async fn a_replaced_secret_that_aws_has_not_finished_deleting_is_a_clear_error() {
    let fake = Fake::onboarded();
    *fake.pending_puts.lock().unwrap() = 100;
    let timing = RecreateTiming {
        interval: std::time::Duration::from_millis(1),
        limit: std::time::Duration::from_millis(20),
    };
    let e = replace_project_secret(&fake, "n", "k", "{}", &[], timing).await.unwrap_err();
    assert!(e.to_string().contains("has not finished deleting"), "{e}");
    assert!(e.to_string().contains("Start the project again in a minute"), "{e}");
    assert!(!e.to_string().contains("Restore"), "{e}");
    assert_eq!(fake.deleted_secrets.lock().unwrap().as_slice(), ["n"]);
}

#[tokio::test(start_paused = true)]
async fn the_start_after_a_timed_out_replace_retries_the_create() {
    // The replace deleted the secret but timed out before making it again:
    // the next start's create meets the deletion AWS is still finishing.
    let env = pairs(&[("TZ", "UTC")]);
    let fake = Fake::onboarded();
    *fake.pending_puts.lock().unwrap() = 3;
    prepare_workspace(&fake, &who(), &env_info(), &copy_at_connect_project(), &env, &[], &[], IMAGE, None, None)
        .await
        .unwrap();
    assert_eq!(fake.secrets.lock().unwrap().len(), 1, "made once the name is free");
    assert!(fake.deleted_secrets.lock().unwrap().is_empty());

    // Secrets Manager delivery retries the same way.
    let fake = Fake::onboarded();
    *fake.pending_puts.lock().unwrap() = 3;
    prepare(&fake, &project(StorageMode::Hybrid), &env).await.unwrap();
    assert_eq!(fake.secrets.lock().unwrap().len(), 1);

    // Still taken past the limit: try again in a minute, never "restore".
    let fake = Fake::onboarded();
    *fake.pending_puts.lock().unwrap() = 1000;
    let e = prepare_workspace(&fake, &who(), &env_info(), &copy_at_connect_project(), &env, &[], &[], IMAGE, None, None)
        .await
        .unwrap_err()
        .to_string();
    assert!(e.contains("Start the project again in a minute"), "{e}");
    assert!(!e.contains("Restore"), "{e}");
}

#[tokio::test(start_paused = true)]
async fn a_copy_at_connect_read_of_a_secret_being_deleted_is_retried() {
    // CreateSecret said it exists, then the read meets a deletion AWS is
    // still finishing (another start replacing it): retried like the writes.
    let env = pairs(&[("TZ", "UTC")]);
    let fake = Fake::onboarded();
    *fake.existing_secret.lock().unwrap() = Some(
        r#"{"TRIPLEC_AGENT_TOKEN":"t","TRIPLEC_CREDENTIALS":"connect","TZ":"UTC","TRIPLEC_REPOS":"[]"}"#.into(),
    );
    *fake.pending_reads.lock().unwrap() = 3;
    prepare_workspace(&fake, &who(), &env_info(), &copy_at_connect_project(), &env, &[], &[], IMAGE, None, None)
        .await
        .unwrap();
    assert_eq!(*fake.secret_reads.lock().unwrap(), 4);
    assert_eq!(fake.secrets.lock().unwrap().len(), 1);

    // Still being deleted past the limit: try again in a minute.
    let fake = Fake::onboarded();
    *fake.existing_secret.lock().unwrap() = Some(r#"{"TZ":"UTC"}"#.into());
    *fake.pending_reads.lock().unwrap() = 1000;
    let e = prepare_workspace(&fake, &who(), &env_info(), &copy_at_connect_project(), &env, &[], &[], IMAGE, None, None)
        .await
        .unwrap_err()
        .to_string();
    assert!(e.contains("Start the project again in a minute"), "{e}");
    assert!(fake.secrets.lock().unwrap().is_empty());
}

#[test]
fn a_copy_at_connect_payload_drops_the_custom_variables_fingerprint() {
    let env = pairs(&[("TRIPLE_C_CUSTOM_ENV", "63ce86"), ("TZ", "UTC")]);
    let custom: Vec<String> = Vec::new();
    let with = |connect_only| {
        payload_map(
            &project_secret_payload_with(
                &env,
                "tok",
                &[],
                &[],
                true,
                None,
                StorageMode::Hybrid,
                PayloadExtras { connect_only, ..Default::default() },
            )
            .unwrap(),
        )
    };
    assert!(!with(Some(&custom)).contains_key("TRIPLE_C_CUSTOM_ENV"));
    assert_eq!(with(None)["TRIPLE_C_CUSTOM_ENV"], "63ce86");
}

// ------------------------------------------------------- gateway sidecar (Q7)

const GW_CONFIG: &str = "model_list: []\ngeneral_settings:\n  master_key: \"os.environ/TRIPLEC_GATEWAY_MASTER_KEY\"\n";

fn gw() -> GatewayPayload<'static> {
    GatewayPayload { config: GW_CONFIG, api_key: "sk-provider", master_key: "mk-sidecar" }
}

fn sidecar_plan(storage: StorageMode) -> WorkspacePlan {
    let mut p = sample_plan(storage);
    p.secret_keys = vec![
        "ANTHROPIC_AUTH_TOKEN".into(),
        "ANTHROPIC_BASE_URL".into(),
        "GIT_TOKEN".into(),
        "TRIPLEC_AGENT_TOKEN".into(),
        "TRIPLEC_GATEWAY_API_KEY".into(),
        "TRIPLEC_GATEWAY_CONFIG".into(),
        "TRIPLEC_GATEWAY_MASTER_KEY".into(),
    ];
    p.gateway_image = Some(crate::docker::gateway::registry_image_ref());
    p
}

fn secret_names(c: &aws_sdk_ecs::types::ContainerDefinition) -> Vec<String> {
    c.secrets().iter().map(|s| s.name().to_string()).collect()
}

#[test]
fn a_sidecar_request_has_a_non_essential_gateway_with_only_its_three_keys() {
    let plan = sidecar_plan(StorageMode::Hybrid);
    let req = task_definition_request(&plan);
    let defs = req.container_definitions();
    assert_eq!(defs.len(), 2);
    let (ws, gw) = (&defs[0], &defs[1]);
    assert_eq!(ws.name(), Some("workspace"));
    assert_eq!(ws.essential(), Some(true));
    // The workspace gets everything but the sidecar's keys.
    assert_eq!(
        secret_names(ws),
        ["ANTHROPIC_AUTH_TOKEN", "ANTHROPIC_BASE_URL", "GIT_TOKEN", "TRIPLEC_AGENT_TOKEN"]
    );

    assert_eq!(gw.name(), Some("gateway"));
    assert_eq!(gw.essential(), Some(false));
    assert_eq!(
        gw.image(),
        Some("ghcr.io/berriai/litellm@sha256:37e2dafb088a7723beb998590682957386dfc83ffea1848486f8e0e5cfac0b06")
    );
    assert_eq!(gw.memory_reservation(), Some(512));
    assert_eq!(
        secret_names(gw),
        ["TRIPLEC_GATEWAY_API_KEY", "TRIPLEC_GATEWAY_CONFIG", "TRIPLEC_GATEWAY_MASTER_KEY"]
    );
    for s in gw.secrets() {
        assert_eq!(s.value_from(), format!("{SECRET_ARN}:{}::", s.name()));
    }
    // P1: no project data in the definition — no environment at all.
    assert!(gw.environment().is_empty());
    // Nothing waits on the gateway and it waits on nothing.
    assert!(gw.depends_on().is_empty() && ws.depends_on().is_empty());
    assert!(gw.health_check().is_none());
    // The command: config to a root-only file, LiteLLM on loopback 4000.
    assert_eq!(gw.entry_point(), ["/bin/sh", "-c"]);
    assert_eq!(gw.command(), [GATEWAY_COMMAND]);
    assert!(GATEWAY_COMMAND.contains("--host 127.0.0.1 --port 4000"));
    assert!(GATEWAY_COMMAND.contains("> /tmp/litellm.yaml"));
    assert!(GATEWAY_COMMAND.contains("--config /tmp/litellm.yaml"));
    assert!(!GATEWAY_COMMAND.contains("0.0.0.0"));
    assert_eq!(
        GATEWAY_SIDECAR_URL,
        format!("http://127.0.0.1:{}", crate::host::port_forwards::GATEWAY_SIDECAR_PORT)
    );
    // Same log group and prefix: awslogs names its streams
    // `<prefix>/gateway/<task-id>`, apart from the workspace's.
    let opts = gw.log_configuration().unwrap().options().unwrap();
    assert_eq!(opts["awslogs-group"], plan.log_group);
    assert_eq!(opts["awslogs-region"], plan.region);
    assert_eq!(opts["awslogs-stream-prefix"], plan.log_stream_prefix);
    // The fingerprint label rides on the workspace container, where
    // `describe_task_definition` reads it.
    assert!(ws.docker_labels().unwrap().contains_key(FINGERPRINT_LABEL));
}

#[test]
fn the_sidecar_changes_the_fingerprint_and_off_leaves_requests_as_they_were() {
    for storage in [StorageMode::Ephemeral, StorageMode::Hybrid, StorageMode::Efs] {
        let off = sample_plan(storage);
        assert_eq!(task_definition_request(&off).container_definitions().len(), 1);
        let mut on = off.clone();
        on.gateway_image = Some("img@sha256:x".into());
        assert_ne!(workspace_fingerprint(&on), workspace_fingerprint(&off));
        let mut other = on.clone();
        other.gateway_image = Some("img@sha256:y".into());
        assert_ne!(workspace_fingerprint(&on), workspace_fingerprint(&other));
    }
    // (The pinned Hybrid/Ephemeral values are in
    // `hybrid_and_ephemeral_requests_are_unchanged_by_efs_mode`.)
}

#[test]
fn a_sidecar_payload_points_claude_code_at_it_and_carries_its_keys() {
    let env = pairs(&[
        ("ANTHROPIC_BASE_URL", "http://host.docker.internal:4000"),
        ("ANTHROPIC_AUTH_TOKEN", "sk-local"),
        ("ANTHROPIC_MODEL", "gpt-5"),
        ("GIT_TOKEN", "ghp"),
    ]);
    let repos = project(StorageMode::Hybrid).aws.unwrap().repos;
    for connect in [false, true] {
        let custom: Vec<String> = Vec::new();
        let m = payload_map(
            &project_secret_payload_with(
                &env,
                "tok",
                &[],
                &repos,
                true,
                None,
                StorageMode::Hybrid,
                PayloadExtras {
                    connect_only: connect.then_some(&custom[..]),
                    gateway: Some(gw()),
                    ..Default::default()
                },
            )
            .unwrap(),
        );
        assert_eq!(m["ANTHROPIC_BASE_URL"], "http://127.0.0.1:4000");
        // R1: the sidecar's token and keys stay, copy-at-connect or not.
        assert_eq!(m["ANTHROPIC_AUTH_TOKEN"], "mk-sidecar", "connect={connect}");
        assert_eq!(m["TRIPLEC_GATEWAY_MASTER_KEY"], "mk-sidecar");
        assert_eq!(m["TRIPLEC_GATEWAY_API_KEY"], "sk-provider");
        assert_eq!(m["TRIPLEC_GATEWAY_CONFIG"], GW_CONFIG);
        assert_eq!(m["ANTHROPIC_MODEL"], "gpt-5");
        // Other credentials still follow the delivery.
        assert_eq!(m.contains_key("GIT_TOKEN"), !connect);
    }
    // Off: the env pairs as they are, no gateway key.
    let m = payload_map(
        &project_secret_payload_with(&env, "tok", &[], &repos, true, None, StorageMode::Hybrid, PayloadExtras::default())
            .unwrap(),
    );
    assert_eq!(m["ANTHROPIC_BASE_URL"], "http://host.docker.internal:4000");
    assert_eq!(m["ANTHROPIC_AUTH_TOKEN"], "sk-local");
    assert!(!m.keys().any(|k| k.starts_with("TRIPLEC_GATEWAY_")));
}

#[test]
fn custom_env_cannot_forge_the_gateway_keys() {
    let env = pairs(&[("TRIPLEC_GATEWAY_CONFIG", "evil"), ("TRIPLEC_GATEWAY_MASTER_KEY", "x")]);
    let m = payload_map(
        &project_secret_payload_with(&env, "t", &[], &[], true, None, StorageMode::Ephemeral, PayloadExtras::default())
            .unwrap(),
    );
    assert!(!m.keys().any(|k| k.starts_with("TRIPLEC_GATEWAY_")));
}

#[test]
fn a_gateway_payloads_debug_shows_no_key() {
    let shown = format!("{:?}", PayloadExtras { gateway: Some(gw()), ..Default::default() });
    assert!(!shown.contains("sk-provider") && !shown.contains("mk-sidecar"), "{shown}");
}

fn sidecar_project() -> Project {
    let mut p = project(StorageMode::Ephemeral);
    p.backend = crate::models::project::Backend::OpenAiCompatible;
    p.aws.as_mut().unwrap().gateway_sidecar = true;
    p
}

#[tokio::test]
async fn prepare_registers_the_sidecar_only_when_the_project_runs_it() {
    let fake = Fake::onboarded();
    let p = sidecar_project();
    prepare_workspace(&fake, &who(), &env_info(), &p, &[], &[], &[], IMAGE, Some(gw()), None)
        .await
        .unwrap();
    assert_eq!(fake.registered.lock().unwrap()[0].container_definitions().len(), 2);
    let m = payload_map(&fake.secrets.lock().unwrap()[0].2);
    assert_eq!(m["TRIPLEC_GATEWAY_API_KEY"], "sk-provider");

    // Toggle off: a payload handed in anyway is ignored.
    let fake = Fake::onboarded();
    prepare_workspace(&fake, &who(), &env_info(), &project(StorageMode::Ephemeral), &[], &[], &[], IMAGE, Some(gw()), None)
        .await
        .unwrap();
    assert_eq!(fake.registered.lock().unwrap()[0].container_definitions().len(), 1);
    let m = payload_map(&fake.secrets.lock().unwrap()[0].2);
    assert!(!m.keys().any(|k| k.starts_with("TRIPLEC_GATEWAY_")));
}

#[tokio::test]
async fn prepare_refuses_a_sidecar_without_its_payload_or_with_an_oversized_config() {
    let fake = Fake::onboarded();
    let p = sidecar_project();
    let e = prepare_workspace(&fake, &who(), &env_info(), &p, &[], &[], &[], IMAGE, None, None)
        .await
        .unwrap_err();
    assert!(e.to_string().contains("gateway"), "{e}");
    let big = "x".repeat(MAX_GATEWAY_CONFIG_BYTES + 1);
    let e = prepare_workspace(
        &fake,
        &who(),
        &env_info(),
        &p,
        &[],
        &[],
        &[],
        IMAGE,
        Some(GatewayPayload { config: &big, ..gw() }),
        None,
    )
    .await
    .unwrap_err();
    assert!(e.to_string().contains("gateway configuration"), "{e}");
    let e = prepare_workspace(
        &fake,
        &who(),
        &env_info(),
        &p,
        &[],
        &[],
        &[],
        IMAGE,
        Some(GatewayPayload { api_key: " ", ..gw() }),
        None,
    )
    .await
    .unwrap_err();
    assert!(e.to_string().contains("API key"), "{e}");
    assert_eq!(fake.writes(), 0);
}

#[test]
fn the_sidecar_needs_the_openai_backend_and_a_gateway_model() {
    use crate::models::gateway_settings::{GatewayModel, GatewaySettings};
    let models = GatewaySettings {
        models: vec![GatewayModel { name: "gpt".into(), model_id: "gpt-5".into() }],
        ..Default::default()
    };
    let p = sidecar_project();
    assert_eq!(gateway_sidecar_problem(&p, &models), None);
    assert!(gateway_sidecar_problem(&p, &GatewaySettings::default())
        .unwrap()
        .contains("Settings → Model Gateway"));
    let mut anthropic = p.clone();
    anthropic.backend = crate::models::project::Backend::Anthropic;
    assert!(gateway_sidecar_problem(&anthropic, &models).unwrap().contains("OpenAI Compatible"));
    // Off: nothing to say, whatever the backend.
    anthropic.aws.as_mut().unwrap().gateway_sidecar = false;
    assert_eq!(gateway_sidecar_problem(&anthropic, &GatewaySettings::default()), None);
}

#[test]
fn the_sidecars_token_is_not_a_leftover_credential_but_a_dropped_provider_key_is() {
    let new: BTreeMap<String, String> = [
        ("ANTHROPIC_AUTH_TOKEN", "mk-new"),
        ("TRIPLEC_GATEWAY_MASTER_KEY", "mk-new"),
        ("TRIPLEC_GATEWAY_API_KEY", "sk"),
        ("TRIPLEC_AGENT_TOKEN", "t"),
    ]
    .iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect();
    let holds = |old: &str, new: &BTreeMap<String, String>| holds_credentials(old, new, &[]);
    // Last start's sidecar key (a different random one): not a credential.
    let sidecar_old = r#"{"ANTHROPIC_AUTH_TOKEN":"mk-old","TRIPLEC_GATEWAY_MASTER_KEY":"mk-old","TRIPLEC_GATEWAY_API_KEY":"sk","TRIPLEC_AGENT_TOKEN":"t"}"#;
    assert!(!holds(sidecar_old, &new));
    // A user's own token from a Secrets Manager period is.
    assert!(holds(r#"{"ANTHROPIC_AUTH_TOKEN":"sk-user","TRIPLEC_AGENT_TOKEN":"t"}"#, &new));
    // Sidecar turned off: the provider key left in the secret counts.
    let off: BTreeMap<String, String> =
        [("TRIPLEC_AGENT_TOKEN", "t")].iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
    assert!(holds(sidecar_old, &off));
}

// ------------------------------------------------------------- EC2 (phase 7)

fn ec2_sample(storage: StorageMode, docker_socket: bool, vpn: bool) -> WorkspacePlan {
    WorkspacePlan { ec2: Some(Ec2Plan { docker_socket, vpn }), ..sample_plan(storage) }
}

fn compute(instance_type: &str) -> UserCompute {
    UserCompute {
        instance_type: instance_type.into(),
        max_instances: 2,
        user_tag: "alice".into(),
        cluster_arn: format!("arn:aws:ecs:{REGION}:{ACCOUNT}:cluster/triple-c-dev--u-alice"),
        capacity_provider: "triple-c-dev--u-alice".into(),
    }
}

fn ec2_project(storage: StorageMode, cpu: u32, memory_mib: u32) -> Project {
    let mut p = project(storage);
    let a = p.aws.as_mut().unwrap();
    a.launch_type = crate::models::LaunchType::Ec2;
    a.cpu = cpu;
    a.memory_mib = memory_mib;
    p
}

#[test]
fn an_ec2_request_is_bridge_mode_with_the_reservations_and_no_fargate_fields() {
    for storage in [StorageMode::Ephemeral, StorageMode::Hybrid, StorageMode::Efs] {
        let plan = ec2_sample(storage, false, false);
        let req = task_definition_request(&plan);
        assert_eq!(req.requires_compatibilities(), &[aws_sdk_ecs::types::Compatibility::Ec2]);
        assert_eq!(req.network_mode(), Some(&aws_sdk_ecs::types::NetworkMode::Bridge));
        assert_eq!((req.cpu(), req.memory()), (Some("1024"), Some("4096")));
        assert!(req.runtime_platform().is_none(), "the instance's architecture decides");
        assert!(req.ephemeral_storage().is_none(), "the instance's disk");
        // Everything else as on Fargate: roles, tags, health check, stop
        // timeout, init, logs, the EFS volumes and the fingerprint label.
        let fargate = task_definition_request(&sample_plan(storage));
        assert_eq!(req.task_role_arn(), fargate.task_role_arn());
        assert_eq!(req.execution_role_arn(), fargate.execution_role_arn());
        assert_eq!(req.tags(), fargate.tags());
        assert_eq!(req.volumes(), fargate.volumes());
        let (c, f) = (&req.container_definitions()[0], &fargate.container_definitions()[0]);
        assert_eq!(c.health_check(), f.health_check());
        assert_eq!(c.stop_timeout(), Some(STOP_TIMEOUT_SECS));
        assert_eq!(c.mount_points(), f.mount_points());
        assert_eq!(c.secrets(), f.secrets());
        assert_eq!(c.environment(), f.environment());
        assert_eq!(c.log_configuration(), f.log_configuration());
        assert_eq!(c.linux_parameters(), f.linux_parameters(), "SYS_PTRACE and init only");
        assert!(c.system_controls().is_empty());
        assert!(c.port_mappings().is_empty());
        assert_eq!(
            c.docker_labels().unwrap().get(FINGERPRINT_LABEL),
            Some(&workspace_fingerprint(&plan))
        );
        assert_ne!(workspace_fingerprint(&plan), workspace_fingerprint(&sample_plan(storage)));
    }
}

#[test]
fn an_ec2_request_for_each_toggle_combination() {
    let mut fingerprints = std::collections::BTreeSet::new();
    for (docker, vpn) in [(false, false), (true, false), (false, true), (true, true)] {
        let plan = ec2_sample(StorageMode::Hybrid, docker, vpn);
        let req = task_definition_request(&plan);
        fingerprints.insert(workspace_fingerprint(&plan));
        let c = &req.container_definitions()[0];
        let lp = c.linux_parameters().unwrap();
        assert_eq!(lp.init_process_enabled(), Some(true));

        // E6: the instance's Docker socket at the same path.
        let socket_volume: Vec<_> =
            req.volumes().iter().filter(|v| v.name() == Some("docker-socket")).collect();
        let socket_mount: Vec<_> = c
            .mount_points()
            .iter()
            .filter(|m| m.container_path() == Some(DOCKER_SOCKET_PATH))
            .collect();
        if docker {
            assert_eq!(socket_volume.len(), 1);
            assert_eq!(
                socket_volume[0].host().and_then(|h| h.source_path()),
                Some("/var/run/docker.sock")
            );
            assert!(socket_volume[0].efs_volume_configuration().is_none());
            assert_eq!(socket_mount.len(), 1);
            assert_eq!(socket_mount[0].source_volume(), Some("docker-socket"));
        } else {
            assert!(socket_volume.is_empty() && socket_mount.is_empty());
        }
        // The home access point is mounted either way.
        assert!(c.mount_points().iter().any(|m| m.container_path() == Some(HOME_MOUNT_PATH)));

        // E7: the three VPN pieces, all or none, plus SYS_PTRACE as always.
        let caps = lp.capabilities().unwrap().add();
        let sysctls: Vec<(&str, &str)> = c
            .system_controls()
            .iter()
            .map(|s| (s.namespace().unwrap(), s.value().unwrap()))
            .collect();
        if vpn {
            assert_eq!(caps, ["SYS_PTRACE".to_string(), "NET_ADMIN".to_string()]);
            assert_eq!(lp.devices().len(), 1);
            let d = &lp.devices()[0];
            assert_eq!((d.host_path(), d.container_path()), ("/dev/net/tun", Some("/dev/net/tun")));
            use aws_sdk_ecs::types::DeviceCgroupPermission as P;
            assert_eq!(d.permissions(), [P::Read, P::Write, P::Mknod]);
            assert_eq!(sysctls, [("net.ipv4.conf.all.src_valid_mark", "1")]);
        } else {
            assert_eq!(caps, ["SYS_PTRACE".to_string()]);
            assert!(lp.devices().is_empty() && sysctls.is_empty());
        }
    }
    assert_eq!(fingerprints.len(), 4, "every combination is its own revision");
}

/// The VPN pieces are Docker's own (`vpn_host_config`), not a second list.
#[test]
fn the_ec2_vpn_pieces_are_the_docker_ones() {
    let vpn = crate::docker::container::vpn_requirements();
    assert_eq!(vpn.capabilities, ["NET_ADMIN"]);
    assert_eq!(
        vpn.devices,
        [("/dev/net/tun".to_string(), "/dev/net/tun".to_string(), "rwm".to_string())]
    );
    assert_eq!(vpn.sysctls, [("net.ipv4.conf.all.src_valid_mark".to_string(), "1".to_string())]);
}

#[test]
fn ec2_plan_checks_the_record_and_the_fit_and_carries_the_toggles() {
    // Fargate: nothing, whatever the record says.
    let fargate = project(StorageMode::Hybrid);
    assert_eq!(ec2_plan(&fargate, "dev", None), Ok(None));
    assert_eq!(ec2_plan(&fargate, "dev", Some(&compute("m7i.large"))), Ok(None));

    // No record: the onboarding has no EC2 capacity (or it was removed).
    let mut p = ec2_project(StorageMode::Hybrid, 1024, 4096);
    let e = ec2_plan(&p, "dev", None).unwrap_err();
    assert_eq!(
        e,
        "You have no EC2 capacity in dev; ask an admin to add it to your onboarding (or run the \
         project on Fargate)."
    );

    // Fits: the toggles ride along.
    p.allow_docker_access = true;
    assert_eq!(
        ec2_plan(&p, "dev", Some(&compute("m7i.large"))),
        Ok(Some(Ec2Plan { docker_socket: true, vpn: false }))
    );
    p.vpn_support_enabled = true;
    assert_eq!(
        ec2_plan(&p, "dev", Some(&compute("m7i.large"))),
        Ok(Some(Ec2Plan { docker_socket: true, vpn: true }))
    );

    // Too big: names what the instance has for a workspace (10 % memory
    // margin: ECS registers less than the nominal 8 GiB).
    let big = ec2_project(StorageMode::Hybrid, 2048, 8192);
    let e = ec2_plan(&big, "dev", Some(&compute("m7i.large"))).unwrap_err();
    assert!(e.contains("m7i.large"), "{e}");
    assert!(e.contains("1920 CPU units, 7373 MiB"), "{e}");
    assert!(ec2_plan(&big, "dev", Some(&compute("m7i.xlarge"))).is_ok());

    // The gateway sidecar is not offered on EC2 (bridge networking).
    let mut gw_on = ec2_project(StorageMode::Hybrid, 1024, 4096);
    gw_on.aws.as_mut().unwrap().gateway_sidecar = true;
    assert_eq!(ec2_plan(&gw_on, "dev", Some(&compute("m7i.large"))).unwrap_err(), EC2_NO_SIDECAR);
}

#[test]
fn the_sidecar_is_refused_on_ec2() {
    use crate::models::gateway_settings::{GatewayModel, GatewaySettings};
    let models = GatewaySettings {
        models: vec![GatewayModel { name: "gpt".into(), model_id: "gpt-5".into() }],
        ..Default::default()
    };
    let mut p = ec2_project(StorageMode::Hybrid, 1024, 4096);
    p.backend = crate::models::project::Backend::OpenAiCompatible;
    p.aws.as_mut().unwrap().gateway_sidecar = true;
    assert_eq!(gateway_sidecar_problem(&p, &models).as_deref(), Some(EC2_NO_SIDECAR));
    p.aws.as_mut().unwrap().gateway_sidecar = false;
    assert_eq!(gateway_sidecar_problem(&p, &models), None);
}

#[tokio::test]
async fn an_ec2_project_registers_an_ec2_revision_on_the_users_instance() {
    let fake = Fake::onboarded();
    let mut p = ec2_project(StorageMode::Hybrid, 1024, 4096);
    p.allow_docker_access = true;
    let c = compute("m7i.large");
    prepare_workspace(&fake, &who(), &env_info(), &p, &[], &[], &[], IMAGE, None, Some(&c))
        .await
        .unwrap();
    let reg = fake.registered.lock().unwrap();
    assert_eq!(reg.len(), 1);
    assert_eq!(reg[0].requires_compatibilities(), &[aws_sdk_ecs::types::Compatibility::Ec2]);
    assert_eq!(reg[0].network_mode(), Some(&aws_sdk_ecs::types::NetworkMode::Bridge));
    assert!(reg[0].volumes().iter().any(|v| v.name() == Some("docker-socket")));
}

#[tokio::test]
async fn an_ec2_project_without_capacity_or_room_fails_before_any_write() {
    let p = ec2_project(StorageMode::Hybrid, 1024, 4096);
    let fake = Fake::onboarded();
    let e = prepare_workspace(&fake, &who(), &env_info(), &p, &[], &[], &[], IMAGE, None, None)
        .await
        .unwrap_err();
    assert!(e.to_string().contains("no EC2 capacity in dev"), "{e}");
    let big = ec2_project(StorageMode::Hybrid, 2048, 8192);
    let c = compute("m7i.large");
    let e = prepare_workspace(&fake, &who(), &env_info(), &big, &[], &[], &[], IMAGE, None, Some(&c))
        .await
        .unwrap_err();
    assert!(e.to_string().contains("more than your EC2 instance type"), "{e}");
    assert_eq!(fake.writes(), 0);
}
