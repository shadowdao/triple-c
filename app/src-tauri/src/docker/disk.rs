//! Disk accounting and reclaim for the objects Triple-C creates.
//!
//! ## The problem this exists to make visible
//!
//! Every recreation runs `docker commit`, and a commit **stacks a new layer**
//! rather than rewriting one. A file deleted after it has been committed does
//! not give its bytes back — the layer above records a whiteout and the
//! original bytes stay below it forever. `container_needs_recreation` has 24
//! conditions, so changing one settings field costs a multi-gigabyte layer that
//! nothing in the app ever reclaims. One project was measured at 14 stacked
//! commit layers, ~5.1 GB above its base, 12.3 GB total.
//!
//! Prevention already landed (the pre-commit scrub, capped container logs,
//! base-image labels, the startup sweep, the migration-pin reaper). What was
//! missing is the half a user can act on: *seeing* where the bytes are, and
//! being able to get them back. That is this module.
//!
//! **Making the layer count visible is the point.** "Snapshot 12.3 GB / 14
//! layers / next commit adds 868 MB" explains the growth mechanism in one row,
//! which no total ever does.
//!
//! ## Why the scan is explicit
//!
//! [`scan`] is built on `Docker::df()` (`GET /system/df`), which walks every
//! image, container and volume on the daemon and computes shared-layer sizes.
//! On a 100 GB store that is seconds, not milliseconds, and it is the *only*
//! call that populates `ImageSummary::shared_size`, `ContainerSummary::size_rw`
//! and `VolumeUsageData::size` at all. So it sits behind a Scan button and is
//! never run on panel open or on a timer.
//!
//! ## Safety, which is the whole design
//!
//! Reclaim targets are split across **two Rust types that cannot be confused
//! for one another**:
//!
//! * [`ReclaimTarget`] — safe and semi-safe work. Everything here is either
//!   already unreachable (dangling images, ownerless rollback pins, probe and
//!   scrub leftovers), regenerable (build cache, package caches), or a rewrite
//!   that preserves content (snapshot compaction). [`reclaim`] accepts these.
//! * [`DestructiveTarget`] — a live project's home volume, config volume,
//!   snapshot image, or a rollback pin whose migration is still awaiting
//!   confirmation. [`destroy`] accepts these, one at a time, and only against a
//!   typed confirmation of the project name.
//!
//! `reclaim` does not have a code path that can reach a `DestructiveTarget` —
//! it cannot be passed one. That is deliberate: the guarantee is in the type
//! system rather than in a runtime check somebody can forget to write.
//!
//! Two rules apply throughout, and both are inherited from
//! `sweep_orphaned_snapshots`:
//!
//! * **Never call an unfiltered prune.** `prune_images`/`prune_volumes` without
//!   filters would reach the user's own postgres, mysql and site-builder work
//!   on the same daemon. Every removal here names one object we created.
//! * **Only ever touch a `triple-c*` name or a `triple-c.*` label.**

use std::collections::{HashMap, HashSet};

use bollard::container::ListContainersOptions;
use bollard::image::{ListImagesOptions, RemoveImageOptions};
use bollard::models::{BuildCache, ContainerSummary, ImageSummary, Volume};
use serde::{Deserialize, Serialize};

use super::client::get_docker;
use super::container::{
    self, config_volume_name, home_volume_name, get_snapshot_image_name, CONFIG_VOLUME_PREFIX,
    HOME_VOLUME_PREFIX, LABEL_BASE, LABEL_MANAGED,
};
use super::migration;
use crate::models::Project;
use crate::storage::migration_store;

/// Default age filter for a build-cache prune, matching `docker builder prune
/// --filter until=168h`. A week is long enough that an active build tree keeps
/// its warm cache and short enough that abandoned trees are collected.
pub const BUILD_CACHE_DEFAULT_UNTIL_HOURS: i64 = 168;

/// How long [`docker_cli`] waits for the `docker` command line tool.
///
/// Long enough for `buildx du` over a large build tree on a cold daemon, short
/// enough that a wedged daemon does not hold the Scan button down forever. A
/// prune uses the same bound; a prune that outruns it has still done its work
/// on the daemon, and the next scan reports the result.
const DOCKER_CLI_TIMEOUT_SECS: u64 = 45;

/// The bound for a `builder prune`, which is a different kind of wait.
///
/// `buildx du` is a query and 45 seconds is generous for it. A prune of a
/// 60 GB cache genuinely takes minutes, and cancelling it does not undo the
/// daemon-side work — it only loses the `Total reclaimed space:` line, so the
/// run would be reported as a failure that in fact freed the space. Ten minutes
/// is a bound against a wedged daemon rather than against a slow one.
const DOCKER_PRUNE_TIMEOUT_SECS: u64 = 600;

// ---------------------------------------------------------------------------
// Scan result
// ---------------------------------------------------------------------------

/// One row of the per-project table — the mental model users actually have.
///
/// Serde is plain snake_case, matching every other IPC struct in this crate and
/// `app/src/lib/types.ts`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ProjectDiskRow {
    pub project_id: String,
    pub project_name: String,
    /// `triple-c-snapshot-{id}:latest`, present whether or not it exists yet.
    pub snapshot_image: String,
    pub snapshot_exists: bool,
    /// Total size of the snapshot image, base image included.
    pub snapshot_bytes: i64,
    /// Bytes of the snapshot that are *also* in some other image — almost
    /// always the shared base. Only `df()` computes this.
    pub snapshot_shared_bytes: i64,
    /// How many layers the snapshot has stacked **above its base image**. This
    /// is the number that explains the growth: one per recreation.
    ///
    /// Only means that when [`Self::base_lineage_known`] is true. Otherwise it
    /// is every layer carrying bytes, base included — an upper bound, and a
    /// misleading one to present as a recreation count.
    pub snapshot_commit_layers: u32,
    /// Whether the base image this snapshot descends from could be identified.
    ///
    /// False when `triple-c.base-image-id` is absent, which is the **normal**
    /// case for a project created before that label existed. The UI must not
    /// present `snapshot_commit_layers` as a recreation count in that state,
    /// and compaction is not offered, because a never-recreated project would
    /// otherwise report its base's ~15 layers and qualify.
    pub base_lineage_known: bool,
    /// Bytes those stacked layers account for. `None` when the base image the
    /// snapshot descends from is no longer on the daemon, so the split cannot
    /// be measured and must not be guessed.
    pub snapshot_above_base_bytes: Option<i64>,
    pub container_exists: bool,
    pub container_running: bool,
    /// The container's writable layer — i.e. **exactly what the next commit
    /// will add** to the snapshot. Surfaced under that name in the UI.
    pub container_writable_bytes: i64,
    pub home_volume_bytes: i64,
    pub home_volume_present: bool,
    pub config_volume_bytes: i64,
    pub config_volume_present: bool,
    /// **The one snapshot figure the row adds up from.**
    ///
    /// The Snapshot column shows this and [`Self::total_bytes`] is computed
    /// from it, so the Total column reconciles with its parts. It did not
    /// before: `total_bytes` used `size - shared_size` unconditionally while
    /// the column fell back to [`Self::snapshot_above_base_bytes`] or to `—`,
    /// and in that fallback branch `size - shared_size` is the *whole base
    /// image*. A row could show `—` for its snapshot and still carry 4.7 GB of
    /// base in its total, which `triple_c_total_bytes` then added again as a
    /// base-image row.
    ///
    /// The rule, in order:
    ///
    /// 1. `df()` computed a shared size → `size - shared`, the daemon's own
    ///    measurement of what is unique to this image.
    /// 2. No shared size but the base lineage is known → the layer arithmetic
    ///    in [`layer_stats`].
    /// 3. Neither → the full size. Not a fallback to zero: an image nothing
    ///    shares with and whose lineage is unknown really does cost its whole
    ///    size, and a flattened snapshot is exactly that shape.
    pub snapshot_attributed_bytes: i64,
    pub total_bytes: i64,
    /// A migration is in flight; every action on this row is blocked.
    pub migrating: bool,
}

/// A base image, shared by every project built from it.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct BaseImageRow {
    pub reference: String,
    pub bytes: i64,
    pub shared_bytes: i64,
    /// Containers still built from it, as `df()` counts them. A base with
    /// `containers > 0` cannot be removed and is not offered.
    pub containers: i64,
    /// Carries `triple-c.base=true`.
    pub is_labelled_base: bool,
}

/// Where the daemon actually keeps its bytes, and whether the Windows/WSL2
/// caveat applies. See [`WSL2_VHDX_NOTE`].
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct HostStorage {
    /// `docker info`'s `DockerRootDir`. On Docker Desktop this is a path
    /// *inside the VM*, not something the host can stat — which is the whole
    /// reason the WSL2 note exists.
    pub docker_root_dir: String,
    /// `docker info`'s `OperatingSystem`, e.g. `"Docker Desktop"`.
    pub operating_system: String,
    pub is_docker_desktop: bool,
    /// The app itself is running on Windows.
    pub is_windows_host: bool,
    /// Windows + Docker Desktop: pruning frees space *inside* `ext4.vhdx` and
    /// returns nothing to `C:` until the disk is compacted.
    pub vhdx_applies: bool,
    /// [`WSL2_VHDX_NOTE`], [`WSL2_VHDX_FIX`] and [`WSL2_VHDX_FIX_GUI`], carried
    /// over IPC rather than restated in the frontend.
    ///
    /// A second copy of this copy in TypeScript would drift from the one the
    /// Rust tests pin, and this is the paragraph that stops a user reporting
    /// "I pruned and C: did not change" as a bug. Empty when the caveat does
    /// not apply, so the UI has nothing to decide.
    pub vhdx_note: String,
    pub vhdx_fix: Vec<String>,
    pub vhdx_fix_gui: String,
}

/// Everything one Scan produces.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct DiskUsageReport {
    /// RFC3339. The UI shows how stale the numbers are rather than refreshing
    /// them, because a refresh costs another `df()`.
    pub scanned_at: String,
    pub projects: Vec<ProjectDiskRow>,
    pub base_images: Vec<BaseImageRow>,
    pub base_images_bytes: i64,
    /// Dangling `triple-c.managed=true` images — superseded snapshot commits.
    pub orphan_image_bytes: i64,
    pub orphan_image_count: usize,
    /// `triple-c-home-*` / `triple-c-claude-config-*` volumes belonging to no
    /// project in the store. Empty — and `orphan_volumes_unavailable` set —
    /// when the store could not be read.
    pub orphan_volumes: Vec<OrphanVolume>,
    pub orphan_volume_bytes: i64,
    pub orphan_volumes_unavailable: Option<String>,
    pub build_cache: BuildCacheUsage,
    /// Daemon-wide totals, for context: the user's unrelated work lives here
    /// too, and the per-project rows will not add up to `docker system df`.
    pub images_total_bytes: i64,
    pub containers_total_bytes: i64,
    pub volumes_total_bytes: i64,
    /// Sum of the per-project rows — the part of the daemon that is ours.
    pub triple_c_total_bytes: i64,
    pub host: HostStorage,
}

/// Build-cache figures, and where they came from.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct BuildCacheUsage {
    pub total_bytes: i64,
    pub reclaimable_bytes: i64,
    /// Bytes a `--filter until=168h` prune would reach.
    pub stale_bytes: i64,
    /// `"buildx du"` or `"system df"`.
    ///
    /// **`docker system df` under-reports build-cache reclaimable while
    /// `docker buildx du` reports it correctly** (df only counts records with
    /// no parent as reclaimable). The buildx figure is preferred whenever the
    /// CLI is reachable; the field says which one is on screen so a user
    /// comparing against their terminal is not left guessing.
    pub source: String,
    /// Set when the `docker` CLI could not be run, so `source` fell back.
    pub cli_error: Option<String>,
}

/// A per-project volume whose project id is not in Triple-C's project store.
///
/// **Not "a volume with no container".** See [`orphan_volumes`] for why that
/// distinction is the whole safety property.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct OrphanVolume {
    pub name: String,
    /// The project id parsed out of the name. Shown, because a user who
    /// recognises it may want to recover it rather than delete it.
    pub project_id: String,
    pub bytes: i64,
    /// `"home"` or `"config"`. The config volume is the one that held Claude
    /// credentials and transcripts, so it is worth saying which is which.
    pub role: String,
    /// When Docker created the volume, from `df()`'s own metadata.
    ///
    /// Evidence, not bookkeeping: a size and a UUID identify nothing, and this
    /// is the only cheap fact that lets a user recognise which project a
    /// candidate was before deleting it. It costs no extra call.
    ///
    /// **Never inspect a volume by mounting it.** `docker run -v <name>:/path`
    /// *creates* the volume when it does not exist, so a "just look inside"
    /// probe can conjure the very thing it was checking for. Everything shown
    /// about a volume here comes from `df()` metadata.
    pub created_at: Option<String>,
}

// ---------------------------------------------------------------------------
// Reclaim targets
// ---------------------------------------------------------------------------

/// How much trust an action needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Safety {
    /// Already unreachable, or regenerated on demand. One button, no
    /// confirmation.
    Safe,
    /// Reversible in substance but not in time — a rewrite, or a cache the user
    /// pays to refill. One clear confirmation.
    SemiSafe,
}

/// Work [`reclaim`] is allowed to do.
///
/// **This type cannot express a destructive action.** Adding a variant that
/// deletes a live project's data would be the mistake this split exists to
/// prevent; put it on [`DestructiveTarget`] instead.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ReclaimTarget {
    /// Dangling `triple-c.managed=true` images that are *not* base images —
    /// the superseded snapshot commits every recreation leaves behind. This is
    /// `sweep_orphaned_snapshots`, made visible and runnable on demand.
    DanglingSnapshots,
    /// Dangling `triple-c.managed=true` images that *are* base images
    /// (`triple-c.base=true`). Same sweep, reported separately because a user
    /// recognises "the old sandbox image" and not "a dangling commit".
    SupersededBaseImages,
    /// `docker builder prune`. **Daemon-wide, not Triple-C-only.**
    BuildCache {
        /// `true` prunes everything; `false` filters `until=168h`.
        all: bool,
    },
    /// `pre-migration-*` tags no migration record claims. Untagged only — the
    /// image becomes dangling and the sweep collects it under its own rules.
    MigrationPins,
    /// `{id}-payload.tar` staging files with no record beside them. These are
    /// host files under the user's data dir, **not** inside the daemon's
    /// storage — on Windows they sit on `C:` directly rather than in the vhdx,
    /// so this is the one bucket that gives space back to `C:` immediately.
    MigrationStaging,
    /// Containers labelled `triple-c.probe=migration`.
    ProbeContainers,
    /// `triple-c-scrub-*` containers left by an interrupted secret rewrite.
    ScrubContainers,
    /// Rewrite a project's stacked commit layers into a single layer. The
    /// highest-yield action in this module.
    CompactSnapshot { project_id: String },
    /// `rm -rf` the regenerable package caches in a running container's home
    /// volume.
    ClearCaches {
        project_id: String,
        /// `~/.rustup/toolchains` — regenerable, but expensive to re-download,
        /// so it is a separate tick rather than part of the set.
        include_rustup: bool,
    },
}

/// Work [`destroy`] is allowed to do, one item at a time, against a typed
/// confirmation. Every variant deletes something with no other copy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DestructiveTarget {
    /// The project's home volume: shell history, dotfiles, installed
    /// toolchains, Playwright browsers.
    HomeVolume { project_id: String },
    /// The project's config volume: **Claude credentials, plugins and every
    /// conversation transcript**.
    ConfigVolume { project_id: String },
    /// The project's snapshot image: every package the agent ever installed.
    /// The project falls back to the base image on its next start.
    SnapshotImage { project_id: String },
    /// A rollback pin whose migration is still awaiting confirmation — the only
    /// copy of that migration's rollback target.
    RollbackPin { project_id: String, tag: String },
    /// A `triple-c-home-*` / `triple-c-claude-config-*` volume whose project id
    /// is in no `projects.json` this app can find.
    ///
    /// **It was a `ReclaimTarget` at `Safety::Safe`** — a tick and the group
    /// Reclaim button, no confirmation. The object behind that tick is a
    /// `triple-c-claude-config-*` volume holding a Claude OAuth credential,
    /// every installed plugin and skill, and every conversation transcript the
    /// project ever had, and the *same volume* for a project still in the store
    /// requires typing the project's name. The only difference between the two
    /// is a lookup against a file this app has been wrong about before: a
    /// second app instance's project is absent from an in-memory list, a
    /// corrupt `projects.json` empties it, and a data directory restored
    /// without it empties it too. So it is confirmed like everything else that
    /// has no other copy — see [`destroy`], where the typed string is the
    /// **volume name**, there being no project name to type.
    OrphanVolume {
        name: String,
        /// The id parsed out of the volume name. Display only — it names no
        /// project in the store, which is the whole reason this variant exists.
        project_id: String,
    },
}

impl ReclaimTarget {
    /// How much confirmation the UI must ask for. Pure, and pinned by a test
    /// that walks every variant.
    pub fn safety(&self) -> Safety {
        match self {
            // Unreachable already, or a host file nothing refers to.
            ReclaimTarget::DanglingSnapshots
            | ReclaimTarget::SupersededBaseImages
            | ReclaimTarget::MigrationPins
            | ReclaimTarget::MigrationStaging
            | ReclaimTarget::ProbeContainers
            | ReclaimTarget::ScrubContainers
            | ReclaimTarget::BuildCache { .. } => Safety::Safe,
            // A rewrite and a cache flush: nothing is lost, but time is.
            ReclaimTarget::CompactSnapshot { .. } | ReclaimTarget::ClearCaches { .. } => {
                Safety::SemiSafe
            }
        }
    }

    /// Whether the action reaches beyond Triple-C's own objects.
    ///
    /// Only the build cache does, and the UI has to say so out loud: the same
    /// daemon holds the user's unrelated postgres/mysql/site-builder work, and
    /// a prune takes their warm cache with ours.
    pub fn is_daemon_wide(&self) -> bool {
        matches!(self, ReclaimTarget::BuildCache { .. })
    }

    /// The project this acts on, when it acts on one.
    pub fn project_id(&self) -> Option<&str> {
        match self {
            ReclaimTarget::CompactSnapshot { project_id }
            | ReclaimTarget::ClearCaches { project_id, .. } => Some(project_id),
            _ => None,
        }
    }
}

impl DestructiveTarget {
    pub fn project_id(&self) -> &str {
        match self {
            DestructiveTarget::HomeVolume { project_id }
            | DestructiveTarget::ConfigVolume { project_id }
            | DestructiveTarget::SnapshotImage { project_id }
            | DestructiveTarget::RollbackPin { project_id, .. }
            | DestructiveTarget::OrphanVolume { project_id, .. } => project_id,
        }
    }

}

