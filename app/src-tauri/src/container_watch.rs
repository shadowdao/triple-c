//! Notices a project container that stopped without the app asking.
//!
//! Until this existed the app only ever learned a container's state from its
//! own commands and from `reconcile_project_statuses`, which the frontend runs
//! once, when Docker first answers. A container that died underneath a running
//! app — the entrypoint's keepalive killed, an OOM, `docker stop` from a shell,
//! a daemon restart — left its project at Running indefinitely: every terminal
//! disconnected, while the sidebar, Start/Stop and Project Home all still
//! described a live container.
//!
//! One subscription to the host's death reports (`host::watch`; for Docker,
//! the daemon's event stream filtered to container `die` events, so an idle
//! daemon costs nothing and the app hears about a death as it happens — no
//! polling loop). Each `die` naming a project's container is
//! confirmed with one inspect before anything changes (an event can be
//! replayed, and a restart can reuse the id), and the flip itself is
//! [`ProjectsStore::mark_exited`]'s compare-and-set, which never touches a
//! project a command owns and refuses if the store changed while the inspect
//! was in flight. The frontend hears it on [`CONTAINER_EXITED_EVENT`].
//!
//! The stream drops whenever the daemon does. Each (re)subscription asks for
//! events since just before it, and first re-checks every Running project once —
//! the only window that replay cannot cover is a daemon restart, which forgets
//! its event history, and that is exactly when containers go away.

use std::future::Future;
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures_util::StreamExt;
use serde::Serialize;
use tauri::{AppHandle, Emitter};
use tokio::sync::watch;

use crate::aws::tasks::StopKind;
use crate::host::watch::ExitSource;
use crate::host::ContainerTarget;
use crate::models::{Project, ProjectStatus};
use crate::storage::projects_store::{ExitMark, ProjectsStore};

/// Emitted as a [`ContainerExited`] whenever this module flips a project to
/// Stopped.
pub const CONTAINER_EXITED_EVENT: &str = "project-container-exited";

const RETRY_MIN: Duration = Duration::from_secs(5);
const RETRY_MAX: Duration = Duration::from_secs(60);
/// A stream that stayed up this long (or delivered anything) was a working
/// connection, so its loss starts the backoff over. Anything shorter — the
/// daemon answers ping but the events call fails — keeps backing off, or it
/// would re-sweep and re-log every five seconds forever.
const STABLE_AFTER: Duration = Duration::from_secs(60);
/// How often `settle` re-confirms when the store keeps changing under it.
const SETTLE_ATTEMPTS: usize = 3;

/// How the app found out, which decides whether it is news to the user.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExitReason {
    /// The container died while this app was running and watching — any
    /// sessions it had were just cut off.
    Exited,
    /// Found already stopped on the first look of this run: it went down
    /// while the app was closed (a host reboot, typically). Nothing the user
    /// can see was interrupted, so it is not news.
    FoundStopped,
}

#[derive(Debug, Clone, Serialize)]
pub struct ContainerExited<'a> {
    pub project: &'a Project,
    pub reason: ExitReason,
    /// AWS: why ECS says the task stopped (P8). `None` on Docker, or when
    /// the watch has no reason on record.
    pub stop: Option<StopNote>,
}

/// Why an AWS task stopped, for the UI's toast.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StopNote {
    /// `spot`, `idle`, `lifetime`, `failed` (the project goes to Error), `user` (someone
    /// stopped it: no toast) or `exited` (on its own).
    pub kind: &'static str,
    /// ECS's stopped reason, or a fixed text.
    pub message: Option<String>,
}

impl StopNote {
    pub fn of(kind: &StopKind) -> StopNote {
        let (kind, message) = match kind {
            StopKind::Spot => ("spot", Some("Spot interruption".to_string())),
            StopKind::Idle => ("idle", None),
            StopKind::LifetimeCap => ("lifetime", None),
            StopKind::FailedToStart(r) => ("failed", Some(r.clone())),
            StopKind::UserInitiated => ("user", None),
            StopKind::Exited(r) => ("exited", r.clone()),
        };
        StopNote { kind, message }
    }
}

