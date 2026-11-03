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
//! One subscription to Docker's event stream, filtered daemon-side to container
//! `die` events, so an idle daemon costs nothing and the app hears about a death
//! as it happens. No polling loop. Each `die` naming a project's container is
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

use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bollard::errors::Error as DockerError;
use bollard::models::{EventMessage, EventMessageTypeEnum};
use bollard::system::EventsOptions;
use bollard::Docker;
use futures_util::StreamExt;
use serde::Serialize;
use tauri::{AppHandle, Emitter};
use tokio::sync::watch;

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
}

/// The container id a Docker event reports as having died, if it is one.
pub(crate) fn died_container(msg: &EventMessage) -> Option<&str> {
    if msg.typ != Some(EventMessageTypeEnum::CONTAINER) || msg.action.as_deref() != Some("die") {
        return None;
    }
    msg.actor.as_ref()?.id.as_deref().filter(|id| !id.is_empty())
}

/// `true` only when Docker positively says the container is not running: it
/// inspects as stopped, or it no longer exists. Any other failure is "don't
/// know" — `docker::is_container_running` maps every error to `false`, which
/// is right for its callers and wrong here, where it would mark every project
/// Stopped the moment the daemon hiccups.
async fn has_exited(docker: &Docker, container_id: &str) -> bool {
    match docker.inspect_container(container_id, None).await {
        Ok(info) => !info.state.and_then(|s| s.running).unwrap_or(false),
        Err(DockerError::DockerResponseServerError { status_code: 404, .. }) => true,
        Err(_) => false,
    }
}

fn project_busy(project_id: &str) -> bool {
    crate::project_lock::held(project_id).is_some()
}

/// Confirm `container_id` really stopped and, if a project was Running on it,
/// flip that project. Returns what changed.
async fn settle(docker: &Docker, store: &ProjectsStore, container_id: &str) -> Option<Project> {
    for _ in 0..SETTLE_ATTEMPTS {
        // Cheap and allocation-free: most containers that die are not ours
        // (probe and scratch containers, other software on the daemon), and
        // those cost no inspect at all.
        let seen = store.running_on(container_id)?;
        if !has_exited(docker, container_id).await {
            return None;
        }
        match store.mark_exited(container_id, &seen, project_busy) {
            ExitMark::Flipped(p) => return Some(*p),
            ExitMark::NotApplicable => return None,
            // The store moved while Docker was answering — possibly a stop and
            // start of this very container. The inspect is stale; ask again.
            ExitMark::Changed => continue,
        }
    }
    None
}

/// Re-check every Running project once. Used on each (re)subscription.
async fn sweep(docker: &Docker, store: &ProjectsStore) -> Vec<Project> {
    let mut changed = Vec::new();
    for p in store.list() {
        if p.status != ProjectStatus::Running {
            continue;
        }
        let Some(cid) = p.container_id.as_deref() else { continue };
        if let Some(c) = settle(docker, store, cid).await {
            changed.push(c);
        }
    }
    changed
}

fn die_filter() -> HashMap<String, Vec<String>> {
    HashMap::from([
        ("type".to_string(), vec!["container".to_string()]),
        ("event".to_string(), vec!["die".to_string()]),
    ])
}

/// The wait before connection attempt `n + 1`, after `n` consecutive
/// unproductive ones: 5 s, 10 s, 20 s, 40 s, then 60 s for good.
fn retry_delay(consecutive_failures: u32) -> Duration {
    let doublings = consecutive_failures.saturating_sub(1).min(8);
    (RETRY_MIN * 2u32.pow(doublings)).min(RETRY_MAX)
}

