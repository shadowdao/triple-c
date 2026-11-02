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
//! project a command owns. The frontend hears it on [`PROJECT_STATUS_EVENT`].
//!
//! The stream drops whenever the daemon does. Each (re)subscription asks for
//! events since just before it, and first re-checks every Running project once —
//! the only window that replay cannot cover is a daemon restart, which forgets
//! its event history, and that is exactly when containers go away.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use bollard::errors::Error as DockerError;
use bollard::models::{EventMessage, EventMessageTypeEnum};
use bollard::system::EventsOptions;
use bollard::Docker;
use futures_util::StreamExt;
use tauri::{AppHandle, Emitter};
use tokio::sync::watch;

use crate::models::{Project, ProjectStatus};
use crate::storage::projects_store::ProjectsStore;

/// Emitted with the updated `Project` whenever this module changes a status.
pub const PROJECT_STATUS_EVENT: &str = "project-status-changed";

const RETRY_MIN: Duration = Duration::from_secs(5);
const RETRY_MAX: Duration = Duration::from_secs(60);

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
    // The cheap filter first: most containers that die are not ours (probe and
    // scratch containers, other software on the daemon), and those cost no
    // inspect at all.
    let ours = store.list().into_iter().any(|p| {
        p.status == ProjectStatus::Running
            && p.container_id
                .as_deref()
                .is_some_and(|c| c.starts_with(container_id) || container_id.starts_with(c))
    });
    if !ours || !has_exited(docker, container_id).await {
        return None;
    }
    store.mark_exited(container_id, project_busy)
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

/// Watch until `cancel` flips, calling `on_change` for every project whose
/// container is found to have stopped on its own. Separate from [`spawn`] so a
/// test can drive it without a Tauri app.
pub(crate) async fn run(
    store: Arc<ProjectsStore>,
    mut cancel: watch::Receiver<bool>,
    on_change: impl Fn(&Project),
) {
    let mut retry = RETRY_MIN;
    loop {
        if *cancel.borrow() {
            return;
        }
        let docker = match crate::docker::get_docker() {
            Ok(d) if d.ping().await.is_ok() => Some(d),
            _ => None,
        };
        if let Some(docker) = docker {
            retry = RETRY_MIN;
            // A second's overlap: the daemon replays from `since`, so a death
            // during the sweep below is still delivered. Duplicates are
            // harmless — `settle` confirms with an inspect every time.
            let since = (chrono::Utc::now().timestamp() - 1).to_string();
            for p in sweep(&docker, &store).await {
                on_change(&p);
            }
            let mut events = docker.events(Some(EventsOptions::<String> {
                since: Some(since),
                until: None,
                filters: die_filter(),
            }));
            loop {
                tokio::select! {
                    _ = cancel.changed() => return,
                    next = events.next() => match next {
                        Some(Ok(msg)) => {
                            if let Some(cid) = died_container(&msg) {
                                if let Some(p) = settle(&docker, &store, cid).await {
                                    on_change(&p);
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
                    },
                }
            }
        }
        tokio::select! {
            _ = cancel.changed() => return,
            _ = tokio::time::sleep(retry) => {}
        }
        retry = (retry * 2).min(RETRY_MAX);
    }
}

/// Start the watcher for the life of the app.
pub fn spawn(
    app: AppHandle,
    store: Arc<ProjectsStore>,
    cancel: watch::Receiver<bool>,
) -> tauri::async_runtime::JoinHandle<()> {
    tauri::async_runtime::spawn(async move {
        run(store, cancel, |p| {
            log::warn!(
                "Project '{}' ({}): its container stopped without the app asking — marking Stopped",
                p.name,
                p.id
            );
            let _ = app.emit(PROJECT_STATUS_EVENT, p);
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

    /// End to end against a real daemon: a Running project whose container is
    /// killed from outside the app is flipped to Stopped and reported, and one
    /// whose container is still up is not.
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

        let dir = std::env::temp_dir().join(format!("triple-c-watch-{tag}"));
        std::fs::create_dir_all(&dir).unwrap();
        let mut a = Project::new("a".to_string(), Vec::new());
        a.container_id = Some(doomed.clone());
        a.status = ProjectStatus::Running;
        let mut b = Project::new("b".to_string(), Vec::new());
        b.container_id = Some(alive.clone());
        b.status = ProjectStatus::Running;
        let (a_id, b_id) = (a.id.clone(), b.id.clone());
        let store = Arc::new(ProjectsStore::over_file_for_test(dir.join("projects.json"), vec![a, b]));

        let (cancel_tx, cancel_rx) = watch::channel(false);
        let (seen_tx, mut seen_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        let watcher = tokio::spawn(run(store.clone(), cancel_rx, move |p| {
            let _ = seen_tx.send(p.id.clone());
        }));
        // Let it subscribe, then kill from "outside".
        tokio::time::sleep(Duration::from_secs(2)).await;
        docker_cli(&["kill", &doomed]);

        let seen = tokio::time::timeout(Duration::from_secs(15), seen_rx.recv()).await;
        let _ = cancel_tx.send(true);
        let _ = watcher.await;
        let a_status = store.get(&a_id).unwrap().status;
        let b_status = store.get(&b_id).unwrap().status;
        docker_cli(&["rm", "-f", &doomed, &alive]);
        std::fs::remove_dir_all(&dir).ok();

        assert_eq!(seen.expect("reported within 15s"), Some(a_id));
        assert_eq!(a_status, ProjectStatus::Stopped);
        assert_eq!(b_status, ProjectStatus::Running);
    }
}
