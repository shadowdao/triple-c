//! Where `container_watch` learns that a project container stopped on its
//! own. Host-neutral shape: a reachability check, a stream of containers
//! reported dead (replayable from a point in time), and a positive-exit check
//! used to confirm each report before anything changes.
//!
//! Docker pushes `die` events. AWS has no push stream from the app's point of
//! view; its [`ExitSource::deaths`] is a `DescribeTasks` poll (every 15 s) over
//! the tasks of Running AWS projects that yields the stopped ones, which
//! `container_watch` handles the same way because every report is confirmed
//! with [`ContainerTarget::has_exited`] anyway.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::Duration;

use futures_util::future::BoxFuture;
use futures_util::{Stream, StreamExt};
use std::pin::Pin;

use super::ContainerTarget;
use crate::docker;

/// How often the AWS watch asks ECS (spec, "Lifecycle and status").
pub const AWS_POLL_INTERVAL: Duration = Duration::from_secs(15);

/// Which host's deaths to watch.
#[derive(Clone)]
pub enum ExitSource {
    Docker,
    // Constructed by the AWS host (phase 5, Task 6); until then only by tests.
    #[allow(dead_code)]
    Aws(AwsWatch),
}

impl std::fmt::Debug for ExitSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ExitSource::Docker => f.write_str("Docker"),
            ExitSource::Aws(w) => write!(f, "Aws(every {:?})", w.interval),
        }
    }
}

/// Where a task's DescribeTasks goes: the connection and region its
/// credentials and endpoint come from, and the cluster.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TaskGroup {
    pub connection_id: String,
    pub region: String,
    pub cluster_arn: String,
}

/// A Running AWS project's task, and the target a death is reported as.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WatchedTask {
    pub target: ContainerTarget,
    pub group: TaskGroup,
    pub task_arn: String,
}

/// The project-to-task lookup the AWS watch runs on, injected so it can be
/// faked (Task 6 backs it with the tunnel registry / project store and
/// `aws::tasks::stopped_among`).
pub trait AwsTaskLookup: Send + Sync {
    /// The tasks of the projects that are Running now. Read on every poll.
    fn running_tasks(&self) -> Vec<WatchedTask>;
    /// Of `arns` (all in `group`, at most 100), the ones that are STOPPED or
    /// gone. `Err` when ECS could not be asked.
    fn stopped<'a>(
        &'a self,
        group: &'a TaskGroup,
        arns: &'a [String],
    ) -> BoxFuture<'a, Result<Vec<String>, String>>;
}

/// The AWS death poll: a lookup and how often to run it.
#[derive(Clone)]
pub struct AwsWatch {
    lookup: Arc<dyn AwsTaskLookup>,
    interval: Duration,
}

#[allow(dead_code)] // Task 6
impl AwsWatch {
    pub fn new(lookup: Arc<dyn AwsTaskLookup>) -> Self {
        Self::every(lookup, AWS_POLL_INTERVAL)
    }
    pub fn every(lookup: Arc<dyn AwsTaskLookup>, interval: Duration) -> Self {
        AwsWatch { lookup, interval }
    }
}

/// DescribeTasks' batch limit.
const DESCRIBE_BATCH: usize = 100;

/// One poll: the targets of Running projects whose task stopped. Tasks are
/// asked about per group (connection + region + cluster), 100 at a time. A
/// group that can't be asked (expired SSO, network) is logged and skipped —
/// it confirms nothing, and the other groups are still watched; the next
/// poll asks again.
pub async fn poll_stopped(lookup: &dyn AwsTaskLookup) -> Vec<ContainerTarget> {
    let mut groups: HashMap<TaskGroup, Vec<WatchedTask>> = HashMap::new();
    for t in lookup.running_tasks() {
        groups.entry(t.group.clone()).or_default().push(t);
    }
    let mut dead = Vec::new();
    for (group, tasks) in groups {
        for chunk in tasks.chunks(DESCRIBE_BATCH) {
            let arns: Vec<String> = chunk.iter().map(|t| t.task_arn.clone()).collect();
            match lookup.stopped(&group, &arns).await {
                Ok(stopped) => dead.extend(
                    chunk
                        .iter()
                        .filter(|t| stopped.contains(&t.task_arn))
                        .map(|t| t.target.clone()),
                ),
                Err(e) => log::warn!(
                    "Container watch: could not check AWS tasks in {} ({}): {e}",
                    group.region,
                    group.cluster_arn
                ),
            }
        }
    }
    dead
}