/// One offered action, with its measured cost.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ReclaimItem {
    pub target: ReclaimTarget,
    pub safety: Safety,
    pub daemon_wide: bool,
    pub label: String,
    pub detail: String,
    /// Bytes this would free, **measured**, never estimated.
    pub bytes: i64,
    /// `false` when `bytes` is a bound rather than a measurement — set only by
    /// snapshot compaction, whose real yield cannot be known until it runs.
    /// The UI must say "up to" whenever this is false.
    pub bytes_are_exact: bool,
    /// For compaction: the lower bound of the range, when `bytes` is the upper.
    pub bytes_floor: Option<i64>,
    /// Populated when the action cannot run right now (migration in flight,
    /// container in the wrong state). The UI disables the tick and shows this.
    pub blocked: Option<String>,
}

/// Everything [`list_reclaimable`] found.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ReclaimPlan {
    pub items: Vec<ReclaimItem>,
    /// Destructive per-project objects, surfaced for display only. These are
    /// never in `items` and [`reclaim`] cannot act on them.
    pub destructive: Vec<DestructiveItem>,
    /// Set when the project store could not be read. Orphan detection is
    /// suppressed entirely in that case — see [`orphan_volumes`].
    pub store_error: Option<String>,
}

/// A destructive object, described so the UI can offer it per project.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DestructiveItem {
    pub target: DestructiveTarget,
    pub project_id: String,
    pub project_name: String,
    pub label: String,
    /// Spelled out in full — this is the copy the confirmation shows.
    pub loses: String,
    pub bytes: i64,
    pub blocked: Option<String>,
}

/// What actually happened.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ReclaimOutcome {
    pub results: Vec<ReclaimResult>,
    /// Sum of the measured `freed_bytes` below.
    pub total_freed_bytes: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ReclaimResult {
    /// The reclaim target this reports on, or `None` when it reports a
    /// [`destroy`].
    ///
    /// Deliberately not reused to carry a destructive action: an earlier
    /// version returned `OrphanVolume { name }` for a home-volume deletion,
    /// which named a volume that was never an orphan and would attribute the
    /// outcome to a plan row the user never ticked. `destroyed` carries it
    /// instead, and exactly one of the two is ever set.
    pub target: Option<ReclaimTarget>,
    /// The destructive action this reports on, when it is one.
    #[serde(default)]
    pub destroyed: Option<DestructiveTarget>,
    pub ok: bool,
    /// Bytes actually freed, measured after the fact.
    pub freed_bytes: i64,
    /// What was projected before the run, for the one action that projects.
    pub projected_bytes: Option<i64>,
    pub message: String,
}

// ---------------------------------------------------------------------------
// Pure helpers — everything below is unit-tested without a daemon
// ---------------------------------------------------------------------------

/// Classification of a dangling `triple-c.managed=true` image.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DanglingClass {
    /// Built from `container/Dockerfile`, which stamps `triple-c.base=true`.
    Base,
    /// A superseded `docker commit` from some project's recreation.
    SnapshotCommit,
}

/// Split the dangling managed images into the two buckets the UI shows.
///
/// The base label is the only thing separating them, and it is reliable for the
/// same reason `triple-c.managed` is: `create_container` writes
/// `triple-c.base` **explicitly empty**, so a container built from a base
/// cannot inherit `true` and have its commit claim to be a base image.
pub fn classify_dangling(labels: &HashMap<String, String>) -> DanglingClass {
    if labels.get(LABEL_BASE).map(String::as_str) == Some("true") {
        DanglingClass::Base
    } else {
        DanglingClass::SnapshotCommit
    }
}

/// A volume as far as orphan detection is concerned.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct VolumeFacts {
    pub name: String,
    pub bytes: i64,
    /// `VolumeUsageData::ref_count` — containers currently referencing it.
    /// `-1` means the daemon did not compute it, which is **not** zero.
    ///
    /// Note what this is *not* used for: a zero ref count is not evidence that
    /// a volume is unclaimed. An idle project whose container has been removed
    /// has exactly this shape. It is only ever an extra brake on top of the
    /// store check.
    pub links: i64,
    pub created_at: Option<String>,
}

/// Volumes that look like ours and belong to no project in the store.
///
/// ## The only authority is the project store
///
/// From the daemon's side an **idle live project and a deleted one are
/// indistinguishable**. A project that has not been opened for a while has had
/// its container removed and may have no snapshot image either — volumes alone,
/// no container, nothing running. That is the *normal* resting state of a live
/// project, not a signal.
///
/// This is not hypothetical: the heuristic "no container and no snapshot image
/// means orphaned" was tried against a real project list and flagged two live
/// projects whose volumes held `.credentials.json`, Claude transcripts and
/// shell history. So nothing in this function looks at containers, images or
/// activity. The test is membership in the project store, and only that.
///
/// ## Why a store-load failure returns nothing
///
/// The whole test is "in the store? then live". If the store failed to load,
/// *every* project's volumes look unclaimed — a blanket delete would wipe the
/// credentials, transcripts and toolchains of every project the user has. So a
/// failure returns an empty set and the caller says why, rather than returning
/// what would look like a very productive reclaim.
///
/// `links == 0` is required on top of that. A volume with a container attached
/// belongs to something, whatever the store says, and `-1` (not computed) is
/// treated as "attached" for the same reason: unknown is never permission.
pub fn orphan_volumes(
    volumes: &[VolumeFacts],
    known_project_ids: &HashSet<String>,
    store_loaded: bool,
) -> Vec<OrphanVolume> {
    if !store_loaded {
        return Vec::new();
    }
    let mut out = Vec::new();
    for volume in volumes {
        let (project_id, role) = match parse_project_volume_name(&volume.name) {
            Some(parsed) => parsed,
            None => continue,
        };
        if known_project_ids.contains(project_id) {
            continue;
        }
        if volume.links != 0 {
            continue;
        }
        out.push(OrphanVolume {
            name: volume.name.clone(),
            project_id: project_id.to_string(),
            bytes: volume.bytes.max(0),
            role: role.to_string(),
            created_at: volume.created_at.clone(),
        });
    }
    out.sort_by(|a, b| b.bytes.cmp(&a.bytes).then_with(|| a.name.cmp(&b.name)));
    out
}

/// Split a project volume name into `(project_id, role)`.
///
/// Order matters and is not interchangeable: `triple-c-claude-config-` is
/// checked first because `triple-c-home-` does not prefix it, but a future
/// prefix that *does* nest would silently mis-attribute if this were reversed.
pub fn parse_project_volume_name(name: &str) -> Option<(&str, &'static str)> {
    if let Some(id) = name.strip_prefix(CONFIG_VOLUME_PREFIX) {
        if id.is_empty() {
            return None;
        }
        return Some((id, "config"));
    }
    if let Some(id) = name.strip_prefix(HOME_VOLUME_PREFIX) {
        if id.is_empty() {
            return None;
        }
        return Some((id, "home"));
    }
    None
}

/// This project's share of its snapshot image, in bytes.
///
/// The single rule behind [`ProjectDiskRow::snapshot_attributed_bytes`], pulled
/// out of [`scan`] so it can be tested without a daemon — the bug it fixes was
/// two call sites disagreeing, and a rule that lives in one function cannot
/// disagree with itself.
///
/// 1. `df()` computed a shared size → `size - shared`. The daemon's own
///    measurement of what is unique to this image, and the only exact answer
///    available.
/// 2. No shared size, but the base lineage is known → the layer arithmetic from
///    [`layer_stats`].
/// 3. Neither → `size - shared`, which with no shared size is the full image.
///    Deliberately not zero: an image that shares nothing measurable really
///    does cost its whole size, and a flattened snapshot is exactly that shape.
pub fn snapshot_attribution(
    snapshot_bytes: i64,
    snapshot_shared_bytes: i64,
    above_base_bytes: Option<i64>,
) -> i64 {
    let unique = (snapshot_bytes - snapshot_shared_bytes.max(0)).max(0);
    if snapshot_shared_bytes > 0 {
        return unique;
    }
    above_base_bytes.map(|b| b.max(0)).unwrap_or(unique)
}

/// What a snapshot's layer stack looks like relative to its base.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LayerStats {
    /// Layers stacked above the base image — one per recreation.
    pub commit_layers: u32,
    /// Bytes those layers account for, or `None` when the base is unknown.
    pub above_base_bytes: Option<i64>,
}

/// Work out how much of a snapshot is stacked commits.
///
/// `Docker::image_history` returns entries **newest first**, and a snapshot's
/// history is its base's history with the commits appended — so the base is the
/// tail, and the commits are the first `len - base_len` entries. Comparing
/// lengths rather than layer digests keeps this a pure function over two
/// vectors of sizes, and is exactly as accurate: the base is by construction a
/// prefix of the snapshot's chain.
///
/// `base_history_len` is `None` when the base image is no longer on the daemon.
/// In that case the count falls back to "layers that carry bytes", which is an
/// honest upper bound on the commits, and the byte split is reported as unknown
/// rather than guessed.
pub fn layer_stats(snapshot_history_sizes: &[i64], base_history_len: Option<usize>) -> LayerStats {
    match base_history_len {
        Some(base_len) if base_len <= snapshot_history_sizes.len() => {
            let commits = &snapshot_history_sizes[..snapshot_history_sizes.len() - base_len];
            LayerStats {
                commit_layers: commits.len() as u32,
                above_base_bytes: Some(commits.iter().sum()),
            }
        }
        // Either no base, or a base longer than the snapshot's own history,
        // which means they are not in the same lineage at all.
        _ => LayerStats {
            commit_layers: snapshot_history_sizes.iter().filter(|s| **s > 0).count() as u32,
            above_base_bytes: None,
        },
    }
}

/// The range a compaction can land in, given the stacked layers it will merge.
///
/// Nothing can measure the real figure in advance: it depends on how much of
/// each layer is superseded by a later one, which is exactly what flattening
/// discovers. But it is bounded, and both bounds are computable:
///
/// * **Floor: zero.** Every byte may still be live, in which case flattening
///   frees nothing — and can even cost a little, since the merged layer
///   recompresses independently. Verified on a synthetic stack with nothing
///   superseded: 29.8 MB → 30.8 MB.
/// * **Ceiling: everything but the largest layer.** The result cannot be
///   smaller than the biggest single layer's worth of content, so at most
///   `sum - max` is superseded.
///
/// Reporting a range is why [`ReclaimItem::bytes_are_exact`] exists. A single
/// invented number here would be the one place in this module that shows a
/// guess as if it were a measurement.
pub fn compaction_bounds(commit_layer_sizes: &[i64]) -> (i64, i64) {
    let sum: i64 = commit_layer_sizes.iter().filter(|s| **s > 0).sum();
    let max = commit_layer_sizes.iter().copied().max().unwrap_or(0).max(0);
    (0, (sum - max).max(0))
}

/// Bytes a `docker builder prune --filter until={hours}h` would reach.
///
/// Records still in use are never reclaimable however old they are, and a
/// record the daemon gave no `last_used_at` is treated as too young to touch —
/// unknown is not permission here either.
pub fn stale_build_cache_bytes(
    entries: &[BuildCacheFacts],
    until_hours: i64,
    now: chrono::DateTime<chrono::Utc>,
) -> i64 {
    let cutoff = now - chrono::Duration::hours(until_hours);
    entries
        .iter()
        .filter(|e| !e.in_use)
        .filter(|e| e.last_used_at.map(|t| t < cutoff).unwrap_or(false))
        .map(|e| e.size.max(0))
        .sum()
}

/// A build-cache record, reduced to what the age filter needs.
#[derive(Debug, Clone, PartialEq)]
pub struct BuildCacheFacts {
    pub size: i64,
    pub in_use: bool,
    pub last_used_at: Option<chrono::DateTime<chrono::Utc>>,
}

impl From<&BuildCache> for BuildCacheFacts {
    fn from(entry: &BuildCache) -> Self {
        BuildCacheFacts {
            size: entry.size.unwrap_or(0),
            in_use: entry.in_use.unwrap_or(false),
            last_used_at: entry.last_used_at.as_ref().and_then(parse_bollard_date),
        }
    }
}

/// bollard's `BollardDate` is a `chrono` type behind a feature flag and a
/// string otherwise; going through its `Display` keeps this working either way
/// without pinning the feature.
fn parse_bollard_date(date: &bollard::models::BollardDate) -> Option<chrono::DateTime<chrono::Utc>> {
    chrono::DateTime::parse_from_rfc3339(&date.to_string())
        .ok()
        .map(|d| d.with_timezone(&chrono::Utc))
}

/// Parse a size the `docker` CLI printed, e.g. `"46.88GB"`, `"0B"`, `"1.5kB"`.
///
/// Docker formats these with `units.HumanSize`, which is **base 1000**, not
/// 1024 — using 1024 here would overstate a 28 GB build cache by ~7%. Returns
/// `None` for anything unrecognised so a CLI output change degrades to "fall
/// back to `df()`" rather than to a wrong number.
pub fn parse_docker_size(raw: &str) -> Option<i64> {
    let raw = raw.trim();
    let split = raw.find(|c: char| c.is_ascii_alphabetic())?;
    let (number, unit) = raw.split_at(split);
    let value: f64 = number.trim().parse().ok()?;
    // Docker never prints a negative size, and a negative `freed_bytes` reaching
    // the UI would subtract from the running total. Fail rather than propagate.
    if !value.is_finite() || value < 0.0 {
        return None;
    }
    let multiplier: f64 = match unit.trim() {
        "B" => 1.0,
        "kB" | "KB" => 1e3,
        "MB" => 1e6,
        "GB" => 1e9,
        "TB" => 1e12,
        "PB" => 1e15,
        _ => return None,
    };
    Some((value * multiplier) as i64)
}

/// Pull `Reclaimable:` and `Total:` out of `docker buildx du`'s trailing
/// summary. Returns `(total, reclaimable)`.
pub fn parse_buildx_du(output: &str) -> Option<(i64, i64)> {
    let mut total = None;
    let mut reclaimable = None;
    for line in output.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("Reclaimable:") {
            reclaimable = parse_docker_size(rest);
        } else if let Some(rest) = line.strip_prefix("Total:") {
            total = parse_docker_size(rest);
        }
    }
    Some((total?, reclaimable.unwrap_or(0)))
}

/// Pull the figure a prune reports out of its output.
///
/// **Two different wordings, and `builder prune` uses the less obvious one.**
/// Verified against Docker 29.7.2: `docker builder prune` ends with a bare
/// `Total:\t20.59MB`, while `docker system prune` and `docker image prune` end
/// with `Total reclaimed space: 20.59MB`. A parser that only knew the second
/// form would silently report every build-cache prune as having freed nothing —
/// which is exactly what the first draft of this did.
///
/// The scan is last-line-first so the summary wins over any record line that
/// happens to contain the word.
pub fn parse_reclaimed_space(output: &str) -> i64 {
    output
        .lines()
        .rev()
        .find_map(|line| {
            let line = line.trim();
            let rest = line
                .strip_prefix("Total reclaimed space:")
                .or_else(|| line.strip_prefix("Total:"))?;
            parse_docker_size(rest)
        })
        .unwrap_or(0)
}

/// The Dockerfile that flattens a snapshot into a single layer.
///
/// ## Why a build and not `docker commit --squash` or export/import
///
/// `commit` cannot squash — squashing is the one thing it does not do, and it
/// is why the stack grows. `--squash` on the classic builder needs an
/// experimental daemon. `docker export | docker import` moves the whole
/// filesystem through this process, and bollard's import takes a fully
/// buffered `Bytes` — a 12 GB image in RAM.
///
/// A two-stage build keeps every byte inside the daemon. `COPY --from` a whole
/// root into `FROM scratch` collapses the chain to one layer, and it preserves
/// uid/gid and setuid bits — **verified against Docker 29.7.2**: a 192.6 MB /
/// 4-layer synthetic came out 45.7 MB / 1 layer with `-rwsr-xr-x root` and
/// `uid 1000` intact, through the plain `POST /build` endpoint bollard uses
/// (no BuildKit session).
///
/// ## Why the scrub runs in the first stage
///
/// The bytes are only free to drop *before* the layer that captures them is
/// written, and here that layer is the flattened one. Running
/// [`container::snapshot_scrub_script`] in the `src` stage costs a throwaway
/// layer on a stage that is discarded, and reuses the one reviewed path list —
/// `SNAPSHOT_SCRUB_PATHS` — rather than forking a second copy of it, which is
/// the failure mode a list like that invites.
///
/// The image's config (env, cmd, entrypoint, labels, workdir) does **not**
/// survive `FROM scratch`; it is replayed afterwards by
/// [`restore_image_config`], which is why this function does not try to emit it
/// as Dockerfile instructions. A multi-line `CLAUDE_INSTRUCTIONS` env var alone
/// makes that escaping a bad bet.
///
/// The one label it *does* emit is `triple-c.managed=true`, and it is not
/// decoration. Everything that cleans up after this build — the discard path
/// when the result is not smaller, the untag after a successful commit — relies
/// on `sweep_orphaned_snapshots` collecting the intermediate, and that sweep
/// filters on `dangling=true` **and** this label. Without it the sweep can
/// never match, and the flattened intermediate is left to whatever `untag_image`
/// happens to delete on its own.
pub fn compaction_dockerfile(snapshot_ref: &str, scrub_script: &str) -> String {
    format!(
        "FROM {snapshot_ref} AS src\n\
         RUN {run}\n\
         FROM scratch\n\
         COPY --from=src / /\n\
         LABEL {LABEL_MANAGED}=true\n",
        run = run_exec_form(scrub_script)
    )
}

/// Render a multi-line `/bin/sh` program as a `RUN` the daemon will actually
/// execute.
///
/// ## What was here before, and why it never worked once
///
/// The scrub script is multi-line shell, and a Dockerfile instruction does not
/// continue over a bare newline — so the script was folded onto one line by
/// joining its lines **with a space**. That is not a rewrite the shell
/// tolerates. `for p in …` on one line and `do` on the next are separated by a
/// newline that *is* load-bearing; joining them produces
/// `… for p in …; do [ -e "$p" ] || continue sz=$(…) …`, and `sh` stops at:
///
/// ```text
/// /bin/sh: line 0: syntax error: unexpected "do"
/// ERROR: process "/bin/sh -c total=0 for p in …" did not complete successfully: exit code: 2
/// ```
///
/// Verified with a real `docker build`, not reasoned about. The build failed on
/// its first stage every single time, so `compact_snapshot` has always returned
/// a failure — the headline action of the whole panel, broken since it landed.
/// Nothing was lost, because the failure is before anything is removed, but
/// nothing was ever reclaimed either. The test that was supposed to catch this
/// asserted only that the `RUN` was *one line*, which the broken fold satisfied
/// perfectly.
///
/// ## Why the JSON exec form rather than a better fold
///
/// Any fold is a rewrite of somebody else's shell, and the script is not this
/// module's to own — `container::snapshot_scrub_script` is free to grow a
/// `case`, an `if`, a heredoc or a function, and each of those breaks a
/// different set of join rules. Inserting `;` between lines is wrong for
/// exactly the same reason a space was: `; do` is fine, but `if x; then; y` is
/// not.
///
/// So the script is not transformed at all. `RUN ["/bin/sh", "-c", "<script>"]`
/// is the exec form, its arguments are a JSON array, and JSON strings carry
/// newlines as `\n` escapes — so the program reaches `sh` byte-for-byte as
/// written, on one Dockerfile line, with no assumption about its shape. The
/// exec form does not go through a shell of its own, which is why `/bin/sh -c`
/// is named explicitly.
///
/// A BuildKit heredoc (`RUN <<EOF`) would also work and is not available here:
/// [`build_from_dockerfile`] goes through bollard's plain `POST /build`, i.e.
/// the classic builder with no BuildKit session, where the heredoc syntax is
/// not parsed.
fn run_exec_form(script: &str) -> String {
    // `serde_json` does the escaping, so a quote, a backslash or a newline in
    // the script cannot break out of the array — the failure mode a
    // hand-rolled escaper would eventually have.
    serde_json::to_string(&["/bin/sh", "-c", script])
        .unwrap_or_else(|_| "[\"/bin/sh\", \"-c\", \"exit 1\"]".to_string())
}

