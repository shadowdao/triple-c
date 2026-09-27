//! Marketplaces: git repos of agents, skills, commands, hooks and plugins that
//! are fetched on the host and synced into containers. See
//! `docs/superpowers/specs/2026-09-27-marketplace-design.md`.

pub mod auth;
pub mod catalog;
pub mod diff;
pub mod gh_login;
pub mod git;
pub mod payload;
pub mod sync;
pub mod tree;
#[cfg(test)]
mod sync_script_tests;
#[cfg(test)]
pub(crate) mod test_support;

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use tauri::Emitter;
use tokio::sync::oneshot;

use crate::models::marketplace::{
    effective_installs, CatalogItem, ItemKind, ItemUpdate, Marketplace, MarketplaceInstall, MarketplaceSnapshot,
    SyncReport,
};
use crate::models::{AppSettings, Project};
use catalog::{item_fingerprint, parse_catalog};
use tree::GitTree;

/// Emitted after every container sync, payload `{ project_id, report }`.
pub const SYNC_FINISHED_EVENT: &str = "marketplace-sync-finished";

pub struct MarketplaceManager {
    data_root: PathBuf,
    snapshots: Mutex<HashMap<String, MarketplaceSnapshot>>,
    reports: Mutex<HashMap<String, SyncReport>>,
    gh_login_cancel: tokio::sync::Mutex<Option<oneshot::Sender<()>>>,
    /// One lock per marketplace cache, serialising its writers (fetch, pins,
    /// cache removal) so they never race on gix ref locks (pre-flight F11a),
    /// without one marketplace's fetch holding up another (PR review #5).
    repo_locks: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    /// One lock per project, held for a whole `sync_project`, so a start sync
    /// and Apply now never run `sync.sh` in one container at once (F11b).
    sync_locks: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
}

