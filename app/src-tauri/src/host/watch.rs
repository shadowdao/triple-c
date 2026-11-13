//! Where `container_watch` learns that a project container stopped on its
//! own. Host-neutral shape: a reachability check, a stream of containers
//! reported dead (replayable from a point in time), and a positive-exit check
//! used to confirm each report before anything changes.
//!
//! Docker pushes `die` events. An AWS host will have no push stream from the
//! app's point of view; its [`deaths`] will be a `DescribeTasks` poll that
//! yields stopped tasks, which `container_watch` handles the same way because
//! every report is confirmed with [`ContainerTarget::has_exited`] anyway.

use futures_util::{Stream, StreamExt};
use std::pin::Pin;

use super::ContainerTarget;
use crate::docker;

/// Which host's deaths to watch. One variant today.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitSource {
    Docker,
}

pub type DeathStream = Pin<Box<dyn Stream<Item = Result<ContainerTarget, String>> + Send>>;

impl ExitSource {
    /// Whether the host answers at all. A `false` here is a failed connection
    /// attempt for the watcher's backoff.
    pub async fn reachable(&self) -> bool {
        match self {
            ExitSource::Docker => docker::events::ping().await,
        }
    }

    /// Containers reported dead, replayed from `since` (unix seconds). An `Err`
    /// item, or the stream ending, means the connection is gone.
    pub fn deaths(&self, since: i64) -> DeathStream {
        match self {
            ExitSource::Docker => Box::pin(
                docker::events::container_deaths(since).map(|r| r.map(ContainerTarget::docker)),
            ),
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
