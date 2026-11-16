use std::collections::HashMap;
use std::sync::atomic::AtomicUsize;
use std::sync::Mutex;

use aws_sdk_ecs::operation::register_task_definition::RegisterTaskDefinitionInput;
use aws_sdk_ecs::operation::run_task::RunTaskInput;

use super::*;
use crate::aws::environments::Knowable;
use crate::aws::tasks::TaskInfo;
use crate::aws::workspace::{AccessPointRef, FileSystemRef, TaskDefinitionRef};
use crate::aws::AwsError;
use crate::host::lifecycle::ProjectHost;
use crate::host::tunnels::tests::{fake_tunnel, FakeTunnel};
use crate::models::{AwsPlacement, AwsWorkspaceConfig, StorageMode};

const REGION: &str = "eu-west-1";
const ACCOUNT: &str = "111122223333";
const USER: &str = "AROAEXAMPLEID:alice@example.com";

fn who() -> CallerIdentity {
    CallerIdentity {
        account: ACCOUNT.into(),
        arn: format!("arn:aws:sts::{ACCOUNT}:assumed-role/dev/alice"),
        user_id: USER.into(),
    }
}

fn aws_project(storage: StorageMode) -> Project {
    let mut p = Project::new("aws".into(), Vec::new());
    p.host_id = Some("c1".into());
    p.aws_placement = Some(AwsPlacement { region: REGION.into(), environment: "dev".into() });
    p.aws = Some(AwsWorkspaceConfig { storage, ..Default::default() });
    p
}

fn family(p: &Project) -> String {
    task_family("dev", &owner_key(REGION, USER), &p.id)
}

fn task_arn(id: &str) -> String {
    format!("arn:aws:ecs:{REGION}:{ACCOUNT}:task/triple-c-dev/{id}")
}

fn cluster_arn() -> String {
    format!("arn:aws:ecs:{REGION}:{ACCOUNT}:cluster/triple-c-dev")
}

fn task(p: &Project, id: &str, last: &str, ready: bool) -> TaskInfo {
    TaskInfo {
        arn: task_arn(id),
        last_status: last.into(),
        desired_status: if last == "STOPPED" { "STOPPED" } else { "RUNNING" }.into(),
        health: None,
        stop_code: (last == "STOPPED").then(|| "EssentialContainerExited".into()),
        stopped_reason: (last == "STOPPED").then(|| "Essential container exited".into()),
        task_definition_arn: format!("arn:aws:ecs:{REGION}:{ACCOUNT}:task-definition/{}:1", family(p)),
        runtime_id: ready.then(|| "rt-1".into()),
        exec_agent_running: ready,
        exit_code: None,
        owner: Some(USER.into()),
        project: Some(p.id.clone()),
    }
}

fn record() -> String {
    serde_json::json!({
        "schema": 1, "name": "dev", "template_version": 1,
        "cluster_arn": cluster_arn(),
        "security_group_id": "sg-0123456789abcdef0",
        "subnet_ids": ["subnet-0a", "subnet-0b"],
        "assign_public_ip": true,
        "kms_key_arn": format!("arn:aws:kms:{REGION}:{ACCOUNT}:key/1234-abcd"),
        "log_group": "/triple-c/dev", "network_mode": "public", "tenancy": "strict"
    })
    .to_string()
}

/// One fake for all three AWS traits and the connector, logging calls.
#[derive(Default)]
struct Fake {
    calls: Mutex<Vec<String>>,
    /// ListTasks answers in turn; the last repeats.
    listed: Mutex<Vec<Vec<String>>>,
    tasks: Mutex<HashMap<String, TaskInfo>>,
    /// What RunTask starts.
    run: Option<TaskInfo>,
    logs: Vec<String>,
    role: Option<Knowable>,
    connect_fails: bool,
    connect_tokens: Mutex<Vec<String>>,
    secret_json: Mutex<Option<String>>,
    /// A losing start rotates the secret's token while this one connects.
    rotated_by_loser: bool,
    /// StopTask is refused (expired credentials, say).
    stop_fails: bool,
    /// What `on_task` was told, in order.
    recorded: Mutex<Vec<Option<String>>>,
    /// `group stream limit` of each log read.
    log_reads: Mutex<Vec<String>>,
}

impl Fake {
    fn log(&self, s: impl Into<String>) {
        self.calls.lock().unwrap().push(s.into());
    }
    fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }
    fn called(&self, prefix: &str) -> bool {
        self.calls().iter().any(|c| c.starts_with(prefix))
    }
    fn with_task(self, t: TaskInfo) -> Self {
        self.tasks.lock().unwrap().insert(t.arn.clone(), t);
        self
    }
}