/// Recover the shell program out of a [`run_exec_form`] `RUN` line.
///
/// Exists for the test that runs `sh -n` over it: a Dockerfile is a string, and
/// the only way to assert the thing the daemon will execute is syntactically
/// valid is to pull that exact string back out and hand it to a shell.
///
/// `pub(crate)` rather than `#[cfg(test)]` deliberately. `container.rs` owns the
/// scrub script and wants to assert that a compaction runs *its* script, not a
/// mangled copy — and the honest form of that assertion is
/// `script_from_run_line(run) == snapshot_scrub_script()`, byte for byte, which
/// is a far stronger statement than "the folded one-liner still parses".
#[allow(dead_code)] // used by `disk_tests.rs`, and by `container.rs`'s own test
pub(crate) fn script_from_run_line(run_line: &str) -> Option<String> {
    let json = run_line.strip_prefix("RUN ")?;
    let parts: Vec<String> = serde_json::from_str(json).ok()?;
    parts.get(2).cloned()
}

/// The `rm -rf` program that clears a project's regenerable package caches.
///
/// Every path here is a cache a tool refills on its next run. None of them is
/// user data, none is a bind mount (all live under `$HOME`, i.e. the project's
/// own home volume), and none overlaps `SNAPSHOT_SCRUB_PATHS` — that list
/// covers `/tmp` and `/var` debris in the *writable layer*, this one covers the
/// *volume*, and the two never see the same bytes.
///
/// Measured on one 8.4 GB home volume: ~6 GB. `~/go/pkg/mod` is read-only by
/// default, so it needs `chmod` before `rm` can touch it — `go clean -modcache`
/// is the supported route and is tried first.
///
/// `~/.rustup/toolchains` is behind `include_rustup` rather than in the set: it
/// is just as regenerable, but a re-download is hundreds of megabytes over the
/// network rather than a rebuild from a local cache.
///
/// Playwright is the one entry that is *not* a blanket delete. The current
/// revision is what an installed browser resolves to; deleting it turns a
/// working browser-view project into one that downloads 400 MB on next use. So
/// only the revisions that are not the newest are dropped.
pub fn cache_clear_script(include_rustup: bool) -> String {
    let mut paths: Vec<&str> = vec![
        "$HOME/.npm/_cacache",
        "$HOME/.npm/_npx",
        "$HOME/.cache/go-build",
        "$HOME/.cache/pip",
        "$HOME/.cache/uv",
        "$HOME/.cache/act",
        "$HOME/.cache/chrome-devtools-mcp",
    ];
    if include_rustup {
        paths.push("$HOME/.rustup/toolchains");
    }

    // `du -sb` before each `rm` so the reported total is measured rather than
    // inferred from a df() delta, which a concurrently running agent would
    // corrupt.
    let mut script = String::from("total=0\n");
    for path in &paths {
        script.push_str(&format!(
            "if [ -e \"{path}\" ]; then sz=$(du -sb \"{path}\" 2>/dev/null | cut -f1); \
             case \"$sz\" in ''|*[!0-9]*) sz=0 ;; esac; \
             rm -rf -- \"{path}\" 2>/dev/null && total=$((total + sz)); fi\n"
        ));
    }

    // Go's module cache is written read-only; `go clean -modcache` handles that
    // properly, and the chmod fallback covers a container with no Go on PATH.
    script.push_str(
        "if [ -d \"$HOME/go/pkg/mod\" ]; then \
         sz=$(du -sb \"$HOME/go/pkg/mod\" 2>/dev/null | cut -f1); \
         case \"$sz\" in ''|*[!0-9]*) sz=0 ;; esac; \
         (command -v go >/dev/null 2>&1 && go clean -modcache 2>/dev/null) || \
         (chmod -R u+w \"$HOME/go/pkg/mod\" 2>/dev/null; rm -rf -- \"$HOME/go/pkg/mod\" 2>/dev/null); \
         [ -d \"$HOME/go/pkg/mod\" ] || total=$((total + sz)); fi\n",
    );

    // Playwright: keep the newest chromium revision, drop the rest. `ls -1`
    // sorts lexically, which is right here because the revision suffix is a
    // fixed-width zero-padded number.
    script.push_str(
        "if [ -d \"$HOME/.cache/ms-playwright\" ]; then \
         keep=$(ls -1 \"$HOME/.cache/ms-playwright\" 2>/dev/null | grep '^chromium' | sort | tail -n 1); \
         for d in \"$HOME/.cache/ms-playwright\"/chromium*; do \
         [ -d \"$d\" ] || continue; \
         [ \"$(basename \"$d\")\" = \"$keep\" ] && continue; \
         sz=$(du -sb \"$d\" 2>/dev/null | cut -f1); \
         case \"$sz\" in ''|*[!0-9]*) sz=0 ;; esac; \
         rm -rf -- \"$d\" 2>/dev/null && total=$((total + sz)); done; fi\n",
    );

    script.push_str(&format!("echo \"{CACHE_MARKER}$total\"\nexit 0\n"));
    script
}

/// Marker the cache-clear script prints so the byte total can be read back out
/// of the exec's interleaved output. Same trick as the snapshot scrub's.
const CACHE_MARKER: &str = "###TRIPLE-C-CACHE-CLEARED ";

/// Read the byte total back. `None` means the script never reached its final
/// line, which is how a killed exec is told apart from one that freed nothing.
pub fn parse_cache_total(output: &str) -> Option<u64> {
    output
        .lines()
        .rev()
        .find_map(|line| line.trim().strip_prefix(CACHE_MARKER)?.trim().parse().ok())
}

/// Whether a typed confirmation matches the project it claims to.
///
/// Trimmed, because a trailing space from a paste is not a different intent,
/// but **case-sensitive**: two projects called `Api` and `api` are different
/// projects, and this is the only thing standing between a user and their
/// transcripts.
pub fn confirmation_matches(expected_project_name: &str, typed: &str) -> bool {
    !expected_project_name.is_empty() && typed.trim() == expected_project_name.trim()
}

/// The Windows/WSL2 caveat, in one place so the UI and the logs cannot drift.
///
/// Docker Desktop keeps the whole daemon inside `ext4.vhdx` under
/// `docker-desktop-data`. That file grows to a high-water mark and **never
/// shrinks on its own**. Everything this module reclaims frees space *inside*
/// the vhdx — real, and it is what stops the file growing further — but `C:`
/// does not change until the disk is compacted. Users who are not told this
/// report it as a bug.
pub const WSL2_VHDX_NOTE: &str = concat!(
    "Docker Desktop keeps this daemon inside ext4.vhdx on C:. That file grows to a ",
    "high-water mark and never shrinks by itself, so reclaiming here frees space inside ",
    "the vhdx — which is what stops it growing — but C: will not change until the disk ",
    "is compacted."
);

/// The two ways to actually shrink the vhdx, in the order to try them.
pub const WSL2_VHDX_FIX: &[&str] = &[
    "wsl --shutdown",
    "Optimize-VHD -Path \"$env:LOCALAPPDATA\\Docker\\wsl\\disk\\docker_data.vhdx\" -Mode Full",
];

/// The GUI route, for users without Hyper-V's `Optimize-VHD`.
pub const WSL2_VHDX_FIX_GUI: &str =
    "Docker Desktop → Settings → Resources → Advanced → Clean up / Purge data";

/// Whether the vhdx caveat applies to this host.
///
/// Both halves are required. Docker Desktop on macOS has the same
/// never-shrinks property but a different file and a different fix, and a
/// Windows host talking to a remote or native daemon has neither.
pub fn vhdx_applies(is_windows_host: bool, operating_system: &str) -> bool {
    is_windows_host && is_docker_desktop(operating_system)
}

/// Docker Desktop reports exactly `"Docker Desktop"` in `docker info`'s
/// `OperatingSystem` on every platform it ships for. Matched loosely so a
/// future suffix does not silently flip the answer — the same test
/// `docker/gateway.rs` already makes.
pub fn is_docker_desktop(operating_system: &str) -> bool {
    operating_system.to_ascii_lowercase().contains("docker desktop")
}

/// Whether the project store can be trusted to say which volumes are live.
///
/// ## Why "the list is empty" is not the same as "there are no projects"
///
/// `ProjectsStore::new()` treats an unparseable `projects.json` as recoverable:
/// it copies the file to `.bak`, **starts with an empty list**, and the app runs
/// normally. That is the right call for the app — and it is catastrophic for
/// orphan detection, because in that state every live project's home and config
/// volume looks unclaimed. Deleting them would take the user's credentials,
/// transcripts and toolchains for every project they have.
///
/// So an empty list is only believed when the file is *also* absent, which is
/// the genuine fresh-install case and the one where there is nothing on the
/// daemon to mis-attribute anyway. Anything else returns the reason, and orphan
/// detection is suppressed rather than run optimistically.
pub fn project_store_trust(
    projects: &[Project],
    json_exists: bool,
    json_ids: Option<&[String]>,
) -> Result<HashSet<String>, String> {
    let Some(json_ids) = json_ids else {
        return Err(
            "projects.json could not be read, so there is no way to tell an orphaned volume from a \
             live project's. Nothing is listed here until it can be."
                .to_string(),
        );
    };
    // **The union, not the in-memory list.** `projects` is
    // `state.projects_store.list()`, a snapshot this process loaded at startup.
    // A project added by a *second* copy of the app — the same daemon, the same
    // data directory, a different process — is on disk and not in that list, so
    // its live home and config volumes matched "no project claims this" and
    // were offered for deletion, at `Safety::Safe`, while the very same volumes
    // for a project this instance knows about require typing the project name.
    // Reading the file is the only way to close that; the in-memory list is
    // still unioned in because a project added here a moment ago is authoritative
    // too, and because the ids on disk are read as opaque strings.
    let mut known: HashSet<String> = json_ids.iter().cloned().collect();
    known.extend(projects.iter().map(|p| p.id.clone()));

    if known.is_empty() && json_exists {
        return Err(
            "The project list loaded empty from a projects.json that exists, which is what a \
             recovered-from-corrupt store looks like. Orphan detection is suppressed rather than \
             treat every project's volumes as unclaimed."
                .to_string(),
        );
    }
    Ok(known)
}

/// Re-read `projects.json` from disk: whether it exists, and the project ids it
/// actually holds.
///
/// The in-memory store cannot answer either question. By the time it is
/// consulted a corrupt file has already been swallowed into an empty list, and
/// a second app instance's writes are not in it at all.
///
/// `None` for the ids means "could not be read or parsed", which is not the
/// same as "holds no projects" and must never be collapsed into it. An id that
/// is not a string is skipped rather than failing the whole read — the file is
/// still parseable, so the honest answer is the ids it does carry.
///
/// Blocking `std::fs`, so every async caller goes through
/// [`projects_json_snapshot_async`].
fn projects_json_snapshot() -> (bool, Option<Vec<String>>) {
    let Some(path) = dirs::data_dir().map(|d| d.join("triple-c").join("projects.json")) else {
        return (false, None);
    };
    if !path.exists() {
        return (false, Some(Vec::new()));
    }
    let parsed = std::fs::read_to_string(&path)
        .ok()
        .and_then(|data| serde_json::from_str::<Vec<serde_json::Value>>(&data).ok())
        .map(|entries| {
            entries
                .iter()
                .filter_map(|entry| entry.get("id")?.as_str().map(str::to_string))
                .collect::<Vec<_>>()
        });
    (true, parsed)
}

/// [`projects_json_snapshot`] off the async worker.
///
/// A `read_to_string` on a spinning disk, a network home directory or a
/// Windows volume behind an antivirus filter blocks the whole tokio worker
/// thread it lands on, and this one is called from inside [`scan`] — the
/// longest-running command in the app.
async fn projects_json_snapshot_async() -> (bool, Option<Vec<String>>) {
    tokio::task::spawn_blocking(projects_json_snapshot)
        .await
        .unwrap_or((false, None))
}

// ---------------------------------------------------------------------------
// Scan
// ---------------------------------------------------------------------------

/// Names of the base images a project can be built from, for the globals block.
///
/// Deletion never keys off this list — that stays on `dangling` +
/// `triple-c.managed` + `triple-c.base`, exactly as the sweep does. This is for
/// *display*: a base image pulled before `container/Dockerfile` grew its
/// `LABEL` lines carries neither label, and a globals block that could not name
/// the 4.7 GB image every project sits on would be missing the obvious.
fn is_base_image_reference(reference: &str) -> bool {
    // Split on the *tag*, not the first colon: `localhost:5000/triple-c-sandbox:latest`
    // has a registry port, and splitting on the first colon would yield
    // `localhost`. A tag never contains `/`, which is what tells the two apart.
    let repo = match reference.rsplit_once(':') {
        Some((repo, tag)) if !tag.contains('/') => repo,
        _ => reference,
    };
    repo == "triple-c"
        || repo.ends_with("/triple-c-sandbox")
        || repo == "triple-c-sandbox"
}

