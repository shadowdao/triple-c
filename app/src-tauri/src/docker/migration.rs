//! Container **base-image migration** — the Docker-level machinery.
//!
//! The orchestration (which Tauri command does what, in which order) lives in
//! [`crate::commands::migration_commands`]. This module holds the two things
//! that benefit from being separate: the *pure* delta computation, which is
//! fully unit-tested below, and the small set of Docker operations migration
//! needs that nothing else in the app does.
//!
//! # Why this is a diff of two image manifests and not `docker diff`
//!
//! `docker diff` reports changes since the container's **last commit**. Every
//! Triple-C project container is created from its own snapshot image and
//! re-committed on each recreation, so `docker diff` on one reports only what
//! happened since the most recent commit — measured on a real project: 2,533
//! entries, almost all of them `/tmp` churn, and none of the actual
//! divergence from the base. It is the wrong tool here and is not used.
//!
//! # Why the diff is filtered through dpkg ownership
//!
//! Raw path diffing lies. On a real project, 11,088 paths differed between the
//! snapshot and the current base and approximately **zero** were user-authored:
//! the rest were the base's *own* AWS CLI and pnpm trees at different versions.
//! Two filters make the set honest:
//!
//! 1. **dpkg ownership** — anything listed in `/var/lib/dpkg/info/*.list` in
//!    either image belongs to a package, not to the user.
//! 2. **presence in the new base** — if the current base already ships a path,
//!    the base's copy wins by definition (that is the point of migrating), so
//!    it is never carried across. This is also what makes the extraction's
//!    never-clobber guarantee cheap: the payload does not even contain the
//!    conflicting files.
//!
//! `pip3 list` is likewise a liar on Ubuntu — its apparent extras are
//! `dist-packages` installed by apt — so Python packages are covered by the apt
//! delta rather than by a pip diff.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use bollard::container::{
    Config, CreateContainerOptions, LogOutput, LogsOptions, RemoveContainerOptions,
    StartContainerOptions, WaitContainerOptions,
};
use bollard::image::TagImageOptions;
use bollard::models::HostConfig;
use futures_util::StreamExt;

use super::client::get_docker;
use crate::models::{ProjectPath, UnpreservedData};

// ─────────────────────────────────────────────────────────────────────────────
// Policy constants
// ─────────────────────────────────────────────────────────────────────────────

/// Roots whose *non-package, not-in-the-base* contents are carried across
/// verbatim.
///
/// `/usr/local` is narrowed to the four directories that hold executables and
/// data rather than configuration, per the migration design. `/workspace` is
/// here because loose files at the workspace root live in the container's
/// writable layer — they are not on any bind mount and are genuinely lost today
/// when a container is recreated from a different image.
pub const COPY_ROOTS: &[&str] = &[
    "/usr/local/bin",
    "/usr/local/sbin",
    "/usr/local/lib",
    "/usr/local/share",
    "/opt",
    "/srv",
    "/workspace",
];

/// Subtrees never copied even though they sit under a [`COPY_ROOTS`] entry.
///
/// Both are the *base image's own* content, shipped by the Dockerfile. Copying
/// them forward would pin the new base to the old base's version of them, which
/// is the exact failure migration exists to fix. (The presence-in-base filter
/// would already catch them; naming them is cheap insurance against a base that
/// relocates one.)
pub const COPY_EXCLUSIONS: &[&str] = &["/usr/local/aws-cli", "/opt/mission-control"];

/// Roots the filesystem manifest walks. Wider than [`COPY_ROOTS`] so the
/// manifest stays useful for debugging; [`compute_verbatim_paths`] applies the
/// narrower policy.
///
/// [`DATA_ROOTS`] are in here for a different reason: they are never copied,
/// but they *are* destroyed by the container swap, so the walk has to see them
/// in order to warn about them.
pub const MANIFEST_ROOTS: &[&str] = &[
    "/usr/local",
    "/opt",
    "/srv",
    "/workspace",
    "/var/lib",
    "/var/www",
];

/// Roots holding **state a base-image swap destroys and no replay can put
/// back**. Reported by [`unpreserved_data`], never copied.
///
/// A container running Postgres, MySQL, Redis or nginx keeps its actual data in
/// `/var/lib/<service>` or `/var/www`. Replaying the apt delta reinstalls the
/// *package* onto the new base and gets an empty data directory back — the
/// database is gone. That is worse than the ordinary recreate path, which
/// creates from the project's snapshot and therefore keeps `/var` intact.
///
/// These are deliberately **not** in [`COPY_ROOTS`]. A live database's on-disk
/// files cannot be tarred out from under a running server and restored into a
/// different base's version of the same package with any confidence — a copy
/// that half-works is worse than a warning that lets the user take a proper
/// dump first. So migration's answer is disclosure, loudly, before anything is
/// touched.
pub const DATA_ROOTS: &[&str] = &["/var/lib", "/var/www"];

/// Base-image capabilities worth telling the user they are missing, as
/// `(path, human label)`.
///
/// A feature is only ever reported as missing when the **current base actually
/// ships it** and the container does not, so this table needs no maintenance
/// when a capability is dropped from the image — it simply stops appearing.
pub const FEATURE_PROBES: &[(&str, &str)] = &[
    ("/usr/bin/socat", "Auth bridge tunnel (socat)"),
    ("/usr/bin/bwrap", "Sandbox mode (bubblewrap)"),
    ("/usr/bin/cron", "Cron daemon (scheduled tasks)"),
    ("/usr/bin/jq", "JSON tooling (jq)"),
    ("/usr/bin/rg", "Fast search (ripgrep)"),
    ("/usr/bin/gh", "GitHub CLI"),
    ("/usr/bin/git", "git"),
    ("/usr/bin/docker", "Docker CLI"),
    ("/usr/bin/node", "Node.js"),
    ("/usr/bin/python3", "Python 3"),
    ("/usr/local/bin/triple-c-open", "Host browser URL relay"),
    ("/usr/local/bin/osc52-clipboard", "Clipboard bridge (OSC 52)"),
    ("/usr/local/bin/audio-shim", "Voice mode audio capture"),
    ("/usr/local/bin/triple-c-scheduler", "Scheduled tasks"),
    ("/usr/local/bin/triple-c-task-runner", "Scheduled task runner"),
    ("/usr/local/bin/triple-c-sso-refresh", "AWS SSO auto-refresh"),
    ("/opt/mission-control", "Mission Control (Flight Control)"),
    ("/usr/bin/wg", "VPN support (WireGuard tools)"),
];

/// Headroom demanded on Docker's storage backend on top of the measured
/// payload, so a migration cannot be the thing that fills the disk. The new
/// snapshot commit is a delta layer over the base (the base itself is already
/// on disk), and a 524 MB commit was measured at 25.6 s — 2 GiB is a generous
/// ceiling for that plus the replayed packages.
pub const DISK_HEADROOM_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// Label carrying the image ID of the base a container's lineage descends from.
pub const LABEL_BASE_IMAGE_ID: &str = "triple-c.base-image-id";
/// Label carrying the image this container was actually created from.
pub const LABEL_CREATE_IMAGE: &str = "triple-c.create-image";
/// Label stamped on a container created *by* a migration, so a crash between
/// the container swap and the final commit is recognisable on restart.
pub const LABEL_MIGRATION_STATE: &str = "triple-c.migration-state";
/// Value of [`LABEL_MIGRATION_STATE`] while a migration is unfinished.
pub const MIGRATION_LABEL_IN_PROGRESS: &str = "in-progress";
/// Label stamped on the short-lived probe containers [`run_throwaway`] creates.
///
/// They are removed on every path including failure, but a hard crash of the
/// app (or of Docker) between create and remove would otherwise leave a
/// container that carries no `triple-c.*` marking at all — invisible to every
/// cleanup this app has, and unattributable by hand. The label makes
/// `docker ps -a --filter label=triple-c.probe=migration` find them.
pub const LABEL_PROBE: &str = "triple-c.probe";
/// Value of [`LABEL_PROBE`] on a migration manifest/pre-flight probe container.
pub const PROBE_LABEL_MIGRATION: &str = "migration";