impl TaskApi for Fake {
    async fn run_task(&self, _req: RunTaskInput) -> Result<TaskInfo, AwsError> {
        self.log("run");
        let t = self.run.clone().expect("a run was expected");
        self.tasks.lock().unwrap().insert(t.arn.clone(), t.clone());
        Ok(t)
    }
    async fn describe(&self, _c: &str, arns: &[String]) -> Result<Vec<TaskInfo>, AwsError> {
        let t = self.tasks.lock().unwrap();
        Ok(arns.iter().filter_map(|a| t.get(a).cloned()).collect())
    }
    async fn list_project_tasks(&self, _c: &str, _f: &str) -> Result<Vec<String>, AwsError> {
        self.log("list");
        let mut l = self.listed.lock().unwrap();
        Ok(if l.len() > 1 { l.remove(0) } else { l.first().cloned().unwrap_or_default() })
    }
    async fn stop_task(&self, _c: &str, arn: &str, reason: &str) -> Result<(), AwsError> {
        self.log(format!("stop {arn} {reason}"));
        if self.stop_fails {
            return Err(AwsError::Other { message: "ExpiredToken".into() });
        }
        Ok(())
    }
    async fn log_tail(&self, g: &str, st: &str, l: i32) -> Result<Vec<String>, AwsError> {
        self.log_reads.lock().unwrap().push(format!("{g} {st} {l}"));
        Ok(self.logs.clone())
    }
}

impl WorkspaceApi for Fake {
    async fn find_file_system(&self, _t: &str) -> Result<Option<FileSystemRef>, AwsError> {
        Ok(Some(FileSystemRef { id: "fs-1".into(), owner: Some(USER.into()) }))
    }
    async fn access_points(&self, _fs: &str) -> Result<Vec<AccessPointRef>, AwsError> {
        Ok(Vec::new())
    }
    async fn create_access_point(
        &self,
        _fs: &str,
        root: &str,
        _tags: &[(String, String)],
    ) -> Result<String, AwsError> {
        self.log("create-ap");
        Ok(if root.starts_with("/triple-c-workspaces/") { "fsap-ws" } else { "fsap-new" }.into())
    }
    async fn put_project_secret(
        &self,
        _name: &str,
        _kms: &str,
        json: &str,
        _tags: &[(String, String)],
    ) -> Result<String, AwsError> {
        self.log("put-secret");
        *self.secret_json.lock().unwrap() = Some(json.into());
        Ok(format!("arn:aws:secretsmanager:{REGION}:{ACCOUNT}:secret:triple-c/dev/x-AbCdEf"))
    }
    async fn task_definition_revisions(&self, _f: &str) -> Result<Vec<String>, AwsError> {
        Ok(Vec::new())
    }
    async fn describe_task_definition(
        &self,
        _arn: &str,
    ) -> Result<Option<TaskDefinitionRef>, AwsError> {
        Ok(None)
    }
    async fn register_task_definition(
        &self,
        req: RegisterTaskDefinitionInput,
    ) -> Result<String, AwsError> {
        self.log("register");
        Ok(format!(
            "arn:aws:ecs:{REGION}:{ACCOUNT}:task-definition/{}:1",
            req.family().unwrap()
        ))
    }
}

impl RuntimeApi for Fake {
    async fn environment_record(&self, env: &str) -> Result<Option<String>, AwsError> {
        self.log(format!("record {env}"));
        Ok(Some(record()))
    }
    async fn role_exists(&self, _name: &str) -> Result<Knowable, AwsError> {
        self.log("role");
        Ok(self.role.clone().unwrap_or(Knowable::Yes))
    }
    async fn secret_string(&self, _name: &str) -> Result<String, AwsError> {
        self.log("get-secret");
        Ok(self
            .secret_json
            .lock()
            .unwrap()
            .clone()
            .unwrap_or_else(|| r#"{"TRIPLEC_AGENT_TOKEN":"token-of-the-running-task"}"#.into()))
    }
    async fn access_point_state(&self, id: &str) -> Result<Option<String>, AwsError> {
        self.log(format!("ap-state {id}"));
        Ok(Some("available".into()))
    }
    async fn delete_secret(&self, _name: &str) -> Result<(), AwsError> {
        Ok(())
    }
    async fn delete_access_point(&self, _id: &str) -> Result<(), AwsError> {
        Ok(())
    }
}

impl Connector for Fake {
    async fn connect(
        &self,
        _task: &TaskInfo,
        token: &str,
    ) -> Result<(Box<dyn TunnelHandle>, AgentEndpoint), String> {
        self.log("connect");
        self.connect_tokens.lock().unwrap().push(token.into());
        if self.rotated_by_loser {
            let mut j = self.secret_json.lock().unwrap();
            let mut v: serde_json::Value = serde_json::from_str(j.as_deref().unwrap()).unwrap();
            v["TRIPLEC_AGENT_TOKEN"] = "the-losers-token".into();
            *j = Some(v.to_string());
        }
        if self.connect_fails {
            return Err("session-manager-plugin was not found".into());
        }
        Ok((
            Box::new(FakeTunnel(std::sync::Arc::new(AtomicUsize::new(0)))),
            AgentEndpoint::loopback(61100, token),
        ))
    }
}

fn inputs<'a>(
    p: &'a Project,
    who: &'a CallerIdentity,
    on_task: &'a (dyn Fn(Option<&TaskRecord>) + Send + Sync),
) -> StartInputs<'a> {
    StartInputs {
        on_task,
        project: p,
        who,
        env_pairs: &[],
        files: &[],
        image: REGISTRY_IMAGE,
        wait: WaitTiming {
            interval: Duration::from_millis(1),
            timeout: Duration::from_millis(30),
        },
        access_point_wait: PollTiming {
            interval: Duration::from_millis(1),
            timeout: Duration::from_millis(30),
        },
    }
}

