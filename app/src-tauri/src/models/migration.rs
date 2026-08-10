//! Contract types for **container base-image migration**.
//!
//! ## Why this exists
//!
//! A project's container is created from `triple-c-snapshot-<id>:latest`
//! whenever that image exists, and every recreation re-commits it. Nothing ever
//! moved a project back onto a *newer base image*: `container_needs_recreation`
//! compared the container's actual image against the `triple-c.image` label that
//! `create_container` wrote from the very image it created from — a tautology
//! that could never fire. So a project stayed pinned to its own snapshot
//! lineage forever and never picked up base-image fixes (a new `socat`, a new
//! `/usr/local/bin` shim, security updates). The only escape was Reset, which
//! deletes both named volumes and takes the login, the skills and every session
//! transcript with it.
//!
//! Migration is the non-destructive alternative: recreate the container from the
//! current base, then replay onto it the small set of things the base does not
//! carry, and leave the volumes strictly alone.
//!
//! ## What actually needs replaying
//!
//! `/home/claude` is the named volume `triple-c-home-<id>`, with
//! `/home/claude/.claude` nested inside it. The image's own `/home/claude` is
//! **seed-only** — once the volume is mounted the image's copy is masked
//! permanently. So Claude Code itself (it installs to `~/.local/bin`), cargo,
//! uv, ruff, the OAuth login, `~/.claude.json`, skills, transcripts, scheduler
//! tasks and SSH keys all re-attach for free across an image swap.
//!
//! What is genuinely lost is confined to the container's writable layer:
//! root-level `apt` installs, `npm -g` packages (npm's prefix is `/usr`),
//! `/usr/local`, `/opt`, `/srv`, and anything under `/workspace` that is not on
//! a bind mount. Those four categories are exactly what
//! [`MigrationOptions`] can replay.
//!
//! ## Serde
//!
//! Plain snake_case, matching every other IPC struct in this crate
//! (`ContainerInfo`, `ClaudeSession`, …) and `app/src/lib/types.ts`.

use serde::{Deserialize, Serialize};

/// How a finished migration attempt ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MigrationPhase {
    /// The container now runs on the current base and everything requested was
    /// replayed.
    Succeeded,
    /// The container now runs on the current base, but at least one package or
    /// path could not be replayed. Deliberately distinct from `Failed`: one
    /// missing apt package must never cost the user the whole migration.
    Partial,
    /// The migration could not complete. If the container had already been
    /// swapped, an automatic rollback was attempted — check
    /// [`MigrationReport::rollback_available`] and the message.
    Failed,
    /// The migration was undone; the container is back on its pre-migration
    /// snapshot image.
    RolledBack,
}

/// One package that could not be replayed onto the new base.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackageFailure {
    pub name: String,
    /// Trimmed tail of the package manager's own error output.
    pub reason: String,
}

/// Everything the UI needs to decide whether a project is worth migrating, and
/// to explain to the user what migrating would actually change.
///
/// A field being empty always means "nothing found", never "not checked" —
/// [`ContainerStaleness::probe_error`] is the single place a failed inspection
/// is reported.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ContainerStaleness {
    /// The container's lineage is not the current base image.
    /// Always `false` when `known` is `false` — an unknown lineage is not a
    /// claim of staleness.
    pub stale: bool,
    /// Whether the lineage could be established at all. `false` means the
    /// container (or its snapshot image) predates the `triple-c.base-image-id`
    /// label, i.e. **"unknown, probe instead"** — never "stale".
    pub known: bool,
    /// Image ID of the base this container's lineage descends from.
    pub base_image_id: Option<String>,
    /// Image ID of the base image currently configured in settings.
    pub current_base_image_id: Option<String>,
    /// `Created` timestamp of the project's snapshot image, RFC 3339.
    pub snapshot_created_at: Option<String>,
    /// Concrete paths the current base ships that this container does not,
    /// e.g. `/usr/bin/socat`.
    pub missing_paths: Vec<String>,
    /// Human labels for the same, e.g. `"Auth bridge tunnel (socat)"`.
    pub missing_features: Vec<String>,
    /// `apt-mark showmanual` in the container minus the base's own set — the
    /// packages a migration would replay.
    pub apt_delta: Vec<String>,
    /// Globally-installed npm packages the base does not ship.
    pub npm_global_delta: Vec<String>,
    /// Non-dpkg-owned paths under the verbatim-copy roots that would be carried
    /// across. Empty when nothing user-authored was found.
    pub verbatim_paths: Vec<String>,
    /// dpkg packages the current base carries at a different version than this
    /// container does. A rough "how much security drift" number, not a promise
    /// that every one of them is newer.
    pub outdated_package_count: u32,
    /// Set when the container/image could not be inspected. Everything else is
    /// then at its default.
    pub probe_error: Option<String>,
}

