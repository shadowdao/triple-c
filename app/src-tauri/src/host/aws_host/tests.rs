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
        container_instance_arn: None,
        exec_enabled: true,
        created_unix: None,
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
    /// When non-empty, what successive RunTasks start (before `run`).
    runs: Mutex<Vec<TaskInfo>>,
    logs: Vec<String>,
    role: Option<Knowable>,
    connect_fails: bool,
    /// The connector times out waiting for the agent (an image without it).
    agent_silent: bool,
    connect_tokens: Mutex<Vec<String>>,
    secret_json: Mutex<Option<String>>,
    /// A losing start rotates the secret's token while this one connects.
    rotated_by_loser: bool,
    /// StopTask is refused (expired credentials, say).
    stop_fails: bool,
    /// StopTask is accepted but the task never reaches STOPPED.
    stop_noop: bool,
    /// What `on_task` was told, in order.
    recorded: Mutex<Vec<Option<String>>>,
    /// `group stream limit` of each log read.
    log_reads: Mutex<Vec<String>>,
    /// The caller's EC2 record (absent by default).
    user_record: Option<crate::aws::environments::Param>,
    /// Reading the EC2 record fails (throttled, say).
    user_record_fails: bool,
    /// RunTask requests, in order.
    run_requests: Mutex<Vec<RunTaskInput>>,
    /// The clusters ListTasks / StopTask / DescribeTasks were asked about.
    list_clusters: Mutex<Vec<String>>,
    stop_clusters: Mutex<Vec<String>>,
    describe_clusters: Mutex<Vec<String>>,
    /// When set, ListTasks answers per cluster (empty for any other).
    listed_by_cluster: Option<HashMap<String, Vec<String>>>,
    /// Successive states of the run task, one per DescribeTasks (the last
    /// repeats); overrides `tasks` for that ARN.
    script: Mutex<Vec<TaskInfo>>,
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
    async fn run_task(&self, req: RunTaskInput) -> Result<TaskInfo, AwsError> {
        self.log("run");
        self.run_requests.lock().unwrap().push(req);
        let queued = {
            let mut q = self.runs.lock().unwrap();
            (!q.is_empty()).then(|| q.remove(0))
        };
        let t = queued.or_else(|| self.run.clone()).expect("a run was expected");
        self.tasks.lock().unwrap().insert(t.arn.clone(), t.clone());
        Ok(t)
    }
    async fn describe(&self, c: &str, arns: &[String]) -> Result<Vec<TaskInfo>, AwsError> {
        self.describe_clusters.lock().unwrap().push(c.into());
        {
            let mut script = self.script.lock().unwrap();
            if let Some(first) = script.first().cloned().filter(|t| arns.contains(&t.arn)) {
                if script.len() > 1 {
                    script.remove(0);
                }
                return Ok(vec![first]);
            }
        }
        let t = self.tasks.lock().unwrap();
        Ok(arns.iter().filter_map(|a| t.get(a).cloned()).collect())
    }
    async fn list_project_tasks(&self, c: &str, _f: &str) -> Result<Vec<String>, AwsError> {
        self.log("list");
        self.list_clusters.lock().unwrap().push(c.into());
        if let Some(by) = &self.listed_by_cluster {
            return Ok(by.get(c).cloned().unwrap_or_default());
        }
        let mut l = self.listed.lock().unwrap();
        Ok(if l.len() > 1 { l.remove(0) } else { l.first().cloned().unwrap_or_default() })
    }
    async fn stop_task(&self, c: &str, arn: &str, reason: &str) -> Result<(), AwsError> {
        self.log(format!("stop {arn} {reason}"));
        self.stop_clusters.lock().unwrap().push(c.into());
        if self.stop_fails {
            return Err(AwsError::Other { message: "ExpiredToken".into() });
        }
        // As ECS does: a stopped task is no longer live.
        if self.stop_noop {
            return Ok(());
        }
        if let Some(t) = self.tasks.lock().unwrap().get_mut(arn) {
            t.desired_status = "STOPPED".into();
            t.last_status = "STOPPED".into();
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
    async fn project_secret_value(&self, _name: &str) -> Result<Option<String>, AwsError> {
        Ok(None)
    }
    async fn create_project_secret(
        &self,
        n: &str,
        k: &str,
        j: &str,
        t: &[(String, String)],
    ) -> Result<Option<String>, AwsError> {
        self.put_project_secret(n, k, j, t).await.map(Some)
    }
    async fn delete_project_secret(&self, _name: &str) -> Result<(), AwsError> {
        Ok(())
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
    async fn user_record(&self, env: &str, _owner_key: &str) -> Result<crate::aws::environments::Param, AwsError> {
        self.log(format!("user-record {env}"));
        if self.user_record_fails {
            return Err(AwsError::Other { message: "Throttling".into() });
        }
        Ok(self.user_record.clone().unwrap_or(crate::aws::environments::Param::Missing))
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
        if self.agent_silent {
            return Err(agent_silent_message(None));
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

fn no_gateway() -> Result<Option<SidecarSecrets>, String> {
    Ok(None)
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
        custom_env_keys: &[],
        image: REGISTRY_IMAGE,
        gateway: &no_gateway,
        wait: WaitTiming {
            interval: Duration::from_millis(1),
            timeout: Duration::from_millis(30),
        },
        ec2_wait: WaitTiming {
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

/// A gateway payload that cannot be built: the provider key is gone.
fn missing_gateway_key() -> Result<Option<SidecarSecrets>, String> {
    Err("The LLM gateway sidecar needs the provider API key".into())
}

#[tokio::test]
async fn attaching_to_a_running_task_never_builds_the_gateway_payload() {
    let mut p = aws_project(StorageMode::Hybrid);
    p.aws.as_mut().unwrap().gateway_sidecar = true;
    let live = task(&p, "live", "RUNNING", true);
    let fake = Fake {
        listed: Mutex::new(vec![vec![live.arn.clone()]]),
        ..Default::default()
    }
    .with_task(live);
    let w = who();
    let on_task = |_: Option<&TaskRecord>| {};
    let inp = StartInputs { gateway: &missing_gateway_key, ..inputs(&p, &w, &on_task) };
    let started = start_core(&fake, &fake, &fake, &fake, &inp, &|_| {}).await.unwrap();
    assert!(!started.created);

    // A start that prepares a new task does need it, and fails before RunTask.
    let fake = Fake::default();
    let inp = StartInputs { gateway: &missing_gateway_key, ..inputs(&p, &w, &on_task) };
    let e = start_core(&fake, &fake, &fake, &fake, &inp, &|_| {}).await.err().unwrap();
    assert!(e.contains("provider API key"), "{e}");
    assert!(!fake.called("run"));
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
async fn a_silent_agent_names_the_image_and_the_edge_tag() {
    let p = aws_project(StorageMode::Ephemeral);
    let fake = Fake { run: Some(task(&p, "new", "RUNNING", true)), agent_silent: true, ..Default::default() };
    let e = start(&fake, &p).await.0.err().unwrap();
    assert!(e.contains(&format!("\"{REGISTRY_IMAGE}\"")), "{e}");
    assert!(e.contains("older than this app's AWS support"), "{e}");
    assert!(e.contains(":latest moves only when a release is cut"), "{e}");
    assert!(e.contains("Settings → Docker → Custom image to ghcr.io/shadowdao/triple-c-sandbox:edge"), "{e}");
}

#[test]
fn the_silent_agent_message_escapes_the_image_name() {
    let m = agent_silent_message(Some("evil\"\nimage"));
    assert!(m.contains(r#""evil\"\nimage""#), "{m}");
    assert!(!m.contains('\n'));
    assert!(agent_silent_message(None).contains("The workspace's image may be older"));
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
    assert!(!sandbox_allowed(&p));
}

/// E8, measured live (phase 7): bubblewrap can't create a user namespace
/// in an ECS task on the ECS-optimized AMI either (Docker's default seccomp
/// profile refuses it without CAP_SYS_ADMIN), so EC2 keeps it off too.
#[test]
fn the_sandbox_is_forced_off_on_ec2_too() {
    let mut p = ec2_project();
    p.sandbox_mode_enabled = true;
    assert!(!sandbox_allowed(&p));
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
    FindTask::new(p, REGION, USER, None)
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

#[tokio::test]
async fn a_persisted_starting_aws_project_with_a_task_arn_is_reconciled_against_ecs_on_load() {
    use crate::commands::project_commands::needs_aws_reconcile;
    use crate::storage::projects_store::settle_stale_statuses;

    // The app quit mid-start: Starting, the task's ARN already recorded.
    let mut p = aws_project(StorageMode::Hybrid);
    p.status = ProjectStatus::Starting;
    p.aws.as_mut().unwrap().task_arn = Some(task_arn("mid"));
    let mut docker = Project::new("local".into(), vec![]);
    docker.status = ProjectStatus::Starting;

    let mut loaded = vec![p.clone(), docker];
    assert!(settle_stale_statuses(&mut loaded));
    // Not Stopped (that would hide a billed task): Error, ARN kept.
    assert_eq!(loaded[0].status, ProjectStatus::Error);
    assert_eq!(loaded[0].aws.as_ref().unwrap().task_arn, Some(task_arn("mid")));
    assert_eq!(loaded[1].status, ProjectStatus::Stopped, "Docker is unchanged");
    assert!(needs_aws_reconcile(&loaded[0]));

    // ECS says it runs: Running.
    let p = loaded[0].clone();
    let fake = Fake::default().with_task(task(&p, "mid", "RUNNING", true));
    let (_d, store) = store_with(&p);
    assert!(matches!(reconcile_project(&fake, &store, &p, &find(&p)).await.unwrap(), Reconciled::Live(_)));
    assert_eq!(store.get(&p.id).unwrap().status, ProjectStatus::Running);
    // ECS says it is gone: Stopped, ARN cleared.
    let (_d, store) = store_with(&p);
    assert_eq!(reconcile_project(&Fake::default(), &store, &p, &find(&p)).await.unwrap(), Reconciled::Gone);
    let after = store.get(&p.id).unwrap();
    assert_eq!(after.status, ProjectStatus::Stopped);
    assert_eq!(after.aws.unwrap().task_arn, None);
}

#[test]
fn a_stopped_aws_project_is_reconciled_only_when_it_still_has_a_task_arn() {
    use crate::commands::project_commands::needs_aws_reconcile;
    let mut p = aws_project(StorageMode::Hybrid);
    p.status = ProjectStatus::Stopped;
    assert!(!needs_aws_reconcile(&p), "a clean stop clears the ARN");
    p.aws.as_mut().unwrap().task_arn = Some(task_arn("left"));
    assert!(needs_aws_reconcile(&p));
    for s in [ProjectStatus::Running, ProjectStatus::Error, ProjectStatus::Starting, ProjectStatus::Stopping] {
        p.status = s;
        p.aws.as_mut().unwrap().task_arn = None;
        assert!(needs_aws_reconcile(&p));
    }
}

// ──────────────────────────────────────────── copy-at-connect (Q2)

mod copy_at_connect {
    use super::*;
    use crate::host::aws_host::connect::*;
    use crate::models::project::SecretsDelivery;
    use crate::models::EnvVar;

    fn connect_project(delivery: SecretsDelivery) -> Project {
        let mut p = aws_project(StorageMode::Hybrid);
        p.aws.as_mut().unwrap().secrets_delivery = delivery;
        p
    }

    /// `(dir, name, data, mode)`.
    type Upload = (String, String, Vec<u8>, u32);

    #[derive(Default)]
    struct FakeTarget {
        calls: Mutex<Vec<String>>,
        uploads: Mutex<Vec<Upload>>,
        apply_result: Mutex<Option<(String, i64)>>,
    }

    impl ConnectTarget for FakeTarget {
        async fn exec_root(&self, cmd: Vec<String>) -> Result<(String, i64), String> {
            self.calls.lock().unwrap().push(format!("exec {}", cmd.join(" ")));
            if cmd.get(1).map(String::as_str) == Some("--prepare") {
                return Ok(("0123456789abcdef\n".into(), 0));
            }
            if let Some(r) = self.apply_result.lock().unwrap().clone() {
                return Ok(r);
            }
            Ok(("triple-c-apply-connect: credentials applied\n".into(), 0))
        }
        async fn upload_root(&self, dir: &str, name: &str, data: &[u8], mode: u32) -> Result<(), String> {
            self.calls.lock().unwrap().push(format!("upload {dir}/{name}"));
            self.uploads.lock().unwrap().push((dir.into(), name.into(), data.to_vec(), mode));
            Ok(())
        }
    }

    fn bundle() -> ConnectBundle {
        ConnectBundle {
            files: vec![
                ("ssh/id_ed25519".into(), b"PRIVATE".to_vec().into()),
                ("ca/corp.crt".into(), b"PEM".to_vec().into()),
            ],
            env: vec![
                ("GIT_TOKEN".into(), String::from("ghp_x").into()),
                ("MULTI".into(), String::from("a=b\nc").into()),
            ],
        }
    }

    #[tokio::test]
    async fn a_push_prepares_uploads_root_only_then_applies() {
        let t = FakeTarget::default();
        push_connect_bundle(&t, &bundle()).await.unwrap();
        assert_eq!(
            *t.calls.lock().unwrap(),
            vec![
                "exec /usr/local/bin/triple-c-apply-connect --prepare",
                "upload /dev/shm/triple-c/connect/0123456789abcdef/ssh/id_ed25519",
                "upload /dev/shm/triple-c/connect/0123456789abcdef/ca/corp.crt",
                "upload /dev/shm/triple-c/connect/0123456789abcdef/connect.env",
                "exec /usr/local/bin/triple-c-apply-connect 0123456789abcdef",
            ]
        );
        let uploads = t.uploads.lock().unwrap();
        assert!(uploads.iter().all(|u| u.3 == 0o600), "every staged file is 0600");
        assert_eq!(uploads[0].2, b"PRIVATE");
        // KEY=<base64> lines: a newline or `=` in a value reads back whole.
        assert_eq!(
            String::from_utf8(uploads[2].2.clone()).unwrap(),
            "GIT_TOKEN=Z2hwX3g=\nMULTI=YT1iCmM=\n"
        );
    }

    #[tokio::test]
    async fn a_failed_apply_reports_its_output_and_an_old_image_says_so() {
        let t = FakeTarget::default();
        *t.apply_result.lock().unwrap() = Some(("line\ntriple-c-apply-connect: /x is missing\n".into(), 1));
        let e = push_connect_bundle(&t, &bundle()).await.unwrap_err();
        assert!(e.contains("failed (1)") && e.contains("/x is missing"), "{e}");
        assert!(!e.contains("ghp_x"));

        let t = FakeTarget::default();
        *t.apply_result.lock().unwrap() = Some(("not found".into(), 127));
        let e = push_connect_bundle(&t, &bundle()).await.unwrap_err();
        assert!(e.contains("predates copying credentials"), "{e}");

        // A bad file key is refused before anything is uploaded under it.
        let t = FakeTarget::default();
        let bad = ConnectBundle { files: vec![("ssh/../x".into(), Vec::new().into())], env: vec![] };
        assert!(push_connect_bundle(&t, &bad).await.is_err());
        assert!(t.uploads.lock().unwrap().is_empty());
    }

    #[test]
    fn only_a_well_formed_connect_id_is_used_as_a_path() {
        assert_eq!(parse_nonce("noise\n0123456789abcdef\n").unwrap(), "0123456789abcdef");
        for bad in ["", "../../etc/passwd", "0123456789ABCDEF", "0123456789abcde", "0123456789abcdef0"] {
            assert!(parse_nonce(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn a_bundle_never_shows_its_values() {
        let shown = format!("{:?}", bundle());
        assert!(shown.contains("GIT_TOKEN") && shown.contains("ssh/id_ed25519"), "{shown}");
        assert!(!shown.contains("ghp_x") && !shown.contains("PRIVATE"), "{shown}");
    }

    #[test]
    fn the_bundle_is_the_ssh_and_ca_files_and_the_credential_variables() {
        let base = tempfile::tempdir().unwrap();
        let ssh = base.path().join("ssh");
        std::fs::create_dir(&ssh).unwrap();
        std::fs::write(ssh.join("id_ed25519"), b"KEY").unwrap();
        let mut p = connect_project(SecretsDelivery::CopyAtConnect);
        p.ssh_key_path = Some(ssh.to_str().unwrap().into());
        p.git_token = Some("ghp_tok".into());
        p.custom_env_vars = vec![EnvVar { key: "MY_KEY".into(), value: "v1".into() }];
        let settings = AppSettings {
            global_custom_env_vars: vec![EnvVar { key: "GLOBAL_KEY".into(), value: "g".into() }],
            ..Default::default()
        };
        let b = connect_credentials(&p, &settings).unwrap();
        assert_eq!(b.files.len(), 1);
        assert_eq!((b.files[0].0.as_str(), b.files[0].1.as_slice()), ("ssh/id_ed25519", &b"KEY"[..]));
        let env: HashMap<String, String> = b.env.iter().map(|(k, v)| (k.clone(), v.to_string())).collect();
        assert_eq!(env["GIT_TOKEN"], "ghp_tok");
        assert_eq!(env["MY_KEY"], "v1");
        assert_eq!(env["GLOBAL_KEY"], "g");
        assert!(!env.contains_key("TZ") && !env.contains_key("CLAUDE_CODE_SETTINGS_JSON"));
        assert!(b.env.iter().all(|(k, _)| crate::aws::workspace::is_credential_key(
            k,
            &["MY_KEY".to_string(), "GLOBAL_KEY".to_string()]
        )));
    }

    #[test]
    fn a_terminal_gets_the_api_keys_even_when_a_file_is_unreadable() {
        let mut p = connect_project(SecretsDelivery::CopyAtConnect);
        p.ssh_key_path = Some("/nonexistent/ssh-dir".into());
        p.ca_cert_path = Some("/nonexistent/ca.pem".into());
        p.git_token = Some("ghp_tok".into());
        let settings = AppSettings::default();
        assert!(connect_credentials(&p, &settings).is_err(), "the push needs the files");
        let env: HashMap<String, String> =
            connect_env(&p, &settings).into_iter().map(|(k, v)| (k, v.to_string())).collect();
        assert_eq!(env["GIT_TOKEN"], "ghp_tok");
    }

    #[test]
    fn a_copy_at_connect_terminal_carries_the_credentials_and_a_secrets_manager_one_does_not() {
        let creds = || Ok(bundle().env);
        let p = connect_project(SecretsDelivery::CopyAtConnect);
        let spec = crate::host::sessions::session_spec(
            vec!["bash".into(), "-l".into()],
            true,
            terminal_env_with(&p, creds),
        );
        assert_eq!(spec.env, vec!["GIT_TOKEN=ghp_x".to_string(), "MULTI=a=b\nc".to_string()]);
        assert!(spec.tty && spec.user == "claude");
        assert_eq!(spec.working_dir.as_deref(), Some("/workspace"));

        let p = connect_project(SecretsDelivery::SecretsManager);
        let env = terminal_env_with(&p, || panic!("not gathered for Secrets Manager"));
        assert!(crate::host::sessions::session_spec(vec!["bash".into()], true, env).env.is_empty());

        // A local project never; a failure opens the terminal without them.
        let mut local = connect_project(SecretsDelivery::CopyAtConnect);
        local.host_id = None;
        assert!(terminal_env_with(&local, || panic!("local")).is_empty());
        let p = connect_project(SecretsDelivery::CopyAtConnect);
        assert!(terminal_env_with(&p, || Err("no CA file".into())).is_empty());
    }

    #[test]
    fn only_a_connected_copy_at_connect_project_gets_a_push() {
        let p = connect_project(SecretsDelivery::SecretsManager);
        assert!(!copies_at_connect(&p));
        assert!(!spawn_connect_push(&p, &AppSettings::default(), |_| panic!("no push")));
        let p = connect_project(SecretsDelivery::CopyAtConnect);
        assert!(copies_at_connect(&p));
        // No tunnel registered for it: nothing to push to.
        assert!(!spawn_connect_push(&p, &AppSettings::default(), |_| panic!("no tunnel")));
    }
    #[test]
    fn a_sidecar_terminal_keeps_the_tasks_own_token() {
        use crate::models::project::{Backend, OpenAiCompatibleConfig};
        let mut p = connect_project(SecretsDelivery::CopyAtConnect);
        p.backend = Backend::OpenAiCompatible;
        p.openai_compatible_config = Some(OpenAiCompatibleConfig {
            base_url: "http://host.docker.internal:4000".into(),
            api_key: Some("sk-local".into()),
            model_id: Some("gpt".into()),
            haiku_model_id: None,
        });
        p.git_token = Some("ghp_tok".into());
        let settings = AppSettings::default();
        let keys = |p: &Project| -> Vec<String> {
            connect_env(p, &settings).into_iter().map(|(k, _)| k).collect()
        };
        assert!(keys(&p).contains(&"ANTHROPIC_AUTH_TOKEN".to_string()));
        p.aws.as_mut().unwrap().gateway_sidecar = true;
        let with_sidecar = keys(&p);
        assert!(!with_sidecar.contains(&"ANTHROPIC_AUTH_TOKEN".to_string()), "{with_sidecar:?}");
        assert!(with_sidecar.contains(&"GIT_TOKEN".to_string()));
    }
}

// ──────────────────────────────────────────── gateway sidecar (Q7)

mod gateway_sidecar_inputs {
    use super::*;
    use crate::models::gateway_settings::GatewayModel;
    use crate::models::project::Backend;

    fn sidecar(on: bool) -> (Project, AppSettings) {
        let mut p = aws_project(StorageMode::Ephemeral);
        p.backend = Backend::OpenAiCompatible;
        p.aws.as_mut().unwrap().gateway_sidecar = on;
        let mut s = AppSettings::default();
        s.gateway.models = vec![GatewayModel { name: "gpt".into(), model_id: "gpt-5".into() }];
        (p, s)
    }

    #[test]
    fn off_reads_no_key() {
        let (p, s) = sidecar(false);
        assert!(gateway_sidecar(&p, &s, || panic!("no keychain read")).unwrap().is_none());
    }

    #[test]
    fn on_renders_a_key_free_config_and_a_fresh_master_key_each_start() {
        let (p, s) = sidecar(true);
        let a = gateway_sidecar(&p, &s, || Ok(Some("sk-provider".into()))).unwrap().unwrap();
        let b = gateway_sidecar(&p, &s, || Ok(Some("sk-provider".into()))).unwrap().unwrap();
        let (pa, pb) = (a.payload(), b.payload());
        assert_eq!(pa.api_key, "sk-provider");
        assert!(!pa.config.contains("sk-provider") && !pa.config.contains(pa.master_key));
        assert!(pa.config.contains("os.environ/TRIPLEC_GATEWAY_API_KEY"));
        assert!(pa.config.contains("model: \"openai/gpt-5\""));
        assert_eq!(pa.master_key.len(), 64);
        assert_ne!(pa.master_key, pb.master_key);
    }

    #[test]
    fn on_refuses_a_missing_key_backend_or_model() {
        let (p, s) = sidecar(true);
        for key in [None, Some("  ".to_string())] {
            let e = gateway_sidecar(&p, &s, || Ok(key.clone())).err().unwrap();
            assert!(e.contains("provider API key"), "{e}");
        }
        let mut wrong = p.clone();
        wrong.backend = Backend::Anthropic;
        assert!(gateway_sidecar(&wrong, &s, || Ok(Some("k".into()))).err().unwrap().contains("OpenAI Compatible"));
        let mut empty = s.clone();
        empty.gateway.models.clear();
        assert!(gateway_sidecar(&p, &empty, || Ok(Some("k".into()))).err().unwrap().contains("Model Gateway"));
    }
}


// ------------------------------------------------------------- EC2 (phase 7)

const USER_CLUSTER: &str = "triple-c-dev--u-alice";

fn user_cluster_arn() -> String {
    format!("arn:aws:ecs:{REGION}:{ACCOUNT}:cluster/{USER_CLUSTER}")
}

fn ec2_task_arn(id: &str) -> String {
    format!("arn:aws:ecs:{REGION}:{ACCOUNT}:task/{USER_CLUSTER}/{id}")
}

fn ec2_project() -> Project {
    let mut p = aws_project(StorageMode::Ephemeral);
    let a = p.aws.as_mut().unwrap();
    a.launch_type = LaunchType::Ec2;
    a.cpu = 1024;
    a.memory_mib = 4096;
    p
}

fn ec2_task(p: &Project, id: &str, last: &str, ready: bool, placed: bool) -> TaskInfo {
    TaskInfo {
        arn: ec2_task_arn(id),
        container_instance_arn: placed.then(|| {
            format!("arn:aws:ecs:{REGION}:{ACCOUNT}:container-instance/{USER_CLUSTER}/ci-1")
        }),
        ..task(p, id, last, ready)
    }
}

fn user_record_param() -> crate::aws::environments::Param {
    crate::aws::environments::Param::Value(
        serde_json::json!({
            "schema": 1, "instance_type": "m7i.large", "max_instances": 2, "user_tag": "alice",
            "cluster_arn": user_cluster_arn(),
            "capacity_provider": USER_CLUSTER,
        })
        .to_string(),
    )
}

#[tokio::test]
async fn an_ec2_start_runs_on_the_users_capacity_and_says_when_it_waits_for_an_instance() {
    let p = ec2_project();
    let fake = Fake {
        user_record: Some(user_record_param()),
        run: Some(ec2_task(&p, "e1", "PROVISIONING", false, false)),
        ..Default::default()
    };
    // Cold start: no instance, then one is placed, then the task is up.
    *fake.script.lock().unwrap() = vec![
        ec2_task(&p, "e1", "PROVISIONING", false, false),
        ec2_task(&p, "e1", "PROVISIONING", false, false),
        ec2_task(&p, "e1", "PENDING", false, true),
        ec2_task(&p, "e1", "RUNNING", true, true),
    ];
    let (r, steps) = start(&fake, &p).await;
    let started = r.unwrap();
    assert!(started.created);
    assert_eq!(started.task.arn, ec2_task_arn("e1"));
    assert_eq!(
        steps,
        vec![
            PROGRESS_READING,
            PROGRESS_PREPARING,
            PROGRESS_STARTING,
            PROGRESS_WAITING,
            PROGRESS_INSTANCE,
            PROGRESS_WAITING,
            PROGRESS_CONNECTING
        ]
    );
    assert_eq!(PROGRESS_INSTANCE, "Starting an instance (about 3 minutes)…");
    // The record is read before anything is written; the project's task is
    // looked for in both clusters, then once more where it will run.
    let calls = fake.calls();
    let pos = |c: &str| calls.iter().position(|x| x == c).unwrap();
    assert!(pos("user-record dev") < pos("put-secret"), "{calls:?}");
    assert_eq!(
        *fake.list_clusters.lock().unwrap(),
        [cluster_arn(), user_cluster_arn(), user_cluster_arn()]
    );
    let req = &fake.run_requests.lock().unwrap()[0];
    assert_eq!(req.cluster(), Some(user_cluster_arn().as_str()));
    assert_eq!(req.capacity_provider_strategy()[0].capacity_provider(), USER_CLUSTER);
    assert!(req.network_configuration().is_none() && req.launch_type().is_none());
    assert!(fake.describe_clusters.lock().unwrap().iter().all(|c| *c == user_cluster_arn()));
}

/// ECS turns ECS Exec off on a RunTask that has to wait for an instance
/// (verified live): that task only brings the instance up; once placed it
/// is stopped and the task run again, onto the instance, with ECS Exec.
#[tokio::test]
async fn a_cold_ec2_start_runs_the_task_again_once_an_instance_is_up() {
    let p = ec2_project();
    let execless = |last: &str, placed: bool| TaskInfo { exec_enabled: false, ..ec2_task(&p, "e1", last, false, placed) };
    let fake = Fake {
        user_record: Some(user_record_param()),
        runs: Mutex::new(vec![execless("PROVISIONING", false), ec2_task(&p, "e9", "PENDING", false, true)]),
        ..Default::default()
    };
    *fake.script.lock().unwrap() = vec![
        execless("PROVISIONING", false),
        execless("PENDING", true),
        TaskInfo { desired_status: "STOPPED".into(), ..execless("PENDING", true) },
        execless("STOPPED", true),
        ec2_task(&p, "e9", "RUNNING", true, true),
    ];
    let (r, steps) = start(&fake, &p).await;
    let started = r.unwrap();
    assert_eq!(started.task.arn, ec2_task_arn("e9"));
    let calls = fake.calls();
    assert_eq!(calls.iter().filter(|c| *c == "run").count(), 2, "{calls:?}");
    let stop = format!("stop {} {STOP_REASON}", ec2_task_arn("e1"));
    let pos = |c: &str| calls.iter().rposition(|x| x == c).unwrap();
    assert!(calls.iter().position(|c| *c == stop).unwrap() < pos("run"), "{calls:?}");
    assert_eq!(*fake.stop_clusters.lock().unwrap(), [user_cluster_arn()]);
    // Both requests ask for ECS Exec on the user's capacity.
    for req in fake.run_requests.lock().unwrap().iter() {
        assert_eq!(req.enable_execute_command(), Some(true));
        assert_eq!(req.capacity_provider_strategy()[0].capacity_provider(), USER_CLUSTER);
    }
    // The second task is the one recorded.
    assert_eq!(*fake.recorded.lock().unwrap(), vec![Some(ec2_task_arn("e1")), Some(ec2_task_arn("e9"))]);
    assert_eq!(
        steps,
        vec![
            PROGRESS_READING,
            PROGRESS_PREPARING,
            PROGRESS_STARTING,
            PROGRESS_WAITING,
            PROGRESS_INSTANCE,
            PROGRESS_WAITING,
            PROGRESS_CONNECTING
        ]
    );
}

#[tokio::test]
async fn a_second_task_without_ecs_exec_is_stopped_and_reported() {
    let p = ec2_project();
    let execless = |id: &str, last: &str, placed: bool| TaskInfo { exec_enabled: false, ..ec2_task(&p, id, last, false, placed) };
    let fake = Fake {
        user_record: Some(user_record_param()),
        runs: Mutex::new(vec![execless("e1", "PROVISIONING", false), execless("e9", "PENDING", true)]),
        ..Default::default()
    };
    *fake.script.lock().unwrap() = vec![execless("e1", "PENDING", true), execless("e1", "STOPPED", true)];
    let (r, _) = start(&fake, &p).await;
    assert_eq!(r.err().unwrap().lines().next(), Some(EXEC_OFF));
    let calls = fake.calls();
    assert!(calls.contains(&format!("stop {} {STOP_REASON}", ec2_task_arn("e9"))), "{calls:?}");
}

#[tokio::test]
async fn a_cold_ec2_task_stopped_before_placement_ends_the_start_with_its_reason() {
    let p = ec2_project();
    let execless = |last: &str| TaskInfo { exec_enabled: false, ..ec2_task(&p, "e1", last, false, false) };
    let fake = Fake {
        user_record: Some(user_record_param()),
        runs: Mutex::new(vec![execless("PROVISIONING")]),
        ..Default::default()
    };
    *fake.script.lock().unwrap() = vec![
        execless("PROVISIONING"),
        TaskInfo {
            desired_status: "STOPPED".into(),
            stopped_reason: Some("Task stopped by user".into()),
            ..execless("PROVISIONING")
        },
    ];
    let (r, _) = start(&fake, &p).await;
    let e = r.err().unwrap();
    assert!(e.contains("Task stopped by user"), "{e}");
    assert_eq!(fake.calls().iter().filter(|c| *c == "run").count(), 1, "no second RunTask");
}

#[tokio::test]
async fn an_execless_task_still_waiting_for_an_instance_is_another_starts() {
    let mut p = ec2_project();
    p.aws.as_mut().unwrap().task_arn = Some(ec2_task_arn("other"));
    let other = TaskInfo { exec_enabled: false, ..ec2_task(&p, "other", "PROVISIONING", false, false) };
    let fake = Fake {
        user_record: Some(user_record_param()),
        listed_by_cluster: Some(HashMap::from([(user_cluster_arn(), vec![ec2_task_arn("other")])])),
        ..Default::default()
    }
    .with_task(other);
    let (r, _) = start(&fake, &p).await;
    assert_eq!(r.err().unwrap(), INSTANCE_RACE);
    assert!(!fake.called("run") && !fake.called("stop"), "{:?}", fake.calls());
}

#[tokio::test]
async fn the_cold_reruns_launch_never_doubles_another_starts_task() {
    let p = ec2_project();
    let execless = |last: &str, placed: bool| TaskInfo { exec_enabled: false, ..ec2_task(&p, "e1", last, false, placed) };
    let winner = ec2_task(&p, "w1", "PENDING", false, true);
    let fake = Fake {
        user_record: Some(user_record_param()),
        runs: Mutex::new(vec![execless("PROVISIONING", false)]),
        // Nothing for the start's own finds; the winner's task by the rerun.
        listed: Mutex::new(vec![vec![], vec![], vec![], vec![ec2_task_arn("w1")]]),
        ..Default::default()
    }
    .with_task(winner);
    *fake.script.lock().unwrap() = vec![execless("PENDING", true), execless("STOPPED", true)];
    let (r, _) = start(&fake, &p).await;
    assert_eq!(r.err().unwrap(), START_RACE);
    assert_eq!(fake.calls().iter().filter(|c| *c == "run").count(), 1, "{:?}", fake.calls());
    // The winner's task is recorded, as on the first launch's race.
    assert_eq!(fake.recorded.lock().unwrap().last().cloned().flatten(), Some(ec2_task_arn("w1")));
}

#[tokio::test]
async fn an_execless_task_that_cannot_be_stopped_fails_the_start() {
    let mut p = ec2_project();
    p.aws.as_mut().unwrap().task_arn = Some(ec2_task_arn("old"));
    let old = TaskInfo { exec_enabled: false, ..ec2_task(&p, "old", "RUNNING", false, true) };
    let fake = Fake {
        user_record: Some(user_record_param()),
        listed_by_cluster: Some(HashMap::from([(user_cluster_arn(), vec![ec2_task_arn("old")])])),
        stop_fails: true,
        ..Default::default()
    }
    .with_task(old);
    let (r, _) = start(&fake, &p).await;
    let e = r.err().unwrap();
    assert!(e.contains("Could not stop") && e.contains("ExpiredToken"), "{e}");
    assert!(!fake.called("run"));
}

fn now_unix() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64
}

/// An exec-off task still waiting for an instance, older than any start
/// could still be waiting (a crashed start's orphan), is replaced; a fresh
/// one is another start's.
#[tokio::test]
async fn an_orphaned_waiting_execless_task_is_replaced_and_a_fresh_one_is_left_alone() {
    let mut p = ec2_project();
    p.aws.as_mut().unwrap().task_arn = Some(ec2_task_arn("old"));
    let waiting = |created: i64| TaskInfo {
        exec_enabled: false,
        created_unix: Some(created),
        ..ec2_task(&p, "old", "PROVISIONING", false, false)
    };
    let fake_with = |t: TaskInfo| {
        Fake {
            user_record: Some(user_record_param()),
            listed_by_cluster: Some(HashMap::from([(user_cluster_arn(), vec![ec2_task_arn("old")])])),
            run: Some(ec2_task(&p, "new", "RUNNING", true, true)),
            ..Default::default()
        }
        .with_task(t)
    };
    let fake = fake_with(waiting(now_unix() - 3600));
    let (r, _) = start(&fake, &p).await;
    assert_eq!(r.unwrap().task.arn, ec2_task_arn("new"));
    assert!(fake.calls().contains(&format!("stop {} {REPLACED_REASON}", ec2_task_arn("old"))), "{:?}", fake.calls());

    let fake = fake_with(waiting(now_unix()));
    let (r, _) = start(&fake, &p).await;
    let e = r.err().unwrap();
    assert_eq!(e, INSTANCE_RACE);
    assert!(e.contains("Stop"), "{e}");
    assert!(!fake.called("run") && !fake.called("stop"));
}

#[tokio::test]
async fn a_cold_start_whose_task_another_start_replaced_says_so() {
    let p = ec2_project();
    let execless = |last: &str| TaskInfo { exec_enabled: false, ..ec2_task(&p, "e1", last, false, false) };
    let fake = Fake {
        user_record: Some(user_record_param()),
        runs: Mutex::new(vec![execless("PROVISIONING")]),
        ..Default::default()
    };
    *fake.script.lock().unwrap() = vec![TaskInfo {
        desired_status: "STOPPED".into(),
        stopped_reason: Some(REPLACED_REASON.into()),
        ..execless("PROVISIONING")
    }];
    let (r, _) = start(&fake, &p).await;
    let e = r.err().unwrap();
    assert!(e.starts_with("Another start of this workspace replaced this one"), "{e}");
    assert_eq!(fake.calls().iter().filter(|c| *c == "run").count(), 1);
}

#[tokio::test]
async fn an_execless_task_that_never_stops_fails_the_start_without_a_new_run() {
    let mut p = ec2_project();
    p.aws.as_mut().unwrap().task_arn = Some(ec2_task_arn("old"));
    let old = TaskInfo { exec_enabled: false, ..ec2_task(&p, "old", "RUNNING", false, true) };
    let fake = Fake {
        user_record: Some(user_record_param()),
        listed_by_cluster: Some(HashMap::from([(user_cluster_arn(), vec![ec2_task_arn("old")])])),
        stop_noop: true,
        ..Default::default()
    }
    .with_task(old);
    let (r, _) = start(&fake, &p).await;
    let e = r.err().unwrap();
    assert!(e.contains("did not stop in time"), "{e}");
    assert!(fake.called("stop") && !fake.called("run"));
}

#[tokio::test]
async fn a_live_task_with_ecs_exec_off_is_stopped_and_replaced() {
    let mut p = ec2_project();
    p.aws.as_mut().unwrap().task_arn = Some(ec2_task_arn("old"));
    let old = TaskInfo { exec_enabled: false, ..ec2_task(&p, "old", "RUNNING", false, true) };
    let fake = Fake {
        user_record: Some(user_record_param()),
        listed_by_cluster: Some(HashMap::from([(user_cluster_arn(), vec![ec2_task_arn("old")])])),
        run: Some(ec2_task(&p, "new", "RUNNING", true, true)),
        ..Default::default()
    }
    .with_task(old);
    let (r, _) = start(&fake, &p).await;
    assert_eq!(r.unwrap().task.arn, ec2_task_arn("new"));
    let calls = fake.calls();
    let stop = calls.iter().position(|c| *c == format!("stop {} {REPLACED_REASON}", ec2_task_arn("old"))).expect("the old task is stopped");
    assert!(stop < calls.iter().position(|c| c == "run").unwrap(), "{calls:?}");
}

#[tokio::test]
async fn a_fargate_start_never_reads_the_ec2_record() {
    let p = aws_project(StorageMode::Ephemeral);
    let fake = Fake {
        user_record: Some(user_record_param()),
        run: Some(task(&p, "f1", "RUNNING", true)),
        ..Default::default()
    };
    let (r, _) = start(&fake, &p).await;
    r.unwrap();
    assert!(!fake.called("user-record"));
    assert_eq!(*fake.list_clusters.lock().unwrap(), [cluster_arn(), cluster_arn()]);
}

#[tokio::test]
async fn an_ec2_start_without_capacity_says_so_before_anything_is_written() {
    let p = ec2_project();
    let fake = Fake::default();
    let (r, _) = start(&fake, &p).await;
    let e = r.err().unwrap();
    assert_eq!(
        e,
        "You have no EC2 capacity in dev; ask an admin to add it to your onboarding (or run the \
         project on Fargate)."
    );
    assert!(!fake.called("put-secret") && !fake.called("register") && !fake.called("run"));

    // A record that can't be used is not mistaken for none.
    let fake = Fake {
        user_record: Some(crate::aws::environments::Param::Value("{}".into())),
        ..Default::default()
    };
    let (r, _) = start(&fake, &p).await;
    let e = r.err().unwrap();
    assert!(e.contains("can't be used") && e.contains("re-apply your onboarding"), "{e}");
    assert!(!fake.called("put-secret"));
}

#[tokio::test]
async fn an_ec2_task_that_never_gets_an_instance_is_stopped_with_what_to_check() {
    let p = ec2_project();
    let fake = Fake {
        user_record: Some(user_record_param()),
        run: Some(ec2_task(&p, "e2", "PROVISIONING", false, false)),
        ..Default::default()
    };
    let (r, steps) = start(&fake, &p).await;
    let e = r.err().unwrap();
    assert!(e.contains("No EC2 instance had room"), "{e}");
    assert!(e.contains("maximum of 2 instances"), "{e}");
    assert!(steps.contains(&PROGRESS_INSTANCE.to_string()));
    assert!(fake.calls().contains(&format!("stop {} {STOP_REASON}", ec2_task_arn("e2"))));
    assert_eq!(*fake.stop_clusters.lock().unwrap(), [user_cluster_arn()]);
    assert_eq!(*fake.recorded.lock().unwrap(), vec![Some(ec2_task_arn("e2")), None]);
}

/// The record is gone (capacity removed) while the task still runs: the
/// stored ARN leads to it, and attaching needs no record.
#[tokio::test]
async fn a_running_ec2_task_is_attached_to_even_after_the_record_is_gone() {
    let mut p = ec2_project();
    p.aws.as_mut().unwrap().task_arn = Some(ec2_task_arn("e3"));
    let live = ec2_task(&p, "e3", "RUNNING", true, true);
    let fake = Fake {
        listed_by_cluster: Some(HashMap::from([(user_cluster_arn(), vec![live.arn.clone()])])),
        ..Default::default()
    }
    .with_task(live);
    let (r, _) = start(&fake, &p).await;
    let started = r.unwrap();
    assert!(!started.created);
    assert_eq!(started.task.arn, ec2_task_arn("e3"));
    assert!(!fake.called("run") && !fake.called("put-secret"));
    assert_eq!(*fake.list_clusters.lock().unwrap(), [cluster_arn(), user_cluster_arn()]);
    assert!(fake.describe_clusters.lock().unwrap().iter().all(|c| *c == user_cluster_arn()));
}

#[tokio::test]
async fn an_ec2_projects_task_is_found_and_watched_on_the_users_cluster() {
    // FindTask: the environment's cluster, the record's, and the stored
    // task's per-user cluster, without repeats.
    let mut p = ec2_project();
    let f = FindTask::new(&p, REGION, USER, Some(user_cluster_arn()));
    assert_eq!(f.clusters, ["triple-c-dev".to_string(), user_cluster_arn()]);
    p.aws.as_mut().unwrap().task_arn = Some(ec2_task_arn("e4"));
    assert_eq!(find(&p).clusters, ["triple-c-dev".to_string(), user_cluster_arn()]);
    let f = FindTask::new(&p, REGION, USER, Some(user_cluster_arn()));
    assert_eq!(f.clusters.len(), 2);

    // Reconcile with no ARN stored finds the task on the user's cluster.
    let mut orphan = ec2_project();
    orphan.status = ProjectStatus::Running;
    let live = ec2_task(&orphan, "e5", "RUNNING", true, true);
    let fake = Fake {
        listed_by_cluster: Some(HashMap::from([(user_cluster_arn(), vec![live.arn.clone()])])),
        ..Default::default()
    }
    .with_task(live);
    let (_d, store) = store_with(&orphan);
    let find = FindTask::new(&orphan, REGION, USER, Some(user_cluster_arn()));
    assert!(matches!(
        reconcile_project(&fake, &store, &orphan, &find).await.unwrap(),
        Reconciled::Live(_)
    ));
    assert_eq!(store.get(&orphan.id).unwrap().aws.unwrap().task_arn, Some(ec2_task_arn("e5")));

    // The watch groups by the task's own cluster.
    let mut running = ec2_project();
    running.status = ProjectStatus::Running;
    running.container_id = Some("http://127.0.0.1:61301/".into());
    running.aws.as_mut().unwrap().task_arn = Some(ec2_task_arn("e6"));
    let w = watched_task(&running).expect("watched");
    assert_eq!(w.group.cluster_arn, user_cluster_arn());
    let mut fargate = aws_project(StorageMode::Ephemeral);
    fargate.status = ProjectStatus::Running;
    fargate.container_id = Some("http://127.0.0.1:61302/".into());
    fargate.aws.as_mut().unwrap().task_arn = Some(task_arn("f6"));
    assert_ne!(watched_task(&fargate).unwrap().group, w.group);
}

#[tokio::test]
async fn stopping_an_ec2_task_by_its_arn_uses_its_cluster() {
    let fake = Fake::default();
    stop_task_arn(&fake, &ec2_task_arn("e7")).await.unwrap();
    assert_eq!(*fake.stop_clusters.lock().unwrap(), [USER_CLUSTER]);
}

/// E7: the entrypoint installs the VPN skill from `VPN_SUPPORT_ENABLED`, as
/// on Docker; an EC2 project with VPN support gets `1`, Fargate `0`.
#[test]
fn vpn_support_reaches_an_ec2_workspace_as_on_docker() {
    let vpn = |p: &Project| {
        let (pairs, _) = aws_env(p, &AppSettings::default()).unwrap();
        pairs.into_iter().find(|(k, _)| k == "VPN_SUPPORT_ENABLED").map(|(_, v)| v)
    };
    let mut p = ec2_project();
    p.vpn_support_enabled = true;
    assert_eq!(vpn(&p).as_deref(), Some("1"));
    p.vpn_support_enabled = false;
    assert_eq!(vpn(&p).as_deref(), Some("0"));
    assert_eq!(vpn(&aws_project(StorageMode::Ephemeral)).as_deref(), Some("0"));
}

/// Host settings' view of the user's EC2 capacity: the record's compute with
/// the fit limits, none, unusable (with the reason) or unknown.
#[test]
fn ec2_capacity_reads_as_host_settings_need_it() {
    use crate::aws::environments::{UserCompute, UserRecord};
    let compute = UserCompute {
        instance_type: "m7i.large".into(),
        max_instances: 1,
        user_tag: "alice".into(),
        cluster_arn: "arn:aws:ecs:us-west-2:123456789012:cluster/triple-c-dev--u-alice".into(),
        capacity_provider: "triple-c-dev--u-alice".into(),
    };
    let c = ec2_capacity_from(Some(UserRecord::Valid(compute.clone())));
    assert_eq!(
        c,
        Ec2Capacity::Ec2 { compute: compute.clone(), available_cpu: Some(1920), available_memory_mib: Some(7373) }
    );
    let v = serde_json::to_value(&c).unwrap();
    assert_eq!(v["state"], "ec2");
    assert_eq!(v["compute"]["instance_type"], "m7i.large");
    assert_eq!(v["available_memory_mib"], 7373);
    let unknown_type = UserCompute { instance_type: "x9.huge".into(), ..compute };
    assert!(matches!(
        ec2_capacity_from(Some(UserRecord::Valid(unknown_type))),
        Ec2Capacity::Ec2 { available_cpu: None, available_memory_mib: None, .. }
    ));
    assert_eq!(serde_json::to_value(ec2_capacity_from(Some(UserRecord::Absent))).unwrap()["state"], "none");
    let v = serde_json::to_value(ec2_capacity_from(Some(UserRecord::Unusable("schema is not 1".into())))).unwrap();
    assert_eq!(v, serde_json::json!({ "state": "unusable", "reason": "schema is not 1" }));
    assert_eq!(serde_json::to_value(ec2_capacity_from(None)).unwrap(), serde_json::json!({ "state": "unknown" }));
}

/// Reconcile never marks an exec-off task Running (an app restart during a
/// cold EC2 start): a placed or orphaned one is stopped (its ARN cleared
/// only once the stop succeeded); a recent one still waiting is Error with
/// its ARN kept.
#[tokio::test]
async fn reconcile_never_attaches_to_a_task_with_ecs_exec_off() {
    let mut p = ec2_project();
    p.status = ProjectStatus::Running;
    p.aws.as_mut().unwrap().task_arn = Some(ec2_task_arn("x"));
    let execless = |last: &str, placed: bool, created: i64| TaskInfo {
        exec_enabled: false,
        created_unix: Some(created),
        ..ec2_task(&p, "x", last, false, placed)
    };
    let find = FindTask::new(&p, REGION, USER, Some(user_cluster_arn()));
    let stopped_with_replaced = |f: &Fake| f.calls().contains(&format!("stop {} {REPLACED_REASON}", ec2_task_arn("x")));

    // Placed: stopped, Stopped, ARN cleared.
    let fake = Fake::default().with_task(execless("PENDING", true, now_unix()));
    let (_d, store) = store_with(&p);
    assert_eq!(reconcile_project(&fake, &store, &p, &find).await.unwrap(), Reconciled::Gone);
    assert!(stopped_with_replaced(&fake), "{:?}", fake.calls());
    let after = store.get(&p.id).unwrap();
    assert_eq!(after.status, ProjectStatus::Stopped);
    assert_eq!(after.aws.unwrap().task_arn, None);

    // Orphaned (still waiting, but older than any start waits): the same.
    let fake = Fake::default().with_task(execless("PROVISIONING", false, now_unix() - 3600));
    let (_d, store) = store_with(&p);
    assert_eq!(reconcile_project(&fake, &store, &p, &find).await.unwrap(), Reconciled::Gone);
    assert!(stopped_with_replaced(&fake));
    assert_eq!(store.get(&p.id).unwrap().status, ProjectStatus::Stopped);

    // The stop fails: Error, the ARN kept.
    let fake = Fake { stop_fails: true, ..Default::default() }.with_task(execless("RUNNING", true, now_unix()));
    let (_d, store) = store_with(&p);
    let e = reconcile_project(&fake, &store, &p, &find).await.unwrap_err();
    assert!(e.contains("Could not stop") && e.contains("ExpiredToken"), "{e}");
    let after = store.get(&p.id).unwrap();
    assert_eq!(after.status, ProjectStatus::Error);
    assert_eq!(after.aws.unwrap().task_arn, Some(ec2_task_arn("x")));

    // Recent and still waiting: Interrupted (Error), ARN kept, nothing stopped.
    let fake = Fake::default().with_task(execless("PROVISIONING", false, now_unix() - 60));
    let (_d, store) = store_with(&p);
    assert_eq!(reconcile_project(&fake, &store, &p, &find).await.unwrap(), Reconciled::Interrupted);
    assert!(!fake.called("stop"));
    let after = store.get(&p.id).unwrap();
    assert_eq!(after.status, ProjectStatus::Error);
    assert_eq!(after.aws.unwrap().task_arn, Some(ec2_task_arn("x")));
    assert!(INTERRUPTED.contains("Start again"));
}

#[test]
fn an_orphan_is_older_than_the_ec2_wait_plus_ten_minutes() {
    assert_eq!(ORPHAN_MARGIN, Duration::from_secs(10 * 60));
    let p = ec2_project();
    let at = |age: i64| TaskInfo { created_unix: Some(1_000_000 - age), ..ec2_task(&p, "o", "PROVISIONING", false, false) };
    let wait = WaitTiming::ec2();
    assert!(!is_orphaned(&at(19 * 60), wait, 1_000_000));
    assert!(is_orphaned(&at(21 * 60), wait, 1_000_000));
    assert!(!is_orphaned(&TaskInfo { created_unix: None, ..at(0) }, wait, 1_000_000));
}

/// A failed read of the EC2 record does not stop an attach: the stored
/// task's cluster is still searched. Only a new run needs the record.
#[tokio::test]
async fn an_ec2_record_read_error_still_attaches_and_fails_only_a_new_run() {
    let mut p = ec2_project();
    p.aws.as_mut().unwrap().task_arn = Some(ec2_task_arn("e8"));
    let live = ec2_task(&p, "e8", "RUNNING", true, true);
    let fake = Fake {
        user_record_fails: true,
        listed_by_cluster: Some(HashMap::from([(user_cluster_arn(), vec![live.arn.clone()])])),
        ..Default::default()
    }
    .with_task(live);
    let (r, _) = start(&fake, &p).await;
    let started = r.unwrap();
    assert!(!started.created);
    assert_eq!(started.task.arn, ec2_task_arn("e8"));
    assert_eq!(*fake.list_clusters.lock().unwrap(), [cluster_arn(), user_cluster_arn()]);

    // Nothing live: the start fails with the read error, before any write.
    let fake = Fake { user_record_fails: true, ..Default::default() };
    let (r, _) = start(&fake, &p).await;
    let e = r.err().unwrap();
    assert!(e.contains("Could not read your EC2 capacity record") && e.contains("Throttling"), "{e}");
    assert!(!fake.called("put-secret") && !fake.called("run"));
}