async fn start(fake: &Fake, p: &Project) -> (Result<Started, String>, Vec<String>) {
    let w = who();
    let steps = Mutex::new(Vec::new());
    let progress = |m: &str| steps.lock().unwrap().push(m.to_string());
    let on_task = |r: Option<&TaskRecord>| {
        fake.recorded.lock().unwrap().push(r.map(|r| r.task_arn.clone()));
    };
    let r = start_core(fake, fake, fake, fake, &inputs(p, &w, &on_task), &progress).await;
    (r, steps.into_inner().unwrap())
}

#[tokio::test]
async fn a_first_start_prepares_runs_waits_and_connects_in_that_order() {
    let p = aws_project(StorageMode::Hybrid);
    let fake = Fake { run: Some(task(&p, "new", "RUNNING", true)), ..Default::default() };
    let (r, steps) = start(&fake, &p).await;
    let started = r.unwrap();
    assert!(started.created);
    assert_eq!(started.access_point_id.as_deref(), Some("fsap-new"));
    assert_eq!(started.task.arn, task_arn("new"));
    assert_eq!(
        fake.calls(),
        vec![
            "record dev", "role", "list", "create-ap", "put-secret", "register",
            "ap-state fsap-new", "list", "run", "connect", "get-secret"
        ],
        "look for a live task before rotating the token; the access point is available before RunTask"
    );
    assert_eq!(steps, vec![PROGRESS_READING, PROGRESS_PREPARING, PROGRESS_STARTING, PROGRESS_WAITING, PROGRESS_CONNECTING]);
    assert_eq!(*fake.recorded.lock().unwrap(), vec![Some(task_arn("new"))], "recorded before the wait");
    // The connector got the token the secret was just written with.
    let secret: serde_json::Value =
        serde_json::from_str(fake.secret_json.lock().unwrap().as_deref().unwrap()).unwrap();
    assert_eq!(
        fake.connect_tokens.lock().unwrap()[0],
        secret["TRIPLEC_AGENT_TOKEN"].as_str().unwrap()
    );
}

#[tokio::test]
async fn an_efs_start_waits_for_both_access_points_and_records_them() {
    let p = aws_project(StorageMode::Efs);
    let fake = Fake { run: Some(task(&p, "new", "RUNNING", true)), ..Default::default() };
    let w = who();
    let records = Mutex::new(Vec::new());
    let on_task = |r: Option<&TaskRecord>| records.lock().unwrap().push(r.cloned());
    let started = start_core(&fake, &fake, &fake, &fake, &inputs(&p, &w, &on_task), &|_| {})
        .await
        .unwrap();
    assert_eq!(started.access_point_id.as_deref(), Some("fsap-new"));
    assert_eq!(started.workspace_access_point_id.as_deref(), Some("fsap-ws"));
    let calls = fake.calls();
    let pos = |c: &str| calls.iter().position(|x| x == c).unwrap_or_else(|| panic!("{c}: {calls:?}"));
    assert!(pos("ap-state fsap-new") < pos("run"), "{calls:?}");
    assert!(pos("ap-state fsap-ws") < pos("run"), "{calls:?}");
    let recs = records.into_inner().unwrap();
    let rec = recs[0].as_ref().unwrap();
    assert_eq!(rec.access_point_id.as_deref(), Some("fsap-new"));
    assert_eq!(rec.workspace_access_point_id.as_deref(), Some("fsap-ws"));

    // Hybrid waits for one and records none for /workspace.
    let p = aws_project(StorageMode::Hybrid);
    let fake = Fake { run: Some(task(&p, "new", "RUNNING", true)), ..Default::default() };
    let records = Mutex::new(Vec::new());
    let on_task = |r: Option<&TaskRecord>| records.lock().unwrap().push(r.cloned());
    let started = start_core(&fake, &fake, &fake, &fake, &inputs(&p, &w, &on_task), &|_| {})
        .await
        .unwrap();
    assert_eq!(started.workspace_access_point_id, None);
    assert!(!fake.called("ap-state fsap-ws"));
    assert_eq!(records.into_inner().unwrap()[0].as_ref().unwrap().workspace_access_point_id, None);
}

