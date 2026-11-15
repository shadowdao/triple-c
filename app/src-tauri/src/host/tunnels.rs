//! The live SSM tunnels to AWS projects' agents, by project id.
//!
//! An AWS project's [`ContainerTarget`](super::ContainerTarget) is an agent
//! endpoint at the loopback end of a [`PortForward`](super::ssm::PortForward);
//! this registry is where `ContainerTarget::for_project` finds it, where
//! `ProjectHost::stop` finds the task behind an endpoint, and what the
//! shutdown path closes (every forward runs a `session-manager-plugin` child
//! that would otherwise outlive the app — see `ssm.rs`).
//!
//! It also remembers which targets the AWS death watch found **stopped**, so
//! `ContainerTarget::has_exited` can answer for an agent target (R3): only
//! the host that runs the task can confirm a death, never the agent.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use std::time::Duration;

use aws_config::SdkConfig;
use futures_util::future::BoxFuture;
use tokio::sync::watch;

use super::agent::AgentEndpoint;
use super::ssm::{EcsTask, PortForward};

/// Something that owns a tunnel's resources: the real [`PortForward`], or a
/// test stand-in.
pub trait TunnelHandle: Send + Sync {
    fn close(self: Box<Self>) -> BoxFuture<'static, ()>;
}

impl TunnelHandle for PortForward {
    fn close(self: Box<Self>) -> BoxFuture<'static, ()> {
        Box::pin(async move { (*self).close().await })
    }
}

/// One project's tunnel and the task it reaches.
pub struct LiveTunnel {
    /// The SSM target (kept for diagnostics; the forward already uses it).
    #[allow(dead_code)]
    pub task: EcsTask,
    pub task_arn: String,
    pub cluster_arn: String,
    pub connection_id: String,
    pub region: String,
    /// The connection's SDK config, so a stop needs no settings lookup.
    pub sdk: SdkConfig,
    pub forward: Box<dyn TunnelHandle>,
    pub endpoint: AgentEndpoint,
    /// Runtime handles for the store (`set_aws_runtime`).
    pub access_point_id: Option<String>,
    pub task_definition_arn: String,
}

/// Where an endpoint's task is: `(connection, region, cluster, task ARN)`.
pub type TaskOf = (String, String, String, String);

#[derive(Default)]
struct Registry {
    live: HashMap<String, LiveTunnel>,
    /// Target ids (endpoint base URLs) whose task the AWS watch saw STOPPED.
    stopped: HashSet<String>,
    /// Projects whose tunnel is being re-opened in the background.
    pending: HashSet<String>,
}

fn registry() -> &'static Mutex<Registry> {
    static R: std::sync::OnceLock<Mutex<Registry>> = std::sync::OnceLock::new();
    R.get_or_init(Default::default)
}

fn lock() -> std::sync::MutexGuard<'static, Registry> {
    registry().lock().unwrap_or_else(|e| e.into_inner())
}

/// Wakes [`wait_endpoint`] callers when the registry changes.
fn changed() -> &'static watch::Sender<u64> {
    static C: std::sync::OnceLock<watch::Sender<u64>> = std::sync::OnceLock::new();
    C.get_or_init(|| watch::channel(0).0)
}

fn notify() {
    changed().send_modify(|n| *n = n.wrapping_add(1));
}

/// Register `tunnel` for `project_id`, returning the one it replaces (the
/// caller closes it).
pub fn register(project_id: &str, tunnel: LiveTunnel) -> Option<LiveTunnel> {
    let old = {
        let mut r = lock();
        r.stopped.remove(tunnel.endpoint.base_url.as_str());
        r.pending.remove(project_id);
        r.live.insert(project_id.to_string(), tunnel)
    };
    notify();
    old
}

pub fn get_endpoint(project_id: &str) -> Option<AgentEndpoint> {
    lock().live.get(project_id).map(|t| t.endpoint.clone())
}

/// `(task ARN, access point id, task definition ARN)` of the project's tunnel.
pub fn runtime(project_id: &str) -> Option<(String, Option<String>, String)> {
    lock().live.get(project_id).map(|t| {
        (
            t.task_arn.clone(),
            t.access_point_id.clone(),
            t.task_definition_arn.clone(),
        )
    })
}

/// The project whose tunnel is `ep` and its task.
pub fn task_of(ep: &AgentEndpoint) -> Option<TaskOf> {
    lock().live.values().find(|t| t.endpoint.base_url == ep.base_url).map(|t| {
        (
            t.connection_id.clone(),
            t.region.clone(),
            t.cluster_arn.clone(),
            t.task_arn.clone(),
        )
    })
}

/// The project id and SDK config behind `ep`.
pub(crate) fn owner_of(ep: &AgentEndpoint) -> Option<(String, SdkConfig)> {
    lock()
        .live
        .iter()
        .find(|(_, t)| t.endpoint.base_url == ep.base_url)
        .map(|(id, t)| (id.clone(), t.sdk.clone()))
}

/// Take the project's tunnel out and close it.
pub async fn close(project_id: &str) {
    let taken = {
        let mut r = lock();
        r.pending.remove(project_id);
        let t = r.live.remove(project_id);
        if let Some(ref t) = t {
            r.stopped.remove(t.endpoint.base_url.as_str());
        }
        t
    };
    notify();
    if let Some(t) = taken {
        t.forward.close().await;
    }
}

/// Close every tunnel, concurrently. The shutdown path's step.
pub async fn close_all() {
    let all: Vec<LiveTunnel> = {
        let mut r = lock();
        r.pending.clear();
        r.stopped.clear();
        r.live.drain().map(|(_, t)| t).collect()
    };
    notify();
    futures_util::future::join_all(all.into_iter().map(|t| t.forward.close())).await;
}