/// One `df()`, one `info()`, and one `image_history` per distinct image, joined
/// against the project store.
///
/// Expensive on purpose — see the module docs. Everything the UI needs comes
/// out of this single call so a user never pays for it twice by accident.
pub async fn scan(projects: &[Project]) -> Result<DiskUsageReport, String> {
    let docker = get_docker()?;

    let usage = docker
        .df()
        .await
        .map_err(|e| format!("Could not read Docker disk usage: {}", e))?;
    let images = usage.images.unwrap_or_default();
    let containers = usage.containers.unwrap_or_default();
    let volumes = usage.volumes.unwrap_or_default();
    let build_cache_records = usage.build_cache.unwrap_or_default();

    let info = docker.info().await.ok();
    let operating_system = info
        .as_ref()
        .and_then(|i| i.operating_system.clone())
        .unwrap_or_default();
    let applies = vhdx_applies(cfg!(target_os = "windows"), &operating_system);
    let host = HostStorage {
        docker_root_dir: info
            .as_ref()
            .and_then(|i| i.docker_root_dir.clone())
            .unwrap_or_default(),
        is_docker_desktop: is_docker_desktop(&operating_system),
        is_windows_host: cfg!(target_os = "windows"),
        vhdx_applies: applies,
        vhdx_note: if applies { WSL2_VHDX_NOTE.to_string() } else { String::new() },
        vhdx_fix: if applies {
            WSL2_VHDX_FIX.iter().map(|s| (*s).to_string()).collect()
        } else {
            Vec::new()
        },
        vhdx_fix_gui: if applies { WSL2_VHDX_FIX_GUI.to_string() } else { String::new() },
        operating_system,
    };

    // Index by tag and by name so the per-project join is a lookup rather than a
    // scan per project — 123 images × 8 projects is otherwise 1,000 comparisons
    // for no reason.
    let mut image_by_tag: HashMap<&str, &ImageSummary> = HashMap::new();
    for image in &images {
        for tag in &image.repo_tags {
            image_by_tag.insert(tag.as_str(), image);
        }
    }
    let mut container_by_name: HashMap<String, &ContainerSummary> = HashMap::new();
    for container in &containers {
        for name in container.names.as_deref().unwrap_or(&[]) {
            // Docker prefixes every name with `/`.
            container_by_name.insert(name.trim_start_matches('/').to_string(), container);
        }
    }
    let volume_by_name: HashMap<&str, &Volume> =
        volumes.iter().map(|v| (v.name.as_str(), v)).collect();

    // `image_history` is one round trip per image, so base histories are shared
    // across every project on the same base — which is all of them, normally.
    let mut base_history_len: HashMap<String, Option<usize>> = HashMap::new();

    let mut rows = Vec::new();
    for project in projects {
        let snapshot_image = get_snapshot_image_name(project);
        let snapshot = image_by_tag.get(snapshot_image.as_str()).copied();

        let (snapshot_bytes, snapshot_shared_bytes) = match snapshot {
            Some(image) => (image.size, image.shared_size.max(0)),
            None => (0, 0),
        };

        let mut stats = LayerStats::default();
        let mut base_lineage_known = false;
        if let Some(image) = snapshot {
            let history = docker
                .image_history(&image.id)
                .await
                .map(|entries| entries.into_iter().map(|e| e.size).collect::<Vec<_>>())
                .unwrap_or_default();

            // The lineage label names the base by image id, which survives the
            // base being retagged. `triple-c.create-image` does not work here:
            // on a project that has been recreated it names the project's own
            // snapshot, not the base.
            let base_ref = image
                .labels
                .get(migration::LABEL_BASE_IMAGE_ID)
                .filter(|id| !id.is_empty())
                .cloned();
            let base_len = match base_ref {
                Some(base_ref) => match base_history_len.get(&base_ref) {
                    Some(cached) => *cached,
                    None => {
                        let len = docker
                            .image_history(&base_ref)
                            .await
                            .ok()
                            .map(|entries| entries.len());
                        base_history_len.insert(base_ref, len);
                        len
                    }
                },
                None => None,
            };
            base_lineage_known = base_len.is_some();
            stats = layer_stats(&history, base_len);
        }

        let container = container_by_name.get(&project.container_name()).copied();
        let home = volume_by_name
            .get(home_volume_name(&project.id).as_str())
            .copied();
        let config = volume_by_name
            .get(config_volume_name(&project.id).as_str())
            .copied();

        let home_volume_bytes = home.and_then(volume_bytes).unwrap_or(0);
        let config_volume_bytes = config.and_then(volume_bytes).unwrap_or(0);
        let container_writable_bytes = container.and_then(|c| c.size_rw).unwrap_or(0).max(0);

        // The snapshot's *unique* bytes, not its total: the base is shared with
        // every other project, so counting it per row would show ~4.7 GB of the
        // same image once per project and make the column meaningless.
        let snapshot_unique = (snapshot_bytes - snapshot_shared_bytes).max(0);

        // See `ProjectDiskRow::snapshot_attributed_bytes`. One function, so the
        // column and the total cannot be derived from two different rules
        // again.
        let snapshot_attributed_bytes = snapshot_attribution(
            snapshot_bytes,
            snapshot_shared_bytes,
            stats.above_base_bytes,
        );

        rows.push(ProjectDiskRow {
            project_id: project.id.clone(),
            project_name: project.name.clone(),
            snapshot_image,
            snapshot_exists: snapshot.is_some(),
            snapshot_bytes,
            snapshot_shared_bytes,
            snapshot_commit_layers: stats.commit_layers,
            base_lineage_known,
            // Prefer the daemon's own measurement of what is unique to this
            // image over layer arithmetic; fall back to the layer sum when
            // `df()` did not compute a shared size.
            snapshot_above_base_bytes: if snapshot_shared_bytes > 0 {
                Some(snapshot_unique)
            } else {
                stats.above_base_bytes
            },
            container_exists: container.is_some(),
            container_running: container.and_then(|c| c.state.as_deref()) == Some("running"),
            container_writable_bytes,
            home_volume_bytes,
            home_volume_present: home.is_some(),
            config_volume_bytes,
            config_volume_present: config.is_some(),
            snapshot_attributed_bytes,
            total_bytes: snapshot_attributed_bytes
                + container_writable_bytes
                + home_volume_bytes
                + config_volume_bytes,
            migrating: crate::commands::migration_commands::is_migrating(&project.id),
        });
    }
    rows.sort_by(|a, b| b.total_bytes.cmp(&a.total_bytes));

    // Globals.
    let mut base_images = Vec::new();
    let mut orphan_image_bytes = 0i64;
    let mut orphan_image_count = 0usize;
    for image in &images {
        let managed = image.labels.get(LABEL_MANAGED).map(String::as_str) == Some("true");
        let labelled_base = image.labels.get(LABEL_BASE).map(String::as_str) == Some("true");
        if image.repo_tags.is_empty() || image.repo_tags.iter().all(|t| t == "<none>:<none>") {
            if managed {
                orphan_image_bytes += (image.size - image.shared_size.max(0)).max(0);
                orphan_image_count += 1;
            }
            continue;
        }
        // One row per *image*, not per tag. A base carrying both
        // `triple-c-sandbox:latest` and `ghcr.io/shadowdao/triple-c-sandbox:latest`
        // is one 4.7 GB image, and pushing a row per tag would count it twice
        // in the total below.
        let matching: Vec<&String> = image
            .repo_tags
            .iter()
            .filter(|tag| labelled_base || is_base_image_reference(tag))
            .collect();
        if let Some(primary) = matching.first() {
            base_images.push(BaseImageRow {
                reference: if matching.len() > 1 {
                    // Name the aliases rather than hiding them; a user looking
                    // for "the sandbox image" should find it under whichever
                    // name they know it by.
                    matching
                        .iter()
                        .map(|tag| tag.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                } else {
                    (*primary).clone()
                },
                bytes: image.size,
                shared_bytes: image.shared_size.max(0),
                containers: image.containers,
                is_labelled_base: labelled_base,
            });
        }
    }
    base_images.sort_by(|a, b| b.bytes.cmp(&a.bytes));
    // Full size per base, not `size - shared_size`: a base's shared bytes are
    // shared with *its own snapshots*, so netting them out would report the
    // 4.7 GB image every project sits on as ~0. The residual imprecision is two
    // *different* bases that share lower layers with each other, whose common
    // layers are counted twice here — worth knowing before treating this total
    // as exact.
    let base_images_bytes = base_images.iter().map(|b| b.bytes).sum();

    let (json_exists, json_ids) = projects_json_snapshot_async().await;
    let (orphan_volumes_list, orphan_volumes_unavailable) =
        match project_store_trust(projects, json_exists, json_ids.as_deref()) {
            Ok(known) => {
                let facts: Vec<VolumeFacts> = volumes.iter().map(volume_facts).collect();
                (orphan_volumes(&facts, &known, true), None)
            }
            Err(reason) => (Vec::new(), Some(reason)),
        };
    let orphan_volume_bytes = orphan_volumes_list.iter().map(|v| v.bytes).sum();

    let build_cache = build_cache_usage(&build_cache_records).await;

    // `total_bytes` now excludes each snapshot's shared base wherever the base
    // could be identified, so adding `base_images_bytes` counts each base
    // exactly once rather than once per project on top of once per project.
    // The residue is the case where a snapshot shares nothing measurable *and*
    // its lineage is unknown: its full size is attributed to the project, which
    // is right, and if the base it descends from is also on the daemon it
    // appears as its own row too. Docker would have reported a shared size if
    // those two genuinely shared layers, so that combination means they do not.
    let triple_c_total_bytes = rows.iter().map(|r| r.total_bytes).sum::<i64>()
        + base_images_bytes
        + orphan_image_bytes
        + orphan_volume_bytes;

    Ok(DiskUsageReport {
        scanned_at: chrono::Utc::now().to_rfc3339(),
        projects: rows,
        base_images,
        base_images_bytes,
        orphan_image_bytes,
        orphan_image_count,
        orphan_volumes: orphan_volumes_list,
        orphan_volume_bytes,
        orphan_volumes_unavailable,
        build_cache,
        // **`layers_size`, not the sum of the images.** Every image's `size`
        // includes every layer it inherits, so summing them counts a shared
        // 4.7 GB base once per snapshot — measured at ~33 GB of phantom on a
        // real ten-project daemon, presented under "Everything on this daemon"
        // right beside "Attributable to Triple-C". `df()` already returns the
        // deduplicated figure and nothing read it. The sum is kept only for a
        // daemon that does not report one.
        images_total_bytes: usage
            .layers_size
            .filter(|size| *size > 0)
            .unwrap_or_else(|| images.iter().map(|i| i.size).sum()),
        containers_total_bytes: containers
            .iter()
            .map(|c| c.size_rw.unwrap_or(0).max(0))
            .sum(),
        volumes_total_bytes: volumes.iter().filter_map(volume_bytes).sum(),
        triple_c_total_bytes,
        host,
    })
}

fn volume_bytes(volume: &Volume) -> Option<i64> {
    volume
        .usage_data
        .as_ref()
        .map(|u| u.size)
        .filter(|size| *size >= 0)
}

fn volume_facts(volume: &Volume) -> VolumeFacts {
    VolumeFacts {
        name: volume.name.clone(),
        bytes: volume.usage_data.as_ref().map(|u| u.size).unwrap_or(0),
        // A daemon that did not compute the ref count reports `-1`, and that is
        // deliberately *not* folded into 0 — `orphan_volumes` requires exactly
        // zero, so unknown fails closed.
        links: volume.usage_data.as_ref().map(|u| u.ref_count).unwrap_or(-1),
        created_at: volume.created_at.as_ref().map(|d| d.to_string()),
    }
}

/// Build-cache figures, preferring `docker buildx du` over `df()`.
///
/// `docker system df` counts only records with no parent as reclaimable, which
/// under-reports a real build tree badly; `buildx du` reports it correctly. The
/// CLI is not a hard dependency though — it ships with Docker Desktop and every
/// normal engine install, but a daemon reached over TCP might have no local
/// binary at all, so a failure falls back to the `df()` numbers and says so.
async fn build_cache_usage(records: &[BuildCache]) -> BuildCacheUsage {
    let facts: Vec<BuildCacheFacts> = records.iter().map(BuildCacheFacts::from).collect();
    let df_total: i64 = facts.iter().map(|f| f.size.max(0)).sum();
    let df_reclaimable: i64 = facts
        .iter()
        .filter(|f| !f.in_use)
        .map(|f| f.size.max(0))
        .sum();
    let stale_bytes = stale_build_cache_bytes(
        &facts,
        BUILD_CACHE_DEFAULT_UNTIL_HOURS,
        chrono::Utc::now(),
    );

    match docker_cli(&["buildx", "du"]).await {
        Ok(output) => match parse_buildx_du(&output) {
            Some((total, reclaimable)) => BuildCacheUsage {
                total_bytes: total,
                reclaimable_bytes: reclaimable,
                // The age split only exists in `df()`'s records, so it is kept
                // from there even when the totals come from buildx. It is a
                // lower bound against the buildx total, which is the safe
                // direction for a number attached to a prune button.
                stale_bytes: stale_bytes.min(reclaimable),
                source: "buildx du".to_string(),
                cli_error: None,
            },
            None => BuildCacheUsage {
                total_bytes: df_total,
                reclaimable_bytes: df_reclaimable,
                stale_bytes,
                source: "system df".to_string(),
                cli_error: Some("`docker buildx du` output could not be parsed".to_string()),
            },
        },
        Err(e) => BuildCacheUsage {
            total_bytes: df_total,
            reclaimable_bytes: df_reclaimable,
            stale_bytes,
            source: "system df".to_string(),
            cli_error: Some(e),
        },
    }
}

/// Run the `docker` CLI and return its stdout.
///
/// Used for exactly one thing: the build cache. bollard 0.18 has no wrapper for
/// `POST /build/prune` and no `buildx du` equivalent — the endpoint simply is
/// not in the crate — and adding a second HTTP client to reach a unix socket
/// (or a Windows named pipe) for one endpoint is a worse trade than shelling
/// out, which `commands/aws_commands.rs` already does for the AWS CLI.
async fn docker_cli(args: &[&str]) -> Result<String, String> {
    docker_cli_with_timeout(args, DOCKER_CLI_TIMEOUT_SECS).await
}

async fn docker_cli_with_timeout(args: &[&str], timeout_secs: u64) -> Result<String, String> {
    // **A timeout, because this sits inside `scan`.** `buildx du` talks to the
    // daemon, and a daemon that is wedged — a hung storage driver, a Docker
    // Desktop VM mid-restart — simply never answers. Without a bound the panel
    // shows "Scanning…" for as long as the app is open, with no error and
    // nothing to retry. On expiry the build-cache figures fall back to `df()`'s
    // and say so in `cli_error`, which is the same degradation path a missing
    // CLI already takes.
    //
    // `kill_on_drop` is what makes the timeout real: dropping the future
    // otherwise leaves the child running and still holding whatever it was
    // waiting on.
    let run = tokio::process::Command::new("docker")
        .args(args)
        .kill_on_drop(true)
        .output();
    let output = match tokio::time::timeout(std::time::Duration::from_secs(timeout_secs), run).await
    {
        Ok(result) => result
            .map_err(|e| format!("Could not run `docker {}`: {}", args.join(" "), e))?,
        Err(_) => {
            return Err(format!(
                "`docker {}` did not answer within {} seconds and was cancelled — the daemon may \
                 be busy or wedged",
                args.join(" "),
                timeout_secs
            ))
        }
    };
    if !output.status.success() {
        return Err(format!(
            "`docker {}` failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

// ---------------------------------------------------------------------------
// Planning
// ---------------------------------------------------------------------------

/// Everything that could be reclaimed, with the bytes measured rather than
/// guessed.
///
/// Takes the already-computed [`DiskUsageReport`] so a plan costs no second
/// `df()` — the UI scans once and then plans, re-plans and re-plans again off
/// the same measurement.
pub async fn list_reclaimable(
    projects: &[Project],
    report: &DiskUsageReport,
) -> Result<ReclaimPlan, String> {
    let docker = get_docker()?;
    let mut items = Vec::new();

    // --- Dangling managed images, split by whether they are bases -----------
    //
    // Both come from the same two conditions the startup sweep runs under —
    // dangling *and* `triple-c.managed=true` — and they are mutually exclusive
    // because `triple-c.base` is only ever `true` on an image built from
    // `container/Dockerfile`.
    let dangling = docker
        .list_images(Some(ListImagesOptions {
            all: false,
            filters: HashMap::from([
                ("dangling".to_string(), vec!["true".to_string()]),
                ("label".to_string(), vec![format!("{}=true", LABEL_MANAGED)]),
            ]),
            ..Default::default()
        }))
        .await
        .map_err(|e| format!("Could not list orphaned images: {}", e))?;

    let mut commit_bytes = 0i64;
    let mut commit_count = 0usize;
    let mut base_bytes = 0i64;
    let mut base_count = 0usize;
    for image in &dangling {
        // `list_images` leaves `shared_size` at -1, so the scan's own figure is
        // the measured one; this loop only needs the split by class.
        match classify_dangling(&image.labels) {
            DanglingClass::Base => {
                base_bytes += image.size;
                base_count += 1;
            }
            DanglingClass::SnapshotCommit => {
                commit_bytes += image.size;
                commit_count += 1;
            }
        }
    }

    if commit_count > 0 {
        items.push({
            // Safety and reach are read off the target, never restated: a literal
            // here that disagreed with the classifier is exactly the drift this
            // module cannot afford.
            let target = ReclaimTarget::DanglingSnapshots;
            ReclaimItem {
                safety: target.safety(),
                daemon_wide: target.is_daemon_wide(),
                target,
            label: format!("Superseded snapshot layers ({} images)", commit_count),
            detail: "Untagged images left behind by past container recreations. Nothing can \
                     start from them and no project refers to them."
                .to_string(),
            bytes: commit_bytes,
            bytes_are_exact: true,
            bytes_floor: None,
            blocked: None,
        }
        });
    }
    if base_count > 0 {
        items.push({
            // Safety and reach are read off the target, never restated: a literal
            // here that disagreed with the classifier is exactly the drift this
            // module cannot afford.
            let target = ReclaimTarget::SupersededBaseImages;
            ReclaimItem {
                safety: target.safety(),
                daemon_wide: target.is_daemon_wide(),
                target,
            label: format!("Superseded base images ({} images)", base_count),
            detail: "Older Triple-C sandbox images that a newer build or pull replaced. Docker \
                     refuses to remove one that a stopped project still needs, so a project \
                     that has not been migrated keeps its own."
                .to_string(),
            bytes: base_bytes,
            bytes_are_exact: true,
            bytes_floor: None,
            blocked: None,
        }
        });
    }

    // --- Build cache --------------------------------------------------------
    if report.build_cache.total_bytes > 0 {
        if report.build_cache.stale_bytes > 0 {
            items.push({
                // Safety and reach are read off the target, never restated: a literal
                // here that disagreed with the classifier is exactly the drift this
                // module cannot afford.
                let target = ReclaimTarget::BuildCache { all: false };
                ReclaimItem {
                    safety: target.safety(),
                    daemon_wide: target.is_daemon_wide(),
                    target,
                label: "Build cache older than 7 days".to_string(),
                detail: "Docker's BuildKit cache, for the WHOLE daemon — not just Triple-C. \
                         Anything else you build here loses its warm cache too and rebuilds \
                         from scratch once."
                    .to_string(),
                bytes: report.build_cache.stale_bytes,
                bytes_are_exact: true,
                bytes_floor: None,
                blocked: None,
            }
            });
        }
        items.push({
            // Safety and reach are read off the target, never restated: a literal
            // here that disagreed with the classifier is exactly the drift this
            // module cannot afford.
            let target = ReclaimTarget::BuildCache { all: true };
            ReclaimItem {
                safety: target.safety(),
                daemon_wide: target.is_daemon_wide(),
                target,
            label: "Build cache, all of it".to_string(),
            detail: "Every reclaimable BuildKit record on the daemon, at any age. Same \
                     daemon-wide caveat, with nothing held back."
                .to_string(),
            bytes: report.build_cache.reclaimable_bytes,
            bytes_are_exact: true,
            bytes_floor: None,
            blocked: None,
        }
        });
    }

    // --- Migration leftovers ------------------------------------------------
    let (pin_bytes, pin_count, live_pins) = survey_rollback_pins(projects).await;
    if pin_count > 0 {
        items.push({
            // Safety and reach are read off the target, never restated: a literal
            // here that disagreed with the classifier is exactly the drift this
            // module cannot afford.
            let target = ReclaimTarget::MigrationPins;
            ReclaimItem {
                safety: target.safety(),
                daemon_wide: target.is_daemon_wide(),
                target,
            label: format!("Ownerless rollback pins ({} images)", pin_count),
            detail: format!(
                "`pre-migration-*` tags whose migration record has been gone for more than {} \
                 days, so nothing can roll back to them. A pin that lost its record more \
                 recently is not here — it is listed under this project's own removals, where \
                 deleting it takes a typed confirmation. Untagged here; the image is then \
                 collected as a superseded snapshot layer under the same rules.",
                migration::STALE_PIN_MAX_AGE_DAYS
            ),
            bytes: pin_bytes,
            bytes_are_exact: true,
            bytes_floor: None,
            blocked: None,
        }
        });
    }

    let staging_bytes = {
        // Same reason as the executor below: `read_dir` + `metadata` per entry
        // is blocking IO, and this runs inside the panel's plan step.
        let owned: Vec<Project> = projects.to_vec();
        tokio::task::spawn_blocking(move || survey_migration_staging(&owned))
            .await
            .unwrap_or(0)
    };
    if staging_bytes > 0 {
        items.push({
            // Safety and reach are read off the target, never restated: a literal
            // here that disagreed with the classifier is exactly the drift this
            // module cannot afford.
            let target = ReclaimTarget::MigrationStaging;
            ReclaimItem {
                safety: target.safety(),
                daemon_wide: target.is_daemon_wide(),
                target,
            label: "Migration staging files".to_string(),
            detail: "Half-finished `*-payload.tar` files with no migration record beside them. \
                     These are ordinary files in your data directory, not Docker storage — on \
                     Windows they are on C: itself, so this is the one item here that gives \
                     space back to C: immediately."
                .to_string(),
            bytes: staging_bytes,
            bytes_are_exact: true,
            bytes_floor: None,
            blocked: None,
        }
        });
    }

    // --- Leftover throwaway containers --------------------------------------
    let probes = survey_containers_by_filter(
        HashMap::from([(
            "label".to_string(),
            vec![format!(
                "{}={}",
                migration::LABEL_PROBE,
                migration::PROBE_LABEL_MIGRATION
            )],
        )]),
        is_reapable_migration_probe,
    )
    .await;
    if probes.1 > 0 {
        items.push({
            // Safety and reach are read off the target, never restated: a literal
            // here that disagreed with the classifier is exactly the drift this
            // module cannot afford.
            let target = ReclaimTarget::ProbeContainers;
            ReclaimItem {
                safety: target.safety(),
                daemon_wide: target.is_daemon_wide(),
                target,
            label: format!("Migration probe containers ({})", probes.1),
            detail: "Throwaway containers a migration used to read a filesystem manifest and \
                     did not get to remove. Only ones that have been sitting there for a while \
                     are listed — a probe that is still running looks exactly the same."
                .to_string(),
            bytes: probes.0,
            bytes_are_exact: true,
            bytes_floor: None,
            blocked: None,
        }
        });
    }

    let scrubs = survey_containers_by_filter(
        HashMap::from([("name".to_string(), vec!["triple-c-scrub-".to_string()])]),
        is_reapable_scrub_container,
    )
    .await;
    if scrubs.1 > 0 {
        items.push({
            // Safety and reach are read off the target, never restated: a literal
            // here that disagreed with the classifier is exactly the drift this
            // module cannot afford.
            let target = ReclaimTarget::ScrubContainers;
            ReclaimItem {
                safety: target.safety(),
                daemon_wide: target.is_daemon_wide(),
                target,
            label: format!("Secret-scrub scratch containers ({})", scrubs.1),
            detail: "`triple-c-scrub-*` containers left by an interrupted rewrite of a snapshot's \
                     baked-in environment. Only ones that have been sitting there for a while \
                     are listed — the same name is used by a rewrite that is still running."
                .to_string(),
            bytes: scrubs.0,
            bytes_are_exact: true,
            bytes_floor: None,
            blocked: None,
        }
        });
    }

    // --- Per-project semi-safe work -----------------------------------------
    for row in &report.projects {
        // The **live** lock first, then the scan's own snapshot as a fallback.
        // `row.migrating` is as old as the scan that produced it — seconds or
        // minutes — and it only ever knew about migrations. The lock knows
        // about a compaction, a Reset and a start too, and it knows now.
        let blocked_by_migration = crate::project_lock::held(&row.project_id)
            .map(|holder| format!("{}.", holder.describe()))
            .or_else(|| {
                row.migrating
                    .then(|| "A base-image migration is in flight for this project.".to_string())
            });

        // A ceiling of zero means flattening this snapshot cannot come out
        // ahead — almost always because its unique delta is smaller than the
        // base it would have to re-duplicate. Offering it anyway would be
        // offering a loss, so it is simply not in the list.
        let ceiling = row
            .snapshot_above_base_bytes
            .map(|unique| {
                compaction_ceiling_for(
                    unique,
                    row.snapshot_shared_bytes,
                    row.snapshot_commit_layers,
                )
            })
            .unwrap_or(0);
        if row.snapshot_exists
            && row.base_lineage_known
            && row.snapshot_commit_layers > 1
            && ceiling > 0
        {
            items.push({
                // Safety and reach are read off the target, never restated: a literal
                // here that disagreed with the classifier is exactly the drift this
                // module cannot afford.
                let target = ReclaimTarget::CompactSnapshot {
                    project_id: row.project_id.clone(),
                };
                ReclaimItem {
                    safety: target.safety(),
                    daemon_wide: target.is_daemon_wide(),
                    target,
                label: format!("Compact {}'s snapshot", row.project_name),
                detail: format!(
                    "{} stacked commit layers are rewritten into one, dropping every byte a \
                     later layer already superseded. Nothing installed is lost. The container \
                     must be stopped, and the rewrite needs room for a second copy while it \
                     runs. Note that the result no longer shares the base image with your other \
                     projects — that cost is already subtracted from the figure here, and the \
                     rewrite is abandoned if it turns out not to come out ahead.",
                    row.snapshot_commit_layers
                ),
                bytes: ceiling,
                bytes_are_exact: false,
                bytes_floor: Some(0),
                blocked: blocked_by_migration.clone().or_else(|| {
                    row.container_running.then(|| {
                        "Stop this project's container first — compaction rewrites the image it \
                         is running from."
                            .to_string()
                    })
                }),
            }
            });
        }

        if row.container_exists {
            for include_rustup in [false, true] {
                items.push({
                    // Safety and reach are read off the target, never restated: a literal
                    // here that disagreed with the classifier is exactly the drift this
                    // module cannot afford.
                    let target = ReclaimTarget::ClearCaches {
                        project_id: row.project_id.clone(),
                        include_rustup,
                    };
                    ReclaimItem {
                        safety: target.safety(),
                        daemon_wide: target.is_daemon_wide(),
                        target,
                    label: if include_rustup {
                        format!("Clear {}'s caches, Rust toolchains included", row.project_name)
                    } else {
                        format!("Clear {}'s package caches", row.project_name)
                    },
                    detail: if include_rustup {
                        "Everything below, plus `~/.rustup/toolchains`. Also regenerable, but \
                         re-downloading a toolchain is hundreds of megabytes over the network \
                         rather than a rebuild from a local cache."
                            .to_string()
                    } else {
                        "npm, npx, pip, uv, Go build and module caches, act, chrome-devtools-mcp, \
                         and the Playwright browser revisions that are not the newest. All \
                         refilled by the next command that needs them."
                            .to_string()
                    },
                    // Measured by the script itself, inside the container,
                    // after the fact — a `df()` delta would be corrupted by
                    // whatever the agent is doing at the same moment.
                    bytes: 0,
                    bytes_are_exact: false,
                    bytes_floor: Some(0),
                    blocked: blocked_by_migration.clone().or_else(|| {
                        (!row.container_running).then(|| {
                            "Start this project's container first — the caches live in its home \
                             volume and are cleared from inside."
                                .to_string()
                        })
                    }),
                }
                });
            }
        }
    }

    // --- Destructive, for display only --------------------------------------
    let mut destructive = Vec::new();

    // Orphaned volumes, one item each and never aggregated. Each of these was
    // some project's home or `.claude` directory, and the user is the only one
    // who can say whether the project it belonged to is really gone — which is
    // why this is here rather than in `items`. See
    // `DestructiveTarget::OrphanVolume`.
    for volume in &report.orphan_volumes {
        destructive.push(DestructiveItem {
            target: DestructiveTarget::OrphanVolume {
                name: volume.name.clone(),
                project_id: volume.project_id.clone(),
            },
            project_id: volume.project_id.clone(),
            // No project in the store answers to this id — the volume's own
            // name is the only handle there is, and it is what has to be typed.
            project_name: volume.name.clone(),
            label: format!("{} ({} volume)", volume.name, volume.role),
            loses: format!(
                "Named for project id {}, which is not in Triple-C's project list, and no \
                 container is attached to it.{} {} Not recoverable. Type the volume name to \
                 confirm.",
                volume.project_id,
                match &volume.created_at {
                    Some(created) => format!(" Docker created it on {}.", created),
                    None => String::new(),
                },
                if volume.role == "config" {
                    "This is a `.claude` volume — it held that project's Claude credential, \
                     plugins and session transcripts."
                } else {
                    "This is a home volume — it held that project's dotfiles, shell history and \
                     installed toolchains."
                }
            ),
            bytes: volume.bytes,
            blocked: None,
        });
    }
    for row in &report.projects {
        let blocked = crate::project_lock::held(&row.project_id)
            .map(|holder| format!("{}.", holder.describe()))
            .or_else(|| {
                row.migrating
                    .then(|| "A base-image migration is in flight for this project.".to_string())
            });
        if row.home_volume_present {
            destructive.push(DestructiveItem {
                target: DestructiveTarget::HomeVolume {
                    project_id: row.project_id.clone(),
                },
                project_id: row.project_id.clone(),
                project_name: row.project_name.clone(),
                label: "Home volume".to_string(),
                loses: "Shell history, dotfiles, every toolchain installed under $HOME, and any \
                        Playwright browsers. Not recoverable. The project's container is removed \
                        too, because a stopped container still holds its volumes open — it is \
                        rebuilt from the snapshot on the next start."
                    .to_string(),
                bytes: row.home_volume_bytes,
                blocked: blocked.clone(),
            });
        }
        if row.config_volume_present {
            destructive.push(DestructiveItem {
                target: DestructiveTarget::ConfigVolume {
                    project_id: row.project_id.clone(),
                },
                project_id: row.project_id.clone(),
                project_name: row.project_name.clone(),
                label: "Claude config volume".to_string(),
                loses: "The Claude login credential, installed plugins and skills, and EVERY \
                        conversation transcript for this project. Not recoverable. The project's \
                        container is removed too, because a stopped container still holds its \
                        volumes open — it is rebuilt from the snapshot on the next start."
                    .to_string(),
                bytes: row.config_volume_bytes,
                blocked: blocked.clone(),
            });
        }
        if row.snapshot_exists {
            destructive.push(DestructiveItem {
                target: DestructiveTarget::SnapshotImage {
                    project_id: row.project_id.clone(),
                },
                project_id: row.project_id.clone(),
                project_name: row.project_name.clone(),
                label: "Snapshot image".to_string(),
                loses: "Every package the agent ever installed in this project's system layer. \
                        The project falls back to the base image next time it starts, and \
                        rebuilds from there. Volumes are untouched."
                    .to_string(),
                // The same figure the Snapshot column shows, so the
                // confirmation and the table cannot quote different numbers for
                // the same image.
                bytes: row.snapshot_attributed_bytes,
                blocked: blocked.clone(),
            });
        }
    }
    for pin in live_pins {
        destructive.push(DestructiveItem {
            target: DestructiveTarget::RollbackPin {
                project_id: pin.project_id.clone(),
                tag: pin.tag.clone(),
            },
            project_id: pin.project_id,
            project_name: pin.project_name,
            label: format!("Rollback pin {}", pin.tag),
            loses: pin.loses,
            bytes: pin.bytes,
            blocked: None,
        });
    }
    destructive.sort_by(|a, b| b.bytes.cmp(&a.bytes));

    items.sort_by(|a, b| {
        // Safe first (that is the one-button group), then by size. Compaction
        // ends up beneath the safe list even when it is the biggest number,
        // which is the right reading order: try the free wins first.
        (a.safety == Safety::SemiSafe)
            .cmp(&(b.safety == Safety::SemiSafe))
            .then_with(|| b.bytes.cmp(&a.bytes))
    });

    Ok(ReclaimPlan {
        items,
        destructive,
        store_error: report.orphan_volumes_unavailable.clone(),
    })
}

/// Upper bound on a compaction's yield.
///
/// ## Flattening breaks base-layer sharing, and that is the dominant term
///
/// `FROM scratch` + `COPY --from` produces an image that shares **nothing**.
/// The base image stays on disk, because every other project is still built
/// from it — so the flattened snapshot now carries its own private copy of all
/// ~4.7 GB of it. That cost is paid whatever the layers held.
///
/// Measured on a real daemon: eight of ten projects had a unique delta between
/// 0.10 GB and 1.32 GB over a 4.72 GB shared base. Flattening any of those
/// turns a 0.63 GB cost into a ~4.7 GB one — **a net loss of about 4 GB**, on
/// an action sold as reclaiming space. Only the project carrying 8.44 GB across
/// 14 layers was plausibly a win.
///
/// So the yield is bounded by two independent things and it is the smaller that
/// binds:
///
/// * **Superseded bytes.** At most everything but the largest layer, which
///   without per-layer sizes to hand is approximated by the even split
///   `unique * (n-1)/n`. See [`compaction_bounds`].
/// * **What is left after re-duplicating the base**: `unique - shared`. A
///   project whose unique delta is smaller than its shared base can never come
///   out ahead, and this returns zero for it — which keeps it out of the plan
///   entirely rather than offering a loss as a saving.
fn compaction_ceiling_for(unique_bytes: i64, shared_bytes: i64, commit_layers: u32) -> i64 {
    if commit_layers < 2 {
        return 0;
    }
    let n = commit_layers as i64;
    let unique = unique_bytes.max(0);
    let superseded_ceiling = (unique / n) * (n - 1);
    // What survives re-duplicating the base. Negative means the flattened image
    // would be bigger than what it replaces.
    let after_base_penalty = unique - shared_bytes.max(0);
    superseded_ceiling.min(after_base_penalty).max(0)
}

/// What this module may do with one `pre-migration-*` tag.
///
/// The single rule behind both the survey and the reclaim, so the count on the
/// button and the work the button does cannot disagree — and so the guards can
/// be tested without a daemon. Before this existed the two paths applied
/// *different* rules and the reclaim's was the weaker one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PinDisposition {
    /// Not a tag `migration::rollback_tag` produced. Never touched, never
    /// offered, never given a tombstone — a hand-made `pre-migration-keepme` is
    /// somebody's deliberate pin.
    NotOurs,
    /// Nothing claims it and its grace period has expired.
    Reapable,
    /// A migration record still claims it. The only copy of a rollback target
    /// that is still awaiting a decision.
    Claimed,
    /// Nothing claims it, but the grace period that starts when a pin loses its
    /// record has not run out — including the case where no reaper has recorded
    /// a sighting yet, which is where the clock starts rather than where it
    /// ends.
    WithinGrace,
}

/// Classify one rollback pin. Pure; see [`PinDisposition`].
pub fn pin_disposition(
    tag: &str,
    has_record: bool,
    ownerless_since: Option<chrono::DateTime<chrono::Utc>>,
    now: &chrono::DateTime<chrono::Utc>,
) -> PinDisposition {
    if migration::parse_rollback_tag(tag).is_none() {
        return PinDisposition::NotOurs;
    }
    if has_record {
        return PinDisposition::Claimed;
    }
    if migration::pin_is_reapable(tag, has_record, ownerless_since, now) {
        PinDisposition::Reapable
    } else {
        PinDisposition::WithinGrace
    }
}

/// One rollback pin this panel will not collect on its own, with the reason.
struct RetainedPin {
    project_id: String,
    project_name: String,
    tag: String,
    /// The image's own size, charged **once per image** — see
    /// [`survey_rollback_pins`].
    bytes: i64,
    /// The sentence the destructive confirmation shows.
    loses: String,
}

/// Rollback pins, split into the ones the reaper may drop and the ones it may
/// not.
///
/// ## Two things this used to get wrong
///
/// * **It applied no guard but `has_record`.** `reap_stale_migration_pins`
///   requires `parse_rollback_tag` *and* `pin_is_reapable`, and `destroy`'s
///   `RollbackPin` arm requires `parse_rollback_tag` with a comment explaining
///   why. This offered anything whose repo matched `triple-c-snapshot-*` and
///   whose tag merely began `pre-migration-` — a hand-made pin included — with
///   no age gate at all, under `Safety::Safe`, i.e. one button and no
///   confirmation.
/// * **It double-counted.** The loop is over `repo_tags`, so an image answering
///   to two `pre-migration-*` tags added its full size twice.
///
/// The survey only ever *peeks* at the ownerless clock. Starting one is a
/// reaper's job — [`reclaim_migration_pins`] and
/// `migration::reap_stale_migration_pins` — because a function whose job is to
/// describe the world should not be the thing that makes a pin collectable
/// fourteen days later.
async fn survey_rollback_pins(projects: &[Project]) -> (i64, usize, Vec<RetainedPin>) {
    let Ok(docker) = get_docker() else {
        return (0, 0, Vec::new());
    };
    let names: HashMap<&str, &str> = projects
        .iter()
        .map(|p| (p.id.as_str(), p.name.as_str()))
        .collect();

    let images = docker
        .list_images(Some(ListImagesOptions {
            all: false,
            filters: HashMap::from([(
                "reference".to_string(),
                vec!["triple-c-snapshot-*:pre-migration-*".to_string()],
            )]),
            ..Default::default()
        }))
        .await
        .unwrap_or_default();

    let now = chrono::Utc::now();
    let mut reapable_bytes = 0;
    let mut reapable_count = 0;
    let mut retained = Vec::new();
    for image in images {
        // Per *image*, not per tag: two `pre-migration-*` tags on one image are
        // two names for the same layers, and removing both frees them once.
        let mut image_is_reapable = false;
        // Set once the image's bytes have been charged to one of its retained
        // tags, so a second tag on the same image is listed but not counted.
        let mut image_bytes_charged = false;

        for reference in &image.repo_tags {
            let Some((project_id, tag)) = migration::parse_snapshot_reference(reference) else {
                continue;
            };
            // Filesystem presence, not `load`: a record that cannot be parsed
            // must still count as "somebody may want this back". Same rule the
            // pin reaper uses, for the same reason.
            let has_record = migration_store::has_record(&project_id).unwrap_or(true);
            let ownerless_since = migration_store::peek_ownerless_since(&project_id, &tag);
            let disposition = pin_disposition(&tag, has_record, ownerless_since, &now);
            match disposition {
                // Never offered anywhere, at any safety level — the same
                // refusal `destroy` makes.
                PinDisposition::NotOurs => continue,
                PinDisposition::Reapable => {
                    image_is_reapable = true;
                    continue;
                }
                PinDisposition::Claimed | PinDisposition::WithinGrace => {}
            }
            let name = names
                .get(project_id.as_str())
                .map(|n| (*n).to_string())
                .unwrap_or_else(|| project_id.clone());
            let loses = if disposition == PinDisposition::Claimed {
                "The only copy of this project's pre-migration system layer. Its migration is \
                 still waiting to be confirmed or rolled back — delete this and rolling back \
                 becomes impossible."
                    .to_string()
            } else {
                format!(
                    "The only copy of this project's pre-migration system layer. Nothing claims \
                     it any more, but it is still inside the {}-day grace period that starts \
                     when a pin loses its migration record — it is collected automatically after \
                     that. Delete it here only if you are sure you will never roll that \
                     migration back.",
                    migration::STALE_PIN_MAX_AGE_DAYS
                )
            };
            retained.push(RetainedPin {
                project_id,
                project_name: name,
                tag,
                bytes: if image_bytes_charged { 0 } else { image.size },
                loses,
            });
            image_bytes_charged = true;
        }

        if image_is_reapable {
            reapable_bytes += image.size;
            reapable_count += 1;
        }
    }
    (reapable_bytes, reapable_count, retained)
}

/// Total bytes of `*-payload.tar` staging files no migration record claims.
fn survey_migration_staging(projects: &[Project]) -> i64 {
    let Ok(dir) = migration_store::migrations_dir() else {
        return 0;
    };
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return 0;
    };
    let mut total = 0i64;
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        let Some(project_id) = name.strip_suffix("-payload.tar") else {
            continue;
        };
        // A record beside it means a migration may still be mid-flight and this
        // tar is its input. Only ownerless ones are offered.
        if migration_store::has_record(project_id).unwrap_or(true) {
            continue;
        }
        // A project the store still knows about, currently migrating, is
        // covered by the record check above; this is belt and braces for the
        // window where the record has been cleared but the run has not
        // unwound.
        if projects
            .iter()
            .any(|p| p.id == project_id && crate::commands::migration_commands::is_migrating(&p.id))
        {
            continue;
        }
        if let Ok(meta) = entry.metadata() {
            total += meta.len() as i64;
        }
    }
    total
}

/// A migration probe: a throwaway container carrying
/// `triple-c.probe=migration`.
///
/// Re-checked on the summary rather than trusted from the daemon's filter.
/// Docker's `label=k=v` filter is an exact match and would be enough — but
/// "enough" is not the standard for a function that removes containers, and a
/// filter is a string built somewhere else in the file.
fn is_migration_probe(summary: &ContainerSummary) -> bool {
    summary
        .labels
        .as_ref()
        .and_then(|labels| labels.get(migration::LABEL_PROBE))
        .map(String::as_str)
        == Some(migration::PROBE_LABEL_MIGRATION)
}

/// A migration probe this bucket is allowed to force-remove.
///
/// The label is daemon-wide, and this target's `project_id()` is `None` — so
/// `reclaim`'s per-project guard never fires for it and a tick here reaches
/// *every* probe on the daemon, including a live one belonging to another
/// instance's migration or to a migration this process started a second ago.
/// The same age gate `migration::reap_probe_containers` applies is the only
/// discriminator there is; a probe is a short `df`, `find` or `apt-get update`,
/// so nothing legitimate is older than it.
fn is_reapable_migration_probe(summary: &ContainerSummary) -> bool {
    if !is_migration_probe(summary) {
        return false;
    }
    match summary.created {
        Some(created) => {
            chrono::Utc::now().timestamp() - created >= migration::PROBE_REAP_MIN_AGE_SECS
        }
        // Unknown is never permission — the same rule the volume ref count and
        // the scratch containers follow.
        None => false,
    }
}

/// A scratch container from an **interrupted** secret rewrite.
///
/// **Docker's `name` filter is a substring match**, so it also returns a user's
/// own `my-triple-c-scrub-notes`. The full name is what decides.
///
/// ## The age gate is not tidiness, it is the point of the whole function
///
/// `triple-c-scrub-*` is not a dead name. It is the **live** name
/// `container::rewrite_image_without_secrets` creates its scratch container
/// under, and that is still reachable today from `clear_claude_token`. Between
/// the `create` and the `commit` there is a window in which this bucket —
/// `Safety::Safe`, one button, no confirmation, and `force: true` on the
/// removal — destroys the container the commit is about to run against. The
/// rewrite then fails, the superseded image is *not* removed, and a revoked
/// OAuth token stays baked into that snapshot's `Config.Env`: precisely the
/// condition `scrub_secrets_from_snapshots` exists to remove, produced by the
/// panel that offers to clean up after it.
///
/// The right discriminator would be a label — the scrub containers carry none,
/// and this module cannot add one without editing `container.rs`. Age is the
/// discriminator available from here. A scrub container is created, committed
/// and removed within one function with no I/O between the steps but a
/// `docker commit`; anything older than [`SCRATCH_CONTAINER_MIN_AGE_SECS`] is
/// therefore a leftover, and anything younger is assumed to belong to a live
/// rewrite — this process's or another instance's.
fn is_scrub_container(summary: &ContainerSummary) -> bool {
    summary
        .names
        .as_deref()
        .unwrap_or(&[])
        .iter()
        .any(|name| name.trim_start_matches('/').starts_with("triple-c-scrub-"))
}

/// A scrub container this module is allowed to force-remove: ours, *not*
/// currently claimed, and old enough not to be a live rewrite. This is the
/// predicate the survey and the reclaim both use; [`is_scrub_container`]
/// answers only "is this name ours".
///
/// The label is the real discriminator and the age is the backstop. A live
/// rewrite now carries `triple-c.scrub=true` **and** holds its project's
/// `ProjectOp::SecretScrub` claim, so within this process the answer is exact.
/// Age still matters because the claim is process-local: a second app instance
/// mid-rewrite is invisible here, and killing that container between its create
/// and its commit leaves the revoked credential baked into the snapshot's
/// `Config.Env` — the state `scrub_secrets_from_snapshots` exists to end.
fn is_reapable_scrub_container(summary: &ContainerSummary) -> bool {
    if !is_scrub_container(summary) {
        return false;
    }
    if scrub_container_project(summary)
        .is_some_and(|id| crate::project_lock::is_held_by(&id, crate::project_lock::ProjectOp::SecretScrub))
    {
        return false;
    }
    is_stale_scratch(summary)
}

/// The project a live scrub container is rewriting, read off the image it was
/// created from. `None` when the image is not a `triple-c-snapshot-*`
/// reference, which is the case for a leftover whose image has since gone.
fn scrub_container_project(summary: &ContainerSummary) -> Option<String> {
    let image = summary.image.as_deref()?;
    crate::docker::migration::parse_snapshot_reference(image).map(|(id, _tag)| id)
}

/// How old a `triple-c-scrub-*` / `triple-c-compact-*` scratch container must be
/// before anything here will force-remove it.
///
/// Both names are used by live code, neither carries a label to tell a leftover
/// from a live one, and the removals are unconditional `force: true`. See
/// [`is_scrub_container`]. Fifteen minutes is far longer than either scratch
/// container's real lifetime (a `create`, a `commit`, a `rm`) and far shorter
/// than "until the user notices".
pub const SCRATCH_CONTAINER_MIN_AGE_SECS: i64 = 15 * 60;

/// Whether a scratch container is old enough to be a leftover rather than a
/// live one. A summary with no creation time is treated as young: unknown is
/// never permission, the same rule the volume ref count follows.
fn is_stale_scratch(summary: &ContainerSummary) -> bool {
    match summary.created {
        Some(created) => chrono::Utc::now().timestamp() - created >= SCRATCH_CONTAINER_MIN_AGE_SECS,
        None => false,
    }
}

/// `(writable_bytes, count)` for containers matching a filter *and* a predicate
/// re-checked on each summary. See [`is_migration_probe`] and
/// [`is_scrub_container`] for why the second check is not redundant.
async fn survey_containers_by_filter(
    filters: HashMap<String, Vec<String>>,
    predicate: fn(&ContainerSummary) -> bool,
) -> (i64, usize) {
    let Ok(docker) = get_docker() else {
        return (0, 0);
    };
    let containers = docker
        .list_containers(Some(ListContainersOptions {
            all: true,
            size: true,
            filters,
            ..Default::default()
        }))
        .await
        .unwrap_or_default();
    let mut bytes = 0;
    let mut count = 0;
    for container in containers {
        if !predicate(&container) {
            continue;
        }
        bytes += container.size_rw.unwrap_or(0).max(0);
        count += 1;
    }
    (bytes, count)
}

// ---------------------------------------------------------------------------
// Execution
// ---------------------------------------------------------------------------

/// Run the ticked targets, in order, and report what each one actually freed.
///
/// One failing target never stops the others: these are independent housekeeping
/// jobs, and a build cache the daemon refuses to prune is no reason to leave a
/// dangling image behind. Every failure becomes a `ReclaimResult { ok: false }`
/// the UI shows beside the item.
pub async fn reclaim(targets: &[ReclaimTarget], projects: &[Project]) -> ReclaimOutcome {
    let mut outcome = ReclaimOutcome::default();
    for target in targets {
        // Anything that stops, removes or rewrites a project's container or
        // image consults the project's lock first — the same rule the rest of
        // the app obeys. Checked once here so a new project-scoped target
        // cannot be added without it, and so a busy project is reported per
        // item rather than failing the whole batch; the executors *acquire*
        // for their own callers, which is the check that actually holds.
        if let Some(project_id) = target.project_id() {
            if let Some(holder) = crate::project_lock::held(project_id) {
                let result = failed(
                    target.clone(),
                    format!("{}. Nothing was done for it.", holder.describe()),
                );
                outcome.results.push(result);
                continue;
            }
        }
        let result = match target {
            ReclaimTarget::DanglingSnapshots | ReclaimTarget::SupersededBaseImages => {
                reclaim_dangling(target).await
            }
            ReclaimTarget::BuildCache { all } => reclaim_build_cache(*all).await,
            ReclaimTarget::MigrationPins => reclaim_migration_pins().await,
            ReclaimTarget::MigrationStaging => {
                // `std::fs` over a directory of multi-gigabyte tars, from an
                // async worker. `spawn_blocking` or the whole runtime thread
                // stalls behind it.
                let owned: Vec<Project> = projects.to_vec();
                tokio::task::spawn_blocking(move || reclaim_migration_staging(&owned))
                    .await
                    .unwrap_or_else(|e| {
                        failed(
                            ReclaimTarget::MigrationStaging,
                            format!("The staging cleanup task did not finish: {}", e),
                        )
                    })
            }
            ReclaimTarget::ProbeContainers => {
                reclaim_containers(
                    HashMap::from([(
                        "label".to_string(),
                        vec![format!(
                            "{}={}",
                            migration::LABEL_PROBE,
                            migration::PROBE_LABEL_MIGRATION
                        )],
                    )]),
                    is_reapable_migration_probe,
                    ReclaimTarget::ProbeContainers,
                )
                .await
            }
            ReclaimTarget::ScrubContainers => {
                reclaim_containers(
                    HashMap::from([("name".to_string(), vec!["triple-c-scrub-".to_string()])]),
                    is_reapable_scrub_container,
                    ReclaimTarget::ScrubContainers,
                )
                .await
            }
            ReclaimTarget::CompactSnapshot { project_id } => {
                match find_project(projects, project_id) {
                    Ok(project) => compact_snapshot(project).await,
                    Err(e) => failed(target.clone(), e),
                }
            }
            ReclaimTarget::ClearCaches {
                project_id,
                include_rustup,
            } => match find_project(projects, project_id) {
                Ok(project) => clear_caches(project, *include_rustup).await,
                Err(e) => failed(target.clone(), e),
            },
        };
        outcome.total_freed_bytes += result.freed_bytes;
        outcome.results.push(result);
    }
    outcome
}

fn find_project<'a>(projects: &'a [Project], project_id: &str) -> Result<&'a Project, String> {
    projects
        .iter()
        .find(|p| p.id == project_id)
        .ok_or_else(|| format!("Project {} is not in the project list", project_id))
}