#[test]
fn switching_efs_to_hybrid_keeps_the_stored_workspace_access_point() {
    let mut p = aws_project(StorageMode::Hybrid);
    p.aws.as_mut().unwrap().workspace_access_point_id = Some("fsap-ws".into());
    let (_d, store) = store_with(&p);
    // A Hybrid start knows no /workspace access point: the stored one stays.
    record_task(
        &store,
        &p.id,
        Some(&TaskRecord {
            task_arn: "arn:t".into(),
            access_point_id: Some("fsap-home".into()),
            task_definition_arn: None,
            workspace_access_point_id: None,
        }),
    );
    let a = store.get(&p.id).unwrap().aws.unwrap();
    assert_eq!(a.workspace_access_point_id.as_deref(), Some("fsap-ws"));
    record_task(&store, &p.id, None);
    let a = store.get(&p.id).unwrap().aws.unwrap();
    assert_eq!(a.workspace_access_point_id.as_deref(), Some("fsap-ws"));
    // An EFS start records its own.
    record_task(
        &store,
        &p.id,
        Some(&TaskRecord {
            task_arn: "arn:t2".into(),
            access_point_id: None,
            task_definition_arn: None,
            workspace_access_point_id: Some("fsap-ws2".into()),
        }),
    );
    let a = store.get(&p.id).unwrap().aws.unwrap();
    assert_eq!(a.workspace_access_point_id.as_deref(), Some("fsap-ws2"));
}

#[tokio::test]
async fn a_running_task_is_attached_with_its_own_token_and_nothing_is_rewritten() {
    let p = aws_project(StorageMode::Hybrid);
    let live = task(&p, "live", "RUNNING", true);
    let fake = Fake {
        listed: Mutex::new(vec![vec![live.arn.clone()]]),
        ..Default::default()
    }
    .with_task(live);
    let (r, steps) = start(&fake, &p).await;
    let started = r.unwrap();
    assert!(!started.created);
    assert_eq!(started.access_point_id, None);
    assert_eq!(fake.calls(), vec!["record dev", "role", "list", "get-secret", "connect"]);
    assert_eq!(fake.connect_tokens.lock().unwrap()[0], "token-of-the-running-task");
    assert_eq!(steps, vec![PROGRESS_READING, PROGRESS_WAITING, PROGRESS_CONNECTING]);
}

#[tokio::test]
async fn losing_the_run_race_is_a_clear_error_and_nothing_is_stopped() {
    let p = aws_project(StorageMode::Ephemeral);
    let other = task(&p, "other", "RUNNING", true);
    let fake = Fake {
        // Nothing at the first look; a concurrent start's task at the second.
        listed: Mutex::new(vec![vec![], vec![other.arn.clone()]]),
        ..Default::default()
    }
    .with_task(other);
    let (r, _) = start(&fake, &p).await;
    assert_eq!(r.err().as_deref(), Some(START_RACE));
    assert_eq!(*fake.recorded.lock().unwrap(), vec![Some(task_arn("other"))], "the live task is recorded");
    assert!(!fake.called("run") && !fake.called("stop") && !fake.called("connect"));
}

#[tokio::test]
async fn a_task_that_stops_while_starting_is_reported_with_its_log() {
    let p = aws_project(StorageMode::Ephemeral);
    let fake = Fake {
        run: Some(task(&p, "dies", "STOPPED", false)),
        logs: vec!["entrypoint: cannot decode TRIPLEC_FILES".into()],
        ..Default::default()
    };
    let (r, _) = start(&fake, &p).await;
    let e = r.err().unwrap();
    assert!(e.contains("Essential container exited") && e.contains("cannot decode"), "{e}");
    assert!(!fake.called("connect"));
}

#[tokio::test]
async fn a_task_that_never_gets_ready_is_stopped_and_its_log_shown() {
    let p = aws_project(StorageMode::Ephemeral);
    let fake = Fake {
        run: Some(task(&p, "slow", "PENDING", false)),
        logs: vec!["pulling image".into()],
        ..Default::default()
    };
    let (r, _) = start(&fake, &p).await;
    let e = r.err().unwrap();
    assert!(e.contains("did not become ready") && e.contains("pulling image"), "{e}");
    assert!(fake.calls().contains(&format!("stop {} {STOP_REASON}", task_arn("slow"))));
    assert_eq!(
        *fake.recorded.lock().unwrap(),
        vec![Some(task_arn("slow")), None],
        "the ARN is stored before the wait, and forgotten once the task is stopped"
    );
}

#[tokio::test]
async fn a_tunnel_failure_stops_a_task_this_start_ran_but_never_an_attached_one() {
    let p = aws_project(StorageMode::Ephemeral);
    let fake = Fake {
        run: Some(task(&p, "new", "RUNNING", true)),
        connect_fails: true,
        ..Default::default()
    };
    let (r, _) = start(&fake, &p).await;
    assert!(r.err().unwrap().contains("session-manager-plugin"));
    assert!(fake.calls().contains(&format!("stop {} {STOP_REASON}", task_arn("new"))));

    let live = task(&p, "live", "RUNNING", true);
    let attached = Fake {
        listed: Mutex::new(vec![vec![live.arn.clone()]]),
        connect_fails: true,
        ..Default::default()
    }
    .with_task(live);
    let (r, _) = start(&attached, &p).await;
    assert!(r.is_err());
    assert!(!attached.called("stop"));
    assert_eq!(
        *attached.recorded.lock().unwrap(),
        vec![Some(task_arn("live"))],
        "an attached task stays recorded: it is still running"
    );
}

