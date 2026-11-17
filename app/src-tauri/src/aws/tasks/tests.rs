use std::sync::Mutex;
use std::time::Duration;

use aws_sdk_ecs::types::{
    AssignPublicIp, Container, LaunchType, ManagedAgent, ManagedAgentName, Tag, Task, TaskStopCode,
};

use super::*;
use crate::aws::environments::EnvironmentInfo;
use crate::aws::tofu_modules::{log_stream_prefix, owner_key};

const ACCOUNT: &str = "111122223333";
const REGION: &str = "eu-west-1";
const USER: &str = "AROAEXAMPLEID:alice@example.com";
const OTHER: &str = "AROAOTHERID:mallory";
const PROJECT_ID: &str = "0f1e2d3c-4b5a-6978-8796-a5b4c3d2e1f0";
const FAMILY: &str = "triple-c-dev-abcdef012345-0f1e2d3c-4b5";

fn cluster() -> String {
    format!("arn:aws:ecs:{REGION}:{ACCOUNT}:cluster/triple-c-dev")
}

fn td_arn(rev: u32) -> String {
    format!("arn:aws:ecs:{REGION}:{ACCOUNT}:task-definition/{FAMILY}:{rev}")
}

fn task_arn(id: &str) -> String {
    format!("arn:aws:ecs:{REGION}:{ACCOUNT}:task/triple-c-dev/{id}")
}

fn env_info() -> EnvironmentInfo {
    EnvironmentInfo {
        schema: 1,
        name: "dev".into(),
        template_version: 1,
        cluster_arn: cluster(),
        security_group_id: "sg-0123456789abcdef0".into(),
        subnet_ids: vec!["subnet-0a".into(), "subnet-0b".into()],
        assign_public_ip: true,
        kms_key_arn: format!("arn:aws:kms:{REGION}:{ACCOUNT}:key/1234"),
        log_group: "/triple-c/dev".into(),
        network_mode: "public".into(),
        tenancy: "strict".into(),
        max_workspace_hours: 0,
    }
}

fn is_quota_error(e: &AwsError) -> bool {
    matches!(e, AwsError::QuotaExceeded { .. })
}

fn info(id: &str, last: &str) -> TaskInfo {
    TaskInfo {
        arn: task_arn(id),
        last_status: last.into(),
        desired_status: if last == "STOPPED" {
            "STOPPED"
        } else {
            "RUNNING"
        }
        .into(),
        health: None,
        stop_code: None,
        stopped_reason: None,
        task_definition_arn: td_arn(3),
        runtime_id: None,
        container_instance_arn: None,
        exec_enabled: true,
        created_unix: None,
        exec_agent_running: false,
        exit_code: None,
        owner: Some(USER.into()),
        project: Some(PROJECT_ID.into()),
    }
}

fn ready(id: &str) -> TaskInfo {
    TaskInfo {
        runtime_id: Some(format!("{id}-123")),
        exec_agent_running: true,
        ..info(id, "RUNNING")
    }
}

/// A scripted `TaskApi`: `listed` answers ListTasks, `described` answers
/// every DescribeTasks in turn (the last one repeats), `run` answers RunTask.
#[derive(Default)]
struct Fake {
    listed: Vec<String>,
    described: Mutex<Vec<Vec<TaskInfo>>>,
    run: Mutex<Option<Result<TaskInfo, AwsError>>>,
    logs: Vec<String>,
    calls: Mutex<Vec<String>>,
    run_requests: Mutex<Vec<RunTaskInput>>,
    log_requests: Mutex<Vec<(String, String, i32)>>,
}

impl Fake {
    fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }
    fn describing(self, answers: Vec<Vec<TaskInfo>>) -> Self {
        *self.described.lock().unwrap() = answers;
        self
    }
    fn running(self, r: Result<TaskInfo, AwsError>) -> Self {
        *self.run.lock().unwrap() = Some(r);
        self
    }
}

