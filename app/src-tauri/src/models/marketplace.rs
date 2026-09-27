//! Marketplace data model — see `docs/superpowers/specs/2026-09-27-marketplace-design.md`.
//!
//! Plain data plus the pure rules that decide what a project actually gets
//! ([`effective_installs`]) and what names are allowed to reach a container
//! path ([`is_valid_item_key`], [`marketplace_slug`]).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemKind {
    Agent,
    Skill,
    Command,
    Hook,
    Plugin,
}

impl ItemKind {
    /// The lowercase name used in report strings (`"agent:code-reviewer"`) and the manifest.
    pub fn as_str(&self) -> &'static str {
        match self {
            ItemKind::Agent => "agent",
            ItemKind::Skill => "skill",
            ItemKind::Command => "command",
            ItemKind::Hook => "hook",
            ItemKind::Plugin => "plugin",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccountMethod {
    GhHost,
    GhContainer,
    Token,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MarketplaceAccount {
    pub id: String,
    pub label: String,
    pub host: String,
    pub method: AccountMethod,
    #[serde(default)]
    pub username: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Marketplace {
    pub id: String,
    pub name: String,
    pub url: String,
    #[serde(default)]
    pub branch: Option<String>,
    #[serde(default)]
    pub account_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct MarketplaceItemRef {
    pub marketplace_id: String,
    pub kind: ItemKind,
    pub key: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MarketplaceInstall {
    pub marketplace_id: String,
    pub kind: ItemKind,
    pub key: String,
    pub commit: String,
}

impl MarketplaceInstall {
    pub fn item_ref(&self) -> MarketplaceItemRef {
        MarketplaceItemRef {
            marketplace_id: self.marketplace_id.clone(),
            kind: self.kind,
            key: self.key.clone(),
        }
    }
}

/// What a project's container actually gets: the global installs minus the
/// ones this project opted out of, plus the project's own installs. When the
/// project installs an item that is also global, the project's entry (and so
/// its pin) wins. Sorted by item ref so the result is deterministic.
pub fn effective_installs(
    global: &[MarketplaceInstall],
    disabled: &[MarketplaceItemRef],
    project: &[MarketplaceInstall],
) -> Vec<MarketplaceInstall> {
    let mut out: BTreeMap<MarketplaceItemRef, MarketplaceInstall> = BTreeMap::new();
    for install in global {
        let item = install.item_ref();
        if disabled.contains(&item) {
            continue;
        }
        out.insert(item, install.clone());
    }
    for install in project {
        out.insert(install.item_ref(), install.clone());
    }
    out.into_values().collect()
}

/// `^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$` — the only names that may become a
/// container path component. No `/`, no leading `.` or `-`, no shell
/// metacharacters.
pub fn is_valid_item_key(key: &str) -> bool {
    let bytes = key.as_bytes();
    if bytes.is_empty() || bytes.len() > 64 {
        return false;
    }
    if !bytes[0].is_ascii_alphanumeric() {
        return false;
    }
    bytes
        .iter()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// A container-safe name for a marketplace (plugin marketplace
/// `triple-c-<slug>`, plugin tree `plugins/<slug>/`): `mp-` and the first 8
/// alphanumeric characters of its id, lowercased. It depends on the id only,
/// never on the editable display name, so a rename cannot make a container
/// see a different marketplace (final review M4). Containers synced with
/// the earlier `<name>-<id8>` slugs move over on their next sync: the
/// plugins are installed under the new name and the old copies uninstalled.
pub fn marketplace_slug(id: &str) -> String {
    let id_part: String = id
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_lowercase())
        .take(8)
        .collect();
    if id_part.is_empty() {
        "mp-marketplace".to_string()
    } else {
        format!("mp-{id_part}")
    }
}

/// A full, lowercase, 40-character hex object id.
pub fn is_valid_commit(commit: &str) -> bool {
    commit.len() == 40
        && commit
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogItem {
    pub kind: ItemKind,
    pub key: String,
    pub name: String,
    pub description: String,
    /// Repo-relative path of the item (file or folder).
    pub path: String,
    /// `Some(reason)` when the item cannot be installed.
    pub invalid: Option<String>,
    /// Hooks only: rendered commands with `${HOOK_DIR}` substituted.
    #[serde(default)]
    pub hook_commands: Vec<String>,
    /// Agents/commands/skills: the markdown body (≤ 64 KiB, truncated);
    /// plugins: a component listing.
    #[serde(default)]
    pub preview: String,
    /// Plugins only: what the plugin brings that runs or adds commands —
    /// inline in its catalog entry and in its folder — shown before an
    /// install is confirmed (PR review #4).
    #[serde(default)]
    pub plugin_components: Vec<PluginComponent>,
}

/// One part of a plugin that can run something: e.g. its catalog entry's
/// `mcpServers`, or its folder's `hooks/hooks.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginComponent {
    /// Where it comes from, e.g. `"marketplace.json entry: mcpServers"`.
    pub label: String,
    /// Pretty-printed JSON, file text or a listing (≤ 64 KiB, truncated).
    pub content: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct MarketplaceSnapshot {
    pub marketplace_id: String,
    pub head_commit: Option<String>,
    /// RFC 3339.
    pub fetched_at: Option<String>,
    pub fetch_error: Option<String>,
    pub items: Vec<CatalogItem>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ItemUpdate {
    pub item: MarketplaceItemRef,
    pub pinned: String,
    pub head: String,
    /// Why the item cannot be installed at `head` (invalid there, or gone),
    /// so the update would be refused; `None` when it can be applied.
    #[serde(default)]
    pub invalid_at_head: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileChange {
    Added,
    Removed,
    Modified,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileDiff {
    pub path: String,
    pub change: FileChange,
    /// Unified diff text; `None` when either side is binary.
    pub unified: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct SkippedItem {
    pub item: String,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct SyncReport {
    #[serde(default)]
    pub installed: Vec<String>,
    #[serde(default)]
    pub updated: Vec<String>,
    #[serde(default)]
    pub removed: Vec<String>,
    #[serde(default)]
    pub skipped: Vec<SkippedItem>,
    #[serde(default)]
    pub errors: Vec<String>,
    /// RFC 3339, set by the host.
    #[serde(default)]
    pub finished_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum InstallScope {
    Global,
    Project { project_id: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectSyncResult {
    pub project_id: String,
    pub report: SyncReport,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn install(market: &str, kind: ItemKind, key: &str, commit: &str) -> MarketplaceInstall {
        MarketplaceInstall {
            marketplace_id: market.to_string(),
            kind,
            key: key.to_string(),
            commit: commit.to_string(),
        }
    }

    #[test]
    fn effective_set_is_global_minus_disabled_plus_project() {
        let global = vec![
            install("m1", ItemKind::Agent, "reviewer", "a"),
            install("m1", ItemKind::Hook, "notify", "a"),
        ];
        let disabled = vec![MarketplaceItemRef {
            marketplace_id: "m1".into(),
            kind: ItemKind::Hook,
            key: "notify".into(),
        }];
        let project = vec![install("m2", ItemKind::Skill, "tidy", "b")];

        let got = effective_installs(&global, &disabled, &project);

        assert_eq!(
            got,
            vec![
                install("m1", ItemKind::Agent, "reviewer", "a"),
                install("m2", ItemKind::Skill, "tidy", "b"),
            ]
        );
    }

    #[test]
    fn project_pin_wins_over_global_pin() {
        let global = vec![install("m1", ItemKind::Agent, "reviewer", "old")];
        let project = vec![install("m1", ItemKind::Agent, "reviewer", "new")];
        let got = effective_installs(&global, &[], &project);
        assert_eq!(got, vec![install("m1", ItemKind::Agent, "reviewer", "new")]);
    }

    #[test]
    fn same_key_different_kind_are_different_items() {
        let global = vec![
            install("m1", ItemKind::Agent, "x", "a"),
            install("m1", ItemKind::Command, "x", "a"),
        ];
        assert_eq!(effective_installs(&global, &[], &[]).len(), 2);
    }

    #[test]
    fn item_keys_follow_the_pattern() {
        for ok in ["a", "code-reviewer", "A.b_c-9", &"x".repeat(64)] {
            assert!(is_valid_item_key(ok), "{ok} should be valid");
        }
        for bad in [
            "",
            ".hidden",
            "-flag",
            "_x",
            "a/b",
            "a b",
            "a;rm",
            "$(x)",
            "ä",
            "..",
            &"x".repeat(65),
        ] {
            assert!(!is_valid_item_key(bad), "{bad:?} should be invalid");
        }
    }

    #[test]
    fn slug_is_derived_from_the_id_only() {
        assert_eq!(
            marketplace_slug("7C9E6679-7425-40de-944b-e07fc1f90ae7"),
            "mp-7c9e6679"
        );
        assert_eq!(marketplace_slug("1A2B-3C4D-ffff"), "mp-1a2b3c4d");
        assert_eq!(marketplace_slug("ab"), "mp-ab");
        assert_eq!(marketplace_slug("--"), "mp-marketplace");
    }

    #[test]
    fn commits_must_be_full_lowercase_hex() {
        assert!(is_valid_commit(&"a".repeat(40)));
        assert!(!is_valid_commit(&"A".repeat(40)));
        assert!(!is_valid_commit(&"a".repeat(39)));
        assert!(!is_valid_commit("HEAD"));
    }

    #[test]
    fn install_scope_serialises_tagged() {
        assert_eq!(
            serde_json::to_value(InstallScope::Global).unwrap(),
            serde_json::json!({"type": "global"})
        );
        assert_eq!(
            serde_json::to_value(InstallScope::Project {
                project_id: "p".into()
            })
            .unwrap(),
            serde_json::json!({"type": "project", "project_id": "p"})
        );
    }

    #[test]
    fn kinds_serialise_snake_case() {
        assert_eq!(serde_json::to_value(ItemKind::Plugin).unwrap(), "plugin");
        assert_eq!(
            serde_json::to_value(AccountMethod::GhHost).unwrap(),
            "gh_host"
        );
    }

    #[test]
    fn settings_and_projects_saved_before_the_marketplace_still_load() {
        let mut settings = serde_json::to_value(crate::models::AppSettings::default()).unwrap();
        for key in [
            "marketplace_accounts",
            "marketplaces",
            "global_marketplace_installs",
        ] {
            settings.as_object_mut().unwrap().remove(key);
        }
        let settings: crate::models::AppSettings = serde_json::from_value(settings).unwrap();
        assert!(settings.marketplace_accounts.is_empty());
        assert!(settings.marketplaces.is_empty());
        assert!(settings.global_marketplace_installs.is_empty());

        let mut project =
            serde_json::to_value(crate::models::Project::new("p".to_string(), Vec::new())).unwrap();
        for key in ["marketplace_installs", "marketplace_disabled"] {
            project.as_object_mut().unwrap().remove(key);
        }
        let project: crate::models::Project = serde_json::from_value(project).unwrap();
        assert!(project.marketplace_installs.is_empty());
        assert!(project.marketplace_disabled.is_empty());
    }
}