/// Polls forever, `interval` apart (the first after one interval: the
/// watcher sweeps on connect), yielding each stopped task's target.
fn aws_deaths(watch: AwsWatch) -> DeathStream {
    Box::pin(futures_util::stream::unfold(
        (watch, VecDeque::new()),
        |(watch, mut queue)| async move {
            loop {
                if let Some(t) = queue.pop_front() {
                    return Some((Ok(t), (watch, queue)));
                }
                tokio::time::sleep(watch.interval).await;
                queue.extend(poll_stopped(&*watch.lookup).await);
            }
        },
    ))
}

pub type DeathStream = Pin<Box<dyn Stream<Item = Result<ContainerTarget, String>> + Send>>;

impl ExitSource {
    /// Whether the host answers at all. A `false` here is a failed connection
    /// attempt for the watcher's backoff.
    pub async fn reachable(&self) -> bool {
        match self {
            ExitSource::Docker => docker::events::ping().await,
            // No one endpoint to ping: each poll asks every group, and a
            // group that fails is skipped rather than ending the stream.
            ExitSource::Aws(_) => true,
        }
    }

    /// Containers reported dead, replayed from `since` (unix seconds). An `Err`
    /// item, or the stream ending, means the connection is gone.
    pub fn deaths(&self, since: i64) -> DeathStream {
        match self {
            ExitSource::Docker => Box::pin(
                docker::events::container_deaths(since).map(|r| r.map(ContainerTarget::docker)),
            ),
            // ECS has no replay: a task that stopped while the stream was
            // down is still STOPPED (or gone) on the next poll.
            ExitSource::Aws(watch) => aws_deaths(watch.clone()),
        }
    }
}

impl ContainerTarget {
    /// `true` only when the host positively says the container is not running
    /// (stopped, or gone). Any failure to find out is `false` — unlike
    /// [`ContainerTarget::is_running`], whose callers want errors to read as
    /// "not running".
    pub async fn has_exited(&self) -> bool {
        match self {
            ContainerTarget::Docker { container_id } => docker::events::has_exited(container_id).await,
            // The agent can only say "I answer" or "I don't"; silence is not a
            // positive exit (an SSM blip looks the same). Only the host that
            // runs the container can confirm a death — an AWS host answers
            // from ECS — so through the agent alone this never confirms one.
            ContainerTarget::Agent(_) => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    fn group(conn: &str, region: &str) -> TaskGroup {
        TaskGroup {
            connection_id: conn.into(),
            region: region.into(),
            cluster_arn: format!("arn:aws:ecs:{region}:111122223333:cluster/triple-c-dev"),
        }
    }

    fn task(n: usize, g: &TaskGroup) -> WatchedTask {
        WatchedTask {
            target: ContainerTarget::docker(format!("target-{:012}", n)),
            group: g.clone(),
            task_arn: format!("arn:aws:ecs:{}:111122223333:task/triple-c-dev/t{n}", g.region),
        }
    }

    /// Running tasks are fixed; `dead` are the stopped ARNs; a group whose
    /// region is `failing` errors. Every `stopped` call is recorded.
    struct FakeLookup {
        tasks: Mutex<Vec<WatchedTask>>,
        dead: Vec<String>,
        failing: &'static str,
        asked: Mutex<Vec<(TaskGroup, usize)>>,
        polls: Mutex<usize>,
    }

    impl FakeLookup {
        fn new(tasks: Vec<WatchedTask>, dead: Vec<String>) -> Self {
            FakeLookup {
                tasks: Mutex::new(tasks),
                dead,
                failing: "",
                asked: Mutex::new(Vec::new()),
                polls: Mutex::new(0),
            }
        }
    }

    impl AwsTaskLookup for FakeLookup {
        fn running_tasks(&self) -> Vec<WatchedTask> {
            *self.polls.lock().unwrap() += 1;
            self.tasks.lock().unwrap().clone()
        }
        fn stopped<'a>(
            &'a self,
            group: &'a TaskGroup,
            arns: &'a [String],
        ) -> BoxFuture<'a, Result<Vec<String>, String>> {
            Box::pin(async move {
                self.asked.lock().unwrap().push((group.clone(), arns.len()));
                if group.region == self.failing {
                    return Err("SSO session expired".into());
                }
                Ok(arns.iter().filter(|a| self.dead.contains(a)).cloned().collect())
            })
        }
    }