impl TaskApi for Fake {
    async fn run_task(&self, req: RunTaskInput) -> Result<TaskInfo, AwsError> {
        self.calls.lock().unwrap().push("run".into());
        self.run_requests.lock().unwrap().push(req);
        self.run
            .lock()
            .unwrap()
            .clone()
            .unwrap_or_else(|| Ok(info("new", "PROVISIONING")))
    }
    async fn describe(&self, cluster: &str, arns: &[String]) -> Result<Vec<TaskInfo>, AwsError> {
        self.calls
            .lock()
            .unwrap()
            .push(format!("describe {cluster} {}", arns.join(",")));
        let mut d = self.described.lock().unwrap();
        let answer = if d.len() > 1 {
            d.remove(0)
        } else {
            d.first().cloned().unwrap_or_default()
        };
        Ok(answer
            .into_iter()
            .filter(|t| arns.contains(&t.arn))
            .collect())
    }
    async fn list_project_tasks(
        &self,
        cluster: &str,
        family: &str,
    ) -> Result<Vec<String>, AwsError> {
        self.calls
            .lock()
            .unwrap()
            .push(format!("list {cluster} {family}"));
        Ok(self.listed.clone())
    }
    async fn stop_task(&self, _cluster: &str, arn: &str, reason: &str) -> Result<(), AwsError> {
        self.calls
            .lock()
            .unwrap()
            .push(format!("stop {arn} {reason}"));
        Ok(())
    }
    async fn log_tail(
        &self,
        group: &str,
        stream: &str,
        limit: i32,
    ) -> Result<Vec<String>, AwsError> {
        self.log_requests
            .lock()
            .unwrap()
            .push((group.into(), stream.into(), limit));
        Ok(self.logs.clone())
    }
}

fn tags() -> Vec<(String, String)> {
    crate::aws::workspace::workspace_tags(USER, PROJECT_ID, "dev")
}

// ------------------------------------------------------------ run request

#[test]
fn on_demand_runs_fargate_on_the_environment_network_with_exec_and_the_three_tags() {
    let req = run_task_request(&env_info(), &td_arn(3), false, &tags());
    assert_eq!(req.cluster(), Some(cluster().as_str()));
    assert_eq!(req.task_definition(), Some(td_arn(3).as_str()));
    assert_eq!(req.launch_type(), Some(&LaunchType::Fargate));
    assert!(req.capacity_provider_strategy().is_empty());
    assert_eq!(req.enable_execute_command(), Some(true));
    assert_eq!(req.count(), Some(1));
    let vpc = req
        .network_configuration()
        .and_then(|n| n.awsvpc_configuration())
        .expect("awsvpc");
    assert_eq!(vpc.subnets(), ["subnet-0a", "subnet-0b"]);
    assert_eq!(vpc.security_groups(), ["sg-0123456789abcdef0"]);
    assert_eq!(vpc.assign_public_ip(), Some(&AssignPublicIp::Enabled));
    let got: Vec<(String, String)> = req
        .tags()
        .iter()
        .map(|t| (t.key().unwrap().to_string(), t.value().unwrap().to_string()))
        .collect();
    assert_eq!(got, tags());
    // Nothing project-shaped rides along (P1), and tags are given, not propagated.
    assert!(req.overrides().is_none());
    assert!(req.propagate_tags().is_none());
    assert!(req.enable_ecs_managed_tags().is_none());
}

#[test]
fn spot_uses_the_fargate_spot_capacity_provider_and_no_launch_type() {
    let req = run_task_request(&env_info(), &td_arn(3), true, &tags());
    assert_eq!(req.launch_type(), None);
    let strategy = req.capacity_provider_strategy();
    assert_eq!(strategy.len(), 1);
    assert_eq!(strategy[0].capacity_provider(), "FARGATE_SPOT");
    assert_eq!(strategy[0].weight(), 1);
}

#[test]
fn a_private_environment_gets_no_public_ip() {
    let mut env = env_info();
    env.assign_public_ip = false;
    let req = run_task_request(&env, &td_arn(3), false, &tags());
    let vpc = req
        .network_configuration()
        .unwrap()
        .awsvpc_configuration()
        .unwrap();
    assert_eq!(vpc.assign_public_ip(), Some(&AssignPublicIp::Disabled));
}

// ------------------------------------------------------------ find or run