fn project_busy(project_id: &str) -> bool {
    crate::project_lock::held(project_id).is_some()
}

/// Confirm `container` really stopped and, if a project was Running on it,
/// flip that project. Returns what changed.
async fn settle(store: &ProjectsStore, container: &ContainerTarget) -> Option<Project> {
    let container_id = container.id();
    for _ in 0..SETTLE_ATTEMPTS {
        // Cheap and allocation-free: most containers that die are not ours
        // (probe and scratch containers, other software on the daemon), and
        // those cost no inspect at all.
        let seen = store.running_on(container_id)?;
        // Positive exit only: a host that can't answer must not mark every
        // project Stopped.
        if !container.has_exited().await {
            return None;
        }
        match store.mark_exited(container_id, &seen, project_busy) {
            ExitMark::Flipped(p) => {
                // An AWS task that stopped by itself (idle, lifetime cap,
                // crash) took its persistent terminals with it.
                if p.is_aws() {
                    crate::storage::terminal_store::clear(&p.id);
                }
                return Some(*p);
            }
            ExitMark::NotApplicable => return None,
            // The store moved while the host was answering — possibly a stop and
            // start of this very container. The inspect is stale; ask again.
            ExitMark::Changed => continue,
        }
    }
    None
}

/// Re-check every Running project once. Used on each (re)subscription.
async fn sweep(store: &ProjectsStore) -> Vec<Project> {
    let mut changed = Vec::new();
    for p in store.list() {
        if p.status != ProjectStatus::Running {
            continue;
        }
        let Some(container) = ContainerTarget::for_project(&p) else { continue };
        if let Some(c) = settle(store, &container).await {
            changed.push(c);
        }
    }
    changed
}

/// The wait before connection attempt `n + 1`, after `n` consecutive
/// unproductive ones: 5 s, 10 s, 20 s, 40 s, then 60 s for good.
fn retry_delay(consecutive_failures: u32) -> Duration {
    let doublings = consecutive_failures.saturating_sub(1).min(8);
    (RETRY_MIN * 2u32.pow(doublings)).min(RETRY_MAX)
}

/// `fut`, unless the app starts shutting down first. Every host call in here
/// goes through this: a wedged daemon must not hold shutdown for its full
/// budget.
async fn unless_cancelled<T>(
    cancel: &mut watch::Receiver<bool>,
    fut: impl Future<Output = T>,
) -> Option<T> {
    if *cancel.borrow() {
        return None;
    }
    tokio::select! {
        // `Err` means the sender is gone, i.e. so is the app.
        _ = cancel.changed() => None,
        v = fut => Some(v),
    }
}

/// One connection: ping, sweep, then follow the event stream until it ends.
/// `None` when cancelled; otherwise whether the connection was a working one
/// (see [`STABLE_AFTER`]).
async fn watch_once(
    store: &ProjectsStore,
    source: &ExitSource,
    cancel: &mut watch::Receiver<bool>,
    sweep_reason: ExitReason,
    on_change: &impl Fn(&Project, ExitReason),
) -> Option<bool> {
    if !unless_cancelled(cancel, source.reachable()).await? {
        return Some(false);
    }
    // A second's overlap: the host replays from `since`, so a death during
    // the sweep below is still delivered. Duplicates are harmless — `settle`
    // confirms with an inspect every time.
    let since = chrono::Utc::now().timestamp() - 1;
    for p in unless_cancelled(cancel, sweep(store)).await? {
        on_change(&p, sweep_reason);
    }
    let mut deaths = source.deaths(since);
    let connected = Instant::now();
    let mut delivered = false;
    loop {
        match unless_cancelled(cancel, deaths.next()).await? {
            Some(Ok(container)) => {
                delivered = true;
                if let Some(p) = unless_cancelled(cancel, settle(store, &container)).await? {
                    on_change(&p, ExitReason::Exited);
                }
            }
            Some(Err(e)) => {
                log::info!("Container watch: event stream ended ({e}) — reconnecting");
                break;
            }
            None => {
                log::info!("Container watch: event stream closed — reconnecting");
                break;
            }
        }
    }
    Some(delivered || connected.elapsed() >= STABLE_AFTER)
}

