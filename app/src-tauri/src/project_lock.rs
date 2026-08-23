//! Per-project mutual exclusion for everything that rewrites a project's
//! container or its snapshot image.
//!
//! ## Why polling was not enough
//!
//! Until this module existed the app had exactly one mutual-exclusion
//! primitive — the `ACTIVE_MIGRATIONS` set behind
//! `migration_commands::is_migrating` — and it was **one-way**. A migration
//! took a guard for its whole run; everything else merely *asked once, at
//! entry*, whether a migration was in flight and then proceeded with no claim
//! of its own. Two non-migration operations on the same project could not see
//! each other at all, and a migration could start underneath one that was
//! already halfway through.
//!
//! That is not a theoretical gap. Compaction resolves
//! `triple-c-snapshot-{id}:latest` when its build starts and commits back over
//! that same tag minutes later, and the Settings panel is a sidebar rather than
//! a modal — so Project Home stays live with Start, Stop, Reset and Migrate all
//! clickable while a compaction runs. Three interleavings were reproduced:
//!
//! * Compaction commits `flat(A)` over `:latest` after a migration has already
//!   moved that tag to a new lineage. The migration is silently reverted, the
//!   config replay lands twice, and the migration record says
//!   `awaiting-confirmation` against a base the tag no longer points at.
//! * Compaction resolves A, the user starts the project and works for an hour,
//!   a recreate commits D over `:latest`, and the compaction then overwrites it
//!   with `flat(A)` — orphaning an hour of system-layer work while reporting
//!   success and a byte saving.
//! * Compaction resurrects the system layer a Reset had just destroyed.
//!
//! Every one of those is "two writers of `:latest`, neither holding anything".
//! So this registry replaces the polling with an actual claim: an operation
//! **acquires** a [`ProjectGuard`] and holds it for its whole run, and a second
//! operation on the same project is refused with a message naming the holder.
//!
//! ## What this does NOT protect against, stated plainly
//!
//! **This is in-process state.** Two copies of the app pointed at the same
//! Docker daemon share nothing here: instance A's compaction and instance B's
//! migration will both acquire happily and then race exactly as before.
//! `reap_probe_containers` and the `triple-c-compact-*` / `triple-c-scrub-*`
//! sweeps are worse than that — they are daemon-wide force-removals driven by
//! a name or a label, so instance B can destroy a container instance A is
//! mid-commit against.
//!
//! A daemon-visible lock was considered and rejected for now, and the reasoning
//! is recorded here so it is not re-derived from scratch:
//!
//! * A **lock container** would work — container names are unique daemon-wide
//!   and `create` fails atomically on a name conflict — but a container has to
//!   be created *from an image*, and that pins the image. A lock on
//!   `triple-c-snapshot-{id}` would block the very `rmi`/sweep paths it guards,
//!   and a leaked lock container would pin multiple gigabytes forever.
//! * A **named volume** is not usable: `create_volume` on an existing name
//!   returns the existing volume rather than failing, so it cannot be a
//!   test-and-set.
//! * A **label on the snapshot image** is not atomic — read/modify/commit has
//!   the same race it would be trying to close.
//!
//! So the cross-process case is **documented, not solved**. What this module
//! does do about it is bound the damage: [`any_held_excluding`] lets the daemon-wide
//! reapers skip work while this process is mid-operation, and the reapers
//! themselves gained age gates so a young container belonging to somebody else
//! is left alone (see `docker::migration::reap_probe_containers`).
//!
//! ## Refuse, do not queue
//!
//! [`try_acquire`] never waits. Every caller is a user-initiated action behind
//! a button, and a button that blocks for the four minutes a compaction takes
//! is worse than one that says what is running. The refusal string is written
//! for the user and names the holder.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

/// The operations that claim a project.
///
/// One variant per *class of writer*, not per command: `Recreate` covers Start
/// as well, because Start's create-and-commit path is the same writer of
/// `triple-c-snapshot-{id}:latest` that a recreate is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectOp {
    /// `migrate_project_to_base`, `resume_migration`, `rollback_migration`,
    /// `confirm_migration`.
    Migration,
    /// `disk::compact_snapshot` — the long one, and the reason this exists.
    ///
    /// Not constructed on this branch: the Disk panel and its compaction were
    /// held back for separate hardening and live on `hold/disk-and-dragout`.
    /// The variant stays because this registry is the thing that made those
    /// operations safe to re-land, and a re-land that had to re-derive the
    /// claim classes would be re-deriving the bug.
    #[allow(dead_code)]
    Compaction,
    /// Start / stop / recreate. Anything in `start_project_container`'s path.
    Recreate,
    /// `rebuild_project_container` — deletes both volumes and the snapshot.
    Reset,
    /// `disk::destroy` — a volume, a snapshot image, or a rollback pin.
    Destroy,
    /// `disk::clear_caches` — an exec into the live container. It does not
    /// write `:latest`, but it must not run while the container is being
    /// removed out from under it.
    ///
    /// Not constructed on this branch, for the same reason as
    /// [`ProjectOp::Compaction`].
    #[allow(dead_code)]
    CacheClear,
    /// `container::scrub_secrets_from_snapshots` — the third writer of
    /// `triple-c-snapshot-{id}:latest`, reached from `clear_claude_token`. It
    /// creates a scratch container from the snapshot and commits back over the
    /// same tag, so it is the same read-modify-write shape as a compaction and
    /// loses the same race: any `:latest` move landing between its create and
    /// its commit is overwritten by an image derived from the pre-read state.
    SecretScrub,
}