#[tokio::test]
async fn a_user_who_has_not_joined_is_told_before_anything_is_written() {
    let p = aws_project(StorageMode::Hybrid);
    let fake = Fake { role: Some(Knowable::No), ..Default::default() };
    let (r, _) = start(&fake, &p).await;
    assert!(r.err().unwrap().contains("not joined"));
    assert_eq!(fake.calls(), vec!["record dev", "role"]);
}

// ──────────────────────────────────────────────────────────────── inputs

#[test]
fn aws_runs_registry_images_only() {
    let mut s = AppSettings { image_source: ImageSource::LocalBuild, ..Default::default() };
    assert_eq!(aws_image(&s).unwrap(), REGISTRY_IMAGE);
    s.image_source = ImageSource::Registry;
    assert_eq!(aws_image(&s).unwrap(), REGISTRY_IMAGE);
    s.image_source = ImageSource::Custom;
    for good in ["registry.example.com/team/img:1", "ghcr.io/x/y", "user/repo:tag", "reg:5000/a"] {
        s.custom_image_name = Some(good.into());
        assert_eq!(aws_image(&s).unwrap(), good);
    }
    for bad in ["", "triple-c:latest", "ubuntu", "localhost/x", "127.0.0.1:5000/x", "a /b", "/x"] {
        s.custom_image_name = Some(bad.into());
        assert!(aws_image(&s).is_err(), "{bad}");
    }
}

#[test]
fn the_sandbox_is_forced_off_on_aws() {
    let mut p = aws_project(StorageMode::Ephemeral);
    p.sandbox_mode_enabled = true;
    let (pairs, _) = aws_env(&p, &AppSettings::default()).unwrap();
    let settings = &pairs.iter().find(|(k, _)| k == "CLAUDE_CODE_SETTINGS_JSON").unwrap().1;
    let v: serde_json::Value = serde_json::from_str(settings).unwrap();
    assert_eq!(v["sandbox"]["enabled"], false, "{settings}");
}

#[test]
fn files_for_the_secret_come_from_the_ssh_folder_without_leaving_it() {
    let base = tempfile::tempdir().unwrap();
    let ssh = base.path().join("ssh");
    std::fs::create_dir(&ssh).unwrap();
    std::fs::write(ssh.join("id_ed25519"), b"KEY").unwrap();
    std::fs::write(ssh.join("config"), b"Host *").unwrap();
    std::fs::create_dir(ssh.join("sub")).unwrap();
    std::fs::write(ssh.join("sub/nested"), b"x").unwrap();
    std::fs::write(base.path().join("outside"), b"SECRET-OUTSIDE").unwrap();
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(base.path().join("outside"), ssh.join("escape")).unwrap();
        std::os::unix::fs::symlink(ssh.join("config"), ssh.join("alias")).unwrap();
    }
    let got = workspace_files(Some(ssh.to_str().unwrap()), None).unwrap();
    let names: Vec<&str> = got.iter().map(|(k, _)| k.as_str()).collect();
    #[cfg(unix)]
    assert_eq!(names, vec!["ssh/alias", "ssh/config", "ssh/id_ed25519"]);
    #[cfg(not(unix))]
    assert_eq!(names, vec!["ssh/config", "ssh/id_ed25519"]);
    assert!(got.iter().all(|(_, v)| v != b"SECRET-OUTSIDE"));

    std::fs::write(ssh.join("huge"), vec![b'x'; MAX_FILE_BYTES as usize + 1]).unwrap();
    let e = workspace_files(Some(ssh.to_str().unwrap()), None).unwrap_err();
    assert!(e.contains("huge") && e.contains("64 KiB"), "{e}");

    assert!(workspace_files(Some("/"), None).is_err(), "a filesystem root is refused");
    assert!(workspace_files(None, None).unwrap().is_empty());
}

#[test]
fn ca_files_are_what_docker_would_mount() {
    let base = tempfile::tempdir().unwrap();
    let dir = base.path().join("ca");
    std::fs::create_dir(&dir).unwrap();
    std::fs::write(dir.join("corp.pem"), b"PEM").unwrap();
    std::fs::write(dir.join("README"), b"not a cert").unwrap();
    let ca = crate::docker::ca_certs::resolve(Some(dir.to_str().unwrap())).unwrap().unwrap();
    let got = workspace_files(None, Some(&ca)).unwrap();
    assert_eq!(got, vec![("ca/corp.pem".to_string(), b"PEM".to_vec())]);

    let one = base.path().join("Root CA.pem");
    std::fs::write(&one, b"ONE").unwrap();
    let ca = crate::docker::ca_certs::resolve(Some(one.to_str().unwrap())).unwrap().unwrap();
    let got = workspace_files(None, Some(&ca)).unwrap();
    assert_eq!(got, vec![("ca/Root_CA.crt".to_string(), b"ONE".to_vec())]);
}

// ───────────────────────────────────────────────── targets and reconcile

