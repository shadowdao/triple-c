//! Marketplace commands: configure marketplaces and accounts, browse, install,
//! update, and push installs into running containers. Spec:
//! `docs/superpowers/specs/2026-09-27-marketplace-design.md`.

use std::collections::{BTreeMap, HashSet};

use tauri::{AppHandle, Emitter, State};
use tokio::sync::oneshot;

use crate::docker::container::is_container_running;
use crate::marketplace::{self as mk, auth, diff, gh_login, git, MarketplaceManager};
use crate::models::marketplace::{
    is_valid_commit, is_valid_item_key, AccountMethod, FileDiff, InstallScope, ItemUpdate,
    Marketplace, MarketplaceAccount, MarketplaceInstall, MarketplaceItemRef, MarketplaceSnapshot,
    ProjectSyncResult, SyncReport,
};
use crate::models::{AppSettings, Project};
use crate::storage::secure;
use crate::AppState;

/// Pure list/field operations behind the commands, kept apart so they are
/// testable without a Tauri runtime.
pub(crate) mod ops {
    use crate::marketplace::{auth, git};
    use crate::models::marketplace::{MarketplaceInstall, MarketplaceItemRef, MarketplaceSnapshot};

    /// Insert, or replace the install of the same item (a re-install re-pins).
    pub fn upsert_install(list: &mut Vec<MarketplaceInstall>, inst: MarketplaceInstall) {
        match list.iter_mut().find(|i| i.item_ref() == inst.item_ref()) {
            Some(existing) => *existing = inst,
            None => list.push(inst),
        }
    }

    pub fn remove_install(list: &mut Vec<MarketplaceInstall>, item: &MarketplaceItemRef) -> bool {
        let before = list.len();
        list.retain(|i| &i.item_ref() != item);
        list.len() != before
    }

    pub fn set_disabled(
        list: &mut Vec<MarketplaceItemRef>,
        item: &MarketplaceItemRef,
        disabled: bool,
    ) {
        list.retain(|r| r != item);
        if disabled {
            list.push(item.clone());
            list.sort();
        }
    }

    pub fn repin(list: &mut [MarketplaceInstall], item: &MarketplaceItemRef, commit: &str) -> bool {
        match list.iter_mut().find(|i| &i.item_ref() == item) {
            Some(i) => {
                i.commit = commit.to_string();
                true
            }
            None => false,
        }
    }

    /// The commit to pin for an install or update: the marketplace's current
    /// head, but only if it is the one the person reviewed (`expected`, the
    /// head the UI showed or diffed against). A refresh that lands between
    /// review and click must not pin content nobody saw (final review I2).
    pub fn reviewed_head(
        head: Option<&str>,
        expected: &str,
        marketplace_name: &str,
    ) -> Result<String, String> {
        let head = head.ok_or_else(|| {
            format!("\"{marketplace_name}\" has not been fetched yet — refresh it first.")
        })?;
        if head != expected {
            return Err(format!(
                "\"{marketplace_name}\" has changed since you reviewed this item — review it again."
            ));
        }
        Ok(head.to_string())
    }

    /// The one rule install and update share (PR review #1): pin
    /// `expected` only if it is still the head (see [`reviewed_head`]) and
    /// the item, as the catalog reads it at that head, is valid. `snap`'s
    /// items are always parsed at `snap.head_commit`, so for a hook this
    /// means its `hook.json` parses and names only known events — exactly
    /// what the payload later requires. `action` is "installed"/"updated".
    pub fn installable_at_head(
        snap: &MarketplaceSnapshot,
        item: &MarketplaceItemRef,
        expected: &str,
        marketplace_name: &str,
        action: &str,
    ) -> Result<String, String> {
        let head = reviewed_head(snap.head_commit.as_deref(), expected, marketplace_name)?;
        let entry = snap
            .items
            .iter()
            .find(|i| i.kind == item.kind && i.key == item.key)
            .ok_or_else(|| {
                format!(
                    "\"{}\" is no longer in \"{}\" — refresh the marketplace.",
                    item.key, marketplace_name
                )
            })?;
        if let Some(reason) = &entry.invalid {
            return Err(format!(
                "\"{}\" cannot be {} at this version: {}",
                entry.name, action, reason
            ));
        }
        Ok(head)
    }

    /// An unvalidated value as it may appear in an error: quoted and escaped
    /// (`{:?}`) and capped at 60 characters, since it can come from an
    /// import file rather than from what the person just typed.
    pub fn shown(value: &str) -> String {
        const MAX: usize = 60;
        if value.chars().count() > MAX {
            let head: String = value.chars().take(MAX).collect();
            format!("{:?}…", head)
        } else {
            format!("{:?}", value)
        }
    }

    pub fn validate_label(label: &str) -> Result<String, String> {
        let label = label.trim();
        if label.is_empty() {
            return Err("Enter a name.".to_string());
        }
        if label.chars().count() > 80 || label.chars().any(char::is_control) {
            return Err("Names are at most 80 characters, with no control characters.".to_string());
        }
        Ok(label.to_string())
    }

    /// `None` or blank means the repository's default branch. Otherwise the
    /// fetch's own rule ([`git::valid_branch`]), so the form never accepts a
    /// name the fetch then refuses (pre-flight F13).
    pub fn validate_branch(branch: Option<String>) -> Result<Option<String>, String> {
        let Some(b) = branch
            .map(|b| b.trim().to_string())
            .filter(|b| !b.is_empty())
        else {
            return Ok(None);
        };
        if git::valid_branch(&b) {
            Ok(Some(b))
        } else {
            Err(format!("{} is not a valid branch name.", shown(&b)))
        }
    }