impl ProjectOp {
    /// What is happening, phrased for the message a user reads.
    pub fn describe(self) -> &'static str {
        match self {
            ProjectOp::Migration => "A container base update is running for this project",
            ProjectOp::Compaction => "This project's snapshot is being compacted",
            ProjectOp::Recreate => "This project's container is being started or recreated",
            ProjectOp::Reset => "This project is being reset",
            ProjectOp::Destroy => "Something of this project's is being deleted",
            ProjectOp::CacheClear => "This project's caches are being cleared",
            ProjectOp::SecretScrub => "A revoked credential is being removed from this project's snapshot",
        }
    }

    /// What the *refused* caller was trying to do, for the tail of the message.
    fn blocked_action(self) -> &'static str {
        match self {
            ProjectOp::Migration => "starting a base update",
            ProjectOp::Compaction => "compacting its snapshot",
            ProjectOp::Recreate => "starting or recreating its container",
            ProjectOp::Reset => "resetting it",
            ProjectOp::Destroy => "deleting anything of its",
            ProjectOp::CacheClear => "clearing its caches",
            ProjectOp::SecretScrub => "removing a credential from its snapshot",
        }
    }
}

/// Project id → the operation currently holding it.
///
/// A `std::sync::Mutex` rather than a `tokio` one on purpose: it is only ever
/// held for the length of a `HashMap` insert or remove, never across an await,
/// and [`is_held_by`] has to be callable from the synchronous helpers in
/// `disk.rs` that already ask this question.
static HOLDERS: OnceLock<Mutex<HashMap<String, ProjectOp>>> = OnceLock::new();

fn holders() -> &'static Mutex<HashMap<String, ProjectOp>> {
    HOLDERS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// A claim on one project, released on drop.
///
/// RAII rather than an explicit release for the reason [`ProjectOp::Migration`]'s
/// predecessor already learned: a plain release statement is skipped by an
/// early `?`, by a panic, and by the future simply being dropped. A guard is
/// not.
/// Dropping this releases the claim, so a caller that discards it has taken no
/// lock at all — `let _ = try_acquire(...)` drops immediately and reads as
/// success. `#[must_use]` makes that a compile warning rather than a race.
#[must_use = "the claim is released as soon as this guard is dropped; bind it for the whole operation"]
#[derive(Debug)]
pub struct ProjectGuard {
    project_id: String,
}

impl Drop for ProjectGuard {
    fn drop(&mut self) {
        // `into_inner` on a poisoned lock: a panic while some other thread held
        // this map for the duration of one insert cannot have left it
        // inconsistent, and refusing to release afterwards would strand the
        // project as permanently busy.
        holders()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&self.project_id);
    }
}

/// Claim a project for `op`, or say who has it.
///
/// The error is user-facing copy, not a debug string — it goes straight back
/// over IPC to a toast.
pub fn try_acquire(project_id: &str, op: ProjectOp) -> Result<ProjectGuard, String> {
    let mut map = holders().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(holder) = map.get(project_id).copied() {
        return Err(format!(
            "{}. Wait for it to finish before {}.",
            holder.describe(),
            op.blocked_action()
        ));
    }
    map.insert(project_id.to_string(), op);
    Ok(ProjectGuard {
        project_id: project_id.to_string(),
    })
}

/// Which operation holds this project, if any.
pub fn held(project_id: &str) -> Option<ProjectOp> {
    holders()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(project_id)
        .copied()
}

/// Whether this project is held by exactly `op`.
///
/// `migration_commands::is_migrating` is this, specialised — which is the whole
/// point of folding `ACTIVE_MIGRATIONS` into this registry: there is now one
/// answer to "is something happening to this project", not two that can
/// disagree.
pub fn is_held_by(project_id: &str, op: ProjectOp) -> bool {
    held(project_id) == Some(op)
}

