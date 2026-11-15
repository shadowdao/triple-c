use std::sync::Mutex;

use aws_sdk_ecs::operation::register_task_definition::RegisterTaskDefinitionInput;
use aws_sdk_ecs::operation::run_task::RunTaskInput;

use super::*;
use crate::aws::tasks::TaskInfo;
use crate::aws::tofu_modules::owner_key;
use crate::aws::workspace::{AccessPointRef, FileSystemRef, TaskDefinitionRef};

const REGION: &str = "eu-west-1";
const ACCOUNT: &str = "111122223333";
const USER: &str = "AROAEXAMPLEID:alice@example.com";
const PROJECT: &str = "0f1e2d3c-4b5a-6978-8796-a5b4c3d2e1f0";

fn ok() -> String {
    owner_key(REGION, USER)
}

fn family() -> String {
    task_family("dev", &ok(), PROJECT)
}

fn task_arn(id: &str) -> String {
    format!("arn:aws:ecs:{REGION}:{ACCOUNT}:task/triple-c-dev/{id}")
}

#[derive(Default)]
struct Fake {
    calls: Mutex<Vec<String>>,
    record: Option<String>,
    role: Option<Knowable>,
    secret: Option<String>,
    ap_states: Mutex<Vec<Option<String>>>,
    live_tasks: Vec<TaskInfo>,
    revisions: Vec<TaskDefinitionRef>,
    access_points: Vec<AccessPointRef>,
    fail_secret_delete: bool,
}

impl Fake {
    fn log(&self, s: String) {
        self.calls.lock().unwrap().push(s);
    }
    fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }
}

impl RuntimeApi for Fake {
    async fn environment_record(&self, env: &str) -> Result<Option<String>, AwsError> {
        self.log(format!("record {env}"));
        Ok(self.record.clone())
    }
    async fn role_exists(&self, name: &str) -> Result<Knowable, AwsError> {
        self.log(format!("role {name}"));
        Ok(self.role.clone().unwrap_or(Knowable::Yes))
    }
    async fn secret_string(&self, name: &str) -> Result<String, AwsError> {
        self.log(format!("secret {name}"));
        self.secret.clone().ok_or_else(|| other("no secret"))
    }
    async fn access_point_state(&self, id: &str) -> Result<Option<String>, AwsError> {
        self.log(format!("ap {id}"));
        let mut s = self.ap_states.lock().unwrap();
        Ok(if s.len() > 1 { s.remove(0) } else { s.first().cloned().flatten() })
    }
    async fn deregister_task_definition(&self, arn: &str) -> Result<(), AwsError> {
        self.log(format!("deregister {arn}"));
        Ok(())
    }
    async fn delete_task_definitions(&self, arns: &[String]) -> Result<(), AwsError> {
        self.log(format!("delete-td {}", arns.join(",")));
        Ok(())
    }
    async fn delete_secret(&self, name: &str) -> Result<(), AwsError> {
        self.log(format!("delete-secret {name}"));
        if self.fail_secret_delete {
            return Err(other("denied"));
        }
        Ok(())
    }
    async fn delete_access_point(&self, id: &str) -> Result<(), AwsError> {
        self.log(format!("delete-ap {id}"));
        Ok(())
    }
}

impl TaskApi for Fake {
    async fn run_task(&self, _req: RunTaskInput) -> Result<TaskInfo, AwsError> {
        unreachable!("removal never runs a task")
    }
    async fn describe(&self, _c: &str, arns: &[String]) -> Result<Vec<TaskInfo>, AwsError> {
        Ok(self.live_tasks.iter().filter(|t| arns.contains(&t.arn)).cloned().collect())
    }
    async fn list_project_tasks(&self, _c: &str, _f: &str) -> Result<Vec<String>, AwsError> {
        Ok(self.live_tasks.iter().map(|t| t.arn.clone()).collect())
    }
    async fn stop_task(&self, cluster: &str, arn: &str, reason: &str) -> Result<(), AwsError> {
        self.log(format!("stop {cluster} {arn} {reason}"));
        Ok(())
    }
    async fn log_tail(&self, _g: &str, _s: &str, _l: i32) -> Result<Vec<String>, AwsError> {
        Ok(Vec::new())
    }
}