#[tokio::test]
async fn an_existing_running_task_is_reused_and_nothing_is_run() {
    let api = Fake {
        listed: vec![task_arn("t1")],
        ..Default::default()
    }
    .describing(vec![vec![ready("t1")]]);
    let got = find_or_run(&api, &env_info(), USER, PROJECT_ID, &td_arn(4), RunOn::Fargate { spot: false })
        .await
        .unwrap();
    assert!(got.reused);
    assert_eq!(got.task.arn, task_arn("t1"));
    assert!(!api.calls().contains(&"run".to_string()));
    assert_eq!(api.calls()[0], format!("list {} {FAMILY}", cluster()));
}

#[tokio::test]
async fn a_pending_task_is_reused_too() {
    let api = Fake {
        listed: vec![task_arn("t1")],
        ..Default::default()
    }
    .describing(vec![vec![info("t1", "PENDING")]]);
    let got = find_or_run(&api, &env_info(), USER, PROJECT_ID, &td_arn(3), RunOn::Fargate { spot: false })
        .await
        .unwrap();
    assert!(got.reused);
    assert_eq!(got.task.last_status, "PENDING");
    assert!(!api.calls().contains(&"run".to_string()));
}

#[tokio::test]
async fn a_task_of_another_owner_or_project_in_the_family_is_never_reused() {
    let mut foreign = info("theirs", "RUNNING");
    foreign.owner = Some(OTHER.into());
    let mut other_project = info("other-project", "RUNNING");
    other_project.project = Some("ffffffff-0000-0000-0000-000000000000".into());
    let mut untagged = info("untagged", "RUNNING");
    untagged.owner = None;
    untagged.project = None;
    let api = Fake {
        listed: vec![
            task_arn("theirs"),
            task_arn("other-project"),
            task_arn("untagged"),
        ],
        ..Default::default()
    }
    .describing(vec![vec![foreign, other_project, untagged]]);
    let got = find_or_run(&api, &env_info(), USER, PROJECT_ID, &td_arn(3), RunOn::Fargate { spot: false })
        .await
        .unwrap();
    assert!(!got.reused);
    assert_eq!(got.task.arn, task_arn("new"));
    assert_eq!(api.calls().last().unwrap(), "run");
}

#[tokio::test]
async fn a_task_already_stopping_is_not_reused() {
    let mut stopping = info("t1", "RUNNING");
    stopping.desired_status = "STOPPED".into();
    let api = Fake {
        listed: vec![task_arn("t1"), task_arn("t2")],
        ..Default::default()
    }
    .describing(vec![vec![stopping, info("t2", "DEPROVISIONING")]]);
    let got = find_or_run(&api, &env_info(), USER, PROJECT_ID, &td_arn(3), RunOn::Fargate { spot: false })
        .await
        .unwrap();
    assert!(!got.reused);
}

#[tokio::test]
async fn a_running_task_is_preferred_over_a_pending_one() {
    let api = Fake {
        listed: vec![task_arn("p"), task_arn("r")],
        ..Default::default()
    }
    .describing(vec![vec![info("p", "PENDING"), ready("r")]]);
    let got = find_project_task(&api, &cluster(), FAMILY, USER, PROJECT_ID)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(got.arn, task_arn("r"));
}

#[tokio::test]
async fn nothing_listed_means_no_describe_and_a_run_with_the_exact_revision_and_tags() {
    let api = Fake::default();
    let got = find_or_run(&api, &env_info(), USER, PROJECT_ID, &td_arn(7), RunOn::Fargate { spot: true })
        .await
        .unwrap();
    assert!(!got.reused);
    assert_eq!(api.calls().len(), 2, "{:?}", api.calls());
    let reqs = api.run_requests.lock().unwrap();
    assert_eq!(reqs[0].task_definition(), Some(td_arn(7).as_str()));
    assert_eq!(reqs[0].launch_type(), None);
    let keys: Vec<&str> = reqs[0].tags().iter().filter_map(|t| t.key()).collect();
    assert_eq!(
        keys,
        ["triple-c:environment", "triple-c:owner", "triple-c:project"]
    );
    assert_eq!(
        reqs[0]
            .tags()
            .iter()
            .find(|t| t.key() == Some("triple-c:owner"))
            .unwrap()
            .value(),
        Some(USER)
    );
}