/// `fut`, unless the app starts shutting down first. Every Docker call in here
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
    cancel: &mut watch::Receiver<bool>,
    sweep_reason: ExitReason,
    on_change: &impl Fn(&Project, ExitReason),
) -> Option<bool> {
    let Ok(docker) = crate::docker::get_docker() else { return Some(false) };
    if unless_cancelled(cancel, docker.ping()).await?.is_err() {
        return Some(false);
    }
    // A second's overlap: the daemon replays from `since`, so a death during
    // the sweep below is still delivered. Duplicates are harmless — `settle`
    // confirms with an inspect every time.
    let since = (chrono::Utc::now().timestamp() - 1).to_string();
    for p in unless_cancelled(cancel, sweep(&docker, store)).await? {
        on_change(&p, sweep_reason);
    }
    let mut events = docker.events(Some(EventsOptions::<String> {
        since: Some(since),
        until: None,
        filters: die_filter(),
    }));
    let connected = Instant::now();
    let mut delivered = false;
    loop {
        match unless_cancelled(cancel, events.next()).await? {
            Some(Ok(msg)) => {
                delivered = true;
                if let Some(cid) = died_container(&msg) {
                    if let Some(p) = unless_cancelled(cancel, settle(&docker, store, cid)).await? {
                        on_change(&p, ExitReason::Exited);
                    }
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
        let Some(healthy) = watch_once(&store, &mut cancel, sweep_reason, &on_change).await else {
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

/// Start the watcher for the life of the app.
pub fn spawn(
    app: AppHandle,
    store: Arc<ProjectsStore>,
    cancel: watch::Receiver<bool>,
) -> tauri::async_runtime::JoinHandle<()> {
    tauri::async_runtime::spawn(async move {
        run(store, cancel, |p, reason| {
            log::warn!(
                "Project '{}' ({}): its container stopped without the app asking ({:?}) — marking Stopped",
                p.name,
                p.id,
                reason
            );
            let _ = app.emit(CONTAINER_EXITED_EVENT, ContainerExited { project: p, reason });
        })
        .await;
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use bollard::models::EventActor;

    fn event(typ: EventMessageTypeEnum, action: &str, id: Option<&str>) -> EventMessage {
        EventMessage {
            typ: Some(typ),
            action: Some(action.to_string()),
            actor: Some(EventActor { id: id.map(str::to_string), attributes: None }),
            ..Default::default()
        }
    }

    #[test]
    fn only_a_container_die_names_a_container() {
        let die = event(EventMessageTypeEnum::CONTAINER, "die", Some("abc"));
        assert_eq!(died_container(&die), Some("abc"));

        for (typ, action, id) in [
            (EventMessageTypeEnum::CONTAINER, "stop", Some("abc")),
            (EventMessageTypeEnum::CONTAINER, "start", Some("abc")),
            (EventMessageTypeEnum::CONTAINER, "exec_die", Some("abc")),
            (EventMessageTypeEnum::NETWORK, "die", Some("abc")),
            (EventMessageTypeEnum::CONTAINER, "die", None),
            (EventMessageTypeEnum::CONTAINER, "die", Some("")),
        ] {
            assert_eq!(died_container(&event(typ, action, id)), None, "{typ:?} {action} {id:?}");
        }
    }

    #[test]
    fn the_subscription_is_filtered_to_container_deaths_daemon_side() {
        let f = die_filter();
        assert_eq!(f.get("type"), Some(&vec!["container".to_string()]));
        assert_eq!(f.get("event"), Some(&vec!["die".to_string()]));
        assert_eq!(f.len(), 2);
    }

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
        let v = serde_json::to_value(ContainerExited { project: &p, reason: ExitReason::Exited }).unwrap();
        assert_eq!(v["reason"], "exited");
        assert_eq!(v["project"]["name"], "demo");
        let v = serde_json::to_value(ContainerExited { project: &p, reason: ExitReason::FoundStopped })
            .unwrap();
        assert_eq!(v["reason"], "found_stopped");
        assert_eq!(CONTAINER_EXITED_EVENT, "project-container-exited");
    }

    /// The shutdown path: a Docker call that never answers (a wedged daemon)
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
        let watcher = tokio::spawn(run(store.clone(), cancel_rx, move |p, why| {
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
