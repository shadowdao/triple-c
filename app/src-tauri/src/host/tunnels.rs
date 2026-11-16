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
use tokio_util::sync::CancellationToken;

use super::agent::AgentEndpoint;
use crate::aws::tasks::StopKind;
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
    /// EFS storage's `/workspace` access point.
    pub workspace_access_point_id: Option<String>,
    /// The start that opened this tunnel ran the task (rather than
    /// attaching to one already running, or re-attaching after a restart).
    pub created: bool,
}

/// Where an endpoint's task is: `(connection, region, cluster, task ARN)`.
pub type TaskOf = (String, String, String, String);

#[derive(Default)]
struct Registry {
    live: HashMap<String, LiveTunnel>,
    /// Target ids (endpoint base URLs) whose task the AWS watch saw STOPPED,
    /// and why.
    stopped: HashMap<String, StopKind>,
    /// Projects whose tunnel is being re-opened in the background.
    pending: HashSet<String>,
    /// Bumped by every [`close`] (a stop or remove), every [`register`] (a
    /// start's own tunnel) and [`cancel_reattach`]: a background re-attach
    /// that began under an older generation must never register.
    generation: HashMap<String, u64>,
    /// The background clone/setup follower per project (`aws_host::status`),
    /// with the id [`begin_follow`] gave it. Cancelled by a new follower,
    /// [`cancel_follow`] (Stop, Remove), and whenever the tunnel is taken
    /// out or every tunnel closed.
    followers: HashMap<String, (u64, CancellationToken)>,
    next_follower: u64,
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

/// The target id an AWS project has while no tunnel is registered for it
/// (see `aws_host::unattached_container_id`): never a loopback URL, so a
/// port a new tunnel reuses can't be confused with it.
pub fn unattached_id(project_id: &str) -> String {
    format!("http://unattached.invalid/{project_id}/")
}

/// Forget that the watch saw the task behind `target_id` stopped. For a
/// project's [`unattached_id`], which (unlike an endpoint) is reused by
/// every later unattached spell of the same project: a stale entry would
/// make a later live task read as stopped.
pub fn forget_stopped(target_id: &str) {
    lock().stopped.remove(target_id);
}

/// Register `tunnel` for `project_id`, returning the one it replaces (the
/// caller closes it).
pub fn register(project_id: &str, tunnel: LiveTunnel) -> Option<LiveTunnel> {
    let old = {
        let mut r = lock();
        *r.generation.entry(project_id.to_string()).or_default() += 1;
        r.stopped.remove(tunnel.endpoint.base_url.as_str());
        r.stopped.remove(&unattached_id(project_id));
        r.pending.remove(project_id);
        r.live.insert(project_id.to_string(), tunnel)
    };
    notify();
    old
}

pub fn get_endpoint(project_id: &str) -> Option<AgentEndpoint> {
    lock().live.get(project_id).map(|t| t.endpoint.clone())
}

/// `(task ARN, access point id, task definition ARN, workspace access point
/// id)` of the project's tunnel.
pub fn runtime(project_id: &str) -> Option<(String, Option<String>, String, Option<String>)> {
    lock().live.get(project_id).map(|t| {
        (
            t.task_arn.clone(),
            t.access_point_id.clone(),
            t.task_definition_arn.clone(),
            t.workspace_access_point_id.clone(),
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

/// A background re-attach of `project_id` begins: marks it pending and
/// returns the generation [`register_if_current`] must still see.
pub fn begin_reattach(project_id: &str) -> u64 {
    let g = {
        let mut r = lock();
        r.pending.insert(project_id.to_string());
        *r.generation.entry(project_id.to_string()).or_default()
    };
    notify();
    g
}

/// [`register`], unless the project was closed (stopped, removed) since
/// `generation` was taken: then the tunnel is handed back for the caller to
/// close, and nothing is registered.
pub fn register_if_current(
    project_id: &str,
    generation: u64,
    tunnel: LiveTunnel,
) -> Result<Option<LiveTunnel>, Box<LiveTunnel>> {
    let old = {
        let mut r = lock();
        if r.generation.get(project_id).copied().unwrap_or_default() != generation {
            return Err(Box::new(tunnel));
        }
        r.stopped.remove(tunnel.endpoint.base_url.as_str());
        r.stopped.remove(&unattached_id(project_id));
        r.pending.remove(project_id);
        r.live.insert(project_id.to_string(), tunnel)
    };
    notify();
    Ok(old)
}

/// [`close`], but only when the registered tunnel leads to `task_arn` (a
/// death report about an older task must not close a newer one's tunnel).
pub async fn close_if_task(project_id: &str, task_arn: &str) {
    let taken = {
        let mut r = lock();
        if !r.live.get(project_id).is_some_and(|t| t.task_arn == task_arn) {
            return;
        }
        take(&mut r, project_id)
    };
    notify();
    if let Some(t) = taken {
        t.forward.close().await;
    }
}

/// Whether the project's registered tunnel came from a start that ran its
/// task (see [`LiveTunnel::created`]).
pub fn ran_task(project_id: &str) -> bool {
    lock().live.get(project_id).is_some_and(|t| t.created)
}

/// A later start found the tunnel already up: it ran no task itself.
pub fn mark_attached(project_id: &str) {
    if let Some(t) = lock().live.get_mut(project_id) {
        t.created = false;
    }
}

/// Register a background follower for `project_id`, cancelling the one
/// before it. Returns its id (for [`end_follow`]) and its token.
pub fn begin_follow(project_id: &str) -> (u64, CancellationToken) {
    let mut r = lock();
    r.next_follower += 1;
    let id = r.next_follower;
    let token = CancellationToken::new();
    if let Some((_, old)) = r.followers.insert(project_id.to_string(), (id, token.clone())) {
        old.cancel();
    }
    (id, token)
}

/// A follower finished: forget it, unless a newer one has replaced it.
pub fn end_follow(project_id: &str, id: u64) {
    let mut r = lock();
    if r.followers.get(project_id).is_some_and(|(i, _)| *i == id) {
        r.followers.remove(project_id);
    }
}

/// End the project's follower, if one runs (Stop, Remove).
pub fn cancel_follow(project_id: &str) {
    cancel_follow_locked(&mut lock(), project_id);
}

fn cancel_follow_locked(r: &mut Registry, project_id: &str) {
    if let Some((_, t)) = r.followers.remove(project_id) {
        t.cancel();
    }
}

/// Whether a follower runs for the project.
#[cfg(test)]
pub fn is_following(project_id: &str) -> bool {
    lock().followers.contains_key(project_id)
}

/// Under the lock: invalidate a re-attach in flight and take the tunnel out.
fn take(r: &mut Registry, project_id: &str) -> Option<LiveTunnel> {
    cancel_follow_locked(r, project_id);
    r.pending.remove(project_id);
    *r.generation.entry(project_id.to_string()).or_default() += 1;
    let t = r.live.remove(project_id);
    if let Some(ref t) = t {
        r.stopped.remove(t.endpoint.base_url.as_str());
    }
    t
}

/// A start is about to attach itself: any background re-attach in flight
/// is cancelled (it will hand its tunnel back and close it).
pub fn cancel_reattach(project_id: &str) {
    {
        let mut r = lock();
        r.pending.remove(project_id);
        *r.generation.entry(project_id.to_string()).or_default() += 1;
    }
    notify();
}

/// A background re-attach that began at `generation` gave up: clear the
/// pending flag, unless something newer has taken over since.
pub fn end_reattach(project_id: &str, generation: u64) {
    {
        let mut r = lock();
        if r.generation.get(project_id).copied().unwrap_or_default() == generation {
            r.pending.remove(project_id);
        }
    }
    notify();
}

/// Take the project's tunnel out and close it, and invalidate a background
/// re-attach in flight.
pub async fn close(project_id: &str) {
    let taken = take(&mut lock(), project_id);
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
        for (_, (_, t)) in r.followers.drain() {
            t.cancel();
        }
        for g in r.generation.values_mut() {
            *g += 1;
        }
        r.live.drain().map(|(_, t)| t).collect()
    };
    notify();
    futures_util::future::join_all(all.into_iter().map(|t| t.forward.close())).await;
}

/// The AWS watch saw the task behind target `target_id` STOPPED, for `why`.
pub fn mark_stopped(target_id: &str, why: StopKind) {
    lock().stopped.insert(target_id.to_string(), why);
}

/// Whether the AWS watch has confirmed the task behind `target_id` stopped.
pub fn is_stopped(target_id: &str) -> bool {
    lock().stopped.contains_key(target_id)
}

/// Why the task behind `target_id` stopped, once the watch has seen it.
pub fn stop_kind(target_id: &str) -> Option<StopKind> {
    lock().stopped.get(target_id).cloned()
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
            workspace_access_point_id: None,
            created: true,
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
    async fn closing_the_tunnel_ends_its_follower_and_an_attach_clears_created() {
        let closes = Arc::new(AtomicUsize::new(0));
        let p = pid("follow");
        register(&p, fake_tunnel(61009, closes.clone()));
        assert!(ran_task(&p));
        mark_attached(&p);
        assert!(!ran_task(&p));
        let (_, token) = begin_follow(&p);
        assert!(is_following(&p) && !token.is_cancelled());
        close(&p).await;
        assert!(token.is_cancelled());
        assert!(!is_following(&p));
    }

    #[tokio::test]
    async fn re_registering_hands_back_the_old_tunnel_and_clears_a_stale_stop() {
        let closes = Arc::new(AtomicUsize::new(0));
        let p = pid("rereg");
        register(&p, fake_tunnel(61002, closes.clone()));
        let id = AgentEndpoint::loopback(61003, "tok").base_url.to_string();
        mark_stopped(&id, StopKind::Spot);
        assert_eq!(stop_kind(&id), Some(StopKind::Spot));
        let old = register(&p, fake_tunnel(61003, closes.clone())).unwrap();
        assert_eq!(old.endpoint.base_url.port(), Some(61002));
        assert!(!is_stopped(&id), "a new task on a reused port is not stopped");
        old.forward.close().await;
        close(&p).await;
        assert_eq!(closes.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn a_registering_tunnel_clears_a_stop_seen_under_the_projects_unattached_id() {
        let p = pid("unattached-stale");
        let placeholder = unattached_id(&p);
        mark_stopped(&placeholder, StopKind::Spot);
        let closes = Arc::new(AtomicUsize::new(0));
        register(&p, fake_tunnel(61301, closes.clone()));
        assert!(!is_stopped(&placeholder), "a later unattached spell must not read as stopped");
        close(&p).await;

        // A re-attach registering clears it too; and it can be forgotten directly.
        let q = pid("unattached-stale-reattach");
        mark_stopped(&unattached_id(&q), StopKind::Spot);
        let g = begin_reattach(&q);
        assert!(register_if_current(&q, g, fake_tunnel(61302, closes.clone())).is_ok());
        assert!(!is_stopped(&unattached_id(&q)));
        close(&q).await;
        mark_stopped(&unattached_id(&q), StopKind::Idle);
        forget_stopped(&unattached_id(&q));
        assert!(!is_stopped(&unattached_id(&q)));
    }

    #[tokio::test]
    async fn a_reattach_that_began_before_a_stop_does_not_register() {
        let closes = Arc::new(AtomicUsize::new(0));
        let p = pid("gen");
        let g = begin_reattach(&p);
        assert!(is_pending(&p));
        close(&p).await; // the user stopped the project meanwhile
        let back = register_if_current(&p, g, fake_tunnel(61005, closes.clone()));
        let t = back.err().expect("handed back, not registered");
        assert!(get_endpoint(&p).is_none() && !is_pending(&p));
        t.forward.close().await;
        assert_eq!(closes.load(Ordering::SeqCst), 1);

        let g = begin_reattach(&p);
        assert!(register_if_current(&p, g, fake_tunnel(61006, closes.clone())).is_ok());
        assert!(get_endpoint(&p).is_some());
        close(&p).await;
    }

    #[tokio::test]
    async fn a_starts_tunnel_survives_a_reattach_that_finishes_after_it() {
        let closes = Arc::new(AtomicUsize::new(0));
        // Start cancels the pending re-attach, then registers its own.
        let p = pid("start-wins");
        let g = begin_reattach(&p);
        cancel_reattach(&p);
        assert!(!is_pending(&p));
        register(&p, fake_tunnel(61010, closes.clone()));
        let late = register_if_current(&p, g, fake_tunnel(61011, closes.clone()));
        late.err().expect("the late re-attach is refused").forward.close().await;
        assert_eq!(get_endpoint(&p).unwrap().base_url.port(), Some(61010), "Start's tunnel stays");
        assert_eq!(closes.load(Ordering::SeqCst), 1, "only the late tunnel was closed");
        // A stale end of that re-attach does not clear a newer one's pending flag.
        let g2 = begin_reattach(&p);
        end_reattach(&p, g);
        assert!(is_pending(&p));
        end_reattach(&p, g2);
        assert!(!is_pending(&p));
        close(&p).await;

        // Even without a cancel, a plain register makes an older re-attach stale.
        let p = pid("register-bumps");
        let g = begin_reattach(&p);
        register(&p, fake_tunnel(61012, closes.clone()));
        assert!(register_if_current(&p, g, fake_tunnel(61013, closes.clone())).is_err());
        assert_eq!(get_endpoint(&p).unwrap().base_url.port(), Some(61012));
        close(&p).await;
    }

    #[tokio::test]
    async fn a_death_report_closes_only_the_tunnel_of_the_task_that_died() {
        let closes = Arc::new(AtomicUsize::new(0));
        let p = pid("died");
        register(&p, fake_tunnel(61007, closes.clone()));
        close_if_task(&p, "arn:aws:ecs:eu-west-1:111122223333:task/triple-c-dev/older").await;
        assert!(get_endpoint(&p).is_some(), "a newer task's tunnel stays");
        close_if_task(&p, "arn:aws:ecs:eu-west-1:111122223333:task/triple-c-dev/t61007").await;
        assert!(get_endpoint(&p).is_none());
        assert_eq!(closes.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn waiting_for_an_endpoint_ends_when_it_is_registered_or_the_reattach_gives_up() {
        let p = pid("wait");
        assert!(wait_endpoint(&p, Duration::from_secs(5)).await.is_none(), "nothing pending");
        begin_reattach(&p);
        let p2 = p.clone();
        let waiter = tokio::spawn(async move { wait_endpoint(&p2, Duration::from_secs(5)).await });
        tokio::time::sleep(Duration::from_millis(20)).await;
        register(&p, fake_tunnel(61004, Arc::new(AtomicUsize::new(0))));
        assert_eq!(waiter.await.unwrap().unwrap().base_url.port(), Some(61004));
        close(&p).await;

        let g = begin_reattach(&p);
        let p2 = p.clone();
        let waiter = tokio::spawn(async move { wait_endpoint(&p2, Duration::from_secs(5)).await });
        tokio::time::sleep(Duration::from_millis(20)).await;
        end_reattach(&p, g);
        assert!(waiter.await.unwrap().is_none());
    }
}