/// Whether any project **other than** `exclude_project_id` is currently held by
/// `op`. Pass an empty id to ask about every project.
///
/// Used by the daemon-wide reapers, which cannot tell which project a
/// `triple-c-compact-*` container belongs to — the name carries a random uuid,
/// not a project id — so "is this process compacting anything right now" is the
/// only in-process question they can ask before force-removing one. The
/// exclusion is for the reaper that runs *inside* a compaction, which is
/// already holding a claim of its own and would otherwise see it and skip.
///
/// No production caller on this branch: the compaction reaper it was written
/// for went to `hold/disk-and-dragout` with the rest of the Disk panel. Kept
/// (and still tested) because it is the only bound this module offers on the
/// cross-process case documented above.
#[allow(dead_code)]
pub fn any_held_excluding(op: ProjectOp, exclude_project_id: &str) -> bool {
    holders()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .any(|(project_id, held)| *held == op && project_id != exclude_project_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Ids are namespaced per test: the registry is process-global, and
    /// `cargo test` runs these on several threads at once.
    fn id(name: &str) -> String {
        format!("project-lock-test-{}", name)
    }

    #[test]
    fn a_second_acquire_on_the_same_project_is_refused() {
        let p = id("second-acquire");
        let first = try_acquire(&p, ProjectOp::Compaction).expect("first claim");
        let second = try_acquire(&p, ProjectOp::Recreate);
        let err = second.expect_err("a second claim must be refused, not queued");
        // The refusal has to name the holder — "busy" alone leaves the user
        // with nothing to wait for.
        assert!(err.contains("snapshot is being compacted"), "{}", err);
        assert!(err.contains("starting or recreating"), "{}", err);
        drop(first);
        // And it has to be retakeable the moment the holder goes away. Bound
        // rather than discarded: `#[must_use]` is what stops a real caller
        // writing `try_acquire(...)` and believing it holds something.
        let retaken = try_acquire(&p, ProjectOp::Recreate).expect("released on drop");
        drop(retaken);
    }

    #[test]
    fn the_guard_releases_on_an_early_return() {
        let p = id("early-return");
        fn bails(project_id: &str) -> Result<(), String> {
            let _guard = try_acquire(project_id, ProjectOp::Reset)?;
            Err("something failed".to_string())
        }
        assert!(bails(&p).is_err());
        assert_eq!(held(&p), None, "an early `?` must not strand the claim");
    }

    #[test]
    fn the_guard_releases_on_a_panic() {
        let p = id("panic");
        let result = std::panic::catch_unwind(|| {
            let _guard = try_acquire(&id("panic"), ProjectOp::Migration).unwrap();
            panic!("boom");
        });
        assert!(result.is_err());
        assert_eq!(held(&p), None, "a panic must not strand the claim either");
    }

    #[test]
    fn two_projects_do_not_block_each_other() {
        let a = id("independent-a");
        let b = id("independent-b");
        let _one = try_acquire(&a, ProjectOp::Compaction).expect("a");
        let _two = try_acquire(&b, ProjectOp::Compaction).expect("b");
        assert!(is_held_by(&a, ProjectOp::Compaction));
        assert!(is_held_by(&b, ProjectOp::Compaction));
    }

    #[test]
    fn is_held_by_distinguishes_the_operation() {
        let p = id("which-op");
        let _guard = try_acquire(&p, ProjectOp::Compaction).unwrap();
        assert!(is_held_by(&p, ProjectOp::Compaction));
        assert!(
            !is_held_by(&p, ProjectOp::Migration),
            "a compaction is not a migration — `is_migrating` is built on this"
        );
    }

    #[test]
    fn any_held_sees_across_projects() {
        let p = id("any-held");
        assert!(!any_held_excluding(ProjectOp::Destroy, ""));
        let _guard = try_acquire(&p, ProjectOp::Destroy).unwrap();
        assert!(any_held_excluding(ProjectOp::Destroy, ""));
        // …and a holder can ask the question without its own claim answering
        // it, which is what lets a compaction sweep leftovers before it starts.
        assert!(!any_held_excluding(ProjectOp::Destroy, &p));
    }

    /// Concurrency, not just sequencing: N threads racing for one project must
    /// produce exactly one winner.
    #[test]
    fn exactly_one_of_many_racing_threads_wins() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;

        let p = id("race");
        let start = Arc::new(std::sync::Barrier::new(8));
        // The second barrier is what makes this deterministic rather than
        // merely likely: no winner releases until every thread has had its
        // turn, so "only one got in" cannot be an artefact of a loser arriving
        // after the winner already left.
        let attempted = Arc::new(std::sync::Barrier::new(8));
        let won = Arc::new(AtomicUsize::new(0));
        let mut handles = Vec::new();
        for _ in 0..8 {
            let start = Arc::clone(&start);
            let attempted = Arc::clone(&attempted);
            let won = Arc::clone(&won);
            let p = p.clone();
            handles.push(std::thread::spawn(move || {
                start.wait();
                let claim = try_acquire(&p, ProjectOp::Compaction);
                if claim.is_ok() {
                    won.fetch_add(1, Ordering::SeqCst);
                }
                attempted.wait();
                drop(claim);
            }));
        }
        for handle in handles {
            handle.join().unwrap();
        }
        assert_eq!(
            won.load(Ordering::SeqCst),
            1,
            "eight threads raced for one project and more than one got in"
        );
        assert_eq!(held(&p), None);
    }
}