#[tokio::test]
async fn a_bare_family_or_foreign_arn_is_refused_before_any_call() {
    let api = Fake::default();
    for bad in [
        FAMILY.to_string(),
        format!("arn:aws:ecs:{REGION}:{ACCOUNT}:task-definition/{FAMILY}"),
        format!("arn:aws:ecs:{REGION}:{ACCOUNT}:task-definition/{FAMILY}:latest"),
        "arn:aws:ecs:eu-west-1:1:task-definition/someone-else:1".to_string(),
    ] {
        let err = find_or_run(&api, &env_info(), USER, PROJECT_ID, &bad, RunOn::Fargate { spot: false })
            .await
            .unwrap_err();
        assert!(matches!(err, AwsError::Other { .. }), "{bad}: {err:?}");
    }
    assert!(api.calls().is_empty());
}

#[tokio::test]
async fn a_quota_refusal_is_reported_once_without_retrying() {
    let api = Fake::default().running(Err(quota_error(
        "You've reached the limit on the number of vCPUs you can run concurrently",
    )));
    let err = find_or_run(&api, &env_info(), USER, PROJECT_ID, &td_arn(3), RunOn::Fargate { spot: false })
        .await
        .unwrap_err();
    assert!(is_quota_error(&err));
    let text = err.to_string();
    assert!(text.contains("limit"), "{text}");
    assert!(text.contains("quota"), "{text}");
    assert!(text.contains("vCPUs"), "{text}");
    assert_eq!(
        api.calls().iter().filter(|c| *c == "run").count(),
        1,
        "no retry"
    );
}

// ------------------------------------------------------------ quota mapping

#[test]
fn quota_texts_are_recognised_and_others_are_not() {
    for q in [
        "You've reached the limit on the number of vCPUs you can run concurrently",
        "You\u{2019}ve reached the limit on the number of tasks you can run concurrently",
        "LimitExceededException: too many",
        "Service quota exceeded for Fargate On-Demand vCPU",
        "limit exceeded",
    ] {
        assert!(looks_like_quota(q), "{q}");
    }
    for not in [
        "Capacity is unavailable at this time. Please try again later",
        "AccessDeniedException: not authorized",
        "CannotPullContainerError",
    ] {
        assert!(!looks_like_quota(not), "{not}");
    }
}

#[test]
fn quota_mapping_turns_other_errors_into_quota_errors_and_leaves_the_rest() {
    let q = quota_or(AwsError::Other {
        message: "ClientException: You've reached the limit on the number of vCPUs".into(),
    });
    assert!(is_quota_error(&q));
    // The frontend's `quota_exceeded` kind (lib/types.ts).
    let v = serde_json::to_value(&q).unwrap();
    assert_eq!(v["kind"], "quota_exceeded");
    assert!(v["message"].as_str().unwrap().contains("vCPUs"));
    let denied = AwsError::AccessDenied {
        action: "ecs:RunTask".into(),
        message: "limit".into(),
    };
    assert_eq!(quota_or(denied.clone()), denied);
    assert!(!is_quota_error(&AwsError::Other {
        message: "x".into()
    }));
}

#[test]
fn run_task_failures_become_errors_and_quota_failures_quota_errors() {
    let ok = run_task_outcome(vec![info("a", "PROVISIONING")], vec![]).unwrap();
    assert_eq!(ok.arn, task_arn("a"));
    let err = run_task_outcome(
        vec![],
        vec![(Some("RESOURCE:ENI".into()), Some("no ENIs left".into()))],
    )
    .unwrap_err();
    assert!(!is_quota_error(&err));
    assert!(err.to_string().contains("RESOURCE:ENI"), "{err}");
    assert!(err.to_string().contains("no ENIs left"), "{err}");
    let err = run_task_outcome(
        vec![],
        vec![(
            Some("You've reached the limit on the number of tasks you can run concurrently".into()),
            None,
        )],
    )
    .unwrap_err();
    assert!(is_quota_error(&err));
    let err = run_task_outcome(vec![], vec![]).unwrap_err();
    assert!(err.to_string().contains("no task"), "{err}");
}

// ------------------------------------------------------------ wait

fn timing() -> WaitTiming {
    WaitTiming {
        interval: Duration::from_secs(5),
        timeout: Duration::from_secs(360),
    }
}

fn logs() -> TaskLogs {
    TaskLogs {
        group: "/triple-c/dev".into(),
        stream_prefix: log_stream_prefix(&owner_key(REGION, USER), PROJECT_ID),
    }
}