/// The AWS watch saw the task behind target `target_id` STOPPED.
pub fn mark_stopped(target_id: &str) {
    lock().stopped.insert(target_id.to_string());
}

/// Whether the AWS watch has confirmed the task behind `target_id` stopped.
pub fn is_stopped(target_id: &str) -> bool {
    lock().stopped.contains(target_id)
}

/// A background re-attach for this project has begun / ended without a tunnel.
pub fn set_pending(project_id: &str, pending: bool) {
    {
        let mut r = lock();
        if pending {
            r.pending.insert(project_id.to_string());
        } else {
            r.pending.remove(project_id);
        }
    }
    notify();
}

pub fn is_pending(project_id: &str) -> bool {
    lock().pending.contains(project_id)
}

/// The project's endpoint, waiting up to `timeout` while a background
/// re-attach is still opening it. `None` when there is none.
pub async fn wait_endpoint(project_id: &str, timeout: Duration) -> Option<AgentEndpoint> {
    let mut rx = changed().subscribe();
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if let Some(ep) = get_endpoint(project_id) {
            return Some(ep);
        }
        if !is_pending(project_id) {
            return None;
        }
        if tokio::time::timeout_at(deadline, rx.changed()).await.is_err() {
            return None;
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    /// A stand-in tunnel that counts its closes.
    pub(crate) struct FakeTunnel(pub Arc<AtomicUsize>);
    impl TunnelHandle for FakeTunnel {
        fn close(self: Box<Self>) -> BoxFuture<'static, ()> {
            let n = self.0.clone();
            Box::pin(async move {
                n.fetch_add(1, Ordering::SeqCst);
            })
        }
    }

    pub(crate) fn fake_tunnel(port: u16, closes: Arc<AtomicUsize>) -> LiveTunnel {
        LiveTunnel {
            task: EcsTask {
                cluster: "triple-c-dev".into(),
                task_id: format!("t{port}"),
                runtime_id: "r".into(),
            },
            task_arn: format!("arn:aws:ecs:eu-west-1:111122223333:task/triple-c-dev/t{port}"),
            cluster_arn: "arn:aws:ecs:eu-west-1:111122223333:cluster/triple-c-dev".into(),
            connection_id: "c1".into(),
            region: "eu-west-1".into(),
            sdk: SdkConfig::builder().build(),
            forward: Box::new(FakeTunnel(closes)),
            endpoint: AgentEndpoint::loopback(port, "tok"),
            access_point_id: Some("fsap-1".into()),
            task_definition_arn: "arn:td:1".into(),
        }
    }

    fn pid(tag: &str) -> String {
        format!("tunnels-test-{tag}-{}", uuid::Uuid::new_v4())
    }

    #[tokio::test]
    async fn a_registered_tunnel_is_found_by_project_and_by_endpoint_and_closed_once() {
        let closes = Arc::new(AtomicUsize::new(0));
        let p = pid("reg");
        assert!(get_endpoint(&p).is_none());
        assert!(register(&p, fake_tunnel(61001, closes.clone())).is_none());
        let ep = get_endpoint(&p).unwrap();
        assert_eq!(ep, AgentEndpoint::loopback(61001, "tok"));
        let (conn, region, _, arn) = task_of(&ep).unwrap();
        assert_eq!((conn.as_str(), region.as_str()), ("c1", "eu-west-1"));
        assert!(arn.ends_with("/t61001"));
        assert_eq!(runtime(&p).unwrap().1.as_deref(), Some("fsap-1"));
        close(&p).await;
        close(&p).await;
        assert_eq!(closes.load(Ordering::SeqCst), 1);
        assert!(get_endpoint(&p).is_none() && task_of(&ep).is_none());
    }

    #[tokio::test]
    async fn re_registering_hands_back_the_old_tunnel_and_clears_a_stale_stop() {
        let closes = Arc::new(AtomicUsize::new(0));
        let p = pid("rereg");
        register(&p, fake_tunnel(61002, closes.clone()));
        let id = AgentEndpoint::loopback(61003, "tok").base_url.to_string();
        mark_stopped(&id);
        let old = register(&p, fake_tunnel(61003, closes.clone())).unwrap();
        assert_eq!(old.endpoint.base_url.port(), Some(61002));
        assert!(!is_stopped(&id), "a new task on a reused port is not stopped");
        old.forward.close().await;
        close(&p).await;
        assert_eq!(closes.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn waiting_for_an_endpoint_ends_when_it_is_registered_or_the_reattach_gives_up() {
        let p = pid("wait");
        assert!(wait_endpoint(&p, Duration::from_secs(5)).await.is_none(), "nothing pending");
        set_pending(&p, true);
        let p2 = p.clone();
        let waiter = tokio::spawn(async move { wait_endpoint(&p2, Duration::from_secs(5)).await });
        tokio::time::sleep(Duration::from_millis(20)).await;
        register(&p, fake_tunnel(61004, Arc::new(AtomicUsize::new(0))));
        assert_eq!(waiter.await.unwrap().unwrap().base_url.port(), Some(61004));
        close(&p).await;

        set_pending(&p, true);
        let p2 = p.clone();
        let waiter = tokio::spawn(async move { wait_endpoint(&p2, Duration::from_secs(5)).await });
        tokio::time::sleep(Duration::from_millis(20)).await;
        set_pending(&p, false);
        assert!(waiter.await.unwrap().is_none());
    }
}
