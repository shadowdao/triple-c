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
        Ok(())
    }
    async fn log_tail(&self, _g: &str, _s: &str, _l: i32) -> Result<Vec<String>, AwsError> {
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
        _root: &str,
        _tags: &[(String, String)],
    ) -> Result<String, AwsError> {
        self.log("create-ap");
        Ok("fsap-new".into())
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
        Ok(r#"{"TRIPLEC_AGENT_TOKEN":"token-of-the-running-task"}"#.into())
    }
    async fn access_point_state(&self, id: &str) -> Result<Option<String>, AwsError> {
        self.log(format!("ap-state {id}"));
        Ok(Some("available".into()))
    }
    async fn deregister_task_definition(&self, _arn: &str) -> Result<(), AwsError> {
        Ok(())
    }
    async fn delete_task_definitions(&self, _arns: &[String]) -> Result<(), AwsError> {
        Ok(())
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
        if self.connect_fails {
            return Err("session-manager-plugin was not found".into());
        }
        Ok((
            Box::new(FakeTunnel(std::sync::Arc::new(AtomicUsize::new(0)))),
            AgentEndpoint::loopback(61100, token),
        ))
    }
}

fn inputs<'a>(p: &'a Project, who: &'a CallerIdentity) -> StartInputs<'a> {
    StartInputs {
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
    let r = start_core(fake, fake, fake, fake, &inputs(p, &w), &progress).await;
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
            "ap-state fsap-new", "list", "run", "connect"
        ],
        "look for a live task before rotating the token; the access point is available before RunTask"
    );
    assert_eq!(steps, vec![PROGRESS_READING, PROGRESS_PREPARING, PROGRESS_STARTING, PROGRESS_WAITING, PROGRESS_CONNECTING]);
    // The connector got the token the secret was just written with.
    let secret: serde_json::Value =
        serde_json::from_str(fake.secret_json.lock().unwrap().as_deref().unwrap()).unwrap();
    assert_eq!(
        fake.connect_tokens.lock().unwrap()[0],
        secret["TRIPLEC_AGENT_TOKEN"].as_str().unwrap()
    );
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
    let mut s = AppSettings::default();
    s.image_source = ImageSource::LocalBuild;
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
    assert_eq!(reconcile_project(&fake, &store, &p).await.unwrap(), Reconciled::Gone);
    let after = store.get(&p.id).unwrap();
    assert_eq!(after.status, ProjectStatus::Stopped);
    let a = after.aws.unwrap();
    assert_eq!(a.task_arn, None, "a stopped task's ARN is cleared");
    assert_eq!(a.access_point_id.as_deref(), Some("fsap-1"), "the data's access point stays");

    // ECS has forgotten it: also gone.
    let forgotten = Fake::default();
    let (_d, store) = store_with(&p);
    assert_eq!(reconcile_project(&forgotten, &store, &p).await.unwrap(), Reconciled::Gone);
    assert_eq!(store.get(&p.id).unwrap().status, ProjectStatus::Stopped);
}

#[tokio::test]
async fn reconcile_keeps_a_project_with_a_live_task_running() {
    let mut p = aws_project(StorageMode::Hybrid);
    p.status = ProjectStatus::Running;
    p.aws.as_mut().unwrap().task_arn = Some(task_arn("up"));
    let fake = Fake::default().with_task(task(&p, "up", "RUNNING", true));
    let (_d, store) = store_with(&p);
    assert!(matches!(reconcile_project(&fake, &store, &p).await.unwrap(), Reconciled::Live(_)));
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
    record_stopped(std::slice::from_ref(&p), &[task_arn("w1")]);
    assert!(w.target.has_exited().await);

    let mut stopped = p.clone();
    stopped.status = ProjectStatus::Stopped;
    assert!(watched_task(&stopped).is_none(), "a Stopped project is not polled again");
    let local = Project::new("l".into(), Vec::new());
    assert!(watched_task(&local).is_none());
}
