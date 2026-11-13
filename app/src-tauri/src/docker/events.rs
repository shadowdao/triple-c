//! The Docker half of `container_watch`: reachability, the daemon's container
//! `die` event stream, and the "has it positively exited" check. Everything
//! about *what to do* with a death (confirm, compare-and-set the store, tell
//! the frontend, back off) is host-neutral and stays in `container_watch.rs`,
//! which reaches these through `host::watch`.

use std::collections::HashMap;
use std::pin::Pin;

use bollard::errors::Error as DockerError;
use bollard::models::{EventMessage, EventMessageTypeEnum};
use bollard::system::EventsOptions;
use futures_util::{Stream, StreamExt};

use super::client::get_docker;

/// Whether the daemon answers at all.
pub async fn ping() -> bool {
    match get_docker() {
        Ok(docker) => docker.ping().await.is_ok(),
        Err(_) => false,
    }
}

/// The container id a Docker event reports as having died, if it is one.
pub(crate) fn died_container(msg: &EventMessage) -> Option<&str> {
    if msg.typ != Some(EventMessageTypeEnum::CONTAINER) || msg.action.as_deref() != Some("die") {
        return None;
    }
    msg.actor.as_ref()?.id.as_deref().filter(|id| !id.is_empty())
}

fn die_filter() -> HashMap<String, Vec<String>> {
    HashMap::from([
        ("type".to_string(), vec!["container".to_string()]),
        ("event".to_string(), vec!["die".to_string()]),
    ])
}

/// Container ids as Docker reports them dying, replayed from `since` (unix
/// seconds). Filtered daemon-side to container `die` events, so an idle daemon
/// costs nothing. Anything else that comes through is dropped here. An `Err`
/// item, or the stream ending, means the connection is gone.
pub fn container_deaths(since: i64) -> Pin<Box<dyn Stream<Item = Result<String, String>> + Send>> {
    let docker = match get_docker() {
        Ok(d) => d,
        Err(e) => return Box::pin(futures_util::stream::once(async move { Err(e) })),
    };
    let events = docker.events(Some(EventsOptions::<String> {
        since: Some(since.to_string()),
        until: None,
        filters: die_filter(),
    }));
    Box::pin(events.filter_map(|item| async move {
        match item {
            Ok(msg) => died_container(&msg).map(|id| Ok(id.to_string())),
            Err(e) => Some(Err(e.to_string())),
        }
    }))
}

/// `true` only when Docker positively says the container is not running: it
/// inspects as stopped, or it no longer exists. Any other failure is "don't
/// know" — `is_container_running` maps every error to `false`, which is right
/// for its callers and wrong for the watcher, where it would mark every project
/// Stopped the moment the daemon hiccups.
pub async fn has_exited(container_id: &str) -> bool {
    let Ok(docker) = get_docker() else { return false };
    match docker.inspect_container(container_id, None).await {
        Ok(info) => !info.state.and_then(|s| s.running).unwrap_or(false),
        Err(DockerError::DockerResponseServerError { status_code: 404, .. }) => true,
        Err(_) => false,
    }
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
}