#[tokio::test(start_paused = true)]
async fn wait_polls_until_running_with_the_exec_agent_and_a_runtime_id() {
    let mut half = info("t1", "RUNNING");
    half.runtime_id = Some("rt".into());
    let api = Fake::default().describing(vec![
        vec![info("t1", "PROVISIONING")],
        vec![info("t1", "PENDING")],
        vec![half],
        vec![ready("t1")],
    ]);
    let seen = Mutex::new(Vec::new());
    let t0 = tokio::time::Instant::now();
    let got = wait_ready(
        &api,
        &cluster(),
        &task_arn("t1"),
        &logs(),
        timing(),
        |t: &TaskInfo| seen.lock().unwrap().push(t.last_status.clone()),
    )
    .await
    .unwrap();
    assert_eq!(got, ready("t1"));
    assert_eq!(
        *seen.lock().unwrap(),
        ["PROVISIONING", "PENDING", "RUNNING", "RUNNING"]
    );
    assert_eq!(t0.elapsed(), Duration::from_secs(15));
}

#[tokio::test(start_paused = true)]
async fn a_task_that_stops_while_waiting_reports_its_reason_and_last_log_lines() {
    let mut stopped = info("t1", "STOPPED");
    stopped.stop_code = Some("EssentialContainerExited".into());
    stopped.stopped_reason = Some("Essential container in task exited".into());
    let api = Fake {
        logs: vec!["Triple-C: starting".into(), "fatal: boom".into()],
        ..Default::default()
    }
    .describing(vec![vec![info("t1", "PENDING")], vec![stopped]]);
    let err = wait_ready(
        &api,
        &cluster(),
        &task_arn("t1"),
        &logs(),
        timing(),
        |_: &TaskInfo| {},
    )
    .await
    .unwrap_err();
    let text = err.to_string();
    assert!(
        text.contains("Essential container in task exited"),
        "{text}"
    );
    assert!(text.contains("Triple-C: starting\nfatal: boom"), "{text}");
    let reqs = api.log_requests.lock().unwrap();
    assert_eq!(
        *reqs,
        [(
            "/triple-c/dev".to_string(),
            format!("{}/workspace/t1", logs().stream_prefix),
            50
        )]
    );
}

#[tokio::test(start_paused = true)]
async fn a_task_being_stopped_fails_the_wait_at_once() {
    let mut stopping = info("t1", "RUNNING");
    stopping.desired_status = "STOPPED".into();
    stopping.stop_code = Some("SpotInterruption".into());
    stopping.stopped_reason = Some("Your Spot Task was interrupted.".into());
    let api = Fake::default().describing(vec![vec![stopping]]);
    let err = wait_ready(
        &api,
        &cluster(),
        &task_arn("t1"),
        &logs(),
        timing(),
        |_: &TaskInfo| {},
    )
    .await
    .unwrap_err();
    assert!(
        err.to_string().contains("Your Spot Task was interrupted."),
        "{err}"
    );
}

#[tokio::test(start_paused = true)]
async fn a_task_that_never_gets_ready_times_out_with_its_last_status() {
    let api = Fake::default().describing(vec![vec![info("t1", "PENDING")]]);
    let t0 = tokio::time::Instant::now();
    let err = wait_ready(
        &api,
        &cluster(),
        &task_arn("t1"),
        &logs(),
        timing(),
        |_: &TaskInfo| {},
    )
    .await
    .unwrap_err();
    assert!(t0.elapsed() >= Duration::from_secs(360));
    assert!(t0.elapsed() < Duration::from_secs(366));
    let text = err.to_string();
    assert!(text.contains("6 minutes"), "{text}");
    assert!(text.contains("PENDING"), "{text}");
}

#[tokio::test(start_paused = true)]
async fn a_task_that_vanishes_is_an_error_not_a_hang() {
    let api = Fake::default().describing(vec![vec![]]);
    let err = wait_ready(
        &api,
        &cluster(),
        &task_arn("t1"),
        &logs(),
        timing(),
        |_: &TaskInfo| {},
    )
    .await
    .unwrap_err();
    assert!(err.to_string().contains("no longer"), "{err}");
}

