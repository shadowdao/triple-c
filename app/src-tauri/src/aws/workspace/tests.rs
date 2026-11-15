use std::collections::BTreeMap;
use std::sync::Mutex;

use aws_sdk_ecs::operation::register_task_definition::RegisterTaskDefinitionInput;
use aws_sdk_ecs::types::{CpuArchitecture, EfsAuthorizationConfigIam, EfsTransitEncryption};

use super::*;
use crate::aws::environments::EnvironmentInfo;
use crate::aws::identity::CallerIdentity;
use crate::aws::tofu_modules::{
    access_point_root, log_stream_prefix, owner_key, project_secret_name, task_family,
    user_file_system_token,
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
        file_system_id: (storage == StorageMode::Hybrid).then(|| "fs-0123abcd".to_string()),
        access_point_id: (storage == StorageMode::Hybrid).then(|| "fsap-0123abcd".to_string()),
        tags: workspace_tags(USER, PROJECT_ID, "dev"),
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
        let secret = project_secret_name("dev", &ok, id);
        assert!(!secret.contains(':') && !secret.contains(".."), "{secret}");
        assert_eq!(secret.matches('/').count(), 3, "{secret}");
    }
    let long = "x".repeat(500);
    assert!(task_family("abcdefghijklmnopqrst", &ok, &long).len() <= 255);
    assert!(access_point_root(&ok, &long).len() <= 100);
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
    let json = project_secret_payload(
        &env,
        "tok",
        &files,
        &repos,
        true,
        Some(30),
        StorageMode::Hybrid,
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
        project_secret_payload(&[], "t", &[], &[], false, None, StorageMode::Ephemeral).unwrap();
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
    let pa = project_secret_payload(&a, "t", &[], &[], true, None, StorageMode::Hybrid).unwrap();
    let pb = project_secret_payload(&b, "t", &[], &[], true, None, StorageMode::Hybrid).unwrap();
    assert_eq!(pa, pb);
}

#[test]
fn payload_rejects_bad_file_names() {
    for name in [
        "ssh/", "ssh/.", "ssh/..", "ssh/a/b", "other/x", "x", "ca/../k", "ssh/a:b",
    ] {
        let files = vec![(name.to_string(), b"x".to_vec())];
        let r = project_secret_payload(&[], "t", &files, &[], true, None, StorageMode::Hybrid);
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
        let r = project_secret_payload(&env, "t", &[], &[], true, None, StorageMode::Hybrid);
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
        ("COLORTERM", "16"),
        ("OK", "1"),
    ]);
    let m = payload_map(
        &project_secret_payload(&env, "real", &[], &[], true, None, StorageMode::Hybrid).unwrap(),
    );
    assert_eq!(m["TRIPLEC_AGENT_TOKEN"], "real");
    // The task definition's fixed environment owns these two names.
    assert!(!m.contains_key("TRIPLEC_HOST"));
    assert!(!m.contains_key("COLORTERM"));
    assert_eq!(m["OK"], "1");
}

#[test]
fn payload_over_64_kib_fails() {
    let env = pairs(&[("BIG", &"x".repeat(65_536))]);
    let err =
        project_secret_payload(&env, "t", &[], &[], true, None, StorageMode::Hybrid).unwrap_err();
    assert!(err.contains("65,536"), "{err}");
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
    async fn put_project_secret(
        &self,
        name: &str,
        kms_key_arn: &str,
        json: &str,
        tags: &[(String, String)],
    ) -> Result<String, AwsError> {
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
    prepare_workspace(fake, &who(), &env_info(), p, env, &[], IMAGE).await
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
        IMAGE,
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
    assert!(msg.contains("scheduled for deletion"), "{msg}");
    assert!(msg.contains("triple-c/dev/abc/p"), "{msg}");
}