#[tokio::test]
async fn an_aws_projects_target_is_its_registered_tunnel() {
    let mut p = aws_project(StorageMode::Ephemeral);
    p.container_id = Some("http://127.0.0.1:61200/".into());
    assert_eq!(ProjectHost::for_project(&p), ProjectHost::Aws);
    assert_eq!(ContainerTarget::for_project(&p), None, "no tunnel, no target, whatever is stored");
    tunnels::register(&p.id, fake_tunnel(61200, std::sync::Arc::new(AtomicUsize::new(0))));
    assert_eq!(
        ContainerTarget::for_project(&p),
        Some(ContainerTarget::Agent(AgentEndpoint::loopback(61200, "tok")))
    );
    assert_eq!(
        ContainerTarget::for_project_waiting(&p).await,
        ContainerTarget::for_project(&p)
    );
    tunnels::close(&p.id).await;

    let local = Project::new("local".into(), Vec::new());
    assert_eq!(ProjectHost::for_project(&local), ProjectHost::Docker);
}

fn find(p: &Project) -> FindTask {
    FindTask::new(p, REGION, USER)
}

fn store_with(p: &Project) -> (tempfile::TempDir, ProjectsStore) {
    let dir = tempfile::tempdir().unwrap();
    let store = ProjectsStore::over_file_for_test(dir.path().join("projects.json"), vec![p.clone()]);
    (dir, store)
}

#[tokio::test]
async fn reconcile_marks_a_project_whose_task_stopped_as_stopped() {
    let mut p = aws_project(StorageMode::Hybrid);
    p.status = ProjectStatus::Running;
    {
        let a = p.aws.as_mut().unwrap();
        a.task_arn = Some(task_arn("gone"));
        a.access_point_id = Some("fsap-1".into());
        a.task_definition_arn = Some("arn:td:1".into());
    }
    let fake = Fake::default().with_task(task(&p, "gone", "STOPPED", false));
    let (_d, store) = store_with(&p);
    assert_eq!(reconcile_project(&fake, &store, &p, &find(&p)).await.unwrap(), Reconciled::Gone);
    let after = store.get(&p.id).unwrap();
    assert_eq!(after.status, ProjectStatus::Stopped);
    let a = after.aws.unwrap();
    assert_eq!(a.task_arn, None, "a stopped task's ARN is cleared");
    assert_eq!(a.access_point_id.as_deref(), Some("fsap-1"), "the data's access point stays");

    // ECS has forgotten it: also gone.
    let forgotten = Fake::default();
    let (_d, store) = store_with(&p);
    assert_eq!(reconcile_project(&forgotten, &store, &p, &find(&p)).await.unwrap(), Reconciled::Gone);
    assert_eq!(store.get(&p.id).unwrap().status, ProjectStatus::Stopped);
}

#[tokio::test]
async fn reconcile_keeps_a_project_with_a_live_task_running() {
    let mut p = aws_project(StorageMode::Hybrid);
    p.status = ProjectStatus::Running;
    p.aws.as_mut().unwrap().task_arn = Some(task_arn("up"));
    let fake = Fake::default().with_task(task(&p, "up", "RUNNING", true));
    let (_d, store) = store_with(&p);
    assert!(matches!(reconcile_project(&fake, &store, &p, &find(&p)).await.unwrap(), Reconciled::Live(_)));
    assert_eq!(store.get(&p.id).unwrap().status, ProjectStatus::Running);
}

#[tokio::test]
async fn the_watch_reports_a_running_aws_project_as_its_stored_target_and_confirms_a_recorded_stop() {
    let mut p = aws_project(StorageMode::Ephemeral);
    p.status = ProjectStatus::Running;
    p.container_id = Some("http://127.0.0.1:61300/".into());
    p.aws.as_mut().unwrap().task_arn = Some(task_arn("w1"));
    let w = watched_task(&p).expect("watched");
    assert_eq!(w.target.id(), "http://127.0.0.1:61300/", "settle's running_on matches the stored id");
    assert_eq!(w.group.cluster_arn, cluster_arn());
    assert_eq!(w.group.connection_id, "c1");
    assert!(!w.target.has_exited().await, "nothing recorded yet");
    record_stopped(std::slice::from_ref(&p), &[(task_arn("w1"), StopKind::Idle)]);
    assert!(w.target.has_exited().await);
    assert_eq!(crate::host::tunnels::stop_kind(w.target.id()), Some(StopKind::Idle));

    let mut stopped = p.clone();
    stopped.status = ProjectStatus::Stopped;
    assert!(watched_task(&stopped).is_none(), "a Stopped project is not polled again");
    let local = Project::new("l".into(), Vec::new());
    assert!(watched_task(&local).is_none());
}

#[tokio::test]
async fn the_winner_of_a_start_race_puts_its_token_back_in_the_secret() {
    let p = aws_project(StorageMode::Ephemeral);
    let fake = Fake {
        run: Some(task(&p, "new", "RUNNING", true)),
        rotated_by_loser: true,
        ..Default::default()
    };
    let (r, _) = start(&fake, &p).await;
    r.unwrap();
    let ours = fake.connect_tokens.lock().unwrap()[0].clone();
    let calls = fake.calls();
    assert_eq!(&calls[calls.len() - 3..], ["connect", "get-secret", "put-secret"]);
    let v: serde_json::Value =
        serde_json::from_str(fake.secret_json.lock().unwrap().as_deref().unwrap()).unwrap();
    assert_eq!(v["TRIPLEC_AGENT_TOKEN"], ours.as_str());
    assert!(v.get("TRIPLEC_FILES").is_some(), "the rest of the payload is kept");
}