/// Watch until `cancel` flips, calling `on_change` for every project whose
/// container is found to have stopped on its own. Separate from [`spawn`] so a
/// test can drive it without a Tauri app.
pub(crate) async fn run(
    store: Arc<ProjectsStore>,
    source: ExitSource,
    mut cancel: watch::Receiver<bool>,
    on_change: impl Fn(&Project, ExitReason),
) {
    // Only the first look of the run finds containers that stopped while the
    // app was closed. A sweep after a reconnect finds ones that died while we
    // were running — the daemon restarting is the usual cause — and those
    // cut off live sessions just as a `die` event would.
    let mut sweep_reason = ExitReason::FoundStopped;
    let mut failures: u32 = 0;
    loop {
        let Some(healthy) = watch_once(&store, &source, &mut cancel, sweep_reason, &on_change).await else {
            return;
        };
        sweep_reason = ExitReason::Exited;
        failures = if healthy { 1 } else { failures + 1 };
        unless_cancelled(&mut cancel, tokio::time::sleep(retry_delay(failures))).await;
        if *cancel.borrow() {
            return;
        }
    }
}

/// Start a watcher on `source` for the life of the app. One per host kind:
/// Docker's event stream, and (Task 6) the AWS task poll.
pub fn spawn(
    app: AppHandle,
    store: Arc<ProjectsStore>,
    source: ExitSource,
    cancel: watch::Receiver<bool>,
) -> tauri::async_runtime::JoinHandle<()> {
    tauri::async_runtime::spawn(async move {
        let store_for_aws = store.clone();
        run(store, source, cancel, move |p, reason| {
            log::warn!(
                "Project '{}' ({}): its container stopped without the app asking ({:?}) — marking Stopped",
                p.name,
                p.id,
                reason
            );
            // Why ECS stopped it, read before the tunnel (and with it the
            // registry's record) goes.
            let kind = p
                .is_aws()
                .then(|| p.container_id.as_deref().and_then(crate::host::tunnels::stop_kind))
                .flatten();
            let mut shown = p.clone();
            // A task that never got going is an error, not a stop (P8).
            if let Some(StopKind::FailedToStart(_)) = kind {
                if store_for_aws.update_status(&p.id, ProjectStatus::Error).is_ok() {
                    shown.status = ProjectStatus::Error;
                }
            }
            // An AWS task that stopped: its tunnel leads nowhere now, and its
            // task ARN is history (the data's access point stays).
            if p.is_aws() {
                if let Some(a) = p.aws.as_ref() {
                    let _ = store_for_aws.set_aws_runtime(
                        &p.id,
                        None,
                        a.access_point_id.clone(),
                        a.task_definition_arn.clone(),
                        a.workspace_access_point_id.clone(),
                    );
                }
                let id = p.id.clone();
                if let Some(arn) = p.aws.as_ref().and_then(|a| a.task_arn.clone()) {
                    tauri::async_runtime::spawn(async move {
                        crate::host::tunnels::close_if_task(&id, &arn).await
                    });
                }
            }
            if let Some(store_p) = store_for_aws.get(&p.id) {
                shown = store_p;
            }
            let stop = kind.as_ref().map(StopNote::of);
            let _ = app.emit(CONTAINER_EXITED_EVENT, ContainerExited { project: &shown, reason, stop });
        })
        .await;
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retries_back_off_to_a_minute_and_stay_there() {
        let secs: Vec<u64> = (1..=7).map(|n| retry_delay(n).as_secs()).collect();
        assert_eq!(secs, vec![5, 10, 20, 40, 60, 60, 60]);
        assert_eq!(retry_delay(0), RETRY_MIN);
        assert_eq!(retry_delay(u32::MAX), RETRY_MAX);
    }

    #[test]
    fn the_event_says_why_so_the_ui_can_tell_news_from_housekeeping() {
        let p = Project::new("demo".to_string(), Vec::new());
        let v = serde_json::to_value(ContainerExited { project: &p, reason: ExitReason::Exited, stop: None })
            .unwrap();
        assert_eq!(v["reason"], "exited");
        assert_eq!(v["project"]["name"], "demo");
        assert!(v["stop"].is_null());
        let v = serde_json::to_value(ContainerExited {
            project: &p,
            reason: ExitReason::FoundStopped,
            stop: Some(StopNote::of(&StopKind::Spot)),
        })
        .unwrap();
        assert_eq!(v["reason"], "found_stopped");
        assert_eq!(v["stop"], serde_json::json!({ "kind": "spot", "message": "Spot interruption" }));
        assert_eq!(CONTAINER_EXITED_EVENT, "project-container-exited");
    }

    #[test]
    fn every_stop_kind_has_a_note_for_the_ui() {
        let cases = [
            (StopKind::Spot, "spot", Some("Spot interruption")),
            (StopKind::Idle, "idle", None),
            (StopKind::LifetimeCap, "lifetime", None),
            (StopKind::FailedToStart("CannotPull".into()), "failed", Some("CannotPull")),
            (StopKind::UserInitiated, "user", None),
            (StopKind::Exited(Some("OOM".into())), "exited", Some("OOM")),
            (StopKind::Exited(None), "exited", None),
        ];
        for (k, kind, message) in cases {
            let n = StopNote::of(&k);
            assert_eq!((n.kind, n.message.as_deref()), (kind, message), "{k:?}");
        }
    }

    /// The shutdown path: a host call that never answers (a wedged daemon)
    /// must give way to cancellation at once, not hold teardown.
    #[tokio::test]
    async fn a_hung_docker_call_yields_to_cancellation() {
        let (tx, mut rx) = watch::channel(false);
        let canceller = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(20)).await;
            let _ = tx.send(true);
        });
        let hung = std::future::pending::<()>();
        let out = tokio::time::timeout(Duration::from_secs(2), unless_cancelled(&mut rx, hung)).await;
        assert_eq!(out.expect("returned promptly"), None);
        canceller.await.unwrap();
        // Already cancelled: it does not even start the call.
        assert_eq!(unless_cancelled(&mut rx, async { 1 }).await, None);
    }

    /// The AWS source drives the same loop: it is "reachable" without a
    /// ping, polls its lookup every interval, and gives way to cancellation.
    /// No project is Running, so nothing is confirmed (and nothing reaches
    /// Docker or AWS).
    #[tokio::test(start_paused = true)]
    async fn an_aws_source_polls_its_lookup_until_cancelled() {
        use crate::host::watch::{AwsTaskLookup, AwsWatch, TaskGroup, WatchedTask};
        use futures_util::future::BoxFuture;
        use std::sync::atomic::{AtomicUsize, Ordering};

        struct Lookup(AtomicUsize);
        impl AwsTaskLookup for Lookup {
            fn running_tasks(&self) -> Vec<WatchedTask> {
                self.0.fetch_add(1, Ordering::SeqCst);
                Vec::new()
            }
            fn stopped<'a>(
                &'a self,
                _: &'a TaskGroup,
                _: &'a [String],
            ) -> BoxFuture<'a, Result<Vec<String>, String>> {
                Box::pin(async { Ok(Vec::new()) })
            }
        }

        let dir = std::env::temp_dir().join(format!("triple-c-watch-aws-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir).unwrap();
        let store = Arc::new(ProjectsStore::over_file_for_test(dir.join("projects.json"), Vec::new()));
        let lookup = Arc::new(Lookup(AtomicUsize::new(0)));
        let source = ExitSource::Aws(AwsWatch::every(lookup.clone(), Duration::from_secs(15)));
        let (cancel_tx, cancel_rx) = watch::channel(false);
        let watcher = tokio::spawn(run(store, source, cancel_rx, |_, _| panic!("nothing stopped")));
        tokio::time::sleep(Duration::from_secs(46)).await;
        assert_eq!(lookup.0.load(Ordering::SeqCst), 3);
        let _ = cancel_tx.send(true);
        tokio::time::timeout(Duration::from_secs(1), watcher)
            .await
            .expect("stops at once")
            .unwrap();
        std::fs::remove_dir_all(&dir).ok();
    }

    /// End to end against a real daemon: a Running project whose container is
    /// killed from outside the app is flipped to Stopped and reported as a live
    /// exit; one found already stopped on the first look is reported as such;
    /// and one whose container is still up is left alone.
    #[tokio::test]
    #[ignore = "needs a Docker daemon; creates and removes throwaway containers"]
    async fn a_container_killed_outside_the_app_flips_its_project_to_stopped() {
        use std::process::Command;
        fn docker_cli(args: &[&str]) -> String {
            let out = Command::new("docker").args(args).output().expect("docker cli");
            assert!(out.status.success(), "docker {:?}: {}", args, String::from_utf8_lossy(&out.stderr));
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        }
        let tag = uuid::Uuid::new_v4().simple().to_string();
        let doomed = docker_cli(&["run", "-d", "--name", &format!("tc-watch-a-{tag}"), "alpine:3", "sleep", "300"]);
        let alive = docker_cli(&["run", "-d", "--name", &format!("tc-watch-b-{tag}"), "alpine:3", "sleep", "300"]);
        let already = docker_cli(&["create", "--name", &format!("tc-watch-c-{tag}"), "alpine:3", "true"]);

        let dir = std::env::temp_dir().join(format!("triple-c-watch-{tag}"));
        std::fs::create_dir_all(&dir).unwrap();
        let running = |name: &str, cid: &str| {
            let mut p = Project::new(name.to_string(), Vec::new());
            p.container_id = Some(cid.to_string());
            p.status = ProjectStatus::Running;
            p
        };
        let (a, b, c) = (running("a", &doomed), running("b", &alive), running("c", &already));
        let (a_id, b_id, c_id) = (a.id.clone(), b.id.clone(), c.id.clone());
        let store = Arc::new(ProjectsStore::over_file_for_test(dir.join("projects.json"), vec![a, b, c]));

        let (cancel_tx, cancel_rx) = watch::channel(false);
        let (seen_tx, mut seen_rx) = tokio::sync::mpsc::unbounded_channel::<(String, ExitReason)>();
        let watcher = tokio::spawn(run(store.clone(), ExitSource::Docker, cancel_rx, move |p, why| {
            let _ = seen_tx.send((p.id.clone(), why));
        }));
        let first = tokio::time::timeout(Duration::from_secs(10), seen_rx.recv()).await;
        // Subscribed by now (the sweep precedes it, and `since` overlaps);
        // kill from "outside".
        docker_cli(&["kill", &doomed]);
        let second = tokio::time::timeout(Duration::from_secs(15), seen_rx.recv()).await;

        let t0 = Instant::now();
        let _ = cancel_tx.send(true);
        let _ = watcher.await;
        let shutdown = t0.elapsed();
        let statuses: Vec<ProjectStatus> =
            [&a_id, &b_id, &c_id].iter().map(|id| store.get(id).unwrap().status).collect();
        docker_cli(&["rm", "-f", &doomed, &alive, &already]);
        std::fs::remove_dir_all(&dir).ok();

        assert_eq!(first.expect("sweep within 10s"), Some((c_id, ExitReason::FoundStopped)));
        assert_eq!(second.expect("die within 15s"), Some((a_id, ExitReason::Exited)));
        assert_eq!(statuses, vec![ProjectStatus::Stopped, ProjectStatus::Running, ProjectStatus::Stopped]);
        assert!(shutdown < Duration::from_secs(1), "stopped watching in {shutdown:?}");
    }
}
