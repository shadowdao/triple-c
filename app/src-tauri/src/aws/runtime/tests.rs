use std::sync::Mutex;

use aws_sdk_ecs::operation::register_task_definition::RegisterTaskDefinitionInput;
use aws_sdk_ecs::operation::run_task::RunTaskInput;

use super::*;
use crate::aws::tasks::TaskInfo;
use crate::aws::tofu_modules::{owner_key, workspace_access_point_root};
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
    /// The clusters ListTasks was asked about, by name.
    lists: Mutex<Vec<String>>,
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
    async fn user_record(&self, env: &str, owner_key: &str) -> Result<crate::aws::environments::Param, AwsError> {
        self.log(format!("user-record {env} {owner_key}"));
        Ok(crate::aws::environments::Param::Missing)
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
    async fn list_project_tasks(&self, c: &str, _f: &str) -> Result<Vec<String>, AwsError> {
        // By cluster name or ARN, like ECS.
        let name = c.rsplit_once(":cluster/").map_or(c, |(_, n)| n);
        self.lists.lock().unwrap().push(name.to_string());
        Ok(self
            .live_tasks
            .iter()
            .filter(|t| cluster_of_task(&t.arn) == Some(name))
            .map(|t| t.arn.clone())
            .collect())
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
    async fn project_secret_value(&self, _n: &str) -> Result<Option<String>, AwsError> {
        unreachable!()
    }
    async fn create_project_secret(
        &self,
        n: &str,
        k: &str,
        j: &str,
        t: &[(String, String)],
    ) -> Result<Option<String>, AwsError> {
        let _ = (n, k, j, t);
        unreachable!()
    }
    async fn delete_project_secret(&self, _n: &str) -> Result<(), AwsError> {
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
        container_instance_arn: None,
        exec_enabled: true,
        created_unix: None,
        exec_agent_running: false,
        exit_code: None,
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
async fn removal_stops_the_task_leaves_the_revisions_and_schedules_the_secret() {
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
        stored_workspace_access_point_id: None,
        ec2_cluster: None,
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
            format!("delete-secret {secret}"),
        ],
        "no task definition is touched (R8) and the access point is kept"
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
        stored_workspace_access_point_id: None,
        ec2_cluster: None,
        discard_data: true,
    };
    let e = remove_workspace_resources(&fake, &fake, &fake, &r).await.unwrap_err();
    assert!(e.to_string().contains("secret"), "{e}");
    assert!(fake.calls().contains(&"delete-ap fsap-1".to_string()));
}

#[tokio::test]
async fn removal_with_discard_deletes_both_access_points_and_without_keeps_both() {
    let ap = |id: &str, root: String| AccessPointRef {
        id: id.into(),
        file_system_id: "fs-1".into(),
        root_path: Some(root),
        owner: Some(USER.into()),
        project: Some(PROJECT.into()),
        state: Some("available".into()),
    };
    let points = vec![
        ap("fsap-home", access_point_root(&ok(), PROJECT)),
        ap("fsap-ws", workspace_access_point_root(&ok(), PROJECT)),
        AccessPointRef {
            owner: Some("AROAOTHER:mallory".into()),
            ..ap("fsap-foreign", workspace_access_point_root(&ok(), PROJECT))
        },
        AccessPointRef {
            project: Some("another".into()),
            ..ap("fsap-other-project", workspace_access_point_root(&ok(), PROJECT))
        },
    ];
    let key = ok();
    let mut r = RemoveRequest {
        env: "dev",
        region: REGION,
        owner_user_id: USER,
        owner_key: &key,
        project_id: PROJECT,
        stored_task_arn: None,
        stored_access_point_id: Some("fsap-home"),
        // Kept in the store after switching EFS -> Hybrid.
        stored_workspace_access_point_id: Some("fsap-ws-stored"),
        ec2_cluster: None,
        discard_data: false,
    };
    let keep = Fake { access_points: points.clone(), ..Default::default() };
    remove_workspace_resources(&keep, &keep, &keep, &r).await.unwrap();
    assert!(
        keep.calls().iter().all(|c| !c.starts_with("delete-ap")),
        "{:?}",
        keep.calls()
    );

    r.discard_data = true;
    let discard = Fake { access_points: points, ..Default::default() };
    remove_workspace_resources(&discard, &discard, &discard, &r).await.unwrap();
    let mut deleted: Vec<String> = discard
        .calls()
        .into_iter()
        .filter_map(|c| c.strip_prefix("delete-ap ").map(String::from))
        .collect();
    deleted.sort();
    assert_eq!(deleted, vec!["fsap-home", "fsap-ws", "fsap-ws-stored"]);
}

// ------------------------------------------------------------- EC2 (phase 7)

fn ec2_task_arn(id: &str) -> String {
    format!("arn:aws:ecs:{REGION}:{ACCOUNT}:task/triple-c-dev--u-alice/{id}")
}

#[test]
fn a_stored_task_on_a_per_user_cluster_names_that_cluster() {
    assert_eq!(
        ec2_cluster_of_task("dev", &ec2_task_arn("t")).as_deref(),
        Some(format!("arn:aws:ecs:{REGION}:{ACCOUNT}:cluster/triple-c-dev--u-alice").as_str())
    );
    // The environment's own cluster, another environment's per-user
    // cluster and anything else are not.
    assert_eq!(ec2_cluster_of_task("dev", &task_arn("t")), None);
    assert_eq!(ec2_cluster_of_task("prod", &ec2_task_arn("t")), None);
    let odd = format!("arn:aws:ecs:{REGION}:{ACCOUNT}:task/triple-c-dev--u-a--b/t");
    assert_eq!(ec2_cluster_of_task("dev", &odd), None);
    assert_eq!(ec2_cluster_of_task("dev", "not-an-arn"), None);
}

#[tokio::test]
async fn the_user_record_is_read_and_validated() {
    struct Rec(Param);
    impl Rec {
        async fn read(&self) -> UserRecord {
            let fake = RecFake(self.0.clone(), Mutex::new(Vec::new()));
            let r = read_user_record(&fake, "dev", &ok(), ACCOUNT, REGION).await.unwrap();
            assert_eq!(*fake.1.lock().unwrap(), vec![format!("dev {}", ok())]);
            r
        }
    }
    let valid = serde_json::json!({
        "schema": 1, "instance_type": "m7i.large", "max_instances": 2, "user_tag": "alice",
        "cluster_arn": format!("arn:aws:ecs:{REGION}:{ACCOUNT}:cluster/triple-c-dev--u-alice"),
        "capacity_provider": "triple-c-dev--u-alice",
    })
    .to_string();
    let UserRecord::Valid(c) = Rec(Param::Value(valid)).read().await else { panic!() };
    assert_eq!((c.instance_type.as_str(), c.max_instances), ("m7i.large", 2));
    assert_eq!(Rec(Param::Missing).read().await, UserRecord::Absent);
    assert!(matches!(Rec(Param::Denied).read().await, UserRecord::Unusable(_)));
    assert!(matches!(Rec(Param::Value("{}".into())).read().await, UserRecord::Unusable(_)));
}

/// Only `user_record` is asked of it.
struct RecFake(Param, Mutex<Vec<String>>);

impl RuntimeApi for RecFake {
    async fn environment_record(&self, _env: &str) -> Result<Option<String>, AwsError> {
        unreachable!()
    }
    async fn user_record(&self, env: &str, owner_key: &str) -> Result<Param, AwsError> {
        self.1.lock().unwrap().push(format!("{env} {owner_key}"));
        Ok(self.0.clone())
    }
    async fn role_exists(&self, _name: &str) -> Result<Knowable, AwsError> {
        unreachable!()
    }
    async fn secret_string(&self, _name: &str) -> Result<String, AwsError> {
        unreachable!()
    }
    async fn access_point_state(&self, _id: &str) -> Result<Option<String>, AwsError> {
        unreachable!()
    }
    async fn delete_secret(&self, _name: &str) -> Result<(), AwsError> {
        unreachable!()
    }
    async fn delete_access_point(&self, _id: &str) -> Result<(), AwsError> {
        unreachable!()
    }
}

#[tokio::test]
async fn removal_finds_and_stops_a_task_on_the_users_ec2_cluster() {
    let key = ok();
    let user_cluster = format!("arn:aws:ecs:{REGION}:{ACCOUNT}:cluster/triple-c-dev--u-alice");
    let base = RemoveRequest {
        env: "dev",
        region: REGION,
        owner_user_id: USER,
        owner_key: &key,
        project_id: PROJECT,
        stored_task_arn: None,
        stored_access_point_id: None,
        stored_workspace_access_point_id: None,
        ec2_cluster: Some(&user_cluster),
        discard_data: false,
    };
    // Found through the record's cluster.
    let mut t = live("e1");
    t.arn = ec2_task_arn("e1");
    let fake = Fake { live_tasks: vec![t.clone()], ..Default::default() };
    remove_workspace_resources(&fake, &fake, &fake, &base).await.unwrap();
    assert_eq!(*fake.lists.lock().unwrap(), ["triple-c-dev", "triple-c-dev--u-alice"]);
    let calls = fake.calls();
    assert!(
        calls.contains(&format!("stop {user_cluster} {} {STOP_REASON}", ec2_task_arn("e1"))),
        "{calls:?}"
    );

    // The record is gone: the stored ARN still leads to its cluster.
    let stored = ec2_task_arn("e1");
    let fake = Fake { live_tasks: vec![t], ..Default::default() };
    let r = RemoveRequest { ec2_cluster: None, stored_task_arn: Some(&stored), ..base };
    remove_workspace_resources(&fake, &fake, &fake, &r).await.unwrap();
    let stops: Vec<String> = fake.calls().into_iter().filter(|c| c.starts_with("stop")).collect();
    assert_eq!(stops.len(), 1, "{stops:?}");
    assert!(stops[0].contains(&ec2_task_arn("e1")), "{stops:?}");

    // A stored ARN on someone else's kind of cluster is not stopped.
    let foreign = format!("arn:aws:ecs:{REGION}:{ACCOUNT}:task/other-cluster/x");
    let fake = Fake::default();
    let r = RemoveRequest { ec2_cluster: None, stored_task_arn: Some(&foreign), ..base };
    remove_workspace_resources(&fake, &fake, &fake, &r).await.unwrap();
    assert!(!fake.calls().iter().any(|c| c.starts_with("stop")));
}