// ─────────────────────────────────────────────────────────────────────────────
// Manifests
// ─────────────────────────────────────────────────────────────────────────────

/// One entry from the filesystem walk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManifestEntry {
    /// `find`'s `%y`: `f` regular, `d` directory, `l` symlink, …
    pub kind: char,
    pub size: u64,
    pub path: String,
}

impl ManifestEntry {
    pub fn is_dir(&self) -> bool {
        self.kind == 'd'
    }
}

/// Everything one probe run learned about an image or a running container.
#[derive(Debug, Clone, Default)]
pub struct Manifest {
    /// Filesystem walk of [`MANIFEST_ROOTS`].
    pub paths: Vec<ManifestEntry>,
    /// Paths under those roots that `dpkg` owns.
    pub dpkg_owned: BTreeSet<String>,
    /// `apt-mark showmanual`.
    pub apt_manual: BTreeSet<String>,
    /// Globally installed npm package names (scoped names kept intact).
    pub npm_global: BTreeSet<String>,
    /// Which [`FEATURE_PROBES`] paths exist.
    pub features: BTreeSet<String>,
    /// Filesystem walk of `/etc`.
    pub etc_paths: BTreeSet<String>,
    /// `package -> version` for every installed dpkg package.
    pub dpkg_versions: BTreeMap<String, String>,
}

impl Manifest {
    /// Index of the filesystem walk, for O(log n) presence tests.
    fn path_set(&self) -> BTreeSet<&str> {
        self.paths.iter().map(|e| e.path.as_str()).collect()
    }
}