fn failed(target: ReclaimTarget, message: String) -> ReclaimResult {
    ReclaimResult {
        target: Some(target),
        destroyed: None,
        ok: false,
        freed_bytes: 0,
        projected_bytes: None,
        message,
    }
}

/// Remove the dangling managed images of one class.
///
/// `force: false` is load-bearing and is the same choice `sweep_orphaned_
/// snapshots` documents: Docker refuses with a 409 while any container is still
/// built from an image, including the stopped container of a project that is
/// not running. Forcing would untag that image out from under the project and
/// leave a container that cannot start. Those are counted and left.
async fn reclaim_dangling(target: &ReclaimTarget) -> ReclaimResult {
    let want = match target {
        ReclaimTarget::SupersededBaseImages => DanglingClass::Base,
        _ => DanglingClass::SnapshotCommit,
    };
    let docker = match get_docker() {
        Ok(d) => d,
        Err(e) => return failed(target.clone(), e),
    };
    let images = match docker
        .list_images(Some(ListImagesOptions {
            all: false,
            filters: HashMap::from([
                ("dangling".to_string(), vec!["true".to_string()]),
                ("label".to_string(), vec![format!("{}=true", LABEL_MANAGED)]),
            ]),
            ..Default::default()
        }))
        .await
    {
        Ok(images) => images,
        Err(e) => return failed(target.clone(), format!("Could not list images: {}", e)),
    };

    let mut freed = 0i64;
    let mut removed = 0usize;
    let mut in_use = 0usize;
    let mut errors = 0usize;
    for image in images {
        if classify_dangling(&image.labels) != want {
            continue;
        }
        match docker
            .remove_image(
                &image.id,
                Some(RemoveImageOptions {
                    force: false,
                    noprune: false,
                }),
                None,
            )
            .await
        {
            Ok(_) => {
                freed += image.size;
                removed += 1;
            }
            Err(bollard::errors::Error::DockerResponseServerError {
                status_code: 409, ..
            }) => in_use += 1,
            Err(e) => {
                log::warn!("Could not remove dangling image {}: {}", image.id, e);
                errors += 1;
            }
        }
    }

    let mut message = format!("Removed {} image(s).", removed);
    if in_use > 0 {
        message.push_str(&format!(
            " {} still had a container built from it and were left alone — start and stop, or \
             recreate, that project and they become collectable.",
            in_use
        ));
    }
    if errors > 0 {
        message.push_str(&format!(" {} could not be removed; see the log.", errors));
    }
    ReclaimResult {
        target: Some(target.clone()),
        destroyed: None,
        ok: errors == 0,
        freed_bytes: freed,
        projected_bytes: None,
        message,
    }
}