#[tokio::test]
async fn an_unchanged_secret_is_not_rewritten_after_a_start() {
    let p = aws_project(StorageMode::Ephemeral);
    let fake = Fake { run: Some(task(&p, "new", "RUNNING", true)), ..Default::default() };
    start(&fake, &p).await.0.unwrap();
    assert_eq!(fake.calls().iter().filter(|c| *c == "put-secret").count(), 1);
}

#[test]
fn a_refused_token_says_to_restart_the_workspace() {
    assert_eq!(
        refusal_message("Agent health check failed: the agent rejected the credentials (401)"),
        TOKEN_MISMATCH
    );
    assert!(refusal_message("Agent health check failed: agent returned HTTP 500").contains("500"));
}

#[tokio::test]
async fn reconcile_finds_a_task_whose_arn_was_never_stored() {
    // The app quit during "Waiting for the task" in a build that stored the
    // ARN only at the end: the task is found by family and tags.
    let mut p = aws_project(StorageMode::Ephemeral);
    p.status = ProjectStatus::Running;
    let live = task(&p, "orphan", "RUNNING", true);
    let fake = Fake { listed: Mutex::new(vec![vec![live.arn.clone()]]), ..Default::default() }
        .with_task(live);
    let (_d, store) = store_with(&p);
    assert!(matches!(
        reconcile_project(&fake, &store, &p, &find(&p)).await.unwrap(),
        Reconciled::Live(_)
    ));
    let after = store.get(&p.id).unwrap();
    assert_eq!(after.status, ProjectStatus::Running);
    assert_eq!(after.aws.unwrap().task_arn, Some(task_arn("orphan")), "now stored");

    let nothing = Fake::default();
    let (_d, store) = store_with(&p);
    assert_eq!(reconcile_project(&nothing, &store, &p, &find(&p)).await.unwrap(), Reconciled::Gone);
    assert_eq!(store.get(&p.id).unwrap().status, ProjectStatus::Stopped);
}

#[test]
fn a_reattach_registers_only_while_the_project_still_runs_that_task() {
    let mut p = aws_project(StorageMode::Ephemeral);
    p.status = ProjectStatus::Running;
    p.aws.as_mut().unwrap().task_arn = Some(task_arn("t"));
    let (_d, store) = store_with(&p);
    assert!(reattach_still_wanted(&store, &p.id, &task_arn("t")));
    assert!(!reattach_still_wanted(&store, &p.id, &task_arn("other")));
    store.update_status(&p.id, ProjectStatus::Stopping).unwrap();
    assert!(!reattach_still_wanted(&store, &p.id, &task_arn("t")));
    store.remove(&p.id).unwrap();
    assert!(!reattach_still_wanted(&store, &p.id, &task_arn("t")));
}

#[test]
fn a_start_records_its_task_and_keeps_the_stored_handles_it_does_not_know() {
    let mut p = aws_project(StorageMode::Hybrid);
    p.aws.as_mut().unwrap().access_point_id = Some("fsap-old".into());
    let (_d, store) = store_with(&p);
    record_task(
        &store,
        &p.id,
        Some(&TaskRecord {
            task_arn: "arn:t".into(),
            access_point_id: None,
            task_definition_arn: Some("arn:td:2".into()),
            workspace_access_point_id: None,
        }),
    );
    let a = store.get(&p.id).unwrap().aws.unwrap();
    assert_eq!((a.task_arn.as_deref(), a.access_point_id.as_deref(), a.task_definition_arn.as_deref()),
        (Some("arn:t"), Some("fsap-old"), Some("arn:td:2")));
    record_task(&store, &p.id, None);
    let a = store.get(&p.id).unwrap().aws.unwrap();
    assert_eq!((a.task_arn, a.access_point_id.as_deref()), (None, Some("fsap-old")));
}

#[tokio::test]
async fn a_task_that_could_not_be_stopped_after_a_failed_start_stays_recorded() {
    let p = aws_project(StorageMode::Ephemeral);
    let fake = Fake {
        run: Some(task(&p, "slow", "PENDING", false)),
        stop_fails: true,
        ..Default::default()
    };
    assert!(start(&fake, &p).await.0.is_err());
    assert!(fake.called("stop"));
    assert_eq!(*fake.recorded.lock().unwrap(), vec![Some(task_arn("slow"))], "still findable");

    let fake = Fake {
        run: Some(task(&p, "new", "RUNNING", true)),
        connect_fails: true,
        stop_fails: true,
        ..Default::default()
    };
    assert!(start(&fake, &p).await.0.is_err());
    assert_eq!(*fake.recorded.lock().unwrap(), vec![Some(task_arn("new"))]);
}