#[test]
fn the_default_wait_is_five_seconds_for_six_minutes() {
    let t = WaitTiming::default();
    assert_eq!(t.interval, Duration::from_secs(5));
    assert_eq!(t.timeout, Duration::from_secs(360));
}

// ------------------------------------------------------------ stop / logs

#[tokio::test]
async fn stop_passes_the_reason() {
    let api = Fake::default();
    api.stop_task(&cluster(), &task_arn("t1"), STOP_REASON)
        .await
        .unwrap();
    assert_eq!(
        api.calls(),
        [format!("stop {} Stopped from Triple-C", task_arn("t1"))]
    );
}

#[test]
fn the_log_stream_is_prefix_container_task_id() {
    assert_eq!(
        task_log_stream("abc/p", &task_arn("0123abcd")),
        "abc/p/workspace/0123abcd"
    );
}

#[tokio::test]
async fn stopped_among_reports_stopped_and_vanished_tasks_only() {
    let api = Fake::default().describing(vec![vec![
        ready("up"),
        info("down", "STOPPED"),
        info("pending", "PENDING"),
    ]]);
    let arns = vec![
        task_arn("up"),
        task_arn("down"),
        task_arn("pending"),
        task_arn("gone"),
    ];
    let mut got = stopped_among(&api, &cluster(), &arns).await.unwrap();
    got.sort_by(|a, b| a.0.cmp(&b.0));
    assert_eq!(
        got,
        [
            (task_arn("down"), StopKind::Exited(None)),
            (task_arn("gone"), StopKind::Exited(None)),
        ]
    );
}

// ------------------------------------------------------------ classify

#[test]
fn classify_stop_follows_the_spec_table() {
    let with = |code: Option<&str>, reason: Option<&str>| TaskInfo {
        stop_code: code.map(String::from),
        stopped_reason: reason.map(String::from),
        ..info("t", "STOPPED")
    };
    assert_eq!(
        classify_stop(&with(Some("EssentialContainerExited"), Some("Essential container in task exited"))),
        StopKind::Exited(Some("Essential container in task exited".into()))
    );
    // The entrypoint's idle stop exits with its own status.
    let idle = TaskInfo { exit_code: Some(IDLE_EXIT_CODE), ..with(Some("EssentialContainerExited"), None) };
    assert_eq!(classify_stop(&idle), StopKind::Idle);
    // So does its lifetime-cap stop.
    let capped = TaskInfo { exit_code: Some(LIFETIME_EXIT_CODE), ..with(Some("EssentialContainerExited"), None) };
    assert_eq!(classify_stop(&capped), StopKind::LifetimeCap);
    let user_capped = TaskInfo { exit_code: Some(LIFETIME_EXIT_CODE), ..with(Some("UserInitiated"), None) };
    assert_eq!(classify_stop(&user_capped), StopKind::UserInitiated);
    let crashed = TaskInfo { exit_code: Some(137), ..with(Some("EssentialContainerExited"), None) };
    assert_eq!(classify_stop(&crashed), StopKind::Exited(None));
    // Only with that stop code: a user stop of a task that exits 75 is a user stop.
    let user = TaskInfo { exit_code: Some(IDLE_EXIT_CODE), ..with(Some("UserInitiated"), None) };
    assert_eq!(classify_stop(&user), StopKind::UserInitiated);
    assert_eq!(
        classify_stop(&with(Some("SpotInterruption"), None)),
        StopKind::Spot
    );
    assert_eq!(
        classify_stop(&with(
            Some("TaskFailedToStart"),
            Some("CannotPullContainerError: nope")
        )),
        StopKind::FailedToStart("CannotPullContainerError: nope".into())
    );
    assert!(matches!(
        classify_stop(&with(Some("TaskFailedToStart"), None)),
        StopKind::FailedToStart(r) if !r.is_empty()
    ));
    assert_eq!(
        classify_stop(&with(Some("UserInitiated"), Some("Stopped from Triple-C"))),
        StopKind::UserInitiated
    );
    assert_eq!(
        classify_stop(&with(Some("TerminationNotice"), None)),
        StopKind::Exited(None)
    );
    assert_eq!(
        classify_stop(&with(Some("ServiceSchedulerInitiated"), None)),
        StopKind::Exited(None)
    );
    assert_eq!(classify_stop(&with(None, Some("  "))), StopKind::Exited(None));
}