/// Prune the BuildKit cache through the `docker` CLI.
///
/// bollard 0.18 has no wrapper for `POST /build/prune` — see [`docker_cli`].
/// The freed figure comes from the CLI's own `Total reclaimed space:` line,
/// which is a measurement rather than a re-`df()` that a concurrent build could
/// have moved underneath us.
async fn reclaim_build_cache(all: bool) -> ReclaimResult {
    let target = ReclaimTarget::BuildCache { all };
    let filter = format!("until={}h", BUILD_CACHE_DEFAULT_UNTIL_HOURS);
    let mut args: Vec<&str> = vec!["builder", "prune", "--force"];
    if all {
        args.push("--all");
    } else {
        args.push("--filter");
        args.push(&filter);
    }
    match docker_cli_with_timeout(&args, DOCKER_PRUNE_TIMEOUT_SECS).await {
        Ok(output) => ReclaimResult {
            target: Some(target),
            destroyed: None,
            ok: true,
            freed_bytes: parse_reclaimed_space(&output),
            projected_bytes: None,
            message: if all {
                "Pruned the whole build cache.".to_string()
            } else {
                "Pruned build cache records older than 7 days.".to_string()
            },
        },
        Err(e) => failed(
            target,
            format!(
                "{e}. Pruning the build cache needs the `docker` command line tool, which the \
                 Docker Engine API does not expose an equivalent for."
            ),
        ),
    }
}

/// Untag the rollback pins nothing claims *and* that are past their grace
/// period.
///
/// ## The guards this was missing
///
/// It used to discard the tag entirely (`let Some((project_id, _tag)) = …`) and
/// untag on `has_record` alone. Everything else that touches a pin applies two
/// more conditions: `migration::parse_rollback_tag`, which is the only thing
/// separating `pre-migration-20260101-101500` from a hand-made
/// `pre-migration-keepme` somebody pinned deliberately, and the fourteen-day
/// grace period in `migration::pin_is_reapable`. `reap_stale_migration_pins`
/// requires both, and `destroy`'s `RollbackPin` arm requires the first with a
/// comment explaining exactly this hazard.
///
/// The gap mattered more here than anywhere else, because this path has none of
/// the brakes the others do: it is `Safety::Safe`, so one button with no
/// confirmation reaches it; its `ReclaimTarget::project_id()` is `None`, so
/// `reclaim`'s `is_migrating` guard never fires for it; and
/// `sweep_orphaned_snapshots` four lines below turns the untag into a deletion
/// on the same pass. A record that had merely gone missing for a minute was
/// enough to lose the only copy of a project's pre-migration system layer.
///
/// Unlike [`survey_rollback_pins`] this **starts** the ownerless clock for pins
/// nothing has sighted yet — it is a reaper, and that is a reaper's job. Those
/// pins are not dropped on this pass; they become collectable fourteen days
/// later.
///
/// Untag only, never `rmi`: dropping the tag makes the image dangling, and it
/// already carries `triple-c.managed=true` because `docker commit` created it,
/// so the sweep collects it under its own two conditions with the daemon's
/// still-in-use refusal in front. Nothing here removes a reachable image.
async fn reclaim_migration_pins() -> ReclaimResult {
    let target = ReclaimTarget::MigrationPins;
    let docker = match get_docker() {
        Ok(d) => d,
        Err(e) => return failed(target, e),
    };
    let images = match docker
        .list_images(Some(ListImagesOptions {
            all: false,
            filters: HashMap::from([(
                "reference".to_string(),
                vec!["triple-c-snapshot-*:pre-migration-*".to_string()],
            )]),
            ..Default::default()
        }))
        .await
    {
        Ok(images) => images,
        Err(e) => return failed(target, format!("Could not list rollback pins: {}", e)),
    };

    let now = chrono::Utc::now();
    let mut freed = 0i64;
    let mut dropped = 0usize;
    let mut held_back = 0usize;
    for image in images {
        for reference in &image.repo_tags {
            let Some((project_id, tag)) = migration::parse_snapshot_reference(reference) else {
                continue;
            };
            if migration::parse_rollback_tag(&tag).is_none() {
                continue;
            }
            let has_record = migration_store::has_record(&project_id).unwrap_or(true);
            if has_record {
                migration_store::clear_ownerless(&project_id, &tag);
                held_back += 1;
                continue;
            }
            // Starts the clock when nothing has sighted this pin before, which
            // is what makes the first pass a no-op for it.
            let ownerless_since =
                migration_store::note_ownerless_since(&project_id, &tag, &now);
            if pin_disposition(&tag, has_record, ownerless_since, &now) != PinDisposition::Reapable
            {
                held_back += 1;
                continue;
            }
            match migration::untag_image(reference).await {
                Ok(()) => {
                    migration_store::clear_ownerless(&project_id, &tag);
                    freed += image.size;
                    dropped += 1;
                }
                Err(e) => log::warn!("Could not drop rollback pin {}: {}", reference, e),
            }
        }
    }

    // Untagging alone frees nothing — it only makes the image dangling. The
    // sweep that follows is the single thing that actually removed layers, so
    // its figure is the only measurement there is. `freed` above is the size of
    // what was *untagged*, which the sweep may well have been refused on
    // because a stopped container still pins it.
    let sweep = container::sweep_orphaned_snapshots().await;
    log::info!(
        "Dropped {} ownerless rollback pin(s) covering {} bytes; the sweep reclaimed {}",
        dropped,
        freed,
        sweep.reclaimed_bytes
    );
    ReclaimResult {
        target: Some(target),
        destroyed: None,
        ok: true,
        freed_bytes: sweep.reclaimed_bytes,
        projected_bytes: None,
        message: {
            let mut message = format!(
                "Dropped {} ownerless pin(s) and swept {} image(s).",
                dropped,
                sweep.removed.len()
            );
            if held_back > 0 {
                message.push_str(&format!(
                    " {} pin(s) were left alone — still claimed by a migration record, or inside \
                     the {}-day grace period that starts when a pin loses one.",
                    held_back,
                    migration::STALE_PIN_MAX_AGE_DAYS
                ));
            }
            message
        },
    }
}

/// Delete ownerless `*-payload.tar` staging files.
///
/// Blocking `std::fs` throughout — `read_dir`, `metadata` and `remove_file` on
/// what can be multi-gigabyte tars — so [`reclaim`] calls it through
/// `spawn_blocking` rather than on the async worker it is running on.
fn reclaim_migration_staging(projects: &[Project]) -> ReclaimResult {
    let target = ReclaimTarget::MigrationStaging;
    let dir = match migration_store::migrations_dir() {
        Ok(dir) => dir,
        Err(e) => return failed(target, e),
    };
    let entries = match std::fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(e) => return failed(target, format!("Could not read {}: {}", dir.display(), e)),
    };

    let mut freed = 0i64;
    let mut removed = 0usize;
    let mut errors = 0usize;
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        let Some(project_id) = name.strip_suffix("-payload.tar") else {
            continue;
        };
        if migration_store::has_record(project_id).unwrap_or(true) {
            continue;
        }
        if projects
            .iter()
            .any(|p| p.id == project_id && crate::commands::migration_commands::is_migrating(&p.id))
        {
            continue;
        }
        let size = entry.metadata().map(|m| m.len() as i64).unwrap_or(0);
        match std::fs::remove_file(entry.path()) {
            Ok(()) => {
                freed += size;
                removed += 1;
            }
            Err(e) => {
                log::warn!("Could not remove {}: {}", entry.path().display(), e);
                errors += 1;
            }
        }
    }
    let mut message = format!("Removed {} staging file(s).", removed);
    if errors > 0 {
        message.push_str(&format!(" {} could not be removed; see the log.", errors));
    }
    ReclaimResult {
        target: Some(target),
        destroyed: None,
        ok: errors == 0,
        freed_bytes: freed,
        projected_bytes: None,
        message,
    }
}