impl WorkspaceApi for Fake {
    async fn find_file_system(&self, _token: &str) -> Result<Option<FileSystemRef>, AwsError> {
        Ok(Some(FileSystemRef { id: "fs-1".into(), owner: Some(USER.into()) }))
    }
    async fn access_points(&self, _fs: &str) -> Result<Vec<AccessPointRef>, AwsError> {
        Ok(self.access_points.clone())
    }
    async fn create_access_point(
        &self,
        _fs: &str,
        _root: &str,
        _tags: &[(String, String)],
    ) -> Result<String, AwsError> {
        unreachable!()
    }
    async fn put_project_secret(
        &self,
        _n: &str,
        _k: &str,
        _j: &str,
        _t: &[(String, String)],
    ) -> Result<String, AwsError> {
        unreachable!()
    }
    async fn task_definition_revisions(&self, _f: &str) -> Result<Vec<String>, AwsError> {
        Ok(self.revisions.iter().map(|r| r.arn.clone()).collect())
    }
    async fn describe_task_definition(
        &self,
        arn: &str,
    ) -> Result<Option<TaskDefinitionRef>, AwsError> {
        Ok(self.revisions.iter().find(|r| r.arn == arn).cloned())
    }
    async fn register_task_definition(
        &self,
        _req: RegisterTaskDefinitionInput,
    ) -> Result<String, AwsError> {
        unreachable!()
    }
}

fn record() -> String {
    serde_json::json!({
        "schema": 1, "name": "dev", "template_version": 1,
        "cluster_arn": format!("arn:aws:ecs:{REGION}:{ACCOUNT}:cluster/triple-c-dev"),
        "security_group_id": "sg-0123456789abcdef0",
        "subnet_ids": ["subnet-0a", "subnet-0b"],
        "assign_public_ip": true,
        "kms_key_arn": format!("arn:aws:kms:{REGION}:{ACCOUNT}:key/1234-abcd"),
        "log_group": "/triple-c/dev", "network_mode": "public", "tenancy": "strict"
    })
    .to_string()
}

#[tokio::test]
async fn the_environment_is_read_with_one_lookup_and_validated() {
    let fake = Fake { record: Some(record()), ..Default::default() };
    let env = read_environment(&fake, "dev", ACCOUNT, REGION).await.unwrap();
    assert_eq!(env.log_group, "/triple-c/dev");
    assert_eq!(fake.calls(), vec!["record dev"]);

    let missing = Fake::default();
    let e = read_environment(&missing, "dev", ACCOUNT, REGION).await.unwrap_err();
    assert!(e.to_string().contains("was not found"), "{e}");

    // A record from another account is refused, by field name only.
    let foreign = Fake { record: Some(record()), ..Default::default() };
    let e = read_environment(&foreign, "dev", "999999999999", REGION).await.unwrap_err();
    assert!(e.to_string().contains("cluster_arn"), "{e}");
}

#[tokio::test]
async fn a_missing_task_role_means_not_joined_and_a_denied_lookup_lets_the_start_try() {
    let no = Fake { role: Some(Knowable::No), ..Default::default() };
    let e = check_onboarded(&no, "dev", &ok()).await.unwrap_err();
    assert!(e.to_string().contains("not joined"), "{e}");
    assert_eq!(no.calls(), vec![format!("role triple-c-dev-{}-task", ok())]);
    let unknown = Fake { role: Some(Knowable::Unknown), ..Default::default() };
    assert!(check_onboarded(&unknown, "dev", &ok()).await.is_ok());
}

#[test]
fn the_agent_token_is_read_from_the_secret_without_echoing_it() {
    let json = r#"{"TRIPLEC_AGENT_TOKEN":"abc-DEF_123","OTHER":"SENTINEL-VALUE-7f3a"}"#;
    assert_eq!(agent_token_from_secret(json).unwrap(), "abc-DEF_123");
    for bad in [
        r#"{"OTHER":"SENTINEL-VALUE-7f3a"}"#,
        r#"{"TRIPLEC_AGENT_TOKEN":""}"#,
        r#"{"TRIPLEC_AGENT_TOKEN":"has space"}"#,
        "SENTINEL-VALUE-7f3a",
    ] {
        let e = agent_token_from_secret(bad).unwrap_err();
        assert!(!e.contains("SENTINEL") && !e.contains("space"), "{e}");
    }
}