#[tokio::test]
async fn the_task_log_reads_the_tasks_stream_in_the_records_log_group() {
    let p = aws_project(StorageMode::Hybrid);
    let fake = Fake { logs: vec!["line 1".into(), "line 2".into()], ..Default::default() };
    let lines = task_log_lines(&fake, &fake, &p, &who(), REGION, &task_arn("abc"), 20).await.unwrap();
    assert_eq!(lines, vec!["line 1", "line 2"]);
    let prefix = log_stream_prefix(&owner_key(REGION, USER), &p.id);
    assert_eq!(
        *fake.log_reads.lock().unwrap(),
        vec![format!("/triple-c/dev {prefix}/workspace/abc 20")]
    );
}

#[test]
fn the_log_task_is_the_stored_one_else_the_last_one_a_start_saw() {
    let mut p = aws_project(StorageMode::Hybrid);
    p.id = "log-task-test-project".into();
    assert_eq!(log_task_arn(&p), None);
    let (_d, store) = store_with(&p);
    record_task(
        &store,
        &p.id,
        Some(&TaskRecord {
            task_arn: "arn:first".into(),
            access_point_id: None,
            task_definition_arn: None,
            workspace_access_point_id: None,
        }),
    );
    record_task(&store, &p.id, None);
    assert_eq!(log_task_arn(&p).as_deref(), Some("arn:first"), "a cleared ARN still has a log");
    p.aws.as_mut().unwrap().task_arn = Some("arn:current".into());
    assert_eq!(log_task_arn(&p).as_deref(), Some("arn:current"));
}

#[tokio::test]
async fn reconcile_falls_back_to_a_newer_live_task_when_the_stored_one_stopped() {
    // m3: another machine restarted the project; the stored ARN is history.
    let mut p = aws_project(StorageMode::Ephemeral);
    p.status = ProjectStatus::Running;
    p.aws.as_mut().unwrap().task_arn = Some(task_arn("old"));
    let newer = task(&p, "newer", "RUNNING", true);
    let fake = Fake { listed: Mutex::new(vec![vec![newer.arn.clone()]]), ..Default::default() }
        .with_task(task(&p, "old", "STOPPED", false))
        .with_task(newer);
    let (_d, store) = store_with(&p);
    assert!(matches!(
        reconcile_project(&fake, &store, &p, &find(&p)).await.unwrap(),
        Reconciled::Live(t) if t.arn == task_arn("newer")
    ));
    let after = store.get(&p.id).unwrap();
    assert_eq!(after.status, ProjectStatus::Running);
    assert_eq!(after.aws.unwrap().task_arn, Some(task_arn("newer")));

    // ECS forgot the stored one and nothing else runs: Gone, after looking.
    let fake = Fake::default();
    let (_d, store) = store_with(&p);
    assert_eq!(reconcile_project(&fake, &store, &p, &find(&p)).await.unwrap(), Reconciled::Gone);
    assert!(fake.called("list"), "it looked for another task first");
}

#[tokio::test]
async fn reconcile_forgets_a_stale_stop_on_the_unattached_id_before_reusing_it() {
    use crate::host::tunnels;
    let mut p = aws_project(StorageMode::Ephemeral);
    p.status = ProjectStatus::Running;
    p.aws.as_mut().unwrap().task_arn = Some(task_arn("u2"));
    let placeholder = tunnels::unattached_id(&p.id);
    // A past unattached spell's task was seen stopped under this id.
    tunnels::mark_stopped(&placeholder, StopKind::Spot);
    p.container_id = Some("http://127.0.0.1:61400/".into());
    assert_eq!(prepare_unattached(&p), Some(placeholder.clone()));
    assert!(!tunnels::is_stopped(&placeholder), "the live task must not read as stopped");
    // Already on the placeholder: nothing to store, the mark still goes.
    p.container_id = Some(placeholder.clone());
    tunnels::mark_stopped(&placeholder, StopKind::Spot);
    assert_eq!(prepare_unattached(&p), None);
    assert!(!tunnels::is_stopped(&placeholder));
    assert_eq!(prepare_unattached(&Project::new("l".into(), Vec::new())), None);
}

#[tokio::test]
async fn an_unattached_aws_project_gets_a_target_id_no_tunnel_can_have() {
    // m9: the last run's loopback port may belong to another project's
    // tunnel now.
    let mut p = aws_project(StorageMode::Ephemeral);
    p.status = ProjectStatus::Running;
    p.container_id = Some("http://127.0.0.1:61400/".into());
    p.aws.as_mut().unwrap().task_arn = Some(task_arn("u1"));
    let id = unattached_container_id(&p).expect("replaced");
    assert_eq!(id, format!("http://unattached.invalid/{}/", p.id));
    p.container_id = Some(id.clone());
    assert_eq!(unattached_container_id(&p), None, "idempotent");
    // The watch still reports it, by that id, and a registered tunnel on the
    // old port can't be confused with it.
    let w = watched_task(&p).expect("still watched");
    assert_eq!(w.target.id(), id);
    p.container_id = None;
    assert_eq!(unattached_container_id(&p), None, "nothing stored, nothing to replace");
    let local = Project::new("l".into(), Vec::new());
    assert_eq!(unattached_container_id(&local), None);
}