/// Remove throwaway containers matching a filter *and* a predicate re-checked
/// on each summary.
async fn reclaim_containers(
    filters: HashMap<String, Vec<String>>,
    predicate: fn(&ContainerSummary) -> bool,
    target: ReclaimTarget,
) -> ReclaimResult {
    let docker = match get_docker() {
        Ok(d) => d,
        Err(e) => return failed(target, e),
    };
    let containers = match docker
        .list_containers(Some(ListContainersOptions {
            all: true,
            size: true,
            filters,
            ..Default::default()
        }))
        .await
    {
        Ok(containers) => containers,
        Err(e) => return failed(target, format!("Could not list containers: {}", e)),
    };

    let mut freed = 0i64;
    let mut removed = 0usize;
    let mut errors = 0usize;
    for summary in containers {
        // The daemon's filter is never the only guard on a removal.
        if !predicate(&summary) {
            continue;
        }
        let Some(id) = summary.id.as_deref() else {
            continue;
        };
        match container::remove_container(id).await {
            Ok(()) => {
                freed += summary.size_rw.unwrap_or(0).max(0);
                removed += 1;
            }
            Err(e) => {
                log::warn!("Could not remove container {}: {}", id, e);
                errors += 1;
            }
        }
    }
    // **The failures have to be in the message.** `ok: false` alone was not
    // enough: a partially failed run returned "Removed 2 container(s)." with
    // three more that could not be touched, and the panel renders that string
    // under a success headline. The count belongs in the sentence the user
    // actually reads.
    let mut message = format!("Removed {} container(s).", removed);
    if errors > 0 {
        message.push_str(&format!(
            " {} could not be removed; see the log.",
            errors
        ));
    }
    ReclaimResult {
        target: Some(target),
        destroyed: None,
        ok: errors == 0,
        freed_bytes: freed,
        projected_bytes: None,
        message,
    }
}

/// Remove one orphaned volume, re-checking every safety condition first.
///
/// The plan that offered this was computed against a `df()` from some seconds
/// ago. A project could have been added since — by this app or by a second copy
/// of it — and a container could have attached. So the store is re-consulted
/// *from disk*, the name is re-parsed and the live ref count is re-read here:
/// the typed confirmation is permission to act, not a promise that the world
/// stood still. Both of those re-checks predate the move to the destructive
/// path and are deliberately unchanged.
/// Drop a rollback pin whose project is no longer in `projects.json`.
///
/// The owned case ([`destroy`]'s `RollbackPin` arm) takes the project's claim
/// and clears the ownerless marker. Neither applies here: there is no project
/// to claim, and nothing else can be mid-operation on an id the store does not
/// know. What *does* still apply is the tag validation — this is the one
/// destructive variant carrying a free-form string over IPC, and `latest` would
/// name a live snapshot rather than a pin.
async fn destroy_ownerless_rollback_pin(
    project_id: &str,
    tag: &str,
) -> Result<ReclaimResult, String> {
    if migration::parse_rollback_tag(tag).is_none() {
        return Err(format!(
            "{:?} is not a rollback pin tag. Nothing was removed.",
            tag
        ));
    }
    let reference = format!("triple-c-snapshot-{}:{}", project_id, tag);
    migration::untag_image(&reference).await?;
    // The grace clock is meaningless once the tag is gone, and the marker file
    // would otherwise outlive everything that could ever read it.
    migration_store::clear_ownerless(project_id, tag);
    // Untagging only makes the image dangling; the sweep applies its own
    // refusal rules to whatever that turns out to be.
    let sweep = container::sweep_orphaned_snapshots().await;
    log::info!(
        "Dropped ownerless rollback pin {} on explicit confirmation",
        reference
    );
    Ok(ReclaimResult {
        target: None,
        destroyed: Some(DestructiveTarget::RollbackPin {
            project_id: project_id.to_string(),
            tag: tag.to_string(),
        }),
        ok: true,
        freed_bytes: sweep.reclaimed_bytes,
        projected_bytes: None,
        message: format!(
            "Dropped rollback pin {} for a project that is no longer in Triple-C.",
            tag
        ),
    })
}

async fn destroy_orphan_volume(name: &str, projects: &[Project]) -> Result<ReclaimResult, String> {
    let docker = get_docker()?;

    let (json_exists, json_ids) = projects_json_snapshot_async().await;
    let known = project_store_trust(projects, json_exists, json_ids.as_deref())?;

    let usage = docker
        .df()
        .await
        .map_err(|e| format!("Could not re-check volume usage: {}", e))?;
    let volumes = usage.volumes.unwrap_or_default();
    let facts: Vec<VolumeFacts> = volumes.iter().map(volume_facts).collect();
    let still_orphaned = orphan_volumes(&facts, &known, true)
        .into_iter()
        .find(|v| v.name == name);
    let Some(volume) = still_orphaned else {
        return Err(format!(
            "{} now matches a project in your project list, or a container has attached to it, \
             so it is no longer unclaimed. Nothing was removed.",
            name
        ));
    };

    docker
        .remove_volume(name, None)
        .await
        .map_err(|e| format!("Could not remove volume {}: {}", name, e))?;
    log::info!("Removed orphaned volume {} on explicit confirmation", name);
    Ok(ReclaimResult {
        target: None,
        destroyed: Some(DestructiveTarget::OrphanVolume {
            name: name.to_string(),
            project_id: volume.project_id.clone(),
        }),
        ok: true,
        freed_bytes: volume.bytes,
        projected_bytes: None,
        message: format!("Removed volume {}.", name),
    })
}

/// Rewrite a project's stacked commit layers into a single layer.
///
/// ## The sequence, and why each step is where it is
///
/// 1. **Refuse while a migration is in flight or the container runs.** The same
///    rule everything else that touches a project's image obeys: the window
///    between a migration's `remove_container` and the create that follows
///    looks exactly like "no container", and rewriting `:latest` underneath a
///    running container's image is not something Docker protects you from.
/// 2. **Capture the image config** — env, cmd, entrypoint, labels, workdir. It
///    does not survive `FROM scratch` and has to be replayed.
/// 3. **Build to a temporary tag**, never over `:latest`. If anything below
///    fails, the project's snapshot is exactly as it was.
/// 4. **Compare sizes.** Compaction is not always a win: with nothing
///    superseded, the merged layer can recompress *larger* — measured at 29.8
///    MB → 30.8 MB on a synthetic stack with no waste in it. A result that is
///    not smaller is discarded and reported as "nothing to reclaim", rather
///    than shipped as an improvement.
/// 5. **Replay the config** by creating a container from the flat image with
///    that config and committing it. `docker commit` bakes the container's
///    config into the image, and it round-trips values a Dockerfile `ENV` could
///    not survive — verified with a multi-line env var and a label containing a
///    double quote.
/// 6. **Move `:latest` last.** Until the final tag, the old snapshot is still
///    what the project starts from, so every failure above self-heals.
///
/// Nothing here forces a removal, and nothing touches a volume.
pub async fn compact_snapshot(project: &Project) -> ReclaimResult {
    let target = ReclaimTarget::CompactSnapshot {
        project_id: project.id.clone(),
    };

    // **Acquired, and held for the whole rewrite.** This is the operation the
    // per-project lock was written for. Compaction resolves
    // `triple-c-snapshot-{id}:latest` when its build starts and commits back
    // over that same tag minutes later, and the Settings panel is a sidebar
    // rather than a modal, so Project Home stays live with Start, Reset and
    // Migrate clickable the whole time. A one-shot `is_migrating` read at the
    // door — which is all this used to have — covered none of that: it could
    // not see a Start, a Reset or a second compaction at all, and it could not
    // see a migration that began *after* the check.
    let _guard =
        match crate::project_lock::try_acquire(&project.id, crate::project_lock::ProjectOp::Compaction)
        {
            Ok(guard) => guard,
            Err(reason) => return failed(target, reason),
        };

    let docker = match get_docker() {
        Ok(d) => d,
        Err(e) => return failed(target, e),
    };
    let snapshot_ref = get_snapshot_image_name(project);

    // Before anything is created, so a leftover from a previous run's crash is
    // gone and nothing this run creates can be mistaken for one. The guard
    // above is already held, which is why this passes the project id: the sweep
    // excludes this compaction's own claim rather than seeing it and skipping.
    // See [`remove_stale_compaction_containers`] for why it moved here from the
    // end of `restore_image_config`.
    reap_stale_compaction_artifacts_for(&project.id).await;

    // The container must be stopped: this rewrites the image it is running
    // from, and a running container also means the writable layer holds work
    // that has not been committed and would be stranded.
    if let Ok(Some(container_id)) = container::find_existing_container(project).await {
        if container::is_container_running(&container_id)
            .await
            .unwrap_or(false)
        {
            return failed(
                target,
                "Stop this project's container before compacting its snapshot.".to_string(),
            );
        }
    }

    let before = match docker.inspect_image(&snapshot_ref).await {
        Ok(image) => image,
        Err(e) => {
            return failed(target, format!("Could not inspect {}: {}", snapshot_ref, e));
        }
    };

    // **What this snapshot actually costs is its *unique* bytes, not its size.**
    // Its size includes the base image, which every other project is still
    // built from and which is not going anywhere. The flattened replacement,
    // by contrast, shares nothing — so it is charged in full. Comparing size to
    // size would score a project with a 0.63 GB delta over a 4.72 GB base as a
    // 0.5 GB saving while it in fact cost 4.1 GB. Measured on a real daemon:
    // eight of ten projects were in exactly that shape.
    let before_bytes = image_unique_bytes(&snapshot_ref).await;
    let before_total = before.size.unwrap_or(0);

    // Projected before the run, so the outcome can be read against it. The
    // plan's figure came from an even-split approximation over the aggregate;
    // here the per-layer sizes are to hand, so the bound is exact — see
    // [`compaction_bounds`] for why it is a bound at all.
    let projected = docker
        .image_history(&snapshot_ref)
        .await
        .ok()
        .map(|entries| {
            let sizes: Vec<i64> = entries.iter().map(|e| e.size).collect();
            // Bounded by the superseded bytes *and* by what survives
            // re-duplicating the shared base — the same two terms
            // `compaction_ceiling_for` weighs. The history here covers the base
            // layers too, so the first term is looser than it could be; the
            // second is what binds in practice and it caps the result anyway.
            let superseded = compaction_bounds(&sizes).1;
            let shared = before_total - before_bytes;
            superseded.min(before_bytes - shared).max(0)
        });
    let Some(config) = before.config.clone() else {
        return failed(
            target,
            format!(
                "{} has no image config to preserve, which means it is not a snapshot this app \
                 committed. Refusing to rewrite it.",
                snapshot_ref
            ),
        );
    };

    // `:compacting` rather than `:latest`. Nothing starts from this tag, and it
    // is removed on every exit path below — every *in-process* one. A process
    // that dies mid-build leaves it behind, which is what
    // [`reap_stale_compaction_artifacts`] exists to collect.
    let staging_ref = compaction_staging_ref(&project.id);
    let dockerfile = compaction_dockerfile(&snapshot_ref, &container::snapshot_scrub_script());

    if let Err(e) = build_from_dockerfile(&dockerfile, &staging_ref).await {
        let _ = migration::untag_image(&staging_ref).await;
        return failed(target, format!("Compaction build failed: {}", e));
    }

    // The flat image shares nothing, so its unique cost *is* its size.
    let after_bytes = match docker.inspect_image(&staging_ref).await {
        Ok(_) => image_unique_bytes(&staging_ref).await,
        Err(e) => {
            let _ = migration::untag_image(&staging_ref).await;
            return failed(target, format!("Could not measure the result: {}", e));
        }
    };

    if after_bytes >= before_bytes {
        let _ = migration::untag_image(&staging_ref).await;
        let _ = container::sweep_orphaned_snapshots().await;
        return ReclaimResult {
            target: Some(target),
            destroyed: None,
            ok: true,
            freed_bytes: 0,
            projected_bytes: projected,
            message: format!(
                "Left untouched — flattening would have made it bigger. This snapshot costs {} \
                 beyond the base image it shares with your other projects, and a flattened copy \
                 would cost {} because it shares nothing. That happens when the layers hold \
                 little superseded data relative to the base.",
                human(before_bytes),
                human(after_bytes)
            ),
        };
    }

    // Replay the config onto the flat image.
    if let Err(e) = restore_image_config(&staging_ref, &snapshot_ref, config).await {
        let _ = migration::untag_image(&staging_ref).await;
        return failed(
            target,
            format!("Could not restore the snapshot's configuration: {}", e),
        );
    }

    // The staging tag has served its purpose; dropping it leaves the flat
    // intermediate dangling and labelled, so the sweep collects it.
    let _ = migration::untag_image(&staging_ref).await;
    let sweep = container::sweep_orphaned_snapshots().await;

    let freed = (before_bytes - after_bytes).max(0);
    log::info!(
        "Compacted {} ({} total): unique cost went from {} to {} bytes, {} reclaimed, and the \
         sweep collected {} superseded image(s)",
        snapshot_ref,
        before_total,
        before_bytes,
        after_bytes,
        freed,
        sweep.removed.len()
    );
    ReclaimResult {
        target: Some(target),
        destroyed: None,
        ok: true,
        freed_bytes: freed,
        projected_bytes: projected,
        message: format!(
            "Rewrote {} into a single layer. Its cost on disk went from {} to {}.",
            snapshot_ref,
            human(before_bytes),
            human(after_bytes)
        ),
    }
}

/// Bytes as a short human string, for a message a user reads.
///
/// Base 1000, matching `docker system df` and the frontend's `formatBytes` —
/// a message saying 4.4 GiB beside a table saying 4.7 GB would read as two
/// different numbers for the same thing.
pub(crate) fn human(bytes: i64) -> String {
    const UNITS: [&str; 6] = ["B", "KB", "MB", "GB", "TB", "PB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value.abs() >= 1000.0 && unit < UNITS.len() - 1 {
        value /= 1000.0;
        unit += 1;
    }
    // **The rounding has to be applied before the unit is settled.** 999,999
    // divides to 999.999 KB, which is under the loop's threshold, and then
    // `{:.1}` rounds it up to "1000.0 KB" — a unit the ladder is supposed to
    // make impossible. That is the exact bug `formatBytes.ts` was written to
    // fix on the frontend, and this function's output is rendered on the same
    // line as `formatBytes`' in a reclaim message, so the two disagreeing about
    // it is visible in one sentence.
    if unit > 0 && (value.abs() * 10.0).round() / 10.0 >= 1000.0 && unit < UNITS.len() - 1 {
        value /= 1000.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{} {}", bytes, UNITS[0])
    } else {
        format!("{:.1} {}", value, UNITS[unit])
    }
}