/// What a migration should replay. All three default to off so that
/// `MigrationOptions::default()` is the minimal, fastest migration.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MigrationOptions {
    /// Replay the apt and `npm -g` deltas onto the new base.
    #[serde(default)]
    pub replay_packages: bool,
    /// Copy the verbatim payload (`/usr/local`, `/opt`, `/srv`, and the
    /// non-bind-mounted parts of `/workspace`) onto the new base.
    #[serde(default)]
    pub copy_paths: bool,
    /// Keep the `:pre-migration-<ts>` rollback tag after the migration reports
    /// success. Costs the full size of the old snapshot image (snapshots share
    /// almost no layers with the current base) but makes rollback instant.
    #[serde(default)]
    pub keep_rollback: bool,
}

/// The outcome of one migration attempt.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MigrationReport {
    pub phase: MigrationPhase,
    pub packages_requested: Vec<String>,
    pub packages_installed: Vec<String>,
    pub packages_failed: Vec<PackageFailure>,
    pub paths_copied: Vec<String>,
    /// Human labels for base features the container gained, e.g.
    /// `"Auth bridge tunnel (socat)"`.
    pub features_restored: Vec<String>,
    /// A `:pre-migration-<ts>` image tag still exists, so
    /// `rollback_migration` can put the old system layer back.
    pub rollback_available: bool,
    /// One paragraph fit to show the user verbatim.
    pub message: String,
}

impl MigrationReport {
    /// A report for a migration that never got past pre-flight. Nothing was
    /// touched, so there is nothing to roll back.
    pub fn failed_preflight(message: impl Into<String>) -> Self {
        Self {
            phase: MigrationPhase::Failed,
            packages_requested: Vec::new(),
            packages_installed: Vec::new(),
            packages_failed: Vec::new(),
            paths_copied: Vec::new(),
            features_restored: Vec::new(),
            rollback_available: false,
            message: message.into(),
        }
    }
}

/// What a migration decided to do, frozen at pre-flight time.
///
/// Persisted with the state because a **resume** cannot recompute it: by the
/// time the app comes back up the container has already been replaced by one
/// created from the base, so its apt/npm sets *are* the base's and the deltas
/// would come out empty.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct MigrationPlan {
    pub apt_packages: Vec<String>,
    pub npm_packages: Vec<String>,
    pub verbatim_paths: Vec<String>,
    /// Base-image paths the old container lacked, so the finished migration can
    /// report which of them it actually gained.
    pub missing_paths: Vec<String>,
}

/// Persisted, host-side migration state. Written **before** anything
/// destructive happens and removed on confirm or rollback, so a crash at any
/// point leaves a record of what was in flight.
///
/// `phase` is a free-form string rather than [`MigrationPhase`] because it also
/// carries the *in-flight* phases, which are not outcomes:
///
/// | `phase` | Meaning | Offered next |
/// |---|---|---|
/// | `in-progress` | A migration is running right now | — |
/// | `interrupted` | The app died after the container swap | resume, rollback |
/// | `awaiting-confirmation` | Migration finished; rollback still possible | confirm, rollback |
///
/// See [`MIGRATION_PHASE_IN_PROGRESS`] and friends.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MigrationState {
    pub phase: String,
    /// Image ID of the snapshot the project was on before the swap.
    pub from_image_id: Option<String>,
    /// Image ID of the base being migrated to.
    pub to_base_id: Option<String>,
    /// RFC 3339.
    pub started_at: String,
    /// Present once the attempt produced one.
    #[serde(default)]
    pub report: Option<MigrationReport>,
    /// The `:pre-migration-<ts>` tag holding the old system layer, if one was
    /// created. `rollback_migration` retags this back to `:latest`.
    #[serde(default)]
    pub rollback_image: Option<String>,
    /// Host path of the staged verbatim payload tar, if one was staged.
    #[serde(default)]
    pub staging_path: Option<String>,
    /// The options the attempt was started with, so a resume replays the same
    /// things the user originally asked for.
    #[serde(default)]
    pub options: MigrationOptions,
    /// The frozen pre-flight plan. See [`MigrationPlan`].
    #[serde(default)]
    pub plan: Option<MigrationPlan>,
}

/// A migration is running in this process right now.
pub const MIGRATION_PHASE_IN_PROGRESS: &str = "in-progress";
/// The app died after the container swap but before the final commit.
pub const MIGRATION_PHASE_INTERRUPTED: &str = "interrupted";
/// The migration finished; the user has not yet confirmed or rolled back.
pub const MIGRATION_PHASE_AWAITING: &str = "awaiting-confirmation";

impl MigrationState {
    pub fn new(
        from_image_id: Option<String>,
        to_base_id: Option<String>,
        options: MigrationOptions,
    ) -> Self {
        Self {
            phase: MIGRATION_PHASE_IN_PROGRESS.to_string(),
            from_image_id,
            to_base_id,
            started_at: chrono::Utc::now().to_rfc3339(),
            report: None,
            rollback_image: None,
            staging_path: None,
            options,
            plan: None,
        }
    }
}