    #[tokio::test]
    async fn a_poll_asks_each_group_once_and_reports_only_stopped_tasks() {
        let (a, b) = (group("c1", "eu-west-1"), group("c2", "us-east-1"));
        let tasks = vec![task(1, &a), task(2, &b), task(3, &a)];
        let dead = vec![tasks[0].task_arn.clone(), tasks[1].task_arn.clone()];
        let lookup = FakeLookup::new(tasks.clone(), dead);
        let mut got = poll_stopped(&lookup).await;
        got.sort_by(|x, y| x.id().cmp(y.id()));
        assert_eq!(got, vec![tasks[0].target.clone(), tasks[1].target.clone()]);
        let mut asked = lookup.asked.lock().unwrap().clone();
        asked.sort_by(|x, y| x.0.region.cmp(&y.0.region));
        assert_eq!(asked, vec![(a, 2), (b, 1)]);
    }

    #[tokio::test]
    async fn a_group_that_cannot_be_asked_confirms_nothing_and_the_others_still_report() {
        let (a, b) = (group("c1", "eu-west-1"), group("c2", "us-east-1"));
        let tasks = vec![task(1, &a), task(2, &b)];
        let dead = tasks.iter().map(|t| t.task_arn.clone()).collect();
        let mut lookup = FakeLookup::new(tasks.clone(), dead);
        lookup.failing = "eu-west-1";
        assert_eq!(poll_stopped(&lookup).await, vec![tasks[1].target.clone()]);
    }

    #[tokio::test]
    async fn tasks_are_described_a_hundred_at_a_time() {
        let a = group("c1", "eu-west-1");
        let lookup = FakeLookup::new((0..250).map(|n| task(n, &a)).collect(), Vec::new());
        assert!(poll_stopped(&lookup).await.is_empty());
        let sizes: Vec<usize> = lookup.asked.lock().unwrap().iter().map(|(_, n)| *n).collect();
        assert_eq!(sizes, vec![100, 100, 50]);
    }

    #[tokio::test]
    async fn no_running_aws_projects_means_no_aws_calls() {
        let lookup = FakeLookup::new(Vec::new(), Vec::new());
        assert!(poll_stopped(&lookup).await.is_empty());
        assert!(lookup.asked.lock().unwrap().is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn the_aws_stream_polls_every_interval_and_yields_each_death() {
        let a = group("c1", "eu-west-1");
        let tasks = vec![task(1, &a), task(2, &a)];
        let lookup = Arc::new(FakeLookup::new(
            tasks.clone(),
            vec![tasks[0].task_arn.clone(), tasks[1].task_arn.clone()],
        ));
        let source = ExitSource::Aws(AwsWatch::new(lookup.clone()));
        assert!(source.reachable().await);
        let mut deaths = source.deaths(0);
        let t0 = tokio::time::Instant::now();
        let first = deaths.next().await.unwrap().unwrap();
        assert_eq!(t0.elapsed(), AWS_POLL_INTERVAL, "the first poll waits one interval");
        let second = deaths.next().await.unwrap().unwrap();
        assert_eq!(t0.elapsed(), AWS_POLL_INTERVAL, "one poll, both reported");
        let mut got = vec![first, second];
        got.sort_by(|x, y| x.id().cmp(y.id()));
        assert_eq!(got, vec![tasks[0].target.clone(), tasks[1].target.clone()]);
        assert_eq!(*lookup.polls.lock().unwrap(), 1);

        // Once the projects are no longer Running, polls go on and report nothing.
        lookup.tasks.lock().unwrap().clear();
        let quiet = tokio::time::timeout(AWS_POLL_INTERVAL * 3 + Duration::from_secs(1), deaths.next()).await;
        assert!(quiet.is_err(), "nothing to report");
        assert_eq!(*lookup.polls.lock().unwrap(), 4);
    }

    #[test]
    fn the_aws_poll_runs_every_fifteen_seconds() {
        assert_eq!(AWS_POLL_INTERVAL, Duration::from_secs(15));
    }

    #[tokio::test]
    async fn an_agent_target_never_confirms_an_exit_on_its_own() {
        // R3: Task 6 wires the tunnel registry into this arm.
        let ep = crate::host::agent::AgentEndpoint::new(
            url::Url::parse("http://127.0.0.1:47901").unwrap(),
            "fake-token",
        )
        .unwrap();
        assert!(!ContainerTarget::Agent(ep).has_exited().await);
    }
}