/// The shell program run inside a throwaway container (or, when the project is
/// running, inside the container itself) to produce a [`Manifest`].
///
/// Sections are separated by sentinel lines so one exec answers every question;
/// on a 5.49 GB image the whole thing takes about three seconds. Every command
/// is failure-tolerant (`2>/dev/null`, no `set -e`) because a missing `npm` or
/// an unreadable directory must degrade one section, not the run.
pub fn manifest_script() -> String {
    let feature_paths = FEATURE_PROBES
        .iter()
        .map(|(p, _)| shell_single_quote(p))
        .collect::<Vec<_>>()
        .join(" ");
    let roots = MANIFEST_ROOTS
        .iter()
        .map(|p| shell_single_quote(p))
        .collect::<Vec<_>>()
        .join(" ");
    // The dpkg grep is anchored to the manifest roots so the section stays a
    // few hundred kB instead of the ~40 MB a full ownership dump would be.
    let dpkg_filter = MANIFEST_ROOTS
        .iter()
        .map(|r| r.trim_start_matches('/'))
        .collect::<Vec<_>>()
        .join("|");
    format!(
        r#"
echo '###PATHS'
find {roots} -xdev -printf '%y\t%s\t%p\n' 2>/dev/null
echo '###DPKG'
cat /var/lib/dpkg/info/*.list 2>/dev/null | grep -E '^/({dpkg_filter})(/|$)'
echo '###APT'
apt-mark showmanual 2>/dev/null
echo '###NPM'
npm ls -g --depth=0 --parseable 2>/dev/null
echo '###FEATURES'
for p in {feature_paths}; do
  if [ -e "$p" ]; then echo "$p"; fi
done
echo '###ETC'
find /etc -xdev -printf '%y\t%s\t%p\n' 2>/dev/null
echo '###PKGVER'
dpkg-query -W -f='${{Package}}\t${{Version}}\n' 2>/dev/null
echo '###END'
exit 0
"#
    )
}

/// Parse the output of [`manifest_script`].
///
/// Unknown sections and malformed lines are skipped rather than failing: the
/// probe runs against images this build has never seen, and one odd line must
/// not cost the whole manifest.
pub fn parse_manifest(raw: &str) -> Manifest {
    let mut m = Manifest::default();
    let mut section = "";
    for line in raw.lines() {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if let Some(name) = line.strip_prefix("###") {
            section = match name {
                "PATHS" | "DPKG" | "APT" | "NPM" | "FEATURES" | "ETC" | "PKGVER" | "END" => name,
                _ => "",
            };
            continue;
        }
        if line.is_empty() {
            continue;
        }
        match section {
            "PATHS" => {
                if let Some(entry) = parse_find_line(line) {
                    m.paths.push(entry);
                }
            }
            "DPKG" => {
                m.dpkg_owned.insert(line.to_string());
            }
            "APT" => {
                m.apt_manual.insert(line.trim().to_string());
            }
            "NPM" => {
                if let Some(name) = npm_package_from_path(line) {
                    m.npm_global.insert(name);
                }
            }
            "FEATURES" => {
                m.features.insert(line.to_string());
            }
            "ETC" => {
                if let Some(entry) = parse_find_line(line) {
                    m.etc_paths.insert(entry.path);
                }
            }
            "PKGVER" => {
                if let Some((pkg, ver)) = line.split_once('\t') {
                    m.dpkg_versions.insert(pkg.to_string(), ver.to_string());
                }
            }
            _ => {}
        }
    }
    m
}

fn parse_find_line(line: &str) -> Option<ManifestEntry> {
    let mut parts = line.splitn(3, '\t');
    let kind = parts.next()?.chars().next()?;
    let size = parts.next()?.parse::<u64>().ok()?;
    let path = parts.next()?;
    if !path.starts_with('/') {
        return None;
    }
    Some(ManifestEntry {
        kind,
        size,
        path: path.to_string(),
    })
}

/// `/usr/lib/node_modules/@scope/pkg` → `@scope/pkg`.
///
/// `npm ls -g --parseable` prints the prefix directory on its first line and
/// one path per installed package after it; splitting on the *last*
/// `/node_modules/` is what keeps scoped names intact.
fn npm_package_from_path(line: &str) -> Option<String> {
    let idx = line.rfind("/node_modules/")?;
    let name = line[idx + "/node_modules/".len()..].trim();
    if name.is_empty() {
        return None;
    }
    Some(name.to_string())
}

/// Quote a string for safe interpolation into a single-quoted shell word.
fn shell_single_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r#"'\''"#))
}

// ─────────────────────────────────────────────────────────────────────────────
// Pure delta computation
// ─────────────────────────────────────────────────────────────────────────────

/// Set difference, sorted. Used for both the apt and the `npm -g` delta.
pub fn set_delta(from: &BTreeSet<String>, base: &BTreeSet<String>) -> Vec<String> {
    from.difference(base).cloned().collect()
}

/// The `/workspace/<mount_name>` targets a project's bind mounts occupy.
///
/// Everything under one of these belongs to the host filesystem and must never
/// be staged: it is not lost by a container swap, and copying a whole mounted
/// repository into a tar would be both pointless and enormous. Computed from
/// `project.paths` rather than hardcoded, because the mount names are
/// user-chosen.
pub fn bind_mount_exclusions(paths: &[ProjectPath]) -> Vec<String> {
    let mut out: Vec<String> = paths
        .iter()
        .map(|p| format!("/workspace/{}", p.mount_name))
        .collect();
    out.sort();
    out.dedup();
    out
}

/// Whether `path` is `root` itself or lives beneath it.
pub fn is_under(path: &str, root: &str) -> bool {
    path == root || path.starts_with(&format!("{}/", root))
}

/// Drop every path that already has an ancestor in the set.
///
/// Turns "one entry per file" into "one entry per newly-added subtree", which
/// is what makes both the reported list and the `tar -T` include list small
/// when someone has installed something large into `/usr/local/lib`.
pub fn prune_to_roots(paths: &BTreeSet<String>) -> Vec<String> {
    let mut kept: Vec<String> = Vec::new();
    // BTreeSet iterates lexicographically, so a parent is always visited before
    // any of its children ("/a" < "/a/b"), and checking only the last kept
    // entry is not enough — a sibling can intervene. Check all kept roots, but
    // short-circuit on the common case.
    for p in paths {
        if kept.iter().any(|k| is_under(p, k) && k != p) {
            continue;
        }
        kept.push(p.clone());
    }
    kept
}

/// The set of paths a migration would carry across verbatim.
///
/// A path qualifies when **all** of:
/// * it lives under a [`COPY_ROOTS`] entry,
/// * it is not under a [`COPY_EXCLUSIONS`] entry or a bind-mount target,
/// * neither image's dpkg database owns it,
/// * the current base image does not already have it.
///
/// The result is then pruned to subtree roots. An empty result means the copy
/// step is skipped entirely.
pub fn compute_verbatim_paths(
    from: &Manifest,
    base: &Manifest,
    bind_targets: &[String],
) -> Vec<String> {
    let base_paths = base.path_set();
    let mut candidates: BTreeSet<String> = BTreeSet::new();

    for entry in &from.paths {
        let p = entry.path.as_str();
        if !COPY_ROOTS.iter().any(|r| is_under(p, r)) {
            continue;
        }
        // A copy root itself is a container for new content, never new content.
        if COPY_ROOTS.contains(&p) {
            continue;
        }
        if COPY_EXCLUSIONS.iter().any(|x| is_under(p, x)) {
            continue;
        }
        if bind_targets.iter().any(|t| is_under(p, t)) {
            continue;
        }
        if from.dpkg_owned.contains(p) || base.dpkg_owned.contains(p) {
            continue;
        }
        if base_paths.contains(p) {
            continue;
        }
        candidates.insert(entry.path.clone());
    }

    let dirs: BTreeSet<&str> = from
        .paths
        .iter()
        .filter(|e| e.is_dir())
        .map(|e| e.path.as_str())
        .collect();

    prune_to_roots(&candidates)
        .into_iter()
        // Drop empty directory trees. Measured on a real project, these were
        // three of the five hits: `/usr/local/share/{fonts,sgml,xml}`, which a
        // package's postinst creates and dpkg does not own, so no other filter
        // catches them. They carry nothing, and replaying the packages that
        // made them recreates them anyway.
        .filter(|p| {
            !dirs.contains(p.as_str())
                || from
                    .paths
                    .iter()
                    .any(|e| !e.is_dir() && is_under(&e.path, p))
        })
        .collect()
}

/// Total on-disk size of a verbatim set, for the pre-flight disk estimate.
pub fn verbatim_payload_bytes(from: &Manifest, verbatim: &[String]) -> u64 {
    from.paths
        .iter()
        .filter(|e| !e.is_dir())
        .filter(|e| verbatim.iter().any(|root| is_under(&e.path, root)))
        .map(|e| e.size)
        .sum()
}

/// The reporting unit under a [`DATA_ROOTS`] entry: the first path component
/// below the root, e.g. `/var/lib/postgresql`. Directory-level, because that is
/// the granularity a user can actually act on ("dump this database"), and
/// because a per-file list of a Postgres cluster would be thousands of lines.
fn data_unit(path: &str) -> Option<String> {
    for root in DATA_ROOTS {
        let prefix = format!("{}/", root);
        if let Some(rest) = path.strip_prefix(&prefix) {
            let first = rest.split('/').next()?;
            if first.is_empty() {
                return None;
            }
            return Some(format!("{}/{}", root, first));
        }
    }
    None
}

/// Data-bearing subtrees under [`DATA_ROOTS`] that the migration will destroy
/// and cannot restore, with the size and file count of what is at risk.
///
/// A subtree qualifies when **all** of:
/// * it is a first-level directory under a [`DATA_ROOTS`] entry,
/// * the current base image does not have that directory **at all** — if the
///   base ships it, it is the base's own machinery (`/var/lib/apt`,
///   `/var/lib/dpkg`, `/var/lib/systemd`, …) and the base's copy is the right
///   one, exactly as for `/etc`,
/// * it contains at least one regular file that neither image's dpkg database
///   owns — a package's own scaffolding is recreated by the apt replay, the
///   data written into it is not.
///
/// That pair of filters is what keeps this quiet on an ordinary container and
/// loud on one running a database: `/var/lib/postgresql` is absent from the
/// base and full of unowned files, while `/var/lib/apt/lists` is present in the
/// base and never reported.
pub fn unpreserved_data(from: &Manifest, base: &Manifest) -> Vec<UnpreservedData> {
    let base_paths = base.path_set();
    let mut acc: BTreeMap<String, (u64, u32)> = BTreeMap::new();

    for entry in &from.paths {
        let Some(unit) = data_unit(&entry.path) else {
            continue;
        };
        if base_paths.contains(unit.as_str()) {
            continue;
        }
        if entry.is_dir() {
            continue;
        }
        if from.dpkg_owned.contains(&entry.path) || base.dpkg_owned.contains(&entry.path) {
            continue;
        }
        let slot = acc.entry(unit).or_insert((0, 0));
        slot.0 = slot.0.saturating_add(entry.size);
        slot.1 += 1;
    }

    acc.into_iter()
        .map(|(path, (bytes, file_count))| UnpreservedData {
            path,
            bytes,
            file_count,
        })
        .collect()
}

/// Base-image capabilities the container does not have, as
/// `(concrete paths, human labels)`.
///
/// Only paths the base actually ships are considered, so this can never
/// recommend migrating to gain something the new base does not have either.
pub fn missing_features(from: &Manifest, base: &Manifest) -> (Vec<String>, Vec<String>) {
    let mut paths = Vec::new();
    let mut labels = Vec::new();
    for (path, label) in FEATURE_PROBES {
        if base.features.contains(*path) && !from.features.contains(*path) {
            paths.push((*path).to_string());
            labels.push((*label).to_string());
        }
    }
    (paths, labels)
}

/// How many dpkg packages the current base carries at a version the container
/// does not have — either a different version, or a package the container is
/// missing entirely.
///
/// A rough drift measure, deliberately not a claim that every one is *newer*:
/// comparing Debian version strings properly needs `dpkg --compare-versions`,
/// and the number exists to answer "is this container far behind?", which
/// inequality answers just as well.
pub fn outdated_package_count(from: &Manifest, base: &Manifest) -> u32 {
    base.dpkg_versions
        .iter()
        .filter(|(pkg, base_ver)| from.dpkg_versions.get(*pkg) != Some(*base_ver))
        .count() as u32
}

/// `/etc` paths the base has that the container does not, and vice versa.
///
/// **Reported, never copied.** The snapshot lineage carries
/// `/etc/apt/sources.list.d/nodesource.sources` where the current base has
/// `nodesource.list`; copying `/etc` wholesale would leave both in place and
/// every `apt-get update` would fail on a duplicate-source conflict. Since
/// `/etc` is also where the base's own configuration lives, the base's copy is
/// always the right one.
pub fn etc_deltas(from: &Manifest, base: &Manifest) -> (Vec<String>, Vec<String>) {
    let only_in_container: Vec<String> = from
        .etc_paths
        .difference(&base.etc_paths)
        .cloned()
        .collect();
    let only_in_base: Vec<String> = base
        .etc_paths
        .difference(&from.etc_paths)
        .cloned()
        .collect();
    (only_in_container, only_in_base)
}

/// The `tar` member names for a verbatim set: absolute paths made relative to
/// `/`, so the archive extracts with `-C /`.
pub fn tar_member_names(verbatim: &[String]) -> Vec<String> {
    verbatim
        .iter()
        .map(|p| p.trim_start_matches('/').to_string())
        .filter(|p| !p.is_empty())
        .collect()
}

// ─────────────────────────────────────────────────────────────────────────────
// Crash-recovery state machine
// ─────────────────────────────────────────────────────────────────────────────

/// What to do about a migration state found on startup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Recovery {
    /// Nothing was in flight.
    None,
    /// The crash happened before the container was swapped. `:latest` still
    /// points at the old lineage and `start_project_container` will recreate
    /// from it unaided, so the only work is to clear the record.
    SelfHeal,
    /// The container was swapped but the migration never finished. The user
    /// must choose: resume, or roll back.
    OfferResumeOrRollback,
    /// The migration finished. The user must choose: confirm, or roll back.
    OfferConfirmOrRollback,
}

/// Decide the recovery action from the two independent signals.
///
/// The host-side state file says a migration was in flight; the container's
/// `triple-c.migration-state` label says whether the *swap* actually happened.
/// Neither alone is sufficient:
///
/// * state file but no labelled container → the crash predates the swap
///   (or the swapped container never got created), and everything self-heals.
/// * labelled container but no state file → a stale label from a migration that
///   was already confirmed; the label rides the final commit into the snapshot
///   image, so it can outlive its migration. It must not trigger anything.
///
/// `phase` is [`crate::models::MigrationState::phase`].
pub fn decide_recovery(phase: Option<&str>, container_has_in_progress_label: bool) -> Recovery {
    use crate::models::{
        MIGRATION_PHASE_AWAITING, MIGRATION_PHASE_INTERRUPTED, MIGRATION_PHASE_IN_PROGRESS,
    };
    match phase {
        None => Recovery::None,
        Some(MIGRATION_PHASE_AWAITING) => Recovery::OfferConfirmOrRollback,
        Some(MIGRATION_PHASE_IN_PROGRESS) | Some(MIGRATION_PHASE_INTERRUPTED) => {
            if container_has_in_progress_label {
                Recovery::OfferResumeOrRollback
            } else {
                Recovery::SelfHeal
            }
        }
        // An unrecognised phase is a record we cannot reason about. Treat it
        // like a finished migration awaiting a decision rather than silently
        // discarding it: the destructive option must always be the user's.
        Some(_) => Recovery::OfferConfirmOrRollback,
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Docker operations
// ─────────────────────────────────────────────────────────────────────────────

/// A throwaway container's stdout plus its exit code.
pub struct ThrowawayResult {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i64,
}

/// Run a shell program in a short-lived container off `image` and collect its
/// output.
///
/// The image's `ENTRYPOINT` is overridden — the Triple-C image's entrypoint
/// ends in `sleep infinity`, so leaving it in place would hang forever. The
/// container is removed on every path, including failure.
pub async fn run_throwaway(image: &str, script: &str) -> Result<ThrowawayResult, String> {
    let docker = get_docker()?;

    let config = Config {
        image: Some(image.to_string()),
        entrypoint: Some(vec!["/bin/sh".to_string()]),
        cmd: Some(vec!["-c".to_string(), script.to_string()]),
        user: Some("root".to_string()),
        working_dir: Some("/".to_string()),
        tty: Some(false),
        // Written explicitly rather than inherited: a probe container that
        // outlives a crash has to be findable, and nothing else in the app
        // labels these.
        labels: Some(HashMap::from([(
            LABEL_PROBE.to_string(),
            PROBE_LABEL_MIGRATION.to_string(),
        )])),
        host_config: Some(HostConfig {
            // No mounts on purpose: this must observe the *image*, not the
            // project's volumes, which are exactly the state migration does
            // not need to move.
            auto_remove: Some(false),
            ..Default::default()
        }),
        ..Default::default()
    };

    let created = docker
        .create_container(
            None::<CreateContainerOptions<String>>,
            config,
        )
        .await
        .map_err(|e| format!("Failed to create probe container for {}: {}", image, e))?;
    let id = created.id;

    let result = run_throwaway_inner(&id).await;

    if let Err(e) = docker
        .remove_container(
            &id,
            Some(RemoveContainerOptions {
                force: true,
                v: true,
                ..Default::default()
            }),
        )
        .await
    {
        log::warn!("Failed to remove probe container {}: {}", id, e);
    }

    result
}

async fn run_throwaway_inner(id: &str) -> Result<ThrowawayResult, String> {
    let docker = get_docker()?;

    docker
        .start_container(id, None::<StartContainerOptions<String>>)
        .await
        .map_err(|e| format!("Failed to start probe container: {}", e))?;

    let mut wait = docker.wait_container(
        id,
        Some(WaitContainerOptions {
            condition: "not-running",
        }),
    );
    let mut exit_code: i64 = -1;
    while let Some(msg) = wait.next().await {
        match msg {
            Ok(r) => exit_code = r.status_code,
            // A non-zero exit is delivered as an Err by bollard; the status
            // code is still what we want, and the logs below carry the detail.
            Err(bollard::errors::Error::DockerContainerWaitError { code, .. }) => exit_code = code,
            Err(e) => return Err(format!("Probe container wait failed: {}", e)),
        }
    }

    let mut logs = docker.logs(
        id,
        Some(LogsOptions::<String> {
            stdout: true,
            stderr: true,
            follow: false,
            ..Default::default()
        }),
    );
    let mut stdout = String::new();
    let mut stderr = String::new();
    while let Some(chunk) = logs.next().await {
        match chunk {
            Ok(LogOutput::StdOut { message }) => {
                stdout.push_str(&String::from_utf8_lossy(&message))
            }
            Ok(LogOutput::StdErr { message }) => {
                stderr.push_str(&String::from_utf8_lossy(&message))
            }
            Ok(other) => stdout.push_str(&String::from_utf8_lossy(&other.into_bytes())),
            Err(e) => return Err(format!("Probe container log stream failed: {}", e)),
        }
    }

    Ok(ThrowawayResult {
        stdout,
        stderr,
        exit_code,
    })
}

/// Capture a [`Manifest`] from an image, via a throwaway container.
pub async fn manifest_from_image(image: &str) -> Result<Manifest, String> {
    let out = run_throwaway(image, &manifest_script()).await?;
    if !out.stdout.contains("###END") {
        return Err(format!(
            "Probe of image {} did not complete (exit {}){}",
            image,
            out.exit_code,
            if out.stderr.trim().is_empty() {
                String::new()
            } else {
                format!(": {}", out.stderr.trim())
            }
        ));
    }
    Ok(parse_manifest(&out.stdout))
}

/// Capture a [`Manifest`] from a *running* container.
///
/// Preferred over [`manifest_from_image`] for the "from" side whenever the
/// project is up: the snapshot image can lag the container by everything
/// installed since the last commit, and a verbatim set computed from a stale
/// manifest would silently fail to carry that work across.
pub async fn manifest_from_container(container_id: &str) -> Result<Manifest, String> {
    let (out, code) = super::exec::exec_oneshot_as(
        container_id,
        "root",
        vec!["/bin/sh".to_string(), "-c".to_string(), manifest_script()],
        Vec::new(),
    )
    .await?;
    if !out.contains("###END") {
        return Err(format!(
            "Probe of the running container did not complete (exit {})",
            code
        ));
    }
    Ok(parse_manifest(&out))
}

/// The image ID (`sha256:…`) of a local image, or `None` if it is not present.
///
/// Deliberately the **ID**, not a repo digest: locally built images and custom
/// images have no `RepoDigests` entry at all, so a digest-based identity would
/// silently be empty for exactly the users most likely to change their base.
pub async fn image_id(image: &str) -> Result<Option<String>, String> {
    let docker = get_docker()?;
    match docker.inspect_image(image).await {
        Ok(info) => Ok(info.id.filter(|s| !s.is_empty())),
        Err(bollard::errors::Error::DockerResponseServerError {
            status_code: 404, ..
        }) => Ok(None),
        Err(e) => Err(format!("Failed to inspect image {}: {}", image, e)),
    }
}

/// An image's labels, or an empty map when it does not exist.
pub async fn image_labels(image: &str) -> HashMap<String, String> {
    let docker = match get_docker() {
        Ok(d) => d,
        Err(_) => return HashMap::new(),
    };
    match docker.inspect_image(image).await {
        Ok(info) => info
            .config
            .and_then(|c| c.labels)
            .unwrap_or_default(),
        Err(_) => HashMap::new(),
    }
}

/// An image's `Created` timestamp, if it exists.
pub async fn image_created(image: &str) -> Option<String> {
    let docker = get_docker().ok()?;
    docker.inspect_image(image).await.ok().and_then(|i| i.created)
}

/// Point a second tag at an existing image.
///
/// Free in both time and space — a 5.49 GB image was measured at 0.036 s and
/// 0 bytes — which is what makes keeping a rollback pin the default-safe
/// choice. (The *image* it pins is not free: snapshots share only 3 of 31
/// layers with the current base, so a retained rollback holds roughly its full
/// size on disk. That is the trade `MigrationOptions::keep_rollback` exposes.)
pub async fn tag_image(source: &str, repo: &str, tag: &str) -> Result<(), String> {
    let docker = get_docker()?;
    docker
        .tag_image(source, Some(TagImageOptions { repo, tag }))
        .await
        .map_err(|e| format!("Failed to tag {} as {}:{}: {}", source, repo, tag, e))
}

/// Remove an image tag. Missing is success — a rollback tag that is already
/// gone is the state the caller wanted.
pub async fn untag_image(reference: &str) -> Result<(), String> {
    let docker = get_docker()?;
    match docker
        .remove_image(
            reference,
            Some(bollard::image::RemoveImageOptions {
                force: false,
                noprune: false,
            }),
            None,
        )
        .await
    {
        Ok(_) => Ok(()),
        Err(bollard::errors::Error::DockerResponseServerError {
            status_code: 404, ..
        }) => Ok(()),
        Err(e) => Err(format!("Failed to remove image tag {}: {}", reference, e)),
    }
}

/// A pre-migration rollback tag for a project's snapshot repo.
pub fn rollback_tag(now: &chrono::DateTime<chrono::Utc>) -> String {
    format!("pre-migration-{}", now.format("%Y%m%d-%H%M%S"))
}

/// Split `repo:tag` into its parts, defaulting the tag to `latest`.
pub fn split_image_ref(image: &str) -> (String, String) {
    match image.rsplit_once(':') {
        // A colon in the *registry host* part is a port, not a tag.
        Some((repo, tag)) if !tag.contains('/') => (repo.to_string(), tag.to_string()),
        _ => (image.to_string(), "latest".to_string()),
    }
}

/// Pre-flight environment checks, run against the **new base** in a throwaway
/// container before anything destructive happens.
pub struct PreflightEnvironment {
    /// `apt-get update` succeeded, so package replay has a chance.
    pub network_ok: bool,
    pub network_detail: String,
    /// Bytes available on Docker's storage backend.
    ///
    /// Measured with `df` **inside a container**, not with a host `statvfs`:
    /// on Windows the Docker root lives inside the WSL2 VM and is not a path
    /// the Tauri process can stat at all.
    pub available_bytes: u64,
}

/// Run the network and disk pre-flight checks in one throwaway container.
pub async fn preflight_environment(base_image: &str) -> Result<PreflightEnvironment, String> {
    let script = r#"
echo '###DF'
df -P / | tail -n 1
echo '###NET'
if apt-get -o Acquire::Retries=2 update >/dev/null 2>&1; then
  echo ok
else
  echo failed
fi
echo '###END'
exit 0
"#;
    let out = run_throwaway(base_image, script).await?;
    if !out.stdout.contains("###END") {
        return Err(format!(
            "Pre-flight probe of {} did not complete (exit {}){}",
            base_image,
            out.exit_code,
            if out.stderr.trim().is_empty() {
                String::new()
            } else {
                format!(": {}", out.stderr.trim())
            }
        ));
    }
    Ok(parse_preflight(&out.stdout))
}

/// Parse the pre-flight probe output. `df -P` reports 1024-byte blocks.
pub fn parse_preflight(raw: &str) -> PreflightEnvironment {
    let mut section = "";
    let mut available_bytes = 0u64;
    let mut network_ok = false;
    let mut network_detail = "not checked".to_string();
    for line in raw.lines() {
        if let Some(name) = line.strip_prefix("###") {
            section = name;
            continue;
        }
        match section {
            "DF" => {
                // Filesystem 1024-blocks Used Available Capacity Mounted-on
                let cols: Vec<&str> = line.split_whitespace().collect();
                if cols.len() >= 4 {
                    if let Ok(kb) = cols[cols.len() - 3].parse::<u64>() {
                        available_bytes = kb.saturating_mul(1024);
                    }
                }
            }
            "NET" => {
                if line.trim() == "ok" {
                    network_ok = true;
                    network_detail = "apt-get update succeeded".to_string();
                } else if line.trim() == "failed" {
                    network_ok = false;
                    network_detail =
                        "apt-get update failed — package replay will be skipped".to_string();
                }
            }
            _ => {}
        }
    }
    PreflightEnvironment {
        network_ok,
        network_detail,
        available_bytes,
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{
        MIGRATION_PHASE_AWAITING, MIGRATION_PHASE_INTERRUPTED, MIGRATION_PHASE_IN_PROGRESS,
    };

    fn manifest(paths: &[(char, u64, &str)], dpkg: &[&str], base_features: &[&str]) -> Manifest {
        Manifest {
            paths: paths
                .iter()
                .map(|(k, s, p)| ManifestEntry {
                    kind: *k,
                    size: *s,
                    path: p.to_string(),
                })
                .collect(),
            dpkg_owned: dpkg.iter().map(|s| s.to_string()).collect(),
            features: base_features.iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        }
    }

    fn strs(v: &[&str]) -> BTreeSet<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    // ── Manifest parsing ────────────────────────────────────────────────────

    #[test]
    fn the_manifest_parser_reads_every_section() {
        let raw = "###PATHS\n\
                   d\t4096\t/usr/local/bin\n\
                   f\t128\t/usr/local/bin/mytool\n\
                   ###DPKG\n\
                   /usr/local/lib/pkgfile\n\
                   ###APT\n\
                   socat\n\
                   postgresql-client\n\
                   ###NPM\n\
                   /usr/lib/node_modules\n\
                   /usr/lib/node_modules/pnpm\n\
                   /usr/lib/node_modules/@scope/tool\n\
                   ###FEATURES\n\
                   /usr/bin/socat\n\
                   ###ETC\n\
                   f\t10\t/etc/hosts\n\
                   ###PKGVER\n\
                   curl\t8.5.0-2ubuntu10.6\n\
                   ###END\n";
        let m = parse_manifest(raw);
        assert_eq!(m.paths.len(), 2);
        assert_eq!(m.paths[1].size, 128);
        assert!(m.dpkg_owned.contains("/usr/local/lib/pkgfile"));
        assert_eq!(m.apt_manual, strs(&["socat", "postgresql-client"]));
        // The prefix line has no `/node_modules/` segment and is dropped;
        // the scoped name survives intact.
        assert_eq!(m.npm_global, strs(&["pnpm", "@scope/tool"]));
        assert!(m.features.contains("/usr/bin/socat"));
        assert!(m.etc_paths.contains("/etc/hosts"));
        assert_eq!(
            m.dpkg_versions.get("curl").map(String::as_str),
            Some("8.5.0-2ubuntu10.6")
        );
    }

    #[test]
    fn malformed_manifest_lines_are_skipped_not_fatal() {
        let raw = "###PATHS\ngarbage\nf\tnotanumber\t/x\nf\t1\trelative/path\nf\t2\t/ok\n###END\n";
        let m = parse_manifest(raw);
        assert_eq!(m.paths.len(), 1);
        assert_eq!(m.paths[0].path, "/ok");
    }

    #[test]
    fn unknown_sections_do_not_leak_into_the_previous_one() {
        let raw = "###APT\nsocat\n###SOMETHINGNEW\nnoise\nmore-noise\n###END\n";
        let m = parse_manifest(raw);
        assert_eq!(m.apt_manual, strs(&["socat"]));
    }

    // ── Deltas ──────────────────────────────────────────────────────────────

    #[test]
    fn the_apt_delta_is_the_containers_manual_set_minus_the_bases() {
        let from = strs(&["socat", "postgresql-client", "redis-tools", "curl"]);
        let base = strs(&["socat", "curl", "git"]);
        assert_eq!(
            set_delta(&from, &base),
            vec!["postgresql-client".to_string(), "redis-tools".to_string()]
        );
        // A base that gained packages does not produce a negative delta.
        assert!(set_delta(&base, &from).contains(&"git".to_string()));
    }

    #[test]
    fn an_identical_package_set_produces_no_delta() {
        let s = strs(&["a", "b"]);
        assert!(set_delta(&s, &s).is_empty());
    }

    #[test]
    fn outdated_packages_count_version_differences_and_absences() {
        let mut from = Manifest::default();
        from.dpkg_versions.insert("curl".into(), "8.5.0-1".into());
        from.dpkg_versions.insert("git".into(), "2.43.0".into());
        from.dpkg_versions.insert("gone".into(), "1.0".into());
        let mut base = Manifest::default();
        base.dpkg_versions.insert("curl".into(), "8.5.0-2".into()); // newer
        base.dpkg_versions.insert("git".into(), "2.43.0".into()); // same
        base.dpkg_versions.insert("brandnew".into(), "1.0".into()); // absent
        // curl differs + brandnew is missing = 2. `gone` is only in the
        // container and is not drift against the base.
        assert_eq!(outdated_package_count(&from, &base), 2);
    }

    // ── dpkg ownership filter ───────────────────────────────────────────────

    #[test]
    fn dpkg_owned_paths_are_never_treated_as_user_authored() {
        // The real-world failure this guards: a path that exists only in the
        // container looks user-authored until you notice a package owns it.
        let from = manifest(
            &[
                ('f', 10, "/usr/local/lib/libowned.so"),
                ('f', 10, "/usr/local/bin/mytool"),
            ],
            &["/usr/local/lib/libowned.so"],
            &[],
        );
        let base = Manifest::default();
        assert_eq!(
            compute_verbatim_paths(&from, &base, &[]),
            vec!["/usr/local/bin/mytool".to_string()]
        );
    }

    #[test]
    fn ownership_recorded_only_in_the_base_still_filters() {
        // A package that moved into the base since the snapshot was taken owns
        // the path there but not in the container's older dpkg database.
        let from = manifest(&[('f', 10, "/opt/tool/bin/x")], &[], &[]);
        let mut base = Manifest::default();
        base.dpkg_owned.insert("/opt/tool/bin/x".to_string());
        assert!(compute_verbatim_paths(&from, &base, &[]).is_empty());
    }

    // ── Verbatim set ────────────────────────────────────────────────────────

    #[test]
    fn the_bases_own_content_is_never_copied_forward() {
        // /usr/local/aws-cli and /opt/mission-control are shipped by the
        // Dockerfile. Carrying the old copies over would pin the new base to
        // the old base's versions — the exact thing migration fixes.
        let from = manifest(
            &[
                ('d', 4096, "/usr/local/aws-cli"),
                ('f', 10, "/usr/local/aws-cli/v2/current/bin/aws"),
                ('d', 4096, "/opt/mission-control"),
                ('f', 10, "/opt/mission-control/README.md"),
                ('d', 4096, "/opt/mine"),
                ('f', 10, "/opt/mine/keep.txt"),
            ],
            &[],
            &[],
        );
        assert_eq!(
            compute_verbatim_paths(&from, &Manifest::default(), &[]),
            vec!["/opt/mine".to_string()]
        );
    }

    #[test]
    fn a_path_the_new_base_already_has_is_left_to_the_base() {
        let from = manifest(
            &[
                ('f', 10, "/usr/local/bin/triple-c-open"),
                ('f', 10, "/usr/local/bin/mytool"),
            ],
            &[],
            &[],
        );
        let base = manifest(&[('f', 20, "/usr/local/bin/triple-c-open")], &[], &[]);
        assert_eq!(
            compute_verbatim_paths(&from, &base, &[]),
            vec!["/usr/local/bin/mytool".to_string()]
        );
    }

    #[test]
    fn copy_roots_are_narrower_than_the_manifest_roots() {
        // /usr/local/etc is walked by the manifest but is not a copy root:
        // configuration is the base's to own.
        let from = manifest(
            &[
                ('f', 10, "/usr/local/etc/some.conf"),
                ('f', 10, "/usr/local/bin/mytool"),
            ],
            &[],
            &[],
        );
        assert_eq!(
            compute_verbatim_paths(&from, &Manifest::default(), &[]),
            vec!["/usr/local/bin/mytool".to_string()]
        );
    }

    #[test]
    fn empty_directory_trees_are_not_carried_across() {
        // Measured on a real project: /usr/local/share/{fonts,sgml,xml} exist
        // in the snapshot, do not exist in the current base, and are owned by
        // no package — a postinst made them. They carry nothing.
        let from = manifest(
            &[
                ('d', 4096, "/usr/local/share/fonts"),
                ('d', 4096, "/usr/local/share/sgml"),
                ('d', 4096, "/usr/local/share/sgml/nested"),
                ('d', 4096, "/opt/real"),
                ('f', 10, "/opt/real/thing"),
            ],
            &[],
            &[],
        );
        assert_eq!(
            compute_verbatim_paths(&from, &Manifest::default(), &[]),
            vec!["/opt/real".to_string()]
        );
    }

    #[test]
    fn an_empty_verbatim_set_is_the_normal_case() {
        // The measured reality: essentially nothing under these roots is
        // user-authored, so the copy step must be skippable.
        let from = manifest(&[('d', 4096, "/usr/local/bin"), ('d', 4096, "/opt")], &[], &[]);
        assert!(compute_verbatim_paths(&from, &Manifest::default(), &[]).is_empty());
    }

    #[test]
    fn subtrees_are_pruned_to_their_root() {
        let from = manifest(
            &[
                ('d', 4096, "/opt/mytool"),
                ('d', 4096, "/opt/mytool/bin"),
                ('f', 100, "/opt/mytool/bin/run"),
                ('f', 100, "/opt/mytool/LICENSE"),
                ('f', 100, "/srv/other.txt"),
            ],
            &[],
            &[],
        );
        assert_eq!(
            compute_verbatim_paths(&from, &Manifest::default(), &[]),
            vec!["/opt/mytool".to_string(), "/srv/other.txt".to_string()]
        );
    }

    #[test]
    fn pruning_keeps_siblings_that_share_a_name_prefix() {
        // "/opt/tool2" starts with "/opt/tool" as a *string* but is not under
        // it as a *path*.
        let set = strs(&["/opt/tool", "/opt/tool2", "/opt/tool/inner"]);
        assert_eq!(
            prune_to_roots(&set),
            vec!["/opt/tool".to_string(), "/opt/tool2".to_string()]
        );
    }

    #[test]
    fn payload_size_sums_files_under_the_pruned_roots_only() {
        let from = manifest(
            &[
                ('d', 4096, "/opt/mytool"),
                ('f', 100, "/opt/mytool/a"),
                ('f', 200, "/opt/mytool/b"),
                ('f', 999, "/opt/mission-control/big"),
            ],
            &[],
            &[],
        );
        let verbatim = compute_verbatim_paths(&from, &Manifest::default(), &[]);
        // Directories contribute their inode size on disk, not their contents;
        // counting them would double-count. Excluded subtrees contribute zero.
        assert_eq!(verbatim_payload_bytes(&from, &verbatim), 300);
    }

    // ── Data that migration destroys and cannot restore ─────────────────────

    #[test]
    fn a_database_under_var_lib_is_reported_because_nothing_replays_it() {
        // The regression this exists for: replaying `postgresql` onto the new
        // base reinstalls the package and gets an empty cluster. The ordinary
        // recreate path keeps /var because it creates from the snapshot, so a
        // silent migration would be *more* destructive than the thing it
        // replaces.
        let from = manifest(
            &[
                ('d', 4096, "/var/lib/postgresql"),
                ('d', 4096, "/var/lib/postgresql/16/main"),
                ('f', 8192, "/var/lib/postgresql/16/main/PG_VERSION"),
                ('f', 1024, "/var/lib/postgresql/16/main/base/1/2"),
                ('d', 4096, "/var/www"),
                ('d', 4096, "/var/www/site"),
                ('f', 500, "/var/www/site/index.html"),
            ],
            &[],
            &[],
        );
        let got = unpreserved_data(&from, &Manifest::default());
        assert_eq!(
            got.iter().map(|d| d.path.as_str()).collect::<Vec<_>>(),
            vec!["/var/lib/postgresql", "/var/www/site"]
        );
        assert_eq!(got[0].bytes, 9216);
        assert_eq!(got[0].file_count, 2);
        // And it is emphatically not in the copy set — reporting is the whole
        // answer here, not a half-working copy of a live database.
        assert!(!COPY_ROOTS.iter().any(|r| is_under("/var/lib/postgresql", r)));
    }

    #[test]
    fn package_machinery_under_var_is_never_reported_as_data_at_risk() {
        // /var/lib/apt exists in the base too, so it is the base's to own —
        // the same rule /etc gets. Reporting apt's lists would bury the one
        // line that matters under noise on every single migration.
        let from = manifest(
            &[
                ('d', 4096, "/var/lib/apt"),
                ('f', 900_000, "/var/lib/apt/lists/some.mirror_InRelease"),
                ('d', 4096, "/var/lib/dpkg"),
                ('f', 4096, "/var/lib/dpkg/status"),
            ],
            &[],
            &[],
        );
        let base = manifest(
            &[
                ('d', 4096, "/var/lib/apt"),
                ('d', 4096, "/var/lib/dpkg"),
                ('f', 4096, "/var/lib/dpkg/status"),
            ],
            &[],
            &[],
        );
        assert!(unpreserved_data(&from, &base).is_empty());
    }

    #[test]
    fn a_packages_own_scaffolding_under_var_is_not_data() {
        // nginx-common ships /var/www/html/index.nginx-debian.html. The apt
        // replay puts that back; only what the user wrote is at risk.
        let from = manifest(
            &[
                ('d', 4096, "/var/www/html"),
                ('f', 612, "/var/www/html/index.nginx-debian.html"),
            ],
            &["/var/www/html/index.nginx-debian.html"],
            &[],
        );
        assert!(unpreserved_data(&from, &Manifest::default()).is_empty());
    }

    #[test]
    fn data_is_reported_per_directory_not_per_file() {
        assert_eq!(
            data_unit("/var/lib/mysql/ibdata1").as_deref(),
            Some("/var/lib/mysql")
        );
        // A first-level directory is its own unit.
        assert_eq!(
            data_unit("/var/lib/mysql").as_deref(),
            Some("/var/lib/mysql")
        );
        // The root itself is not: it exists in every image.
        assert_eq!(data_unit("/var/lib").as_deref(), None);
        assert_eq!(data_unit("/var/www").as_deref(), None);
        assert_eq!(data_unit("/usr/local/bin/tool"), None);
    }

    // ── Bind-mount exclusion ────────────────────────────────────────────────

    fn pp(mount: &str) -> ProjectPath {
        ProjectPath {
            host_path: format!("/host/{}", mount),
            mount_name: mount.to_string(),
        }
    }

    #[test]
    fn bind_mount_targets_are_derived_from_the_projects_own_mount_names() {
        let paths = vec![pp("repo"), pp("docs"), pp("repo")];
        assert_eq!(
            bind_mount_exclusions(&paths),
            vec!["/workspace/docs".to_string(), "/workspace/repo".to_string()]
        );
        assert!(bind_mount_exclusions(&[]).is_empty());
    }

    #[test]
    fn workspace_content_on_a_bind_mount_is_excluded_but_loose_files_are_not() {
        // The whole reason /workspace is a copy root: `scratch.md` at the
        // workspace root is in the writable layer and is lost today.
        let from = manifest(
            &[
                ('d', 4096, "/workspace/repo"),
                ('f', 10, "/workspace/repo/src/main.rs"),
                ('f', 10, "/workspace/scratch.md"),
                ('d', 4096, "/workspace/notes"),
                ('f', 10, "/workspace/notes/todo.txt"),
            ],
            &[],
            &[],
        );
        let excl = bind_mount_exclusions(&[pp("repo")]);
        assert_eq!(
            compute_verbatim_paths(&from, &Manifest::default(), &excl),
            vec![
                "/workspace/notes".to_string(),
                "/workspace/scratch.md".to_string()
            ]
        );
    }

    #[test]
    fn a_mount_name_that_prefixes_another_does_not_over_exclude() {
        let from = manifest(
            &[
                ('d', 4096, "/workspace/app"),
                ('f', 10, "/workspace/app/x"),
                ('d', 4096, "/workspace/app-notes"),
                ('f', 10, "/workspace/app-notes/y"),
            ],
            &[],
            &[],
        );
        let excl = bind_mount_exclusions(&[pp("app")]);
        assert_eq!(
            compute_verbatim_paths(&from, &Manifest::default(), &excl),
            vec!["/workspace/app-notes".to_string()]
        );
    }

    #[test]
    fn tar_member_names_are_relative_so_the_archive_extracts_at_root() {
        assert_eq!(
            tar_member_names(&["/opt/mytool".to_string(), "/srv".to_string()]),
            vec!["opt/mytool".to_string(), "srv".to_string()]
        );
        assert!(tar_member_names(&["/".to_string()]).is_empty());
    }

    // ── Missing features ────────────────────────────────────────────────────

    #[test]
    fn a_feature_is_missing_only_when_the_new_base_actually_has_it() {
        let from = manifest(&[], &[], &["/usr/bin/jq"]);
        let base = manifest(&[], &[], &["/usr/bin/jq", "/usr/bin/socat"]);
        let (paths, labels) = missing_features(&from, &base);
        assert_eq!(paths, vec!["/usr/bin/socat".to_string()]);
        assert_eq!(labels, vec!["Auth bridge tunnel (socat)".to_string()]);

        // A capability the base dropped is never advertised as a reason to
        // migrate, even though the container "differs" from the base.
        let (paths, _) = missing_features(&base, &from);
        assert!(paths.is_empty());
    }

    // ── /etc ────────────────────────────────────────────────────────────────

    #[test]
    fn etc_deltas_surface_the_nodesource_rename_rather_than_copying_it() {
        let mut from = Manifest::default();
        from.etc_paths
            .insert("/etc/apt/sources.list.d/nodesource.sources".into());
        let mut base = Manifest::default();
        base.etc_paths
            .insert("/etc/apt/sources.list.d/nodesource.list".into());
        let (only_container, only_base) = etc_deltas(&from, &base);
        assert_eq!(
            only_container,
            vec!["/etc/apt/sources.list.d/nodesource.sources".to_string()]
        );
        assert_eq!(
            only_base,
            vec!["/etc/apt/sources.list.d/nodesource.list".to_string()]
        );
        // And /etc is not a copy root, so neither can be carried across —
        // having both would break every apt-get update with a duplicate source.
        assert!(!COPY_ROOTS.iter().any(|r| is_under("/etc/apt", r)));
    }

    // ── Crash-state machine ─────────────────────────────────────────────────

    #[test]
    fn no_state_file_means_no_recovery() {
        assert_eq!(decide_recovery(None, false), Recovery::None);
        // A stale in-progress label with no state file is a label that rode the
        // final commit into the snapshot image. It must not trigger anything.
        assert_eq!(decide_recovery(None, true), Recovery::None);
    }

    #[test]
    fn a_crash_before_the_container_swap_self_heals() {
        // `:latest` still points at the old lineage, so start_project_container
        // recreates from it unaided.
        assert_eq!(
            decide_recovery(Some(MIGRATION_PHASE_IN_PROGRESS), false),
            Recovery::SelfHeal
        );
        assert_eq!(
            decide_recovery(Some(MIGRATION_PHASE_INTERRUPTED), false),
            Recovery::SelfHeal
        );
    }

    #[test]
    fn a_crash_after_the_container_swap_needs_a_decision() {
        assert_eq!(
            decide_recovery(Some(MIGRATION_PHASE_IN_PROGRESS), true),
            Recovery::OfferResumeOrRollback
        );
        assert_eq!(
            decide_recovery(Some(MIGRATION_PHASE_INTERRUPTED), true),
            Recovery::OfferResumeOrRollback
        );
    }

    #[test]
    fn a_finished_migration_waits_for_confirm_or_rollback_either_way() {
        // The label is irrelevant once the final commit landed: the phase alone
        // decides, because the container is the migrated one by definition.
        for labelled in [true, false] {
            assert_eq!(
                decide_recovery(Some(MIGRATION_PHASE_AWAITING), labelled),
                Recovery::OfferConfirmOrRollback
            );
        }
    }

    #[test]
    fn an_unrecognised_phase_never_silently_discards_the_record() {
        assert_eq!(
            decide_recovery(Some("who-knows"), false),
            Recovery::OfferConfirmOrRollback
        );
    }

    // ── Misc ────────────────────────────────────────────────────────────────

    #[test]
    fn image_refs_split_on_the_tag_not_a_registry_port() {
        assert_eq!(
            split_image_ref("triple-c-snapshot-abc:latest"),
            ("triple-c-snapshot-abc".to_string(), "latest".to_string())
        );
        assert_eq!(
            split_image_ref("ghcr.io/shadowdao/triple-c-sandbox:latest"),
            (
                "ghcr.io/shadowdao/triple-c-sandbox".to_string(),
                "latest".to_string()
            )
        );
        assert_eq!(
            split_image_ref("registry:5000/img"),
            ("registry:5000/img".to_string(), "latest".to_string())
        );
    }

    #[test]
    fn rollback_tags_are_sortable_and_docker_legal() {
        let t = rollback_tag(
            &chrono::DateTime::parse_from_rfc3339("2026-08-09T17:04:05Z")
                .unwrap()
                .with_timezone(&chrono::Utc),
        );
        assert_eq!(t, "pre-migration-20260809-170405");
        assert!(t
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.'));
    }

    #[test]
    fn the_preflight_parser_reads_df_blocks_as_kibibytes() {
        let raw = "###DF\n/dev/sdc 1055762868 12345 900000000 2% /\n###NET\nok\n###END\n";
        let p = parse_preflight(raw);
        assert_eq!(p.available_bytes, 900_000_000u64 * 1024);
        assert!(p.network_ok);

        let raw = "###DF\n###NET\nfailed\n###END\n";
        let p = parse_preflight(raw);
        assert_eq!(p.available_bytes, 0);
        assert!(!p.network_ok);
    }

    #[test]
    fn the_manifest_script_emits_every_section_the_parser_expects() {
        let s = manifest_script();
        for section in [
            "###PATHS", "###DPKG", "###APT", "###NPM", "###FEATURES", "###ETC", "###PKGVER",
            "###END",
        ] {
            assert!(s.contains(section), "script is missing {}", section);
        }
        // Every probed feature path must reach the script, or the missing-
        // feature report would silently under-report.
        for (path, _) in FEATURE_PROBES {
            assert!(s.contains(path), "script is missing probe {}", path);
        }
    }

    #[test]
    fn shell_quoting_survives_an_apostrophe() {
        assert_eq!(shell_single_quote("/opt/a'b"), r#"'/opt/a'\''b'"#);
    }

}