/// Project ids become file names; anything outside this set is not persisted.
fn safe_file_stem(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

impl MarketplaceManager {
    /// `data_root` is `<data_dir>/triple-c`.
    pub fn new(data_root: PathBuf) -> Self {
        Self {
            data_root,
            snapshots: Mutex::new(HashMap::new()),
            reports: Mutex::new(HashMap::new()),
            gh_login_cancel: tokio::sync::Mutex::new(None),
            repo_locks: Mutex::new(HashMap::new()),
            sync_locks: Mutex::new(HashMap::new()),
        }
    }

    pub fn data_root(&self) -> &Path {
        &self.data_root
    }

    /// The marketplace's cache lock. Hold it while writing to that cache
    /// (fetch, `git::set_pins`, removing it) and never drop it mid-fetch: a
    /// blocking fetch keeps running after its future is cancelled.
    pub fn repo_lock(&self, marketplace_id: &str) -> Arc<tokio::sync::Mutex<()>> {
        self.repo_locks
            .lock()
            .unwrap()
            .entry(marketplace_id.to_string())
            .or_default()
            .clone()
    }

    /// The project's sync lock; see `sync_project`.
    pub fn sync_lock(&self, project_id: &str) -> Arc<tokio::sync::Mutex<()>> {
        self.sync_locks
            .lock()
            .unwrap()
            .entry(project_id.to_string())
            .or_default()
            .clone()
    }

    /// The in-memory snapshot's head, without copying the snapshot's items.
    pub fn snapshot_head(&self, marketplace_id: &str) -> Option<String> {
        self.snapshots
            .lock()
            .unwrap()
            .get(marketplace_id)
            .and_then(|s| s.head_commit.clone())
    }

    /// Each item's `invalid` reason in the in-memory snapshot, if that
    /// snapshot is at `head` — without copying the items' previews.
    fn invalid_reasons_at(
        &self,
        marketplace_id: &str,
        head: &str,
    ) -> Option<HashMap<(ItemKind, String), Option<String>>> {
        let snapshots = self.snapshots.lock().unwrap();
        let snap = snapshots.get(marketplace_id)?;
        (snap.head_commit.as_deref() == Some(head)).then(|| invalid_reasons(&snap.items))
    }

    pub fn snapshot(&self, marketplace_id: &str) -> Option<MarketplaceSnapshot> {
        self.snapshots.lock().unwrap().get(marketplace_id).cloned()
    }

    pub fn put_snapshot(&self, snap: MarketplaceSnapshot) {
        self.snapshots
            .lock()
            .unwrap()
            .insert(snap.marketplace_id.clone(), snap);
    }

    pub fn remove_snapshot(&self, marketplace_id: &str) {
        self.snapshots.lock().unwrap().remove(marketplace_id);
    }

    fn report_path(&self, project_id: &str) -> PathBuf {
        self.data_root
            .join("marketplace-sync")
            .join(format!("{project_id}.json"))
    }

    pub fn report(&self, project_id: &str) -> Option<SyncReport> {
        if let Some(r) = self.reports.lock().unwrap().get(project_id) {
            return Some(r.clone());
        }
        if !safe_file_stem(project_id) {
            return None;
        }
        let text = std::fs::read_to_string(self.report_path(project_id)).ok()?;
        let report: SyncReport = serde_json::from_str(&text).ok()?;
        self.reports
            .lock()
            .unwrap()
            .insert(project_id.to_string(), report.clone());
        Some(report)
    }

    pub fn put_report(&self, project_id: &str, report: SyncReport) {
        self.reports
            .lock()
            .unwrap()
            .insert(project_id.to_string(), report.clone());
        if !safe_file_stem(project_id) {
            return;
        }
        let path = self.report_path(project_id);
        let write = || -> std::io::Result<()> {
            std::fs::create_dir_all(path.parent().unwrap())?;
            let tmp = path.with_extension("json.tmp");
            std::fs::write(&tmp, serde_json::to_vec_pretty(&report).unwrap_or_default())?;
            std::fs::rename(&tmp, &path)
        };
        if let Err(e) = write() {
            log::warn!(
                "Could not persist the marketplace sync report for {}: {}",
                project_id,
                e
            );
        }
    }

    /// Claim (`Some`) or release (`None`) the single gh-login slot. Claiming
    /// fails while another login holds it.
    pub async fn set_gh_login_cancel(&self, tx: Option<oneshot::Sender<()>>) -> bool {
        let mut slot = self.gh_login_cancel.lock().await;
        match tx {
            Some(tx) => {
                if slot.is_some() {
                    return false;
                }
                *slot = Some(tx);
                true
            }
            None => {
                *slot = None;
                true
            }
        }
    }

    /// Free the slot after a login ends, but only if it still holds that
    /// login's sender (its receiver is gone once the login returns). A cancel
    /// may have emptied the slot and a newer login claimed it meanwhile; a
    /// plain `set_gh_login_cancel(None)` would drop that login's sender,
    /// which it reads as a cancel.
    pub async fn release_gh_login(&self) {
        let mut slot = self.gh_login_cancel.lock().await;
        if slot.as_ref().is_some_and(|tx| tx.is_closed()) {
            *slot = None;
        }
    }

    pub async fn cancel_gh_login(&self) {
        if let Some(tx) = self.gh_login_cancel.lock().await.take() {
            let _ = tx.send(());
        }
    }
}

/// Head commit for a marketplace: the in-memory snapshot's, else the cache's.
pub fn head_for(mgr: &MarketplaceManager, m: &Marketplace) -> Option<String> {
    mgr.snapshot_head(&m.id).or_else(|| {
        git::cached_head(&git::cache_path(mgr.data_root(), &m.id))
            .ok()
            .flatten()
    })
}

fn parse_at(repo: &Path, commit: &str) -> Result<Vec<CatalogItem>, String> {
    let tree = GitTree::open(repo, commit)?;
    Ok(parse_catalog(&tree))
}

/// Snapshot from the cache alone (no network): startup, and after an install
/// when nothing is in memory. `fetched_at` stays `None`.
pub fn load_cached_snapshot(
    mgr: &MarketplaceManager,
    marketplace: &Marketplace,
) -> MarketplaceSnapshot {
    let repo = git::cache_path(mgr.data_root(), &marketplace.id);
    let mut snap = MarketplaceSnapshot {
        marketplace_id: marketplace.id.clone(),
        ..Default::default()
    };
    match git::cached_head(&repo) {
        Ok(Some(head)) => match parse_at(&repo, &head) {
            Ok(items) => {
                snap.head_commit = Some(head);
                snap.items = items;
            }
            Err(e) => snap.fetch_error = Some(format!("The cached copy could not be read: {e}")),
        },
        Ok(None) => {}
        Err(e) => snap.fetch_error = Some(format!("The cached copy could not be read: {e}")),
    }
    snap
}

/// Keep the previous items and head (in memory, else from the cache) and
/// record why this refresh failed.
fn failed_snapshot(
    mgr: &MarketplaceManager,
    m: &Marketplace,
    message: String,
) -> MarketplaceSnapshot {
    let mut snap = mgr
        .snapshot(&m.id)
        .unwrap_or_else(|| load_cached_snapshot(mgr, m));
    snap.fetch_error = Some(message);
    mgr.put_snapshot(snap.clone());
    snap
}

/// Refresh one marketplace: resolve the credential, fetch (blocking task),
/// parse the catalog at head and store the snapshot. On failure the previous
/// items and head are kept and `fetch_error` is set.
///
/// Everything runs under the marketplace's repo lock, and `current_settings`
/// (the settings store as it is *now*, not a copy taken before the lock) is
/// read only once the lock is held: a marketplace removed meanwhile gets no
/// cache and no snapshot (PR review #6), since its removal deletes both
/// under the same lock.
pub async fn refresh_marketplace(
    mgr: &MarketplaceManager,
    current_settings: &(dyn Fn() -> AppSettings + Sync),
    marketplace_id: &str,
) -> MarketplaceSnapshot {
    let lock = mgr.repo_lock(marketplace_id);
    let _repo_guard = lock.lock().await;
    let settings = current_settings();
    let Some(m) = settings
        .marketplaces
        .iter()
        .find(|m| m.id == marketplace_id)
        .cloned()
    else {
        return MarketplaceSnapshot {
            marketplace_id: marketplace_id.to_string(),
            fetch_error: Some("This marketplace is no longer configured.".to_string()),
            ..Default::default()
        };
    };
    let account = m
        .account_id
        .as_ref()
        .and_then(|id| settings.marketplace_accounts.iter().find(|a| &a.id == id))
        .cloned();
    let cred = match &account {
        Some(a) => match auth::resolve_credential(a).await {
            Ok(c) => Some(c),
            Err(e) => return failed_snapshot(mgr, &m, e),
        },
        None => None,
    };

    let repo = git::cache_path(mgr.data_root(), &m.id);
    let (url, branch) = (m.url.clone(), m.branch.clone());
    let joined = tokio::task::spawn_blocking(move || {
        let head = git::fetch(&repo, &url, branch.as_deref(), cred)?;
        let items = parse_at(&repo, &head).map_err(git::FetchError::Other)?;
        Ok::<_, git::FetchError>((head, items))
    })
    .await;

    match joined {
        Ok(Ok((head, items))) => {
            let snap = MarketplaceSnapshot {
                marketplace_id: m.id.clone(),
                head_commit: Some(head),
                fetched_at: Some(chrono::Utc::now().to_rfc3339()),
                fetch_error: None,
                items,
            };
            mgr.put_snapshot(snap.clone());
            snap
        }
        Ok(Err(e)) => failed_snapshot(
            mgr,
            &m,
            auth::describe_fetch_error(&e, account.as_ref(), &m.url),
        ),
        Err(e) => failed_snapshot(mgr, &m, format!("The refresh task failed: {e}")),
    }
}

/// Forget a marketplace's snapshot and delete its cache, under its repo lock,
/// so a refresh already under way either finishes first (and is then
/// deleted) or sees the marketplace gone and stores nothing.
pub async fn remove_marketplace_cache(mgr: &MarketplaceManager, marketplace_id: &str) {
    let lock = mgr.repo_lock(marketplace_id);
    let _repo_guard = lock.lock().await;
    mgr.remove_snapshot(marketplace_id);
    let path = git::cache_path(mgr.data_root(), marketplace_id);
    let _ = tokio::task::spawn_blocking(move || {
        if path.exists() {
            if let Err(e) = std::fs::remove_dir_all(&path) {
                log::warn!(
                    "Could not delete the marketplace cache {}: {}",
                    path.display(),
                    e
                );
            }
        }
    })
    .await;
}

fn invalid_reasons(items: &[CatalogItem]) -> HashMap<(ItemKind, String), Option<String>> {
    items
        .iter()
        .map(|i| ((i.kind, i.key.clone()), i.invalid.clone()))
        .collect()
}

/// One marketplace's side of an update check: its head, read once, and its
/// cache, opened once, with trees shared across installs (PR review #10).
struct UpdateCheck {
    head: String,
    repo: Option<gix::Repository>,
    trees: HashMap<String, Result<GitTree, String>>,
    /// Catalog `invalid` per item at head, read once when first needed.
    invalid_at_head: Option<HashMap<(ItemKind, String), Option<String>>>,
}

impl UpdateCheck {
    fn new(mgr: &MarketplaceManager, m: &Marketplace) -> Option<Self> {
        let head = head_for(mgr, m)?;
        Some(Self {
            head,
            repo: None,
            trees: HashMap::new(),
            invalid_at_head: None,
        })
    }

    /// Why `inst`'s item cannot be installed at head — the rule
    /// `update_marketplace_item` applies (round 2) — from the snapshot when
    /// it is at head, else from the catalog parsed at head.
    fn invalid_reason(
        &mut self,
        mgr: &MarketplaceManager,
        m: &Marketplace,
        inst: &MarketplaceInstall,
    ) -> Option<String> {
        if self.invalid_at_head.is_none() {
            let head = self.head.clone();
            let reasons = match mgr.invalid_reasons_at(&m.id, &head) {
                Some(r) => r,
                None => match self.tree(&git::cache_path(mgr.data_root(), &m.id), &head) {
                    Ok(tree) => invalid_reasons(&parse_catalog(tree)),
                    Err(e) => return Some(e),
                },
            };
            self.invalid_at_head = Some(reasons);
        }
        match self
            .invalid_at_head
            .as_ref()
            .and_then(|r| r.get(&(inst.kind, inst.key.clone())))
        {
            Some(reason) => reason.clone(),
            None => Some(format!("\"{}\" is no longer in \"{}\".", inst.key, m.name)),
        }
    }

    fn tree(&mut self, repo_path: &Path, commit: &str) -> Result<&GitTree, String> {
        if !self.trees.contains_key(commit) {
            if self.repo.is_none() {
                self.repo = Some(tree::open_repo(repo_path)?);
            }
            let repo = self.repo.clone().expect("opened above");
            self.trees
                .insert(commit.to_string(), GitTree::at(repo, commit));
        }
        self.trees[commit].as_ref().map_err(Clone::clone)
    }

    fn changed(&mut self, repo_path: &Path, inst: &MarketplaceInstall) -> Result<bool, String> {
        let head = self.head.clone();
        let new = item_fingerprint(self.tree(repo_path, &head)?, inst.kind, &inst.key)?;
        let old = item_fingerprint(self.tree(repo_path, &inst.commit)?, inst.kind, &inst.key)?;
        Ok(old != new)
    }
}

/// Every install (global + all projects) whose item fingerprint at head
/// differs from its pin. Installs whose pin is not in the cache are skipped.
pub fn compute_updates(
    mgr: &MarketplaceManager,
    settings: &AppSettings,
    projects: &[Project],
) -> Vec<ItemUpdate> {
    let mut seen = BTreeSet::new();
    let mut checks: HashMap<String, Option<UpdateCheck>> = HashMap::new();
    let mut out = Vec::new();
    let all = settings
        .global_marketplace_installs
        .iter()
        .chain(projects.iter().flat_map(|p| p.marketplace_installs.iter()));
    for inst in all {
        if !seen.insert((inst.item_ref(), inst.commit.clone())) {
            continue;
        }
        let Some(m) = settings
            .marketplaces
            .iter()
            .find(|m| m.id == inst.marketplace_id)
        else {
            continue;
        };
        let Some(check) = checks
            .entry(m.id.clone())
            .or_insert_with(|| UpdateCheck::new(mgr, m))
        else {
            continue;
        };
        if check.head == inst.commit {
            continue;
        }
        let repo = git::cache_path(mgr.data_root(), &m.id);
        match check.changed(&repo, inst) {
            Ok(true) => out.push(ItemUpdate {
                item: inst.item_ref(),
                pinned: inst.commit.clone(),
                head: check.head.clone(),
                invalid_at_head: check.invalid_reason(mgr, m, inst),
            }),
            Ok(false) => {}
            Err(e) => log::debug!("Update check skipped for {}: {}", inst.key, e),
        }
    }
    out
}

fn project_installs(settings: &AppSettings, project: &Project) -> Vec<MarketplaceInstall> {
    effective_installs(
        &settings.global_marketplace_installs,
        &project.marketplace_disabled,
        &project.marketplace_installs,
    )
}

/// Build the project's payload and sync it into its running container. The
/// report is stored (and persisted) whatever happens. Holds the project's sync
/// lock throughout, so concurrent syncs of one project run one after another.
pub async fn sync_project(
    mgr: &MarketplaceManager,
    settings: &AppSettings,
    project: &Project,
    container_id: &str,
) -> SyncReport {
    let lock = mgr.sync_lock(&project.id);
    let _sync_guard = lock.lock().await;

    let installs = project_installs(settings, project);
    let marketplaces = settings.marketplaces.clone();
    let root = mgr.data_root().to_path_buf();
    let built = tokio::task::spawn_blocking(move || {
        payload::build_payload(&payload::PayloadInput {
            installs: &installs,
            marketplaces: &marketplaces,
            data_root: &root,
        })
    })
    .await
    .map_err(|e| format!("Building the marketplace payload failed: {e}"))
    .and_then(|r| r);

    let report = match built {
        Ok(p) => {
            let items = p.manifest["items"].as_array().map_or(0, Vec::len);
            log::debug!(
                "Marketplace sync for project {}: {} item(s) in the payload, {} skipped on the host",
                project.id,
                items,
                p.skipped.len()
            );
            let result = sync::sync_container(container_id, &p).await;
            sync::with_payload_skips(sync::report_from_result(result), &p.skipped)
        }
        Err(e) => sync::report_from_result(Err(e)),
    };
    mgr.put_report(&project.id, report.clone());
    report
}

/// A project with no items that has never been synced has nothing to add and
/// nothing to remove, so its start does not wait on a sync at all.
pub fn should_sync(mgr: &MarketplaceManager, settings: &AppSettings, project: &Project) -> bool {
    !project_installs(settings, project).is_empty() || mgr.report(&project.id).is_some()
}

/// Sync in the background after a container start. The sync waits for the
/// entrypoint to finish (which can include a two-minute `claude update`), and
/// its failure must never fail the start — so the start never awaits it.
pub fn spawn_project_sync(
    app: tauri::AppHandle,
    mgr: Arc<MarketplaceManager>,
    settings: AppSettings,
    project: Project,
    container_id: String,
) {
    if !should_sync(&mgr, &settings, &project) {
        return;
    }
    tauri::async_runtime::spawn(async move {
        let report = sync_project(&mgr, &settings, &project, &container_id).await;
        if !report.errors.is_empty() {
            log::warn!(
                "Marketplace sync for project {} reported errors: {:?}",
                project.id,
                report.errors
            );
        }
        let _ = app.emit(
            SYNC_FINISHED_EVENT,
            serde_json::json!({ "project_id": project.id, "report": report }),
        );
    });
}

/// Make each cache's pin refs exactly the commits installs reference, so a
/// pinned version can never be garbage-collected away — for every configured
/// marketplace, or only `only`. Each marketplace's pins are set under its own
/// repo lock (pre-flight F11, PR review #5): a fetch writes refs there too.
pub async fn set_pins(
    mgr: &MarketplaceManager,
    settings: &AppSettings,
    projects: &[Project],
    only: Option<&str>,
) {
    let pins = pins_by_marketplace(settings, projects);
    for m in settings
        .marketplaces
        .iter()
        .filter(|m| only.is_none_or(|id| id == m.id))
    {
        let lock = mgr.repo_lock(&m.id);
        let _repo_guard = lock.lock().await;
        let repo = git::cache_path(mgr.data_root(), &m.id);
        let commits = pins.get(&m.id).cloned().unwrap_or_default();
        let id = m.id.clone();
        let _ = tokio::task::spawn_blocking(move || {
            if !repo.exists() {
                return;
            }
            if let Err(e) = git::set_pins(&repo, &commits) {
                log::warn!(
                    "Could not update the pinned commits of marketplace {}: {}",
                    id,
                    e
                );
            }
        })
        .await;
    }
}

/// All commits referenced by installs, per marketplace (for `git::set_pins`).
pub fn pins_by_marketplace(
    settings: &AppSettings,
    projects: &[Project],
) -> HashMap<String, Vec<String>> {
    let mut map: HashMap<String, BTreeSet<String>> = HashMap::new();
    let all = settings
        .global_marketplace_installs
        .iter()
        .chain(projects.iter().flat_map(|p| p.marketplace_installs.iter()));
    for inst in all {
        map.entry(inst.marketplace_id.clone())
            .or_default()
            .insert(inst.commit.clone());
    }
    map.into_iter()
        .map(|(k, v)| (k, v.into_iter().collect()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::marketplace::test_support::GitFixture;
    use crate::models::marketplace::{ItemKind, Marketplace, MarketplaceInstall};

    fn settings_with(url: &str) -> AppSettings {
        let mut s = AppSettings::default();
        s.marketplaces.push(Marketplace {
            id: "m1".into(),
            name: "Test".into(),
            url: url.into(),
            branch: None,
            account_id: None,
        });
        s
    }

    fn install(kind: ItemKind, key: &str, commit: &str) -> MarketplaceInstall {
        MarketplaceInstall {
            marketplace_id: "m1".into(),
            kind,
            key: key.into(),
            commit: commit.into(),
        }
    }

    #[tokio::test]
    async fn refresh_parses_the_catalog_at_head() {
        let Some(fx) = GitFixture::new() else { return };
        let c1 = fx.with_all_kinds();
        let data = tempfile::tempdir().unwrap();
        let mgr = MarketplaceManager::new(data.path().to_path_buf());

        let snap = refresh_marketplace(&mgr, &|| settings_with(&fx.url()), "m1").await;

        assert_eq!(snap.fetch_error, None);
        assert_eq!(snap.head_commit.as_deref(), Some(c1.as_str()));
        assert!(snap.fetched_at.is_some());
        let mut keys: Vec<String> = snap
            .items
            .iter()
            .map(|i| format!("{:?}:{}", i.kind, i.key))
            .collect();
        keys.sort();
        assert_eq!(
            keys,
            vec![
                "Agent:code-reviewer",
                "Command:example-command",
                "Hook:notify-on-stop",
                "Plugin:example-plugin",
                "Skill:example-skill",
            ]
        );
        assert_eq!(mgr.snapshot("m1"), Some(snap));
    }

    #[tokio::test]
    async fn refresh_failure_keeps_snapshot() {
        let Some(fx) = GitFixture::new() else { return };
        let c1 = fx.with_all_kinds();
        let url = fx.url();
        let data = tempfile::tempdir().unwrap();
        let mgr = MarketplaceManager::new(data.path().to_path_buf());
        let settings = settings_with(&url);
        let first = refresh_marketplace(&mgr, &|| settings.clone(), "m1").await;
        assert_eq!(first.fetch_error, None);

        drop(fx); // the source repository disappears (offline, deleted, …)
        let second = refresh_marketplace(&mgr, &|| settings.clone(), "m1").await;

        assert!(second.fetch_error.is_some(), "expected a fetch error");
        assert_eq!(second.head_commit.as_deref(), Some(c1.as_str()));
        assert_eq!(second.items, first.items);
        assert_eq!(second.fetched_at, first.fetched_at);
    }

    #[tokio::test]
    async fn refresh_waits_for_the_repo_lock() {
        let Some(fx) = GitFixture::new() else { return };
        let c1 = fx.with_all_kinds();
        let data = tempfile::tempdir().unwrap();
        let mgr = MarketplaceManager::new(data.path().to_path_buf());
        let settings = settings_with(&fx.url());

        let lock = mgr.repo_lock("m1");
        let guard = lock.lock().await;
        let blocked = tokio::time::timeout(
            std::time::Duration::from_millis(300),
            refresh_marketplace(&mgr, &|| settings.clone(), "m1"),
        )
        .await;
        assert!(blocked.is_err(), "refresh must not fetch while the repo lock is held");
        assert!(
            !git::cache_path(data.path(), "m1").exists(),
            "nothing may touch the cache while the lock is held"
        );
        drop(guard);

        let snap = refresh_marketplace(&mgr, &|| settings.clone(), "m1").await;
        assert_eq!(snap.head_commit.as_deref(), Some(c1.as_str()));
    }

    #[test]
    fn repo_locks_are_per_marketplace() {
        let mgr = MarketplaceManager::new(std::env::temp_dir());
        let a1 = mgr.repo_lock("a");
        let a2 = mgr.repo_lock("a");
        let b = mgr.repo_lock("b");
        assert!(Arc::ptr_eq(&a1, &a2), "one lock per marketplace");
        assert!(!Arc::ptr_eq(&a1, &b), "marketplaces do not block each other");
    }

    /// PR review #5: a long fetch of one marketplace must not hold up work
    /// (another refresh, pins, installs) on a different one.
    #[tokio::test]
    async fn a_busy_marketplace_does_not_block_another() {
        let Some(fx) = GitFixture::new() else { return };
        let c1 = fx.with_all_kinds();
        let data = tempfile::tempdir().unwrap();
        let mgr = MarketplaceManager::new(data.path().to_path_buf());
        let settings = settings_with(&fx.url());

        let other = mgr.repo_lock("some-other-marketplace");
        let _busy = other.lock().await;
        let snap = tokio::time::timeout(
            std::time::Duration::from_secs(20),
            refresh_marketplace(&mgr, &|| settings.clone(), "m1"),
        )
        .await
        .expect("m1 must not wait for another marketplace's lock");
        assert_eq!(snap.head_commit.as_deref(), Some(c1.as_str()));
    }

    /// PR review #6: a refresh that was already under way when the
    /// marketplace was removed must not recreate its cache or snapshot.
    #[tokio::test]
    async fn a_refresh_of_a_removed_marketplace_leaves_nothing_behind() {
        let Some(fx) = GitFixture::new() else { return };
        fx.with_all_kinds();
        let data = tempfile::tempdir().unwrap();
        let mgr = MarketplaceManager::new(data.path().to_path_buf());
        let current = Mutex::new(settings_with(&fx.url()));
        let read_current = || current.lock().unwrap().clone();

        let lock = mgr.repo_lock("m1");
        let guard = lock.lock().await;
        let refresh = refresh_marketplace(&mgr, &read_current, "m1");
        tokio::pin!(refresh);
        // The refresh starts, then waits for the lock…
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(100), &mut refresh)
                .await
                .is_err()
        );
        // …while the marketplace is removed from settings.
        current.lock().unwrap().marketplaces.clear();
        drop(guard);
        let snap = refresh.await;

        assert!(
            snap.fetch_error.as_deref().unwrap_or("").contains("no longer configured"),
            "{snap:?}"
        );
        assert!(!git::cache_path(data.path(), "m1").exists(), "cache recreated");
        assert_eq!(mgr.snapshot("m1"), None, "snapshot stored");
    }

    #[tokio::test]
    async fn removing_a_cache_waits_for_the_marketplaces_lock() {
        let Some(fx) = GitFixture::new() else { return };
        fx.with_all_kinds();
        let data = tempfile::tempdir().unwrap();
        let mgr = MarketplaceManager::new(data.path().to_path_buf());
        let settings = settings_with(&fx.url());
        refresh_marketplace(&mgr, &|| settings.clone(), "m1").await;
        let cache = git::cache_path(data.path(), "m1");
        assert!(cache.exists());

        let lock = mgr.repo_lock("m1");
        let guard = lock.lock().await;
        let blocked = tokio::time::timeout(
            std::time::Duration::from_millis(200),
            remove_marketplace_cache(&mgr, "m1"),
        )
        .await;
        assert!(blocked.is_err(), "removal must wait for an in-flight fetch");
        assert!(cache.exists());
        drop(guard);

        remove_marketplace_cache(&mgr, "m1").await;
        assert!(!cache.exists());
        assert_eq!(mgr.snapshot("m1"), None);
    }

    #[tokio::test]
    async fn concurrent_refreshes_all_succeed() {
        let Some(fx) = GitFixture::new() else { return };
        let c1 = fx.with_all_kinds();
        let data = tempfile::tempdir().unwrap();
        let mgr = MarketplaceManager::new(data.path().to_path_buf());
        let settings = settings_with(&fx.url());

        let current = || settings.clone();
        let (a, b, c) = tokio::join!(
            refresh_marketplace(&mgr, &current, "m1"),
            refresh_marketplace(&mgr, &current, "m1"),
            refresh_marketplace(&mgr, &current, "m1"),
        );
        for snap in [a, b, c] {
            assert_eq!(snap.fetch_error, None);
            assert_eq!(snap.head_commit.as_deref(), Some(c1.as_str()));
        }
    }

    #[tokio::test]
    async fn cached_snapshot_loads_without_network() {
        let Some(fx) = GitFixture::new() else { return };
        let c1 = fx.with_all_kinds();
        let data = tempfile::tempdir().unwrap();
        let settings = settings_with(&fx.url());
        {
            let mgr = MarketplaceManager::new(data.path().to_path_buf());
            refresh_marketplace(&mgr, &|| settings.clone(), "m1").await;
        }
        drop(fx);
        let mgr = MarketplaceManager::new(data.path().to_path_buf());
        let snap = load_cached_snapshot(&mgr, &settings.marketplaces[0]);
        assert_eq!(snap.head_commit.as_deref(), Some(c1.as_str()));
        assert_eq!(snap.items.len(), 5);
        assert_eq!(snap.fetch_error, None);
    }

    #[tokio::test]
    async fn only_items_whose_own_files_changed_have_updates() {
        let Some(fx) = GitFixture::new() else { return };
        let c1 = fx.with_all_kinds();
        fx.write(
            "agents/code-reviewer.md",
            "---\nname: code-reviewer\ndescription: Reviews code\n---\nReview harder.\n",
        );
        let c2 = fx.commit("tweak agent");
        let data = tempfile::tempdir().unwrap();
        let mgr = MarketplaceManager::new(data.path().to_path_buf());
        let mut settings = settings_with(&fx.url());
        settings.global_marketplace_installs = vec![
            install(ItemKind::Agent, "code-reviewer", &c1),
            install(ItemKind::Hook, "notify-on-stop", &c1),
        ];
        let mut project = crate::models::Project::new("p".into(), vec![]);
        project.marketplace_installs = vec![install(ItemKind::Skill, "example-skill", &c1)];
        refresh_marketplace(&mgr, &|| settings.clone(), "m1").await;

        let updates = compute_updates(&mgr, &settings, &[project]);

        assert_eq!(updates.len(), 1, "{updates:?}");
        assert_eq!(updates[0].item.key, "code-reviewer");
        assert_eq!(updates[0].pinned, c1);
        assert_eq!(updates[0].head, c2);
    }

    /// PR review #10: one repo open and one head read per marketplace,
    /// however many installs (and pinned commits) point into it.
    #[tokio::test]
    async fn update_check_opens_each_repo_once() {
        let Some(fx) = GitFixture::new() else { return };
        let c1 = fx.with_all_kinds();
        fx.write(
            "agents/code-reviewer.md",
            "---\nname: code-reviewer\ndescription: Reviews code\n---\nReview harder.\n",
        );
        let c2 = fx.commit("tweak agent");
        let data = tempfile::tempdir().unwrap();
        let mgr = MarketplaceManager::new(data.path().to_path_buf());
        let mut settings = settings_with(&fx.url());
        settings.global_marketplace_installs = vec![
            install(ItemKind::Agent, "code-reviewer", &c1),
            install(ItemKind::Hook, "notify-on-stop", &c1),
        ];
        let mut a = crate::models::Project::new("a".into(), vec![]);
        a.marketplace_installs = vec![install(ItemKind::Skill, "example-skill", &c1)];
        let mut b = crate::models::Project::new("b".into(), vec![]);
        b.marketplace_installs = vec![install(ItemKind::Agent, "code-reviewer", &c2)];
        refresh_marketplace(&mgr, &|| settings.clone(), "m1").await;

        let before = tree::repo_opens();
        let updates = compute_updates(&mgr, &settings, &[a, b]);
        assert_eq!(tree::repo_opens() - before, 1, "one open for the whole check");

        assert_eq!(updates.len(), 1, "{updates:?}");
        assert_eq!(updates[0].item.key, "code-reviewer");
        assert_eq!(updates[0].pinned, c1);
        assert_eq!(updates[0].head, c2);
    }

    /// Re-review round 2: an update to a head where the item is not
    /// installable is listed with the reason, since update_marketplace_item
    /// would always refuse it.
    #[tokio::test]
    async fn an_update_to_an_invalid_version_carries_the_reason() {
        let Some(fx) = GitFixture::new() else { return };
        let c1 = fx.with_all_kinds();
        fx.write(
            "hooks/notify-on-stop/hook.json",
            r#"{"hooks":{"PreFoo":[{"hooks":[{"type":"command","command":"x"}]}]}}"#,
        );
        fx.write(
            "agents/code-reviewer.md",
            "---\nname: code-reviewer\ndescription: Reviews code\n---\nReview harder.\n",
        );
        std::fs::remove_file(fx.dir.path().join("commands/example-command.md")).unwrap();
        let c2 = fx.commit("break the hook, tweak the agent, drop the command");
        let data = tempfile::tempdir().unwrap();
        let mut settings = settings_with(&fx.url());
        settings.global_marketplace_installs = vec![
            install(ItemKind::Hook, "notify-on-stop", &c1),
            install(ItemKind::Agent, "code-reviewer", &c1),
            install(ItemKind::Command, "example-command", &c1),
        ];
        {
            let mgr = MarketplaceManager::new(data.path().to_path_buf());
            refresh_marketplace(&mgr, &|| settings.clone(), "m1").await;
            check_reasons(&compute_updates(&mgr, &settings, &[]), &c2);
        }
        // Same answer from the cache alone (no snapshot in memory).
        let mgr = MarketplaceManager::new(data.path().to_path_buf());
        check_reasons(&compute_updates(&mgr, &settings, &[]), &c2);
    }

    fn check_reasons(updates: &[ItemUpdate], head: &str) {
        let reason = |key: &str| {
            let u = updates.iter().find(|u| u.item.key == key).unwrap();
            assert_eq!(u.head, head);
            u.invalid_at_head.clone()
        };
        assert_eq!(updates.len(), 3, "{updates:?}");
        assert!(reason("notify-on-stop").unwrap().contains("PreFoo"));
        assert_eq!(reason("code-reviewer"), None);
        assert!(reason("example-command").unwrap().contains("no longer in"));
    }

    /// PR review #10: refreshing one marketplace sets only its pins, and so
    /// never waits on another marketplace's lock.
    #[tokio::test]
    async fn pins_can_be_set_for_one_marketplace_alone() {
        let Some(fx) = GitFixture::new() else { return };
        let c1 = fx.with_all_kinds();
        let data = tempfile::tempdir().unwrap();
        let mgr = MarketplaceManager::new(data.path().to_path_buf());
        let mut settings = settings_with(&fx.url());
        let mut m2 = settings.marketplaces[0].clone();
        m2.id = "m2".into();
        settings.marketplaces.push(m2);
        settings.global_marketplace_installs = vec![install(ItemKind::Agent, "code-reviewer", &c1)];
        refresh_marketplace(&mgr, &|| settings.clone(), "m1").await;
        refresh_marketplace(&mgr, &|| settings.clone(), "m2").await;

        let other = mgr.repo_lock("m2");
        let _busy = other.lock().await;
        tokio::time::timeout(
            std::time::Duration::from_secs(20),
            set_pins(&mgr, &settings, &[], Some("m1")),
        )
        .await
        .expect("setting m1's pins must not wait for m2");

        let refs = git::test_support::git(
            &git::cache_path(data.path(), "m1"),
            &["for-each-ref", "--format=%(refname)", "refs/triple-c/pins"],
        );
        assert_eq!(refs, format!("refs/triple-c/pins/{c1}"));
    }

    #[test]
    fn pins_are_grouped_and_deduplicated_per_marketplace() {
        let a = "a".repeat(40);
        let b = "b".repeat(40);
        let mut settings = settings_with("https://example.invalid/r.git");
        settings.global_marketplace_installs = vec![
            install(ItemKind::Agent, "x", &b),
            install(ItemKind::Hook, "y", &a),
        ];
        let mut project = crate::models::Project::new("p".into(), vec![]);
        project.marketplace_installs = vec![install(ItemKind::Agent, "z", &a)];
        let pins = pins_by_marketplace(&settings, &[project]);
        assert_eq!(pins.get("m1"), Some(&vec![a.clone(), b.clone()]));
    }

    #[test]
    fn reports_are_persisted_per_project() {
        let data = tempfile::tempdir().unwrap();
        let report = SyncReport {
            installed: vec!["agent:x".into()],
            ..Default::default()
        };
        MarketplaceManager::new(data.path().to_path_buf()).put_report("proj-1", report.clone());
        let fresh = MarketplaceManager::new(data.path().to_path_buf());
        assert_eq!(fresh.report("proj-1"), Some(report));
        assert_eq!(fresh.report("proj-2"), None);
    }

    #[tokio::test]
    async fn only_one_gh_login_may_hold_the_cancel_slot() {
        let mgr = MarketplaceManager::new(std::env::temp_dir());
        let (tx1, rx1) = tokio::sync::oneshot::channel();
        let (tx2, _rx2) = tokio::sync::oneshot::channel();
        assert!(mgr.set_gh_login_cancel(Some(tx1)).await);
        assert!(!mgr.set_gh_login_cancel(Some(tx2)).await);
        mgr.cancel_gh_login().await;
        assert!(rx1.await.is_ok(), "cancel must signal the running login");
        let (tx3, _rx3) = tokio::sync::oneshot::channel();
        assert!(
            mgr.set_gh_login_cancel(Some(tx3)).await,
            "slot is free after cancel"
        );
    }

    #[tokio::test]
    async fn releasing_a_finished_login_never_frees_a_newer_ones_slot() {
        let mgr = MarketplaceManager::new(std::env::temp_dir());
        // Login A is cancelled, and login B claims the slot before A returns.
        let (tx_a, rx_a) = tokio::sync::oneshot::channel::<()>();
        assert!(mgr.set_gh_login_cancel(Some(tx_a)).await);
        mgr.cancel_gh_login().await;
        let (tx_b, mut rx_b) = tokio::sync::oneshot::channel::<()>();
        assert!(mgr.set_gh_login_cancel(Some(tx_b)).await);
        drop(rx_a); // A returns.
        mgr.release_gh_login().await;
        assert!(
            matches!(rx_b.try_recv(), Err(tokio::sync::oneshot::error::TryRecvError::Empty)),
            "B's sender must still be held, not dropped"
        );
        // B returns: its slot is freed.
        drop(rx_b);
        mgr.release_gh_login().await;
        let (tx_c, _rx_c) = tokio::sync::oneshot::channel::<()>();
        assert!(mgr.set_gh_login_cancel(Some(tx_c)).await);
    }

    #[test]
    fn a_project_that_never_had_items_is_not_synced() {
        let data = tempfile::tempdir().unwrap();
        let mgr = MarketplaceManager::new(data.path().to_path_buf());
        let settings = settings_with("https://example.invalid/r.git");
        let project = crate::models::Project::new("p".into(), vec![]);
        assert!(!should_sync(&mgr, &settings, &project));
    }

    #[test]
    fn a_project_with_items_or_a_previous_sync_is_synced() {
        let data = tempfile::tempdir().unwrap();
        let mgr = MarketplaceManager::new(data.path().to_path_buf());
        let mut settings = settings_with("https://example.invalid/r.git");
        let project = crate::models::Project::new("p".into(), vec![]);
        settings.global_marketplace_installs = vec![install(ItemKind::Agent, "a", &"a".repeat(40))];
        assert!(should_sync(&mgr, &settings, &project), "global items apply");

        // Everything was uninstalled since the last sync: the container still
        // holds the old files, so it must be synced to remove them.
        settings.global_marketplace_installs.clear();
        mgr.put_report(&project.id, SyncReport::default());
        assert!(should_sync(&mgr, &settings, &project));
    }

    #[test]
    fn a_project_whose_only_item_is_disabled_is_not_synced() {
        let data = tempfile::tempdir().unwrap();
        let mgr = MarketplaceManager::new(data.path().to_path_buf());
        let mut settings = settings_with("https://example.invalid/r.git");
        let inst = install(ItemKind::Agent, "a", &"a".repeat(40));
        let mut project = crate::models::Project::new("p".into(), vec![]);
        project.marketplace_disabled = vec![inst.item_ref()];
        settings.global_marketplace_installs = vec![inst];
        assert!(!should_sync(&mgr, &settings, &project));
    }

    #[test]
    fn sync_locks_are_per_project() {
        let mgr = MarketplaceManager::new(std::env::temp_dir());
        let a1 = mgr.sync_lock("a");
        let a2 = mgr.sync_lock("a");
        let b = mgr.sync_lock("b");
        assert!(Arc::ptr_eq(&a1, &a2), "one lock per project");
        assert!(!Arc::ptr_eq(&a1, &b), "projects do not block each other");
    }

    #[tokio::test]
    async fn sync_project_waits_for_the_projects_sync_lock() {
        // Pre-flight F11b: a start sync and Apply now must never run sync.sh
        // in the same container at once.
        let data = tempfile::tempdir().unwrap();
        let mgr = MarketplaceManager::new(data.path().to_path_buf());
        let settings = settings_with("https://example.invalid/r.git");
        let project = crate::models::Project::new("p".into(), vec![]);

        let lock = mgr.sync_lock(&project.id);
        let guard = lock.lock().await;
        let blocked = tokio::time::timeout(
            std::time::Duration::from_millis(300),
            sync_project(&mgr, &settings, &project, "no-such-container"),
        )
        .await;
        assert!(blocked.is_err(), "sync must wait while another sync holds the lock");
        assert_eq!(mgr.report(&project.id), None, "nothing ran while blocked");
        drop(guard);

        // No Docker (or no such container) here: the failure becomes a stored
        // report instead of an error.
        let report = sync_project(&mgr, &settings, &project, "no-such-container").await;
        assert_eq!(report.errors.len(), 1, "{report:?}");
        assert!(!report.finished_at.is_empty());
        assert_eq!(mgr.report(&project.id), Some(report));
    }
}