// ------------------------------------------------------------ SDK mapping

#[test]
fn task_info_reads_the_workspace_container_and_its_exec_agent() {
    let task = Task::builder()
        .task_arn(task_arn("t1"))
        .last_status("RUNNING")
        .desired_status("RUNNING")
        .health_status(aws_sdk_ecs::types::HealthStatus::Healthy)
        .stop_code(TaskStopCode::SpotInterruption)
        .stopped_reason("r")
        .task_definition_arn(td_arn(3))
        .containers(
            Container::builder()
                .name("sidecar")
                .runtime_id("wrong")
                .exit_code(1)
                .build(),
        )
        .containers(
            Container::builder()
                .name("workspace")
                .runtime_id("rt-1")
                .exit_code(75)
                .managed_agents(
                    ManagedAgent::builder()
                        .name(ManagedAgentName::ExecuteCommandAgent)
                        .last_status("RUNNING")
                        .build(),
                )
                .build(),
        )
        .tags(Tag::builder().key("triple-c:owner").value(USER).build())
        .tags(
            Tag::builder()
                .key("triple-c:project")
                .value(PROJECT_ID)
                .build(),
        )
        .build();
    let i = task_info(&task);
    assert_eq!(i.arn, task_arn("t1"));
    assert_eq!(i.health.as_deref(), Some("HEALTHY"));
    assert_eq!(i.stop_code.as_deref(), Some("SpotInterruption"));
    assert_eq!(i.runtime_id.as_deref(), Some("rt-1"));
    assert!(i.exec_agent_running);
    assert_eq!(i.exit_code, Some(75), "the workspace container's, not the sidecar's");
    assert_eq!(i.owner.as_deref(), Some(USER));
    assert_eq!(i.project.as_deref(), Some(PROJECT_ID));

    let bare = task_info(&Task::builder().task_arn("x").last_status("PENDING").build());
    assert!(!bare.exec_agent_running);
    assert_eq!(bare.runtime_id, None);
    assert_eq!(bare.exit_code, None);
    assert_eq!(bare.owner, None);
}

// ------------------------------------------------------------- EC2 (phase 7)

fn user_cluster() -> String {
    format!("arn:aws:ecs:{REGION}:{ACCOUNT}:cluster/triple-c-dev--u-alice")
}

fn compute() -> UserCompute {
    UserCompute {
        instance_type: "m7i.large".into(),
        max_instances: 2,
        user_tag: "alice".into(),
        cluster_arn: user_cluster(),
        capacity_provider: "triple-c-dev--u-alice".into(),
    }
}

#[test]
fn an_ec2_run_uses_the_users_cluster_and_capacity_provider_and_no_network() {
    let c = compute();
    let req = ec2_run_task_request(&c, &td_arn(3), &tags());
    assert_eq!(req.cluster(), Some(user_cluster().as_str()));
    assert_eq!(req.task_definition(), Some(td_arn(3).as_str()));
    assert_eq!(req.launch_type(), None, "exclusive with a capacity provider strategy");
    let strategy = req.capacity_provider_strategy();
    assert_eq!(strategy.len(), 1);
    assert_eq!(strategy[0].capacity_provider(), "triple-c-dev--u-alice");
    assert_eq!(strategy[0].weight(), 1);
    assert!(req.network_configuration().is_none(), "bridge networking (R2)");
    assert_eq!(req.enable_execute_command(), Some(true));
    assert_eq!(req.count(), Some(1));
    assert!(req.client_token().is_some_and(|t| !t.is_empty()));
    let got: Vec<(String, String)> = req
        .tags()
        .iter()
        .map(|t| (t.key().unwrap().to_string(), t.value().unwrap().to_string()))
        .collect();
    assert_eq!(got, tags());
    assert!(req.overrides().is_none());
    assert!(req.propagate_tags().is_none());
    assert!(req.enable_ecs_managed_tags().is_none());
}