/// Build a one-file context and hand it to the daemon.
///
/// The context holds nothing but the Dockerfile — every byte the build touches
/// is already inside the daemon, which is the entire point of doing this as a
/// build rather than an export/import round trip through this process.
async fn build_from_dockerfile(dockerfile: &str, tag: &str) -> Result<(), String> {
    use bollard::image::BuildImageOptions;
    use futures_util::StreamExt;

    let mut context = Vec::new();
    {
        let mut archive = tar::Builder::new(&mut context);
        let mut header = tar::Header::new_gnu();
        header.set_path("Dockerfile").map_err(|e| e.to_string())?;
        header.set_size(dockerfile.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        archive
            .append(&header, dockerfile.as_bytes())
            .map_err(|e| e.to_string())?;
        archive.finish().map_err(|e| e.to_string())?;
    }

    let docker = get_docker()?;
    let options = BuildImageOptions {
        dockerfile: "Dockerfile".to_string(),
        t: tag.to_string(),
        rm: true,
        forcerm: true,
        ..Default::default()
    };

    let mut stream = docker.build_image(options, None, Some(context.into()));
    while let Some(item) = stream.next().await {
        match item {
            Ok(info) => {
                if let Some(error) = info.error {
                    return Err(error);
                }
            }
            Err(e) => return Err(e.to_string()),
        }
    }
    Ok(())
}

/// Name prefix for the throwaway container that replays a compacted image's
/// config. Distinct from `triple-c-scrub-*` on purpose — see
/// [`restore_image_config`].
const COMPACTION_CONTAINER_PREFIX: &str = "triple-c-compact-";

/// The tag a compaction builds to before it commits back over `:latest`.
pub fn compaction_staging_ref(project_id: &str) -> String {
    format!("triple-c-snapshot-{}:compacting", project_id)
}

/// Collect what a **crashed** compaction leaves on the daemon.
///
/// ## The stranded image nothing could find
///
/// `triple-c-snapshot-{id}:compacting` is dropped on every exit path inside
/// [`compact_snapshot`] — every path that runs *in this process*. Kill the app
/// mid-build and the tag survives, holding a whole flattened copy of a
/// multi-gigabyte snapshot, and it was invisible to every single reclaim path
/// in this module:
///
/// * the startup sweep filters `dangling=true`, and a tagged image is not
///   dangling;
/// * the pin reaper matches `:pre-migration-*`;
/// * the per-project scan joins on `:latest`, so the row does not mention it;
/// * `list_reclaimable` has no bucket that names it.
///
/// So it sat there forever with nothing in the UI that would ever say why the
/// daemon was several gigabytes larger than the panel's own total. The same is
/// true of a leftover `triple-c-compact-*` container, which is worse: it also
/// *pins* the flattened image, so even untagging would not let the sweep
/// collect it.
///
/// ## What it will not touch
///
/// A `:compacting` tag belonging to a compaction that is still running. In this
/// process that is the project lock; in another instance of the app there is no
/// lock to consult, so the image's own age is the brake — the tag exists for
/// the few seconds to minutes between the build finishing and the commit, and
/// [`SCRATCH_CONTAINER_MIN_AGE_SECS`] is far longer than that.
pub async fn reap_stale_compaction_artifacts() -> usize {
    reap_stale_compaction_artifacts_for("").await
}

/// [`reap_stale_compaction_artifacts`], ignoring `owner_project_id`'s own claim
/// and its own staging tag age.
///
/// Called at the top of [`compact_snapshot`], which is already holding that
/// project's lock: without the exclusion the sweep would see its own claim and
/// skip, and the leftovers it is there to clear would survive the very run that
/// went looking for them.
async fn reap_stale_compaction_artifacts_for(owner_project_id: &str) -> usize {
    remove_stale_compaction_containers(owner_project_id).await;

    let Ok(docker) = get_docker() else {
        return 0;
    };
    let images = docker
        .list_images(Some(ListImagesOptions {
            all: false,
            filters: HashMap::from([(
                "reference".to_string(),
                vec!["triple-c-snapshot-*:compacting".to_string()],
            )]),
            ..Default::default()
        }))
        .await
        .unwrap_or_default();

    let now = chrono::Utc::now().timestamp();
    let mut dropped = 0usize;
    for image in images {
        for reference in &image.repo_tags {
            let Some((project_id, tag)) = migration::parse_snapshot_reference(reference) else {
                continue;
            };
            if tag != "compacting" {
                continue;
            }
            let is_owner = project_id == owner_project_id;
            if !is_owner
                && crate::project_lock::is_held_by(
                    &project_id,
                    crate::project_lock::ProjectOp::Compaction,
                )
            {
                continue;
            }
            // Age is the only thing that can see another instance's live
            // compaction, so it applies to the owner's own tag too.
            if now - image.created < SCRATCH_CONTAINER_MIN_AGE_SECS {
                log::info!(
                    "Leaving {} alone — it was built less than {} minutes ago and may belong to \
                     a compaction that is still running",
                    reference,
                    SCRATCH_CONTAINER_MIN_AGE_SECS / 60
                );
                continue;
            }
            match migration::untag_image(reference).await {
                Ok(()) => {
                    log::info!(
                        "Dropped stranded compaction staging tag {} ({:.2} GB) — the compaction \
                         that built it never finished",
                        reference,
                        image.size as f64 / 1_073_741_824.0
                    );
                    dropped += 1;
                }
                Err(e) => log::warn!("Could not drop {}: {}", reference, e),
            }
        }
    }
    dropped
}

/// Remove any container left behind by an interrupted compaction.
///
/// ## It was called from the wrong end of the compaction
///
/// The doc used to say "runs at the start of a compaction … by the time this is
/// called, this task owns the compaction path". It did not: the only call was
/// from inside [`restore_image_config`], which is the *last* step of a
/// compaction. So it ran at the end, and it force-removed every
/// `triple-c-compact-*` container on the daemon — including the one a second,
/// concurrent compaction had just created and was about to commit. The claim in
/// the comment is now true because the call moved to the top of
/// [`compact_snapshot`], before anything is created.
///
/// Two brakes on top of the move, because the name carries a random uuid rather
/// than a project id and so cannot be attributed to an owner:
///
/// * [`crate::project_lock::any_held_excluding`] — if this process is compacting anything
///   at all, every `triple-c-compact-*` container might be that compaction's,
///   and none is touched.
/// * [`SCRATCH_CONTAINER_MIN_AGE_SECS`] — the same age gate the scrub bucket
///   uses, which is the only thing that can see *another instance's* live
///   compaction.
async fn remove_stale_compaction_containers(owner_project_id: &str) {
    if crate::project_lock::any_held_excluding(
        crate::project_lock::ProjectOp::Compaction,
        owner_project_id,
    ) {
        log::debug!(
            "Skipping the compaction-container sweep: a compaction is in flight in this process"
        );
        return;
    }
    let Ok(docker) = get_docker() else {
        return;
    };
    let containers = docker
        .list_containers(Some(ListContainersOptions {
            all: true,
            size: false,
            filters: HashMap::from([(
                "name".to_string(),
                vec![COMPACTION_CONTAINER_PREFIX.to_string()],
            )]),
            ..Default::default()
        }))
        .await
        .unwrap_or_default();
    for summary in containers {
        // Docker's `name` filter is a substring match; the full name decides.
        if !is_compaction_container(&summary) {
            continue;
        }
        if !is_stale_scratch(&summary) {
            log::info!(
                "Leaving compaction container {} alone — it is younger than {} minutes, so it \
                 may belong to another Triple-C instance's live compaction",
                summary.id.as_deref().unwrap_or("<unknown>"),
                SCRATCH_CONTAINER_MIN_AGE_SECS / 60
            );
            continue;
        }
        if let Some(id) = summary.id.as_deref() {
            match container::remove_container(id).await {
                Ok(()) => log::info!("Removed stale compaction container {}", id),
                Err(e) => log::warn!("Could not remove stale compaction container {}: {}", id, e),
            }
        }
    }
}

/// Whether a container is one of ours from an interrupted compaction.
fn is_compaction_container(summary: &ContainerSummary) -> bool {
    summary
        .names
        .as_deref()
        .unwrap_or(&[])
        .iter()
        .any(|name| {
            name.trim_start_matches('/')
                .starts_with(COMPACTION_CONTAINER_PREFIX)
        })
}

/// Put a captured image config back onto a flattened image, under the original
/// tag.
///
/// `FROM scratch` discards env, cmd, entrypoint, workdir, user, labels and
/// exposed ports, and there is no way to hand them to a build without rendering
/// them as Dockerfile instructions — which a multi-line `CLAUDE_INSTRUCTIONS`
/// or a label containing a quote would not survive. Creating a container with
/// the config and committing it round-trips the structured values instead.
/// Verified against Docker 29.7.2 with both of those cases.
///
/// The container is never started. `docker commit` on a created container is
/// well defined and adds an empty layer, so the result is still one layer of
/// content.
async fn restore_image_config(
    flat_ref: &str,
    final_ref: &str,
    config: bollard::models::ImageConfig,
) -> Result<(), String> {
    use bollard::container::{Config, CreateContainerOptions, RemoveContainerOptions};
    use bollard::image::CommitContainerOptions;

    let docker = get_docker()?;

    // **Its own prefix, not `triple-c-scrub-*`.** An earlier version reused the
    // secret-rewrite name on the grounds that the existing reclaim bucket would
    // then collect any leftover. It would — including the live one: that bucket
    // removes with `force: true`, so a "remove scrub containers" reclaim fired
    // from a second window while a compaction was mid-flight would destroy the
    // container the commit is about to run against. Sequential execution inside
    // one `reclaim` call is not a guarantee when two can be in flight.
    //
    // Stale ones are instead swept at the *start* of the next compaction. That
    // call used to be right here — i.e. at the end of a compaction, contradicting
    // its own doc comment — where it force-removed a concurrent compaction's
    // live container. It now sits at the top of `compact_snapshot`.
    let scratch_name = format!("{}{}", COMPACTION_CONTAINER_PREFIX, uuid::Uuid::new_v4().simple());

    // `image` is the flat build; everything else is copied from the original so
    // the committed image is byte-for-byte the same configuration.
    let create_config = Config::<String> {
        image: Some(flat_ref.to_string()),
        env: config.env.clone(),
        cmd: config.cmd.clone(),
        entrypoint: config.entrypoint.clone(),
        working_dir: config.working_dir.clone(),
        user: config.user.clone(),
        labels: config.labels.clone(),
        exposed_ports: config.exposed_ports.clone(),
        volumes: config.volumes.clone(),
        stop_signal: config.stop_signal.clone(),
        shell: config.shell.clone(),
        healthcheck: config.healthcheck.clone(),
        ..Default::default()
    };

    docker
        .create_container(
            Some(CreateContainerOptions {
                name: scratch_name.clone(),
                platform: None,
            }),
            create_config,
        )
        .await
        .map_err(|e| format!("Could not create the config-restore container: {}", e))?;

    let (repo, tag) = migration::split_image_ref(final_ref);
    let commit = docker
        .commit_container(
            CommitContainerOptions {
                container: scratch_name.clone(),
                repo,
                tag,
                // Never started, so there is nothing to pause.
                pause: false,
                ..Default::default()
            },
            // Deliberately empty: the container was created with the config
            // already on it, and commit inherits every field it is not told
            // about. Passing the config twice would be the only way to get the
            // two copies out of step.
            Config::<String>::default(),
        )
        .await
        .map_err(|e| format!("Could not commit the compacted snapshot: {}", e));

    // Remove the scratch container whether or not the commit worked — a
    // leftover would pin the flat image and show up in this very panel as a
    // scrub container.
    if let Err(e) = docker
        .remove_container(
            &scratch_name,
            Some(RemoveContainerOptions {
                v: false,
                force: true,
                ..Default::default()
            }),
        )
        .await
    {
        log::warn!(
            "Could not remove the config-restore container {}: {}",
            scratch_name,
            e
        );
    }

    commit.map(|_| ())
}

/// Delete the regenerable package caches inside a running container.
///
/// A `docker exec`, so the container has to be running — the caches are in the
/// home *volume*, and the only way to size and delete them accurately is from
/// inside, where `du` can see them.
///
/// Runs as `claude`, not root: every path is under that user's `$HOME`, and a
/// root `rm` that got a path wrong would have the authority to act on it.
pub async fn clear_caches(project: &Project, include_rustup: bool) -> ReclaimResult {
    let target = ReclaimTarget::ClearCaches {
        project_id: project.id.clone(),
        include_rustup,
    };
    // Held rather than polled, for the same reason as everything else here: a
    // Reset removes both volumes, and an exec `rm -rf`-ing paths inside one of
    // them while it is being deleted is not a race worth having.
    let _guard = match crate::project_lock::try_acquire(
        &project.id,
        crate::project_lock::ProjectOp::CacheClear,
    ) {
        Ok(guard) => guard,
        Err(reason) => return failed(target, reason),
    };

    let container_id = match container::find_existing_container(project).await {
        Ok(Some(id)) => id,
        Ok(None) => {
            return failed(
                target,
                "This project has no container. Start it first — the caches are cleared from \
                 inside."
                    .to_string(),
            )
        }
        Err(e) => return failed(target, e),
    };
    if !container::is_container_running(&container_id)
        .await
        .unwrap_or(false)
    {
        return failed(
            target,
            "Start this project's container first — the caches live in its home volume and are \
             cleared from inside."
                .to_string(),
        );
    }

    let script = cache_clear_script(include_rustup);
    let cmd = vec!["/bin/sh".to_string(), "-c".to_string(), script];
    match super::exec::exec_oneshot_as(&container_id, "claude", cmd, Vec::new()).await {
        Ok((output, _exit)) => match parse_cache_total(&output) {
            Some(bytes) => ReclaimResult {
                target: Some(target),
                destroyed: None,
                ok: true,
                freed_bytes: bytes as i64,
                projected_bytes: None,
                message: "Cleared. Every one of these refills itself the next time a tool needs \
                          it."
                    .to_string(),
            },
            None => failed(
                target,
                format!(
                    "The cache clear did not report a total, so nothing can be confirmed. \
                     Output: {}",
                    output.trim()
                ),
            ),
        },
        Err(e) => failed(target, format!("Could not clear caches: {}", e)),
    }
}

// ---------------------------------------------------------------------------
// Destructive
// ---------------------------------------------------------------------------

/// Delete one object that has no other copy.
///
/// Takes a [`DestructiveTarget`], which [`reclaim`] cannot construct or be
/// handed — a bulk selection has no way to reach this function. `confirmation`
/// must be the project's name, typed.
///
/// Everything here is refused while a migration is in flight, and while the
/// project's container exists in a state that would be broken by the removal.
pub async fn destroy(
    target: &DestructiveTarget,
    confirmation: &str,
    projects: &[Project],
) -> Result<ReclaimResult, String> {
    // **The orphan arm comes first, because it has no project to find.** Its id
    // is parsed out of a volume name and matches nothing in the store — that is
    // its definition — so `find_project` would refuse it and the typed subject
    // is the volume's own name.
    if let DestructiveTarget::OrphanVolume { name, .. } = target {
        if !confirmation_matches(name, confirmation) {
            return Err(format!(
                "Type the volume name ({}) exactly to confirm. Nothing was removed.",
                name
            ));
        }
        return destroy_orphan_volume(name, projects).await;
    }

    // **A rollback pin can outlive the project it belongs to.**
    // `survey_rollback_pins` walks *images*, not projects, and deliberately
    // tolerates an absent project by falling back to the raw id as the display
    // name. So a pin left by a project the user has since deleted is measured
    // and listed — and `find_project` below would refuse it on every attempt,
    // making a multi-GB image permanently undeletable through the panel that
    // exists to find exactly that. Take the same early return `OrphanVolume`
    // takes, confirming against the subject the UI actually showed: the id.
    if let DestructiveTarget::RollbackPin { project_id, tag } = target {
        if find_project(projects, target.project_id()).is_err() {
            if !confirmation_matches(project_id, confirmation) {
                return Err(format!(
                    "This pin's project is no longer in Triple-C, so there is no name to type.                      Type the project id ({}) exactly to confirm. Nothing was removed.",
                    project_id
                ));
            }
            return destroy_ownerless_rollback_pin(project_id, tag).await;
        }
    }

    let project = find_project(projects, target.project_id())?;
    if !confirmation_matches(&project.name, confirmation) {
        return Err(format!(
            "Type the project name ({}) exactly to confirm. Nothing was removed.",
            project.name
        ));
    }
    // **Held for the whole removal, not polled at the door.** The old check was
    // a single `is_migrating` read, so a compaction that had already resolved
    // `:latest` could commit its flattened result back over a snapshot this
    // function had just deleted, resurrecting the layer the user typed a
    // project name to be rid of.
    let _guard =
        crate::project_lock::try_acquire(&project.id, crate::project_lock::ProjectOp::Destroy)?;

    let docker = get_docker()?;

    // A running container holds all three of these open, and Docker's refusal
    // is not something to lean on for the volumes: it would happily leave a
    // half-removed project behind.
    let existing_container = container::find_existing_container(project).await.ok().flatten();
    if let Some(container_id) = existing_container.as_deref() {
        if container::is_container_running(container_id)
            .await
            .unwrap_or(false)
        {
            return Err(
                "Stop this project's container first. Nothing was removed.".to_string(),
            );
        }
    }

    match target {
        DestructiveTarget::HomeVolume { .. } | DestructiveTarget::ConfigVolume { .. } => {
            let name = match target {
                DestructiveTarget::HomeVolume { .. } => home_volume_name(&project.id),
                _ => config_volume_name(&project.id),
            };
            // Size it before it goes, so the report is a measurement.
            let bytes = volume_size(&name).await;

            // **A stopped container still pins its volumes.** Docker refuses
            // `remove_volume` with a 409 while any container references one,
            // and every project that has ever been started has exactly that —
            // a stopped container is the resting state, not an edge case. So
            // the container is removed first rather than letting the user type
            // a project name and then meet a raw 409. It is regenerable from
            // the snapshot; `DestructiveItem::loses` says so.
            if let Some(container_id) = existing_container.as_deref() {
                container::remove_container(container_id).await.map_err(|e| {
                    format!(
                        "Could not remove this project's container, which still holds the volume \
                         open: {}. Nothing was removed.",
                        e
                    )
                })?;
                log::info!(
                    "Removed container {} so {} could be deleted",
                    container_id,
                    name
                );
            }

            docker
                .remove_volume(&name, None)
                .await
                .map_err(|e| format!("Could not remove volume {}: {}", name, e))?;
            log::info!("Removed volume {} on explicit confirmation", name);
            Ok(ReclaimResult {
                target: None,
                destroyed: Some(target.clone()),
                ok: true,
                freed_bytes: bytes,
                projected_bytes: None,
                message: format!("Removed volume {}.", name),
            })
        }
        DestructiveTarget::SnapshotImage { .. } => {
            let reference = get_snapshot_image_name(project);
            // Measured before the removal, and net of the shared base — which
            // other projects are still built from and which is not freed.
            let bytes = image_unique_bytes(&reference).await;
            container::remove_snapshot_image(project).await?;
            let sweep = container::sweep_orphaned_snapshots().await;
            Ok(ReclaimResult {
                target: None,
                destroyed: Some(target.clone()),
                ok: true,
                freed_bytes: bytes + sweep.reclaimed_bytes,
                projected_bytes: None,
                message: format!(
                    "Removed {}. This project will build from the base image next time it starts.",
                    reference
                ),
            })
        }
        DestructiveTarget::OrphanVolume { .. } => {
            // Handled above, before the project lookup that this arm's siblings
            // all depend on. Unreachable, and an `unreachable!()` here would be
            // a panic in a function that deletes things.
            Err("An orphaned volume is not a project object.".to_string())
        }
        DestructiveTarget::RollbackPin { tag, .. } => {
            // **The one destructive variant carrying a free-form string.**
            // Every other arm builds its target from constants; this one takes
            // a tag over IPC and interpolates it into an image reference that
            // is then removed. Unvalidated, `tag: "latest"` names the project's
            // live snapshot — deleted under a dialog that says "rollback pin".
            // `parse_rollback_tag` accepts only `pre-migration-<YYYYmmdd-HHMMSS>`,
            // which is exactly what `rollback_tag` produces and nothing else.
            if migration::parse_rollback_tag(tag).is_none() {
                return Err(format!(
                    "{:?} is not a rollback pin tag. Nothing was removed.",
                    tag
                ));
            }
            let reference = format!("triple-c-snapshot-{}:{}", project.id, tag);
            migration::untag_image(&reference).await?;
            // The grace clock this pin may have had is meaningless once the tag
            // is gone; leaving the marker would keep a dead file in the
            // migrations directory forever.
            migration_store::clear_ownerless(&project.id, tag);
            // Untagging only makes the image dangling. Whatever came back came
            // back through the sweep, under its own refusal rules.
            let sweep = container::sweep_orphaned_snapshots().await;
            log::info!("Dropped rollback pin {} on explicit confirmation", reference);
            Ok(ReclaimResult {
                target: None,
                destroyed: Some(target.clone()),
                ok: true,
                freed_bytes: sweep.reclaimed_bytes,
                projected_bytes: None,
                message: format!(
                    "Dropped {}. Rolling that migration back is no longer possible.",
                    reference
                ),
            })
        }
    }
}

/// Bytes an image would actually give back if it were removed: its size minus
/// what it shares with other images.
///
/// The plain `Size` from `inspect_image` includes the base, which several other
/// projects are still built from and which is not going anywhere. Reporting it
/// as freed would overstate a snapshot removal by ~4.7 GB every time. Only
/// `df()` computes `SharedSize`, so each call costs a full daemon walk.
///
/// That is three `df()`s on a compaction (before, after, and the scan that
/// planned it) and one per destructive removal. Acceptable because both are
/// single-object, user-initiated actions that already take seconds to minutes —
/// but it is why nothing in the *scan* path calls this: `scan` gets shared
/// sizes from the one `df()` it already makes.
async fn image_unique_bytes(reference: &str) -> i64 {
    let Ok(docker) = get_docker() else {
        return 0;
    };
    let Ok(usage) = docker.df().await else {
        return 0;
    };
    usage
        .images
        .unwrap_or_default()
        .iter()
        .find(|image| image.repo_tags.iter().any(|tag| tag == reference))
        .map(|image| (image.size - image.shared_size.max(0)).max(0))
        .unwrap_or(0)
}

/// One volume's size, or 0 when the daemon will not say. Costs a `df()`, which
/// is why it is only used on the destructive path where there is exactly one.
async fn volume_size(name: &str) -> i64 {
    let Ok(docker) = get_docker() else {
        return 0;
    };
    let Ok(usage) = docker.df().await else {
        return 0;
    };
    usage
        .volumes
        .unwrap_or_default()
        .iter()
        .find(|v| v.name == name)
        .and_then(volume_bytes)
        .unwrap_or(0)
}

#[cfg(test)]
#[path = "disk_tests.rs"]
mod tests;