    /// Lowercased host name with an optional `:port`: [`auth::valid_host`]'s
    /// character rule, plus a numeric port (pre-flight F13).
    pub fn validate_host(host: &str) -> Result<String, String> {
        let host = host.trim().to_ascii_lowercase();
        let port_ok = host
            .split_once(':')
            .map(|(_, p)| p)
            .is_none_or(|p| !p.is_empty() && p.len() <= 5 && p.bytes().all(|b| b.is_ascii_digit()));
        if auth::valid_host(&host) && !host.starts_with('.') && !host.starts_with(':') && port_ok {
            Ok(host)
        } else {
            Err(format!("{} is not a valid host name.", shown(&host)))
        }
    }

    /// Account and marketplace ids name keychain entries and cache
    /// directories, so an id from outside (an import) must be the shape the
    /// commands mint: a UUID-like `[A-Za-z0-9-]{1,64}`.
    pub fn validate_id(id: &str) -> Result<(), String> {
        let ok = !id.is_empty()
            && id.len() <= 64
            && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-');
        if ok {
            Ok(())
        } else {
            Err("An account or marketplace id is malformed.".to_string())
        }
    }

    /// A pasted token, trimmed. Never echoed back in the error.
    pub fn validate_token_text(token: &str) -> Result<String, String> {
        let token = token.trim();
        if token.is_empty() || token.chars().any(|c| c.is_whitespace() || c.is_control()) {
            return Err(
                "Paste the whole token — it cannot be empty or contain spaces.".to_string(),
            );
        }
        Ok(token.to_string())
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use crate::models::marketplace::{ItemKind, MarketplaceInstall, MarketplaceItemRef};

        fn r(key: &str) -> MarketplaceItemRef {
            MarketplaceItemRef {
                marketplace_id: "m".into(),
                kind: ItemKind::Agent,
                key: key.into(),
            }
        }
        fn i(key: &str, commit: &str) -> MarketplaceInstall {
            MarketplaceInstall {
                marketplace_id: "m".into(),
                kind: ItemKind::Agent,
                key: key.into(),
                commit: commit.into(),
            }
        }

        #[test]
        fn upsert_replaces_the_same_item_instead_of_duplicating_it() {
            let mut list = vec![i("a", "1"), i("b", "1")];
            upsert_install(&mut list, i("a", "2"));
            upsert_install(&mut list, i("c", "1"));
            assert_eq!(list, vec![i("a", "2"), i("b", "1"), i("c", "1")]);
        }

        #[test]
        fn remove_reports_whether_anything_was_removed() {
            let mut list = vec![i("a", "1")];
            assert!(!remove_install(&mut list, &r("zzz")));
            assert!(remove_install(&mut list, &r("a")));
            assert!(list.is_empty());
        }

        #[test]
        fn disabling_is_idempotent_and_sorted() {
            let mut list = vec![];
            set_disabled(&mut list, &r("b"), true);
            set_disabled(&mut list, &r("a"), true);
            set_disabled(&mut list, &r("a"), true);
            assert_eq!(list, vec![r("a"), r("b")]);
            set_disabled(&mut list, &r("a"), false);
            assert_eq!(list, vec![r("b")]);
        }

        #[test]
        fn repin_moves_only_the_named_item() {
            let mut list = vec![i("a", "1"), i("b", "1")];
            assert!(repin(&mut list, &r("b"), "2"));
            assert!(!repin(&mut list, &r("c"), "2"));
            assert_eq!(list, vec![i("a", "1"), i("b", "2")]);
        }

        #[test]
        fn labels_branches_and_hosts_are_validated() {
            assert_eq!(validate_label("  Work  ").unwrap(), "Work");
            assert!(validate_label(" ").is_err());
            assert!(validate_label(&"x".repeat(81)).is_err());
            assert!(validate_label("a\u{7}b").is_err());
            assert_eq!(validate_branch(None).unwrap(), None);
            assert_eq!(validate_branch(Some("  ".into())).unwrap(), None);
            assert_eq!(
                validate_branch(Some("release/1.x".into())).unwrap(),
                Some("release/1.x".into())
            );
            for bad in ["-x", "a..b", "a b", "a;b", "/a", "a/"] {
                assert!(validate_branch(Some(bad.into())).is_err(), "{bad}");
            }
            assert_eq!(validate_host("GitHub.com").unwrap(), "github.com");
            assert_eq!(
                validate_host("repo.example.net:3000").unwrap(),
                "repo.example.net:3000"
            );
            for bad in [
                "", "-a", "a b", "a/b", "a:", "a:x", "a;rm", "a:1:2", "a:123456",
            ] {
                assert!(validate_host(bad).is_err(), "{bad}");
            }
        }

        /// Final review I2: an install or update pins exactly the commit the
        /// person reviewed, or nothing.
        #[test]
        fn only_the_reviewed_head_is_pinned() {
            let h = "a".repeat(40);
            assert_eq!(reviewed_head(Some(&h), &h, "Team").unwrap(), h);
            let moved = reviewed_head(Some(&h), &"b".repeat(40), "Team").unwrap_err();
            assert!(moved.contains("changed since you reviewed"), "{moved}");
            assert!(moved.contains("review it again"), "{moved}");
            assert!(reviewed_head(Some(&h), "", "Team").is_err());
            let unfetched = reviewed_head(None, &h, "Team").unwrap_err();
            assert!(
                unfetched.contains("has not been fetched yet"),
                "{unfetched}"
            );
        }

        /// PR review #1: install and update share one rule — the item as the
        /// catalog reads it at the reviewed head must be valid. `item_files`
        /// alone (the old update check) accepts a hook whose `hook.json` names
        /// an unknown event, which every sync would then hold back.
        #[tokio::test]
        async fn an_item_invalid_at_the_reviewed_head_is_neither_installed_nor_updated() {
            use crate::marketplace::test_support::GitFixture;
            use crate::marketplace::{catalog, tree::GitTree, MarketplaceManager};
            use crate::models::marketplace::Marketplace;
            use crate::models::AppSettings;

            let Some(fx) = GitFixture::new() else { return };
            fx.with_all_kinds();
            fx.write(
                "hooks/notify-on-stop/hook.json",
                r#"{"hooks":{"PreFoo":[{"hooks":[{"type":"command","command":"x"}]}]}}"#,
            );
            let head = fx.commit("bad hook event");
            let data = tempfile::tempdir().unwrap();
            let mgr = MarketplaceManager::new(data.path().to_path_buf());
            let mut settings = AppSettings::default();
            settings.marketplaces.push(Marketplace {
                id: "m1".into(),
                name: "Team".into(),
                url: fx.url(),
                branch: None,
                account_id: None,
            });
            let snap =
                crate::marketplace::refresh_marketplace(&mgr, &|| settings.clone(), "m1").await;

            let repo = crate::marketplace::git::cache_path(data.path(), "m1");
            let tree = GitTree::open(&repo, &head).unwrap();
            assert!(
                catalog::item_files(&tree, ItemKind::Hook, "notify-on-stop").is_ok(),
                "the old update check let this through"
            );

            let hook = MarketplaceItemRef {
                marketplace_id: "m1".into(),
                kind: ItemKind::Hook,
                key: "notify-on-stop".into(),
            };
            for action in ["installed", "updated"] {
                let e = installable_at_head(&snap, &hook, &head, "Team", action).unwrap_err();
                assert!(e.contains(&format!("cannot be {action}")), "{e}");
                assert!(e.contains("PreFoo"), "{e}");
            }
            let agent = MarketplaceItemRef {
                kind: ItemKind::Agent,
                key: "code-reviewer".into(),
                ..hook.clone()
            };
            assert_eq!(
                installable_at_head(&snap, &agent, &head, "Team", "updated").unwrap(),
                head
            );
            let gone = MarketplaceItemRef {
                key: "no-such-agent".into(),
                ..agent
            };
            let e = installable_at_head(&snap, &gone, &head, "Team", "updated").unwrap_err();
            assert!(e.contains("no longer in"), "{e}");
            assert!(
                installable_at_head(&snap, &hook, &"b".repeat(40), "Team", "installed")
                    .unwrap_err()
                    .contains("changed since you reviewed")
            );
        }

        /// Pre-flight F13: the add form and the fetch agree on what a branch
        /// is, so a name the fetch would refuse is refused up front.
        #[test]
        fn the_branch_rule_is_the_fetchs_rule() {
            assert!(!crate::marketplace::git::valid_branch("x.lock"));
            assert!(validate_branch(Some("x.lock".into())).is_err());
        }

        #[test]
        fn ids_and_pasted_tokens_are_validated() {
            assert!(validate_id("0f8fad5b-d9cb-469f-a165-70867728950e").is_ok());
            for bad in ["", "../x", "a/b", "a b", "a.git", &"a".repeat(65)] {
                assert!(validate_id(bad).is_err(), "{bad}");
            }
            assert_eq!(
                validate_token_text("  test-token-not-real \n").unwrap(),
                "test-token-not-real"
            );
            for bad in ["", "   ", "test token", "test-token\u{7}"] {
                assert!(validate_token_text(bad).is_err(), "{bad:?}");
            }
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Helpers
// ─────────────────────────────────────────────────────────────────────────────

fn find_marketplace(settings: &AppSettings, id: &str) -> Result<Marketplace, String> {
    settings
        .marketplaces
        .iter()
        .find(|m| m.id == id)
        .cloned()
        .ok_or_else(|| "That marketplace is no longer configured.".to_string())
}

fn find_account(settings: &AppSettings, id: &str) -> Result<MarketplaceAccount, String> {
    settings
        .marketplace_accounts
        .iter()
        .find(|a| a.id == id)
        .cloned()
        .ok_or_else(|| "That account no longer exists.".to_string())
}

fn find_project(state: &AppState, id: &str) -> Result<Project, String> {
    state
        .projects_store
        .get(id)
        .ok_or_else(|| format!("Project {id} not found"))
}

/// Normalises `m` in place (name, URL, branch) and checks its account is on
/// the marketplace's host.
fn validate_marketplace(settings: &AppSettings, m: &mut Marketplace) -> Result<(), String> {
    m.name = ops::validate_label(&m.name)?;
    m.url = m.url.trim().to_string();
    let host = auth::host_of(&m.url)?;
    m.branch = ops::validate_branch(m.branch.take())?;
    if let Some(account_id) = &m.account_id {
        let a = find_account(settings, account_id)?;
        if !a.host.eq_ignore_ascii_case(&host) {
            return Err(format!(
                "The account \"{}\" is for {}, but this marketplace is on {}.",
                a.label, a.host, host
            ));
        }
    }
    Ok(())
}

fn same_source(a: &Marketplace, b: &Marketplace) -> bool {
    a.url.eq_ignore_ascii_case(&b.url) && a.branch == b.branch
}

fn validate_item(item: &MarketplaceItemRef) -> Result<(), String> {
    if is_valid_item_key(&item.key) {
        Ok(())
    } else {
        Err(format!(
            "{} is not a valid item name.",
            ops::shown(&item.key)
        ))
    }
}

fn validate_install(inst: &MarketplaceInstall) -> Result<(), String> {
    ops::validate_id(&inst.marketplace_id)?;
    validate_item(&inst.item_ref())?;
    if !is_valid_commit(&inst.commit) {
        return Err(format!(
            "The install of \"{}\" has an invalid commit id.",
            inst.key
        ));
    }
    Ok(())
}

/// Checks and normalises the marketplace half of an imported settings file
/// with the same rules the commands apply, before anything is written
/// (pre-flight F10). `tokens` is `ExportedSecrets::marketplace_account_tokens`.
///
/// Installs whose marketplace is not in the file are accepted: they are what
/// the Installed tab lists as "source removed".
pub(crate) fn validate_imported_marketplace_state(
    settings: &mut AppSettings,
    tokens: &BTreeMap<String, String>,
) -> Result<(), String> {
    let wrap = |e: String| format!("The file's marketplace settings were refused: {e}");

    let mut ids = HashSet::new();
    for a in &mut settings.marketplace_accounts {
        ops::validate_id(&a.id).map_err(wrap)?;
        if !ids.insert(a.id.clone()) {
            return Err(wrap("an account appears twice.".to_string()));
        }
        a.label = ops::validate_label(&a.label).map_err(wrap)?;
        a.host = ops::validate_host(&a.host).map_err(wrap)?;
        if let Some(u) = &a.username {
            if u.chars().count() > 100 || u.chars().any(char::is_control) {
                return Err(wrap(format!(
                    "the account \"{}\" has an invalid user name.",
                    a.label
                )));
            }
        }
    }

    let accounts_only = AppSettings {
        marketplace_accounts: settings.marketplace_accounts.clone(),
        ..AppSettings::default()
    };
    let mut ids = HashSet::new();
    for i in 0..settings.marketplaces.len() {
        let m = &mut settings.marketplaces[i];
        ops::validate_id(&m.id).map_err(wrap)?;
        if !ids.insert(m.id.clone()) {
            return Err(wrap("a marketplace appears twice.".to_string()));
        }
        validate_marketplace(&accounts_only, m).map_err(wrap)?;
        let m = &settings.marketplaces[i];
        if settings.marketplaces[..i].iter().any(|x| same_source(x, m)) {
            return Err(wrap(format!(
                "\"{}\" repeats another marketplace's repository.",
                m.name
            )));
        }
    }

    for inst in &settings.global_marketplace_installs {
        validate_install(inst).map_err(wrap)?;
    }

    for (account_id, token) in tokens {
        let account = settings
            .marketplace_accounts
            .iter()
            .find(|a| &a.id == account_id)
            .ok_or_else(|| wrap("a token belongs to no account in the file.".to_string()))?;
        if account.method == AccountMethod::GhHost {
            return Err(wrap(format!(
                "\"{}\" signs in through this computer's gh and cannot carry a token.",
                account.label
            )));
        }
        ops::validate_token_text(token).map_err(wrap)?;
    }
    Ok(())
}

/// Marketplaces configured in `before` that `after` no longer has: an import
/// that drops them leaves their caches and snapshots to be removed.
pub(crate) fn dropped_marketplace_ids(before: &AppSettings, after: &AppSettings) -> Vec<String> {
    before
        .marketplaces
        .iter()
        .filter(|m| !after.marketplaces.iter().any(|a| a.id == m.id))
        .map(|m| m.id.clone())
        .collect()
}

/// The in-memory snapshot, else the cached one (which is then remembered).
fn snapshot_or_cached(mgr: &MarketplaceManager, m: &Marketplace) -> MarketplaceSnapshot {
    if let Some(s) = mgr.snapshot(&m.id) {
        return s;
    }
    let s = mk::load_cached_snapshot(mgr, m);
    mgr.put_snapshot(s.clone());
    s
}

async fn snapshot_blocking(
    state: &AppState,
    m: &Marketplace,
) -> Result<MarketplaceSnapshot, String> {
    let mgr = state.marketplace.clone();
    let m = m.clone();
    tokio::task::spawn_blocking(move || snapshot_or_cached(&mgr, &m))
        .await
        .map_err(|e| format!("Reading the marketplace cache failed: {e}"))
}

/// Make each cache's pin refs exactly the commits installs reference (see
/// [`mk::set_pins`]), for every marketplace.
pub(crate) async fn refresh_pins(state: &AppState) {
    refresh_pins_of(state, None).await;
}

/// [`refresh_pins`] for every marketplace, or only `only`.
async fn refresh_pins_of(state: &AppState, only: Option<&str>) {
    let settings = state.settings_store.get();
    let projects = state.projects_store.list();
    mk::set_pins(&state.marketplace, &settings, &projects, only).await;
}

/// Forget a marketplace's snapshot and delete its cache, under its repo lock.
pub(crate) async fn remove_cache(state: &AppState, marketplace_id: &str) {
    mk::remove_marketplace_cache(&state.marketplace, marketplace_id).await;
}

fn save_new_account(
    state: &AppState,
    account: MarketplaceAccount,
    stored_token: bool,
) -> Result<MarketplaceAccount, String> {
    let mut settings = state.settings_store.get();
    settings.marketplace_accounts.push(account.clone());
    if let Err(e) = state.settings_store.update(settings) {
        if stored_token {
            let _ = secure::delete_marketplace_token(&account.id);
        }
        return Err(e);
    }
    Ok(account)
}

// ─────────────────────────────────────────────────────────────────────────────
// Marketplaces
// ─────────────────────────────────────────────────────────────────────────────

#[tauri::command]
pub async fn list_marketplace_snapshots(
    state: State<'_, AppState>,
) -> Result<Vec<MarketplaceSnapshot>, String> {
    let settings = state.settings_store.get();
    let mgr = state.marketplace.clone();
    tokio::task::spawn_blocking(move || {
        settings
            .marketplaces
            .iter()
            .map(|m| snapshot_or_cached(&mgr, m))
            .collect()
    })
    .await
    .map_err(|e| format!("Reading the marketplace caches failed: {e}"))
}

/// With `marketplace_id`, refreshes (and re-pins) only that marketplace and
/// returns only its snapshot — the frontend merges snapshots by id — or
/// nothing if it was removed meanwhile. Without, refreshes all and returns
/// every snapshot.
#[tauri::command]
pub async fn refresh_marketplaces(
    marketplace_id: Option<String>,
    state: State<'_, AppState>,
) -> Result<Vec<MarketplaceSnapshot>, String> {
    let current = || state.settings_store.get();
    if let Some(id) = &marketplace_id {
        find_marketplace(&current(), id)?;
        let snap = mk::refresh_marketplace(&state.marketplace, &current, id).await;
        refresh_pins_of(&state, Some(id)).await;
        let still_configured = find_marketplace(&current(), id).is_ok();
        return Ok(if still_configured { vec![snap] } else { vec![] });
    }
    for m in current().marketplaces {
        mk::refresh_marketplace(&state.marketplace, &current, &m.id).await;
    }
    refresh_pins(&state).await;
    list_marketplace_snapshots(state).await
}

/// Test-fetches before saving: a wrong URL or credential fails here, and
/// nothing is stored.
#[tauri::command]
pub async fn add_marketplace(
    name: String,
    url: String,
    branch: Option<String>,
    account_id: Option<String>,
    state: State<'_, AppState>,
) -> Result<MarketplaceSnapshot, String> {
    let settings = state.settings_store.get();
    let mut m = Marketplace {
        id: uuid::Uuid::new_v4().to_string(),
        name,
        url,
        branch,
        account_id,
    };
    validate_marketplace(&settings, &mut m)?;
    if settings.marketplaces.iter().any(|x| same_source(x, &m)) {
        return Err("This repository (and branch) has already been added.".to_string());
    }

    let mut trial = settings.clone();
    trial.marketplaces.push(m.clone());
    // Not yet in the store: the trial settings stand in for it.
    let snap = mk::refresh_marketplace(&state.marketplace, &|| trial.clone(), &m.id).await;
    let failure = snap.fetch_error.clone().or_else(|| {
        snap.head_commit
            .is_none()
            .then(|| "The repository has no commits yet.".to_string())
    });
    if let Some(e) = failure {
        remove_cache(&state, &m.id).await;
        return Err(e);
    }

    let mut current = state.settings_store.get();
    current.marketplaces.push(m);
    state.settings_store.update(current)?;
    Ok(snap)
}

#[tauri::command]
pub async fn update_marketplace(
    marketplace: Marketplace,
    state: State<'_, AppState>,
) -> Result<AppSettings, String> {
    let mut settings = state.settings_store.get();
    let mut m = marketplace;
    validate_marketplace(&settings, &mut m)?;
    if settings
        .marketplaces
        .iter()
        .any(|x| x.id != m.id && same_source(x, &m))
    {
        return Err("This repository (and branch) has already been added.".to_string());
    }
    let slot = settings
        .marketplaces
        .iter_mut()
        .find(|x| x.id == m.id)
        .ok_or_else(|| "That marketplace is no longer configured.".to_string())?;
    *slot = m;
    state.settings_store.update(settings)
}

/// Installs from it stay listed as "source removed" until forgotten.
#[tauri::command]
pub async fn remove_marketplace(
    marketplace_id: String,
    state: State<'_, AppState>,
) -> Result<AppSettings, String> {
    let mut settings = state.settings_store.get();
    find_marketplace(&settings, &marketplace_id)?;
    settings.marketplaces.retain(|m| m.id != marketplace_id);
    let saved = state.settings_store.update(settings)?;
    remove_cache(&state, &marketplace_id).await;
    Ok(saved)
}

#[tauri::command]
pub async fn forget_marketplace_installs(
    marketplace_id: String,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let mut settings = state.settings_store.get();
    settings
        .global_marketplace_installs
        .retain(|i| i.marketplace_id != marketplace_id);
    state.settings_store.update(settings)?;
    state
        .projects_store
        .update_all_marketplace_fields(|installs, disabled| {
            installs.retain(|i| i.marketplace_id != marketplace_id);
            disabled.retain(|r| r.marketplace_id != marketplace_id);
        })?;
    refresh_pins(&state).await;
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// Installs
// ─────────────────────────────────────────────────────────────────────────────

/// Pins the item at `expected_commit`, the head the person reviewed, which
/// must still be the marketplace's head. Returns fresh settings; for a
/// project scope the caller reloads projects.
#[tauri::command]
pub async fn install_marketplace_item(
    item: MarketplaceItemRef,
    scope: InstallScope,
    expected_commit: String,
    state: State<'_, AppState>,
) -> Result<AppSettings, String> {
    validate_item(&item)?;
    let settings = state.settings_store.get();
    let m = find_marketplace(&settings, &item.marketplace_id)?;
    let snap = snapshot_blocking(&state, &m).await?;
    let head = ops::installable_at_head(&snap, &item, &expected_commit, &m.name, "installed")?;
    let inst = MarketplaceInstall {
        marketplace_id: item.marketplace_id.clone(),
        kind: item.kind,
        key: item.key.clone(),
        commit: head,
    };
    match scope {
        InstallScope::Global => {
            let mut s = state.settings_store.get();
            ops::upsert_install(&mut s.global_marketplace_installs, inst);
            state.settings_store.update(s)?;
        }
        InstallScope::Project { project_id } => {
            state
                .projects_store
                .update_marketplace_fields(&project_id, |installs, _| {
                    ops::upsert_install(installs, inst);
                    Ok(())
                })?;
        }
    }
    refresh_pins(&state).await;
    Ok(state.settings_store.get())
}

#[tauri::command]
pub async fn uninstall_marketplace_item(
    item: MarketplaceItemRef,
    scope: InstallScope,
    state: State<'_, AppState>,
) -> Result<(), String> {
    match scope {
        InstallScope::Global => {
            let mut s = state.settings_store.get();
            if !ops::remove_install(&mut s.global_marketplace_installs, &item) {
                return Err("That item is not installed for all projects.".to_string());
            }
            state.settings_store.update(s)?;
            // An opt-out of an item that is no longer global means nothing.
            state
                .projects_store
                .update_all_marketplace_fields(|_, disabled| {
                    ops::set_disabled(disabled, &item, false)
                })?;
        }
        InstallScope::Project { project_id } => {
            let name = find_project(&state, &project_id)?.name;
            state
                .projects_store
                .update_marketplace_fields(&project_id, |installs, _| {
                    if ops::remove_install(installs, &item) {
                        Ok(())
                    } else {
                        Err(format!("That item is not installed in \"{name}\"."))
                    }
                })?;
        }
    }
    refresh_pins(&state).await;
    Ok(())
}

#[tauri::command]
pub async fn set_global_item_disabled(
    project_id: String,
    item: MarketplaceItemRef,
    disabled: bool,
    state: State<'_, AppState>,
) -> Result<Project, String> {
    validate_item(&item)?;
    let ((), saved) = state
        .projects_store
        .update_marketplace_fields(&project_id, |_, list| {
            ops::set_disabled(list, &item, disabled);
            Ok(())
        })?;
    Ok(saved)
}

// ─────────────────────────────────────────────────────────────────────────────
// Updates
// ─────────────────────────────────────────────────────────────────────────────

#[tauri::command]
pub async fn list_marketplace_updates(
    state: State<'_, AppState>,
) -> Result<Vec<ItemUpdate>, String> {
    let settings = state.settings_store.get();
    let projects = state.projects_store.list();
    let mgr = state.marketplace.clone();
    tokio::task::spawn_blocking(move || mk::compute_updates(&mgr, &settings, &projects))
        .await
        .map_err(|e| format!("Checking for updates failed: {e}"))
}

#[tauri::command]
pub async fn marketplace_item_diff(
    item: MarketplaceItemRef,
    from_commit: String,
    to_commit: String,
    state: State<'_, AppState>,
) -> Result<Vec<FileDiff>, String> {
    validate_item(&item)?;
    if !is_valid_commit(&from_commit) || !is_valid_commit(&to_commit) {
        return Err("Invalid commit id.".to_string());
    }
    let settings = state.settings_store.get();
    let m = find_marketplace(&settings, &item.marketplace_id)?;
    let repo = git::cache_path(state.marketplace.data_root(), &m.id);
    tokio::task::spawn_blocking(move || {
        diff::item_diff(&repo, item.kind, &item.key, &from_commit, &to_commit)
    })
    .await
    .map_err(|e| format!("Computing the diff failed: {e}"))?
}

/// Moves one install's pin to `expected_commit`, the head whose diff the
/// person accepted, if that is still the marketplace's head and the item is
/// still installable there.
#[tauri::command]
pub async fn update_marketplace_item(
    item: MarketplaceItemRef,
    scope: InstallScope,
    expected_commit: String,
    state: State<'_, AppState>,
) -> Result<(), String> {
    validate_item(&item)?;
    let settings = state.settings_store.get();
    let m = find_marketplace(&settings, &item.marketplace_id)?;
    let snap = snapshot_blocking(&state, &m).await?;
    let head = ops::installable_at_head(&snap, &item, &expected_commit, &m.name, "updated")?;

    match scope {
        InstallScope::Global => {
            let mut s = state.settings_store.get();
            if !ops::repin(&mut s.global_marketplace_installs, &item, &head) {
                return Err("That item is not installed for all projects.".to_string());
            }
            state.settings_store.update(s)?;
        }
        InstallScope::Project { project_id } => {
            let name = find_project(&state, &project_id)?.name;
            state
                .projects_store
                .update_marketplace_fields(&project_id, |installs, _| {
                    if ops::repin(installs, &item, &head) {
                        Ok(())
                    } else {
                        Err(format!("That item is not installed in \"{name}\"."))
                    }
                })?;
        }
    }
    refresh_pins(&state).await;
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// Sync
// ─────────────────────────────────────────────────────────────────────────────

/// Sync one running project, or every running project when `project_id` is
/// None. Emits `marketplace-sync-finished` after each sync, like a start sync
/// does, so skips and errors reach the same toast (pre-flight F4).
#[tauri::command]
pub async fn apply_marketplace_now(
    project_id: Option<String>,
    app_handle: AppHandle,
    state: State<'_, AppState>,
) -> Result<Vec<ProjectSyncResult>, String> {
    let settings = state.settings_store.get();
    let projects = match &project_id {
        Some(id) => vec![find_project(&state, id)?],
        None => state.projects_store.list(),
    };
    let mut results = Vec::new();
    for p in projects {
        let running = match &p.container_id {
            Some(cid) => is_container_running(cid).await.unwrap_or(false),
            None => false,
        };
        if !running {
            if project_id.is_some() {
                return Err(format!(
                    "\"{}\" is not running. Its marketplace items are applied when it starts.",
                    p.name
                ));
            }
            continue;
        }
        let cid = p.container_id.clone().unwrap_or_default();
        let report = mk::sync_project(&state.marketplace, &settings, &p, &cid).await;
        let _ = app_handle.emit(
            mk::SYNC_FINISHED_EVENT,
            serde_json::json!({ "project_id": p.id, "report": report }),
        );
        results.push(ProjectSyncResult {
            project_id: p.id.clone(),
            report,
        });
    }
    Ok(results)
}

#[tauri::command]
pub async fn get_marketplace_sync_report(
    project_id: String,
    state: State<'_, AppState>,
) -> Result<Option<SyncReport>, String> {
    Ok(state.marketplace.report(&project_id))
}

// ─────────────────────────────────────────────────────────────────────────────
// Accounts
// ─────────────────────────────────────────────────────────────────────────────

#[tauri::command]
pub async fn add_marketplace_token_account(
    label: String,
    host: String,
    token: String,
    state: State<'_, AppState>,
) -> Result<MarketplaceAccount, String> {
    let label = ops::validate_label(&label)?;
    let host = ops::validate_host(&host)?;
    let token = ops::validate_token_text(&token)?;
    // None = a host with no known "who am I" API; the marketplace's test fetch proves the token.
    let username = auth::validate_token(&host, &token).await?;
    let account = MarketplaceAccount {
        id: uuid::Uuid::new_v4().to_string(),
        label,
        host,
        method: AccountMethod::Token,
        username,
    };
    secure::store_marketplace_token(&account.id, &token)?;
    save_new_account(&state, account, true)
}

#[tauri::command]
pub async fn add_marketplace_gh_host_account(
    label: String,
    host: String,
    state: State<'_, AppState>,
) -> Result<MarketplaceAccount, String> {
    let label = ops::validate_label(&label)?;
    let host = ops::validate_host(&host)?;
    if !auth::gh_host_available().await {
        return Err(
            "The GitHub CLI (gh) is not installed on this computer. Sign in through a running \
             container instead, or add a token."
                .to_string(),
        );
    }
    let username = auth::gh_host_login(&host).await?;
    let account = MarketplaceAccount {
        id: uuid::Uuid::new_v4().to_string(),
        label,
        host,
        method: AccountMethod::GhHost,
        username: Some(username),
    };
    save_new_account(&state, account, false)
}

/// Long-running: drives `gh auth login --web` in the project's container and
/// emits `marketplace-gh-login-code` / `-output` while it waits.
///
/// The cancel sender stays in the manager's slot for the whole login: the
/// login treats a dropped sender as a cancel.
#[tauri::command]
pub async fn start_marketplace_gh_container_login(
    label: String,
    host: String,
    project_id: String,
    app_handle: AppHandle,
    state: State<'_, AppState>,
) -> Result<MarketplaceAccount, String> {
    let label = ops::validate_label(&label)?;
    let host = ops::validate_host(&host)?;
    if !gh_login::valid_host(&host) {
        return Err("Signing in through a container needs a host name without a port.".to_string());
    }
    let project = find_project(&state, &project_id)?;
    let container_id = project.container_id.clone().ok_or_else(|| {
        format!(
            "\"{}\" has no container yet. Start it, then try again.",
            project.name
        )
    })?;
    if !is_container_running(&container_id).await.unwrap_or(false) {
        return Err(format!(
            "\"{}\" is not running. Start it, then try again.",
            project.name
        ));
    }

    let (tx, rx) = oneshot::channel();
    if !state.marketplace.set_gh_login_cancel(Some(tx)).await {
        return Err("A GitHub sign-in is already running. Finish or cancel it first.".to_string());
    }
    let account_id = uuid::Uuid::new_v4().to_string();
    let result =
        gh_login::run_gh_container_login(&app_handle, &account_id, &container_id, &host, rx).await;
    // `rx` is gone now, so this frees our slot and never a newer login's.
    state.marketplace.release_gh_login().await;
    let token = result?;

    let username = auth::validate_token(&host, &token).await?;
    secure::store_marketplace_token(&account_id, &token)?;
    let account = MarketplaceAccount {
        id: account_id,
        label,
        host,
        method: AccountMethod::GhContainer,
        username,
    };
    save_new_account(&state, account, true)
}

#[tauri::command]
pub async fn cancel_marketplace_gh_login(state: State<'_, AppState>) -> Result<(), String> {
    state.marketplace.cancel_gh_login().await;
    Ok(())
}

#[tauri::command]
pub async fn test_marketplace_account(
    account_id: String,
    state: State<'_, AppState>,
) -> Result<String, String> {
    let settings = state.settings_store.get();
    let account = find_account(&settings, &account_id)?;
    let cred = auth::resolve_credential(&account).await?;
    Ok(auth::validate_token(&account.host, &cred.password)
        .await?
        .unwrap_or_else(|| "token present (this host has no sign-in check)".to_string()))
}

#[tauri::command]
pub async fn remove_marketplace_account(
    account_id: String,
    state: State<'_, AppState>,
) -> Result<AppSettings, String> {
    let mut settings = state.settings_store.get();
    let account = find_account(&settings, &account_id)?;
    if let Some(m) = settings
        .marketplaces
        .iter()
        .find(|m| m.account_id.as_deref() == Some(account_id.as_str()))
    {
        return Err(format!(
            "\"{}\" uses this account. Change or remove that marketplace first.",
            m.name
        ));
    }
    // Keychain first: if it refuses, nothing has changed yet.
    if account.method != AccountMethod::GhHost {
        secure::delete_marketplace_token(&account.id)?;
    }
    settings.marketplace_accounts.retain(|a| a.id != account_id);
    state.settings_store.update(settings)
}

#[tauri::command]
pub async fn marketplace_gh_host_available() -> Result<bool, String> {
    Ok(auth::gh_host_available().await)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::models::marketplace::{
        AccountMethod, ItemKind, Marketplace, MarketplaceAccount, MarketplaceInstall,
    };
    use crate::models::AppSettings;

    const ACCOUNT: &str = "0f8fad5b-d9cb-469f-a165-70867728950e";
    const MARKET: &str = "7c9e6679-7425-40de-944b-e07fc1f90ae7";

    fn imported() -> AppSettings {
        let mut s = AppSettings::default();
        s.marketplace_accounts.push(MarketplaceAccount {
            id: ACCOUNT.into(),
            label: " Work ".into(),
            host: "GitHub.com".into(),
            method: AccountMethod::Token,
            username: Some("octo".into()),
        });
        s.marketplaces.push(Marketplace {
            id: MARKET.into(),
            name: "Team".into(),
            url: " https://github.com/org/repo.git ".into(),
            branch: Some(" ".into()),
            account_id: Some(ACCOUNT.into()),
        });
        s.global_marketplace_installs.push(MarketplaceInstall {
            marketplace_id: MARKET.into(),
            kind: ItemKind::Hook,
            key: "fmt".into(),
            commit: "a".repeat(40),
        });
        s
    }

    fn tokens() -> BTreeMap<String, String> {
        BTreeMap::from([(ACCOUNT.to_string(), "test-token-not-real".to_string())])
    }

    #[test]
    fn a_valid_import_passes_and_is_normalised_like_a_command_would() {
        let mut s = imported();
        validate_imported_marketplace_state(&mut s, &tokens()).unwrap();
        assert_eq!(s.marketplace_accounts[0].label, "Work");
        assert_eq!(s.marketplace_accounts[0].host, "github.com");
        assert_eq!(s.marketplaces[0].url, "https://github.com/org/repo.git");
        assert_eq!(s.marketplaces[0].branch, None);
    }

    #[test]
    fn an_import_is_refused_for_anything_a_command_would_refuse() {
        type Break = fn(&mut AppSettings, &mut BTreeMap<String, String>);
        let cases: Vec<(&str, Break)> = vec![
            ("http url", |s, _| {
                s.marketplaces[0].url = "http://github.com/o/r.git".into()
            }),
            ("url with credentials", |s, _| {
                s.marketplaces[0].url = "https://u:p@github.com/o/r.git".into()
            }),
            ("bad branch", |s, _| {
                s.marketplaces[0].branch = Some("a..b".into())
            }),
            ("path in marketplace id", |s, _| {
                s.marketplaces[0].id = "../x".into()
            }),
            ("duplicate marketplace id", |s, _| {
                let m = s.marketplaces[0].clone();
                s.marketplaces.push(m)
            }),
            ("unknown account", |s, _| {
                s.marketplaces[0].account_id = Some("nope".into())
            }),
            ("account on another host", |s, _| {
                s.marketplace_accounts[0].host = "gitlab.com".into()
            }),
            ("path in account id", |s, _| {
                s.marketplace_accounts[0].id = "../x".into()
            }),
            ("bad account host", |s, _| {
                s.marketplace_accounts[0].host = "a;rm".into()
            }),
            ("blank account label", |s, _| {
                s.marketplace_accounts[0].label = " ".into()
            }),
            ("bad item key", |s, _| {
                s.global_marketplace_installs[0].key = "../x".into()
            }),
            ("bad commit", |s, _| {
                s.global_marketplace_installs[0].commit = "HEAD".into()
            }),
            ("bad install marketplace id", |s, _| {
                s.global_marketplace_installs[0].marketplace_id = "a/b".into()
            }),
            ("token for no account", |_, t| {
                t.insert(
                    "7c9e6679-0000-0000-0000-000000000000".into(),
                    "test-token-not-real".into(),
                );
            }),
            ("token for a gh-host account", |s, _| {
                s.marketplace_accounts[0].method = AccountMethod::GhHost
            }),
            ("token with spaces", |_, t| {
                t.insert(ACCOUNT.into(), "test token".into());
            }),
        ];
        for (name, f) in cases {
            let (mut s, mut t) = (imported(), tokens());
            f(&mut s, &mut t);
            assert!(
                validate_imported_marketplace_state(&mut s, &t).is_err(),
                "{name} should be refused"
            );
        }
    }

    /// Installs of a marketplace the file no longer configures are allowed:
    /// they are what the Installed tab lists as "source removed".
    #[test]
    fn an_install_whose_marketplace_is_gone_is_still_accepted() {
        let mut s = imported();
        s.marketplaces.clear();
        validate_imported_marketplace_state(&mut s, &tokens()).unwrap();
    }

    #[test]
    fn an_invalid_item_key_is_quoted_and_capped_in_the_error() {
        use crate::models::marketplace::MarketplaceItemRef;
        let item = |key: String| MarketplaceItemRef {
            marketplace_id: MARKET.into(),
            kind: ItemKind::Agent,
            key,
        };
        let e = validate_item(&item("bad\nkey".into())).unwrap_err();
        assert!(!e.contains('\n'), "raw control character in {e:?}");
        assert!(e.contains("\"bad\\nkey\""), "{e}");
        let e = validate_item(&item(format!("{}/", "x".repeat(500)))).unwrap_err();
        assert!(
            e.chars().count() < 150,
            "not capped: {} chars",
            e.chars().count()
        );
    }

    #[test]
    fn marketplaces_an_import_drops_are_the_ones_whose_caches_go() {
        let mut before = imported();
        let mut kept = before.marketplaces[0].clone();
        kept.id = "kept-1".into();
        before.marketplaces.push(kept.clone());
        let mut after = AppSettings::default();
        after.marketplaces.push(kept);
        assert_eq!(
            dropped_marketplace_ids(&before, &after),
            vec![MARKET.to_string()]
        );
        assert!(dropped_marketplace_ids(&after, &before).is_empty());
    }
}