#[tokio::test]
async fn an_ec2_find_or_run_looks_and_runs_on_the_users_cluster() {
    let api = Fake::default();
    let c = compute();
    let got = find_or_run(&api, &env_info(), USER, PROJECT_ID, &td_arn(3), RunOn::Ec2(&c))
        .await
        .unwrap();
    assert!(!got.reused);
    assert_eq!(api.calls(), vec![format!("list {} {FAMILY}", user_cluster()), "run".to_string()]);
    let req = &api.run_requests.lock().unwrap()[0];
    assert_eq!(req.cluster(), Some(user_cluster().as_str()));
    assert_eq!(req.capacity_provider_strategy()[0].capacity_provider(), "triple-c-dev--u-alice");
    assert_eq!(RunOn::Ec2(&c).cluster(&env_info()), user_cluster());
    assert_eq!(RunOn::Fargate { spot: true }.cluster(&env_info()), cluster());
}

#[tokio::test]
async fn access_denied_on_the_users_cluster_says_the_triple_c_user_value_is_wrong() {
    let api = Fake::default().running(Err(AwsError::AccessDenied {
        action: "ecs:RunTask".into(),
        message: "not authorized to perform ecs:RunTask on cluster/triple-c-dev--u-alice".into(),
    }));
    let c = compute();
    let e = find_or_run(&api, &env_info(), USER, PROJECT_ID, &td_arn(3), RunOn::Ec2(&c))
        .await
        .unwrap_err();
    let AwsError::AccessDenied { action, message } = &e else { panic!("{e:?}") };
    assert_eq!(action, "ecs:RunTask");
    assert!(message.starts_with(EC2_TAG_MISMATCH), "{message}");
    assert!(message.contains("not authorized"), "AWS's words are kept: {message}");
    assert!(EC2_TAG_MISMATCH.contains("different triple-c-user value"));
    assert!(EC2_TAG_MISMATCH.contains("Ask an admin"));

    // The same refusal on Fargate is left as it is.
    let api = Fake::default().running(Err(AwsError::AccessDenied {
        action: "ecs:RunTask".into(),
        message: "nope".into(),
    }));
    let e = find_or_run(&api, &env_info(), USER, PROJECT_ID, &td_arn(3), RunOn::Fargate { spot: false })
        .await
        .unwrap_err();
    assert_eq!(e, AwsError::AccessDenied { action: "ecs:RunTask".into(), message: "nope".into() });
}

#[test]
fn a_placement_failure_on_ec2_says_no_instance_has_room() {
    let e = ec2_run_error(
        run_task_outcome(Vec::new(), vec![(Some("RESOURCE:MEMORY".into()), None)]).unwrap_err(),
    );
    assert!(e.to_string().contains("RESOURCE:MEMORY"), "{e}");
    assert!(e.to_string().contains("No instance of your EC2 capacity has room"), "{e}");
    // Anything else passes through.
    let q = quota_error("limit");
    assert_eq!(ec2_run_error(q.clone()), q);
}

#[test]
fn an_ec2_task_without_an_instance_is_waiting_for_one() {
    let provisioning = info("t", "PROVISIONING");
    assert!(waiting_for_instance(&provisioning));
    assert!(waiting_for_instance(&info("t", "PENDING")));
    let placed = TaskInfo {
        container_instance_arn: Some(format!(
            "arn:aws:ecs:{REGION}:{ACCOUNT}:container-instance/triple-c-dev--u-alice/abc"
        )),
        ..info("t", "PENDING")
    };
    assert!(!waiting_for_instance(&placed));
    assert!(!waiting_for_instance(&info("t", "RUNNING")));
    assert!(!waiting_for_instance(&info("t", "STOPPED")));
    // The EC2 wait leaves room for an instance launch and an image pull.
    assert_eq!(WaitTiming::ec2().timeout, Duration::from_secs(600));
    assert_eq!(WaitTiming::ec2().interval, WaitTiming::default().interval);
}

#[test]
fn task_info_reads_the_container_instance() {
    let t = Task::builder()
        .task_arn(task_arn("t"))
        .container_instance_arn("arn:aws:ecs:eu-west-1:111122223333:container-instance/c/1")
        .build();
    assert_eq!(
        task_info(&t).container_instance_arn.as_deref(),
        Some("arn:aws:ecs:eu-west-1:111122223333:container-instance/c/1")
    );
    assert_eq!(task_info(&Task::builder().build()).container_instance_arn, None);
}