#[tokio::test]
async fn the_token_comes_from_the_projects_own_secret() {
    let fake = Fake {
        secret: Some(r#"{"TRIPLEC_AGENT_TOKEN":"tok"}"#.into()),
        ..Default::default()
    };
    assert_eq!(read_agent_token(&fake, "dev", &ok(), PROJECT).await.unwrap(), "tok");
    assert_eq!(fake.calls(), vec![format!("secret triple-c/dev/{}/{PROJECT}", ok())]);
}

#[tokio::test(start_paused = true)]
async fn a_new_access_point_is_waited_for_until_available() {
    let fake = Fake {
        ap_states: Mutex::new(vec![Some("creating".into()), Some("creating".into()), Some("available".into())]),
        ..Default::default()
    };
    wait_access_point(&fake, "fsap-1", PollTiming::default()).await.unwrap();
    assert_eq!(fake.calls().len(), 3);

    let stuck = Fake { ap_states: Mutex::new(vec![Some("creating".into())]), ..Default::default() };
    let e = wait_access_point(&stuck, "fsap-1", PollTiming::default()).await.unwrap_err();
    assert!(e.to_string().contains("did not become available"), "{e}");

    let gone = Fake { ap_states: Mutex::new(vec![Some("deleting".into())]), ..Default::default() };
    assert!(wait_access_point(&gone, "fsap-1", PollTiming::default()).await.is_err());
}

#[test]
fn task_arns_name_their_cluster() {
    let arn = task_arn("abc123");
    assert_eq!(cluster_of_task(&arn), Some("triple-c-dev"));
    assert_eq!(
        cluster_arn_of_task(&arn).as_deref(),
        Some(format!("arn:aws:ecs:{REGION}:{ACCOUNT}:cluster/triple-c-dev").as_str())
    );
    assert_eq!(cluster_of_task("arn:aws:ecs:r:a:task/abc"), None, "old format, no cluster");
    assert_eq!(cluster_of_task("task/triple-c-dev/abc"), None);
    assert_eq!(cluster_of_task("arn:aws:ecs:r:a:task/c/a/b"), None);
}

fn live(id: &str) -> TaskInfo {
    TaskInfo {
        arn: task_arn(id),
        last_status: "RUNNING".into(),
        desired_status: "RUNNING".into(),
        health: None,
        stop_code: None,
        stopped_reason: None,
        task_definition_arn: format!(
            "arn:aws:ecs:{REGION}:{ACCOUNT}:task-definition/{}:2",
            family()
        ),
        runtime_id: None,
        exec_agent_running: false,
        owner: Some(USER.into()),
        project: Some(PROJECT.into()),
    }
}

fn rev(n: u32, owner: &str) -> TaskDefinitionRef {
    TaskDefinitionRef {
        arn: format!("arn:aws:ecs:{REGION}:{ACCOUNT}:task-definition/{}:{n}", family()),
        family: family(),
        owner: Some(owner.into()),
        project: Some(PROJECT.into()),
        fingerprint: None,
    }
}

#[tokio::test]
async fn removal_stops_the_task_retires_own_revisions_and_schedules_the_secret() {
    let fake = Fake {
        live_tasks: vec![live("t1")],
        revisions: vec![rev(2, USER), rev(3, "AROAOTHER:mallory"), rev(1, USER)],
        access_points: vec![AccessPointRef {
            id: "fsap-found".into(),
            file_system_id: "fs-1".into(),
            root_path: Some(access_point_root(&ok(), PROJECT)),
            owner: Some(USER.into()),
            project: Some(PROJECT.into()),
            state: Some("available".into()),
        }],
        ..Default::default()
    };
    let key = ok();
    let t0 = task_arn("t0");
    let mut r = RemoveRequest {
        env: "dev",
        region: REGION,
        owner_user_id: USER,
        owner_key: &key,
        project_id: PROJECT,
        stored_task_arn: Some(&t0),
        stored_access_point_id: Some("fsap-stored"),
        discard_data: false,
    };
    remove_workspace_resources(&fake, &fake, &fake, &r).await.unwrap();
    let calls = fake.calls();
    let secret = project_secret_name("dev", &ok(), PROJECT);
    assert_eq!(
        calls,
        vec![
            format!("stop triple-c-dev {} {STOP_REASON}", task_arn("t1")),
            format!("stop triple-c-dev {} {STOP_REASON}", task_arn("t0")),
            format!("deregister {}", rev(2, USER).arn),
            format!("deregister {}", rev(1, USER).arn),
            format!("delete-td {},{}", rev(2, USER).arn, rev(1, USER).arn),
            format!("delete-secret {secret}"),
        ],
        "another user's revision is left alone and the access point is kept"
    );

    let keep = Fake { access_points: fake.access_points.clone(), ..Default::default() };
    r.discard_data = true;
    remove_workspace_resources(&keep, &keep, &keep, &r).await.unwrap();
    let calls = keep.calls();
    assert!(calls.contains(&"delete-ap fsap-found".to_string()), "{calls:?}");
    assert!(calls.contains(&"delete-ap fsap-stored".to_string()), "{calls:?}");
}

#[tokio::test]
async fn a_failed_step_is_reported_but_the_rest_still_runs() {
    let fake = Fake { fail_secret_delete: true, ..Default::default() };
    let key = ok();
    let r = RemoveRequest {
        env: "dev",
        region: REGION,
        owner_user_id: USER,
        owner_key: &key,
        project_id: PROJECT,
        stored_task_arn: None,
        stored_access_point_id: Some("fsap-1"),
        discard_data: true,
    };
    let e = remove_workspace_resources(&fake, &fake, &fake, &r).await.unwrap_err();
    assert!(e.to_string().contains("secret"), "{e}");
    assert!(fake.calls().contains(&"delete-ap fsap-1".to_string()));
}
