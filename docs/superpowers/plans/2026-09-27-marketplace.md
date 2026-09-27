# Triple-C Marketplace Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Users add git-repo marketplaces in Settings, browse agents/skills/commands/hooks/plugins, and install them for all projects or per project; installs are pinned and synced into containers on start.

**Architecture:** The host fetches each marketplace into a bare `gix` cache and reads item files straight from git objects at pinned commits. On container start (and on "Apply now") the host builds one tar of the project's effective install set, uploads it, and runs a constant sync script (shipped inside the app and uploaded alongside the payload) that copies files, merges hook entries with `jq`, and drives `claude plugin`. Credentials live in the OS keychain (or are fetched live from host `gh`) and never enter containers.

**Tech Stack:** Rust (Tauri 2, gix 0.88, bollard 0.18, keyring 3, reqwest 0.12, similar), React 19 + TypeScript + Zustand + Tailwind, Vitest, POSIX sh + jq in the container.

**Spec:** `docs/superpowers/specs/2026-09-27-marketplace-design.md` — read it before starting any task.

## Global Constraints

- Branch: `feat/marketplace` in `/workspace/triple-c` (already contains the SharedAuthSettings `flex-wrap` fix `ece0d74` and the spec `a6b00e0`).
- Item kinds: exactly `agent`, `skill`, `command`, `hook`, `plugin` (serde `snake_case`).
- Item key pattern: `^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$`.
- Per-item limits: 2 MiB total, 200 files; any symlink in an item makes it invalid.
- Marketplace URLs: `https://` only. Plugins' catalog `source` must be a relative path inside `plugins/`.
- Hook placeholder: `${HOOK_DIR}` → `/home/claude/.claude/triple-c/hooks/<key>`.
- Container paths: payload `/home/claude/.claude/triple-c/marketplace/incoming/`, state `/home/claude/.claude/triple-c/marketplace/state.json`, hooks `/home/claude/.claude/triple-c/hooks/<key>/`, plugin trees `/home/claude/.claude/triple-c/plugins/<slug>/`, plugin marketplace name `triple-c-<slug>`.
- Readiness: poll `pgrep -x -f 'su -s /bin/bash claude -c exec sleep infinity'` (as root) every 2 s, up to 180 s. No marker file and **no changes to `container/`** — image/entrypoint changes never reach existing projects (CLAUDE.md). Plugin commands run under `flock /tmp/.triple-c-claude-update.lock` when `flock` exists.
- The sync script is `app/src-tauri/src/marketplace/sync.sh`, embedded with `include_str!` and uploaded next to `payload.tar` on every sync.
- Cache: `<dirs::data_dir()>/triple-c/marketplaces/<marketplace_id>.git` (bare); fetched tip stored at `refs/triple-c/head`; pins at `refs/triple-c/pins/<commit>`.
- Keychain service per account: `triple-c-marketplace-account-<account_id>`, account `secret` (existing `KEYCHAIN_ACCOUNT`).
- Refresh: on Marketplace tab open when last fetch > 15 min, on Refresh, once at app start.
- GitHub fetch username `x-access-token`; other hosts: account `username`, else `oauth2`.
- New Tauri command = `#[tauri::command]` + `generate_handler!` entry in `lib.rs` + `"allow-<name-with-dashes>"` in `app/src-tauri/capabilities/default.json` + wrapper in `app/src/lib/tauri-commands.ts` (+ types in `app/src/lib/types.ts`). Only `lib/tauri-commands.ts` may import `@tauri-apps/api/core`.
- All new serde fields `#[serde(default)]`. No secrets in `settings.json`, `projects.json`, container labels, logs, events or test output. Test fixtures must not look like live tokens (the pre-commit secret scan rejects `ghp_…`, `gho_…` etc.) — use `test-token-not-real`.
- Errors are `Result<T, String>`; UI copy says changes apply to **new** Claude sessions.
- Commit after every task; messages end with `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.
- Test commands: `cd app/src-tauri && cargo test --lib <filter>`; `cd app && npx vitest run <path>`. Run `cargo fmt` only on files you touch (the repo is not globally rustfmt-clean): `rustfmt --edition 2021 <file>`.
- Note for local runs in the dev container only: `SSL_CERT_FILE` is set to an empty string there, which breaks rustls cert loading for HTTPS tests; run with `env -u SSL_CERT_FILE`. Unit tests use `file://` fixtures and are unaffected.

## Review Focus

1. **A user already has an agent/skill/command file with the same name as a marketplace item** → the sync must skip it and report a conflict, never overwrite (Task 8 test `sync_skips_user_owned_agent`).
2. **User-authored hooks in `~/.claude/settings.json`** (and the entrypoint's managed-settings merge) → a sync, an update and an uninstall must leave them byte-identical in meaning (Task 8 test `sync_preserves_user_hooks`).
3. **Marketplace fetch fails (offline, 401/403/404)** → the last cache remains browsable and installs keep syncing from cached pins; the error names the account and org causes (Task 4 `fetch_error_mapping`, Task 6 `refresh_failure_keeps_snapshot`).
4. **Container not ready / sync script fails** → container start still succeeds; the report is stored and surfaced (Task 9 `sync_failure_does_not_fail_start`).
5. **Hostile repo content** (symlinks, `../` in plugin sources, oversized items, keys with shell metacharacters) → item marked invalid, never copied, never interpolated into a shell (Task 3 tests `rejects_symlink_items`, `rejects_escaping_plugin_source`, `rejects_bad_keys`, `enforces_item_limits`).

---

## File Structure

Backend (`app/src-tauri/`):

| File | Responsibility |
|---|---|
| `Cargo.toml` | add `gix`, `similar`; dev-dep `tempfile` |
| `src/models/marketplace.rs` | serde types, key validation, slug, effective-set merge |
| `src/models/mod.rs`, `models/app_settings.rs`, `models/project.rs` | new fields |
| `src/marketplace/mod.rs` | module root; `MarketplaceManager` (in-memory snapshots, sync reports, paths) |
| `src/marketplace/tree.rs` | `TreeView` trait, `MemTree` (tests), `GitTree` (gix) |
| `src/marketplace/catalog.rs` | parse repo → `CatalogItem`s; item files; fingerprints; validation |
| `src/marketplace/git.rs` | gix fetch w/ credentials, head, pin refs, error mapping |
| `src/marketplace/auth.rs` | credential resolution (host gh / keychain), token validation (who-am-I) |
| `src/marketplace/gh_login.rs` | `gh auth login --web` inside a container (attached pty exec) |
| `src/marketplace/diff.rs` | per-item text diff between two commits |
| `src/marketplace/payload.rs` | build the tar for a project's effective set (+ generated plugin catalog, manifest) |
| `src/marketplace/sync.rs` | wait-ready, upload, run script, parse report, persist report |
| `src/storage/secure.rs` | marketplace account token helpers |
| `src/commands/marketplace_commands.rs` | all Tauri commands |
| `src/commands/project_commands.rs` | call sync after start |
| `src/lib.rs` | `mod marketplace;`, `AppState.marketplace`, handlers, startup refresh |
| `capabilities/default.json` | grants |
| `src/marketplace/sync.sh` | the constant sync script (embedded, uploaded each sync) |
| `src/marketplace/sync_script_tests.rs` | drives the real script against a temp HOME |

Frontend (`app/src/`):

| File | Responsibility |
|---|---|
| `lib/types.ts`, `lib/tauri-commands.ts` | mirrors + wrappers |
| `store/appState.ts` | `MARKETPLACE_TAB_KEY`, `openMarketplace`, `closeMarketplaceTab`, `marketplaceFilterProjectId` |
| `App.tsx`, `components/layout/MainTabs.tsx`, `hooks/useKeyboardShortcuts.ts`, `components/layout/NotesDock.tsx` | render/label/close the singleton tab |
| `hooks/useMarketplace.ts` | load/refresh snapshots, accounts, updates; mutations |
| `components/settings/MarketplaceSettings.tsx` | sidebar summary + Open Marketplace |
| `components/marketplace/MarketplaceView.tsx` | tab shell with Browse / Installed / Accounts |
| `components/marketplace/BrowsePane.tsx`, `ItemDetail.tsx`, `InstallControls.tsx`, `HookConfirmModal.tsx` | browse + install |
| `components/marketplace/AddMarketplaceModal.tsx` | add repo |
| `components/marketplace/InstalledPane.tsx`, `UpdateDiffModal.tsx` | installed list, updates, apply now |
| `components/marketplace/AccountsPane.tsx`, `AddAccountModal.tsx`, `GhContainerLoginModal.tsx` | accounts |
| `components/projects/home/config/MarketplaceSection.tsx` | per-project effective set + opt-out |
| `lib/marketplace.ts` | pure helpers: item state per project, grouping |

Starter repo: `/workspace/projects/triple-c-marketplace` → `github.com/shadowdao/triple-c-marketplace` (public).

## Interface Contract

Every task uses these exact names. Rust first, TypeScript mirror after.

### `src/models/marketplace.rs`

```rust
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemKind { Agent, Skill, Command, Hook, Plugin }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccountMethod { GhHost, GhContainer, Token }

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
    pub fn item_ref(&self) -> MarketplaceItemRef;
}

/// `(global − disabled) ∪ project`; project wins on clash; sorted by item_ref.
pub fn effective_installs(
    global: &[MarketplaceInstall],
    disabled: &[MarketplaceItemRef],
    project: &[MarketplaceInstall],
) -> Vec<MarketplaceInstall>;

pub fn is_valid_item_key(key: &str) -> bool;
/// lowercase, [a-z0-9-] only, collapsed dashes, ≤ 32 chars, then "-" + first 8 chars of id.
pub fn marketplace_slug(name: &str, id: &str) -> String;
pub fn is_valid_commit(commit: &str) -> bool; // 40 lowercase hex

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogItem {
    pub kind: ItemKind,
    pub key: String,
    pub name: String,
    pub description: String,
    /// repo-relative path of the item (file or folder)
    pub path: String,
    /// Some(reason) when the item cannot be installed
    pub invalid: Option<String>,
    /// hooks only: rendered commands with ${HOOK_DIR} substituted
    #[serde(default)]
    pub hook_commands: Vec<String>,
    /// agents/commands/skills: the markdown body (≤ 64 KiB, truncated); plugins: component listing
    #[serde(default)]
    pub preview: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct MarketplaceSnapshot {
    pub marketplace_id: String,
    pub head_commit: Option<String>,
    /// RFC 3339
    pub fetched_at: Option<String>,
    pub fetch_error: Option<String>,
    pub items: Vec<CatalogItem>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ItemUpdate {
    pub item: MarketplaceItemRef,
    pub pinned: String,
    pub head: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileChange { Added, Removed, Modified }

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileDiff {
    pub path: String,
    pub change: FileChange,
    /// unified diff text; None when either side is binary
    pub unified: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct SkippedItem { pub item: String, pub reason: String }

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct SyncReport {
    #[serde(default)] pub installed: Vec<String>,
    #[serde(default)] pub updated: Vec<String>,
    #[serde(default)] pub removed: Vec<String>,
    #[serde(default)] pub skipped: Vec<SkippedItem>,
    #[serde(default)] pub errors: Vec<String>,
    /// RFC 3339, set by the host
    #[serde(default)] pub finished_at: String,
}
```

Item strings in `SyncReport` are `"<kind>:<key>"` (e.g. `"agent:code-reviewer"`).

New fields: `AppSettings { marketplace_accounts: Vec<MarketplaceAccount>, marketplaces: Vec<Marketplace>, global_marketplace_installs: Vec<MarketplaceInstall> }`; `Project { marketplace_installs: Vec<MarketplaceInstall>, marketplace_disabled: Vec<MarketplaceItemRef> }` — all `#[serde(default)]`, listed explicitly in `Default` impls / project constructors.

### `src/marketplace/tree.rs`

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind { File, Dir, Symlink, Other }

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirEntry { pub name: String, pub kind: EntryKind, pub executable: bool }

pub trait TreeView {
    /// Entries of the directory at `path` ("" = root). Ok(None) if absent or not a dir.
    fn list_dir(&self, path: &str) -> Result<Option<Vec<DirEntry>>, String>;
    /// Contents of the regular file at `path`. Ok(None) if absent or not a file.
    fn read_file(&self, path: &str) -> Result<Option<Vec<u8>>, String>;
    /// Stable content id of the entry at `path` (git object id hex); None if absent.
    fn entry_id(&self, path: &str) -> Result<Option<String>, String>;
}

/// In-memory tree for tests: path → (bytes, executable). Dirs are implied; symlinks via `add_symlink`.
pub struct MemTree { /* private */ }
impl MemTree {
    pub fn new() -> Self;
    pub fn file(self, path: &str, contents: &str) -> Self;
    pub fn exec_file(self, path: &str, contents: &str) -> Self;
    pub fn symlink(self, path: &str, target: &str) -> Self;
}
impl TreeView for MemTree { /* entry_id = sha256 hex of path-sorted contents */ }

/// A tree at a commit in a bare gix repo.
pub struct GitTree { /* private: repo + tree id */ }
impl GitTree {
    pub fn open(repo_path: &std::path::Path, commit: &str) -> Result<Self, String>;
}
impl TreeView for GitTree {}
```

### `src/marketplace/catalog.rs`

```rust
pub const MAX_ITEM_BYTES: u64 = 2 * 1024 * 1024;
pub const MAX_ITEM_FILES: usize = 200;

/// One file of an item, path relative to the item root (for single-file items: the file name).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemFile { pub rel_path: String, pub data: Vec<u8>, pub executable: bool }

/// Parse every item in the repo. Never fails as a whole; broken items carry `invalid`.
pub fn parse_catalog(tree: &dyn TreeView) -> Vec<CatalogItem>;

/// All files of one item. Err if the item is missing/invalid or breaks the limits.
pub fn item_files(tree: &dyn TreeView, kind: ItemKind, key: &str) -> Result<Vec<ItemFile>, String>;

/// Content fingerprint used for update detection (changes iff the item's files or,
/// for plugins, its catalog entry change).
pub fn item_fingerprint(tree: &dyn TreeView, kind: ItemKind, key: &str) -> Result<Option<String>, String>;

/// Plugins only: the plugin's entry from plugins/.claude-plugin/marketplace.json.
pub fn plugin_catalog_entry(tree: &dyn TreeView, key: &str) -> Result<serde_json::Value, String>;

/// Hooks only: parsed hook.json `hooks` object with ${HOOK_DIR} substituted.
pub fn rendered_hook_settings(tree: &dyn TreeView, key: &str) -> Result<serde_json::Value, String>;

pub fn hook_dir(key: &str) -> String; // "/home/claude/.claude/triple-c/hooks/<key>"
```

### `src/marketplace/git.rs` (blocking; call from `tokio::task::spawn_blocking`)

```rust
#[derive(Clone)]
pub struct Credential { pub username: String, pub password: String }
impl std::fmt::Debug for Credential { /* prints username and "<redacted>" */ }

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FetchError {
    Auth { status: u16 },   // 401 / 403 or "credentials … not accepted"
    NotFound,               // 404 / "repository not found"
    Network(String),
    Other(String),
}
impl std::fmt::Display for FetchError {}

pub fn cache_path(data_root: &std::path::Path, marketplace_id: &str) -> std::path::PathBuf;
/// Init the bare repo if missing, fetch branch (or remote HEAD) into refs/triple-c/head, return head commit hex.
pub fn fetch(repo_path: &std::path::Path, url: &str, branch: Option<&str>, cred: Option<Credential>) -> Result<String, FetchError>;
/// Current refs/triple-c/head, if fetched before.
pub fn cached_head(repo_path: &std::path::Path) -> Result<Option<String>, String>;
/// Make refs/triple-c/pins/* exactly the given set.
pub fn set_pins(repo_path: &std::path::Path, commits: &[String]) -> Result<(), String>;
pub fn has_commit(repo_path: &std::path::Path, commit: &str) -> bool;
```

### `src/marketplace/auth.rs`

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostKind { GitHub, Gitea, GitLab, Unknown }
pub fn host_kind(host: &str) -> HostKind; // github.com → GitHub, gitlab.com → GitLab, else Unknown (Gitea detected by probe)
pub fn host_of(url: &str) -> Result<String, String>; // https only
pub fn fetch_username(account: &MarketplaceAccount) -> String; // x-access-token for GitHub, else username or "oauth2"
/// Resolve the credential for an account: GhHost → `gh auth token --hostname <host>`; others → keychain.
pub async fn resolve_credential(account: &MarketplaceAccount) -> Result<crate::marketplace::git::Credential, String>;
/// "Who am I" check; returns the login name.
pub async fn validate_token(host: &str, token: &str) -> Result<String, String>;
pub async fn gh_host_available() -> bool;
pub async fn gh_host_login(host: &str) -> Result<String, String>; // `gh api user --jq .login` when logged in; Err with instructions otherwise
/// User-facing message for a FetchError, naming the account and org causes.
pub fn describe_fetch_error(err: &crate::marketplace::git::FetchError, account: Option<&MarketplaceAccount>, url: &str) -> String;
```

`storage/secure.rs` additions:

```rust
pub fn store_marketplace_token(account_id: &str, token: &str) -> Result<(), String>;
pub fn get_marketplace_token(account_id: &str) -> Result<Option<String>, String>;
pub fn delete_marketplace_token(account_id: &str) -> Result<(), String>;
```

### `src/marketplace/gh_login.rs`

Events (payload always has `account_id`):
- `marketplace-gh-login-code` → `{ account_id, code, url }`
- `marketplace-gh-login-output` → `{ account_id, chunk }` (redacted, ANSI-stripped)

```rust
/// Runs `gh auth login` in the container with a temp GH_CONFIG_DIR, returns the token.
pub async fn run_gh_container_login(app: &tauri::AppHandle, account_id: &str, container_id: &str, host: &str, cancel: tokio::sync::oneshot::Receiver<()>) -> Result<String, String>;
/// Parse gh's "First copy your one-time code: XXXX-XXXX" and URL out of accumulated output.
pub fn parse_device_prompt(output: &str) -> Option<(String, String)>;
```

### `src/marketplace/diff.rs`

```rust
pub fn item_diff(repo_path: &std::path::Path, kind: ItemKind, key: &str, from_commit: &str, to_commit: &str) -> Result<Vec<FileDiff>, String>;
```

### `src/marketplace/payload.rs`

```rust
pub struct PayloadInput<'a> {
    pub installs: &'a [MarketplaceInstall],
    pub marketplaces: &'a [Marketplace],
    /// data root used to find caches (see git::cache_path)
    pub data_root: &'a std::path::Path,
}
pub struct Payload { pub tar: Vec<u8>, pub manifest: serde_json::Value, pub skipped: Vec<SkippedItem> }
/// Build the tar described in spec §4. Items whose marketplace/cache/commit is missing go to `skipped`.
pub fn build_payload(input: &PayloadInput) -> Result<Payload, String>;
```

Manifest shape (`manifest.json` at the tar root):

```json
{
  "version": 1,
  "items": [
    { "kind": "agent",   "key": "code-reviewer", "marketplace": "<id>", "commit": "<sha>", "file": "agents/code-reviewer.md" },
    { "kind": "skill",   "key": "example-skill", "marketplace": "<id>", "commit": "<sha>", "dir": "skills/example-skill" },
    { "kind": "command", "key": "example-command", "marketplace": "<id>", "commit": "<sha>", "file": "commands/example-command.md" },
    { "kind": "hook",    "key": "notify-on-stop", "marketplace": "<id>", "commit": "<sha>", "dir": "hooks/notify-on-stop", "settings": { "Stop": [ ... ] } },
    { "kind": "plugin",  "key": "example-plugin", "marketplace": "<id>", "commit": "<sha>", "slug": "<slug>" }
  ],
  "plugin_marketplaces": [ { "slug": "<slug>", "dir": "plugins/<slug>", "plugins": ["example-plugin"] } ]
}
```

Tar layout: `agents/<key>.md`, `skills/<key>/…`, `commands/<key>.md`, `hooks/<key>/…`, `plugins/<slug>/.claude-plugin/marketplace.json` (generated: `{"name":"triple-c-<slug>","owner":{"name":"Triple-C"},"plugins":[<entries with source rewritten to "./<key>">]}`), `plugins/<slug>/<key>/…`, `manifest.json`. Modes: 0644, or 0755 when executable; dirs 0755.

### `src/marketplace/sync.rs`

```rust
pub const INCOMING_DIR: &str = "/home/claude/.claude/triple-c/marketplace/incoming";
pub const SYNC_SCRIPT: &str = include_str!("sync.sh");
/// Wait for readiness (pgrep, see Global Constraints), upload payload.tar + sync.sh, run `sh sync.sh` as claude, parse its JSON report.
pub async fn sync_container(container_id: &str, payload: &Payload) -> Result<SyncReport, String>;
/// Parse the script's stdout (last line is the JSON report).
pub fn parse_report(stdout: &str) -> Result<SyncReport, String>;
```

### `src/marketplace/mod.rs`

```rust
pub mod auth; pub mod catalog; pub mod diff; pub mod gh_login; pub mod git; pub mod payload; pub mod sync; pub mod tree;
#[cfg(test)] mod sync_script_tests;

pub struct MarketplaceManager {
    // private: data_root, snapshots: Mutex<HashMap<String, MarketplaceSnapshot>>, reports: Mutex<HashMap<String, SyncReport>>, gh_login_cancel: tokio::sync::Mutex<Option<oneshot::Sender<()>>>
}
impl MarketplaceManager {
    pub fn new(data_root: std::path::PathBuf) -> Self; // data_root = <data_dir>/triple-c
    pub fn data_root(&self) -> &std::path::Path;
    pub fn snapshot(&self, marketplace_id: &str) -> Option<MarketplaceSnapshot>;
    pub fn put_snapshot(&self, snap: MarketplaceSnapshot);
    pub fn remove_snapshot(&self, marketplace_id: &str);
    /// Reports are also persisted to <data_root>/marketplace-sync/<project_id>.json
    pub fn report(&self, project_id: &str) -> Option<SyncReport>;
    pub fn put_report(&self, project_id: &str, report: SyncReport);
    pub async fn set_gh_login_cancel(&self, tx: Option<tokio::sync::oneshot::Sender<()>>) -> bool; // false if one already running
    pub async fn cancel_gh_login(&self);
}

/// Refresh one marketplace: resolve credential, fetch (blocking task), parse catalog at head, store snapshot.
/// On fetch failure keep the previous items and head, set fetch_error.
pub async fn refresh_marketplace(mgr: &MarketplaceManager, settings: &crate::models::AppSettings, marketplace_id: &str) -> MarketplaceSnapshot;
/// Load snapshot from the cache without network (used at startup and after install when no snapshot is in memory).
pub fn load_cached_snapshot(mgr: &MarketplaceManager, marketplace: &Marketplace) -> MarketplaceSnapshot;
/// Every install (global + all projects) whose item fingerprint at head differs from its pin.
pub fn compute_updates(mgr: &MarketplaceManager, settings: &crate::models::AppSettings, projects: &[crate::models::Project]) -> Vec<ItemUpdate>;
/// All commits referenced by installs, per marketplace (for git::set_pins).
pub fn pins_by_marketplace(settings: &crate::models::AppSettings, projects: &[crate::models::Project]) -> std::collections::HashMap<String, Vec<String>>;
/// Build payload for a project and sync it into its running container; stores the report.
pub async fn sync_project(mgr: &MarketplaceManager, settings: &crate::models::AppSettings, project: &crate::models::Project, container_id: &str) -> SyncReport;
```

`AppState` gains `pub marketplace: Arc<marketplace::MarketplaceManager>`.

### Tauri commands (`src/commands/marketplace_commands.rs`)

| Command | Args (Rust) | Returns |
|---|---|---|
| `list_marketplace_snapshots` | – | `Vec<MarketplaceSnapshot>` (one per configured marketplace; cached or empty) |
| `refresh_marketplaces` | `marketplace_id: Option<String>` | `Vec<MarketplaceSnapshot>` |
| `add_marketplace` | `name: String, url: String, branch: Option<String>, account_id: Option<String>` | `MarketplaceSnapshot` (test fetch first; nothing saved on failure) |
| `update_marketplace` | `marketplace: Marketplace` | `AppSettings` |
| `remove_marketplace` | `marketplace_id: String` | `AppSettings` |
| `install_marketplace_item` | `item: MarketplaceItemRef, scope: InstallScope` | `AppSettings` (global) — frontend reloads projects for project scope |
| `uninstall_marketplace_item` | `item: MarketplaceItemRef, scope: InstallScope` | `()` |
| `set_global_item_disabled` | `project_id: String, item: MarketplaceItemRef, disabled: bool` | `Project` |
| `forget_marketplace_installs` | `marketplace_id: String` | `()` (drops installs of a removed marketplace everywhere) |
| `list_marketplace_updates` | – | `Vec<ItemUpdate>` |
| `marketplace_item_diff` | `item: MarketplaceItemRef, from_commit: String, to_commit: String` | `Vec<FileDiff>` |
| `update_marketplace_item` | `item: MarketplaceItemRef, scope: InstallScope` | `()` (moves that install's pin to head) |
| `apply_marketplace_now` | `project_id: Option<String>` | `Vec<ProjectSyncResult>` (all running projects when None) |
| `get_marketplace_sync_report` | `project_id: String` | `Option<SyncReport>` |
| `add_marketplace_token_account` | `label: String, host: String, token: String` | `MarketplaceAccount` |
| `add_marketplace_gh_host_account` | `label: String, host: String` | `MarketplaceAccount` |
| `start_marketplace_gh_container_login` | `label: String, host: String, project_id: String` | `MarketplaceAccount` (long-running; emits events) |
| `cancel_marketplace_gh_login` | – | `()` |
| `test_marketplace_account` | `account_id: String` | `String` (login name) |
| `remove_marketplace_account` | `account_id: String` | `AppSettings` |
| `marketplace_gh_host_available` | – | `bool` |

```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum InstallScope { Global, Project { project_id: String } }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectSyncResult { pub project_id: String, pub report: SyncReport }
```

(`InstallScope` and `ProjectSyncResult` live in `models/marketplace.rs`.)

### TypeScript mirror (`app/src/lib/types.ts`)

```ts
export type ItemKind = "agent" | "skill" | "command" | "hook" | "plugin";
export type AccountMethod = "gh_host" | "gh_container" | "token";
export interface MarketplaceAccount { id: string; label: string; host: string; method: AccountMethod; username: string | null; }
export interface Marketplace { id: string; name: string; url: string; branch: string | null; account_id: string | null; }
export interface MarketplaceItemRef { marketplace_id: string; kind: ItemKind; key: string; }
export interface MarketplaceInstall extends MarketplaceItemRef { commit: string; }
export interface CatalogItem { kind: ItemKind; key: string; name: string; description: string; path: string; invalid: string | null; hook_commands: string[]; preview: string; }
export interface MarketplaceSnapshot { marketplace_id: string; head_commit: string | null; fetched_at: string | null; fetch_error: string | null; items: CatalogItem[]; }
export interface ItemUpdate { item: MarketplaceItemRef; pinned: string; head: string; }
export type FileChange = "added" | "removed" | "modified";
export interface FileDiff { path: string; change: FileChange; unified: string | null; }
export interface SkippedItem { item: string; reason: string; }
export interface SyncReport { installed: string[]; updated: string[]; removed: string[]; skipped: SkippedItem[]; errors: string[]; finished_at: string; }
export type InstallScope = { type: "global" } | { type: "project"; project_id: string };
export interface ProjectSyncResult { project_id: string; report: SyncReport; }
// AppSettings += marketplace_accounts: MarketplaceAccount[]; marketplaces: Marketplace[]; global_marketplace_installs: MarketplaceInstall[];
// Project += marketplace_installs: MarketplaceInstall[]; marketplace_disabled: MarketplaceItemRef[];
```

Wrappers in `lib/tauri-commands.ts` (camelCase args): `listMarketplaceSnapshots()`, `refreshMarketplaces(marketplaceId?: string)`, `addMarketplace(name, url, branch: string | null, accountId: string | null)`, `updateMarketplace(marketplace)`, `removeMarketplace(marketplaceId)`, `installMarketplaceItem(item, scope)`, `uninstallMarketplaceItem(item, scope)`, `setGlobalItemDisabled(projectId, item, disabled)`, `forgetMarketplaceInstalls(marketplaceId)`, `listMarketplaceUpdates()`, `marketplaceItemDiff(item, fromCommit, toCommit)`, `updateMarketplaceItem(item, scope)`, `applyMarketplaceNow(projectId?: string)`, `getMarketplaceSyncReport(projectId)`, `addMarketplaceTokenAccount(label, host, token)`, `addMarketplaceGhHostAccount(label, host)`, `startMarketplaceGhContainerLogin(label, host, projectId)`, `cancelMarketplaceGhLogin()`, `testMarketplaceAccount(accountId)`, `removeMarketplaceAccount(accountId)`, `marketplaceGhHostAvailable()`.

### `app/src/lib/marketplace.ts`

```ts
export type ProjectItemState = "none" | "inherited" | "opted_out" | "project" | "project_pinned_differently";
export const itemRefKey = (r: MarketplaceItemRef) => `${r.marketplace_id}/${r.kind}/${r.key}`;
export function projectItemState(item: MarketplaceItemRef, globalInstalls: MarketplaceInstall[], project: Project): ProjectItemState;
export function effectiveInstalls(globalInstalls: MarketplaceInstall[], project: Project): (MarketplaceInstall & { source: "global" | "project" })[];
export const KIND_LABELS: Record<ItemKind, string>; // Agents, Skills, Commands, Hooks, Plugins
```

### Store (`app/src/store/appState.ts`)

```ts
export const MARKETPLACE_TAB_KEY = "marketplace";
export const isMarketplaceTab = (key: string) => key === MARKETPLACE_TAB_KEY;
// state + actions
marketplaceFilterProjectId: string | null;
openMarketplace: (filterProjectId?: string | null) => void;
closeMarketplaceTab: () => void;
```

---

## Tasks

### Contract amendments (these override the Interface Contract above where they differ)

From Tasks 2–5:

1. `auth::validate_token(host, token)` returns `Result<Option<String>, String>` (not `Result<String, String>`). `Ok(Some(login))` = host confirmed the token; `Ok(None)` = host is not GitHub/Gitea/GitLab, so the token is unchecked and the marketplace's test fetch proves it. Tasks 11/15 must handle `None` (store the account with `username: None`).
2. Additions (no renames): `ItemKind::as_str()` ("agent"…"plugin", for report strings and the manifest); `git::HEAD_REF`, `git::PIN_PREFIX`, `pub fn git::classify_fetch_error(&str) -> FetchError`; test-only fixtures `git::test_support::{git_available, git, init_repo, commit_files, file_url}` (`#[cfg(test)] pub(crate)`) that later tasks' tests (Task 6 refresh, Task 7 payload, Task 11 diff) should reuse.
3. `GitTree` lives in `tree.rs` as the contract says, but is added in Task 4 together with the `gix` dependency; Task 3's `tree.rs` has `TreeView` + `MemTree` only.
4. `models/mod.rs` gets `pub mod marketplace; pub use marketplace::*;` (checked: no name clashes with existing models).
5. Until Task 11 wires the new modules into commands, `cargo build` prints dead-code warnings for them. That is expected; do not add `allow` attributes.

From Tasks 6–11:

1. **Sync script is shipped by the app, not the image** (lead correction). `container/` is not touched. The script is `app/src-tauri/src/marketplace/sync.sh`, embedded as `pub const SYNC_SCRIPT: &str = include_str!("sync.sh");` in `sync.rs`, uploaded as `<INCOMING_DIR>/sync.sh` (mode 0755) next to `payload.tar` and run as `sh <INCOMING_DIR>/sync.sh`. `SYNC_SCRIPT_PATH` and `READY_MARKER` are dropped. Readiness = polling (every 2 s, ≤ 180 s, as root) `pgrep -x -f 'su -s /bin/bash claude -c exec sleep infinity' >/dev/null`.
2. The script honours two env overrides used only by tests: `MARKETPLACE_INCOMING` (default `$HOME/.claude/triple-c/marketplace/incoming`) and `MARKETPLACE_LOCK` (default `/tmp/.triple-c-claude-update.lock`).
3. `gh_login::parse_device_prompt(output: &str, host: &str) -> Option<(String, String)>` takes the host: gh prints "Press Enter to open github.com in your browser", not a URL, so the URL falls back to `https://<host>/login/device`. New pure helpers `gh_login::extract_token(text) -> Option<String>` and `gh_login::take_display_lines(pending: &mut String, chunk: &str) -> String`.
4. `gh auth login` runs with `--git-protocol ssh --skip-ssh-key` and `GIT_CONFIG_GLOBAL` inside the temp dir: with `https` gh asks "Authenticate Git with your GitHub credentials?" and would write a credential helper into `~/.gitconfig`. The host reaches the script as `$1` (argv), because `create_attached_exec_as` has no env parameter.
5. New in `sync.rs`: `pub fn report_from_result(r: Result<SyncReport, String>) -> SyncReport`. New in `marketplace/mod.rs`: `pub const SYNC_FINISHED_EVENT: &str = "marketplace-sync-finished";` (payload `{ project_id, report }`), `pub fn should_sync(mgr, settings, project) -> bool`, `pub fn spawn_project_sync(app: tauri::AppHandle, mgr: Arc<MarketplaceManager>, settings: AppSettings, project: Project, container_id: String)`, `pub fn head_for(mgr: &MarketplaceManager, m: &Marketplace) -> Option<String>`.
6. Container-start sync runs **in the background** (spawned), because it waits for the entrypoint (which may spend up to 120 s in `claude update`); the start command never waits on it.
7. `AppState.marketplace` is wired in **Task 6** (Task 9's start hook needs it); Task 11 only adds handlers and the startup refresh.
8. `#[cfg(test)] pub(crate) mod test_support;` with `GitFixture` is added in Task 6. If Task 4 already created an equivalent helper, keep one and adapt call sites.
9. Marketplace fields are **store-owned**: `update_settings` restores `marketplace_accounts`, `marketplaces`, `global_marketplace_installs` from the stored settings, and `update_project` restores `marketplace_installs`, `marketplace_disabled` (a stale frontend copy must not undo an install). `apply_settings_import` writes the imported marketplace fields explicitly after its `update_settings` call.
10. `remove_marketplace_account` refuses while a marketplace uses the account. `apply_marketplace_now(Some(id))` errors when that project is not running.

From Tasks 1, 12–17:

1. New backend event `marketplace-sync-finished`, payload `{ project_id: string, report: SyncReport }`, emitted by the backend after **every** container sync (container start path in Task 9 and `apply_marketplace_now` in Task 11). The frontend (Task 12 `useMarketplaceSyncToasts`) toasts errors/skips from it. Tasks 9/11 must emit it via `app_handle.emit("marketplace-sync-finished", serde_json::json!({ "project_id": id, "report": report }))`.
2. `lib/marketplace.ts` gains `isStale(snapshot: MarketplaceSnapshot, now: number): boolean` (null `fetched_at` or older than 15 min) and `formatItemRef(r: MarketplaceItemRef): string` (`"<kind>:<key>"`, same format as `SyncReport` item strings). Both are frontend-only.
3. GhContainerLoginModal cannot know the new account id before `startMarketplaceGhContainerLogin` resolves, so it accepts **every** `marketplace-gh-login-code` / `marketplace-gh-login-output` event while it is open. This is safe because the backend single-flights the login (`MarketplaceManager::set_gh_login_cancel` returns false when one is running). The `account_id` field is still in the payload but is not used for filtering.

---

### Task 1: Starter marketplace repo

Creates `/workspace/projects/triple-c-marketplace`, a standalone git repo in the format from the spec, and publishes it as the public `github.com/shadowdao/triple-c-marketplace`. It is independent of the app code and doubles as the end-to-end fixture in Task 17.

**Files:**
- Create: `/workspace/projects/triple-c-marketplace/README.md`
- Create: `/workspace/projects/triple-c-marketplace/agents/code-reviewer.md`
- Create: `/workspace/projects/triple-c-marketplace/skills/example-skill/SKILL.md`
- Create: `/workspace/projects/triple-c-marketplace/commands/example-command.md`
- Create: `/workspace/projects/triple-c-marketplace/hooks/notify-on-stop/hook.json`
- Create: `/workspace/projects/triple-c-marketplace/hooks/notify-on-stop/notify.sh`
- Create: `/workspace/projects/triple-c-marketplace/plugins/.claude-plugin/marketplace.json`
- Create: `/workspace/projects/triple-c-marketplace/plugins/example-plugin/.claude-plugin/plugin.json`
- Create: `/workspace/projects/triple-c-marketplace/plugins/example-plugin/skills/hello/SKILL.md`

**Interfaces:**
- Consumes: nothing.
- Produces: a public repo at `https://github.com/shadowdao/triple-c-marketplace.git` (default branch `main`) containing exactly one valid item per kind: `agent:code-reviewer`, `skill:example-skill`, `command:example-command`, `hook:notify-on-stop`, `plugin:example-plugin`.

- [ ] **Step 1: Create the folder and README**

```bash
mkdir -p /workspace/projects/triple-c-marketplace/{agents,skills/example-skill,commands,hooks/notify-on-stop,plugins/.claude-plugin,plugins/example-plugin/.claude-plugin,plugins/example-plugin/skills/hello}
```

`/workspace/projects/triple-c-marketplace/README.md`:

````markdown
# Triple-C Marketplace

A marketplace of Claude Code agents, skills, commands, hooks and plugins for
[Triple-C](https://repo.anhonesthost.net/CyberCoveLLC/Triple-C). Add this repo in
Triple-C under **Settings → Marketplace → Open Marketplace → Add**, then install
items for all projects or for individual projects.

The `plugins/` folder is also a standard Claude Code marketplace, so it works
without Triple-C:

```
/plugin marketplace add shadowdao/triple-c-marketplace/plugins
```

## Layout

```
agents/<name>.md                    Claude Code agent (front matter: name, description)
skills/<name>/SKILL.md (+ files)    Claude Code skill folder
commands/<name>.md                  Claude Code slash command
hooks/<name>/hook.json (+ scripts)  Triple-C hook manifest
plugins/.claude-plugin/marketplace.json
plugins/<plugin>/…                  Claude Code plugins
```

Every folder is optional.

## Item rules

- **Names** (file stem, folder name, plugin name) must match
  `^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$`.
- **No symlinks** anywhere in an item.
- **Limits:** 2 MiB and 200 files per item.
- **Agents** are installed to `~/.claude/agents/<name>.md`. `name` and
  `description` come from the YAML front matter.
- **Skills** are installed to `~/.claude/skills/<name>/`. The folder must contain `SKILL.md`.
- **Commands** are installed to `~/.claude/commands/<name>.md`. The description
  comes from front matter `description`, or the first non-empty line.
- **Hooks** are a folder with a `hook.json`:

  ```json
  {
    "name": "notify-on-stop",
    "description": "Rings the terminal bell when Claude finishes",
    "hooks": {
      "Stop": [{ "hooks": [{ "type": "command", "command": "${HOOK_DIR}/notify.sh" }] }]
    }
  }
  ```

  `hooks` uses Claude Code's `settings.json` hooks format verbatim.
  `${HOOK_DIR}` is replaced with the folder the hook is installed to
  (`~/.claude/triple-c/hooks/<name>`). The whole folder is copied, and files keep
  their executable bit. Triple-C shows every command a hook runs before it is installed.
- **Plugins** are the entries of `plugins/.claude-plugin/marketplace.json`.
  Each `source` must be a relative path inside `plugins/` (for example
  `"./example-plugin"`). Remote sources are not installable through Triple-C,
  because they would bypass pinning.

## Versioning

Triple-C pins every install to the commit it was installed from. Pushing to this
repo never changes a container by itself: Triple-C shows **update available**
for the items whose files changed, and the user reviews a diff before accepting.
````

- [ ] **Step 2: Write the agent, skill and command**

`agents/code-reviewer.md`:

```markdown
---
name: code-reviewer
description: Reviews the current diff for correctness bugs, risky changes and missing tests. Use after finishing a change and before committing.
tools: Read, Grep, Glob, Bash
---

You are a careful code reviewer. Review the uncommitted changes in this repository.

1. Run `git diff` (and `git diff --staged`) to see what changed.
2. For every changed file, read enough surrounding code to understand the change.
3. Report only real problems, most severe first:
   - correctness bugs (wrong logic, unhandled errors, off-by-one, races)
   - security issues (injection, secrets in code, unsafe input handling)
   - behaviour changes without tests
4. For each finding give the file and line, what goes wrong, and a concrete fix.

If you find nothing worth fixing, say so in one line. Do not restate the diff.
```

`skills/example-skill/SKILL.md`:

```markdown
---
name: example-skill
description: Summarises the repository's recent git history. Use when the user asks what changed recently or wants a changelog draft.
---

# Recent changes summary

1. Run `git log --oneline -20`.
2. Group the commits by theme (features, fixes, chores).
3. Write a short bulleted summary per group, newest first.
4. Mention any commit that looks like a revert or a hotfix.
```

`commands/example-command.md`:

```markdown
---
description: Show the files changed on this branch compared with main
---

Run `git diff --stat main...HEAD` and summarise which areas of the codebase this
branch touches, in three bullets or fewer.
```

- [ ] **Step 3: Write the hook**

`hooks/notify-on-stop/hook.json`:

```json
{
  "name": "notify-on-stop",
  "description": "Rings the terminal bell and prints a line when Claude finishes a turn",
  "hooks": {
    "Stop": [
      {
        "hooks": [
          { "type": "command", "command": "${HOOK_DIR}/notify.sh" }
        ]
      }
    ]
  }
}
```

`hooks/notify-on-stop/notify.sh`:

```sh
#!/bin/sh
# Stop hook: ring the terminal bell and leave a line on stderr.
# Portable on purpose: no desktop notification tools exist in the container.
printf '\a' >&2
printf 'Claude finished at %s\n' "$(date '+%H:%M:%S')" >&2
exit 0
```

```bash
chmod +x /workspace/projects/triple-c-marketplace/hooks/notify-on-stop/notify.sh
```

- [ ] **Step 4: Write the plugin marketplace and plugin**

`plugins/.claude-plugin/marketplace.json`:

```json
{
  "name": "triple-c-marketplace",
  "owner": { "name": "shadowdao" },
  "plugins": [
    {
      "name": "example-plugin",
      "source": "./example-plugin",
      "description": "A single-skill example plugin that greets the user"
    }
  ]
}
```

`plugins/example-plugin/.claude-plugin/plugin.json`:

```json
{
  "name": "example-plugin",
  "version": "0.1.0",
  "description": "A single-skill example plugin that greets the user",
  "author": { "name": "shadowdao" }
}
```

`plugins/example-plugin/skills/hello/SKILL.md`:

```markdown
---
name: hello
description: Greets the user and lists the plugin's capabilities. Use when the user says hello to the example plugin.
---

Greet the user by name if you know it, then say that this skill comes from the
`example-plugin` plugin in the Triple-C starter marketplace.
```

- [ ] **Step 5: Validate the plugin marketplace and plugin**

Run:
```bash
cd /workspace/projects/triple-c-marketplace && claude plugin validate plugins && claude plugin validate plugins/example-plugin
```
Expected: both report the manifest as valid (exit status 0). If either reports an error, fix the named field and re-run before continuing.

- [ ] **Step 6: Check the item-rule constraints locally**

Run:
```bash
cd /workspace/projects/triple-c-marketplace && find . -path ./.git -prune -o -type l -print | wc -l && ls agents commands | grep -Ev '^(agents:|commands:|)$' | grep -Evc '^[A-Za-z0-9][A-Za-z0-9._-]{0,63}\.md$'; python3 -c "import json;json.load(open('hooks/notify-on-stop/hook.json'));json.load(open('plugins/.claude-plugin/marketplace.json'));print('json ok')"
```
Expected: `0` (no symlinks), `0` (no badly named files), `json ok`.

- [ ] **Step 7: Initialise git and commit**

```bash
cd /workspace/projects/triple-c-marketplace && git init -q -b main && git add -A && git commit -qm "Starter marketplace: one example agent, skill, command, hook and plugin

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>" && git log --oneline -1
```
Expected: one commit hash printed.

- [ ] **Step 8: Publish to GitHub (public)**

```bash
cd /workspace/projects/triple-c-marketplace && gh repo create shadowdao/triple-c-marketplace --public --source . --push --description "Agents, skills, commands, hooks and plugins for Triple-C"
```
Expected: `✓ Created repository shadowdao/triple-c-marketplace on GitHub` and the push succeeds. Verify without printing credentials:
```bash
gh repo view shadowdao/triple-c-marketplace --json visibility,defaultBranchRef -q '.visibility + " " + .defaultBranchRef.name'
```
Expected: `PUBLIC main`.

---

---

### Task 2: Marketplace data model

**Files:**
- Create: `app/src-tauri/src/models/marketplace.rs`
- Modify: `app/src-tauri/src/models/mod.rs` (add module + re-export)
- Modify: `app/src-tauri/src/models/app_settings.rs` (three fields + `Default` impl)
- Modify: `app/src-tauri/src/models/project.rs` (two fields + `Project::new`)
- Modify: `app/src/lib/types.ts` (TS mirror + `AppSettings`/`Project` fields)
- Modify (fixtures that build a full `Project` literal): `app/src/components/projects/home/BrowserTab.test.tsx`, `app/src/components/projects/home/config/RuntimeSection.test.tsx`, `app/src/components/settings/SharedAuthSettings.test.tsx`, `app/src/components/projects/home/TaskEditorModal.test.tsx`, `app/src/components/projects/ProjectRow.test.tsx`, `app/src/components/projects/PermissionModeControl.test.tsx`, `app/src/components/projects/home/config/ModelSection.test.tsx`, `app/src/components/projects/home/config/WorkspaceSection.test.tsx`
- Test: `app/src-tauri/src/models/marketplace.rs` (`#[cfg(test)] mod tests`)

**Interfaces:**
- Consumes: nothing new.
- Produces: every type and function in the Interface Contract section `src/models/marketplace.rs` (exact names), plus `ItemKind::as_str(&self) -> &'static str`. New fields `AppSettings.marketplace_accounts: Vec<MarketplaceAccount>`, `AppSettings.marketplaces: Vec<Marketplace>`, `AppSettings.global_marketplace_installs: Vec<MarketplaceInstall>`, `Project.marketplace_installs: Vec<MarketplaceInstall>`, `Project.marketplace_disabled: Vec<MarketplaceItemRef>`. Reachable as `crate::models::X` (glob re-export). TS: all types in the contract's TypeScript mirror.

- [ ] **Step 1: Write the failing tests**

Create `app/src-tauri/src/models/marketplace.rs` containing only this tests module for now:

```rust
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
            "", ".hidden", "-flag", "_x", "a/b", "a b", "a;rm", "$(x)", "ä", "..", &"x".repeat(65),
        ] {
            assert!(!is_valid_item_key(bad), "{bad:?} should be invalid");
        }
    }

    #[test]
    fn slug_is_sanitised_and_suffixed_with_the_id() {
        assert_eq!(
            marketplace_slug("Triple-C  Marketplace!", "1A2B3C4D-ffff"),
            "triple-c-marketplace-1a2b3c4d"
        );
        assert_eq!(marketplace_slug("***", "abcdef0123"), "marketplace-abcdef01");
        let long = marketplace_slug(&"x".repeat(80), "12345678");
        assert_eq!(long, format!("{}-12345678", "x".repeat(32)));
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
            serde_json::to_value(InstallScope::Project { project_id: "p".into() }).unwrap(),
            serde_json::json!({"type": "project", "project_id": "p"})
        );
    }

    #[test]
    fn kinds_serialise_snake_case() {
        assert_eq!(serde_json::to_value(ItemKind::Plugin).unwrap(), "plugin");
        assert_eq!(serde_json::to_value(AccountMethod::GhHost).unwrap(), "gh_host");
    }

    #[test]
    fn settings_and_projects_saved_before_the_marketplace_still_load() {
        let mut settings = serde_json::to_value(crate::models::AppSettings::default()).unwrap();
        for key in ["marketplace_accounts", "marketplaces", "global_marketplace_installs"] {
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
```

Register the module in `app/src-tauri/src/models/mod.rs` — add the line `pub mod marketplace;` after `pub mod gateway_settings;`, and `pub use marketplace::*;` after `pub use gateway_settings::*;`.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cd /workspace/triple-c/app/src-tauri && cargo test --lib models::marketplace 2>&1 | tail -20`
Expected: compile errors such as "cannot find function effective_installs in this scope" and "cannot find type MarketplaceInstall".

- [ ] **Step 3: Write the implementation**

Prepend this to `app/src-tauri/src/models/marketplace.rs`, above the tests module:

```rust
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

/// A container-safe, collision-free name for a marketplace: its name
/// lowercased to `[a-z0-9-]`, dashes collapsed, at most 32 characters, then
/// `-` and the first 8 characters of its id. An empty sanitised name becomes
/// `marketplace`.
pub fn marketplace_slug(name: &str, id: &str) -> String {
    let mut base = String::new();
    for c in name.chars() {
        let c = c.to_ascii_lowercase();
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            base.push(c);
        } else if !base.ends_with('-') && !base.is_empty() {
            base.push('-');
        }
    }
    let mut base: String = base.trim_matches('-').chars().take(32).collect();
    while base.ends_with('-') {
        base.pop();
    }
    if base.is_empty() {
        base.push_str("marketplace");
    }
    let id_part: String = id
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_lowercase())
        .take(8)
        .collect();
    format!("{}-{}", base, id_part)
}

/// A full, lowercase, 40-character hex object id.
pub fn is_valid_commit(commit: &str) -> bool {
    commit.len() == 40 && commit.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
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
```

Add the fields. In `app/src-tauri/src/models/app_settings.rs`:

1. Below `use super::gateway_settings::GatewaySettings;` add:

```rust
use super::marketplace::{Marketplace, MarketplaceAccount, MarketplaceInstall};
```

2. In `pub struct AppSettings`, directly after the `global_claude_code_settings` field (`pub global_claude_code_settings: Option<ClaudeCodeSettings>,`) add:

```rust
    /// Sign-in accounts for private marketplace repos. Secrets live in the
    /// OS keychain (`storage::secure::*_marketplace_token`), never here.
    #[serde(default)]
    pub marketplace_accounts: Vec<MarketplaceAccount>,
    /// Marketplace git repos the user added.
    #[serde(default)]
    pub marketplaces: Vec<Marketplace>,
    /// Items installed for every project (projects may opt out per item).
    #[serde(default)]
    pub global_marketplace_installs: Vec<MarketplaceInstall>,
```

3. In `impl Default for AppSettings`, after `global_claude_code_settings: None,` add:

```rust
            marketplace_accounts: Vec::new(),
            marketplaces: Vec::new(),
            global_marketplace_installs: Vec::new(),
```

In `app/src-tauri/src/models/project.rs`:

1. In `pub struct Project`, directly after `pub renamed_session_names: HashMap<String, String>,` add:

```rust
    /// Marketplace items installed for this project only (spec §2).
    #[serde(default)]
    pub marketplace_installs: Vec<super::marketplace::MarketplaceInstall>,
    /// Global marketplace installs this project opts out of.
    #[serde(default)]
    pub marketplace_disabled: Vec<super::marketplace::MarketplaceItemRef>,
```

2. In `Project::new`, after `renamed_session_names: HashMap::new(),` add:

```rust
            marketplace_installs: Vec::new(),
            marketplace_disabled: Vec::new(),
```

(`Project::new` and `AppSettings::default()` are the only places in the Rust crate that build these structs field-by-field; every other constructor goes through them, `..AppSettings::default()` or `serde_json`, so nothing else needs the fields.)

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cd /workspace/triple-c/app/src-tauri && cargo test --lib models:: 2>&1 | grep -E "^test result|FAILED|panicked"`
Expected: `test result: ok.` with the 9 `models::marketplace::tests` passing and no failures elsewhere in `models::`.

- [ ] **Step 5: Mirror the types in TypeScript**

Append to `app/src/lib/types.ts` (after the closing `}` of `export interface AppSettings`, i.e. just before the doc comment that starts "What preview_settings_import returns"):

```ts

// ── Marketplace (mirrors src-tauri/src/models/marketplace.rs) ───────────────

export type ItemKind = "agent" | "skill" | "command" | "hook" | "plugin";
export type AccountMethod = "gh_host" | "gh_container" | "token";
export interface MarketplaceAccount { id: string; label: string; host: string; method: AccountMethod; username: string | null; }
export interface Marketplace { id: string; name: string; url: string; branch: string | null; account_id: string | null; }
export interface MarketplaceItemRef { marketplace_id: string; kind: ItemKind; key: string; }
export interface MarketplaceInstall extends MarketplaceItemRef { commit: string; }
export interface CatalogItem { kind: ItemKind; key: string; name: string; description: string; path: string; invalid: string | null; hook_commands: string[]; preview: string; }
export interface MarketplaceSnapshot { marketplace_id: string; head_commit: string | null; fetched_at: string | null; fetch_error: string | null; items: CatalogItem[]; }
export interface ItemUpdate { item: MarketplaceItemRef; pinned: string; head: string; }
export type FileChange = "added" | "removed" | "modified";
export interface FileDiff { path: string; change: FileChange; unified: string | null; }
export interface SkippedItem { item: string; reason: string; }
export interface SyncReport { installed: string[]; updated: string[]; removed: string[]; skipped: SkippedItem[]; errors: string[]; finished_at: string; }
export type InstallScope = { type: "global" } | { type: "project"; project_id: string };
export interface ProjectSyncResult { project_id: string; report: SyncReport; }
```

In `export interface AppSettings`, after `terminal_gpu_rendering: boolean | null;` add:

```ts
  marketplace_accounts: MarketplaceAccount[];
  marketplaces: Marketplace[];
  global_marketplace_installs: MarketplaceInstall[];
```

In `export interface Project`, after `renamed_session_names: Record<string, string>;` add:

```ts
  marketplace_installs: MarketplaceInstall[];
  marketplace_disabled: MarketplaceItemRef[];
```

Update the test fixtures that spell out a whole `Project` (each has exactly one `renamed_session_names: {},` line inside its fixture):

```bash
cd /workspace/triple-c/app/src
for f in components/projects/home/BrowserTab.test.tsx components/projects/home/config/RuntimeSection.test.tsx components/settings/SharedAuthSettings.test.tsx components/projects/home/TaskEditorModal.test.tsx components/projects/ProjectRow.test.tsx components/projects/PermissionModeControl.test.tsx components/projects/home/config/ModelSection.test.tsx components/projects/home/config/WorkspaceSection.test.tsx; do
  grep -c "renamed_session_names: {}," "$f"   # expect 1
  sed -i 's/^\(\s*\)renamed_session_names: {},$/\1renamed_session_names: {},\n\1marketplace_installs: [],\n\1marketplace_disabled: [],/' "$f"
done
git diff --stat -- .
```

Expected: each `grep -c` prints `1`; the diff touches those 8 files with 2 insertions each.

- [ ] **Step 6: Verify the frontend still type-checks and its tests pass**

Run: `cd /workspace/triple-c/app && npx tsc --noEmit -p . && npx vitest run 2>&1 | grep -E "Test Files|Tests "`
Expected: `tsc` prints nothing; Vitest reports all test files and tests passed.

- [ ] **Step 7: Commit**

```bash
cd /workspace/triple-c
git add app/src-tauri/src/models/marketplace.rs app/src-tauri/src/models/mod.rs app/src-tauri/src/models/app_settings.rs app/src-tauri/src/models/project.rs app/src/lib/types.ts app/src/components/projects/home/BrowserTab.test.tsx app/src/components/projects/home/config/RuntimeSection.test.tsx app/src/components/settings/SharedAuthSettings.test.tsx app/src/components/projects/home/TaskEditorModal.test.tsx app/src/components/projects/ProjectRow.test.tsx app/src/components/projects/PermissionModeControl.test.tsx app/src/components/projects/home/config/ModelSection.test.tsx app/src/components/projects/home/config/WorkspaceSection.test.tsx
git commit -m "Marketplace: data model, settings and project fields

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

### Task 3: Tree view and catalog parsing

**Files:**
- Create: `app/src-tauri/src/marketplace/mod.rs`
- Create: `app/src-tauri/src/marketplace/tree.rs` (`TreeView`, `DirEntry`, `EntryKind`, `MemTree`; `GitTree` is added in Task 4)
- Create: `app/src-tauri/src/marketplace/catalog.rs`
- Modify: `app/src-tauri/src/lib.rs` (declare the module)
- Test: unit tests inside `tree.rs` and `catalog.rs`

**Interfaces:**
- Consumes: `crate::models::marketplace::{is_valid_item_key, CatalogItem, ItemKind}` (Task 2).
- Produces: `marketplace::tree::{TreeView, DirEntry, EntryKind, MemTree}` and `marketplace::catalog::{MAX_ITEM_BYTES, MAX_ITEM_FILES, ItemFile, parse_catalog, item_files, item_fingerprint, plugin_catalog_entry, rendered_hook_settings, hook_dir}` with the contract's signatures. `ItemFile.rel_path` is relative to the item root; for agents/commands it is `"<key>.md"`. `parse_catalog` order: agents, skills, commands, hooks, plugins. A broken `plugins/.claude-plugin/marketplace.json` yields one invalid `Plugin` item with key `"catalog"`. Recognised hook events: `PreToolUse, PostToolUse, PostToolUseFailure, PermissionRequest, Notification, UserPromptSubmit, SessionStart, SessionEnd, Stop, SubagentStart, SubagentStop, PreCompact`.

- [ ] **Step 1: Create the module skeleton and the tree tests**

`app/src-tauri/src/marketplace/mod.rs`:

```rust
//! Marketplace support — see `docs/superpowers/specs/2026-09-27-marketplace-design.md`.

pub mod catalog;
pub mod tree;
```

In `app/src-tauri/src/lib.rs` add `mod marketplace;` on its own line between `mod logging;` and `mod models;`.

Create `app/src-tauri/src/marketplace/tree.rs` with only its tests for now:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mem_tree_lists_files_dirs_and_symlinks() {
        let t = MemTree::new()
            .file("agents/a.md", "x")
            .exec_file("hooks/h/run.sh", "#!/bin/sh")
            .symlink("agents/link.md", "a.md");
        let root = t.list_dir("").unwrap().unwrap();
        assert_eq!(
            root.iter().map(|e| (e.name.as_str(), e.kind)).collect::<Vec<_>>(),
            vec![("agents", EntryKind::Dir), ("hooks", EntryKind::Dir)]
        );
        let agents = t.list_dir("agents").unwrap().unwrap();
        assert_eq!(agents[1].kind, EntryKind::Symlink);
        let hook = t.list_dir("hooks/h").unwrap().unwrap();
        assert!(hook[0].executable);
        assert_eq!(t.list_dir("agents/a.md").unwrap(), None);
        assert_eq!(t.list_dir("missing").unwrap(), None);
        assert_eq!(t.read_file("agents/a.md").unwrap().unwrap(), b"x");
        assert_eq!(t.read_file("agents").unwrap(), None);
    }

    #[test]
    fn mem_tree_entry_id_changes_only_with_content() {
        let a = MemTree::new().file("skills/s/SKILL.md", "one").file("agents/x.md", "x");
        let b = MemTree::new().file("skills/s/SKILL.md", "one").file("agents/x.md", "changed");
        let c = MemTree::new().file("skills/s/SKILL.md", "two").file("agents/x.md", "x");
        assert_eq!(a.entry_id("skills/s").unwrap(), b.entry_id("skills/s").unwrap());
        assert_ne!(a.entry_id("skills/s").unwrap(), c.entry_id("skills/s").unwrap());
        assert_eq!(a.entry_id("nope").unwrap(), None);
    }
}
```

Create `app/src-tauri/src/marketplace/catalog.rs` with only its tests for now:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::marketplace::tree::MemTree;

    const HOOK_JSON: &str = r#"{
        "name": "notify-on-stop",
        "description": "Ping when Claude stops",
        "hooks": { "Stop": [ { "hooks": [ { "type": "command", "command": "${HOOK_DIR}/notify.sh" } ] } ] }
    }"#;

    const PLUGIN_CATALOG: &str = r#"{
        "name": "example",
        "owner": { "name": "t" },
        "plugins": [
            { "name": "example-plugin", "source": "./example-plugin", "description": "Adds a skill" }
        ]
    }"#;

    fn full_repo() -> MemTree {
        MemTree::new()
            .file("README.md", "# repo")
            .file("agents/code-reviewer.md", "---\nname: code-reviewer\ndescription: Reviews diffs\n---\nYou review code.\n")
            .file("skills/example-skill/SKILL.md", "---\nname: example-skill\ndescription: \"Says hi\"\n---\nSay hi.\n")
            .file("skills/example-skill/ref/notes.md", "notes")
            .file("commands/example-command.md", "# Summarise the branch\n\nDo it.\n")
            .file("hooks/notify-on-stop/hook.json", HOOK_JSON)
            .exec_file("hooks/notify-on-stop/notify.sh", "#!/bin/sh\necho done\n")
            .file("plugins/.claude-plugin/marketplace.json", PLUGIN_CATALOG)
            .file("plugins/example-plugin/.claude-plugin/plugin.json", r#"{"name":"example-plugin"}"#)
            .file("plugins/example-plugin/skills/hello/SKILL.md", "---\nname: hello\n---\nhi")
    }

    #[test]
    fn parses_every_kind() {
        let items = parse_catalog(&full_repo());
        let summary: Vec<_> = items
            .iter()
            .map(|i| (i.kind, i.key.as_str(), i.invalid.as_deref()))
            .collect();
        assert_eq!(
            summary,
            vec![
                (ItemKind::Agent, "code-reviewer", None),
                (ItemKind::Skill, "example-skill", None),
                (ItemKind::Command, "example-command", None),
                (ItemKind::Hook, "notify-on-stop", None),
                (ItemKind::Plugin, "example-plugin", None),
            ]
        );
        assert_eq!(items[0].description, "Reviews diffs");
        assert_eq!(items[0].preview, "You review code.\n");
        assert_eq!(items[1].description, "Says hi");
        assert_eq!(items[2].description, "Summarise the branch");
        assert_eq!(items[3].name, "notify-on-stop");
        assert_eq!(
            items[3].hook_commands,
            vec!["/home/claude/.claude/triple-c/hooks/notify-on-stop/notify.sh".to_string()]
        );
        assert_eq!(items[4].path, "plugins/example-plugin");
        assert_eq!(items[4].preview, ".claude-plugin/\nskills/");
    }

    #[test]
    fn an_empty_repo_has_no_items() {
        assert!(parse_catalog(&MemTree::new().file("README.md", "x")).is_empty());
    }

    #[test]
    fn name_falls_back_to_the_file_stem() {
        let t = MemTree::new().file("agents/plain.md", "no front matter here");
        let items = parse_catalog(&t);
        assert_eq!(items[0].name, "plain");
        assert_eq!(items[0].description, "");
        assert!(items[0].invalid.is_none());
    }

    #[test]
    fn rejects_symlink_items() {
        let t = MemTree::new()
            .symlink("agents/evil.md", "/etc/passwd")
            .file("skills/s/SKILL.md", "x")
            .symlink("skills/s/link", "../../..")
            .symlink("hooks/h", "../skills/s");
        let items = parse_catalog(&t);
        assert_eq!(items.len(), 3);
        for it in &items {
            let reason = it.invalid.as_deref().unwrap_or_else(|| panic!("{} should be invalid", it.key));
            assert!(reason.contains("symlink"), "{}: {}", it.key, reason);
        }
        assert!(item_files(&t, ItemKind::Skill, "s").is_err());
    }

    #[test]
    fn rejects_escaping_plugin_source() {
        for source in [
            r#""../outside""#,
            r#""./a/../../b""#,
            r#""/abs""#,
            r#""https://evil.example/x.git""#,
            r#"{"source":"github","repo":"x/y"}"#,
            r#""""#,
        ] {
            let catalog = format!(r#"{{"plugins":[{{"name":"p","source":{}}}]}}"#, source);
            let t = MemTree::new()
                .file("plugins/.claude-plugin/marketplace.json", &catalog)
                .file("plugins/p/x.md", "x")
                .file("outside/x.md", "x");
            let items = parse_catalog(&t);
            assert!(items[0].invalid.is_some(), "source {} should be refused", source);
            assert!(item_files(&t, ItemKind::Plugin, "p").is_err());
        }
    }

    #[test]
    fn rejects_bad_keys() {
        let t = MemTree::new()
            .file("agents/-rf.md", "x")
            .file("agents/a b.md", "x")
            .file("skills/$(id)/SKILL.md", "x")
            .file("plugins/.claude-plugin/marketplace.json", r#"{"plugins":[{"name":"bad;name","source":"./p"}]}"#)
            .file("plugins/p/x", "x");
        let items = parse_catalog(&t);
        assert_eq!(items.len(), 4);
        assert!(items.iter().all(|i| i.invalid.is_some()), "{:?}", items);
        assert!(item_files(&t, ItemKind::Agent, "-rf").is_err());
        assert!(item_files(&t, ItemKind::Skill, "$(id)").is_err());
        assert!(item_files(&t, ItemKind::Agent, "../x").is_err());
    }

    #[test]
    fn enforces_item_limits() {
        let mut many = MemTree::new().file("skills/big/SKILL.md", "x");
        for i in 0..MAX_ITEM_FILES {
            many = many.file(&format!("skills/big/f{}.txt", i), "x");
        }
        let err = item_files(&many, ItemKind::Skill, "big").unwrap_err();
        assert!(err.contains("more than 200 files"), "{}", err);

        let huge = "x".repeat(MAX_ITEM_BYTES as usize + 1);
        let t = MemTree::new().file("agents/huge.md", &huge);
        assert!(item_files(&t, ItemKind::Agent, "huge").unwrap_err().contains("larger than 2 MiB"));
        assert!(parse_catalog(&t)[0].invalid.is_some());
    }

    #[test]
    fn hooks_must_name_known_events_and_commands() {
        let t = MemTree::new()
            .file("hooks/a/hook.json", r#"{"hooks":{"NotAnEvent":[{"hooks":[{"type":"command","command":"x"}]}]}}"#)
            .file("hooks/b/hook.json", r#"{"hooks":{"Stop":[{"hooks":[{"type":"command"}]}]}}"#)
            .file("hooks/c/hook.json", "not json")
            .file("hooks/d/other.txt", "no hook.json");
        let items = parse_catalog(&t);
        assert_eq!(items.len(), 4);
        assert!(items[0].invalid.as_deref().unwrap().contains("unknown hook event"));
        assert!(items[1].invalid.as_deref().unwrap().contains("no \"command\""));
        assert!(items[2].invalid.as_deref().unwrap().contains("not valid JSON"));
        assert!(items[3].invalid.as_deref().unwrap().contains("missing"));
    }

    #[test]
    fn broken_plugin_catalog_is_one_invalid_entry() {
        let t = MemTree::new()
            .file("agents/ok.md", "x")
            .file("plugins/.claude-plugin/marketplace.json", "{");
        let items = parse_catalog(&t);
        assert_eq!(items.len(), 2);
        assert!(items[0].invalid.is_none());
        assert!(items[1].invalid.as_deref().unwrap().contains("not valid JSON"));
    }

    #[test]
    fn item_files_are_relative_to_the_item_and_keep_exec_bits() {
        let t = full_repo();
        let agent = item_files(&t, ItemKind::Agent, "code-reviewer").unwrap();
        assert_eq!(agent.len(), 1);
        assert_eq!(agent[0].rel_path, "code-reviewer.md");

        let hook = item_files(&t, ItemKind::Hook, "notify-on-stop").unwrap();
        let names: Vec<_> = hook.iter().map(|f| (f.rel_path.as_str(), f.executable)).collect();
        assert_eq!(names, vec![("hook.json", false), ("notify.sh", true)]);

        let skill = item_files(&t, ItemKind::Skill, "example-skill").unwrap();
        assert!(skill.iter().any(|f| f.rel_path == "ref/notes.md"));

        let plugin = item_files(&t, ItemKind::Plugin, "example-plugin").unwrap();
        assert!(plugin.iter().any(|f| f.rel_path == "skills/hello/SKILL.md"));
    }

    #[test]
    fn fingerprint_tracks_the_item_only() {
        let a = full_repo();
        let b = full_repo().file("agents/code-reviewer.md", "changed");
        for (kind, key) in [
            (ItemKind::Skill, "example-skill"),
            (ItemKind::Hook, "notify-on-stop"),
            (ItemKind::Plugin, "example-plugin"),
        ] {
            assert_eq!(item_fingerprint(&a, kind, key).unwrap(), item_fingerprint(&b, kind, key).unwrap());
        }
        assert_ne!(
            item_fingerprint(&a, ItemKind::Agent, "code-reviewer").unwrap(),
            item_fingerprint(&b, ItemKind::Agent, "code-reviewer").unwrap()
        );
        assert_eq!(item_fingerprint(&a, ItemKind::Agent, "absent").unwrap(), None);
    }

    #[test]
    fn plugin_fingerprint_changes_with_its_catalog_entry() {
        let a = full_repo();
        let b = full_repo().file(
            "plugins/.claude-plugin/marketplace.json",
            &PLUGIN_CATALOG.replace("Adds a skill", "Adds two skills"),
        );
        assert_ne!(
            item_fingerprint(&a, ItemKind::Plugin, "example-plugin").unwrap(),
            item_fingerprint(&b, ItemKind::Plugin, "example-plugin").unwrap()
        );
    }

    #[test]
    fn hook_settings_are_rendered_with_the_install_dir() {
        let hooks = rendered_hook_settings(&full_repo(), "notify-on-stop").unwrap();
        assert_eq!(
            hooks["Stop"][0]["hooks"][0]["command"],
            "/home/claude/.claude/triple-c/hooks/notify-on-stop/notify.sh"
        );
        assert_eq!(hook_dir("x"), "/home/claude/.claude/triple-c/hooks/x");
    }

    #[test]
    fn plugin_catalog_entry_is_returned_verbatim() {
        let entry = plugin_catalog_entry(&full_repo(), "example-plugin").unwrap();
        assert_eq!(entry["description"], "Adds a skill");
        assert!(plugin_catalog_entry(&full_repo(), "nope").is_err());
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cd /workspace/triple-c/app/src-tauri && cargo test --lib marketplace:: 2>&1 | tail -20`
Expected: compile errors, e.g. "cannot find type MemTree in this scope", "cannot find function parse_catalog".

- [ ] **Step 3: Implement `tree.rs`**

Prepend to `app/src-tauri/src/marketplace/tree.rs`, above its tests module:

```rust
//! A read-only view of a repository tree at one commit.
//!
//! The catalog parser only ever talks to [`TreeView`], so it is tested
//! against [`MemTree`] with no git involved, and runs in production against
//! [`GitTree`], which reads git objects straight out of the bare cache.

use std::collections::BTreeMap;

use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    File,
    Dir,
    Symlink,
    /// Anything else git can hold (submodule commits). Never installable.
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirEntry {
    pub name: String,
    pub kind: EntryKind,
    pub executable: bool,
}

pub trait TreeView {
    /// Entries of the directory at `path` (`""` = root). `Ok(None)` if absent or not a dir.
    fn list_dir(&self, path: &str) -> Result<Option<Vec<DirEntry>>, String>;
    /// Contents of the regular file at `path`. `Ok(None)` if absent or not a file.
    fn read_file(&self, path: &str) -> Result<Option<Vec<u8>>, String>;
    /// Stable content id of the entry at `path`; `None` if absent.
    fn entry_id(&self, path: &str) -> Result<Option<String>, String>;
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{:02x}", b)).collect()
}

#[derive(Debug, Clone)]
enum MemNode {
    File { data: Vec<u8>, executable: bool },
    Symlink { target: String },
}

/// In-memory tree for tests: path → node. Directories are implied by paths.
#[derive(Debug, Clone, Default)]
pub struct MemTree {
    nodes: BTreeMap<String, MemNode>,
}

impl MemTree {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn file(mut self, path: &str, contents: &str) -> Self {
        self.nodes.insert(
            path.to_string(),
            MemNode::File { data: contents.as_bytes().to_vec(), executable: false },
        );
        self
    }

    pub fn exec_file(mut self, path: &str, contents: &str) -> Self {
        self.nodes.insert(
            path.to_string(),
            MemNode::File { data: contents.as_bytes().to_vec(), executable: true },
        );
        self
    }

    pub fn symlink(mut self, path: &str, target: &str) -> Self {
        self.nodes
            .insert(path.to_string(), MemNode::Symlink { target: target.to_string() });
        self
    }

    fn is_dir(&self, path: &str) -> bool {
        if path.is_empty() {
            return true;
        }
        let prefix = format!("{}/", path);
        self.nodes.keys().any(|k| k.starts_with(&prefix))
    }
}

impl TreeView for MemTree {
    fn list_dir(&self, path: &str) -> Result<Option<Vec<DirEntry>>, String> {
        if self.nodes.contains_key(path) || !self.is_dir(path) {
            return Ok(None);
        }
        let prefix = if path.is_empty() { String::new() } else { format!("{}/", path) };
        let mut out: BTreeMap<String, DirEntry> = BTreeMap::new();
        for (key, node) in &self.nodes {
            let Some(rest) = key.strip_prefix(&prefix) else { continue };
            match rest.split_once('/') {
                Some((dir, _)) => {
                    out.entry(dir.to_string()).or_insert(DirEntry {
                        name: dir.to_string(),
                        kind: EntryKind::Dir,
                        executable: false,
                    });
                }
                None => {
                    let (kind, executable) = match node {
                        MemNode::File { executable, .. } => (EntryKind::File, *executable),
                        MemNode::Symlink { .. } => (EntryKind::Symlink, false),
                    };
                    out.insert(rest.to_string(), DirEntry { name: rest.to_string(), kind, executable });
                }
            }
        }
        Ok(Some(out.into_values().collect()))
    }

    fn read_file(&self, path: &str) -> Result<Option<Vec<u8>>, String> {
        match self.nodes.get(path) {
            Some(MemNode::File { data, .. }) => Ok(Some(data.clone())),
            _ => Ok(None),
        }
    }

    fn entry_id(&self, path: &str) -> Result<Option<String>, String> {
        let mut hasher = Sha256::new();
        let mut found = false;
        let prefix = format!("{}/", path);
        for (key, node) in &self.nodes {
            if key != path && !key.starts_with(&prefix) {
                continue;
            }
            found = true;
            hasher.update(key.as_bytes());
            hasher.update([0]);
            match node {
                MemNode::File { data, executable } => {
                    hasher.update([if *executable { b'x' } else { b'f' }]);
                    hasher.update(data);
                }
                MemNode::Symlink { target } => {
                    hasher.update(b"l");
                    hasher.update(target.as_bytes());
                }
            }
            hasher.update([0]);
        }
        Ok(found.then(|| hex(&hasher.finalize())))
    }
}
```

- [ ] **Step 4: Implement `catalog.rs`**

Prepend to `app/src-tauri/src/marketplace/catalog.rs`, above its tests module:

```rust
//! Reading a marketplace repo: which items it offers, and the files of one item.
//!
//! Layout (spec §1): `agents/<key>.md`, `skills/<key>/SKILL.md`,
//! `commands/<key>.md`, `hooks/<key>/hook.json`, and `plugins/` as a standard
//! Claude Code marketplace. Every item is validated here — key pattern,
//! symlinks, size and file-count limits, plugin sources that stay inside
//! `plugins/` — so nothing downstream ever sees a name or a file it would
//! have to distrust. A broken item is listed with its reason; it never stops
//! the rest of the repo from loading.

use sha2::{Digest, Sha256};

use crate::marketplace::tree::{EntryKind, TreeView};
use crate::models::marketplace::{is_valid_item_key, CatalogItem, ItemKind};

pub const MAX_ITEM_BYTES: u64 = 2 * 1024 * 1024;
pub const MAX_ITEM_FILES: usize = 200;
/// Preview text is truncated to this many bytes (on a char boundary).
const MAX_PREVIEW_BYTES: usize = 64 * 1024;

const PLUGIN_CATALOG_PATH: &str = "plugins/.claude-plugin/marketplace.json";

/// Hook events Claude Code understands. A `hook.json` naming anything else is
/// invalid rather than silently ignored by Claude Code at runtime.
const HOOK_EVENTS: &[&str] = &[
    "PreToolUse",
    "PostToolUse",
    "PostToolUseFailure",
    "PermissionRequest",
    "Notification",
    "UserPromptSubmit",
    "SessionStart",
    "SessionEnd",
    "Stop",
    "SubagentStart",
    "SubagentStop",
    "PreCompact",
];

/// One file of an item, path relative to the item root (for single-file
/// items: the file name).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemFile {
    pub rel_path: String,
    pub data: Vec<u8>,
    pub executable: bool,
}

pub fn hook_dir(key: &str) -> String {
    format!("/home/claude/.claude/triple-c/hooks/{}", key)
}

// ─────────────────────────────────────────────────────────────────────────────
// Parsing helpers
// ─────────────────────────────────────────────────────────────────────────────

/// Minimal YAML front matter: `key: value` lines between leading `---`
/// fences. Returns `(fields, body)`. Quotes around values are stripped. No
/// front matter → no fields, the whole text is the body.
fn front_matter(text: &str) -> (Vec<(String, String)>, &str) {
    let rest = match text.strip_prefix("---\n").or_else(|| text.strip_prefix("---\r\n")) {
        Some(rest) => rest,
        None => return (Vec::new(), text),
    };
    let mut fields = Vec::new();
    let mut offset = 0;
    for line in rest.split_inclusive('\n') {
        offset += line.len();
        let trimmed = line.trim_end_matches(['\n', '\r']);
        if trimmed == "---" {
            return (fields, &rest[offset..]);
        }
        if let Some((k, v)) = trimmed.split_once(':') {
            let k = k.trim();
            if !k.is_empty() && !k.starts_with(' ') && !line.starts_with(' ') {
                let v = v.trim().trim_matches('"').trim_matches('\'').to_string();
                fields.push((k.to_string(), v));
            }
        }
    }
    // Unterminated front matter: treat the whole file as body.
    (Vec::new(), text)
}

fn field<'a>(fields: &'a [(String, String)], name: &str) -> Option<&'a str> {
    fields
        .iter()
        .find(|(k, _)| k == name)
        .map(|(_, v)| v.as_str())
        .filter(|v| !v.is_empty())
}

fn truncate_preview(text: &str) -> String {
    if text.len() <= MAX_PREVIEW_BYTES {
        return text.to_string();
    }
    let mut cut = MAX_PREVIEW_BYTES;
    while !text.is_char_boundary(cut) {
        cut -= 1;
    }
    format!("{}\n…(truncated)", &text[..cut])
}

fn read_utf8(tree: &dyn TreeView, path: &str) -> Result<Option<String>, String> {
    match tree.read_file(path)? {
        None => Ok(None),
        Some(bytes) => String::from_utf8(bytes)
            .map(Some)
            .map_err(|_| format!("{} is not UTF-8 text", path)),
    }
}

/// Normalise a plugin `source` into a path under `plugins/`, refusing
/// anything that is not a plain relative path staying inside `plugins/`.
fn plugin_source_path(source: &serde_json::Value) -> Result<String, String> {
    let source = source.as_str().ok_or_else(|| {
        "remote plugin sources are not supported — the plugin must live in this repo's plugins/ folder"
            .to_string()
    })?;
    if source.starts_with('/') || source.contains('\\') || source.contains(':') {
        return Err(format!("plugin source {:?} must be a relative path inside plugins/", source));
    }
    let mut parts = Vec::new();
    for part in source.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                return Err(format!("plugin source {:?} must stay inside plugins/", source));
            }
            p => parts.push(p),
        }
    }
    if parts.is_empty() {
        return Err(format!("plugin source {:?} must name a folder inside plugins/", source));
    }
    Ok(format!("plugins/{}", parts.join("/")))
}

fn read_plugin_catalog(tree: &dyn TreeView) -> Result<Option<Vec<serde_json::Value>>, String> {
    let Some(text) = read_utf8(tree, PLUGIN_CATALOG_PATH)? else { return Ok(None) };
    let json: serde_json::Value = serde_json::from_str(&text)
        .map_err(|e| format!("{} is not valid JSON: {}", PLUGIN_CATALOG_PATH, e))?;
    let plugins = json
        .get("plugins")
        .and_then(|p| p.as_array())
        .ok_or_else(|| format!("{} has no \"plugins\" array", PLUGIN_CATALOG_PATH))?;
    Ok(Some(plugins.clone()))
}

/// Plugins only: the plugin's entry from `plugins/.claude-plugin/marketplace.json`.
pub fn plugin_catalog_entry(tree: &dyn TreeView, key: &str) -> Result<serde_json::Value, String> {
    let entries = read_plugin_catalog(tree)?
        .ok_or_else(|| format!("{} is missing", PLUGIN_CATALOG_PATH))?;
    entries
        .into_iter()
        .find(|e| e.get("name").and_then(|n| n.as_str()) == Some(key))
        .ok_or_else(|| format!("plugin {} is not in {}", key, PLUGIN_CATALOG_PATH))
}

/// Repo path of an item: a file for agents/commands, a folder otherwise.
fn item_path(tree: &dyn TreeView, kind: ItemKind, key: &str) -> Result<String, String> {
    if !is_valid_item_key(key) {
        return Err(format!(
            "{:?} is not a valid name (letters, digits, '.', '_' and '-', starting with a letter or digit, at most 64)",
            key
        ));
    }
    Ok(match kind {
        ItemKind::Agent => format!("agents/{}.md", key),
        ItemKind::Command => format!("commands/{}.md", key),
        ItemKind::Skill => format!("skills/{}", key),
        ItemKind::Hook => format!("hooks/{}", key),
        ItemKind::Plugin => {
            let entry = plugin_catalog_entry(tree, key)?;
            plugin_source_path(entry.get("source").unwrap_or(&serde_json::Value::Null))?
        }
    })
}

/// Recursively collect a folder's files, enforcing the item rules.
fn collect_dir(
    tree: &dyn TreeView,
    root: &str,
    rel: &str,
    out: &mut Vec<ItemFile>,
    total: &mut u64,
) -> Result<(), String> {
    let path = if rel.is_empty() { root.to_string() } else { format!("{}/{}", root, rel) };
    let entries = tree
        .list_dir(&path)?
        .ok_or_else(|| format!("{} is not a folder", path))?;
    for entry in entries {
        let child_rel = if rel.is_empty() { entry.name.clone() } else { format!("{}/{}", rel, entry.name) };
        match entry.kind {
            EntryKind::Symlink => {
                return Err(format!("contains a symlink ({}), which is not allowed", child_rel));
            }
            EntryKind::Other => {
                return Err(format!("contains a submodule or special entry ({})", child_rel));
            }
            EntryKind::Dir => collect_dir(tree, root, &child_rel, out, total)?,
            EntryKind::File => {
                let data = tree
                    .read_file(&format!("{}/{}", root, child_rel))?
                    .ok_or_else(|| format!("{} vanished while reading", child_rel))?;
                *total += data.len() as u64;
                if out.len() + 1 > MAX_ITEM_FILES {
                    return Err(format!("has more than {} files", MAX_ITEM_FILES));
                }
                if *total > MAX_ITEM_BYTES {
                    return Err(format!("is larger than {} MiB", MAX_ITEM_BYTES / (1024 * 1024)));
                }
                out.push(ItemFile { rel_path: child_rel, data, executable: entry.executable });
            }
        }
    }
    Ok(())
}

/// Kind of the entry at `path`, looked up through its parent listing.
fn entry_kind(tree: &dyn TreeView, path: &str) -> Result<Option<(EntryKind, bool)>, String> {
    let (parent, name) = match path.rsplit_once('/') {
        Some((p, n)) => (p, n),
        None => ("", path),
    };
    Ok(tree
        .list_dir(parent)?
        .and_then(|entries| entries.into_iter().find(|e| e.name == name))
        .map(|e| (e.kind, e.executable)))
}

/// All files of one item. Err if the item is missing/invalid or breaks the limits.
pub fn item_files(tree: &dyn TreeView, kind: ItemKind, key: &str) -> Result<Vec<ItemFile>, String> {
    let path = item_path(tree, kind, key)?;
    match kind {
        ItemKind::Agent | ItemKind::Command => {
            let (entry, executable) = entry_kind(tree, &path)?
                .ok_or_else(|| format!("{} is missing", path))?;
            match entry {
                EntryKind::File => {}
                EntryKind::Symlink => return Err(format!("{} is a symlink, which is not allowed", path)),
                _ => return Err(format!("{} is not a regular file", path)),
            }
            let data = tree.read_file(&path)?.ok_or_else(|| format!("{} is missing", path))?;
            if data.len() as u64 > MAX_ITEM_BYTES {
                return Err(format!("is larger than {} MiB", MAX_ITEM_BYTES / (1024 * 1024)));
            }
            Ok(vec![ItemFile { rel_path: format!("{}.md", key), data, executable }])
        }
        ItemKind::Skill | ItemKind::Hook | ItemKind::Plugin => {
            match entry_kind(tree, &path)? {
                Some((EntryKind::Dir, _)) => {}
                Some((EntryKind::Symlink, _)) => {
                    return Err(format!("{} is a symlink, which is not allowed", path))
                }
                Some(_) => return Err(format!("{} is not a folder", path)),
                None => return Err(format!("{} is missing", path)),
            }
            let mut out = Vec::new();
            let mut total = 0u64;
            collect_dir(tree, &path, "", &mut out, &mut total)?;
            let required = match kind {
                ItemKind::Skill => Some("SKILL.md"),
                ItemKind::Hook => Some("hook.json"),
                _ => None,
            };
            if let Some(required) = required {
                if !out.iter().any(|f| f.rel_path == required) {
                    return Err(format!("{} has no {}", path, required));
                }
            }
            Ok(out)
        }
    }
}

/// Content fingerprint for update detection: changes iff the item's files or,
/// for plugins, its catalog entry change. `Ok(None)` when the item is absent.
pub fn item_fingerprint(tree: &dyn TreeView, kind: ItemKind, key: &str) -> Result<Option<String>, String> {
    if kind == ItemKind::Plugin {
        let entry = match plugin_catalog_entry(tree, key) {
            Ok(entry) => entry,
            Err(_) => return Ok(None),
        };
        let path = match plugin_source_path(entry.get("source").unwrap_or(&serde_json::Value::Null)) {
            Ok(path) => path,
            Err(_) => return Ok(None),
        };
        let Some(dir_id) = tree.entry_id(&path)? else { return Ok(None) };
        let mut hasher = Sha256::new();
        hasher.update(dir_id.as_bytes());
        hasher.update([0]);
        // serde_json's Map is ordered by key (no preserve_order), so this is canonical.
        hasher.update(entry.to_string().as_bytes());
        return Ok(Some(hasher.finalize().iter().map(|b| format!("{:02x}", b)).collect()));
    }
    if !is_valid_item_key(key) {
        return Ok(None);
    }
    let path = item_path(tree, kind, key)?;
    tree.entry_id(&path)
}

fn substitute_hook_dir(value: &mut serde_json::Value, dir: &str) {
    match value {
        serde_json::Value::String(s) => {
            if s.contains("${HOOK_DIR}") {
                *s = s.replace("${HOOK_DIR}", dir);
            }
        }
        serde_json::Value::Array(items) => items.iter_mut().for_each(|v| substitute_hook_dir(v, dir)),
        serde_json::Value::Object(map) => map.values_mut().for_each(|v| substitute_hook_dir(v, dir)),
        _ => {}
    }
}

/// Validate a `hooks` object and return the command strings it runs.
fn validate_hooks(hooks: &serde_json::Value) -> Result<Vec<String>, String> {
    let map = hooks
        .as_object()
        .ok_or_else(|| "\"hooks\" must be an object keyed by event name".to_string())?;
    if map.is_empty() {
        return Err("\"hooks\" is empty".to_string());
    }
    let mut commands = Vec::new();
    for (event, matchers) in map {
        if !HOOK_EVENTS.contains(&event.as_str()) {
            return Err(format!("unknown hook event {:?}", event));
        }
        let matchers = matchers
            .as_array()
            .ok_or_else(|| format!("\"{}\" must be an array", event))?;
        for matcher in matchers {
            let handlers = matcher
                .get("hooks")
                .and_then(|h| h.as_array())
                .ok_or_else(|| format!("each \"{}\" entry needs a \"hooks\" array", event))?;
            for handler in handlers {
                let kind = handler.get("type").and_then(|t| t.as_str()).unwrap_or("");
                if kind.is_empty() {
                    return Err(format!("a \"{}\" hook has no \"type\"", event));
                }
                if kind == "command" {
                    let command = handler
                        .get("command")
                        .and_then(|c| c.as_str())
                        .filter(|c| !c.trim().is_empty())
                        .ok_or_else(|| format!("a \"{}\" command hook has no \"command\"", event))?;
                    commands.push(command.to_string());
                }
            }
        }
    }
    Ok(commands)
}

fn read_hook_json(tree: &dyn TreeView, key: &str) -> Result<serde_json::Value, String> {
    let path = format!("hooks/{}/hook.json", key);
    let text = read_utf8(tree, &path)?.ok_or_else(|| format!("{} is missing", path))?;
    serde_json::from_str(&text).map_err(|e| format!("{} is not valid JSON: {}", path, e))
}

/// Hooks only: the parsed `hooks` object with `${HOOK_DIR}` substituted.
pub fn rendered_hook_settings(tree: &dyn TreeView, key: &str) -> Result<serde_json::Value, String> {
    if !is_valid_item_key(key) {
        return Err(format!("{:?} is not a valid hook name", key));
    }
    let json = read_hook_json(tree, key)?;
    let mut hooks = json
        .get("hooks")
        .cloned()
        .ok_or_else(|| format!("hooks/{}/hook.json has no \"hooks\" object", key))?;
    validate_hooks(&hooks)?;
    substitute_hook_dir(&mut hooks, &hook_dir(key));
    Ok(hooks)
}

// ─────────────────────────────────────────────────────────────────────────────
// Catalog
// ─────────────────────────────────────────────────────────────────────────────

fn item(kind: ItemKind, key: &str, path: String) -> CatalogItem {
    CatalogItem {
        kind,
        key: key.to_string(),
        name: key.to_string(),
        description: String::new(),
        path,
        invalid: None,
        hook_commands: Vec::new(),
        preview: String::new(),
    }
}

/// Fill name/description/preview from a markdown file with front matter.
fn describe_markdown(it: &mut CatalogItem, text: &str, first_line_fallback: bool) {
    let (fields, body) = front_matter(text);
    if let Some(name) = field(&fields, "name") {
        it.name = name.to_string();
    }
    if let Some(desc) = field(&fields, "description") {
        it.description = desc.to_string();
    } else if first_line_fallback {
        if let Some(line) = body.lines().map(str::trim).find(|l| !l.is_empty()) {
            it.description = line.trim_start_matches('#').trim().to_string();
        }
    }
    it.preview = truncate_preview(body.trim_start_matches(['\n', '\r']));
}

/// Mark `it` invalid when its files break the rules.
fn validate_files(tree: &dyn TreeView, it: &mut CatalogItem) {
    if it.invalid.is_some() {
        return;
    }
    if let Err(reason) = item_files(tree, it.kind, &it.key) {
        it.invalid = Some(reason);
    }
}

fn parse_single_files(tree: &dyn TreeView, kind: ItemKind, folder: &str, out: &mut Vec<CatalogItem>) {
    let entries = match tree.list_dir(folder) {
        Ok(Some(entries)) => entries,
        Ok(None) => return,
        Err(e) => {
            let mut it = item(kind, folder, folder.to_string());
            it.invalid = Some(e);
            out.push(it);
            return;
        }
    };
    for entry in entries {
        let Some(stem) = entry.name.strip_suffix(".md") else { continue };
        let mut it = item(kind, stem, format!("{}/{}", folder, entry.name));
        if !is_valid_item_key(stem) {
            it.invalid = Some(format!(
                "{:?} is not a valid name (letters, digits, '.', '_' and '-', starting with a letter or digit, at most 64)",
                stem
            ));
            out.push(it);
            continue;
        }
        match entry.kind {
            EntryKind::File => match read_utf8(tree, &it.path) {
                Ok(Some(text)) => describe_markdown(&mut it, &text, kind == ItemKind::Command),
                Ok(None) => it.invalid = Some(format!("{} is missing", it.path)),
                Err(e) => it.invalid = Some(e),
            },
            EntryKind::Symlink => it.invalid = Some(format!("{} is a symlink, which is not allowed", it.path)),
            _ => it.invalid = Some(format!("{} is not a regular file", it.path)),
        }
        validate_files(tree, &mut it);
        out.push(it);
    }
}

fn parse_folders(tree: &dyn TreeView, kind: ItemKind, folder: &str, out: &mut Vec<CatalogItem>) {
    let entries = match tree.list_dir(folder) {
        Ok(Some(entries)) => entries,
        Ok(None) => return,
        Err(e) => {
            let mut it = item(kind, folder, folder.to_string());
            it.invalid = Some(e);
            out.push(it);
            return;
        }
    };
    for entry in entries {
        if entry.kind == EntryKind::File {
            continue; // e.g. a README.md next to the item folders
        }
        let mut it = item(kind, &entry.name, format!("{}/{}", folder, entry.name));
        if !is_valid_item_key(&entry.name) {
            it.invalid = Some(format!(
                "{:?} is not a valid name (letters, digits, '.', '_' and '-', starting with a letter or digit, at most 64)",
                entry.name
            ));
            out.push(it);
            continue;
        }
        if entry.kind == EntryKind::Symlink {
            it.invalid = Some(format!("{} is a symlink, which is not allowed", it.path));
            out.push(it);
            continue;
        }
        match kind {
            ItemKind::Skill => match read_utf8(tree, &format!("{}/SKILL.md", it.path)) {
                Ok(Some(text)) => describe_markdown(&mut it, &text, false),
                Ok(None) => it.invalid = Some(format!("{} has no SKILL.md", it.path)),
                Err(e) => it.invalid = Some(e),
            },
            ItemKind::Hook => match read_hook_json(tree, &entry.name) {
                Ok(json) => {
                    if let Some(name) = json.get("name").and_then(|n| n.as_str()).filter(|n| !n.is_empty()) {
                        it.name = name.to_string();
                    }
                    if let Some(desc) = json.get("description").and_then(|d| d.as_str()) {
                        it.description = desc.to_string();
                    }
                    match rendered_hook_settings(tree, &entry.name) {
                        Ok(hooks) => match validate_hooks(&hooks) {
                            Ok(commands) => it.hook_commands = commands,
                            Err(e) => it.invalid = Some(e),
                        },
                        Err(e) => it.invalid = Some(e),
                    }
                }
                Err(e) => it.invalid = Some(e),
            },
            _ => {}
        }
        validate_files(tree, &mut it);
        out.push(it);
    }
}

fn parse_plugins(tree: &dyn TreeView, out: &mut Vec<CatalogItem>) {
    let entries = match read_plugin_catalog(tree) {
        Ok(Some(entries)) => entries,
        Ok(None) => return,
        Err(e) => {
            let mut it = item(ItemKind::Plugin, "catalog", PLUGIN_CATALOG_PATH.to_string());
            it.name = PLUGIN_CATALOG_PATH.to_string();
            it.invalid = Some(e);
            out.push(it);
            return;
        }
    };
    for entry in entries {
        let key = entry.get("name").and_then(|n| n.as_str()).unwrap_or("").to_string();
        let mut it = item(ItemKind::Plugin, &key, PLUGIN_CATALOG_PATH.to_string());
        if let Some(desc) = entry.get("description").and_then(|d| d.as_str()) {
            it.description = desc.to_string();
        }
        if !is_valid_item_key(&key) {
            it.invalid = Some(format!("plugin name {:?} is not a valid name", key));
            out.push(it);
            continue;
        }
        match plugin_source_path(entry.get("source").unwrap_or(&serde_json::Value::Null)) {
            Ok(path) => {
                it.path = path.clone();
                if let Ok(Some(children)) = tree.list_dir(&path) {
                    it.preview = children
                        .iter()
                        .map(|c| if c.kind == EntryKind::Dir { format!("{}/", c.name) } else { c.name.clone() })
                        .collect::<Vec<_>>()
                        .join("\n");
                }
            }
            Err(e) => it.invalid = Some(e),
        }
        validate_files(tree, &mut it);
        out.push(it);
    }
}

/// Parse every item in the repo. Never fails as a whole; broken items carry `invalid`.
/// Order: agents, skills, commands, hooks, plugins; each in listing order.
pub fn parse_catalog(tree: &dyn TreeView) -> Vec<CatalogItem> {
    let mut out = Vec::new();
    parse_single_files(tree, ItemKind::Agent, "agents", &mut out);
    parse_folders(tree, ItemKind::Skill, "skills", &mut out);
    parse_single_files(tree, ItemKind::Command, "commands", &mut out);
    parse_folders(tree, ItemKind::Hook, "hooks", &mut out);
    parse_plugins(tree, &mut out);
    out
}
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cd /workspace/triple-c/app/src-tauri && cargo test --lib marketplace:: 2>&1 | grep -E "^test |^test result"`
Expected: 16 tests pass (2 in `tree::tests`, 14 in `catalog::tests`), including `rejects_symlink_items`, `rejects_escaping_plugin_source`, `rejects_bad_keys`, `enforces_item_limits`. Dead-code warnings for the new module are expected until Task 11.

- [ ] **Step 6: Format and commit**

```bash
cd /workspace/triple-c/app/src-tauri
rustfmt --edition 2021 src/marketplace/mod.rs src/marketplace/tree.rs src/marketplace/catalog.rs
cargo test --lib marketplace:: 2>&1 | grep "^test result"
```

```bash
cd /workspace/triple-c
git add app/src-tauri/src/lib.rs app/src-tauri/src/marketplace/mod.rs app/src-tauri/src/marketplace/tree.rs app/src-tauri/src/marketplace/catalog.rs
git commit -m "Marketplace: repo tree view and catalog parsing

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

### Task 4: Git cache (gix fetch, pins, `GitTree`)

**Files:**
- Modify: `app/src-tauri/Cargo.toml` (`gix`, dev-dep `tempfile`), `app/src-tauri/Cargo.lock` (generated)
- Create: `app/src-tauri/src/marketplace/git.rs`
- Modify: `app/src-tauri/src/marketplace/tree.rs` (add `GitTree`)
- Modify: `app/src-tauri/src/marketplace/mod.rs` (`pub mod git;`)
- Test: unit tests in `git.rs` (fixture repos built with the git CLI over `file://`; each test returns early when `git` is not installed)

**Interfaces:**
- Consumes: `tree::{TreeView, DirEntry, EntryKind}` (Task 3).
- Produces: `git::{Credential, FetchError, HEAD_REF, PIN_PREFIX, classify_fetch_error, cache_path, fetch, cached_head, set_pins, has_commit}`; `tree::GitTree::open(repo_path, commit)`. `fetch` stores the tip at `refs/triple-c/head` via refspec `+HEAD:refs/triple-c/head` (default branch) or `+refs/heads/<branch>:refs/triple-c/head`; branch names outside `[A-Za-z0-9._/-]` (or containing `..`, leading `-`/`/`, trailing `/` or `.lock`) are refused with `FetchError::Other("… is not a valid branch name")`. `FetchError` mapping: `HTTP status 401` or `not accepted by the remote` → `Auth{401}`, `HTTP status 403` → `Auth{403}`, `HTTP status 404`/`repository not found` → `NotFound`, dns/connect/timeout text → `Network`, else `Other`. Test fixtures for later tasks: `git::test_support::{git_available() -> bool, git(dir, args) -> String, init_repo(dir, files: &[(&str, &str, bool)]) -> String /*commit*/, commit_files(dir, files, message) -> String, file_url(dir) -> String}`.

- [ ] **Step 1: Add the dependencies**

In `app/src-tauri/Cargo.toml` under `[dependencies]` (after `url = "2"`) add:

```toml
# Marketplace repos are fetched on the host into a bare cache (spec §3).
# Blocking client + rustls: no git binary or OpenSSL needed on the host.
gix = { version = "0.88", default-features = false, features = ["blocking-network-client", "blocking-http-transport-reqwest-rust-tls", "credentials", "sha1"] }
```

Under `[dev-dependencies]` add:

```toml
tempfile = "3"
```

Run: `cd /workspace/triple-c/app/src-tauri && cargo build 2>&1 | tail -3`
Expected: builds (first build of gix takes a few minutes).

- [ ] **Step 2: Write the failing tests**

Add `pub mod git;` to `app/src-tauri/src/marketplace/mod.rs` (keep the list alphabetical: `catalog`, `git`, `tree`).

Create `app/src-tauri/src/marketplace/git.rs` with only the test code for now:

```rust
#[cfg(test)]
pub(crate) mod test_support {
    //! Fixture repos built with the git CLI. Tests that need one call
    //! [`git_available`] first and return early without it.
    use std::path::Path;
    use std::process::Command;

    pub fn git_available() -> bool {
        Command::new("git").arg("--version").output().map(|o| o.status.success()).unwrap_or(false)
    }

    pub fn git(dir: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .args(["-c", "user.name=t", "-c", "user.email=t@example.invalid", "-c", "init.defaultBranch=main"])
            .args(args)
            .current_dir(dir)
            .output()
            .expect("git runs");
        assert!(out.status.success(), "git {:?}: {}", args, String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// Write `files` (path, contents, executable) into a new repo and commit.
    pub fn init_repo(dir: &Path, files: &[(&str, &str, bool)]) -> String {
        git(dir, &["init", "-q"]);
        commit_files(dir, files, "initial")
    }

    pub fn commit_files(dir: &Path, files: &[(&str, &str, bool)], message: &str) -> String {
        for (path, contents, exec) in files {
            let full = dir.join(path);
            std::fs::create_dir_all(full.parent().unwrap()).unwrap();
            std::fs::write(&full, contents).unwrap();
            #[cfg(unix)]
            if *exec {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&full, std::fs::Permissions::from_mode(0o755)).unwrap();
            }
            #[cfg(not(unix))]
            let _ = exec;
        }
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-q", "-m", message]);
        git(dir, &["rev-parse", "HEAD"])
    }

    pub fn file_url(dir: &Path) -> String {
        format!("file://{}", dir.display())
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::*;
    use super::*;
    use crate::marketplace::tree::{GitTree, TreeView};

    #[test]
    fn fetch_error_mapping() {
        let cases = [
            ("Credentials provided for \"https://x\" were not accepted by the remote\n└─ Received HTTP status 401", FetchError::Auth { status: 401 }),
            ("handshake\n└─ Received HTTP status 403", FetchError::Auth { status: 403 }),
            ("└─ Received HTTP status 404", FetchError::NotFound),
            ("remote: Repository not found.", FetchError::NotFound),
        ];
        for (text, want) in cases {
            assert_eq!(classify_fetch_error(text), want, "{}", text);
        }
        assert!(matches!(
            classify_fetch_error("error sending request\n└─ dns error: failed to lookup address"),
            FetchError::Network(_)
        ));
        assert!(matches!(classify_fetch_error("operation timed out"), FetchError::Network(_)));
        assert!(matches!(classify_fetch_error("something odd"), FetchError::Other(_)));
    }

    #[test]
    fn credential_debug_never_shows_the_password() {
        let c = Credential { username: "u".into(), password: "test-token-not-real".into() };
        let shown = format!("{:?}", c);
        assert!(!shown.contains("test-token-not-real"));
        assert!(shown.contains("<redacted>"));
    }

    #[test]
    fn refuses_unsafe_branch_names() {
        let dir = tempfile::tempdir().unwrap();
        for bad in ["-x", "a..b", "a b", "a:b", "x*", "a.lock", ""] {
            let err = fetch(&dir.path().join("c.git"), "file:///nowhere", Some(bad), None).unwrap_err();
            assert!(matches!(err, FetchError::Other(ref m) if m.contains("branch")), "{bad:?}: {err:?}");
        }
    }

    #[test]
    fn fetches_default_branch_then_updates() {
        if !git_available() {
            return;
        }
        let src = tempfile::tempdir().unwrap();
        let first = init_repo(src.path(), &[("agents/a.md", "one", false), ("hooks/h/run.sh", "#!/bin/sh", true)]);
        let cache = tempfile::tempdir().unwrap();
        let repo = cache_path(cache.path(), "m1");

        assert_eq!(cached_head(&repo).unwrap(), None);
        let head = fetch(&repo, &file_url(src.path()), None, None).unwrap();
        assert_eq!(head, first);
        assert_eq!(cached_head(&repo).unwrap(), Some(first.clone()));
        assert!(has_commit(&repo, &first));

        let tree = GitTree::open(&repo, &first).unwrap();
        assert_eq!(tree.read_file("agents/a.md").unwrap().unwrap(), b"one");
        let hook = tree.list_dir("hooks/h").unwrap().unwrap();
        assert!(hook[0].executable);
        assert!(tree.entry_id("agents/a.md").unwrap().is_some());
        assert_eq!(tree.list_dir("agents/a.md").unwrap(), None);

        let second = commit_files(src.path(), &[("agents/a.md", "two", false)], "second");
        assert_eq!(fetch(&repo, &file_url(src.path()), None, None).unwrap(), second);
        // The old commit is still readable after the update.
        assert_eq!(
            GitTree::open(&repo, &first).unwrap().read_file("agents/a.md").unwrap().unwrap(),
            b"one"
        );
    }

    #[test]
    fn fetches_a_named_branch() {
        if !git_available() {
            return;
        }
        let src = tempfile::tempdir().unwrap();
        init_repo(src.path(), &[("a.md", "main", false)]);
        git(src.path(), &["checkout", "-q", "-b", "next"]);
        let next = commit_files(src.path(), &[("a.md", "next", false)], "next");
        git(src.path(), &["checkout", "-q", "main"]);

        let cache = tempfile::tempdir().unwrap();
        let repo = cache_path(cache.path(), "m1");
        assert_eq!(fetch(&repo, &file_url(src.path()), Some("next"), None).unwrap(), next);
    }

    #[test]
    fn missing_repo_is_an_error_not_a_panic() {
        let cache = tempfile::tempdir().unwrap();
        let err = fetch(&cache_path(cache.path(), "m"), "file:///definitely/not/here", None, None).unwrap_err();
        assert!(!matches!(err, FetchError::Auth { .. }), "{err:?}");
    }

    #[test]
    fn pins_are_exactly_the_requested_set() {
        if !git_available() {
            return;
        }
        let src = tempfile::tempdir().unwrap();
        let a = init_repo(src.path(), &[("x", "1", false)]);
        let b = commit_files(src.path(), &[("x", "2", false)], "b");
        let cache = tempfile::tempdir().unwrap();
        let repo = cache_path(cache.path(), "m");
        fetch(&repo, &file_url(src.path()), None, None).unwrap();

        set_pins(&repo, &[a.clone(), b.clone(), "f".repeat(40)]).unwrap();
        let pins = |repo: &Path| -> Vec<String> {
            let r = gix::open(repo).unwrap();
            let mut names: Vec<String> = r
                .references()
                .unwrap()
                .prefixed(PIN_PREFIX)
                .unwrap()
                .map(|x| x.unwrap().name().as_bstr().to_string())
                .collect();
            names.sort();
            names
        };
        let mut want = vec![format!("{}{}", PIN_PREFIX, a), format!("{}{}", PIN_PREFIX, b)];
        want.sort();
        assert_eq!(pins(&repo), want);

        set_pins(&repo, &[b.clone()]).unwrap();
        assert_eq!(pins(&repo), vec![format!("{}{}", PIN_PREFIX, b)]);
    }
}
```

- [ ] **Step 3: Run the tests to verify they fail**

Run: `cd /workspace/triple-c/app/src-tauri && cargo test --lib marketplace::git 2>&1 | tail -20`
Expected: compile errors, e.g. "cannot find function classify_fetch_error", "cannot find type GitTree in module crate::marketplace::tree".

- [ ] **Step 4: Add `GitTree` to `tree.rs`**

In `app/src-tauri/src/marketplace/tree.rs`, insert this block immediately above `#[cfg(test)]`:

```rust
/// A tree at one commit of a bare gix repository.
pub struct GitTree {
    repo: gix::Repository,
    tree_id: gix::ObjectId,
}

impl GitTree {
    pub fn open(repo_path: &std::path::Path, commit: &str) -> Result<Self, String> {
        let repo = gix::open(repo_path)
            .map_err(|e| format!("Could not open the marketplace cache: {}", e))?;
        let oid = gix::ObjectId::from_hex(commit.as_bytes())
            .map_err(|e| format!("Invalid commit id {}: {}", commit, e))?;
        let tree_id = repo
            .find_commit(oid)
            .map_err(|e| format!("Commit {} is not in the marketplace cache: {}", commit, e))?
            .tree_id()
            .map_err(|e| format!("Commit {} has no tree: {}", commit, e))?
            .detach();
        Ok(Self { repo, tree_id })
    }

    fn root(&self) -> Result<gix::Tree<'_>, String> {
        self.repo
            .find_tree(self.tree_id)
            .map_err(|e| format!("Could not read tree {}: {}", self.tree_id, e))
    }

    /// `(object id, mode)` of the entry at `path`, or `None`.
    fn lookup(&self, path: &str) -> Result<Option<(gix::ObjectId, gix::object::tree::EntryMode)>, String> {
        if path.is_empty() {
            return Ok(Some((self.tree_id, gix::object::tree::EntryKind::Tree.into())));
        }
        let root = self.root()?;
        let entry = root
            .lookup_entry_by_path(path)
            .map_err(|e| format!("Could not look up {}: {}", path, e))?;
        Ok(entry.map(|e| (e.object_id(), e.mode())))
    }
}

impl TreeView for GitTree {
    fn list_dir(&self, path: &str) -> Result<Option<Vec<DirEntry>>, String> {
        let Some((id, mode)) = self.lookup(path)? else { return Ok(None) };
        if !mode.is_tree() {
            return Ok(None);
        }
        let tree = self
            .repo
            .find_tree(id)
            .map_err(|e| format!("Could not read {}: {}", path, e))?;
        let mut out = Vec::new();
        for entry in tree.iter() {
            let entry = entry.map_err(|e| format!("Could not read {}: {:?}", path, e))?;
            let mode = entry.mode();
            let kind = if mode.is_tree() {
                EntryKind::Dir
            } else if mode.is_link() {
                EntryKind::Symlink
            } else if mode.is_blob() {
                EntryKind::File
            } else {
                EntryKind::Other
            };
            out.push(DirEntry {
                name: entry.filename().to_string(),
                kind,
                executable: mode.is_executable(),
            });
        }
        Ok(Some(out))
    }

    fn read_file(&self, path: &str) -> Result<Option<Vec<u8>>, String> {
        let Some((id, mode)) = self.lookup(path)? else { return Ok(None) };
        if !mode.is_blob() {
            return Ok(None);
        }
        let blob = self
            .repo
            .find_blob(id)
            .map_err(|e| format!("Could not read {}: {}", path, e))?;
        Ok(Some(blob.data.clone()))
    }

    fn entry_id(&self, path: &str) -> Result<Option<String>, String> {
        Ok(self.lookup(path)?.map(|(id, _)| id.to_string()))
    }
}
```

- [ ] **Step 5: Implement `git.rs`**

Prepend to `app/src-tauri/src/marketplace/git.rs`, above `#[cfg(test)] pub(crate) mod test_support`:

```rust
//! The marketplace cache: one bare `gix` repository per marketplace.
//!
//! Everything here is blocking — call it from `tokio::task::spawn_blocking`.
//! Credentials are handed to gix through its credential callback for the
//! duration of one fetch and are never written to disk or into the repo
//! config.

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

/// The ref the fetched branch tip is stored under.
pub const HEAD_REF: &str = "refs/triple-c/head";
/// Prefix of the refs that keep pinned commits alive.
pub const PIN_PREFIX: &str = "refs/triple-c/pins/";

#[derive(Clone)]
pub struct Credential {
    pub username: String,
    pub password: String,
}

impl std::fmt::Debug for Credential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Credential")
            .field("username", &self.username)
            .field("password", &"<redacted>")
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FetchError {
    /// 401 / 403, or gix's "credentials … were not accepted".
    Auth { status: u16 },
    /// 404 / "repository not found".
    NotFound,
    Network(String),
    Other(String),
}

impl std::fmt::Display for FetchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FetchError::Auth { status } => write!(f, "access denied (HTTP {})", status),
            FetchError::NotFound => write!(f, "repository not found"),
            FetchError::Network(m) => write!(f, "network error: {}", m),
            FetchError::Other(m) => write!(f, "{}", m),
        }
    }
}

/// Classify a gix error by its Debug-formatted chain. gix wraps transport
/// errors several layers deep and some layers are not `std::error::Error`,
/// so the text is the one stable thing to match on.
pub fn classify_fetch_error(chain: &str) -> FetchError {
    let lower = chain.to_ascii_lowercase();
    if lower.contains("http status 401") || lower.contains("not accepted by the remote") {
        return FetchError::Auth { status: 401 };
    }
    if lower.contains("http status 403") {
        return FetchError::Auth { status: 403 };
    }
    if lower.contains("http status 404") || lower.contains("repository not found") {
        return FetchError::NotFound;
    }
    const NETWORK: &[&str] = &[
        "dns error",
        "failed to lookup address",
        "connection refused",
        "connection reset",
        "timed out",
        "timeout",
        "network is unreachable",
        "no route to host",
        "error sending request",
        "tcp connect error",
    ];
    if NETWORK.iter().any(|needle| lower.contains(needle)) {
        return FetchError::Network(first_line(chain));
    }
    FetchError::Other(first_line(chain))
}

fn first_line(chain: &str) -> String {
    chain.lines().next().unwrap_or("").trim().chars().take(300).collect()
}

fn classify<E: std::fmt::Debug>(e: E) -> FetchError {
    classify_fetch_error(&format!("{:?}", e))
}

pub fn cache_path(data_root: &Path, marketplace_id: &str) -> PathBuf {
    data_root.join("marketplaces").join(format!("{}.git", marketplace_id))
}

/// Branch names that are safe inside a refspec. Stricter than git's own
/// rules on purpose: nothing that could change the refspec's meaning.
fn valid_branch(branch: &str) -> bool {
    !branch.is_empty()
        && branch.len() <= 200
        && !branch.starts_with('-')
        && !branch.starts_with('/')
        && !branch.ends_with('/')
        && !branch.ends_with(".lock")
        && !branch.contains("..")
        && !branch.contains("//")
        && branch
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'/'))
}

fn open_or_init(repo_path: &Path) -> Result<gix::Repository, FetchError> {
    if repo_path.exists() {
        gix::open(repo_path).map_err(|e| FetchError::Other(format!("Could not open the marketplace cache: {}", e)))
    } else {
        if let Some(parent) = repo_path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| FetchError::Other(format!("Could not create {}: {}", parent.display(), e)))?;
        }
        gix::init_bare(repo_path)
            .map_err(|e| FetchError::Other(format!("Could not create the marketplace cache: {}", e)))
    }
}

/// Init the bare repo if missing, fetch `branch` (or the remote's default
/// branch) into [`HEAD_REF`], and return the head commit hex.
pub fn fetch(
    repo_path: &Path,
    url: &str,
    branch: Option<&str>,
    cred: Option<Credential>,
) -> Result<String, FetchError> {
    let refspec = match branch {
        Some(b) if !valid_branch(b) => {
            return Err(FetchError::Other(format!("{:?} is not a valid branch name", b)));
        }
        Some(b) => format!("+refs/heads/{}:{}", b, HEAD_REF),
        None => format!("+HEAD:{}", HEAD_REF),
    };
    let repo = open_or_init(repo_path)?;
    let remote = repo
        .remote_at(url)
        .map_err(|e| FetchError::Other(format!("Invalid repository URL: {}", e)))?
        .with_refspecs([refspec.as_str()], gix::remote::Direction::Fetch)
        .map_err(|e| FetchError::Other(format!("Invalid refspec: {}", e)))?;
    let connection = remote
        .connect(gix::remote::Direction::Fetch)
        .map_err(classify)?
        .with_credentials(move |action| match (action, &cred) {
            (gix::credentials::helper::Action::Get(ctx), Some(c)) => {
                Ok(Some(gix::credentials::protocol::Outcome {
                    identity: gix::sec::identity::Account {
                        username: c.username.clone(),
                        password: c.password.clone(),
                        oauth_refresh_token: None,
                    },
                    next: gix::credentials::helper::NextAction::from(ctx),
                }))
            }
            _ => Ok(None),
        });
    connection
        .prepare_fetch(gix::progress::Discard, Default::default())
        .map_err(classify)?
        .receive(gix::progress::Discard, &AtomicBool::new(false))
        .map_err(classify)?;
    cached_head(repo_path)
        .map_err(FetchError::Other)?
        .ok_or_else(|| FetchError::Other("The remote did not return a branch to fetch".to_string()))
}

/// Current [`HEAD_REF`], if fetched before.
pub fn cached_head(repo_path: &Path) -> Result<Option<String>, String> {
    if !repo_path.exists() {
        return Ok(None);
    }
    let repo = gix::open(repo_path).map_err(|e| format!("Could not open the marketplace cache: {}", e))?;
    let reference = repo
        .try_find_reference(HEAD_REF)
        .map_err(|e| format!("Could not read {}: {}", HEAD_REF, e))?;
    match reference {
        None => Ok(None),
        Some(mut r) => {
            let id = r
                .peel_to_id()
                .map_err(|e| format!("Could not resolve {}: {}", HEAD_REF, e))?;
            Ok(Some(id.to_string()))
        }
    }
}

pub fn has_commit(repo_path: &Path, commit: &str) -> bool {
    let Ok(repo) = gix::open(repo_path) else { return false };
    let Ok(oid) = gix::ObjectId::from_hex(commit.as_bytes()) else { return false };
    repo.find_commit(oid).is_ok()
}

/// Make `refs/triple-c/pins/*` exactly the given set (commits missing from
/// the cache are skipped), so pinned commits survive later fetches.
pub fn set_pins(repo_path: &Path, commits: &[String]) -> Result<(), String> {
    let repo = gix::open(repo_path).map_err(|e| format!("Could not open the marketplace cache: {}", e))?;
    let wanted: std::collections::BTreeSet<&str> = commits.iter().map(String::as_str).collect();

    let mut existing = Vec::new();
    let platform = repo.references().map_err(|e| format!("Could not list refs: {}", e))?;
    for reference in platform
        .prefixed(PIN_PREFIX)
        .map_err(|e| format!("Could not list pins: {}", e))?
    {
        let reference = reference.map_err(|e| format!("Could not read a pin: {:?}", e))?;
        existing.push(reference.name().as_bstr().to_string());
    }

    for name in &existing {
        let commit = name.trim_start_matches(PIN_PREFIX);
        if !wanted.contains(commit) {
            if let Some(r) = repo
                .try_find_reference(name.as_str())
                .map_err(|e| format!("Could not read {}: {}", name, e))?
            {
                r.delete().map_err(|e| format!("Could not remove {}: {}", name, e))?;
            }
        }
    }
    for commit in wanted {
        let name = format!("{}{}", PIN_PREFIX, commit);
        if existing.contains(&name) {
            continue;
        }
        let Ok(oid) = gix::ObjectId::from_hex(commit.as_bytes()) else { continue };
        if repo.find_commit(oid).is_err() {
            continue;
        }
        repo.reference(name.as_str(), oid, gix::refs::transaction::PreviousValue::Any, "triple-c pin")
            .map_err(|e| format!("Could not pin {}: {}", commit, e))?;
    }
    Ok(())
}
```

- [ ] **Step 6: Run the tests to verify they pass**

Run: `cd /workspace/triple-c/app/src-tauri && cargo test --lib marketplace:: 2>&1 | grep -E "^test |^test result"`
Expected: all `marketplace::` tests pass, including `git::tests::fetch_error_mapping`, `fetches_default_branch_then_updates`, `fetches_a_named_branch`, `pins_are_exactly_the_requested_set` (these three print `ok` even without `git`, because they return early — on CI runners `git` is present, so they really run).

- [ ] **Step 7: Format and commit**

```bash
cd /workspace/triple-c/app/src-tauri
rustfmt --edition 2021 src/marketplace/git.rs src/marketplace/tree.rs src/marketplace/mod.rs
```

```bash
cd /workspace/triple-c
git add app/src-tauri/Cargo.toml app/src-tauri/Cargo.lock app/src-tauri/src/marketplace/git.rs app/src-tauri/src/marketplace/tree.rs app/src-tauri/src/marketplace/mod.rs
git commit -m "Marketplace: gix cache with credentialed fetch, pins and GitTree

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

### Task 5: Accounts — credentials, token checks, fetch-error advice

**Files:**
- Modify: `app/src-tauri/src/storage/secure.rs` (marketplace token helpers + test)
- Create: `app/src-tauri/src/marketplace/auth.rs`
- Modify: `app/src-tauri/src/marketplace/mod.rs` (`pub mod auth;`)
- Test: unit tests in `auth.rs` (a local `axum` server stands in for the GitHub/Gitea/GitLab APIs) and `secure.rs`

**Interfaces:**
- Consumes: `models::marketplace::{MarketplaceAccount, AccountMethod}` (Task 2); `git::{Credential, FetchError}` (Task 4).
- Produces: `secure::{store_marketplace_token(account_id, token), get_marketplace_token(account_id) -> Result<Option<String>, String>, delete_marketplace_token(account_id)}` (service `triple-c-marketplace-account-<id>`, ids must match `[A-Za-z0-9-]{1,64}`); `auth::{HostKind, host_kind, host_of, fetch_username, resolve_credential, validate_token /* -> Result<Option<String>, String> */, gh_host_available, gh_host_login, describe_fetch_error}`. `host_of` rejects non-https URLs and URLs with embedded credentials. `resolve_credential` errors (user-facing): GhHost not logged in → "gh on this computer is not logged in to <host>. Run `gh auth login --hostname <host>` in a terminal, then try again."; missing keychain token → "No token is stored for the account \"<label>\". Remove it and sign in again."

- [ ] **Step 1: Write the failing tests**

In `app/src-tauri/src/storage/secure.rs`, add this test inside the existing `#[cfg(test)] mod tests { … }` (after `an_unlisted_key_cannot_be_stored_at_all`):

```rust

    /// Account ids become part of a keychain service name, so a malformed one
    /// is refused before any entry is constructed — and so before the
    /// keychain is touched, which is also what lets this run in CI.
    #[test]
    fn marketplace_token_ids_are_validated_before_the_keychain() {
        for bad in ["", "../x", "a b", "x;y", &"a".repeat(65)] {
            let err = store_marketplace_token(bad, "test-token-not-real").unwrap_err();
            assert!(err.contains("Invalid marketplace account id"), "{bad:?}: {err}");
            assert!(!err.contains("test-token-not-real"));
            assert!(get_marketplace_token(bad).is_err());
            assert!(delete_marketplace_token(bad).is_err());
        }
        let err = store_marketplace_token("0b9e6a2c-1111-4222-8333-944445555666", "   ").unwrap_err();
        assert!(err.contains("empty"));
    }
```

Add `pub mod auth;` to `app/src-tauri/src/marketplace/mod.rs` (alphabetical: `auth`, `catalog`, `git`, `tree`).

Create `app/src-tauri/src/marketplace/auth.rs` with only its tests for now:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn account(host: &str, username: Option<&str>) -> MarketplaceAccount {
        MarketplaceAccount {
            id: "acc-1".into(),
            label: "Work".into(),
            host: host.into(),
            method: AccountMethod::Token,
            username: username.map(str::to_string),
        }
    }

    #[test]
    fn host_of_accepts_https_only() {
        assert_eq!(host_of("https://GitHub.com/a/b.git").unwrap(), "github.com");
        assert_eq!(host_of("https://git.example.com:8443/a/b").unwrap(), "git.example.com:8443");
        assert!(host_of("http://github.com/a/b").is_err());
        assert!(host_of("git@github.com:a/b.git").is_err());
        assert!(host_of("file:///tmp/x").is_err());
        let err = host_of("https://user:test-token-not-real@github.com/a/b").unwrap_err();
        assert!(!err.contains("test-token-not-real"));
    }

    #[test]
    fn fetch_username_per_host() {
        assert_eq!(fetch_username(&account("github.com", Some("me"))), "x-access-token");
        assert_eq!(fetch_username(&account("repo.example.net", Some("jk"))), "jk");
        assert_eq!(fetch_username(&account("repo.example.net", None)), "oauth2");
        assert_eq!(fetch_username(&account("repo.example.net", Some(" "))), "oauth2");
    }

    #[test]
    fn host_kinds() {
        assert_eq!(host_kind("GITHUB.com"), HostKind::GitHub);
        assert_eq!(host_kind("gitlab.com"), HostKind::GitLab);
        assert_eq!(host_kind("repo.example.net"), HostKind::Unknown);
    }

    #[test]
    fn describe_access_errors_names_account_and_org_causes() {
        let url = "https://github.com/acme/private-market.git";
        let msg = describe_fetch_error(&FetchError::Auth { status: 403 }, Some(&account("github.com", Some("me"))), url);
        assert!(msg.contains("\"Work\" (me)"), "{}", msg);
        assert!(msg.contains("HTTP 403"));
        assert!(msg.contains("third-party app access"));
        assert!(msg.contains("single sign-on"));
        assert!(msg.contains("fine-grained"));

        let anon = describe_fetch_error(&FetchError::NotFound, None, url);
        assert!(anon.contains("anonymously"));
        assert!(anon.contains("may be private"));

        let gitea = describe_fetch_error(
            &FetchError::Auth { status: 401 },
            Some(&account("repo.example.net", None)),
            "https://repo.example.net/o/r.git",
        );
        assert!(!gitea.contains("single sign-on"));
        assert!(gitea.contains("expired"));
    }

    #[test]
    fn describe_network_and_other_errors() {
        let msg = describe_fetch_error(&FetchError::Network("dns error".into()), None, "https://github.com/a/b");
        assert!(msg.contains("Could not reach github.com"));
        assert!(msg.contains("last fetched copy"));
        let msg = describe_fetch_error(&FetchError::Other("weird".into()), None, "https://github.com/a/b");
        assert!(msg.contains("weird"));
    }

    // ── validate_token against a local mock API ──────────────────────────────

    const FAKE: &str = "test-token-not-real";

    async fn serve(app: axum::Router) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        format!("http://{}", addr)
    }

    fn authorised(headers: &axum::http::HeaderMap, name: &str, want: &str) -> bool {
        headers.get(name).and_then(|v| v.to_str().ok()) == Some(want)
    }

    #[tokio::test]
    async fn github_token_returns_login_or_is_rejected() {
        use axum::{http::HeaderMap, http::StatusCode, routing::get, Json, Router};
        let app = Router::new().route(
            "/user",
            get(|headers: HeaderMap| async move {
                if authorised(&headers, "authorization", &format!("Bearer {}", FAKE)) {
                    Ok(Json(serde_json::json!({ "login": "octo" })))
                } else {
                    Err(StatusCode::UNAUTHORIZED)
                }
            }),
        );
        let base = serve(app).await;
        assert_eq!(
            validate_token_at("github.com", Some(&base), "unused", FAKE).await.unwrap(),
            Some("octo".to_string())
        );
        let err = validate_token_at("github.com", Some(&base), "unused", "wrong").await.unwrap_err();
        assert!(err.contains("HTTP 401"), "{}", err);
        assert!(!err.contains("wrong"), "the token must not appear in the error");
    }

    #[tokio::test]
    async fn gitea_is_detected_first() {
        use axum::{http::HeaderMap, http::StatusCode, routing::get, Json, Router};
        let app = Router::new().route(
            "/api/v1/user",
            get(|headers: HeaderMap| async move {
                if authorised(&headers, "authorization", &format!("token {}", FAKE)) {
                    Ok(Json(serde_json::json!({ "login": "jk" })))
                } else {
                    Err(StatusCode::UNAUTHORIZED)
                }
            }),
        );
        let site = serve(app).await;
        assert_eq!(validate_token_at("h", None, &site, FAKE).await.unwrap(), Some("jk".to_string()));
        assert!(validate_token_at("h", None, &site, "wrong").await.is_err());
    }

    #[tokio::test]
    async fn gitlab_is_tried_after_gitea_404() {
        use axum::{http::HeaderMap, http::StatusCode, routing::get, Json, Router};
        let app = Router::new().route(
            "/api/v4/user",
            get(|headers: HeaderMap| async move {
                if authorised(&headers, "private-token", FAKE) {
                    Ok(Json(serde_json::json!({ "username": "gl-user" })))
                } else {
                    Err(StatusCode::UNAUTHORIZED)
                }
            }),
        );
        let site = serve(app).await;
        assert_eq!(validate_token_at("h", None, &site, FAKE).await.unwrap(), Some("gl-user".to_string()));
    }

    #[tokio::test]
    async fn unknown_host_is_left_unchecked() {
        let site = serve(axum::Router::new()).await; // every path 404s
        assert_eq!(validate_token_at("h", None, &site, FAKE).await.unwrap(), None);
    }

    #[tokio::test]
    async fn validate_token_refuses_bad_input_without_network() {
        assert!(validate_token("-evil", FAKE).await.is_err());
        assert!(validate_token("github.com", "  ").await.is_err());
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cd /workspace/triple-c/app/src-tauri && cargo test --lib marketplace::auth 2>&1 | tail -20` (the crate fails to compile as a whole, so this one run shows both files' errors)
Expected: compile errors, e.g. "cannot find function store_marketplace_token", "cannot find function host_of".

- [ ] **Step 3: Implement**

Append to `app/src-tauri/src/storage/secure.rs`, directly above its `#[cfg(test)]` line:

```rust

// ─────────────────────────────────────────────────────────────────────────────
// Marketplace account tokens (global, one entry per account)
// ─────────────────────────────────────────────────────────────────────────────

/// Keychain service prefix; the account id completes it.
const MARKETPLACE_TOKEN_SERVICE_PREFIX: &str = "triple-c-marketplace-account-";

/// The service name for one account. Ids are uuids; anything else is refused
/// before a keychain entry is constructed.
fn marketplace_token_service(account_id: &str) -> Result<String, String> {
    let ok = !account_id.is_empty()
        && account_id.len() <= 64
        && account_id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-');
    if !ok {
        return Err(format!("Invalid marketplace account id {:?}", account_id));
    }
    Ok(format!("{}{}", MARKETPLACE_TOKEN_SERVICE_PREFIX, account_id))
}

pub fn store_marketplace_token(account_id: &str, token: &str) -> Result<(), String> {
    let service = marketplace_token_service(account_id)?;
    if token.trim().is_empty() {
        return Err("Refusing to store an empty marketplace token.".to_string());
    }
    let entry = keyring::Entry::new(&service, KEYCHAIN_ACCOUNT)
        .map_err(|e| format!("Keyring error: {}", e))?;
    entry
        .set_password(token.trim())
        .map_err(|e| format!("Failed to store the marketplace account token: {}", e))
}

pub fn get_marketplace_token(account_id: &str) -> Result<Option<String>, String> {
    read_entry(&marketplace_token_service(account_id)?, "the marketplace account token")
}

pub fn delete_marketplace_token(account_id: &str) -> Result<(), String> {
    delete_entry(&marketplace_token_service(account_id)?, "the marketplace account token")
}
```

Prepend to `app/src-tauri/src/marketplace/auth.rs`, above its tests module:

```rust
//! Marketplace accounts: where a fetch credential comes from, checking a
//! pasted token, and turning a failed fetch into advice a person can act on.
//!
//! Nothing here logs, returns or formats a token into an error string. A
//! `GhHost` account stores nothing at all: its token is asked of the host's
//! `gh` every time, so a later `gh auth refresh` or logout takes effect.

use std::time::Duration;

use crate::marketplace::git::{Credential, FetchError};
use crate::models::marketplace::{AccountMethod, MarketplaceAccount};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostKind {
    GitHub,
    Gitea,
    GitLab,
    Unknown,
}

/// Known by name only; Gitea (and self-hosted GitLab) are recognised by
/// probing their API in [`validate_token`].
pub fn host_kind(host: &str) -> HostKind {
    match host.to_ascii_lowercase().as_str() {
        "github.com" => HostKind::GitHub,
        "gitlab.com" => HostKind::GitLab,
        _ => HostKind::Unknown,
    }
}

/// `host[:port]` characters only — also what keeps a host safe as a `gh` argument.
fn valid_host(host: &str) -> bool {
    !host.is_empty()
        && host.len() <= 253
        && !host.starts_with('-')
        && host
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b':'))
}

/// The host of an `https://` marketplace URL, lowercased, with a non-default port kept.
pub fn host_of(url: &str) -> Result<String, String> {
    let parsed = url::Url::parse(url.trim()).map_err(|e| format!("{:?} is not a valid URL: {}", url, e))?;
    if parsed.scheme() != "https" {
        return Err("Only https:// marketplace URLs are supported.".to_string());
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(
            "Put credentials in a marketplace account, not in the URL.".to_string(),
        );
    }
    let host = parsed
        .host_str()
        .ok_or_else(|| format!("{:?} has no host", url))?
        .to_ascii_lowercase();
    let host = match parsed.port() {
        Some(port) => format!("{}:{}", host, port),
        None => host,
    };
    if !valid_host(&host) {
        return Err(format!("{:?} is not a supported host name", host));
    }
    Ok(host)
}

/// The username sent with the token over HTTPS.
pub fn fetch_username(account: &MarketplaceAccount) -> String {
    if host_kind(&account.host) == HostKind::GitHub {
        return "x-access-token".to_string();
    }
    account
        .username
        .clone()
        .filter(|u| !u.trim().is_empty())
        .unwrap_or_else(|| "oauth2".to_string())
}

// ─────────────────────────────────────────────────────────────────────────────
// Host `gh`
// ─────────────────────────────────────────────────────────────────────────────

const GH_TIMEOUT: Duration = Duration::from_secs(15);

/// Run the host's `gh` with a plain argv (no shell) and return trimmed stdout.
async fn run_gh(args: &[&str]) -> Result<String, String> {
    let mut cmd = tokio::process::Command::new("gh");
    cmd.args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    let output = tokio::time::timeout(GH_TIMEOUT, cmd.output())
        .await
        .map_err(|_| "gh did not answer within 15 seconds".to_string())?
        .map_err(|e| format!("Could not run gh: {}", e))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(stderr.lines().next().unwrap_or("gh failed").trim().to_string());
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

pub async fn gh_host_available() -> bool {
    run_gh(&["--version"]).await.is_ok()
}

fn gh_login_instructions(host: &str) -> String {
    format!(
        "gh on this computer is not logged in to {host}. Run `gh auth login --hostname {host}` \
         in a terminal, then try again.",
        host = host
    )
}

/// The login name `gh` on the host is signed in as for `host`.
pub async fn gh_host_login(host: &str) -> Result<String, String> {
    if !valid_host(host) {
        return Err(format!("{:?} is not a supported host name", host));
    }
    run_gh(&["auth", "status", "--hostname", host])
        .await
        .map_err(|_| gh_login_instructions(host))?;
    let login = run_gh(&["api", "user", "--hostname", host, "--jq", ".login"]).await?;
    if login.is_empty() {
        return Err(gh_login_instructions(host));
    }
    Ok(login)
}

/// Resolve the credential for an account: `GhHost` → `gh auth token
/// --hostname <host>`; `GhContainer`/`Token` → the keychain.
pub async fn resolve_credential(account: &MarketplaceAccount) -> Result<Credential, String> {
    let password = match account.method {
        AccountMethod::GhHost => {
            if !valid_host(&account.host) {
                return Err(format!("{:?} is not a supported host name", account.host));
            }
            let token = run_gh(&["auth", "token", "--hostname", &account.host])
                .await
                .map_err(|_| gh_login_instructions(&account.host))?;
            if token.is_empty() {
                return Err(gh_login_instructions(&account.host));
            }
            token
        }
        AccountMethod::GhContainer | AccountMethod::Token => {
            crate::storage::secure::get_marketplace_token(&account.id)?.ok_or_else(|| {
                format!(
                    "No token is stored for the account \"{}\". Remove it and sign in again.",
                    account.label
                )
            })?
        }
    };
    Ok(Credential { username: fetch_username(account), password })
}

// ─────────────────────────────────────────────────────────────────────────────
// Token validation
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, PartialEq, Eq)]
enum Probe {
    Login(String),
    Rejected(u16),
    NotThisKind,
}

fn http_client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .user_agent("Triple-C")
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|e| format!("Could not create an HTTP client: {}", e))
}

/// One "who am I" call. `base` is the API root for GitHub
/// (`https://api.github.com`) and the site root for Gitea/GitLab.
async fn who_am_i(client: &reqwest::Client, kind: HostKind, base: &str, token: &str) -> Result<Probe, String> {
    let (url, header, value, field) = match kind {
        HostKind::GitHub => (format!("{}/user", base), "Authorization", format!("Bearer {}", token), "login"),
        HostKind::Gitea => (format!("{}/api/v1/user", base), "Authorization", format!("token {}", token), "login"),
        HostKind::GitLab => (format!("{}/api/v4/user", base), "PRIVATE-TOKEN", token.to_string(), "username"),
        HostKind::Unknown => return Ok(Probe::NotThisKind),
    };
    let response = client
        .get(&url)
        .header(header, value)
        .header("Accept", "application/json")
        .send()
        .await
        // reqwest's error text carries the URL, never the header.
        .map_err(|e| format!("Could not reach {}: {}", base, e.without_url()))?;
    let status = response.status().as_u16();
    match status {
        200 => {
            let json: serde_json::Value = match response.json().await {
                Ok(json) => json,
                Err(_) => return Ok(Probe::NotThisKind),
            };
            match json.get(field).and_then(|v| v.as_str()) {
                Some(login) if !login.is_empty() => Ok(Probe::Login(login.to_string())),
                _ => Ok(Probe::NotThisKind),
            }
        }
        401 | 403 => Ok(Probe::Rejected(status)),
        404 => Ok(Probe::NotThisKind),
        other => Err(format!("{} answered HTTP {} when checking the token", base, other)),
    }
}

fn rejected(host: &str, status: u16) -> String {
    format!(
        "{} rejected the token (HTTP {}). Check that it has not expired and can read repositories.",
        host, status
    )
}

/// GitHub is asked at `github_api`; anything else is probed as Gitea, then
/// GitLab, at `site`. `Ok(None)`: the host is neither, so the token could not
/// be checked here — the marketplace's test fetch checks it instead.
async fn validate_token_at(
    host: &str,
    github_api: Option<&str>,
    site: &str,
    token: &str,
) -> Result<Option<String>, String> {
    let client = http_client()?;
    if let Some(api) = github_api {
        return match who_am_i(&client, HostKind::GitHub, api, token).await? {
            Probe::Login(login) => Ok(Some(login)),
            Probe::Rejected(status) => Err(rejected(host, status)),
            Probe::NotThisKind => Err(format!("{} did not return a user for this token", host)),
        };
    }
    for kind in [HostKind::Gitea, HostKind::GitLab] {
        match who_am_i(&client, kind, site, token).await? {
            Probe::Login(login) => return Ok(Some(login)),
            Probe::Rejected(status) => return Err(rejected(host, status)),
            Probe::NotThisKind => {}
        }
    }
    Ok(None)
}

/// "Who am I" check for a pasted token. `Ok(Some(login))` when the host
/// confirmed it; `Ok(None)` when the host is not GitHub, Gitea or GitLab and
/// the token is left to the first fetch to prove.
pub async fn validate_token(host: &str, token: &str) -> Result<Option<String>, String> {
    if !valid_host(host) {
        return Err(format!("{:?} is not a supported host name", host));
    }
    if token.trim().is_empty() {
        return Err("Paste a token first.".to_string());
    }
    let site = format!("https://{}", host);
    match host_kind(host) {
        HostKind::GitHub => validate_token_at(host, Some("https://api.github.com"), &site, token.trim()).await,
        _ => validate_token_at(host, None, &site, token.trim()).await,
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Fetch errors
// ─────────────────────────────────────────────────────────────────────────────

fn who(account: Option<&MarketplaceAccount>) -> String {
    match account {
        None => "anonymously (no account)".to_string(),
        Some(a) => match &a.username {
            Some(u) if !u.is_empty() => format!("with the account \"{}\" ({})", a.label, u),
            _ => format!("with the account \"{}\"", a.label),
        },
    }
}

/// User-facing message for a failed fetch, naming the account used and, for
/// access problems, the usual organisation causes with the page that fixes each.
pub fn describe_fetch_error(err: &FetchError, account: Option<&MarketplaceAccount>, url: &str) -> String {
    let host = host_of(url).unwrap_or_else(|_| url.to_string());
    match err {
        FetchError::Auth { .. } | FetchError::NotFound => {
            let what = match err {
                FetchError::Auth { status } => format!("access was denied (HTTP {})", status),
                _ => "the repository was not found".to_string(),
            };
            let mut msg = format!("Could not read {} {}: {}.", url, who(account), what);
            if account.is_none() {
                msg.push_str(
                    "\n• The repository may be private — choose an account that can read it.",
                );
            }
            if host_kind(&host) == HostKind::GitHub {
                msg.push_str(
                    "\n• The organization may restrict third-party app access and not have approved \
                     the GitHub CLI or your token: \
                     https://docs.github.com/en/organizations/managing-oauth-access-to-your-organizations-data/about-oauth-app-access-restrictions\
                     \n• If the organization uses SAML single sign-on, the token must be authorized for it: \
                     https://github.com/settings/tokens\
                     \n• A fine-grained token only reaches repositories of the owner it was created for: \
                     https://github.com/settings/personal-access-tokens",
                );
            } else if account.is_some() {
                msg.push_str("\n• Check that the account's token has not expired and can read this repository.");
            }
            msg
        }
        FetchError::Network(m) => format!(
            "Could not reach {}: {}. The last fetched copy is still used.",
            host, m
        ),
        FetchError::Other(m) => format!("Fetching {} failed: {}", url, m),
    }
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cd /workspace/triple-c/app/src-tauri && cargo test --lib marketplace::auth && cargo test --lib storage::secure`
Expected: all pass — 10 in `marketplace::auth::tests` (the five `#[tokio::test]` ones bind `127.0.0.1:0` and never leave the machine) and the new `marketplace_token_ids_are_validated_before_the_keychain`. No test prints a token.

- [ ] **Step 5: Run the whole backend suite**

Run: `cd /workspace/triple-c/app/src-tauri && cargo test --lib 2>&1 | grep -E "^test result|FAILED"`
Expected: `test result: ok.` and no `FAILED`.

- [ ] **Step 6: Format and commit**

```bash
cd /workspace/triple-c/app/src-tauri
rustfmt --edition 2021 src/marketplace/auth.rs src/marketplace/mod.rs
# secure.rs: format only the new block by hand if rustfmt would reflow unrelated code
```

```bash
cd /workspace/triple-c
git add app/src-tauri/src/storage/secure.rs app/src-tauri/src/marketplace/auth.rs app/src-tauri/src/marketplace/mod.rs
git commit -m "Marketplace: account credentials, token validation and fetch-error advice

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 6: Item diff, MarketplaceManager, refresh and update detection

**Files:**
- Modify: `app/src-tauri/Cargo.toml` (add `similar = "2"`)
- Create: `app/src-tauri/src/marketplace/diff.rs`
- Create: `app/src-tauri/src/marketplace/test_support.rs`
- Modify: `app/src-tauri/src/marketplace/mod.rs` (created in Tasks 3–5 with `pub mod tree; pub mod catalog; pub mod git; pub mod auth;`)
- Modify: `app/src-tauri/src/lib.rs` (`AppState.marketplace`)

**Interfaces:**
- Consumes: `models::marketplace::*` (Task 2); `tree::{TreeView, GitTree}` (Task 3); `catalog::{parse_catalog, item_files, item_fingerprint, ItemFile}` (Task 3); `git::{fetch, cache_path, cached_head, FetchError, Credential}` (Task 4); `auth::{resolve_credential, describe_fetch_error}` (Task 5).
- Produces: `diff::item_diff`, `MarketplaceManager` (full API from the contract), `refresh_marketplace`, `load_cached_snapshot`, `compute_updates`, `pins_by_marketplace`, `head_for`, `test_support::GitFixture`, `AppState.marketplace: Arc<MarketplaceManager>`.

- [ ] **Step 1: Add the dependency**

In `app/src-tauri/Cargo.toml` under `[dependencies]`, after `url = "2"`:

```toml
similar = "2"
```

If `[dev-dependencies]` has no `tempfile` yet (Task 4 adds it), add `tempfile = "3"` there.

Run: `cd app/src-tauri && cargo check --lib`
Expected: compiles (warnings allowed).

- [ ] **Step 2: Create the git fixture helper**

Create `app/src-tauri/src/marketplace/test_support.rs`:

```rust
//! Test-only helpers: throwaway git repositories built with the `git` CLI, so
//! marketplace code is exercised against real git objects over `file://`.

use std::fs;
use std::path::Path;
use std::process::Command;

pub struct GitFixture {
    pub dir: tempfile::TempDir,
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(["-c", "user.name=Triple-C Test", "-c", "user.email=test@example.invalid"])
        .args(args)
        .current_dir(dir)
        .output()
        .expect("run git");
    assert!(
        out.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

impl GitFixture {
    /// `None` (with a note on stderr) when `git` is not installed; callers skip.
    pub fn new() -> Option<Self> {
        if Command::new("git").arg("--version").output().is_err() {
            eprintln!("skipping: git is not installed");
            return None;
        }
        let dir = tempfile::tempdir().expect("tempdir");
        git(dir.path(), &["init", "-q", "-b", "main"]);
        Some(Self { dir })
    }

    pub fn url(&self) -> String {
        format!("file://{}", self.dir.path().display())
    }

    pub fn write(&self, path: &str, contents: &str) -> &Self {
        let p = self.dir.path().join(path);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(&p, contents).unwrap();
        self
    }

    pub fn write_exec(&self, path: &str, contents: &str) -> &Self {
        self.write(path, contents);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let p = self.dir.path().join(path);
            fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
        }
        self
    }

    pub fn remove(&self, path: &str) -> &Self {
        let p = self.dir.path().join(path);
        if p.is_dir() {
            fs::remove_dir_all(&p).unwrap();
        } else {
            fs::remove_file(&p).unwrap();
        }
        self
    }

    /// Commit everything and return the new commit id (40 hex).
    pub fn commit(&self, message: &str) -> String {
        git(self.dir.path(), &["add", "-A"]);
        git(self.dir.path(), &["commit", "-q", "--allow-empty", "-m", message]);
        git(self.dir.path(), &["rev-parse", "HEAD"])
    }

    /// A repo with one item of every kind, committed. Returns the commit.
    pub fn with_all_kinds(&self) -> String {
        self.write(
            "agents/code-reviewer.md",
            "---\nname: code-reviewer\ndescription: Reviews code\n---\nReview the diff.\n",
        )
        .write(
            "skills/example-skill/SKILL.md",
            "---\nname: example-skill\ndescription: An example skill\n---\nDo the thing.\n",
        )
        .write(
            "commands/example-command.md",
            "---\ndescription: An example command\n---\nRun the example.\n",
        )
        .write(
            "hooks/notify-on-stop/hook.json",
            r#"{"name":"notify-on-stop","description":"Ping on stop","hooks":{"Stop":[{"hooks":[{"type":"command","command":"${HOOK_DIR}/notify.sh"}]}]}}"#,
        )
        .write_exec("hooks/notify-on-stop/notify.sh", "#!/bin/sh\necho done\n")
        .write(
            "plugins/.claude-plugin/marketplace.json",
            r#"{"name":"upstream","owner":{"name":"Test"},"plugins":[{"name":"example-plugin","source":"./example-plugin","description":"An example plugin"}]}"#,
        )
        .write(
            "plugins/example-plugin/.claude-plugin/plugin.json",
            r#"{"name":"example-plugin","version":"0.1.0"}"#,
        )
        .write(
            "plugins/example-plugin/skills/hello/SKILL.md",
            "---\nname: hello\ndescription: Says hello\n---\nSay hello.\n",
        );
        self.commit("all kinds")
    }
}
```

- [ ] **Step 3: Write the failing diff tests**

Create `app/src-tauri/src/marketplace/diff.rs` with only the tests first:

```rust
//! Text diff of one item between two commits, for the "Update" review.

use std::collections::BTreeMap;
use std::path::Path;

use similar::TextDiff;

use super::catalog::{item_files, ItemFile};
use super::tree::GitTree;
use crate::models::marketplace::{FileChange, FileDiff, ItemKind};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::marketplace::git;
    use crate::marketplace::test_support::GitFixture;

    fn f(path: &str, text: &str, executable: bool) -> ItemFile {
        ItemFile { rel_path: path.to_string(), data: text.as_bytes().to_vec(), executable }
    }

    #[test]
    fn unchanged_files_are_omitted_and_changes_are_classified() {
        let old = vec![f("a.md", "one\n", false), f("gone.sh", "x\n", true), f("same", "s\n", false)];
        let new = vec![f("a.md", "two\n", false), f("new.txt", "n\n", false), f("same", "s\n", false)];
        let diffs = diff_files(&old, &new);
        let summary: Vec<(&str, FileChange)> =
            diffs.iter().map(|d| (d.path.as_str(), d.change.clone())).collect();
        assert_eq!(
            summary,
            vec![
                ("a.md", FileChange::Modified),
                ("gone.sh", FileChange::Removed),
                ("new.txt", FileChange::Added),
            ]
        );
        let a = diffs[0].unified.as_deref().unwrap();
        assert!(a.contains("-one") && a.contains("+two"), "{a}");
    }

    #[test]
    fn binary_files_have_no_text_diff() {
        let old = vec![ItemFile { rel_path: "b.bin".into(), data: vec![0, 1, 2], executable: false }];
        let new = vec![ItemFile { rel_path: "b.bin".into(), data: vec![0, 1, 3], executable: false }];
        let diffs = diff_files(&old, &new);
        assert_eq!(diffs.len(), 1);
        assert_eq!(diffs[0].unified, None);
    }

    #[test]
    fn an_executable_bit_change_is_reported() {
        let old = vec![f("run.sh", "echo\n", false)];
        let new = vec![f("run.sh", "echo\n", true)];
        let diffs = diff_files(&old, &new);
        assert_eq!(diffs.len(), 1);
        assert!(diffs[0].unified.as_deref().unwrap().contains("executable: false -> true"));
    }

    #[test]
    fn item_diff_reads_both_commits_from_the_cache() {
        let Some(fx) = GitFixture::new() else { return };
        let c1 = fx.with_all_kinds();
        fx.write(
            "hooks/notify-on-stop/notify.sh",
            "#!/bin/sh\ncurl https://example.invalid\n",
        );
        let c2 = fx.commit("change hook");
        let data = tempfile::tempdir().unwrap();
        let repo = git::cache_path(data.path(), "m1");
        git::fetch(&repo, &fx.url(), None, None).unwrap();

        let diffs = item_diff(&repo, ItemKind::Hook, "notify-on-stop", &c1, &c2).unwrap();
        assert_eq!(diffs.len(), 1);
        assert_eq!(diffs[0].path, "notify.sh");
        assert!(diffs[0].unified.as_deref().unwrap().contains("+curl https://example.invalid"));
    }
}
```

- [ ] **Step 4: Run tests to verify they fail**

Add `pub mod diff;` and `#[cfg(test)] pub(crate) mod test_support;` to `app/src-tauri/src/marketplace/mod.rs`.

Run: `cd app/src-tauri && cargo test --lib marketplace::diff`
Expected: FAIL to compile — `cannot find function diff_files` / `item_diff`.

- [ ] **Step 5: Implement the diff**

Insert above the `#[cfg(test)]` block in `diff.rs`:

```rust
/// Files of `kind`/`key` at `commit`, or an empty list when the item does not
/// exist (or is not installable) at that commit — a removal upstream then reads
/// as every file removed rather than as an error.
fn files_at(repo_path: &Path, kind: ItemKind, key: &str, commit: &str) -> Result<Vec<ItemFile>, String> {
    let tree = GitTree::open(repo_path, commit)?;
    Ok(item_files(&tree, kind, key).unwrap_or_default())
}

pub fn item_diff(
    repo_path: &Path,
    kind: ItemKind,
    key: &str,
    from_commit: &str,
    to_commit: &str,
) -> Result<Vec<FileDiff>, String> {
    let old = files_at(repo_path, kind, key, from_commit)?;
    let new = files_at(repo_path, kind, key, to_commit)?;
    Ok(diff_files(&old, &new))
}

fn as_text(data: &[u8]) -> Option<&str> {
    if data.contains(&0) {
        return None;
    }
    std::str::from_utf8(data).ok()
}

fn unified(path: &str, old: &str, new: &str) -> String {
    TextDiff::from_lines(old, new)
        .unified_diff()
        .context_radius(3)
        .header(&format!("a/{path}"), &format!("b/{path}"))
        .to_string()
}

/// Per-file diff, sorted by path; files identical in content and mode are left out.
pub(crate) fn diff_files(old: &[ItemFile], new: &[ItemFile]) -> Vec<FileDiff> {
    let old: BTreeMap<&str, &ItemFile> = old.iter().map(|f| (f.rel_path.as_str(), f)).collect();
    let new: BTreeMap<&str, &ItemFile> = new.iter().map(|f| (f.rel_path.as_str(), f)).collect();
    let mut paths: Vec<&str> = old.keys().chain(new.keys()).copied().collect();
    paths.sort_unstable();
    paths.dedup();

    let mut out = Vec::new();
    for path in paths {
        match (old.get(path), new.get(path)) {
            (Some(o), Some(n)) => {
                if o.data == n.data && o.executable == n.executable {
                    continue;
                }
                let text = match (as_text(&o.data), as_text(&n.data)) {
                    (Some(a), Some(b)) => {
                        let mut s = String::new();
                        if o.executable != n.executable {
                            s.push_str(&format!("# executable: {} -> {}\n", o.executable, n.executable));
                        }
                        s.push_str(&unified(path, a, b));
                        Some(s)
                    }
                    _ => None,
                };
                out.push(FileDiff { path: path.to_string(), change: FileChange::Modified, unified: text });
            }
            (Some(o), None) => out.push(FileDiff {
                path: path.to_string(),
                change: FileChange::Removed,
                unified: as_text(&o.data).map(|a| unified(path, a, "")),
            }),
            (None, Some(n)) => out.push(FileDiff {
                path: path.to_string(),
                change: FileChange::Added,
                unified: as_text(&n.data).map(|b| unified(path, "", b)),
            }),
            (None, None) => {}
        }
    }
    out
}
```

- [ ] **Step 6: Run the diff tests**

Run: `cd app/src-tauri && cargo test --lib marketplace::diff`
Expected: 4 passed (the last one prints "skipping" and passes when `git` is absent).

- [ ] **Step 7: Write the failing manager/refresh tests**

Append to `app/src-tauri/src/marketplace/mod.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::marketplace::{Marketplace, MarketplaceInstall};
    use crate::marketplace::test_support::GitFixture;

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
        MarketplaceInstall { marketplace_id: "m1".into(), kind, key: key.into(), commit: commit.into() }
    }

    #[tokio::test]
    async fn refresh_parses_the_catalog_at_head() {
        let Some(fx) = GitFixture::new() else { return };
        let c1 = fx.with_all_kinds();
        let data = tempfile::tempdir().unwrap();
        let mgr = MarketplaceManager::new(data.path().to_path_buf());

        let snap = refresh_marketplace(&mgr, &settings_with(&fx.url()), "m1").await;

        assert_eq!(snap.fetch_error, None);
        assert_eq!(snap.head_commit.as_deref(), Some(c1.as_str()));
        assert!(snap.fetched_at.is_some());
        let mut keys: Vec<String> =
            snap.items.iter().map(|i| format!("{:?}:{}", i.kind, i.key)).collect();
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
        let first = refresh_marketplace(&mgr, &settings, "m1").await;
        assert_eq!(first.fetch_error, None);

        drop(fx); // the source repository disappears (offline, deleted, …)
        let second = refresh_marketplace(&mgr, &settings, "m1").await;

        assert!(second.fetch_error.is_some(), "expected a fetch error");
        assert_eq!(second.head_commit.as_deref(), Some(c1.as_str()));
        assert_eq!(second.items, first.items);
        assert_eq!(second.fetched_at, first.fetched_at);
    }

    #[tokio::test]
    async fn cached_snapshot_loads_without_network() {
        let Some(fx) = GitFixture::new() else { return };
        let c1 = fx.with_all_kinds();
        let data = tempfile::tempdir().unwrap();
        let settings = settings_with(&fx.url());
        {
            let mgr = MarketplaceManager::new(data.path().to_path_buf());
            refresh_marketplace(&mgr, &settings, "m1").await;
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
        refresh_marketplace(&mgr, &settings, "m1").await;

        let updates = compute_updates(&mgr, &settings, &[project]);

        assert_eq!(updates.len(), 1, "{updates:?}");
        assert_eq!(updates[0].item.key, "code-reviewer");
        assert_eq!(updates[0].pinned, c1);
        assert_eq!(updates[0].head, c2);
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
        let report = SyncReport { installed: vec!["agent:x".into()], ..Default::default() };
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
        assert!(mgr.set_gh_login_cancel(Some(tx3)).await, "slot is free after cancel");
    }
}
```

- [ ] **Step 8: Run tests to verify they fail**

Run: `cd app/src-tauri && cargo test --lib marketplace::tests`
Expected: FAIL to compile — `MarketplaceManager`, `refresh_marketplace`, … not found.

- [ ] **Step 9: Implement the manager and refresh**

Make the top of `app/src-tauri/src/marketplace/mod.rs` read (keep the `pub mod` lines Tasks 3–5 added; the full list after this task):

```rust
//! Marketplaces: git repos of agents, skills, commands, hooks and plugins that
//! are fetched on the host and synced into containers. See
//! `docs/superpowers/specs/2026-09-27-marketplace-design.md`.

pub mod auth;
pub mod catalog;
pub mod diff;
pub mod git;
pub mod tree;
#[cfg(test)]
pub(crate) mod test_support;

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use tokio::sync::oneshot;

use crate::models::marketplace::{
    CatalogItem, ItemKind, ItemUpdate, Marketplace, MarketplaceInstall, MarketplaceSnapshot,
    SyncReport,
};
use crate::models::{AppSettings, Project};
use catalog::{item_fingerprint, parse_catalog};
use tree::GitTree;

pub struct MarketplaceManager {
    data_root: PathBuf,
    snapshots: Mutex<HashMap<String, MarketplaceSnapshot>>,
    reports: Mutex<HashMap<String, SyncReport>>,
    gh_login_cancel: tokio::sync::Mutex<Option<oneshot::Sender<()>>>,
}

/// Project ids become file names; anything outside this set is not persisted.
fn safe_file_stem(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

impl MarketplaceManager {
    /// `data_root` is `<data_dir>/triple-c`.
    pub fn new(data_root: PathBuf) -> Self {
        Self {
            data_root,
            snapshots: Mutex::new(HashMap::new()),
            reports: Mutex::new(HashMap::new()),
            gh_login_cancel: tokio::sync::Mutex::new(None),
        }
    }

    pub fn data_root(&self) -> &Path {
        &self.data_root
    }

    pub fn snapshot(&self, marketplace_id: &str) -> Option<MarketplaceSnapshot> {
        self.snapshots.lock().unwrap().get(marketplace_id).cloned()
    }

    pub fn put_snapshot(&self, snap: MarketplaceSnapshot) {
        self.snapshots.lock().unwrap().insert(snap.marketplace_id.clone(), snap);
    }

    pub fn remove_snapshot(&self, marketplace_id: &str) {
        self.snapshots.lock().unwrap().remove(marketplace_id);
    }

    fn report_path(&self, project_id: &str) -> PathBuf {
        self.data_root.join("marketplace-sync").join(format!("{project_id}.json"))
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
        self.reports.lock().unwrap().insert(project_id.to_string(), report.clone());
        Some(report)
    }

    pub fn put_report(&self, project_id: &str, report: SyncReport) {
        self.reports.lock().unwrap().insert(project_id.to_string(), report.clone());
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
            log::warn!("Could not persist the marketplace sync report for {}: {}", project_id, e);
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

    pub async fn cancel_gh_login(&self) {
        if let Some(tx) = self.gh_login_cancel.lock().await.take() {
            let _ = tx.send(());
        }
    }
}

/// Head commit for a marketplace: the in-memory snapshot's, else the cache's.
pub fn head_for(mgr: &MarketplaceManager, m: &Marketplace) -> Option<String> {
    mgr.snapshot(&m.id)
        .and_then(|s| s.head_commit)
        .or_else(|| git::cached_head(&git::cache_path(mgr.data_root(), &m.id)).ok().flatten())
}

fn parse_at(repo: &Path, commit: &str) -> Result<Vec<CatalogItem>, String> {
    let tree = GitTree::open(repo, commit)?;
    Ok(parse_catalog(&tree))
}

pub fn load_cached_snapshot(mgr: &MarketplaceManager, marketplace: &Marketplace) -> MarketplaceSnapshot {
    let repo = git::cache_path(mgr.data_root(), &marketplace.id);
    let mut snap = MarketplaceSnapshot { marketplace_id: marketplace.id.clone(), ..Default::default() };
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

fn failed_snapshot(mgr: &MarketplaceManager, m: &Marketplace, message: String) -> MarketplaceSnapshot {
    let mut snap = mgr.snapshot(&m.id).unwrap_or_else(|| load_cached_snapshot(mgr, m));
    snap.fetch_error = Some(message);
    mgr.put_snapshot(snap.clone());
    snap
}

pub async fn refresh_marketplace(
    mgr: &MarketplaceManager,
    settings: &AppSettings,
    marketplace_id: &str,
) -> MarketplaceSnapshot {
    let Some(m) = settings.marketplaces.iter().find(|m| m.id == marketplace_id).cloned() else {
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
        Ok(Err(e)) => failed_snapshot(mgr, &m, auth::describe_fetch_error(&e, account.as_ref(), &m.url)),
        Err(e) => failed_snapshot(mgr, &m, format!("The refresh task failed: {e}")),
    }
}

fn item_changed(repo: &Path, inst: &MarketplaceInstall, head: &str) -> Result<bool, String> {
    let old = GitTree::open(repo, &inst.commit)?;
    let new = GitTree::open(repo, head)?;
    Ok(item_fingerprint(&old, inst.kind, &inst.key)? != item_fingerprint(&new, inst.kind, &inst.key)?)
}

pub fn compute_updates(mgr: &MarketplaceManager, settings: &AppSettings, projects: &[Project]) -> Vec<ItemUpdate> {
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    let all = settings
        .global_marketplace_installs
        .iter()
        .chain(projects.iter().flat_map(|p| p.marketplace_installs.iter()));
    for inst in all {
        if !seen.insert((inst.item_ref(), inst.commit.clone())) {
            continue;
        }
        let Some(m) = settings.marketplaces.iter().find(|m| m.id == inst.marketplace_id) else { continue };
        let Some(head) = head_for(mgr, m) else { continue };
        if head == inst.commit {
            continue;
        }
        let repo = git::cache_path(mgr.data_root(), &m.id);
        match item_changed(&repo, inst, &head) {
            Ok(true) => out.push(ItemUpdate { item: inst.item_ref(), pinned: inst.commit.clone(), head }),
            Ok(false) => {}
            Err(e) => log::debug!("Update check skipped for {}: {}", inst.key, e),
        }
    }
    out
}

pub fn pins_by_marketplace(settings: &AppSettings, projects: &[Project]) -> HashMap<String, Vec<String>> {
    let mut map: HashMap<String, BTreeSet<String>> = HashMap::new();
    let all = settings
        .global_marketplace_installs
        .iter()
        .chain(projects.iter().flat_map(|p| p.marketplace_installs.iter()));
    for inst in all {
        map.entry(inst.marketplace_id.clone()).or_default().insert(inst.commit.clone());
    }
    map.into_iter().map(|(k, v)| (k, v.into_iter().collect())).collect()
}
```

(`ItemKind` is imported for the tests' `use super::*`.)

- [ ] **Step 10: Run tests to verify they pass**

Run: `cd app/src-tauri && cargo test --lib marketplace::`
Expected: all marketplace tests pass (git-backed ones print "skipping" only when `git` is absent).

- [ ] **Step 11: Wire the manager into AppState**

In `app/src-tauri/src/lib.rs`, add the field to `AppState` (after `pending_settings_import`):

```rust
    pub marketplace: Arc<marketplace::MarketplaceManager>,
```

In `run()`, after `let lifecycle = Arc::new(Lifecycle::new());`:

```rust
    let marketplace = Arc::new(marketplace::MarketplaceManager::new(
        dirs::data_dir()
            .map(|d| d.join("triple-c"))
            .unwrap_or_else(|| std::env::temp_dir().join("triple-c")),
    ));
    let marketplace_setup = marketplace.clone();
```

and in `.manage(AppState { … })` add `marketplace,` after `pending_settings_import: …,`. (`marketplace_setup` is used in Task 11; until then add `let _ = &marketplace_setup;` right after its declaration to keep the build warning-free, and delete that line in Task 11.)

Run: `cd app/src-tauri && cargo check --lib`
Expected: compiles.

- [ ] **Step 12: Commit**

```bash
cd /workspace/triple-c
rustfmt --edition 2021 app/src-tauri/src/marketplace/mod.rs app/src-tauri/src/marketplace/diff.rs app/src-tauri/src/marketplace/test_support.rs
git add app/src-tauri/Cargo.toml app/src-tauri/Cargo.lock app/src-tauri/src/marketplace app/src-tauri/src/lib.rs
git commit -m "Marketplace: item diff, manager, refresh and update detection

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

### Task 7: Payload builder

**Files:**
- Create: `app/src-tauri/src/marketplace/payload.rs`
- Modify: `app/src-tauri/src/marketplace/mod.rs` (add `pub mod payload;`)

**Interfaces:**
- Consumes: `models::marketplace::{MarketplaceInstall, Marketplace, ItemKind, SkippedItem, is_valid_item_key, is_valid_commit, marketplace_slug}` (Task 2); `tree::GitTree`, `catalog::{item_files, plugin_catalog_entry, rendered_hook_settings, ItemFile}` (Task 3); `git::{cache_path, has_commit}` (Task 4); `test_support::GitFixture` (Task 6).
- Produces: `payload::{PayloadInput, Payload, build_payload}` exactly as in the contract; the tar layout and `manifest.json` shape from the contract (consumed by the Task 8 script).

- [ ] **Step 1: Write the failing tests**

Create `app/src-tauri/src/marketplace/payload.rs` with the imports and tests first:

```rust
//! Builds the tar a project's container receives: every effective install's
//! files, read from the cache at its pinned commit, plus `manifest.json` and a
//! generated Claude Code catalog per marketplace that contributes plugins.
//! Layout: see the Interface Contract in the plan / spec §4.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde_json::{json, Value};

use super::catalog::{item_files, plugin_catalog_entry, rendered_hook_settings, ItemFile};
use super::git;
use super::tree::GitTree;
use crate::models::marketplace::{
    is_valid_commit, is_valid_item_key, marketplace_slug, ItemKind, Marketplace, MarketplaceInstall,
    SkippedItem,
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::marketplace::test_support::GitFixture;
    use std::collections::HashMap;
    use std::io::Read;

    struct Entry {
        data: Vec<u8>,
        mode: u32,
    }

    fn unpack(tar_bytes: &[u8]) -> HashMap<String, Entry> {
        let mut archive = tar::Archive::new(tar_bytes);
        let mut out = HashMap::new();
        for e in archive.entries().unwrap() {
            let mut e = e.unwrap();
            let path = e.path().unwrap().to_string_lossy().into_owned();
            let mode = e.header().mode().unwrap();
            let mut data = Vec::new();
            e.read_to_end(&mut data).unwrap();
            out.insert(path, Entry { data, mode });
        }
        out
    }

    fn market(id: &str) -> Marketplace {
        Marketplace { id: id.into(), name: "Team Tools".into(), url: "https://example.invalid/r.git".into(), branch: None, account_id: None }
    }

    fn inst(kind: ItemKind, key: &str, commit: &str) -> MarketplaceInstall {
        MarketplaceInstall { marketplace_id: "m1aaaaaaaa".into(), kind, key: key.into(), commit: commit.into() }
    }

    /// Fetch the fixture into `<data>/marketplaces/m1aaaaaaaa.git`.
    fn cache(fx: &GitFixture, data: &Path) {
        let repo = git::cache_path(data, "m1aaaaaaaa");
        git::fetch(&repo, &fx.url(), None, None).unwrap();
    }

    #[test]
    fn every_kind_lands_at_its_contract_path() {
        let Some(fx) = GitFixture::new() else { return };
        let c = fx.with_all_kinds();
        let data = tempfile::tempdir().unwrap();
        cache(&fx, data.path());
        let installs = vec![
            inst(ItemKind::Agent, "code-reviewer", &c),
            inst(ItemKind::Skill, "example-skill", &c),
            inst(ItemKind::Command, "example-command", &c),
            inst(ItemKind::Hook, "notify-on-stop", &c),
            inst(ItemKind::Plugin, "example-plugin", &c),
        ];
        let marketplaces = vec![market("m1aaaaaaaa")];
        let p = build_payload(&PayloadInput { installs: &installs, marketplaces: &marketplaces, data_root: data.path() }).unwrap();

        assert!(p.skipped.is_empty(), "{:?}", p.skipped);
        let files = unpack(&p.tar);
        let slug = marketplace_slug("Team Tools", "m1aaaaaaaa");
        for path in [
            "agents/code-reviewer.md".to_string(),
            "skills/example-skill/SKILL.md".to_string(),
            "commands/example-command.md".to_string(),
            "hooks/notify-on-stop/hook.json".to_string(),
            "hooks/notify-on-stop/notify.sh".to_string(),
            format!("plugins/{slug}/.claude-plugin/marketplace.json"),
            format!("plugins/{slug}/example-plugin/.claude-plugin/plugin.json"),
            format!("plugins/{slug}/example-plugin/skills/hello/SKILL.md"),
            "manifest.json".to_string(),
        ] {
            assert!(files.contains_key(&path), "missing {path}; have {:?}", files.keys().collect::<Vec<_>>());
        }
        assert_eq!(files["hooks/notify-on-stop/notify.sh"].mode & 0o777, 0o755);
        assert_eq!(files["agents/code-reviewer.md"].mode & 0o777, 0o644);
    }

    #[test]
    fn manifest_and_generated_catalog_match_the_contract() {
        let Some(fx) = GitFixture::new() else { return };
        let c = fx.with_all_kinds();
        let data = tempfile::tempdir().unwrap();
        cache(&fx, data.path());
        let installs = vec![inst(ItemKind::Hook, "notify-on-stop", &c), inst(ItemKind::Plugin, "example-plugin", &c)];
        let marketplaces = vec![market("m1aaaaaaaa")];
        let p = build_payload(&PayloadInput { installs: &installs, marketplaces: &marketplaces, data_root: data.path() }).unwrap();
        let slug = marketplace_slug("Team Tools", "m1aaaaaaaa");

        let files = unpack(&p.tar);
        let manifest: Value = serde_json::from_slice(&files["manifest.json"].data).unwrap();
        assert_eq!(manifest, p.manifest);
        assert_eq!(manifest["version"], 1);
        let hook = &manifest["items"][0];
        assert_eq!(hook["kind"], "hook");
        assert_eq!(hook["dir"], "hooks/notify-on-stop");
        assert_eq!(
            hook["settings"]["Stop"][0]["hooks"][0]["command"],
            "/home/claude/.claude/triple-c/hooks/notify-on-stop/notify.sh"
        );
        let plugin = &manifest["items"][1];
        assert_eq!(plugin["kind"], "plugin");
        assert_eq!(plugin["slug"], slug.as_str());
        assert_eq!(
            manifest["plugin_marketplaces"],
            json!([{ "slug": slug, "dir": format!("plugins/{slug}"), "plugins": ["example-plugin"] }])
        );

        let catalog: Value =
            serde_json::from_slice(&files[&format!("plugins/{slug}/.claude-plugin/marketplace.json")].data).unwrap();
        assert_eq!(catalog["name"], format!("triple-c-{slug}"));
        assert_eq!(catalog["owner"]["name"], "Triple-C");
        assert_eq!(catalog["plugins"][0]["name"], "example-plugin");
        assert_eq!(catalog["plugins"][0]["source"], "./example-plugin");
    }

    #[test]
    fn items_that_cannot_be_built_are_skipped_not_fatal() {
        let Some(fx) = GitFixture::new() else { return };
        let c = fx.with_all_kinds();
        let data = tempfile::tempdir().unwrap();
        cache(&fx, data.path());
        let mut gone = inst(ItemKind::Agent, "code-reviewer", &c);
        gone.marketplace_id = "removed".into();
        let installs = vec![
            gone,
            inst(ItemKind::Agent, "code-reviewer", &"0".repeat(40)),
            inst(ItemKind::Agent, "does-not-exist", &c),
            inst(ItemKind::Command, "example-command", &c),
        ];
        let marketplaces = vec![market("m1aaaaaaaa")];
        let p = build_payload(&PayloadInput { installs: &installs, marketplaces: &marketplaces, data_root: data.path() }).unwrap();

        let skipped: Vec<&str> = p.skipped.iter().map(|s| s.item.as_str()).collect();
        assert_eq!(skipped, vec!["agent:code-reviewer", "agent:code-reviewer", "agent:does-not-exist"]);
        assert!(p.skipped[0].reason.contains("marketplace"), "{}", p.skipped[0].reason);
        assert!(p.skipped[1].reason.contains("cache"), "{}", p.skipped[1].reason);
        assert_eq!(p.manifest["items"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn a_second_marketplace_cannot_shadow_an_installed_name() {
        let Some(fx) = GitFixture::new() else { return };
        let c = fx.with_all_kinds();
        let data = tempfile::tempdir().unwrap();
        cache(&fx, data.path());
        let other = git::cache_path(data.path(), "m2bbbbbbbb");
        git::fetch(&other, &fx.url(), None, None).unwrap();
        let mut second = inst(ItemKind::Agent, "code-reviewer", &c);
        second.marketplace_id = "m2bbbbbbbb".into();
        let installs = vec![inst(ItemKind::Agent, "code-reviewer", &c), second];
        let marketplaces = vec![market("m1aaaaaaaa"), market("m2bbbbbbbb")];
        let p = build_payload(&PayloadInput { installs: &installs, marketplaces: &marketplaces, data_root: data.path() }).unwrap();
        assert_eq!(p.manifest["items"].as_array().unwrap().len(), 1);
        assert_eq!(p.skipped.len(), 1);
        assert!(p.skipped[0].reason.contains("another marketplace"));
    }

    #[test]
    fn an_empty_install_set_still_yields_a_manifest() {
        let data = tempfile::tempdir().unwrap();
        let p = build_payload(&PayloadInput { installs: &[], marketplaces: &[], data_root: data.path() }).unwrap();
        assert_eq!(p.manifest, json!({ "version": 1, "items": [], "plugin_marketplaces": [] }));
        assert!(unpack(&p.tar).contains_key("manifest.json"));
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Add `pub mod payload;` to `app/src-tauri/src/marketplace/mod.rs`.

Run: `cd app/src-tauri && cargo test --lib marketplace::payload`
Expected: FAIL to compile — `PayloadInput`, `build_payload` not found.

- [ ] **Step 3: Implement**

Insert above `#[cfg(test)]` in `payload.rs`:

```rust
pub struct PayloadInput<'a> {
    pub installs: &'a [MarketplaceInstall],
    pub marketplaces: &'a [Marketplace],
    /// data root used to find caches (see git::cache_path)
    pub data_root: &'a Path,
}

pub struct Payload {
    pub tar: Vec<u8>,
    pub manifest: Value,
    pub skipped: Vec<SkippedItem>,
}

fn kind_str(kind: ItemKind) -> &'static str {
    match kind {
        ItemKind::Agent => "agent",
        ItemKind::Skill => "skill",
        ItemKind::Command => "command",
        ItemKind::Hook => "hook",
        ItemKind::Plugin => "plugin",
    }
}

/// A relative path from `item_files` is joined under a directory we chose, so
/// it must not be able to climb out of it. The catalog already refuses such
/// entries; this is the second line.
fn safe_rel(rel: &str) -> bool {
    !rel.is_empty()
        && !rel.starts_with('/')
        && !rel.contains('\\')
        && rel.split('/').all(|seg| !seg.is_empty() && seg != "." && seg != "..")
}

struct TarWriter {
    builder: tar::Builder<Vec<u8>>,
    mtime: u64,
}

impl TarWriter {
    fn new() -> Self {
        let mtime = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        Self { builder: tar::Builder::new(Vec::new()), mtime }
    }

    fn file(&mut self, path: &str, data: &[u8], executable: bool) -> Result<(), String> {
        let mut header = tar::Header::new_gnu();
        header.set_size(data.len() as u64);
        header.set_mode(if executable { 0o755 } else { 0o644 });
        header.set_mtime(self.mtime);
        header.set_entry_type(tar::EntryType::Regular);
        self.builder
            .append_data(&mut header, path, data)
            .map_err(|e| format!("Could not add {path} to the marketplace payload: {e}"))
    }

    fn finish(self) -> Result<Vec<u8>, String> {
        self.builder.into_inner().map_err(|e| format!("Could not finish the marketplace payload: {e}"))
    }
}

struct PluginGroup {
    entries: Vec<Value>,
    keys: Vec<String>,
}

/// Files of one install, validated for use as payload paths.
fn install_files(repo: &Path, inst: &MarketplaceInstall) -> Result<(GitTree, Vec<ItemFile>), String> {
    let tree = GitTree::open(repo, &inst.commit)?;
    let files = item_files(&tree, inst.kind, &inst.key)?;
    if let Some(bad) = files.iter().find(|f| !safe_rel(&f.rel_path)) {
        return Err(format!("contains an unsafe path ({})", bad.rel_path));
    }
    Ok((tree, files))
}

pub fn build_payload(input: &PayloadInput) -> Result<Payload, String> {
    let mut tar = TarWriter::new();
    let mut items: Vec<Value> = Vec::new();
    let mut skipped: Vec<SkippedItem> = Vec::new();
    let mut plugin_groups: BTreeMap<String, PluginGroup> = BTreeMap::new();
    // Non-plugin items share one namespace in ~/.claude; plugins are namespaced
    // by their per-marketplace catalog, so they never collide.
    let mut taken: BTreeSet<(ItemKind, String)> = BTreeSet::new();

    for inst in input.installs {
        let label = format!("{}:{}", kind_str(inst.kind), inst.key);
        let mut skip = |reason: String| skipped.push(SkippedItem { item: label.clone(), reason });

        let Some(m) = input.marketplaces.iter().find(|m| m.id == inst.marketplace_id) else {
            skip("its marketplace has been removed".to_string());
            continue;
        };
        if !is_valid_item_key(&inst.key) || !is_valid_commit(&inst.commit) {
            skip("the saved install entry is invalid".to_string());
            continue;
        }
        if inst.kind != ItemKind::Plugin && taken.contains(&(inst.kind, inst.key.clone())) {
            skip(format!("another marketplace's {label} is already installed"));
            continue;
        }
        let repo = git::cache_path(input.data_root, &m.id);
        if !git::has_commit(&repo, &inst.commit) {
            skip(format!(
                "pinned commit {} is not in the local cache of \"{}\" — refresh the marketplace",
                &inst.commit[..8],
                m.name
            ));
            continue;
        }
        let (tree, files) = match install_files(&repo, inst) {
            Ok(v) => v,
            Err(e) => {
                skip(e);
                continue;
            }
        };

        let key = &inst.key;
        let mut item = json!({
            "kind": kind_str(inst.kind),
            "key": key,
            "marketplace": m.id,
            "commit": inst.commit,
        });
        match inst.kind {
            ItemKind::Agent | ItemKind::Command => {
                let dir = if inst.kind == ItemKind::Agent { "agents" } else { "commands" };
                let Some(f) = files.first() else {
                    skip("has no files".to_string());
                    continue;
                };
                let path = format!("{dir}/{key}.md");
                tar.file(&path, &f.data, false)?;
                item["file"] = json!(path);
            }
            ItemKind::Skill | ItemKind::Hook => {
                let dir = if inst.kind == ItemKind::Skill { format!("skills/{key}") } else { format!("hooks/{key}") };
                if inst.kind == ItemKind::Hook {
                    match rendered_hook_settings(&tree, key) {
                        Ok(settings) => item["settings"] = settings,
                        Err(e) => {
                            skip(e);
                            continue;
                        }
                    }
                }
                for f in &files {
                    tar.file(&format!("{dir}/{}", f.rel_path), &f.data, f.executable)?;
                }
                item["dir"] = json!(dir);
            }
            ItemKind::Plugin => {
                let mut entry = match plugin_catalog_entry(&tree, key) {
                    Ok(e) => e,
                    Err(e) => {
                        skip(e);
                        continue;
                    }
                };
                entry["source"] = json!(format!("./{key}"));
                let slug = marketplace_slug(&m.name, &m.id);
                for f in &files {
                    tar.file(&format!("plugins/{slug}/{key}/{}", f.rel_path), &f.data, f.executable)?;
                }
                let group = plugin_groups
                    .entry(slug.clone())
                    .or_insert_with(|| PluginGroup { entries: Vec::new(), keys: Vec::new() });
                group.entries.push(entry);
                group.keys.push(key.clone());
                item["slug"] = json!(slug);
            }
        }
        if inst.kind != ItemKind::Plugin {
            taken.insert((inst.kind, key.clone()));
        }
        items.push(item);
    }

    let mut plugin_marketplaces = Vec::new();
    for (slug, group) in plugin_groups {
        let catalog = json!({
            "name": format!("triple-c-{slug}"),
            "owner": { "name": "Triple-C" },
            "plugins": group.entries,
        });
        let bytes = serde_json::to_vec_pretty(&catalog).map_err(|e| e.to_string())?;
        tar.file(&format!("plugins/{slug}/.claude-plugin/marketplace.json"), &bytes, false)?;
        plugin_marketplaces.push(json!({ "slug": slug, "dir": format!("plugins/{slug}"), "plugins": group.keys }));
    }

    let manifest = json!({ "version": 1, "items": items, "plugin_marketplaces": plugin_marketplaces });
    let bytes = serde_json::to_vec_pretty(&manifest).map_err(|e| e.to_string())?;
    tar.file("manifest.json", &bytes, false)?;

    Ok(Payload { tar: tar.finish()?, manifest, skipped })
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cd app/src-tauri && cargo test --lib marketplace::payload`
Expected: 5 passed.

- [ ] **Step 5: Commit**

```bash
cd /workspace/triple-c
rustfmt --edition 2021 app/src-tauri/src/marketplace/payload.rs app/src-tauri/src/marketplace/mod.rs
git add app/src-tauri/src/marketplace/payload.rs app/src-tauri/src/marketplace/mod.rs
git commit -m "Marketplace: build the per-project payload tar

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

### Task 8: Container sync script

The script ships inside the app (see CONTRACT CHANGES 1): `container/` is **not** modified, so it works on every existing project without an image migration.

**Files:**
- Create: `app/src-tauri/src/marketplace/sync.sh`
- Create: `app/src-tauri/src/marketplace/sync.rs` (constants only in this task; Task 9 adds the rest)
- Create: `app/src-tauri/src/marketplace/sync_script_tests.rs`
- Modify: `app/src-tauri/src/marketplace/mod.rs` (add `pub mod sync;` and `#[cfg(test)] mod sync_script_tests;`)

**Interfaces:**
- Consumes: the payload layout and `manifest.json` shape from the contract (Task 7); `models::marketplace::SyncReport` (Task 2).
- Produces: `sync::SYNC_SCRIPT` (`include_str!("sync.sh")`), `sync::INCOMING_DIR`. Script contract: env `HOME` (required), `MARKETPLACE_INCOMING` (optional, default `$HOME/.claude/triple-c/marketplace/incoming`, must contain `payload.tar`), `MARKETPLACE_LOCK` (optional); stdout's last line is a `SyncReport` JSON object; exit status 0 unless `HOME` is unset.

State file `~/.claude/triple-c/marketplace/state.json` (script-private):

```json
{ "version": 1,
  "items": {
    "agent:code-reviewer": { "commit": "<sha>", "path": "/home/claude/.claude/agents/code-reviewer.md" },
    "skill:example-skill": { "commit": "<sha>", "path": "/home/claude/.claude/skills/example-skill" },
    "hook:notify-on-stop": { "commit": "<sha>", "path": "/home/claude/.claude/triple-c/hooks/notify-on-stop", "entries": { "Stop": [ … ] } },
    "plugin:example-plugin": { "commit": "<sha>", "slug": "team-tools-m1aaaaaa" } },
  "plugin_marketplaces": ["team-tools-m1aaaaaa"] }
```

- [ ] **Step 1: Create the constants module**

Create `app/src-tauri/src/marketplace/sync.rs`:

```rust
//! Pushes a project's marketplace payload into its container and runs the
//! sync script there (spec §4).

/// Where the payload and the script are uploaded. Owned by `claude`.
pub const INCOMING_DIR: &str = "/home/claude/.claude/triple-c/marketplace/incoming";

/// The sync script. Shipped with the app and uploaded on every sync, so a new
/// app version reaches existing containers without an image migration.
pub const SYNC_SCRIPT: &str = include_str!("sync.sh");
```

and create an empty `app/src-tauri/src/marketplace/sync.sh` (the tests need the file to exist to compile). Add to `mod.rs`: `pub mod sync;` and

```rust
#[cfg(test)]
mod sync_script_tests;
```

- [ ] **Step 2: Write the failing script tests**

Create `app/src-tauri/src/marketplace/sync_script_tests.rs`:

```rust
//! Runs the real `sync.sh` against a throwaway `$HOME`, with a stub `claude`
//! on `PATH` that records its arguments. Skipped when `jq` or `tar` is missing.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{json, Value};

use super::sync::SYNC_SCRIPT;
use crate::models::marketplace::SyncReport;

const C1: &str = "1111111111111111111111111111111111111111";
const C2: &str = "2222222222222222222222222222222222222222";
const SLUG: &str = "team-tools-m1aaaaaa";

fn have(tool: &str) -> bool {
    Command::new(tool).arg("--version").output().map(|o| o.status.success()).unwrap_or(false)
}

struct Env {
    _root: tempfile::TempDir,
    home: PathBuf,
    incoming: PathBuf,
    stub_dir: PathBuf,
    log: PathBuf,
    script: PathBuf,
    lock: PathBuf,
}

fn env() -> Option<Env> {
    if !have("jq") || !have("tar") {
        eprintln!("skipping: jq or tar is not installed");
        return None;
    }
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    let incoming = root.path().join("incoming");
    let stub_dir = root.path().join("bin");
    for d in [&home, &incoming, &stub_dir] {
        fs::create_dir_all(d).unwrap();
    }
    let log = root.path().join("claude.log");
    let stub = stub_dir.join("claude");
    fs::write(&stub, "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$CLAUDE_LOG\"\nexit 0\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&stub, fs::Permissions::from_mode(0o755)).unwrap();
    }
    let script = root.path().join("sync.sh");
    fs::write(&script, SYNC_SCRIPT).unwrap();
    let lock = root.path().join("lock");
    Some(Env { home, incoming, stub_dir, log, script, lock, _root: root })
}

/// Write `payload.tar` into the incoming dir: `files` plus `manifest.json`.
fn payload(env: &Env, files: &[(&str, &str, bool)], manifest: Value) {
    let mut b = tar::Builder::new(Vec::new());
    let mut add = |path: &str, data: &[u8], exec: bool| {
        let mut h = tar::Header::new_gnu();
        h.set_size(data.len() as u64);
        h.set_mode(if exec { 0o755 } else { 0o644 });
        h.set_entry_type(tar::EntryType::Regular);
        b.append_data(&mut h, path, data).unwrap();
    };
    for (path, text, exec) in files {
        add(path, text.as_bytes(), *exec);
    }
    add("manifest.json", manifest.to_string().as_bytes(), false);
    fs::write(env.incoming.join("payload.tar"), b.into_inner().unwrap()).unwrap();
}

fn run(env: &Env) -> SyncReport {
    let out = Command::new("sh")
        .arg(&env.script)
        .env_clear()
        .env("HOME", &env.home)
        .env("PATH", format!("{}:/usr/local/bin:/usr/bin:/bin", env.stub_dir.display()))
        .env("MARKETPLACE_INCOMING", &env.incoming)
        .env("MARKETPLACE_LOCK", &env.lock)
        .env("CLAUDE_LOG", &env.log)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "script failed: {}", String::from_utf8_lossy(&out.stderr));
    let last = stdout.lines().rev().find(|l| !l.trim().is_empty()).expect("a report line");
    serde_json::from_str(last).unwrap_or_else(|e| panic!("bad report {last:?}: {e}"))
}

fn claude_log(env: &Env) -> Vec<String> {
    fs::read_to_string(&env.log).unwrap_or_default().lines().map(str::to_string).collect()
}

fn read_json(p: &Path) -> Value {
    serde_json::from_str(&fs::read_to_string(p).unwrap()).unwrap()
}

fn hook_settings() -> Value {
    json!({ "Stop": [{ "hooks": [{ "type": "command",
        "command": "/home/claude/.claude/triple-c/hooks/notify-on-stop/notify.sh" }] }] })
}

fn all_kinds(commit: &str) -> (Vec<(&'static str, &'static str, bool)>, Value) {
    (
        vec![
            ("agents/code-reviewer.md", "agent body\n", false),
            ("skills/example-skill/SKILL.md", "skill body\n", false),
            ("commands/example-command.md", "command body\n", false),
            ("hooks/notify-on-stop/hook.json", "{}", false),
            ("hooks/notify-on-stop/notify.sh", "#!/bin/sh\necho hi\n", true),
        ],
        json!({ "version": 1, "plugin_marketplaces": [], "items": [
            { "kind": "agent", "key": "code-reviewer", "marketplace": "m1", "commit": commit, "file": "agents/code-reviewer.md" },
            { "kind": "skill", "key": "example-skill", "marketplace": "m1", "commit": commit, "dir": "skills/example-skill" },
            { "kind": "command", "key": "example-command", "marketplace": "m1", "commit": commit, "file": "commands/example-command.md" },
            { "kind": "hook", "key": "notify-on-stop", "marketplace": "m1", "commit": commit, "dir": "hooks/notify-on-stop", "settings": hook_settings() }
        ]}),
    )
}

fn empty_manifest() -> Value {
    json!({ "version": 1, "items": [], "plugin_marketplaces": [] })
}

fn sorted(mut v: Vec<String>) -> Vec<String> {
    v.sort();
    v
}

#[test]
fn sync_installs_all_kinds() {
    let Some(env) = env() else { return };
    let (files, manifest) = all_kinds(C1);
    payload(&env, &files, manifest);

    let r = run(&env);

    assert_eq!(r.errors, Vec::<String>::new());
    assert_eq!(
        sorted(r.installed),
        vec!["agent:code-reviewer", "command:example-command", "hook:notify-on-stop", "skill:example-skill"]
    );
    let claude = env.home.join(".claude");
    assert_eq!(fs::read_to_string(claude.join("agents/code-reviewer.md")).unwrap(), "agent body\n");
    assert!(claude.join("skills/example-skill/SKILL.md").is_file());
    assert!(claude.join("commands/example-command.md").is_file());
    let script = claude.join("triple-c/hooks/notify-on-stop/notify.sh");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_ne!(fs::metadata(&script).unwrap().permissions().mode() & 0o111, 0, "hook script must stay executable");
    }
    assert_eq!(read_json(&claude.join("settings.json"))["hooks"], hook_settings());
    assert!(!env.incoming.join("payload.tar").exists(), "payload is consumed");

    // A second identical run is a no-op in the report.
    payload(&env, &all_kinds(C1).0, all_kinds(C1).1);
    let again = run(&env);
    assert!(again.installed.is_empty() && again.updated.is_empty() && again.removed.is_empty(), "{again:?}");
    assert_eq!(read_json(&claude.join("settings.json"))["hooks"]["Stop"].as_array().unwrap().len(), 1);
}

#[test]
fn sync_updates_report_changed_commits() {
    let Some(env) = env() else { return };
    let (files, manifest) = all_kinds(C1);
    payload(&env, &files, manifest);
    run(&env);
    let (files, manifest) = all_kinds(C2);
    payload(&env, &files, manifest);

    let r = run(&env);

    assert_eq!(r.updated.len(), 4, "{r:?}");
    assert!(r.installed.is_empty());
}

#[test]
fn sync_removes_deselected() {
    let Some(env) = env() else { return };
    let (files, manifest) = all_kinds(C1);
    payload(&env, &files, manifest);
    run(&env);
    payload(&env, &[], empty_manifest());

    let r = run(&env);

    assert_eq!(
        sorted(r.removed),
        vec!["agent:code-reviewer", "command:example-command", "hook:notify-on-stop", "skill:example-skill"]
    );
    let claude = env.home.join(".claude");
    assert!(!claude.join("agents/code-reviewer.md").exists());
    assert!(!claude.join("skills/example-skill").exists());
    assert!(!claude.join("commands/example-command.md").exists());
    assert!(!claude.join("triple-c/hooks/notify-on-stop").exists());
    assert_eq!(read_json(&claude.join("settings.json")).get("hooks"), None);
}

#[test]
fn sync_skips_user_owned_agent() {
    let Some(env) = env() else { return };
    let mine = env.home.join(".claude/agents/code-reviewer.md");
    fs::create_dir_all(mine.parent().unwrap()).unwrap();
    fs::write(&mine, "mine\n").unwrap();
    let (files, manifest) = all_kinds(C1);
    payload(&env, &files, manifest);

    let r = run(&env);

    assert_eq!(r.skipped.len(), 1, "{r:?}");
    assert_eq!(r.skipped[0].item, "agent:code-reviewer");
    assert!(r.skipped[0].reason.contains("was not installed by Triple-C"), "{}", r.skipped[0].reason);
    assert_eq!(fs::read_to_string(&mine).unwrap(), "mine\n");

    // Deselecting everything must not delete the user's own file either.
    payload(&env, &[], empty_manifest());
    let r = run(&env);
    assert!(!r.removed.contains(&"agent:code-reviewer".to_string()));
    assert_eq!(fs::read_to_string(&mine).unwrap(), "mine\n");
}

#[test]
fn sync_preserves_user_hooks() {
    let Some(env) = env() else { return };
    let settings_path = env.home.join(".claude/settings.json");
    fs::create_dir_all(settings_path.parent().unwrap()).unwrap();
    let original = json!({
        "model": "opus",
        "hooks": {
            "Stop": [{ "hooks": [{ "type": "command", "command": "echo mine" }] }],
            "PreToolUse": [{ "matcher": "Bash", "hooks": [{ "type": "command", "command": "echo pre" }] }]
        }
    });
    fs::write(&settings_path, serde_json::to_string_pretty(&original).unwrap()).unwrap();

    let (files, manifest) = all_kinds(C1);
    payload(&env, &files, manifest);
    run(&env);
    let merged = read_json(&settings_path);
    assert_eq!(merged["model"], "opus");
    assert_eq!(merged["hooks"]["PreToolUse"], original["hooks"]["PreToolUse"]);
    assert_eq!(merged["hooks"]["Stop"][0], original["hooks"]["Stop"][0], "user hook stays first");
    assert_eq!(merged["hooks"]["Stop"][1], hook_settings()["Stop"][0]);

    // An update with a changed hook entry replaces only ours.
    let (files, mut manifest) = all_kinds(C2);
    manifest["items"][3]["settings"]["Stop"][0]["hooks"][0]["timeout"] = json!(5);
    payload(&env, &files, manifest);
    run(&env);
    let updated = read_json(&settings_path);
    assert_eq!(updated["hooks"]["Stop"].as_array().unwrap().len(), 2);
    assert_eq!(updated["hooks"]["Stop"][0], original["hooks"]["Stop"][0]);
    assert_eq!(updated["hooks"]["Stop"][1]["hooks"][0]["timeout"], 5);

    // Uninstalling everything restores the user's settings exactly.
    payload(&env, &[], empty_manifest());
    run(&env);
    assert_eq!(read_json(&settings_path), original);
}

#[test]
fn sync_plugin_calls() {
    let Some(env) = env() else { return };
    let files = [
        ("plugins/team-tools-m1aaaaaa/.claude-plugin/marketplace.json", r#"{"name":"triple-c-team-tools-m1aaaaaa","owner":{"name":"Triple-C"},"plugins":[{"name":"example-plugin","source":"./example-plugin"}]}"#, false),
        ("plugins/team-tools-m1aaaaaa/example-plugin/.claude-plugin/plugin.json", r#"{"name":"example-plugin"}"#, false),
    ];
    let manifest = |commit: &str| {
        json!({ "version": 1,
            "items": [{ "kind": "plugin", "key": "example-plugin", "marketplace": "m1", "commit": commit, "slug": SLUG }],
            "plugin_marketplaces": [{ "slug": SLUG, "dir": format!("plugins/{SLUG}"), "plugins": ["example-plugin"] }] })
    };
    let tree = env.home.join(".claude/triple-c/plugins").join(SLUG);

    payload(&env, &files, manifest(C1));
    let r = run(&env);
    assert_eq!(r.installed, vec!["plugin:example-plugin"]);
    assert_eq!(
        claude_log(&env),
        vec![
            format!("plugin marketplace add {}", tree.display()),
            format!("plugin install example-plugin@triple-c-{SLUG}"),
        ]
    );
    assert!(tree.join(".claude-plugin/marketplace.json").is_file());

    // Same commit again: catalog refreshed, nothing reinstalled.
    fs::remove_file(&env.log).unwrap();
    payload(&env, &files, manifest(C1));
    run(&env);
    assert_eq!(claude_log(&env), vec![format!("plugin marketplace update triple-c-{SLUG}")]);

    // New commit: uninstall + install.
    fs::remove_file(&env.log).unwrap();
    payload(&env, &files, manifest(C2));
    let r = run(&env);
    assert_eq!(r.updated, vec!["plugin:example-plugin"]);
    assert_eq!(
        claude_log(&env),
        vec![
            format!("plugin marketplace update triple-c-{SLUG}"),
            format!("plugin uninstall example-plugin@triple-c-{SLUG}"),
            format!("plugin install example-plugin@triple-c-{SLUG}"),
        ]
    );

    // Deselected: uninstall, drop the registration and the tree.
    fs::remove_file(&env.log).unwrap();
    payload(&env, &[], empty_manifest());
    let r = run(&env);
    assert_eq!(r.removed, vec!["plugin:example-plugin"]);
    assert_eq!(
        claude_log(&env),
        vec![
            format!("plugin uninstall example-plugin@triple-c-{SLUG}"),
            format!("plugin marketplace remove triple-c-{SLUG}"),
        ]
    );
    assert!(!tree.exists());
}

#[test]
fn sync_rejects_bad_keys() {
    let Some(env) = env() else { return };
    payload(
        &env,
        &[("agents/x.md", "x", false)],
        json!({ "version": 1, "plugin_marketplaces": [], "items": [
            { "kind": "agent", "key": "../../evil", "marketplace": "m1", "commit": C1 },
            { "kind": "agent", "key": "-rf", "marketplace": "m1", "commit": C1 },
            { "kind": "agent", "key": "ok", "marketplace": "m1", "commit": "not-a-sha" }
        ]}),
    );
    let r = run(&env);
    assert_eq!(r.skipped.len(), 3, "{r:?}");
    assert!(r.installed.is_empty());
    assert!(!env.home.join("evil.md").exists());
}

#[test]
fn a_missing_payload_is_reported_not_fatal() {
    let Some(env) = env() else { return };
    let r = run(&env);
    assert_eq!(r.errors, vec!["no payload was uploaded"]);
}
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `cd app/src-tauri && cargo test --lib marketplace::sync_script_tests`
Expected: FAIL — every test panics with "a report line" / "bad report" because `sync.sh` is empty (on a machine without `jq` the tests print "skipping" and pass; CI's ubuntu runner has `jq`).

- [ ] **Step 4: Write the script**

Replace `app/src-tauri/src/marketplace/sync.sh` with:

```sh
#!/bin/sh
# Triple-C marketplace sync: applies the payload the app uploaded.
#
# A constant script, shipped inside the app and uploaded next to the payload on
# every sync. Nothing is ever interpolated into it: its only inputs are the
# files under $MARKETPLACE_INCOMING (written by the host) and $HOME. Item keys
# and slugs are re-validated here although the host validated them, and every
# destination path is derived from them rather than taken from the manifest.
#
# Progress and tool output go to stderr. stdout carries exactly one line: the
# JSON report. Exit status is 0 unless HOME is unset; per-item failures are
# reported, never fatal.
set -u

if [ -z "${HOME:-}" ]; then
  echo "triple-c-marketplace-sync: HOME is not set" >&2
  exit 2
fi
PATH="$HOME/.claude/bin:$HOME/.local/bin:$PATH"
export PATH

CLAUDE_DIR="$HOME/.claude"
BASE="$CLAUDE_DIR/triple-c"
INCOMING="${MARKETPLACE_INCOMING:-$BASE/marketplace/incoming}"
LOCK="${MARKETPLACE_LOCK:-/tmp/.triple-c-claude-update.lock}"
STATE="$BASE/marketplace/state.json"
WORK="$BASE/marketplace/work"
SETTINGS="$CLAUDE_DIR/settings.json"
TAB=$(printf '\t')

if ! command -v jq >/dev/null 2>&1; then
  printf '%s\n' '{"errors":["jq is not installed in this container, so marketplace items were not applied"]}'
  exit 0
fi

R=$(mktemp -d) || exit 2
trap 'rm -rf "$R"' EXIT
for f in installed updated removed skipped errors newstate new_slugs final_slugs; do
  : >"$R/$f"
done

report() { printf '%s\n' "$2" >>"$R/$1"; }
skip() { printf '%s\t%s\n' "$1" "$2" >>"$R/skipped"; }
fail() { printf '%s\n' "$1" >>"$R/errors"; }
record() { printf '%s\t%s\n' "$1" "$2" >>"$R/newstate"; }

emit_report() {
  jq -cn \
    --rawfile i "$R/installed" --rawfile u "$R/updated" --rawfile d "$R/removed" \
    --rawfile s "$R/skipped" --rawfile e "$R/errors" '
    def lines: split("\n") | map(select(length > 0));
    { installed: ($i | lines), updated: ($u | lines), removed: ($d | lines),
      skipped: ($s | lines | map(split("\t") | { item: .[0], reason: (.[1:] | join("\t")) })),
      errors: ($e | lines) }'
}

valid_key() {
  case "$1" in
    '' | [!A-Za-z0-9]* | *[!A-Za-z0-9._-]*) return 1 ;;
  esac
  [ "${#1}" -le 64 ]
}

valid_slug() {
  case "$1" in
    '' | -* | *[!a-z0-9-]*) return 1 ;;
  esac
  [ "${#1}" -le 64 ]
}

valid_commit() {
  case "$1" in
    '' | *[!0-9a-f]*) return 1 ;;
  esac
  [ "${#1}" -eq 40 ]
}

# Run `claude` serialised with the entrypoint's and every session's
# `claude update`, which rewrite ~/.claude/bin under the same lock.
claude_cmd() {
  if command -v flock >/dev/null 2>&1; then
    flock -w 120 "$LOCK" claude "$@" </dev/null >&2
  else
    claude "$@" </dev/null >&2
  fi
}

owned() { jq -e --arg id "$1" '.items | has($id)' "$STATE" >/dev/null 2>&1; }
prev_commit() { jq -r --arg id "$1" '.items[$id].commit // ""' "$STATE"; }
in_manifest() {
  jq -e --arg id "$1" 'any(.items[]; (.kind + ":" + .key) == $id)' "$MANIFEST" >/dev/null 2>&1
}
carry_forward() { record "$1" "$(jq -c --arg id "$1" '.items[$id]' "$STATE")"; }

outcome() {
  p=$(prev_commit "$1")
  if [ -z "$p" ]; then
    report installed "$1"
  elif [ "$p" != "$2" ]; then
    report updated "$1"
  fi
}

# ── Unpack ───────────────────────────────────────────────────────────────────
if [ ! -f "$INCOMING/payload.tar" ]; then
  fail "no payload was uploaded"
  emit_report
  exit 0
fi
mkdir -p "$BASE/marketplace" "$BASE/hooks" "$BASE/plugins"
rm -rf "$WORK"
mkdir -p "$WORK"
if ! tar -xf "$INCOMING/payload.tar" -C "$WORK" >&2; then
  rm -f "$INCOMING/payload.tar"
  fail "the payload could not be unpacked"
  emit_report
  exit 0
fi
rm -f "$INCOMING/payload.tar"
MANIFEST="$WORK/manifest.json"
if ! jq -e '.version == 1' "$MANIFEST" >/dev/null 2>&1; then
  fail "the payload manifest is missing or has an unsupported version"
  emit_report
  exit 0
fi
if ! jq -e '(.items | type) == "object"' "$STATE" >/dev/null 2>&1; then
  printf '%s\n' '{"version":1,"items":{},"plugin_marketplaces":[]}' >"$STATE"
fi

# ── Agents, skills, commands, hooks ──────────────────────────────────────────
jq -r '.items[] | select(.kind != "plugin") | [.kind, .key, .commit] | @tsv' "$MANIFEST" >"$R/items.tsv"
while IFS="$TAB" read -r kind key commit; do
  id="$kind:$key"
  if ! valid_key "$key"; then skip "$id" "invalid item name"; continue; fi
  if ! valid_commit "$commit"; then skip "$id" "invalid commit"; continue; fi
  case "$kind" in
    agent | command)
      dir="$CLAUDE_DIR/${kind}s"
      src="$WORK/${kind}s/$key.md"
      dest="$dir/$key.md"
      if [ ! -f "$src" ]; then fail "$id: missing from the payload"; continue; fi
      if [ -e "$dest" ] && ! owned "$id"; then
        skip "$id" "~/.claude/${kind}s/$key.md already exists and was not installed by Triple-C"
        continue
      fi
      if ! { mkdir -p "$dir" && cp "$src" "$dest.tmp.$$" && mv -f "$dest.tmp.$$" "$dest"; }; then
        rm -f "$dest.tmp.$$"
        fail "$id: could not write $dest"
        continue
      fi
      outcome "$id" "$commit"
      record "$id" "$(jq -cn --arg c "$commit" --arg p "$dest" '{commit: $c, path: $p}')"
      ;;
    skill)
      dir="$CLAUDE_DIR/skills"
      src="$WORK/skills/$key"
      dest="$dir/$key"
      if [ ! -d "$src" ]; then fail "$id: missing from the payload"; continue; fi
      if [ -e "$dest" ] && ! owned "$id"; then
        skip "$id" "~/.claude/skills/$key already exists and was not installed by Triple-C"
        continue
      fi
      if ! { mkdir -p "$dir" && rm -rf "$dest" && cp -R "$src" "$dest"; }; then
        fail "$id: could not write $dest"
        continue
      fi
      outcome "$id" "$commit"
      record "$id" "$(jq -cn --arg c "$commit" --arg p "$dest" '{commit: $c, path: $p}')"
      ;;
    hook)
      src="$WORK/hooks/$key"
      dest="$BASE/hooks/$key"
      entries=$(jq -c --arg k "$key" \
        'first(.items[] | select(.kind == "hook" and .key == $k) | .settings) // {}' "$MANIFEST")
      if ! printf '%s' "$entries" | jq -e 'type == "object" and all(.[]; type == "array")' >/dev/null 2>&1; then
        skip "$id" "its hook settings are not an object of arrays"
        continue
      fi
      if [ ! -d "$src" ]; then fail "$id: missing from the payload"; continue; fi
      if ! { rm -rf "$dest" && cp -R "$src" "$dest"; }; then
        fail "$id: could not write $dest"
        continue
      fi
      outcome "$id" "$commit"
      record "$id" "$(jq -cn --arg c "$commit" --arg p "$dest" --argjson e "$entries" \
        '{commit: $c, path: $p, entries: $e}')"
      ;;
    *)
      skip "$id" "unknown item kind"
      ;;
  esac
done <"$R/items.tsv"

# ── Removals (non-plugin) ────────────────────────────────────────────────────
cut -f1 "$R/newstate" >"$R/new_ids"
jq -r '.items | keys[]' "$STATE" >"$R/old_ids"
while read -r id; do
  case "$id" in plugin:*) continue ;; esac
  if grep -qxF "$id" "$R/new_ids"; then continue; fi
  # Still selected but failed this run: keep the old files and record.
  if in_manifest "$id"; then carry_forward "$id"; continue; fi
  path=$(jq -r --arg id "$id" '.items[$id].path // ""' "$STATE")
  case "$path" in
    "$CLAUDE_DIR"/*) rm -rf "$path" && report removed "$id" || fail "$id: could not remove $path" ;;
    *) fail "$id: refusing to remove unexpected path $path" ;;
  esac
done <"$R/old_ids"

# ── Hook entries in settings.json ────────────────────────────────────────────
MERGE_ENTRIES='[.[] | .entries? // empty]
  | reduce .[] as $e ({}; reduce ($e | to_entries[]) as $x (.; .[$x.key] += $x.value))'
OLD_HOOKS=$(jq -c "[.items[]] | $MERGE_ENTRIES" "$STATE")
NEW_HOOKS=$(cut -f2- "$R/newstate" | jq -cs "$MERGE_ENTRIES")
HOOKS_FAILED=0
if [ "$OLD_HOOKS" != "{}" ] || [ "$NEW_HOOKS" != "{}" ]; then
  if [ -f "$SETTINGS" ]; then
    current="$SETTINGS"
  else
    printf '{}\n' >"$R/empty.json"
    current="$R/empty.json"
  fi
  if jq --argjson old "$OLD_HOOKS" --argjson new "$NEW_HOOKS" '
      def remove_first($x):
        (to_entries | map(select(.value == $x)) | first(.[].key) // null) as $i
        | if $i == null then . else del(.[$i]) end;
      reduce ($old | to_entries[]) as $ev (.;
        if (.hooks[$ev.key] | type) == "array"
        then reduce $ev.value[] as $g (.; .hooks[$ev.key] |= remove_first($g))
        else . end)
      | reduce ($new | to_entries[]) as $ev (.;
          .hooks[$ev.key] = ((.hooks[$ev.key] // []) + $ev.value))
      | if (.hooks | type) == "object" then .hooks |= with_entries(select(.value != [])) else . end
      | if .hooks == {} then del(.hooks) else . end
    ' "$current" >"$SETTINGS.tmp.$$"; then
    mv -f "$SETTINGS.tmp.$$" "$SETTINGS"
  else
    rm -f "$SETTINGS.tmp.$$"
    HOOKS_FAILED=1
    fail "~/.claude/settings.json is not valid JSON, so hook changes were not applied"
  fi
fi

# ── Plugins ──────────────────────────────────────────────────────────────────
jq -r '.plugin_marketplaces[]?' "$STATE" >"$R/old_slugs"
jq -r '.plugin_marketplaces[] | .slug' "$MANIFEST" >"$R/new_slugs"
while read -r slug; do
  if ! valid_slug "$slug"; then fail "invalid plugin marketplace name"; continue; fi
  mname="triple-c-$slug"
  dest="$BASE/plugins/$slug"
  if ! { rm -rf "$dest" && cp -R "$WORK/plugins/$slug" "$dest"; }; then
    fail "$mname: could not write $dest"
    continue
  fi
  if grep -qxF "$slug" "$R/old_slugs"; then
    claude_cmd plugin marketplace update "$mname" || fail "$mname: marketplace update failed"
  elif ! claude_cmd plugin marketplace add "$dest"; then
    claude_cmd plugin marketplace update "$mname" || { fail "$mname: could not be registered"; continue; }
  fi
  printf '%s\n' "$slug" >>"$R/final_slugs"
  jq -r --arg s "$slug" '.items[] | select(.kind == "plugin" and .slug == $s) | [.key, .commit] | @tsv' \
    "$MANIFEST" >"$R/plugins.tsv"
  while IFS="$TAB" read -r key commit; do
    id="plugin:$key"
    if ! valid_key "$key"; then skip "$id" "invalid item name"; continue; fi
    if ! valid_commit "$commit"; then skip "$id" "invalid commit"; continue; fi
    p=$(prev_commit "$id")
    if [ -z "$p" ]; then
      claude_cmd plugin install "$key@$mname" || { fail "$id: install failed"; continue; }
      report installed "$id"
    elif [ "$p" != "$commit" ]; then
      claude_cmd plugin uninstall "$key@$mname"
      claude_cmd plugin install "$key@$mname" || { fail "$id: reinstall failed"; continue; }
      report updated "$id"
    fi
    record "$id" "$(jq -cn --arg c "$commit" --arg s "$slug" '{commit: $c, slug: $s}')"
  done <"$R/plugins.tsv"
done <"$R/new_slugs"

# Plugins no longer selected.
while read -r id; do
  case "$id" in plugin:*) ;; *) continue ;; esac
  if grep -qxF "$id" "$R/new_ids" || cut -f1 "$R/newstate" | grep -qxF "$id"; then continue; fi
  if in_manifest "$id"; then carry_forward "$id"; continue; fi
  key=${id#plugin:}
  slug=$(jq -r --arg id "$id" '.items[$id].slug // ""' "$STATE")
  if valid_key "$key" && valid_slug "$slug" && claude_cmd plugin uninstall "$key@triple-c-$slug"; then
    report removed "$id"
  else
    fail "$id: uninstall failed"
    carry_forward "$id"
  fi
done <"$R/old_ids"

# Plugin marketplaces with nothing left in them.
cut -f2- "$R/newstate" | jq -r 'select(has("slug")) | .slug' >>"$R/final_slugs"
while read -r slug; do
  if grep -qxF "$slug" "$R/final_slugs"; then continue; fi
  valid_slug "$slug" || continue
  claude_cmd plugin marketplace remove "triple-c-$slug" || fail "triple-c-$slug: could not be removed"
  rm -rf "$BASE/plugins/$slug"
done <"$R/old_slugs"

# ── State ────────────────────────────────────────────────────────────────────
jq -Rn '[inputs | split("\t") | { key: .[0], value: (.[1:] | join("\t") | fromjson) }] | from_entries' \
  <"$R/newstate" >"$R/items.json"
if [ "$HOOKS_FAILED" = 1 ]; then
  # settings.json still holds the old entries, so the old records stay true.
  jq -s '.[0] as $new | .[1].items as $old
    | ($new | with_entries(select(.key | startswith("hook:") | not)))
      + ($old | with_entries(select(.key | startswith("hook:"))))' \
    "$R/items.json" "$STATE" >"$R/items2.json" && mv -f "$R/items2.json" "$R/items.json"
fi
if jq -n --slurpfile it "$R/items.json" --rawfile sl "$R/final_slugs" \
  '{ version: 1, items: $it[0], plugin_marketplaces: ($sl | split("\n") | map(select(length > 0)) | unique) }' \
  >"$STATE.tmp.$$"; then
  mv -f "$STATE.tmp.$$" "$STATE"
else
  rm -f "$STATE.tmp.$$"
  fail "the marketplace state could not be saved"
fi

rm -rf "$WORK"
emit_report
```

- [ ] **Step 5: Run the script tests**

Run: `cd app/src-tauri && cargo test --lib marketplace::sync_script_tests`
Expected: 8 passed.

Also check the script is POSIX: `sh -n app/src-tauri/src/marketplace/sync.sh && (command -v dash >/dev/null && dash -n app/src-tauri/src/marketplace/sync.sh || true)`
Expected: no output.

- [ ] **Step 6: Commit**

```bash
cd /workspace/triple-c
git add app/src-tauri/src/marketplace/sync.sh app/src-tauri/src/marketplace/sync.rs app/src-tauri/src/marketplace/sync_script_tests.rs app/src-tauri/src/marketplace/mod.rs
git commit -m "Marketplace: container sync script and its tests

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

### Task 9: Sync orchestration and the container-start hook

**Files:**
- Modify: `app/src-tauri/src/marketplace/sync.rs`
- Modify: `app/src-tauri/src/marketplace/mod.rs` (`sync_project`, `should_sync`, `spawn_project_sync`, `SYNC_FINISHED_EVENT`)
- Modify: `app/src-tauri/src/commands/project_commands.rs` (`start_project_container_locked`, after `sync_bedrock_credentials`)

**Interfaces:**
- Consumes: `docker::exec::{exec_oneshot_as, exec_oneshot_streams_as, upload_bytes_to_container}` (existing: `exec_oneshot_as(container_id, user, cmd, env) -> Result<(String, i64), String>`, `exec_oneshot_streams_as(...) -> Result<(String, String, i64), String>`, `upload_bytes_to_container(container_id, dest_dir, file_name, data, mode) -> Result<String, String>`; uploaded files are root-owned and `dest_dir` must exist); `payload::{build_payload, PayloadInput, Payload}` (Task 7); `models::marketplace::effective_installs` (Task 2); `AppState.marketplace` (Task 6).
- Produces: `sync::{sync_container, parse_report, report_from_result}`; `marketplace::{sync_project, should_sync, spawn_project_sync, SYNC_FINISHED_EVENT}`. Event `marketplace-sync-finished` with payload `{ "project_id": String, "report": SyncReport }`.

- [ ] **Step 1: Write the failing tests**

Append to `app/src-tauri/src/marketplace/sync.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_report_is_the_last_non_empty_stdout_line() {
        let out = "noise\n{\"installed\":[\"agent:a\"],\"errors\":[]}\n\n";
        let r = parse_report(out).unwrap();
        assert_eq!(r.installed, vec!["agent:a"]);
        assert!(r.skipped.is_empty());
    }

    #[test]
    fn missing_or_garbled_reports_are_errors() {
        assert!(parse_report("").unwrap_err().contains("no report"));
        assert!(parse_report("not json\n").unwrap_err().contains("could not be read"));
    }

    #[test]
    fn sync_failure_does_not_fail_start() {
        // A failed sync becomes a report with the error in it — never an Err
        // that could propagate into container start.
        let r = report_from_result(Err("container went away".into()));
        assert_eq!(r.errors, vec!["container went away"]);
        assert!(!r.finished_at.is_empty());

        let ok = report_from_result(Ok(SyncReport { installed: vec!["hook:h".into()], ..Default::default() }));
        assert_eq!(ok.installed, vec!["hook:h"]);
        assert!(chrono::DateTime::parse_from_rfc3339(&ok.finished_at).is_ok());
    }

    #[test]
    fn the_embedded_script_is_the_sync_script() {
        assert!(SYNC_SCRIPT.starts_with("#!/bin/sh"));
        assert!(SYNC_SCRIPT.contains("MARKETPLACE_INCOMING"));
    }
}
```

Append to the `tests` module in `app/src-tauri/src/marketplace/mod.rs`:

```rust
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
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cd app/src-tauri && cargo test --lib marketplace::`
Expected: FAIL to compile — `parse_report`, `report_from_result`, `should_sync` not found.

- [ ] **Step 3: Implement `sync.rs`**

Replace the body of `app/src-tauri/src/marketplace/sync.rs` above the tests with:

```rust
//! Pushes a project's marketplace payload into its container and runs the
//! sync script there (spec §4).

use std::time::Duration;

use super::payload::Payload;
use crate::docker::exec::{exec_oneshot_as, exec_oneshot_streams_as, upload_bytes_to_container};
use crate::models::marketplace::SyncReport;

/// Where the payload and the script are uploaded. Owned by `claude`.
pub const INCOMING_DIR: &str = "/home/claude/.claude/triple-c/marketplace/incoming";

/// The sync script. Shipped with the app and uploaded on every sync, so a new
/// app version reaches existing containers without an image migration.
pub const SYNC_SCRIPT: &str = include_str!("sync.sh");

/// True once the entrypoint has finished: its last step execs this exact
/// command line. Before that it may still be merging `settings.json` or running
/// `claude update`, both of which the sync would race.
const READY_PROBE: &str = "pgrep -x -f 'su -s /bin/bash claude -c exec sleep infinity' >/dev/null";
const READY_TIMEOUT: Duration = Duration::from_secs(180);
const READY_POLL: Duration = Duration::from_secs(2);

/// Run as root: `~/.claude` is a volume and `triple-c/` may not exist yet, and
/// the uploads below are root-owned files in a directory `claude` must own so
/// the script can delete them.
const PREPARE_SCRIPT: &str = r#"set -e
d=/home/claude/.claude/triple-c/marketplace/incoming
mkdir -p "$d"
chown -R claude:claude /home/claude/.claude/triple-c
rm -f "$d/payload.tar" "$d/sync.sh""#;

fn sh(script: &str) -> Vec<String> {
    vec!["sh".to_string(), "-c".to_string(), script.to_string()]
}

async fn wait_until_ready(container_id: &str) -> Result<(), String> {
    let deadline = tokio::time::Instant::now() + READY_TIMEOUT;
    loop {
        let (_, code) = exec_oneshot_as(container_id, "root", sh(READY_PROBE), vec![]).await?;
        if code == 0 {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(format!(
                "The container did not finish starting within {} seconds, so marketplace items \
                 were not applied. They are applied on the next start, or with Apply now.",
                READY_TIMEOUT.as_secs()
            ));
        }
        tokio::time::sleep(READY_POLL).await;
    }
}

fn tail(text: &str, max: usize) -> &str {
    let text = text.trim();
    if text.len() <= max {
        return text;
    }
    let mut start = text.len() - max;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    &text[start..]
}

/// Wait for readiness, upload the payload and the script, run the script as
/// `claude`, and return its report.
pub async fn sync_container(container_id: &str, payload: &Payload) -> Result<SyncReport, String> {
    wait_until_ready(container_id).await?;

    let (out, code) = exec_oneshot_as(container_id, "root", sh(PREPARE_SCRIPT), vec![]).await?;
    if code != 0 {
        return Err(format!("Could not prepare the container for the marketplace sync: {}", tail(&out, 500)));
    }
    upload_bytes_to_container(container_id, INCOMING_DIR, "payload.tar", &payload.tar, 0o644).await?;
    upload_bytes_to_container(container_id, INCOMING_DIR, "sync.sh", SYNC_SCRIPT.as_bytes(), 0o755).await?;

    let (stdout, stderr, code) = exec_oneshot_streams_as(
        container_id,
        "claude",
        vec!["sh".to_string(), format!("{INCOMING_DIR}/sync.sh")],
        vec!["HOME=/home/claude".to_string()],
    )
    .await?;
    parse_report(&stdout).map_err(|e| {
        format!("The marketplace sync script failed (exit {code}): {e}. {}", tail(&stderr, 500))
    })
}

/// The script's report is the last non-empty line of stdout.
pub fn parse_report(stdout: &str) -> Result<SyncReport, String> {
    let line = stdout
        .lines()
        .rev()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .ok_or_else(|| "the sync script printed no report".to_string())?;
    serde_json::from_str(line).map_err(|e| format!("the sync script's report could not be read: {e}"))
}

/// A sync never fails its caller: an error becomes a report that says so.
pub fn report_from_result(r: Result<SyncReport, String>) -> SyncReport {
    let mut report = match r {
        Ok(report) => report,
        Err(e) => SyncReport { errors: vec![e], ..Default::default() },
    };
    report.finished_at = chrono::Utc::now().to_rfc3339();
    report
}
```

- [ ] **Step 4: Implement the project-level sync in `mod.rs`**

Add `pub mod sync;` if not present, extend the imports with `use std::sync::Arc;`, `use tauri::Emitter;` and `use crate::models::marketplace::effective_installs;`, then add:

```rust
/// Emitted when a background sync finishes. Payload `{ project_id, report }`.
pub const SYNC_FINISHED_EVENT: &str = "marketplace-sync-finished";

fn project_installs(settings: &AppSettings, project: &Project) -> Vec<MarketplaceInstall> {
    effective_installs(
        &settings.global_marketplace_installs,
        &project.marketplace_disabled,
        &project.marketplace_installs,
    )
}

/// Build the project's payload and sync it into its running container. The
/// report is stored (and persisted) whatever happens.
pub async fn sync_project(
    mgr: &MarketplaceManager,
    settings: &AppSettings,
    project: &Project,
    container_id: &str,
) -> SyncReport {
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

    let result = match built {
        Ok(p) => sync::sync_container(container_id, &p).await.map(|mut report| {
            let mut skipped = p.skipped.clone();
            skipped.extend(report.skipped);
            report.skipped = skipped;
            report
        }),
        Err(e) => Err(e),
    };
    let report = sync::report_from_result(result);
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
            log::warn!("Marketplace sync for project {} reported errors: {:?}", project.id, report.errors);
        }
        let _ = app.emit(
            SYNC_FINISHED_EVENT,
            serde_json::json!({ "project_id": project.id, "report": report }),
        );
    });
}
```

- [ ] **Step 5: Run the tests**

Run: `cd app/src-tauri && cargo test --lib marketplace::`
Expected: all pass.

- [ ] **Step 6: Hook the sync into container start**

In `app/src-tauri/src/commands/project_commands.rs`, inside `start_project_container_locked`, directly after the existing block

```rust
        if let Err(e) = docker::sync_bedrock_credentials(&container_id, &project).await {
            log::warn!("Failed to sync AWS credentials for project {}: {}", project.id, e);
        }
```

insert:

```rust
        // Marketplace items sync in the background — see `spawn_project_sync`
        // for why the start never waits on it or fails because of it.
        crate::marketplace::spawn_project_sync(
            app_handle.clone(),
            state.marketplace.clone(),
            state.settings_store.get(),
            project.clone(),
            container_id.clone(),
        );
```

Run: `cd app/src-tauri && cargo check --lib && cargo test --lib project_commands`
Expected: compiles; existing project command tests still pass.

- [ ] **Step 7: Commit**

```bash
cd /workspace/triple-c
rustfmt --edition 2021 app/src-tauri/src/marketplace/sync.rs app/src-tauri/src/marketplace/mod.rs
git add app/src-tauri/src/marketplace/sync.rs app/src-tauri/src/marketplace/mod.rs app/src-tauri/src/commands/project_commands.rs
git commit -m "Marketplace: sync projects into their containers on start

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

### Task 10: GitHub sign-in through `gh` inside a container

**Files:**
- Create: `app/src-tauri/src/marketplace/gh_login.rs`
- Modify: `app/src-tauri/src/marketplace/mod.rs` (add `pub mod gh_login;`)

**Interfaces:**
- Consumes: `docker::exec::{create_attached_exec_as, wait_for_exec_exit, AttachedExec}` (existing: `create_attached_exec_as(container_id, cmd, tty, user, working_dir) -> Result<AttachedExec, String>`; `AttachedExec { exec_id, output: Stream<Item = Result<LogOutput, _>>, input: AsyncWrite }`; `wait_for_exec_exit(exec_id) -> Option<i64>`).
- Produces: `gh_login::{run_gh_container_login, parse_device_prompt, extract_token, take_display_lines, strip_ansi, valid_host, CODE_EVENT, OUTPUT_EVENT}`. Events: `marketplace-gh-login-code` `{ account_id, code, url }`; `marketplace-gh-login-output` `{ account_id, chunk }` (complete lines only; never contains the token).

Why the flags: `--web` does the device flow; `--git-protocol ssh --skip-ssh-key` avoids gh's "Authenticate Git with your GitHub credentials?" prompt (which `https` triggers and which writes a credential helper into the git config); `GH_CONFIG_DIR` and `GIT_CONFIG_GLOBAL` point into a temp dir that is deleted on exit, so Claude in that container is not left logged into the user's GitHub; `BROWSER=true` makes gh's "open the browser" step a no-op inside the container. The host arrives as `$1` (argv, never interpolated) because `create_attached_exec_as` takes no env. The Enter that dismisses "Press Enter to open github.com in your browser…" is its own write, sent 250 ms after the prompt appears (the PR #64 lesson: text and Enter in one write can be swallowed as a paste).

- [ ] **Step 1: Write the failing tests**

Create `app/src-tauri/src/marketplace/gh_login.rs`:

```rust
//! GitHub sign-in through `gh auth login --web` inside a running container, for
//! hosts that have no `gh` of their own. The token is read back through the
//! exec, returned to the caller for the keychain, and never emitted, logged or
//! left behind in the container.

use std::time::Duration;

use futures_util::StreamExt;
use tauri::{AppHandle, Emitter};
use tokio::io::AsyncWriteExt;
use tokio::sync::oneshot;

use crate::docker::exec::{create_attached_exec_as, wait_for_exec_exit, AttachedExec};

#[cfg(test)]
mod tests {
    use super::*;

    const GH_PROMPT: &str = "! First copy your one-time code: 4F2A-9C1B\nPress Enter to open github.com in your browser... ";

    #[test]
    fn the_device_code_is_read_and_the_url_defaults_to_the_host() {
        assert_eq!(
            parse_device_prompt(GH_PROMPT, "github.com"),
            Some(("4F2A-9C1B".to_string(), "https://github.com/login/device".to_string()))
        );
    }

    #[test]
    fn an_explicit_device_url_wins() {
        let out = "! First copy your one-time code: AB12-CD34\nOpen this URL to continue in your web browser: https://ghe.example.com/login/device\n";
        assert_eq!(
            parse_device_prompt(out, "ghe.example.com"),
            Some(("AB12-CD34".to_string(), "https://ghe.example.com/login/device".to_string()))
        );
    }

    #[test]
    fn no_code_yet_means_no_prompt() {
        assert_eq!(parse_device_prompt("! First copy your one-time", "github.com"), None);
        assert_eq!(parse_device_prompt("", "github.com"), None);
    }

    #[test]
    fn the_token_is_taken_from_between_the_markers() {
        let out = "✓ Logged in\n__TRIPLEC_TOKEN_BEGIN__test-token-not-real__TRIPLEC_TOKEN_END__\n";
        assert_eq!(extract_token(out), Some("test-token-not-real".to_string()));
        assert_eq!(extract_token("__TRIPLEC_TOKEN_BEGIN__test-token-not-real"), None, "unterminated");
        assert_eq!(extract_token("__TRIPLEC_TOKEN_BEGIN____TRIPLEC_TOKEN_END__"), None, "empty");
        assert_eq!(extract_token("__TRIPLEC_TOKEN_BEGIN__a b__TRIPLEC_TOKEN_END__"), None, "whitespace");
    }

    #[test]
    fn only_complete_lines_are_shown_and_the_token_line_never_is() {
        let mut pending = String::new();
        assert_eq!(take_display_lines(&mut pending, "! First copy your one-"), "");
        assert_eq!(take_display_lines(&mut pending, "time code: 4F2A-9C1B\nPress"), "! First copy your one-time code: 4F2A-9C1B\n");
        assert_eq!(pending, "Press");
        let shown = take_display_lines(
            &mut pending,
            " Enter\n__TRIPLEC_TOKEN_BEGIN__test-token-not-real__TRIPLEC_TOKEN_END__\ndone\n",
        );
        assert_eq!(shown, "Press Enter\ndone\n");
        assert!(!shown.contains("test-token-not-real"));
    }

    #[test]
    fn escape_sequences_and_carriage_returns_are_removed() {
        assert_eq!(strip_ansi("\u{1b}[1;32m✓\u{1b}[0m done\r\n"), "✓ done\n");
        assert_eq!(strip_ansi("a\u{1b}]8;;https://x\u{7}link\u{1b}]8;;\u{7}b"), "alinkb");
        assert_eq!(strip_ansi("cut\u{1b}["), "cut");
    }

    #[test]
    fn hosts_are_plain_names() {
        assert!(valid_host("github.com"));
        assert!(valid_host("ghe.corp-1.example"));
        for bad in ["", "-x", "a b", "a;b", "a/b", "$(id)"] {
            assert!(!valid_host(bad), "{bad:?}");
        }
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Add `pub mod gh_login;` to `app/src-tauri/src/marketplace/mod.rs`.

Run: `cd app/src-tauri && cargo test --lib marketplace::gh_login`
Expected: FAIL to compile — functions not found.

- [ ] **Step 3: Implement**

Insert above `#[cfg(test)]` in `gh_login.rs`:

```rust
pub const CODE_EVENT: &str = "marketplace-gh-login-code";
pub const OUTPUT_EVENT: &str = "marketplace-gh-login-output";

const LOGIN_TIMEOUT: Duration = Duration::from_secs(10 * 60);
const ENTER_DELAY: Duration = Duration::from_millis(250);
const TOKEN_BEGIN: &str = "__TRIPLEC_TOKEN_BEGIN__";
const TOKEN_END: &str = "__TRIPLEC_TOKEN_END__";
const MAX_TRANSCRIPT: usize = 64 * 1024;
const MAX_PENDING_LINE: usize = 4096;

/// Constant script; the host is `$1`. See the task notes for each flag.
const GH_LOGIN_SCRIPT: &str = r#"set -eu
host="$1"
case "$host" in
  '' | -* | *[!A-Za-z0-9.-]*) echo "invalid host" >&2; exit 2 ;;
esac
export HOME=/home/claude
d=$(mktemp -d)
trap 'rm -rf "$d"' EXIT
export GH_CONFIG_DIR="$d" GIT_CONFIG_GLOBAL="$d/gitconfig" BROWSER=true
gh auth login --hostname "$host" --web --git-protocol ssh --skip-ssh-key --scopes repo
t=$(gh auth token --hostname "$host")
printf '\n%s%s%s\n' __TRIPLEC_TOKEN_BEGIN__ "$t" __TRIPLEC_TOKEN_END__
"#;

pub fn valid_host(host: &str) -> bool {
    !host.is_empty()
        && host.len() <= 253
        && !host.starts_with('-')
        && host.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
}

/// Remove CSI and OSC sequences, other two-byte escapes, and `\r`. An
/// unterminated sequence at the end is dropped.
pub fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\u{1b}' => match chars.next() {
                Some('[') => {
                    for c in chars.by_ref() {
                        if ('\u{40}'..='\u{7e}').contains(&c) {
                            break;
                        }
                    }
                }
                Some(']') => {
                    while let Some(c) = chars.next() {
                        if c == '\u{7}' {
                            break;
                        }
                        if c == '\u{1b}' && chars.peek() == Some(&'\\') {
                            chars.next();
                            break;
                        }
                    }
                }
                _ => {}
            },
            '\r' => {}
            c => out.push(c),
        }
    }
    out
}

/// gh prints `! First copy your one-time code: XXXX-XXXX`, then either a URL
/// or "Press Enter to open <host> in your browser". Returns (code, url).
pub fn parse_device_prompt(output: &str, host: &str) -> Option<(String, String)> {
    const LABEL: &str = "one-time code:";
    let at = output.find(LABEL)? + LABEL.len();
    let code: String = output[at..]
        .trim_start()
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '-')
        .collect();
    if code.len() < 6 || !code.contains('-') {
        return None;
    }
    let url = output
        .split_whitespace()
        .find(|w| w.starts_with("https://") && w.contains("/login/device"))
        .map(|w| w.trim_end_matches(|c: char| !c.is_ascii_alphanumeric() && c != '/').to_string())
        .unwrap_or_else(|| format!("https://{host}/login/device"));
    Some((code, url))
}

pub fn extract_token(text: &str) -> Option<String> {
    let start = text.find(TOKEN_BEGIN)? + TOKEN_BEGIN.len();
    let end = start + text[start..].find(TOKEN_END)?;
    let token = text[start..end].trim();
    if token.is_empty() || token.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return None;
    }
    Some(token.to_string())
}

/// Append `chunk` and hand back the complete lines, minus any line carrying the
/// token markers. A partial line waits in `pending` (so a marker split across
/// chunks is never shown), and is dropped if it grows past a bound.
pub fn take_display_lines(pending: &mut String, chunk: &str) -> String {
    pending.push_str(chunk);
    let Some(last_nl) = pending.rfind('\n') else {
        if pending.len() > MAX_PENDING_LINE {
            pending.clear();
        }
        return String::new();
    };
    let complete: String = pending.drain(..=last_nl).collect();
    complete
        .lines()
        .filter(|l| !l.contains("__TRIPLEC_TOKEN"))
        .map(|l| format!("{l}\n"))
        .collect()
}

fn push_capped(buf: &mut String, text: &str) {
    buf.push_str(text);
    if buf.len() > MAX_TRANSCRIPT {
        let mut cut = buf.len() - MAX_TRANSCRIPT;
        while !buf.is_char_boundary(cut) {
            cut += 1;
        }
        buf.drain(..cut);
    }
}

/// What to show when the login ends without a token: the last few lines, with
/// any marker line removed.
fn failure_tail(transcript: &str) -> String {
    let lines: Vec<&str> = transcript
        .lines()
        .filter(|l| !l.contains("__TRIPLEC_TOKEN") && !l.trim().is_empty())
        .collect();
    lines[lines.len().saturating_sub(5)..].join("\n")
}

/// Run `gh auth login --web` in the container and return the token it minted.
pub async fn run_gh_container_login(
    app: &AppHandle,
    account_id: &str,
    container_id: &str,
    host: &str,
    mut cancel: oneshot::Receiver<()>,
) -> Result<String, String> {
    if !valid_host(host) {
        return Err(format!("\"{host}\" is not a valid host name."));
    }
    let AttachedExec { exec_id, mut output, mut input } = create_attached_exec_as(
        container_id,
        vec![
            "sh".to_string(),
            "-c".to_string(),
            GH_LOGIN_SCRIPT.to_string(),
            "triple-c-gh-login".to_string(),
            host.to_string(),
        ],
        true,
        "claude",
        "/home/claude",
    )
    .await?;

    let deadline = tokio::time::Instant::now() + LOGIN_TIMEOUT;
    let mut transcript = String::new();
    let mut pending = String::new();
    let mut code_sent = false;
    let mut enter_sent = false;

    loop {
        let next = tokio::select! {
            _ = &mut cancel => {
                return Err("GitHub sign-in cancelled. Nothing was stored.".to_string());
            }
            next = tokio::time::timeout_at(deadline, output.next()) => match next {
                Ok(next) => next,
                Err(_) => {
                    return Err(format!(
                        "Timed out after {} minutes waiting for the GitHub sign-in. Nothing was stored.",
                        LOGIN_TIMEOUT.as_secs() / 60
                    ));
                }
            },
        };
        let frame = match next {
            Some(Ok(frame)) => frame,
            Some(Err(e)) => return Err(format!("Lost the connection to gh: {e}. Nothing was stored.")),
            None => break,
        };
        let text = strip_ansi(&String::from_utf8_lossy(&frame.into_bytes()));
        push_capped(&mut transcript, &text);

        let shown = take_display_lines(&mut pending, &text);
        if !shown.is_empty() {
            let _ = app.emit(OUTPUT_EVENT, serde_json::json!({ "account_id": account_id, "chunk": shown }));
        }
        if !code_sent {
            if let Some((code, url)) = parse_device_prompt(&transcript, host) {
                let _ = app.emit(
                    CODE_EVENT,
                    serde_json::json!({ "account_id": account_id, "code": code, "url": url }),
                );
                code_sent = true;
            }
        }
        if code_sent && !enter_sent && transcript.contains("Press Enter") {
            tokio::time::sleep(ENTER_DELAY).await;
            input
                .write_all(b"\r")
                .await
                .map_err(|e| format!("Could not answer gh's prompt: {e}. Nothing was stored."))?;
            let _ = input.flush().await;
            enter_sent = true;
        }
    }

    let status = wait_for_exec_exit(&exec_id).await;
    if let Some(token) = extract_token(&transcript) {
        return Ok(token);
    }
    Err(format!(
        "gh did not complete the sign-in (exit status {}). Nothing was stored.\n{}",
        status.map(|c| c.to_string()).unwrap_or_else(|| "unknown".to_string()),
        failure_tail(&transcript)
    ))
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cd app/src-tauri && cargo test --lib marketplace::gh_login`
Expected: 7 passed.

- [ ] **Step 5: Commit**

```bash
cd /workspace/triple-c
rustfmt --edition 2021 app/src-tauri/src/marketplace/gh_login.rs
git add app/src-tauri/src/marketplace/gh_login.rs app/src-tauri/src/marketplace/mod.rs
git commit -m "Marketplace: GitHub sign-in through gh inside a container

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

### Task 11: Tauri commands, store-owned fields, wiring and startup refresh

**Files:**
- Create: `app/src-tauri/src/commands/marketplace_commands.rs`
- Modify: `app/src-tauri/src/commands/mod.rs` (add `pub mod marketplace_commands;`)
- Modify: `app/src-tauri/src/lib.rs` (`generate_handler!` entries; startup refresh; drop Task 6's `let _ = &marketplace_setup;`)
- Modify: `app/src-tauri/capabilities/default.json` (21 grants)
- Modify: `app/src-tauri/src/commands/settings_commands.rs` (`restore_marketplace_fields`)
- Modify: `app/src-tauri/src/commands/project_commands.rs` (`restore_store_owned_fields`)
- Modify: `app/src-tauri/src/commands/settings_export_commands.rs` (`apply_settings_import`)

**Interfaces:**
- Consumes: everything in `crate::marketplace` from Tasks 3–10; `models::marketplace::{InstallScope, ProjectSyncResult, …}` (Task 2); `storage::secure::{store_marketplace_token, delete_marketplace_token}` and `auth::{host_of, validate_token, resolve_credential, gh_host_available, gh_host_login}` (Task 5); `crate::docker::container::is_container_running(&str) -> Result<bool, String>`.
- Produces: the 21 commands in the contract table, with exactly those names, argument names and return types; `ops` pure helpers (`upsert_install`, `remove_install`, `set_disabled`, `repin`, `validate_label`, `validate_branch`, `validate_host`).
- Note for the frontend (Task 15): `start_marketplace_gh_container_login` generates the account id itself, so its `marketplace-gh-login-*` events carry an id the dialog has not seen yet. Only one login can run at a time, so the dialog should accept events while it is open without filtering on `account_id`.

- [ ] **Step 1: Write the failing tests for the pure helpers and store-owned fields**

Create `app/src-tauri/src/commands/marketplace_commands.rs` with the helper module and its tests first:

```rust
//! Marketplace commands: configure marketplaces and accounts, browse, install,
//! update, and push installs into running containers. Spec:
//! `docs/superpowers/specs/2026-09-27-marketplace-design.md`.

use tauri::{AppHandle, State};
use tokio::sync::oneshot;

use crate::docker::container::is_container_running;
use crate::marketplace::{self as mk, auth, catalog, diff, gh_login, git, tree::GitTree, MarketplaceManager};
use crate::models::marketplace::{
    is_valid_commit, is_valid_item_key, AccountMethod, FileDiff, InstallScope, ItemUpdate, Marketplace,
    MarketplaceAccount, MarketplaceInstall, MarketplaceItemRef, MarketplaceSnapshot, ProjectSyncResult,
    SyncReport,
};
use crate::models::{AppSettings, Project};
use crate::storage::secure;
use crate::AppState;

/// Pure list/field operations behind the commands, kept apart so they are
/// testable without a Tauri runtime.
pub(crate) mod ops {
    use crate::models::marketplace::{MarketplaceInstall, MarketplaceItemRef};

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

    pub fn set_disabled(list: &mut Vec<MarketplaceItemRef>, item: &MarketplaceItemRef, disabled: bool) {
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

    /// `None` or blank means the repository's default branch.
    pub fn validate_branch(branch: Option<String>) -> Result<Option<String>, String> {
        let Some(b) = branch.map(|b| b.trim().to_string()).filter(|b| !b.is_empty()) else {
            return Ok(None);
        };
        let ok = b.len() <= 200
            && !b.starts_with('-')
            && !b.starts_with('/')
            && !b.ends_with('/')
            && !b.contains("..")
            && !b.contains("//")
            && b.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '/'));
        if ok {
            Ok(Some(b))
        } else {
            Err(format!("\"{b}\" is not a valid branch name."))
        }
    }

    /// Lowercased host name with an optional `:port`.
    pub fn validate_host(host: &str) -> Result<String, String> {
        let host = host.trim().to_ascii_lowercase();
        let (name, port) = match host.split_once(':') {
            Some((n, p)) => (n, Some(p)),
            None => (host.as_str(), None),
        };
        let name_ok = !name.is_empty()
            && name.len() <= 253
            && !name.starts_with('-')
            && !name.starts_with('.')
            && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-');
        let port_ok = port.map_or(true, |p| !p.is_empty() && p.len() <= 5 && p.chars().all(|c| c.is_ascii_digit()));
        if name_ok && port_ok {
            Ok(host)
        } else {
            Err(format!("\"{host}\" is not a valid host name."))
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use crate::models::marketplace::ItemKind;

        fn r(key: &str) -> MarketplaceItemRef {
            MarketplaceItemRef { marketplace_id: "m".into(), kind: ItemKind::Agent, key: key.into() }
        }
        fn i(key: &str, commit: &str) -> MarketplaceInstall {
            MarketplaceInstall { marketplace_id: "m".into(), kind: ItemKind::Agent, key: key.into(), commit: commit.into() }
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
            assert_eq!(validate_branch(None).unwrap(), None);
            assert_eq!(validate_branch(Some("  ".into())).unwrap(), None);
            assert_eq!(validate_branch(Some("release/1.x".into())).unwrap(), Some("release/1.x".into()));
            for bad in ["-x", "a..b", "a b", "a;b", "/a", "a/"] {
                assert!(validate_branch(Some(bad.into())).is_err(), "{bad}");
            }
            assert_eq!(validate_host("GitHub.com").unwrap(), "github.com");
            assert_eq!(validate_host("repo.example.net:3000").unwrap(), "repo.example.net:3000");
            for bad in ["", "-a", "a b", "a/b", "a:", "a:x", "a;rm"] {
                assert!(validate_host(bad).is_err(), "{bad}");
            }
        }
    }
}
```

Add `pub mod marketplace_commands;` to `app/src-tauri/src/commands/mod.rs`.

Append to the `tests` module in `app/src-tauri/src/commands/settings_commands.rs`:

```rust
    #[test]
    fn a_stale_settings_save_cannot_overwrite_marketplace_state() {
        use crate::models::marketplace::Marketplace;
        let mut stored = AppSettings::default();
        stored.marketplaces.push(Marketplace {
            id: "m1".into(),
            name: "Team".into(),
            url: "https://example.invalid/r.git".into(),
            branch: None,
            account_id: None,
        });
        // The frontend's copy predates the marketplace being added.
        let mut incoming = AppSettings::default();
        incoming.auto_check_updates = false;

        restore_marketplace_fields(&mut incoming, &stored);

        assert_eq!(incoming.marketplaces, stored.marketplaces);
        assert!(!incoming.auto_check_updates, "the edit the save was for still applies");
    }
```

Append to the `tests` module in `app/src-tauri/src/commands/project_commands.rs`:

```rust
    /// The marketplace commands own a project's installs and opt-outs; the
    /// Config tab's next unrelated save carries a stale copy of both.
    #[test]
    fn a_stale_save_cannot_undo_a_marketplace_install() {
        use crate::models::marketplace::{ItemKind, MarketplaceInstall, MarketplaceItemRef};
        let (mut stored, mut payload) = stored_and_stale_payload();
        stored.marketplace_installs = vec![MarketplaceInstall {
            marketplace_id: "m1".into(),
            kind: ItemKind::Agent,
            key: "code-reviewer".into(),
            commit: "a".repeat(40),
        }];
        stored.marketplace_disabled =
            vec![MarketplaceItemRef { marketplace_id: "m1".into(), kind: ItemKind::Hook, key: "h".into() }];

        restore_store_owned_fields(&mut payload, &stored);

        assert_eq!(payload.marketplace_installs, stored.marketplace_installs);
        assert_eq!(payload.marketplace_disabled, stored.marketplace_disabled);
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cd app/src-tauri && cargo test --lib marketplace_commands settings_commands project_commands`
Expected: FAIL — `restore_marketplace_fields` not found; the project test fails its assertions (the fields are not restored yet). (Cargo accepts one filter; run the three names one at a time if needed: `cargo test --lib marketplace_commands`, then `settings_commands`, then `project_commands`.)

- [ ] **Step 3: Make marketplace fields store-owned**

In `app/src-tauri/src/commands/settings_commands.rs`, add above `update_settings`:

```rust
/// Marketplace state is written only by the marketplace commands
/// (`commands/marketplace_commands.rs`), each of which returns fresh settings.
/// Every other settings save posts the frontend's copy back whole, and that
/// copy can predate an install made a moment ago, so what is stored wins.
fn restore_marketplace_fields(incoming: &mut AppSettings, stored: &AppSettings) {
    incoming.marketplace_accounts = stored.marketplace_accounts.clone();
    incoming.marketplaces = stored.marketplaces.clone();
    incoming.global_marketplace_installs = stored.global_marketplace_installs.clone();
}
```

and in `update_settings` change the parameter to `mut settings: AppSettings` and add, directly after `validate_settings_update(&before, &settings)?;`:

```rust
    restore_marketplace_fields(&mut settings, &before);
```

In `app/src-tauri/src/commands/project_commands.rs`, add to the end of `restore_store_owned_fields`:

```rust
    // Owned by the marketplace commands; a Config-tab save carries a stale copy.
    project.marketplace_installs = stored.marketplace_installs.clone();
    project.marketplace_disabled = stored.marketplace_disabled.clone();
```

In `app/src-tauri/src/commands/settings_export_commands.rs`, `apply_settings_import`, replace

```rust
    let saved =
        crate::commands::settings_commands::update_settings(settings, state.clone()).await?;
```

with

```rust
    let imported_marketplace = (
        settings.marketplace_accounts.clone(),
        settings.marketplaces.clone(),
        settings.global_marketplace_installs.clone(),
    );
    let saved =
        crate::commands::settings_commands::update_settings(settings, state.clone()).await?;
    // `update_settings` keeps marketplace state store-owned. An import is the
    // one caller entitled to replace it wholesale.
    let saved = {
        let mut s = saved;
        (s.marketplace_accounts, s.marketplaces, s.global_marketplace_installs) = imported_marketplace;
        state.settings_store.update(s)?
    };
```

Run: `cd app/src-tauri && cargo test --lib settings_commands && cargo test --lib project_commands && cargo test --lib marketplace_commands`
Expected: all pass.

- [ ] **Step 4: Implement the commands**

Append to `app/src-tauri/src/commands/marketplace_commands.rs` (below the `ops` module):

```rust
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
    state.projects_store.get(id).ok_or_else(|| format!("Project {id} not found"))
}

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

fn validate_item(item: &MarketplaceItemRef) -> Result<(), String> {
    if is_valid_item_key(&item.key) {
        Ok(())
    } else {
        Err(format!("\"{}\" is not a valid item name.", item.key))
    }
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

async fn snapshot_blocking(state: &AppState, m: &Marketplace) -> Result<MarketplaceSnapshot, String> {
    let mgr = state.marketplace.clone();
    let m = m.clone();
    tokio::task::spawn_blocking(move || snapshot_or_cached(&mgr, &m))
        .await
        .map_err(|e| format!("Reading the marketplace cache failed: {e}"))
}

/// Make each cache's pin refs exactly the commits installs reference, so a
/// pinned version can never be garbage-collected away.
async fn refresh_pins(state: &AppState) {
    let settings = state.settings_store.get();
    let pins = mk::pins_by_marketplace(&settings, &state.projects_store.list());
    let root = state.marketplace.data_root().to_path_buf();
    let ids: Vec<String> = settings.marketplaces.iter().map(|m| m.id.clone()).collect();
    let _ = tokio::task::spawn_blocking(move || {
        for id in ids {
            let repo = git::cache_path(&root, &id);
            if !repo.exists() {
                continue;
            }
            let commits = pins.get(&id).cloned().unwrap_or_default();
            if let Err(e) = git::set_pins(&repo, &commits) {
                log::warn!("Could not update the pinned commits of marketplace {}: {}", id, e);
            }
        }
    })
    .await;
}

fn remove_cache(state: &AppState, marketplace_id: &str) {
    state.marketplace.remove_snapshot(marketplace_id);
    let path = git::cache_path(state.marketplace.data_root(), marketplace_id);
    if path.exists() {
        if let Err(e) = std::fs::remove_dir_all(&path) {
            log::warn!("Could not delete the marketplace cache {}: {}", path.display(), e);
        }
    }
}

fn save_new_account(state: &AppState, account: MarketplaceAccount, stored_token: bool) -> Result<MarketplaceAccount, String> {
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
pub async fn list_marketplace_snapshots(state: State<'_, AppState>) -> Result<Vec<MarketplaceSnapshot>, String> {
    let settings = state.settings_store.get();
    let mgr = state.marketplace.clone();
    tokio::task::spawn_blocking(move || settings.marketplaces.iter().map(|m| snapshot_or_cached(&mgr, m)).collect())
        .await
        .map_err(|e| format!("Reading the marketplace caches failed: {e}"))
}

#[tauri::command]
pub async fn refresh_marketplaces(
    marketplace_id: Option<String>,
    state: State<'_, AppState>,
) -> Result<Vec<MarketplaceSnapshot>, String> {
    let settings = state.settings_store.get();
    if let Some(id) = &marketplace_id {
        find_marketplace(&settings, id)?;
    }
    for m in settings
        .marketplaces
        .iter()
        .filter(|m| marketplace_id.as_deref().map_or(true, |id| id == m.id))
    {
        mk::refresh_marketplace(&state.marketplace, &settings, &m.id).await;
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
    let mut m = Marketplace { id: uuid::Uuid::new_v4().to_string(), name, url, branch, account_id };
    validate_marketplace(&settings, &mut m)?;
    if settings
        .marketplaces
        .iter()
        .any(|x| x.url.eq_ignore_ascii_case(&m.url) && x.branch == m.branch)
    {
        return Err("This repository (and branch) has already been added.".to_string());
    }

    let mut trial = settings.clone();
    trial.marketplaces.push(m.clone());
    let snap = mk::refresh_marketplace(&state.marketplace, &trial, &m.id).await;
    let failure = snap
        .fetch_error
        .clone()
        .or_else(|| snap.head_commit.is_none().then(|| "The repository has no commits yet.".to_string()));
    if let Some(e) = failure {
        remove_cache(&state, &m.id);
        return Err(e);
    }

    let mut current = state.settings_store.get();
    current.marketplaces.push(m);
    state.settings_store.update(current)?;
    Ok(snap)
}

#[tauri::command]
pub async fn update_marketplace(marketplace: Marketplace, state: State<'_, AppState>) -> Result<AppSettings, String> {
    let mut settings = state.settings_store.get();
    let mut m = marketplace;
    validate_marketplace(&settings, &mut m)?;
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
pub async fn remove_marketplace(marketplace_id: String, state: State<'_, AppState>) -> Result<AppSettings, String> {
    let mut settings = state.settings_store.get();
    find_marketplace(&settings, &marketplace_id)?;
    settings.marketplaces.retain(|m| m.id != marketplace_id);
    let saved = state.settings_store.update(settings)?;
    remove_cache(&state, &marketplace_id);
    Ok(saved)
}

#[tauri::command]
pub async fn forget_marketplace_installs(marketplace_id: String, state: State<'_, AppState>) -> Result<(), String> {
    let mut settings = state.settings_store.get();
    settings.global_marketplace_installs.retain(|i| i.marketplace_id != marketplace_id);
    state.settings_store.update(settings)?;
    for mut p in state.projects_store.list() {
        let before = (p.marketplace_installs.len(), p.marketplace_disabled.len());
        p.marketplace_installs.retain(|i| i.marketplace_id != marketplace_id);
        p.marketplace_disabled.retain(|r| r.marketplace_id != marketplace_id);
        if (p.marketplace_installs.len(), p.marketplace_disabled.len()) != before {
            state.projects_store.update(p)?;
        }
    }
    refresh_pins(&state).await;
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// Installs
// ─────────────────────────────────────────────────────────────────────────────

/// Pins the item at the marketplace's current head. Returns fresh settings;
/// for a project scope the caller reloads projects.
#[tauri::command]
pub async fn install_marketplace_item(
    item: MarketplaceItemRef,
    scope: InstallScope,
    state: State<'_, AppState>,
) -> Result<AppSettings, String> {
    validate_item(&item)?;
    let settings = state.settings_store.get();
    let m = find_marketplace(&settings, &item.marketplace_id)?;
    let snap = snapshot_blocking(&state, &m).await?;
    let head = snap
        .head_commit
        .clone()
        .ok_or_else(|| format!("\"{}\" has not been fetched yet — refresh it first.", m.name))?;
    let entry = snap
        .items
        .iter()
        .find(|i| i.kind == item.kind && i.key == item.key)
        .ok_or_else(|| format!("\"{}\" is no longer in \"{}\" — refresh the marketplace.", item.key, m.name))?;
    if let Some(reason) = &entry.invalid {
        return Err(format!("\"{}\" cannot be installed: {}", entry.name, reason));
    }
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
            let mut p = find_project(&state, &project_id)?;
            ops::upsert_install(&mut p.marketplace_installs, inst);
            state.projects_store.update(p)?;
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
            for mut p in state.projects_store.list() {
                if p.marketplace_disabled.contains(&item) {
                    ops::set_disabled(&mut p.marketplace_disabled, &item, false);
                    state.projects_store.update(p)?;
                }
            }
        }
        InstallScope::Project { project_id } => {
            let mut p = find_project(&state, &project_id)?;
            if !ops::remove_install(&mut p.marketplace_installs, &item) {
                return Err(format!("That item is not installed in \"{}\".", p.name));
            }
            state.projects_store.update(p)?;
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
    let mut p = find_project(&state, &project_id)?;
    ops::set_disabled(&mut p.marketplace_disabled, &item, disabled);
    state.projects_store.update(p)
}

// ─────────────────────────────────────────────────────────────────────────────
// Updates
// ─────────────────────────────────────────────────────────────────────────────

#[tauri::command]
pub async fn list_marketplace_updates(state: State<'_, AppState>) -> Result<Vec<ItemUpdate>, String> {
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
    tokio::task::spawn_blocking(move || diff::item_diff(&repo, item.kind, &item.key, &from_commit, &to_commit))
        .await
        .map_err(|e| format!("Computing the diff failed: {e}"))?
}

/// Moves one install's pin to the marketplace's head, if the item is still
/// installable there.
#[tauri::command]
pub async fn update_marketplace_item(
    item: MarketplaceItemRef,
    scope: InstallScope,
    state: State<'_, AppState>,
) -> Result<(), String> {
    validate_item(&item)?;
    let settings = state.settings_store.get();
    let m = find_marketplace(&settings, &item.marketplace_id)?;
    let head = snapshot_blocking(&state, &m)
        .await?
        .head_commit
        .ok_or_else(|| format!("\"{}\" has not been fetched yet — refresh it first.", m.name))?;

    let repo = git::cache_path(state.marketplace.data_root(), &m.id);
    let (kind, key, at) = (item.kind, item.key.clone(), head.clone());
    tokio::task::spawn_blocking(move || -> Result<(), String> {
        let tree = GitTree::open(&repo, &at)?;
        catalog::item_files(&tree, kind, &key).map(|_| ())
    })
    .await
    .map_err(|e| format!("Checking the new version failed: {e}"))?
    .map_err(|e| format!("\"{}\" cannot be updated: {e}", item.key))?;

    match scope {
        InstallScope::Global => {
            let mut s = state.settings_store.get();
            if !ops::repin(&mut s.global_marketplace_installs, &item, &head) {
                return Err("That item is not installed for all projects.".to_string());
            }
            state.settings_store.update(s)?;
        }
        InstallScope::Project { project_id } => {
            let mut p = find_project(&state, &project_id)?;
            if !ops::repin(&mut p.marketplace_installs, &item, &head) {
                return Err(format!("That item is not installed in \"{}\".", p.name));
            }
            state.projects_store.update(p)?;
        }
    }
    refresh_pins(&state).await;
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// Sync
// ─────────────────────────────────────────────────────────────────────────────

/// Sync one running project, or every running project when `project_id` is None.
#[tauri::command]
pub async fn apply_marketplace_now(
    project_id: Option<String>,
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
                return Err(format!("\"{}\" is not running. Its marketplace items are applied when it starts.", p.name));
            }
            continue;
        }
        let cid = p.container_id.clone().unwrap_or_default();
        let report = mk::sync_project(&state.marketplace, &settings, &p, &cid).await;
        results.push(ProjectSyncResult { project_id: p.id.clone(), report });
    }
    Ok(results)
}

#[tauri::command]
pub async fn get_marketplace_sync_report(project_id: String, state: State<'_, AppState>) -> Result<Option<SyncReport>, String> {
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
    let token = token.trim().to_string();
    if token.is_empty() || token.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err("Paste the whole token — it cannot be empty or contain spaces.".to_string());
    }
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
    let project = find_project(&state, &project_id)?;
    let container_id = project
        .container_id
        .clone()
        .ok_or_else(|| format!("\"{}\" has no container yet. Start it, then try again.", project.name))?;
    if !is_container_running(&container_id).await.unwrap_or(false) {
        return Err(format!("\"{}\" is not running. Start it, then try again.", project.name));
    }

    let (tx, rx) = oneshot::channel();
    if !state.marketplace.set_gh_login_cancel(Some(tx)).await {
        return Err("A GitHub sign-in is already running. Finish or cancel it first.".to_string());
    }
    let account_id = uuid::Uuid::new_v4().to_string();
    let result = gh_login::run_gh_container_login(&app_handle, &account_id, &container_id, &host, rx).await;
    state.marketplace.set_gh_login_cancel(None).await;
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
pub async fn test_marketplace_account(account_id: String, state: State<'_, AppState>) -> Result<String, String> {
    let settings = state.settings_store.get();
    let account = find_account(&settings, &account_id)?;
    let cred = auth::resolve_credential(&account).await?;
    Ok(auth::validate_token(&account.host, &cred.password)
        .await?
        .unwrap_or_else(|| "token present (this host has no sign-in check)".to_string()))
}

#[tauri::command]
pub async fn remove_marketplace_account(account_id: String, state: State<'_, AppState>) -> Result<AppSettings, String> {
    let mut settings = state.settings_store.get();
    let account = find_account(&settings, &account_id)?;
    if let Some(m) = settings.marketplaces.iter().find(|m| m.account_id.as_deref() == Some(account_id.as_str())) {
        return Err(format!("\"{}\" uses this account. Change or remove that marketplace first.", m.name));
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
```

Run: `cd app/src-tauri && cargo check --lib`
Expected: compiles, with "function is never used" warnings for the commands (they are registered next).

- [ ] **Step 5: Register the commands and grant them**

In `app/src-tauri/src/lib.rs`, inside `tauri::generate_handler![ … ]`, after `commands::auth_token_commands::sweep_claude_token_snapshots,` add:

```rust
            // Marketplace
            commands::marketplace_commands::list_marketplace_snapshots,
            commands::marketplace_commands::refresh_marketplaces,
            commands::marketplace_commands::add_marketplace,
            commands::marketplace_commands::update_marketplace,
            commands::marketplace_commands::remove_marketplace,
            commands::marketplace_commands::install_marketplace_item,
            commands::marketplace_commands::uninstall_marketplace_item,
            commands::marketplace_commands::set_global_item_disabled,
            commands::marketplace_commands::forget_marketplace_installs,
            commands::marketplace_commands::list_marketplace_updates,
            commands::marketplace_commands::marketplace_item_diff,
            commands::marketplace_commands::update_marketplace_item,
            commands::marketplace_commands::apply_marketplace_now,
            commands::marketplace_commands::get_marketplace_sync_report,
            commands::marketplace_commands::add_marketplace_token_account,
            commands::marketplace_commands::add_marketplace_gh_host_account,
            commands::marketplace_commands::start_marketplace_gh_container_login,
            commands::marketplace_commands::cancel_marketplace_gh_login,
            commands::marketplace_commands::test_marketplace_account,
            commands::marketplace_commands::remove_marketplace_account,
            commands::marketplace_commands::marketplace_gh_host_available,
```

In `app/src-tauri/capabilities/default.json`, change the last entry `"allow-clear-scheduler-notifications"` to `"allow-clear-scheduler-notifications",` and append:

```json
    "allow-list-marketplace-snapshots",
    "allow-refresh-marketplaces",
    "allow-add-marketplace",
    "allow-update-marketplace",
    "allow-remove-marketplace",
    "allow-install-marketplace-item",
    "allow-uninstall-marketplace-item",
    "allow-set-global-item-disabled",
    "allow-forget-marketplace-installs",
    "allow-list-marketplace-updates",
    "allow-marketplace-item-diff",
    "allow-update-marketplace-item",
    "allow-apply-marketplace-now",
    "allow-get-marketplace-sync-report",
    "allow-add-marketplace-token-account",
    "allow-add-marketplace-gh-host-account",
    "allow-start-marketplace-gh-container-login",
    "allow-cancel-marketplace-gh-login",
    "allow-test-marketplace-account",
    "allow-remove-marketplace-account",
    "allow-marketplace-gh-host-available"
```

(The `description` string in that file is the reviewed threat-model census; add one sentence to it: "The `*marketplace*` commands fetch user-configured https git repos on the host and push pinned files into containers; account tokens stay in the OS keychain and never cross IPC outward — the only inbound one is the token pasted into `add_marketplace_token_account`.")

- [ ] **Step 6: Refresh marketplaces at startup**

In `app/src-tauri/src/lib.rs`, delete the `let _ = &marketplace_setup;` line added in Task 6, and inside `.setup(move |app| { … })`, next to the other background spawns (after the housekeeping `reap_probe_containers` spawn), add:

```rust
            // Marketplaces: refresh each once at startup, in the background.
            // Failures are logged, not toasted — the Marketplace tab shows them.
            {
                let settings = settings_store_setup.get();
                let marketplace = marketplace_setup.clone();
                tauri::async_runtime::spawn(async move {
                    for m in &settings.marketplaces {
                        let snap = crate::marketplace::refresh_marketplace(&marketplace, &settings, &m.id).await;
                        if let Some(e) = snap.fetch_error {
                            log::warn!("Marketplace \"{}\" could not be refreshed at startup: {}", m.name, e);
                        }
                    }
                });
            }
```

- [ ] **Step 7: Build and run the whole Rust suite**

Run: `cd app/src-tauri && cargo build && cargo test --lib`
Expected: builds (`build.rs` accepts the 21 new grants — a missing or misspelled one fails here with the command name); all tests pass. `gen/schemas/*.json` is regenerated by the build.

- [ ] **Step 8: Commit**

```bash
cd /workspace/triple-c
rustfmt --edition 2021 app/src-tauri/src/commands/marketplace_commands.rs
git add app/src-tauri/src/commands/marketplace_commands.rs app/src-tauri/src/commands/mod.rs \
  app/src-tauri/src/commands/settings_commands.rs app/src-tauri/src/commands/project_commands.rs \
  app/src-tauri/src/commands/settings_export_commands.rs app/src-tauri/src/lib.rs \
  app/src-tauri/capabilities/default.json app/src-tauri/gen/schemas
git commit -m "Marketplace: Tauri commands, store-owned fields and startup refresh

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 12: Frontend plumbing — wrappers, store, tab, settings section, view shell

**Files:**
- Modify: `app/src/lib/tauri-commands.ts` (append a `// ---- Marketplace ----` section)
- Create: `app/src/lib/marketplace.ts`, `app/src/lib/marketplace.test.ts`
- Modify: `app/src/store/appState.ts` (helpers near the tab-key helpers, lines ~82-95; interface ~131-170; implementation near `closeHomeTab` ~377)
- Modify: `app/src/store/appState.test.ts` (append a describe block)
- Modify: `app/src/App.tsx` (import line 24; pane list ~lines 121-164; call `useMarketplaceSyncToasts()`)
- Modify: `app/src/components/layout/MainTabs.tsx` (imports lines 5-10; store selector ~line 44; `tabLabel` ~194; ghost icon ~275; `renderTab` ~314)
- Modify: `app/src/components/layout/MainTabs.test.tsx` (append a test)
- Modify: `app/src/hooks/useKeyboardShortcuts.ts` (import line 2; Ctrl+Shift+W branch ~line 65)
- Modify: `app/src/hooks/useKeyboardShortcuts.test.tsx` (append a test)
- Modify: `app/src/components/layout/NotesDock.tsx` (comment only, see Step 11)
- Create: `app/src/hooks/useMarketplace.ts`, `app/src/hooks/useMarketplace.test.ts`
- Create: `app/src/components/settings/MarketplaceSettings.tsx`, `app/src/components/settings/MarketplaceSettings.test.tsx`
- Modify: `app/src/components/settings/SettingsPanel.tsx` (after the `claude-auth` section, lines 170-172)
- Create: `app/src/components/marketplace/MarketplaceView.tsx`, `app/src/components/marketplace/MarketplaceView.test.tsx`

**Interfaces:**
- Consumes: TS types from Task 2 (`lib/types.ts`: `ItemKind`, `Marketplace`, `MarketplaceAccount`, `MarketplaceItemRef`, `MarketplaceInstall`, `MarketplaceSnapshot`, `ItemUpdate`, `FileDiff`, `SyncReport`, `InstallScope`, `ProjectSyncResult`, and the new `AppSettings`/`Project` fields); backend commands and `default.json` grants from Task 11.
- Produces:
  - wrappers exactly as in the contract;
  - `lib/marketplace.ts`: `ProjectItemState`, `itemRefKey`, `formatItemRef`, `projectItemState`, `effectiveInstalls`, `KIND_LABELS`, `KIND_ORDER`, `isStale`, `STALE_AFTER_MS`;
  - store: `MARKETPLACE_TAB_KEY`, `isMarketplaceTab`, `marketplaceFilterProjectId`, `openMarketplace`, `closeMarketplaceTab`, `setMarketplaceFilterProjectId`;
  - `hooks/useMarketplace.ts`: `useMarketplace(): MarketplaceApi` and `useMarketplaceSyncToasts(): void` (see code);
  - `MarketplaceView` with sub-tab ids `"browse" | "installed" | "accounts"` and panes that Tasks 13/14/15 replace.

- [ ] **Step 1: Write failing tests for `lib/marketplace.ts`**

`app/src/lib/marketplace.test.ts`:

```ts
import { describe, it, expect } from "vitest";
import {
  effectiveInstalls,
  formatItemRef,
  isStale,
  itemRefKey,
  projectItemState,
  STALE_AFTER_MS,
} from "./marketplace";
import type { MarketplaceInstall, MarketplaceSnapshot, Project } from "./types";

const A = "a".repeat(40);
const B = "b".repeat(40);

const inst = (key: string, commit = A, kind: MarketplaceInstall["kind"] = "agent"): MarketplaceInstall => ({
  marketplace_id: "m1",
  kind,
  key,
  commit,
});

const project = (patch: Partial<Project> = {}): Project =>
  ({
    id: "p1",
    name: "api",
    marketplace_installs: [],
    marketplace_disabled: [],
    ...patch,
  }) as unknown as Project;

describe("itemRefKey / formatItemRef", () => {
  it("keys and formats a ref", () => {
    const r = { marketplace_id: "m1", kind: "hook" as const, key: "notify" };
    expect(itemRefKey(r)).toBe("m1/hook/notify");
    expect(formatItemRef(r)).toBe("hook:notify");
  });
});

describe("projectItemState", () => {
  const ref = { marketplace_id: "m1", kind: "agent" as const, key: "rev" };

  it("is none when nothing installs it", () => {
    expect(projectItemState(ref, [], project())).toBe("none");
  });

  it("is inherited from a global install", () => {
    expect(projectItemState(ref, [inst("rev")], project())).toBe("inherited");
  });

  it("is opted_out when the project disabled the global install", () => {
    const p = project({ marketplace_disabled: [ref] });
    expect(projectItemState(ref, [inst("rev")], p)).toBe("opted_out");
  });

  it("is project for a project-only install", () => {
    const p = project({ marketplace_installs: [inst("rev")] });
    expect(projectItemState(ref, [], p)).toBe("project");
  });

  it("is project when project and global share the pin", () => {
    const p = project({ marketplace_installs: [inst("rev", A)] });
    expect(projectItemState(ref, [inst("rev", A)], p)).toBe("project");
  });

  it("flags a project pin that differs from the global pin", () => {
    const p = project({ marketplace_installs: [inst("rev", B)] });
    expect(projectItemState(ref, [inst("rev", A)], p)).toBe("project_pinned_differently");
  });

  it("does not confuse kinds with the same key", () => {
    expect(projectItemState(ref, [inst("rev", A, "skill")], project())).toBe("none");
  });
});

describe("effectiveInstalls", () => {
  it("merges global minus disabled plus project, project winning", () => {
    const disabledRef = { marketplace_id: "m1", kind: "agent" as const, key: "off" };
    const p = project({
      marketplace_disabled: [disabledRef],
      marketplace_installs: [inst("both", B), inst("mine")],
    });
    const out = effectiveInstalls([inst("glob"), inst("off"), inst("both", A)], p);
    expect(out.map((i) => [i.key, i.commit, i.source])).toEqual([
      ["both", B, "project"],
      ["glob", A, "global"],
      ["mine", A, "project"],
    ]);
  });
});

describe("isStale", () => {
  const snap = (fetched_at: string | null): MarketplaceSnapshot => ({
    marketplace_id: "m1",
    head_commit: null,
    fetched_at,
    fetch_error: null,
    items: [],
  });
  const now = Date.parse("2026-09-27T12:00:00Z");

  it("treats a never-fetched snapshot as stale", () => {
    expect(isStale(snap(null), now)).toBe(true);
  });

  it("is fresh within 15 minutes and stale after", () => {
    expect(isStale(snap(new Date(now - STALE_AFTER_MS + 1000).toISOString()), now)).toBe(false);
    expect(isStale(snap(new Date(now - STALE_AFTER_MS - 1000).toISOString()), now)).toBe(true);
  });

  it("treats an unparsable timestamp as stale", () => {
    expect(isStale(snap("not a date"), now)).toBe(true);
  });
});
```

- [ ] **Step 2: Run to verify it fails**

Run: `cd app && npx vitest run src/lib/marketplace.test.ts`
Expected: FAIL — `Failed to resolve import "./marketplace"`.

- [ ] **Step 3: Implement `lib/marketplace.ts`**

```ts
import type {
  ItemKind,
  MarketplaceInstall,
  MarketplaceItemRef,
  MarketplaceSnapshot,
  Project,
} from "./types";

/** How a project relates to one marketplace item. */
export type ProjectItemState =
  | "none"
  | "inherited"
  | "opted_out"
  | "project"
  | "project_pinned_differently";

export const KIND_ORDER: ItemKind[] = ["agent", "skill", "command", "hook", "plugin"];

export const KIND_LABELS: Record<ItemKind, string> = {
  agent: "Agents",
  skill: "Skills",
  command: "Commands",
  hook: "Hooks",
  plugin: "Plugins",
};

/** A marketplace is refreshed when its tab opens if the last fetch is older than this. */
export const STALE_AFTER_MS = 15 * 60 * 1000;

export const itemRefKey = (r: MarketplaceItemRef) => `${r.marketplace_id}/${r.kind}/${r.key}`;

/** Same shape as the item strings in a `SyncReport`. */
export const formatItemRef = (r: MarketplaceItemRef) => `${r.kind}:${r.key}`;

const sameItem = (a: MarketplaceItemRef, b: MarketplaceItemRef) =>
  a.marketplace_id === b.marketplace_id && a.kind === b.kind && a.key === b.key;

export function projectItemState(
  item: MarketplaceItemRef,
  globalInstalls: MarketplaceInstall[],
  project: Project,
): ProjectItemState {
  const own = project.marketplace_installs.find((i) => sameItem(i, item));
  const global = globalInstalls.find((i) => sameItem(i, item));
  if (own) {
    return global && global.commit !== own.commit ? "project_pinned_differently" : "project";
  }
  if (!global) return "none";
  return project.marketplace_disabled.some((d) => sameItem(d, item)) ? "opted_out" : "inherited";
}

/** Mirror of the backend's `effective_installs`, tagged with where each install comes from. */
export function effectiveInstalls(
  globalInstalls: MarketplaceInstall[],
  project: Project,
): (MarketplaceInstall & { source: "global" | "project" })[] {
  const byKey = new Map<string, MarketplaceInstall & { source: "global" | "project" }>();
  for (const g of globalInstalls) {
    if (project.marketplace_disabled.some((d) => sameItem(d, g))) continue;
    byKey.set(itemRefKey(g), { ...g, source: "global" });
  }
  for (const p of project.marketplace_installs) {
    byKey.set(itemRefKey(p), { ...p, source: "project" });
  }
  return [...byKey.entries()]
    .sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0))
    .map(([, v]) => v);
}

export function isStale(snapshot: MarketplaceSnapshot, now: number): boolean {
  if (!snapshot.fetched_at) return true;
  const at = Date.parse(snapshot.fetched_at);
  if (Number.isNaN(at)) return true;
  return now - at > STALE_AFTER_MS;
}
```

Note on sort order: the backend sorts by `(marketplace_id, kind, key)` with Rust enum order; the frontend sorts by the `itemRefKey` string. Both are only used for display, so they do not need to match.

- [ ] **Step 4: Run to verify it passes**

Run: `cd app && npx vitest run src/lib/marketplace.test.ts`
Expected: PASS (11 tests).

- [ ] **Step 5: Add the command wrappers**

Append to `app/src/lib/tauri-commands.ts`, and add the new names to the file's single `import type {…} from "./types"` line (`FileDiff, InstallScope, ItemUpdate, Marketplace, MarketplaceAccount, MarketplaceItemRef, MarketplaceSnapshot, ProjectSyncResult, SyncReport`):

```ts
// ---- Marketplace ----

export const listMarketplaceSnapshots = () =>
  invoke<MarketplaceSnapshot[]>("list_marketplace_snapshots");
export const refreshMarketplaces = (marketplaceId?: string) =>
  invoke<MarketplaceSnapshot[]>("refresh_marketplaces", { marketplaceId: marketplaceId ?? null });
export const addMarketplace = (
  name: string,
  url: string,
  branch: string | null,
  accountId: string | null,
) => invoke<MarketplaceSnapshot>("add_marketplace", { name, url, branch, accountId });
export const updateMarketplace = (marketplace: Marketplace) =>
  invoke<AppSettings>("update_marketplace", { marketplace });
export const removeMarketplace = (marketplaceId: string) =>
  invoke<AppSettings>("remove_marketplace", { marketplaceId });
export const installMarketplaceItem = (item: MarketplaceItemRef, scope: InstallScope) =>
  invoke<AppSettings>("install_marketplace_item", { item, scope });
export const uninstallMarketplaceItem = (item: MarketplaceItemRef, scope: InstallScope) =>
  invoke<void>("uninstall_marketplace_item", { item, scope });
export const setGlobalItemDisabled = (
  projectId: string,
  item: MarketplaceItemRef,
  disabled: boolean,
) => invoke<Project>("set_global_item_disabled", { projectId, item, disabled });
export const forgetMarketplaceInstalls = (marketplaceId: string) =>
  invoke<void>("forget_marketplace_installs", { marketplaceId });
export const listMarketplaceUpdates = () => invoke<ItemUpdate[]>("list_marketplace_updates");
export const marketplaceItemDiff = (
  item: MarketplaceItemRef,
  fromCommit: string,
  toCommit: string,
) => invoke<FileDiff[]>("marketplace_item_diff", { item, fromCommit, toCommit });
export const updateMarketplaceItem = (item: MarketplaceItemRef, scope: InstallScope) =>
  invoke<void>("update_marketplace_item", { item, scope });
export const applyMarketplaceNow = (projectId?: string) =>
  invoke<ProjectSyncResult[]>("apply_marketplace_now", { projectId: projectId ?? null });
export const getMarketplaceSyncReport = (projectId: string) =>
  invoke<SyncReport | null>("get_marketplace_sync_report", { projectId });
export const addMarketplaceTokenAccount = (label: string, host: string, token: string) =>
  invoke<MarketplaceAccount>("add_marketplace_token_account", { label, host, token });
export const addMarketplaceGhHostAccount = (label: string, host: string) =>
  invoke<MarketplaceAccount>("add_marketplace_gh_host_account", { label, host });
export const startMarketplaceGhContainerLogin = (label: string, host: string, projectId: string) =>
  invoke<MarketplaceAccount>("start_marketplace_gh_container_login", { label, host, projectId });
export const cancelMarketplaceGhLogin = () => invoke<void>("cancel_marketplace_gh_login");
export const testMarketplaceAccount = (accountId: string) =>
  invoke<string>("test_marketplace_account", { accountId });
export const removeMarketplaceAccount = (accountId: string) =>
  invoke<AppSettings>("remove_marketplace_account", { accountId });
export const marketplaceGhHostAvailable = () => invoke<boolean>("marketplace_gh_host_available");
```

`AppSettings` and `Project` are already imported in that file (used by `getSettings` and `updateProject`).

- [ ] **Step 6: Write failing store tests**

Append to `app/src/store/appState.test.ts` (and extend its import to `import { useAppState, homeTabKey, terminalTabKey, MARKETPLACE_TAB_KEY } from "./appState";`):

```ts
describe("marketplace tab", () => {
  beforeEach(() => {
    seed([A, B], A);
    useAppState.setState({ marketplaceFilterProjectId: null });
  });

  it("opens once, activates, and records the project filter", () => {
    useAppState.getState().openMarketplace("p9");
    useAppState.getState().openMarketplace("p9");
    const s = useAppState.getState();
    expect(s.tabOrder).toEqual([A, B, MARKETPLACE_TAB_KEY]);
    expect(s.activeTabKey).toBe(MARKETPLACE_TAB_KEY);
    expect(s.activeSessionId).toBeNull();
    expect(s.marketplaceFilterProjectId).toBe("p9");
  });

  it("clears the filter when opened without a project", () => {
    useAppState.getState().openMarketplace("p9");
    useAppState.getState().openMarketplace();
    expect(useAppState.getState().marketplaceFilterProjectId).toBeNull();
  });

  it("does not select a project when activated", () => {
    useAppState.getState().openMarketplace();
    useAppState.getState().setActiveTabKey(MARKETPLACE_TAB_KEY);
    expect(useAppState.getState().selectedProjectId).toBeNull();
  });

  it("closes and activates the neighbour", () => {
    useAppState.getState().openMarketplace();
    useAppState.getState().closeMarketplaceTab();
    const s = useAppState.getState();
    expect(s.tabOrder).toEqual([A, B]);
    expect(s.activeTabKey).toBe(B);
  });

  it("closing when not open is a no-op", () => {
    useAppState.getState().closeMarketplaceTab();
    expect(useAppState.getState().tabOrder).toEqual([A, B]);
  });
});
```

- [ ] **Step 7: Run to verify it fails**

Run: `cd app && npx vitest run src/store/appState.test.ts`
Expected: FAIL — `MARKETPLACE_TAB_KEY` is undefined / `openMarketplace is not a function`.

- [ ] **Step 8: Implement the store additions**

In `app/src/store/appState.ts`, directly after `export const tabKeyId = …` (line ~86), add:

```ts
/** The Marketplace view is a singleton main-area tab; its key has no id part. */
export const MARKETPLACE_TAB_KEY = "marketplace";
export const isMarketplaceTab = (key: string) => key === MARKETPLACE_TAB_KEY;
```

In the `AppState` interface, next to `closeHomeTab: (projectId: string) => void;`, add:

```ts
  /** Project the Marketplace view is filtered to, or null for all projects. */
  marketplaceFilterProjectId: string | null;
  setMarketplaceFilterProjectId: (projectId: string | null) => void;
  /** Open (or focus) the singleton Marketplace tab, optionally filtered to one project. */
  openMarketplace: (filterProjectId?: string | null) => void;
  closeMarketplaceTab: () => void;
```

In the `create<AppState>(…)` body, directly after the `closeHomeTab` implementation, add:

```ts
  marketplaceFilterProjectId: null,
  setMarketplaceFilterProjectId: (projectId) => set({ marketplaceFilterProjectId: projectId }),
  openMarketplace: (filterProjectId = null) =>
    set((state) => ({
      marketplaceFilterProjectId: filterProjectId,
      tabOrder: state.tabOrder.includes(MARKETPLACE_TAB_KEY)
        ? state.tabOrder
        : [...state.tabOrder, MARKETPLACE_TAB_KEY],
      ...activation(MARKETPLACE_TAB_KEY),
    })),
  closeMarketplaceTab: () =>
    set((state) => {
      const index = state.tabOrder.indexOf(MARKETPLACE_TAB_KEY);
      if (index === -1) return {};
      const tabOrder = state.tabOrder.filter((k) => k !== MARKETPLACE_TAB_KEY);
      const activeTabKey =
        state.activeTabKey === MARKETPLACE_TAB_KEY
          ? (tabOrder[Math.min(index, tabOrder.length - 1)] ?? null)
          : state.activeTabKey;
      return { tabOrder, ...activation(activeTabKey) };
    }),
```

`setActiveTabKey`, `cycleTab` and `focusTabIndex` need no change: they only set `selectedProjectId` for `isHomeTab` keys, and `"marketplace"` is not one.

- [ ] **Step 9: Run to verify it passes**

Run: `cd app && npx vitest run src/store/appState.test.ts`
Expected: PASS.

- [ ] **Step 10: Failing tests for the tab strip and Ctrl+Shift+W**

Append to `app/src/components/layout/MainTabs.test.tsx` (extend the store import with `MARKETPLACE_TAB_KEY`):

```ts
describe("marketplace tab", () => {
  beforeEach(() => {
    useAppState.setState({
      tabOrder: [HOME, MARKETPLACE_TAB_KEY],
      activeTabKey: MARKETPLACE_TAB_KEY,
      activeSessionId: null,
    });
  });

  it("renders a Marketplace tab that closes", () => {
    render(<MainTabs />);
    expect(screen.getByRole("tab", { name: /marketplace/i })).toHaveAttribute("aria-selected", "true");
    fireEvent.click(screen.getByRole("button", { name: "Close Marketplace tab" }));
    expect(useAppState.getState().tabOrder).toEqual([HOME]);
  });
});
```

Append to `app/src/hooks/useKeyboardShortcuts.test.tsx` (extend the store import with `MARKETPLACE_TAB_KEY`):

```ts
describe("Ctrl+Shift+W on the Marketplace tab", () => {
  it("closes the Marketplace tab", () => {
    useAppState.setState({
      tabOrder: [HOME, MARKETPLACE_TAB_KEY],
      activeTabKey: MARKETPLACE_TAB_KEY,
      activeSessionId: null,
    });
    renderHook(() => useKeyboardShortcuts());
    press("W", { shift: true });
    expect(useAppState.getState().tabOrder).toEqual([HOME]);
  });
});
```

Run: `cd app && npx vitest run src/components/layout/MainTabs.test.tsx src/hooks/useKeyboardShortcuts.test.tsx`
Expected: FAIL — no Marketplace tab is rendered (the key falls through to the session branch and returns `null`), and Ctrl+Shift+W calls `closeHomeTab("marketplace")`, which does nothing.

- [ ] **Step 11: Implement the tab strip, shortcut and dock changes**

`app/src/components/layout/MainTabs.tsx`:

1. Extend the store import:
```ts
import {
  useAppState,
  isHomeTab,
  isMarketplaceTab,
  tabKeyId,
  terminalTabKey,
} from "../../store/appState";
```
2. Add `closeMarketplaceTab` to the `useAppState(useShallow(...))` selector next to `closeHomeTab`:
```ts
  const { tabOrder, activeTabKey, setActiveTabKey, closeHomeTab, closeMarketplaceTab, moveTab } = useAppState(
    useShallow((s) => ({
      tabOrder: s.tabOrder,
      activeTabKey: s.activeTabKey,
      setActiveTabKey: s.setActiveTabKey,
      closeHomeTab: s.closeHomeTab,
      closeMarketplaceTab: s.closeMarketplaceTab,
      moveTab: s.moveTab,
    })),
  );
```
(Keep any other fields the existing selector already returns; only add `closeMarketplaceTab`.)
3. First line inside `tabLabel`:
```ts
    if (isMarketplaceTab(key)) return "Marketplace";
```
4. Ghost icon: `icon: isMarketplaceTab(drag.key) ? "◈" : isHomeTab(drag.key) ? "⌂" : "▣",`
5. First branch inside `renderTab`, before `if (isHomeTab(key))`:
```tsx
    if (isMarketplaceTab(key)) {
      return (
        <div
          role="tab"
          aria-selected={active}
          tabIndex={0}
          data-tab-index={index}
          onClick={() => activateTab(key)}
          onKeyDown={(e) => {
            if (e.key === "Enter" || e.key === " ") {
              e.preventDefault();
              setActiveTabKey(key);
            }
          }}
          {...pointerProps(key, false)}
          className={tabClass(active, dragKey === key)}
        >
          <span aria-hidden="true" className="text-[var(--text-secondary)]">◈</span>
          <span className="truncate max-w-[160px]">Marketplace</span>
          <button
            type="button"
            onClick={(e) => {
              e.stopPropagation();
              closeMarketplaceTab();
            }}
            aria-label="Close Marketplace tab"
            title="Close tab"
            className="w-6 h-6 flex items-center justify-center rounded-[var(--radius-control)] text-[var(--text-secondary)] hover:text-[var(--error)] hover:bg-[var(--bg-tertiary)] transition-colors"
          >
            <span aria-hidden="true">×</span>
          </button>
        </div>
      );
    }
```

`app/src/hooks/useKeyboardShortcuts.ts`: change the import to `import { useAppState, isMarketplaceTab, isTerminalTab, tabKeyId } from "../store/appState";`, and replace the `else` branch of the Ctrl+Shift+W handler with:
```ts
        } else if (isMarketplaceTab(key)) {
          state.closeMarketplaceTab();
        } else {
          state.closeHomeTab(tabKeyId(key));
        }
```

`app/src/components/layout/NotesDock.tsx`: behaviour is already right (for the Marketplace tab `projectId` stays `null`, so the dock shows its no-project state). Only replace the comment above `let projectId` with:
```ts
  // Follow whatever is in front: a home tab is its own project, a terminal tab
  // is the project it belongs to. The Marketplace tab belongs to no project.
```

Run: `cd app && npx vitest run src/components/layout/MainTabs.test.tsx src/hooks/useKeyboardShortcuts.test.tsx`
Expected: PASS.

- [ ] **Step 12: Failing test for `useMarketplace`**

`app/src/hooks/useMarketplace.test.ts`:

```ts
import { describe, it, expect, vi, beforeEach } from "vitest";
import { act, renderHook, waitFor } from "@testing-library/react";
import { useAppState } from "../store/appState";
import type { AppSettings, MarketplaceSnapshot } from "../lib/types";

const listMarketplaceSnapshots = vi.fn();
const refreshMarketplaces = vi.fn();
const listMarketplaceUpdates = vi.fn();
const getSettings = vi.fn();
const listProjects = vi.fn();
const installMarketplaceItem = vi.fn();

vi.mock("../lib/tauri-commands", () => ({
  listMarketplaceSnapshots: () => listMarketplaceSnapshots(),
  refreshMarketplaces: (id?: string) => refreshMarketplaces(id),
  listMarketplaceUpdates: () => listMarketplaceUpdates(),
  getSettings: () => getSettings(),
  listProjects: () => listProjects(),
  installMarketplaceItem: (...a: unknown[]) => installMarketplaceItem(...a),
}));

let syncHandler: ((e: { payload: unknown }) => void) | null = null;
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async (_name: string, cb: (e: { payload: unknown }) => void) => {
    syncHandler = cb;
    return vi.fn();
  }),
}));

import { useMarketplace, useMarketplaceSyncToasts } from "./useMarketplace";

const snap = (id: string, fetched_at: string | null): MarketplaceSnapshot => ({
  marketplace_id: id,
  head_commit: null,
  fetched_at,
  fetch_error: null,
  items: [],
});

describe("useMarketplace", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    useAppState.setState({ toasts: [], appSettings: { marketplaces: [] } as unknown as AppSettings });
    listMarketplaceUpdates.mockResolvedValue([]);
    getSettings.mockResolvedValue({ marketplaces: [] });
    listProjects.mockResolvedValue([]);
  });

  it("loads snapshots and refreshes only stale ones", async () => {
    const fresh = snap("m1", new Date().toISOString());
    const stale = snap("m2", null);
    listMarketplaceSnapshots.mockResolvedValue([fresh, stale]);
    refreshMarketplaces.mockResolvedValue([{ ...stale, fetched_at: new Date().toISOString() }]);

    const { result } = renderHook(() => useMarketplace());
    await act(() => result.current.load({ refreshStale: true }));

    expect(refreshMarketplaces).toHaveBeenCalledTimes(1);
    expect(refreshMarketplaces).toHaveBeenCalledWith("m2");
    expect(result.current.snapshots.map((s) => s.marketplace_id)).toEqual(["m1", "m2"]);
    expect(result.current.snapshots[1].fetched_at).not.toBeNull();
  });

  it("toasts and reloads after a failed mutation", async () => {
    listMarketplaceSnapshots.mockResolvedValue([]);
    installMarketplaceItem.mockRejectedValue("boom");
    const { result } = renderHook(() => useMarketplace());
    const ok = await act(() =>
      result.current.install({ marketplace_id: "m1", kind: "agent", key: "a" }, { type: "global" }),
    );
    expect(ok).toBe(false);
    expect(useAppState.getState().toasts[0]).toMatchObject({ kind: "error", detail: "boom" });
  });
});

describe("useMarketplaceSyncToasts", () => {
  beforeEach(() => {
    syncHandler = null;
    useAppState.setState({
      toasts: [],
      projects: [{ id: "p1", name: "api" }] as never,
    });
  });

  it("toasts a sync with errors and stays quiet on a clean one", async () => {
    renderHook(() => useMarketplaceSyncToasts());
    await waitFor(() => expect(syncHandler).not.toBeNull());

    act(() =>
      syncHandler!({
        payload: {
          project_id: "p1",
          report: { installed: ["agent:a"], updated: [], removed: [], skipped: [], errors: [], finished_at: "" },
        },
      }),
    );
    expect(useAppState.getState().toasts).toHaveLength(0);

    act(() =>
      syncHandler!({
        payload: {
          project_id: "p1",
          report: {
            installed: [],
            updated: [],
            removed: [],
            skipped: [{ item: "agent:a", reason: "a file you created has the same name" }],
            errors: ["claude plugin install failed"],
            finished_at: "",
          },
        },
      }),
    );
    const toast = useAppState.getState().toasts[0];
    expect(toast.kind).toBe("error");
    expect(toast.message).toContain("api");
    expect(toast.detail).toContain("claude plugin install failed");
    expect(toast.detail).toContain("agent:a");
  });
});
```

Run: `cd app && npx vitest run src/hooks/useMarketplace.test.ts`
Expected: FAIL — `Failed to resolve import "./useMarketplace"`.

- [ ] **Step 13: Implement `hooks/useMarketplace.ts`**

```ts
import { useCallback, useEffect, useState } from "react";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import * as commands from "../lib/tauri-commands";
import { useAppState } from "../store/appState";
import { isStale } from "../lib/marketplace";
import type {
  InstallScope,
  ItemUpdate,
  MarketplaceItemRef,
  MarketplaceSnapshot,
  SyncReport,
} from "../lib/types";

export interface MarketplaceApi {
  snapshots: MarketplaceSnapshot[];
  updates: ItemUpdate[];
  loading: boolean;
  /** Ids of marketplaces currently being fetched. */
  refreshing: string[];
  load: (opts?: { refreshStale?: boolean }) => Promise<void>;
  refresh: (marketplaceId?: string) => Promise<void>;
  /** Reload settings, projects and the update list after a mutation. */
  reloadState: () => Promise<void>;
  install: (item: MarketplaceItemRef, scope: InstallScope) => Promise<boolean>;
  uninstall: (item: MarketplaceItemRef, scope: InstallScope) => Promise<boolean>;
  setDisabled: (projectId: string, item: MarketplaceItemRef, disabled: boolean) => Promise<boolean>;
  update: (item: MarketplaceItemRef, scope: InstallScope) => Promise<boolean>;
  forget: (marketplaceId: string) => Promise<boolean>;
  remove: (marketplaceId: string) => Promise<boolean>;
}

function errorText(e: unknown): string {
  return typeof e === "string" ? e : e instanceof Error ? e.message : String(e);
}

export function useMarketplace(): MarketplaceApi {
  const setAppSettings = useAppState((s) => s.setAppSettings);
  const setProjects = useAppState((s) => s.setProjects);
  const pushToast = useAppState((s) => s.pushToast);
  const [snapshots, setSnapshots] = useState<MarketplaceSnapshot[]>([]);
  const [updates, setUpdates] = useState<ItemUpdate[]>([]);
  const [loading, setLoading] = useState(false);
  const [refreshing, setRefreshing] = useState<string[]>([]);

  const merge = useCallback((fresh: MarketplaceSnapshot[]) => {
    setSnapshots((prev) => {
      const byId = new Map(prev.map((s) => [s.marketplace_id, s]));
      for (const s of fresh) byId.set(s.marketplace_id, s);
      return [...byId.values()];
    });
  }, []);

  const loadUpdates = useCallback(async () => {
    try {
      setUpdates(await commands.listMarketplaceUpdates());
    } catch (e) {
      console.error("Failed to list marketplace updates:", e);
    }
  }, []);

  const refresh = useCallback(
    async (marketplaceId?: string) => {
      const ids = marketplaceId ? [marketplaceId] : snapshots.map((s) => s.marketplace_id);
      setRefreshing((r) => [...new Set([...r, ...ids])]);
      try {
        merge(await commands.refreshMarketplaces(marketplaceId));
        await loadUpdates();
      } catch (e) {
        pushToast({ kind: "error", message: "Could not refresh the marketplace", detail: errorText(e) });
      } finally {
        setRefreshing((r) => r.filter((id) => !ids.includes(id)));
      }
    },
    [snapshots, merge, loadUpdates, pushToast],
  );

  const load = useCallback(
    async (opts: { refreshStale?: boolean } = {}) => {
      setLoading(true);
      try {
        const list = await commands.listMarketplaceSnapshots();
        setSnapshots(list);
        await loadUpdates();
        if (opts.refreshStale) {
          const now = Date.now();
          const stale = list.filter((s) => isStale(s, now)).map((s) => s.marketplace_id);
          if (stale.length > 0) {
            setRefreshing(stale);
            try {
              // One call per marketplace so one slow or failing repo does not hold up the rest.
              await Promise.all(
                stale.map(async (id) => {
                  try {
                    merge(await commands.refreshMarketplaces(id));
                  } finally {
                    setRefreshing((r) => r.filter((x) => x !== id));
                  }
                }),
              );
            } finally {
              await loadUpdates();
            }
          }
        }
      } catch (e) {
        pushToast({ kind: "error", message: "Could not load marketplaces", detail: errorText(e) });
      } finally {
        setLoading(false);
      }
    },
    [merge, loadUpdates, pushToast],
  );

  const reloadState = useCallback(async () => {
    const [settings, projects] = await Promise.all([commands.getSettings(), commands.listProjects()]);
    setAppSettings(settings);
    setProjects(projects);
    await loadUpdates();
  }, [setAppSettings, setProjects, loadUpdates]);

  /** Run a mutation; on failure toast it. Always resync local state afterwards. */
  const mutate = useCallback(
    async (label: string, run: () => Promise<unknown>): Promise<boolean> => {
      let ok = true;
      try {
        await run();
      } catch (e) {
        ok = false;
        pushToast({ kind: "error", message: label, detail: errorText(e) });
      }
      try {
        await reloadState();
      } catch (e) {
        console.error("Failed to reload after marketplace change:", e);
      }
      return ok;
    },
    [reloadState, pushToast],
  );

  return {
    snapshots,
    updates,
    loading,
    refreshing,
    load,
    refresh,
    reloadState,
    install: (item, scope) =>
      mutate(`Could not install ${item.key}`, () => commands.installMarketplaceItem(item, scope)),
    uninstall: (item, scope) =>
      mutate(`Could not remove ${item.key}`, () => commands.uninstallMarketplaceItem(item, scope)),
    setDisabled: (projectId, item, disabled) =>
      mutate(`Could not change ${item.key} for this project`, () =>
        commands.setGlobalItemDisabled(projectId, item, disabled),
      ),
    update: (item, scope) =>
      mutate(`Could not update ${item.key}`, () => commands.updateMarketplaceItem(item, scope)),
    forget: (marketplaceId) =>
      mutate("Could not forget those installs", () => commands.forgetMarketplaceInstalls(marketplaceId)),
    remove: async (marketplaceId) => {
      const ok = await mutate("Could not remove the marketplace", () =>
        commands.removeMarketplace(marketplaceId),
      );
      if (ok) setSnapshots((prev) => prev.filter((s) => s.marketplace_id !== marketplaceId));
      return ok;
    },
  };
}

interface SyncFinishedEvent {
  project_id: string;
  report: SyncReport;
}

/**
 * App-wide: toast when a marketplace sync (container start or "Apply now")
 * reports errors or skipped items. A clean sync is silent.
 */
export function useMarketplaceSyncToasts() {
  useEffect(() => {
    let cancelled = false;
    let unlisten: UnlistenFn | null = null;
    void listen<SyncFinishedEvent>("marketplace-sync-finished", (event) => {
      const { project_id, report } = event.payload;
      if (report.errors.length === 0 && report.skipped.length === 0) return;
      const state = useAppState.getState();
      const name = state.projects.find((p) => p.id === project_id)?.name ?? project_id;
      const lines = [
        ...report.errors,
        ...report.skipped.map((s) => `${s.item}: ${s.reason}`),
      ];
      state.pushToast({
        kind: report.errors.length > 0 ? "error" : "info",
        message: `Marketplace sync for “${name}” ${report.errors.length > 0 ? "had errors" : "skipped items"}`,
        detail: lines.join("\n"),
        dedupeKey: `marketplace-sync-${project_id}`,
      });
    }).then((fn) => {
      if (cancelled) fn();
      else unlisten = fn;
    });
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, []);
}
```

The store exposes `setProjects: (projects: Project[]) => void` (`store/appState.ts:119`) and `projects`.

Run: `cd app && npx vitest run src/hooks/useMarketplace.test.ts`
Expected: PASS.

- [ ] **Step 14: Failing tests for the settings section and the view shell**

`app/src/components/settings/MarketplaceSettings.test.tsx`:

```tsx
import { describe, it, expect, vi, beforeEach } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import MarketplaceSettings from "./MarketplaceSettings";
import { useAppState, MARKETPLACE_TAB_KEY } from "../../store/appState";
import type { AppSettings } from "../../lib/types";

const listMarketplaceUpdates = vi.fn();
vi.mock("../../lib/tauri-commands", () => ({
  listMarketplaceUpdates: () => listMarketplaceUpdates(),
}));

describe("MarketplaceSettings", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    useAppState.setState({
      tabOrder: [],
      activeTabKey: null,
      appSettings: {
        marketplaces: [{ id: "m1", name: "Starter", url: "https://x/y.git", branch: null, account_id: null }],
        global_marketplace_installs: [
          { marketplace_id: "m1", kind: "agent", key: "a", commit: "a".repeat(40) },
          { marketplace_id: "m1", kind: "hook", key: "h", commit: "a".repeat(40) },
        ],
        marketplace_accounts: [],
      } as unknown as AppSettings,
    });
    listMarketplaceUpdates.mockResolvedValue([
      { item: { marketplace_id: "m1", kind: "agent", key: "a" }, pinned: "a".repeat(40), head: "b".repeat(40) },
    ]);
  });

  it("summarises and opens the Marketplace tab", async () => {
    render(<MarketplaceSettings />);
    expect(screen.getByTestId("marketplace-summary")).toHaveTextContent("1 marketplace");
    expect(screen.getByTestId("marketplace-summary")).toHaveTextContent("2 installed for all projects");
    await waitFor(() =>
      expect(screen.getByTestId("marketplace-summary")).toHaveTextContent("1 update available"),
    );
    fireEvent.click(screen.getByRole("button", { name: "Open Marketplace" }));
    expect(useAppState.getState().activeTabKey).toBe(MARKETPLACE_TAB_KEY);
  });
});
```

`app/src/components/marketplace/MarketplaceView.test.tsx`:

```tsx
import { describe, it, expect, vi, beforeEach } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";

const load = vi.fn(async () => {});
vi.mock("../../hooks/useMarketplace", () => ({
  useMarketplace: () => ({
    snapshots: [],
    updates: [],
    loading: false,
    refreshing: [],
    load,
    refresh: vi.fn(),
    reloadState: vi.fn(),
    install: vi.fn(),
    uninstall: vi.fn(),
    setDisabled: vi.fn(),
    update: vi.fn(),
    forget: vi.fn(),
    remove: vi.fn(),
  }),
}));
vi.mock("./BrowsePane", () => ({ default: () => <div>browse pane</div> }));
vi.mock("./InstalledPane", () => ({ default: () => <div>installed pane</div> }));
vi.mock("./AccountsPane", () => ({ default: () => <div>accounts pane</div> }));

import MarketplaceView from "./MarketplaceView";

describe("MarketplaceView", () => {
  beforeEach(() => vi.clearAllMocks());

  it("loads with stale refresh when first shown and switches sub-tabs", async () => {
    render(<MarketplaceView active />);
    await waitFor(() => expect(load).toHaveBeenCalledWith({ refreshStale: true }));
    expect(screen.getByText("browse pane")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("tab", { name: "Installed" }));
    expect(screen.getByText("installed pane")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("tab", { name: "Accounts" }));
    expect(screen.getByText("accounts pane")).toBeInTheDocument();
  });

  it("does not load while hidden", () => {
    render(<MarketplaceView active={false} />);
    expect(load).not.toHaveBeenCalled();
  });
});
```

Run: `cd app && npx vitest run src/components/settings/MarketplaceSettings.test.tsx src/components/marketplace/MarketplaceView.test.tsx`
Expected: FAIL — modules not found.

- [ ] **Step 15: Implement the settings section**

`app/src/components/settings/MarketplaceSettings.tsx`:

```tsx
import { useEffect, useState } from "react";
import { useAppState } from "../../store/appState";
import { listMarketplaceUpdates } from "../../lib/tauri-commands";
import Button from "../ui/Button";

const plural = (n: number, one: string, many: string) => `${n} ${n === 1 ? one : many}`;

export default function MarketplaceSettings() {
  const appSettings = useAppState((s) => s.appSettings);
  const openMarketplace = useAppState((s) => s.openMarketplace);
  const [updateCount, setUpdateCount] = useState<number | null>(null);

  useEffect(() => {
    let cancelled = false;
    listMarketplaceUpdates()
      .then((u) => {
        if (!cancelled) setUpdateCount(u.length);
      })
      .catch(() => {
        if (!cancelled) setUpdateCount(null);
      });
    return () => {
      cancelled = true;
    };
  }, [appSettings?.marketplaces.length]);

  const marketplaces = appSettings?.marketplaces.length ?? 0;
  const globalInstalls = appSettings?.global_marketplace_installs.length ?? 0;

  return (
    <div className="space-y-2">
      <p data-testid="marketplace-summary" className="text-xs text-[var(--text-secondary)] leading-snug">
        {plural(marketplaces, "marketplace", "marketplaces")} ·{" "}
        {globalInstalls} installed for all projects
        {updateCount !== null && updateCount > 0 && (
          <> · {plural(updateCount, "update available", "updates available")}</>
        )}
      </p>
      <p className="text-xs text-[var(--text-secondary)] leading-snug">
        Agents, skills, commands, hooks and plugins from git repositories, installed for all
        projects or per project. Changes apply to new Claude sessions.
      </p>
      <Button size="md" variant="secondary" onClick={() => openMarketplace()}>
        Open Marketplace
      </Button>
    </div>
  );
}
```

In `app/src/components/settings/SettingsPanel.tsx` add `import MarketplaceSettings from "./MarketplaceSettings";` next to the `SharedAuthSettings` import, and directly after the `claude-auth` `AccordionSection` (lines 170-172) add:

```tsx
      <AccordionSection id="marketplace" title="Marketplace" defaultOpen={false}>
        <MarketplaceSettings />
      </AccordionSection>
```

- [ ] **Step 16: Implement the view shell and wire it into `App.tsx`**

`app/src/components/marketplace/MarketplaceView.tsx`:

```tsx
import { useEffect, useRef, useState } from "react";
import { useMarketplace } from "../../hooks/useMarketplace";
import BrowsePane from "./BrowsePane";
import InstalledPane from "./InstalledPane";
import AccountsPane from "./AccountsPane";

const SUB_TABS = [
  { id: "browse", label: "Browse" },
  { id: "installed", label: "Installed" },
  { id: "accounts", label: "Accounts" },
] as const;

export type MarketplaceSubTab = (typeof SUB_TABS)[number]["id"];

interface Props {
  active: boolean;
}

export default function MarketplaceView({ active }: Props) {
  const mp = useMarketplace();
  const [tab, setTab] = useState<MarketplaceSubTab>("browse");
  const { load } = mp;
  const wasActive = useRef(false);

  // Load (and refresh stale marketplaces) each time the tab comes to the front.
  useEffect(() => {
    if (active && !wasActive.current) void load({ refreshStale: true });
    wasActive.current = active;
  }, [active, load]);

  return (
    <div className={`w-full h-full flex flex-col min-h-0 ${active ? "" : "hidden"}`}>
      <div
        role="tablist"
        aria-label="Marketplace sections"
        className="flex gap-1 px-3 pt-3 border-b border-[var(--border-color)]"
      >
        {SUB_TABS.map((t) => (
          <button
            key={t.id}
            type="button"
            role="tab"
            aria-selected={tab === t.id}
            onClick={() => setTab(t.id)}
            className={`px-3 py-1.5 text-xs rounded-t-[var(--radius-control)] ${
              tab === t.id
                ? "bg-[var(--bg-primary)] text-[var(--text-primary)]"
                : "text-[var(--text-secondary)] hover:text-[var(--text-primary)]"
            }`}
          >
            {t.label}
            {t.id === "installed" && mp.updates.length > 0 && (
              <span className="ml-1.5 px-1 rounded-[4px] text-[10px] bg-[var(--accent-muted)] text-[var(--accent)]">
                {mp.updates.length}
              </span>
            )}
          </button>
        ))}
      </div>
      <div className="flex-1 min-h-0 overflow-auto">
        {tab === "browse" && <BrowsePane mp={mp} />}
        {tab === "installed" && <InstalledPane mp={mp} />}
        {tab === "accounts" && <AccountsPane mp={mp} />}
      </div>
    </div>
  );
}
```

Create the three panes with minimal real content now; Tasks 13, 14 and 15 replace each file completely.

`app/src/components/marketplace/BrowsePane.tsx`:

```tsx
import type { MarketplaceApi } from "../../hooks/useMarketplace";

export default function BrowsePane({ mp }: { mp: MarketplaceApi }) {
  return (
    <p className="p-4 text-xs text-[var(--text-secondary)]">
      {mp.snapshots.length} marketplace{mp.snapshots.length === 1 ? "" : "s"} configured.
    </p>
  );
}
```

`app/src/components/marketplace/InstalledPane.tsx`:

```tsx
import type { MarketplaceApi } from "../../hooks/useMarketplace";

export default function InstalledPane({ mp }: { mp: MarketplaceApi }) {
  return (
    <p className="p-4 text-xs text-[var(--text-secondary)]">
      {mp.updates.length} update{mp.updates.length === 1 ? "" : "s"} available.
    </p>
  );
}
```

`app/src/components/marketplace/AccountsPane.tsx`:

```tsx
import type { MarketplaceApi } from "../../hooks/useMarketplace";
import { useAppState } from "../../store/appState";

export default function AccountsPane(_props: { mp: MarketplaceApi }) {
  const count = useAppState((s) => s.appSettings?.marketplace_accounts.length ?? 0);
  return (
    <p className="p-4 text-xs text-[var(--text-secondary)]">
      {count} account{count === 1 ? "" : "s"}.
    </p>
  );
}
```

`app/src/App.tsx`:
1. Change the store import (line 24) to `import { useAppState, isHomeTab, tabKeyId, homeTabKey, MARKETPLACE_TAB_KEY } from "./store/appState";`.
2. Add imports: `import MarketplaceView from "./components/marketplace/MarketplaceView";` and `import { useMarketplaceSyncToasts } from "./hooks/useMarketplace";`.
3. In the `App` component body next to the other hook calls (e.g. after `useKeyboardShortcuts()`), add `useMarketplaceSyncToasts();`.
4. Inside `<div className="w-full h-full">`, after the `sessions.map(...)` block, add:

```tsx
              {tabOrder.includes(MARKETPLACE_TAB_KEY) && (
                <PaneVisibilityProvider visible={activeTabKey === MARKETPLACE_TAB_KEY}>
                  <MarketplaceView active={activeTabKey === MARKETPLACE_TAB_KEY} />
                </PaneVisibilityProvider>
              )}
```

- [ ] **Step 17: Run the new and neighbouring tests**

Run: `cd app && npx vitest run src/components/settings src/components/marketplace src/components/layout src/hooks src/store src/lib src/test/capabilities.test.ts`
Expected: PASS. `capabilities.test.ts` passes only once Task 11 has added the `allow-*` grants for every new wrapper; if Task 11 is not merged yet, this test fails listing the new wrappers, which is expected at this point and must pass before committing Task 17.

- [ ] **Step 18: Type-check and commit**

Run: `cd app && npx tsc --noEmit -p .`
Expected: no errors.

```bash
cd /workspace/triple-c && git add app/src && git commit -qm "Marketplace UI plumbing: wrappers, singleton tab, settings section, view shell

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 13: Browse — marketplace list, item detail, install controls, hook confirm, add marketplace

**Files:**
- Replace: `app/src/components/marketplace/BrowsePane.tsx`
- Create: `app/src/components/marketplace/ItemDetail.tsx`
- Create: `app/src/components/marketplace/InstallControls.tsx`
- Create: `app/src/components/marketplace/HookConfirmModal.tsx`
- Create: `app/src/components/marketplace/AddMarketplaceModal.tsx`
- Test: `app/src/components/marketplace/BrowsePane.test.tsx`, `InstallControls.test.tsx`, `AddMarketplaceModal.test.tsx`

**Interfaces:**
- Consumes: `MarketplaceApi` (Task 12), `projectItemState`, `KIND_LABELS`, `KIND_ORDER`, `itemRefKey` (Task 12), `addMarketplace` wrapper, store `appSettings`, `projects`, `marketplaceFilterProjectId`, `setMarketplaceFilterProjectId`.
- Produces: `BrowsePane({ mp })` default export (same props as the Task 12 stub); `InstallControls({ mp, item, marketplaceId })`; `HookConfirmModal({ item, onConfirm, onCancel })`; `AddMarketplaceModal({ onClose, onAdded })`.

- [ ] **Step 1: Failing test for InstallControls**

`app/src/components/marketplace/InstallControls.test.tsx`:

```tsx
import { describe, it, expect, vi, beforeEach } from "vitest";
import { fireEvent, render, screen, within } from "@testing-library/react";
import InstallControls from "./InstallControls";
import { useAppState } from "../../store/appState";
import type { AppSettings, CatalogItem, Project } from "../../lib/types";
import type { MarketplaceApi } from "../../hooks/useMarketplace";

const C = "c".repeat(40);

function api(): MarketplaceApi {
  return {
    snapshots: [],
    updates: [],
    loading: false,
    refreshing: [],
    load: vi.fn(),
    refresh: vi.fn(),
    reloadState: vi.fn(),
    install: vi.fn(async () => true),
    uninstall: vi.fn(async () => true),
    setDisabled: vi.fn(async () => true),
    update: vi.fn(),
    forget: vi.fn(),
    remove: vi.fn(),
  };
}

const item = (kind: CatalogItem["kind"], patch: Partial<CatalogItem> = {}): CatalogItem => ({
  kind,
  key: "rev",
  name: "rev",
  description: "",
  path: `agents/rev.md`,
  invalid: null,
  hook_commands: kind === "hook" ? ["/home/claude/.claude/triple-c/hooks/rev/run.sh"] : [],
  preview: "",
  ...patch,
});

const project = (id: string, patch: Partial<Project> = {}) =>
  ({ id, name: `proj-${id}`, marketplace_installs: [], marketplace_disabled: [], ...patch }) as unknown as Project;

function seed(globalInstalls: AppSettings["global_marketplace_installs"], projects: Project[]) {
  useAppState.setState({
    appSettings: { global_marketplace_installs: globalInstalls, marketplaces: [], marketplace_accounts: [] } as unknown as AppSettings,
    projects,
    marketplaceFilterProjectId: null,
  });
}

const ref = { marketplace_id: "m1", kind: "agent" as const, key: "rev" };

describe("InstallControls", () => {
  beforeEach(() => seed([], [project("p1"), project("p2")]));

  it("installs for all projects", () => {
    const mp = api();
    render(<InstallControls mp={mp} item={item("agent")} marketplaceId="m1" />);
    fireEvent.click(screen.getByRole("switch", { name: "All projects" }));
    expect(mp.install).toHaveBeenCalledWith(ref, { type: "global" });
  });

  it("installs for one project", () => {
    const mp = api();
    render(<InstallControls mp={mp} item={item("agent")} marketplaceId="m1" />);
    fireEvent.click(screen.getByRole("checkbox", { name: /proj-p2/ }));
    expect(mp.install).toHaveBeenCalledWith(ref, { type: "project", project_id: "p2" });
  });

  it("opts a project out of a global install and back in", () => {
    const mp = api();
    seed([{ ...ref, commit: C }], [project("p1"), project("p2", { marketplace_disabled: [ref] })]);
    render(<InstallControls mp={mp} item={item("agent")} marketplaceId="m1" />);
    const row1 = screen.getByTestId("install-row-p1");
    expect(within(row1).getByText("Inherited")).toBeInTheDocument();
    fireEvent.click(within(row1).getByRole("checkbox"));
    expect(mp.setDisabled).toHaveBeenCalledWith("p1", ref, true);
    const row2 = screen.getByTestId("install-row-p2");
    expect(within(row2).getByText("Opted out")).toBeInTheDocument();
    fireEvent.click(within(row2).getByRole("checkbox"));
    expect(mp.setDisabled).toHaveBeenCalledWith("p2", ref, false);
  });

  it("removes a project-only install", () => {
    const mp = api();
    seed([], [project("p1", { marketplace_installs: [{ ...ref, commit: C }] })]);
    render(<InstallControls mp={mp} item={item("agent")} marketplaceId="m1" />);
    fireEvent.click(screen.getByRole("checkbox", { name: /proj-p1/ }));
    expect(mp.uninstall).toHaveBeenCalledWith(ref, { type: "project", project_id: "p1" });
  });

  it("requires confirmation before installing a hook", () => {
    const mp = api();
    render(<InstallControls mp={mp} item={item("hook")} marketplaceId="m1" />);
    fireEvent.click(screen.getByRole("switch", { name: "All projects" }));
    expect(mp.install).not.toHaveBeenCalled();
    expect(screen.getByText("/home/claude/.claude/triple-c/hooks/rev/run.sh")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Install hook" }));
    expect(mp.install).toHaveBeenCalledWith({ ...ref, kind: "hook" }, { type: "global" });
  });

  it("disables everything for an invalid item", () => {
    render(<InstallControls mp={api()} item={item("agent", { invalid: "bad front matter" })} marketplaceId="m1" />);
    expect(screen.getByRole("switch", { name: "All projects" })).toBeDisabled();
    expect(screen.getByRole("checkbox", { name: /proj-p1/ })).toBeDisabled();
  });

  it("shows only the filtered project when a filter is set", () => {
    useAppState.setState({ marketplaceFilterProjectId: "p2" });
    render(<InstallControls mp={api()} item={item("agent")} marketplaceId="m1" />);
    expect(screen.queryByTestId("install-row-p1")).not.toBeInTheDocument();
    expect(screen.getByTestId("install-row-p2")).toBeInTheDocument();
  });
});
```

`Toggle` renders `role="switch"` with `aria-label={label}` and `aria-checked` (`components/ui/Toggle.tsx:31-33`), so it is queried as a switch.

Run: `cd app && npx vitest run src/components/marketplace/InstallControls.test.tsx`
Expected: FAIL — module not found.

- [ ] **Step 2: Implement HookConfirmModal and InstallControls**

`app/src/components/marketplace/HookConfirmModal.tsx`:

```tsx
import Modal from "../ui/Modal";
import Button from "../ui/Button";
import type { CatalogItem } from "../../lib/types";

interface Props {
  item: CatalogItem;
  onConfirm: () => void;
  onCancel: () => void;
}

/** Hooks run shell commands in every Claude session, so installing one is always confirmed. */
export default function HookConfirmModal({ item, onConfirm, onCancel }: Props) {
  return (
    <Modal
      title={`Install hook “${item.name}”?`}
      description="This hook runs the commands below inside the container whenever its event fires."
      widthClassName="w-[40rem]"
      onClose={onCancel}
      footer={
        <>
          <Button size="md" variant="ghost" onClick={onCancel}>
            Cancel
          </Button>
          <Button size="md" variant="primary" onClick={onConfirm}>
            Install hook
          </Button>
        </>
      }
    >
      {item.hook_commands.length === 0 ? (
        <p className="text-xs text-[var(--text-secondary)]">This hook declares no commands.</p>
      ) : (
        <ul className="space-y-1">
          {item.hook_commands.map((c) => (
            <li key={c}>
              <code className="block font-mono text-xs break-all px-2 py-1 rounded-[var(--radius-control)] bg-[var(--bg-primary)] border border-[var(--border-color)]">
                {c}
              </code>
            </li>
          ))}
        </ul>
      )}
    </Modal>
  );
}
```

`app/src/components/marketplace/InstallControls.tsx`:

```tsx
import { useState } from "react";
import { useAppState } from "../../store/appState";
import { projectItemState, type ProjectItemState } from "../../lib/marketplace";
import type { MarketplaceApi } from "../../hooks/useMarketplace";
import type { CatalogItem, InstallScope, MarketplaceItemRef } from "../../lib/types";
import Toggle from "../ui/Toggle";
import HookConfirmModal from "./HookConfirmModal";

const STATE_LABEL: Record<ProjectItemState, string> = {
  none: "",
  inherited: "Inherited",
  opted_out: "Opted out",
  project: "This project",
  project_pinned_differently: "Pinned to a different commit",
};

interface Props {
  mp: MarketplaceApi;
  item: CatalogItem;
  marketplaceId: string;
}

export default function InstallControls({ mp, item, marketplaceId }: Props) {
  const appSettings = useAppState((s) => s.appSettings);
  const projects = useAppState((s) => s.projects);
  const filterId = useAppState((s) => s.marketplaceFilterProjectId);
  const [pendingHook, setPendingHook] = useState<InstallScope | null>(null);
  const [busy, setBusy] = useState(false);

  const ref: MarketplaceItemRef = { marketplace_id: marketplaceId, kind: item.kind, key: item.key };
  const globalInstalls = appSettings?.global_marketplace_installs ?? [];
  const isGlobal = globalInstalls.some(
    (g) => g.marketplace_id === marketplaceId && g.kind === item.kind && g.key === item.key,
  );
  const disabled = item.invalid !== null || busy;
  const shown = filterId ? projects.filter((p) => p.id === filterId) : projects;

  const run = async (fn: () => Promise<boolean>) => {
    setBusy(true);
    try {
      await fn();
    } finally {
      setBusy(false);
    }
  };

  /** Every install goes through here so a hook is always confirmed first. */
  const install = (scope: InstallScope) => {
    if (item.kind === "hook") {
      setPendingHook(scope);
      return;
    }
    void run(() => mp.install(ref, scope));
  };

  const toggleProject = (projectId: string, state: ProjectItemState) => {
    const scope: InstallScope = { type: "project", project_id: projectId };
    switch (state) {
      case "none":
        install(scope);
        break;
      case "inherited":
        void run(() => mp.setDisabled(projectId, ref, true));
        break;
      case "opted_out":
        void run(() => mp.setDisabled(projectId, ref, false));
        break;
      case "project":
      case "project_pinned_differently":
        void run(() => mp.uninstall(ref, scope));
        break;
    }
  };

  return (
    <div className="space-y-2">
      <Toggle
        label="All projects"
        checked={isGlobal}
        disabled={disabled}
        onChange={(v) => (v ? install({ type: "global" }) : void run(() => mp.uninstall(ref, { type: "global" })))}
      />
      <ul className="space-y-1">
        {shown.map((p) => {
          const state = projectItemState(ref, globalInstalls, p);
          const checked = state === "inherited" || state === "project" || state === "project_pinned_differently";
          return (
            <li
              key={p.id}
              data-testid={`install-row-${p.id}`}
              className="flex items-center justify-between gap-2 text-xs"
            >
              <label className="flex items-center gap-2 min-w-0">
                <input
                  type="checkbox"
                  checked={checked}
                  disabled={disabled}
                  onChange={() => toggleProject(p.id, state)}
                />
                <span className="truncate">{p.name}</span>
              </label>
              {STATE_LABEL[state] && (
                <span className="text-[var(--text-secondary)] whitespace-nowrap">{STATE_LABEL[state]}</span>
              )}
            </li>
          );
        })}
      </ul>
      {projects.length === 0 && (
        <p className="text-xs text-[var(--text-secondary)]">No projects yet — “All projects” also covers projects added later.</p>
      )}
      {pendingHook && (
        <HookConfirmModal
          item={item}
          onCancel={() => setPendingHook(null)}
          onConfirm={() => {
            const scope = pendingHook;
            setPendingHook(null);
            void run(() => mp.install(ref, scope));
          }}
        />
      )}
    </div>
  );
}
```

Run: `cd app && npx vitest run src/components/marketplace/InstallControls.test.tsx`
Expected: PASS.

- [ ] **Step 3: Failing tests for AddMarketplaceModal and BrowsePane**

`app/src/components/marketplace/AddMarketplaceModal.test.tsx`:

```tsx
import { describe, it, expect, vi, beforeEach } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { useAppState } from "../../store/appState";
import type { AppSettings } from "../../lib/types";

const addMarketplace = vi.fn();
vi.mock("../../lib/tauri-commands", () => ({
  addMarketplace: (...a: unknown[]) => addMarketplace(...a),
}));

import AddMarketplaceModal from "./AddMarketplaceModal";

describe("AddMarketplaceModal", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    useAppState.setState({
      appSettings: {
        marketplace_accounts: [{ id: "acc1", label: "Work", host: "github.com", method: "token", username: "me" }],
        marketplaces: [],
        global_marketplace_installs: [],
      } as unknown as AppSettings,
    });
  });

  it("submits name, url, branch and account", async () => {
    const onAdded = vi.fn();
    addMarketplace.mockResolvedValue({ marketplace_id: "m1", head_commit: null, fetched_at: null, fetch_error: null, items: [] });
    render(<AddMarketplaceModal onClose={vi.fn()} onAdded={onAdded} />);
    fireEvent.change(screen.getByLabelText("Name"), { target: { value: "Starter" } });
    fireEvent.change(screen.getByLabelText("Repository URL"), { target: { value: "https://github.com/shadowdao/triple-c-marketplace.git" } });
    fireEvent.change(screen.getByLabelText("Branch"), { target: { value: "" } });
    fireEvent.change(screen.getByLabelText("Account"), { target: { value: "acc1" } });
    fireEvent.click(screen.getByRole("button", { name: "Add marketplace" }));
    await waitFor(() => expect(onAdded).toHaveBeenCalled());
    expect(addMarketplace).toHaveBeenCalledWith("Starter", "https://github.com/shadowdao/triple-c-marketplace.git", null, "acc1");
  });

  it("rejects non-https URLs before calling the backend", () => {
    render(<AddMarketplaceModal onClose={vi.fn()} onAdded={vi.fn()} />);
    fireEvent.change(screen.getByLabelText("Name"), { target: { value: "x" } });
    fireEvent.change(screen.getByLabelText("Repository URL"), { target: { value: "git@github.com:a/b.git" } });
    expect(screen.getByRole("button", { name: "Add marketplace" })).toBeDisabled();
    expect(screen.getByText(/must start with https:\/\//)).toBeInTheDocument();
  });

  it("shows the backend error and stays open", async () => {
    addMarketplace.mockRejectedValue("Work cannot read this repository (HTTP 404)");
    render(<AddMarketplaceModal onClose={vi.fn()} onAdded={vi.fn()} />);
    fireEvent.change(screen.getByLabelText("Name"), { target: { value: "x" } });
    fireEvent.change(screen.getByLabelText("Repository URL"), { target: { value: "https://github.com/a/b.git" } });
    fireEvent.click(screen.getByRole("button", { name: "Add marketplace" }));
    expect(await screen.findByText(/HTTP 404/)).toBeInTheDocument();
  });
});
```

`app/src/components/marketplace/BrowsePane.test.tsx`:

```tsx
import { describe, it, expect, vi, beforeEach } from "vitest";
import { fireEvent, render, screen } from "@testing-library/react";
import { useAppState } from "../../store/appState";
import type { AppSettings, CatalogItem, MarketplaceSnapshot } from "../../lib/types";
import type { MarketplaceApi } from "../../hooks/useMarketplace";

vi.mock("./InstallControls", () => ({ default: () => <div>install controls</div> }));
vi.mock("./AddMarketplaceModal", () => ({ default: () => <div>add modal</div> }));

import BrowsePane from "./BrowsePane";

const it_ = (kind: CatalogItem["kind"], key: string, patch: Partial<CatalogItem> = {}): CatalogItem => ({
  kind,
  key,
  name: key,
  description: `${key} description`,
  path: key,
  invalid: null,
  hook_commands: [],
  preview: `${key} preview body`,
  ...patch,
});

const snapshot: MarketplaceSnapshot = {
  marketplace_id: "m1",
  head_commit: "a".repeat(40),
  fetched_at: "2026-09-27T12:00:00Z",
  fetch_error: "network unreachable",
  items: [it_("agent", "code-reviewer"), it_("hook", "notify-on-stop"), it_("skill", "broken", { invalid: "SKILL.md missing" })],
};

function api(patch: Partial<MarketplaceApi> = {}): MarketplaceApi {
  return {
    snapshots: [snapshot],
    updates: [],
    loading: false,
    refreshing: [],
    load: vi.fn(),
    refresh: vi.fn(),
    reloadState: vi.fn(),
    install: vi.fn(),
    uninstall: vi.fn(),
    setDisabled: vi.fn(),
    update: vi.fn(),
    forget: vi.fn(),
    remove: vi.fn(),
    ...patch,
  };
}

describe("BrowsePane", () => {
  beforeEach(() => {
    useAppState.setState({
      appSettings: {
        marketplaces: [{ id: "m1", name: "Starter", url: "https://github.com/s/m.git", branch: null, account_id: null }],
        marketplace_accounts: [],
        global_marketplace_installs: [],
      } as unknown as AppSettings,
      projects: [],
      marketplaceFilterProjectId: null,
    });
  });

  it("lists items, filters by kind and search, and shows detail", () => {
    render(<BrowsePane mp={api()} />);
    expect(screen.getByText("network unreachable")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /code-reviewer/ })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /notify-on-stop/ })).toBeInTheDocument();

    fireEvent.click(screen.getByRole("radio", { name: "Hooks" }));
    expect(screen.queryByRole("button", { name: /code-reviewer/ })).not.toBeInTheDocument();

    fireEvent.click(screen.getByRole("radio", { name: "All" }));
    fireEvent.change(screen.getByLabelText("Search items"), { target: { value: "review" } });
    expect(screen.queryByRole("button", { name: /notify-on-stop/ })).not.toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: /code-reviewer/ }));
    expect(screen.getByText("code-reviewer preview body")).toBeInTheDocument();
    expect(screen.getByText("install controls")).toBeInTheDocument();
  });

  it("shows why an item is invalid", () => {
    render(<BrowsePane mp={api()} />);
    fireEvent.click(screen.getByRole("button", { name: /broken/ }));
    expect(screen.getByText("SKILL.md missing")).toBeInTheDocument();
  });

  it("refreshes one marketplace", () => {
    const mp = api();
    render(<BrowsePane mp={mp} />);
    fireEvent.click(screen.getByRole("button", { name: "Refresh Starter" }));
    expect(mp.refresh).toHaveBeenCalledWith("m1");
  });

  it("offers Add when there are no marketplaces", () => {
    useAppState.setState({
      appSettings: { marketplaces: [], marketplace_accounts: [], global_marketplace_installs: [] } as unknown as AppSettings,
    });
    render(<BrowsePane mp={api({ snapshots: [] })} />);
    fireEvent.click(screen.getByRole("button", { name: "Add marketplace" }));
    expect(screen.getByText("add modal")).toBeInTheDocument();
  });
});
```

`SegmentedControl` renders a `role="radiogroup"` with one `role="radio"` per segment (`components/ui/SegmentedControl.tsx:51,78`).

Run: `cd app && npx vitest run src/components/marketplace/AddMarketplaceModal.test.tsx src/components/marketplace/BrowsePane.test.tsx`
Expected: FAIL — modules not found / stub BrowsePane lacks the list.

- [ ] **Step 4: Implement AddMarketplaceModal**

`app/src/components/marketplace/AddMarketplaceModal.tsx`:

```tsx
import { useState } from "react";
import Modal from "../ui/Modal";
import Button from "../ui/Button";
import Field, { inputClass, selectClass } from "../ui/Field";
import { addMarketplace } from "../../lib/tauri-commands";
import { useAppState } from "../../store/appState";
import type { MarketplaceSnapshot } from "../../lib/types";

interface Props {
  onClose: () => void;
  onAdded: (snapshot: MarketplaceSnapshot) => void;
}

export default function AddMarketplaceModal({ onClose, onAdded }: Props) {
  const accounts = useAppState((s) => s.appSettings?.marketplace_accounts ?? []);
  const [name, setName] = useState("");
  const [url, setUrl] = useState("");
  const [branch, setBranch] = useState("");
  const [accountId, setAccountId] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const trimmedUrl = url.trim();
  const urlProblem =
    trimmedUrl !== "" && !trimmedUrl.startsWith("https://")
      ? "The repository URL must start with https:// (SSH URLs are not supported)."
      : null;
  const canSubmit = name.trim() !== "" && trimmedUrl !== "" && !urlProblem && !busy;

  const submit = async () => {
    setBusy(true);
    setError(null);
    try {
      const snap = await addMarketplace(
        name.trim(),
        trimmedUrl,
        branch.trim() === "" ? null : branch.trim(),
        accountId === "" ? null : accountId,
      );
      onAdded(snap);
      onClose();
    } catch (e) {
      setError(typeof e === "string" ? e : String(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <Modal
      title="Add marketplace"
      description="Triple-C fetches the repository now to check it can be read. Nothing is saved if that fails."
      widthClassName="w-[36rem]"
      dismissible={!busy}
      onClose={onClose}
      footer={
        <>
          <Button size="md" variant="ghost" onClick={onClose} disabled={busy}>
            Cancel
          </Button>
          <Button size="md" variant="primary" onClick={() => void submit()} disabled={!canSubmit}>
            {busy ? "Checking…" : "Add marketplace"}
          </Button>
        </>
      }
    >
      <div className="space-y-3">
        <Field label="Name">
          {(id) => (
            <input id={id} value={name} onChange={(e) => setName(e.target.value)} className={inputClass} placeholder="Team marketplace" />
          )}
        </Field>
        <Field label="Repository URL" hint={urlProblem ?? "HTTPS clone URL, e.g. https://github.com/owner/repo.git"}>
          {(id) => (
            <input id={id} value={url} onChange={(e) => setUrl(e.target.value)} className={inputClass} placeholder="https://github.com/owner/repo.git" />
          )}
        </Field>
        <Field label="Branch" hint="Leave empty to use the repository's default branch.">
          {(id) => (
            <input id={id} value={branch} onChange={(e) => setBranch(e.target.value)} className={inputClass} placeholder="main" />
          )}
        </Field>
        <Field label="Account" hint="Needed for private repositories. Add accounts on the Accounts tab.">
          {(id) => (
            <select id={id} value={accountId} onChange={(e) => setAccountId(e.target.value)} className={selectClass}>
              <option value="">None (public repository)</option>
              {accounts.map((a) => (
                <option key={a.id} value={a.id}>
                  {a.label} — {a.host}
                  {a.username ? ` (${a.username})` : ""}
                </option>
              ))}
            </select>
          )}
        </Field>
        {error && (
          <p role="alert" className="text-xs text-[var(--error)] whitespace-pre-wrap leading-snug">
            {error}
          </p>
        )}
      </div>
    </Modal>
  );
}
```

`Field` renders its `hint` below the control; the URL problem is shown there so the test can find it by text.

- [ ] **Step 5: Implement ItemDetail and BrowsePane**

`app/src/components/marketplace/ItemDetail.tsx`:

```tsx
import type { CatalogItem } from "../../lib/types";
import type { MarketplaceApi } from "../../hooks/useMarketplace";
import { KIND_LABELS } from "../../lib/marketplace";
import StatusIndicator from "../ui/StatusIndicator";
import InstallControls from "./InstallControls";

interface Props {
  mp: MarketplaceApi;
  item: CatalogItem;
  marketplaceId: string;
}

export default function ItemDetail({ mp, item, marketplaceId }: Props) {
  return (
    <div className="space-y-3">
      <div>
        <p className="text-[10px] uppercase tracking-wide text-[var(--text-secondary)]">
          {KIND_LABELS[item.kind].replace(/s$/, "")} · <code className="font-mono">{item.path}</code>
        </p>
        <h3 className="text-sm font-medium text-[var(--text-primary)]">{item.name}</h3>
        {item.description && <p className="text-xs text-[var(--text-secondary)] leading-snug">{item.description}</p>}
      </div>
      {item.invalid && (
        <div className="rounded-[var(--radius-control)] border border-[var(--error)]/40 bg-[var(--error-muted)] p-2">
          <StatusIndicator tone="error" label="Cannot be installed" className="text-xs" />
          <p className="mt-1 text-xs text-[var(--text-secondary)]">{item.invalid}</p>
        </div>
      )}
      {item.kind === "hook" && item.hook_commands.length > 0 && (
        <div>
          <p className="text-xs font-medium mb-1">Commands this hook runs</p>
          <ul className="space-y-1">
            {item.hook_commands.map((c) => (
              <li key={c}>
                <code className="block font-mono text-xs break-all">{c}</code>
              </li>
            ))}
          </ul>
        </div>
      )}
      {item.preview && (
        <pre className="max-h-80 overflow-auto p-2 text-xs font-mono whitespace-pre-wrap rounded-[var(--radius-control)] bg-[var(--bg-primary)] border border-[var(--border-color)]">
          {item.preview}
        </pre>
      )}
      <div>
        <p className="text-xs font-medium mb-1">Install</p>
        <InstallControls mp={mp} item={item} marketplaceId={marketplaceId} />
        <p className="mt-2 text-[11px] text-[var(--text-secondary)]">
          Running containers pick changes up on their next start or with “Apply now” on the Installed tab. Changes
          apply to new Claude sessions.
        </p>
      </div>
    </div>
  );
}
```

`app/src/components/marketplace/BrowsePane.tsx` (replaces the Task 12 stub):

```tsx
import { useMemo, useState } from "react";
import type { MarketplaceApi } from "../../hooks/useMarketplace";
import { useAppState } from "../../store/appState";
import { KIND_LABELS, KIND_ORDER, itemRefKey } from "../../lib/marketplace";
import type { CatalogItem, ItemKind } from "../../lib/types";
import Button from "../ui/Button";
import SegmentedControl from "../ui/SegmentedControl";
import { inputClass, selectClass } from "../ui/Field";
import AddMarketplaceModal from "./AddMarketplaceModal";
import ItemDetail from "./ItemDetail";

type KindFilter = ItemKind | "all";

const when = (iso: string | null) => (iso ? new Date(iso).toLocaleString() : "never");

export default function BrowsePane({ mp }: { mp: MarketplaceApi }) {
  const marketplaces = useAppState((s) => s.appSettings?.marketplaces ?? []);
  const projects = useAppState((s) => s.projects);
  const filterId = useAppState((s) => s.marketplaceFilterProjectId);
  const setFilterId = useAppState((s) => s.setMarketplaceFilterProjectId);
  const [kind, setKind] = useState<KindFilter>("all");
  const [query, setQuery] = useState("");
  const [selected, setSelected] = useState<{ marketplaceId: string; item: CatalogItem } | null>(null);
  const [adding, setAdding] = useState(false);

  const rows = useMemo(() => {
    const q = query.trim().toLowerCase();
    return mp.snapshots.flatMap((snap) =>
      snap.items
        .filter((i) => kind === "all" || i.kind === kind)
        .filter((i) => q === "" || `${i.name} ${i.key} ${i.description}`.toLowerCase().includes(q))
        .sort((a, b) => KIND_ORDER.indexOf(a.kind) - KIND_ORDER.indexOf(b.kind) || a.name.localeCompare(b.name))
        .map((item) => ({ marketplaceId: snap.marketplace_id, item })),
    );
  }, [mp.snapshots, kind, query]);

  const nameOf = (id: string) => marketplaces.find((m) => m.id === id)?.name ?? id;

  return (
    <div className="flex h-full min-h-0">
      <aside className="w-64 flex-shrink-0 border-r border-[var(--border-color)] p-3 space-y-3 overflow-auto">
        <div className="flex items-center justify-between">
          <h2 className="text-xs font-medium">Marketplaces</h2>
          <Button size="sm" variant="secondary" onClick={() => setAdding(true)}>
            Add marketplace
          </Button>
        </div>
        {marketplaces.length === 0 && (
          <p className="text-xs text-[var(--text-secondary)]">No marketplaces yet. Add a git repository to browse its items.</p>
        )}
        {marketplaces.map((m) => {
          const snap = mp.snapshots.find((s) => s.marketplace_id === m.id);
          const refreshing = mp.refreshing.includes(m.id);
          return (
            <div key={m.id} className="space-y-1 text-xs">
              <div className="flex items-center justify-between gap-2">
                <span className="font-medium truncate" title={m.url}>
                  {m.name}
                </span>
                <Button
                  size="sm"
                  variant="ghost"
                  aria-label={`Refresh ${m.name}`}
                  disabled={refreshing}
                  onClick={() => void mp.refresh(m.id)}
                >
                  {refreshing ? "…" : "↻"}
                </Button>
              </div>
              <p className="text-[var(--text-secondary)]">Last fetched {when(snap?.fetched_at ?? null)}</p>
              {snap?.fetch_error && (
                <p className="text-[var(--error)] whitespace-pre-wrap leading-snug">{snap.fetch_error}</p>
              )}
            </div>
          );
        })}
        {projects.length > 0 && (
          <label className="block text-xs space-y-1">
            <span className="text-[var(--text-secondary)]">Show install state for</span>
            <select
              value={filterId ?? ""}
              onChange={(e) => setFilterId(e.target.value === "" ? null : e.target.value)}
              className={selectClass}
            >
              <option value="">All projects</option>
              {projects.map((p) => (
                <option key={p.id} value={p.id}>
                  {p.name}
                </option>
              ))}
            </select>
          </label>
        )}
      </aside>

      <section className="w-80 flex-shrink-0 border-r border-[var(--border-color)] p-3 space-y-2 overflow-auto">
        <SegmentedControl<KindFilter>
          label="Item kind"
          value={kind}
          onChange={setKind}
          segments={[
            { value: "all", label: "All" },
            ...KIND_ORDER.map((k) => ({ value: k as KindFilter, label: KIND_LABELS[k] })),
          ]}
        />
        <input
          aria-label="Search items"
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          placeholder="Search"
          className={inputClass}
        />
        <ul className="space-y-1">
          {rows.map(({ marketplaceId, item }) => {
            const key = itemRefKey({ marketplace_id: marketplaceId, kind: item.kind, key: item.key });
            const isSel =
              selected?.marketplaceId === marketplaceId &&
              selected.item.kind === item.kind &&
              selected.item.key === item.key;
            return (
              <li key={key}>
                <button
                  type="button"
                  onClick={() => setSelected({ marketplaceId, item })}
                  className={`w-full text-left px-2 py-1.5 rounded-[var(--radius-control)] text-xs ${
                    isSel ? "bg-[var(--bg-tertiary)]" : "hover:bg-[var(--bg-tertiary)]"
                  }`}
                >
                  <span className="font-medium">{item.name}</span>
                  <span className="ml-1 text-[var(--text-secondary)]">{KIND_LABELS[item.kind].replace(/s$/, "").toLowerCase()}</span>
                  {item.invalid && <span className="ml-1 text-[var(--error)]">invalid</span>}
                  {mp.snapshots.length > 1 && (
                    <span className="block text-[var(--text-secondary)]">{nameOf(marketplaceId)}</span>
                  )}
                  {item.description && (
                    <span className="block text-[var(--text-secondary)] truncate">{item.description}</span>
                  )}
                </button>
              </li>
            );
          })}
          {rows.length === 0 && mp.snapshots.length > 0 && (
            <li className="text-xs text-[var(--text-secondary)]">No items match.</li>
          )}
        </ul>
      </section>

      <section className="flex-1 min-w-0 p-4 overflow-auto">
        {selected ? (
          <ItemDetail mp={mp} item={selected.item} marketplaceId={selected.marketplaceId} />
        ) : (
          <p className="text-xs text-[var(--text-secondary)]">Select an item to see what it contains and install it.</p>
        )}
      </section>

      {adding && (
        <AddMarketplaceModal
          onClose={() => setAdding(false)}
          onAdded={() => {
            void mp.reloadState();
            void mp.load();
          }}
        />
      )}
    </div>
  );
}
```

The item buttons include the description in their accessible name; the test's `/code-reviewer/` regexes match it. The selected item's `CatalogItem` is a copy from the snapshot at selection time; after a refresh, re-selecting shows the new data (acceptable, the install controls read install state live from the store).

- [ ] **Step 6: Run to verify they pass**

Run: `cd app && npx vitest run src/components/marketplace`
Expected: PASS (InstallControls, AddMarketplaceModal, BrowsePane, MarketplaceView).

- [ ] **Step 7: Type-check and commit**

Run: `cd app && npx tsc --noEmit -p .`
Expected: no errors.

```bash
cd /workspace/triple-c && git add app/src/components/marketplace && git commit -qm "Marketplace UI: browse, item detail, install controls, hook confirmation, add marketplace

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 14: Installed — install list, updates with diff, source removed, Apply now

**Files:**
- Replace: `app/src/components/marketplace/InstalledPane.tsx`
- Create: `app/src/components/marketplace/UpdateDiffModal.tsx`
- Test: `app/src/components/marketplace/InstalledPane.test.tsx`, `UpdateDiffModal.test.tsx`

**Interfaces:**
- Consumes: `MarketplaceApi` (`updates`, `snapshots`, `update`, `uninstall`, `forget`), wrappers `marketplaceItemDiff`, `applyMarketplaceNow`, `effectiveInstalls`/`itemRefKey`/`formatItemRef`/`KIND_LABELS` (Task 12), store `appSettings`, `projects`, `pushToast`.
- Produces: `InstalledPane({ mp })`, `UpdateDiffModal({ update, scope, onClose, onAccept })`.

Update semantics: an `ItemUpdate` is per item (`MarketplaceItemRef` + pinned + head). An item can be installed in several scopes with different pins, so the Installed tab lists one row per install (scope) and shows the badge on each row whose own `commit` differs from the update's `head` for that item. Accepting updates that one install via `updateMarketplaceItem(item, scope)`.

- [ ] **Step 1: Failing test for UpdateDiffModal**

`app/src/components/marketplace/UpdateDiffModal.test.tsx`:

```tsx
import { describe, it, expect, vi, beforeEach } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";

const marketplaceItemDiff = vi.fn();
vi.mock("../../lib/tauri-commands", () => ({
  marketplaceItemDiff: (...a: unknown[]) => marketplaceItemDiff(...a),
}));

import UpdateDiffModal from "./UpdateDiffModal";

const A = "a".repeat(40);
const B = "b".repeat(40);
const item = { marketplace_id: "m1", kind: "hook" as const, key: "notify" };

describe("UpdateDiffModal", () => {
  beforeEach(() => vi.clearAllMocks());

  it("loads the diff from the install's pin to head and accepts", async () => {
    marketplaceItemDiff.mockResolvedValue([
      { path: "notify.sh", change: "modified", unified: "-echo old\n+echo new\n" },
      { path: "icon.png", change: "added", unified: null },
    ]);
    const onAccept = vi.fn(async () => true);
    render(<UpdateDiffModal item={item} fromCommit={A} toCommit={B} scopeLabel="All projects" onClose={vi.fn()} onAccept={onAccept} />);
    await waitFor(() => expect(marketplaceItemDiff).toHaveBeenCalledWith(item, A, B));
    expect(screen.getByText(/\+echo new/)).toBeInTheDocument();
    expect(screen.getByText("Binary file — no text diff")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Update" }));
    await waitFor(() => expect(onAccept).toHaveBeenCalled());
  });

  it("shows a load error and keeps Update disabled", async () => {
    marketplaceItemDiff.mockRejectedValue("commit not in cache");
    render(<UpdateDiffModal item={item} fromCommit={A} toCommit={B} scopeLabel="p" onClose={vi.fn()} onAccept={vi.fn()} />);
    expect(await screen.findByText(/commit not in cache/)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Update" })).toBeDisabled();
  });
});
```

Run: `cd app && npx vitest run src/components/marketplace/UpdateDiffModal.test.tsx`
Expected: FAIL — module not found.

- [ ] **Step 2: Implement UpdateDiffModal**

`app/src/components/marketplace/UpdateDiffModal.tsx`:

```tsx
import { useEffect, useState } from "react";
import Modal from "../ui/Modal";
import Button from "../ui/Button";
import { marketplaceItemDiff } from "../../lib/tauri-commands";
import { formatItemRef } from "../../lib/marketplace";
import type { FileDiff, MarketplaceItemRef } from "../../lib/types";

interface Props {
  item: MarketplaceItemRef;
  fromCommit: string;
  toCommit: string;
  scopeLabel: string;
  onClose: () => void;
  /** Resolves true when the update was applied. */
  onAccept: () => Promise<boolean>;
}

const CHANGE_LABEL: Record<FileDiff["change"], string> = {
  added: "added",
  removed: "removed",
  modified: "modified",
};

export default function UpdateDiffModal({ item, fromCommit, toCommit, scopeLabel, onClose, onAccept }: Props) {
  const [diffs, setDiffs] = useState<FileDiff[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    let cancelled = false;
    marketplaceItemDiff(item, fromCommit, toCommit)
      .then((d) => {
        if (!cancelled) setDiffs(d);
      })
      .catch((e) => {
        if (!cancelled) setError(typeof e === "string" ? e : String(e));
      });
    return () => {
      cancelled = true;
    };
  }, [item, fromCommit, toCommit]);

  const accept = async () => {
    setBusy(true);
    try {
      if (await onAccept()) onClose();
    } finally {
      setBusy(false);
    }
  };

  return (
    <Modal
      title={`Update ${formatItemRef(item)}`}
      description={`${scopeLabel}: ${fromCommit.slice(0, 8)} → ${toCommit.slice(0, 8)}. Review the changes before accepting.`}
      widthClassName="w-[52rem]"
      dismissible={!busy}
      onClose={onClose}
      footer={
        <>
          <Button size="md" variant="ghost" onClick={onClose} disabled={busy}>
            Cancel
          </Button>
          <Button size="md" variant="primary" onClick={() => void accept()} disabled={busy || diffs === null}>
            Update
          </Button>
        </>
      }
    >
      {error && <p role="alert" className="text-xs text-[var(--error)]">{error}</p>}
      {!error && diffs === null && <p className="text-xs text-[var(--text-secondary)]">Loading changes…</p>}
      {diffs && diffs.length === 0 && (
        <p className="text-xs text-[var(--text-secondary)]">No file changes (only the catalog entry changed).</p>
      )}
      {diffs && diffs.length > 0 && (
        <div className="space-y-3 max-h-[60vh] overflow-auto">
          {diffs.map((d) => (
            <div key={d.path}>
              <p className="text-xs font-mono mb-1">
                {d.path} <span className="text-[var(--text-secondary)]">({CHANGE_LABEL[d.change]})</span>
              </p>
              {d.unified === null ? (
                <p className="text-xs text-[var(--text-secondary)]">Binary file — no text diff</p>
              ) : (
                <pre className="p-2 text-xs font-mono whitespace-pre overflow-auto rounded-[var(--radius-control)] bg-[var(--bg-primary)] border border-[var(--border-color)]">
                  {d.unified}
                </pre>
              )}
            </div>
          ))}
        </div>
      )}
    </Modal>
  );
}
```

Run: `cd app && npx vitest run src/components/marketplace/UpdateDiffModal.test.tsx`
Expected: PASS.

- [ ] **Step 3: Failing test for InstalledPane**

`app/src/components/marketplace/InstalledPane.test.tsx`:

```tsx
import { describe, it, expect, vi, beforeEach } from "vitest";
import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { useAppState } from "../../store/appState";
import type { AppSettings, Project } from "../../lib/types";
import type { MarketplaceApi } from "../../hooks/useMarketplace";

const applyMarketplaceNow = vi.fn();
vi.mock("../../lib/tauri-commands", () => ({
  applyMarketplaceNow: (id?: string) => applyMarketplaceNow(id),
}));
vi.mock("./UpdateDiffModal", () => ({
  default: ({ onAccept }: { onAccept: () => Promise<boolean> }) => (
    <button onClick={() => void onAccept()}>accept diff</button>
  ),
}));

import InstalledPane from "./InstalledPane";

const A = "a".repeat(40);
const B = "b".repeat(40);

function api(patch: Partial<MarketplaceApi> = {}): MarketplaceApi {
  return {
    snapshots: [],
    updates: [],
    loading: false,
    refreshing: [],
    load: vi.fn(),
    refresh: vi.fn(),
    reloadState: vi.fn(),
    install: vi.fn(),
    uninstall: vi.fn(async () => true),
    setDisabled: vi.fn(),
    update: vi.fn(async () => true),
    forget: vi.fn(async () => true),
    remove: vi.fn(),
    ...patch,
  };
}

describe("InstalledPane", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    useAppState.setState({
      toasts: [],
      appSettings: {
        marketplaces: [{ id: "m1", name: "Starter", url: "https://x/y.git", branch: null, account_id: null }],
        marketplace_accounts: [],
        global_marketplace_installs: [
          { marketplace_id: "m1", kind: "agent", key: "rev", commit: A },
          { marketplace_id: "gone", kind: "skill", key: "old", commit: A },
        ],
      } as unknown as AppSettings,
      projects: [
        {
          id: "p1",
          name: "api",
          status: "running",
          marketplace_installs: [{ marketplace_id: "m1", kind: "command", key: "cmd", commit: B }],
          marketplace_disabled: [],
        },
      ] as unknown as Project[],
    });
  });

  it("lists global and project installs", () => {
    render(<InstalledPane mp={api()} />);
    const global = screen.getByTestId("installed-global");
    expect(within(global).getByText("rev")).toBeInTheDocument();
    const proj = screen.getByTestId("installed-project-p1");
    expect(within(proj).getByText("cmd")).toBeInTheDocument();
  });

  it("badges and accepts an update for the matching install", async () => {
    const mp = api({
      updates: [{ item: { marketplace_id: "m1", kind: "agent", key: "rev" }, pinned: A, head: B }],
    });
    render(<InstalledPane mp={mp} />);
    fireEvent.click(screen.getByRole("button", { name: "Review update for rev" }));
    fireEvent.click(screen.getByRole("button", { name: "accept diff" }));
    await waitFor(() =>
      expect(mp.update).toHaveBeenCalledWith({ marketplace_id: "m1", kind: "agent", key: "rev" }, { type: "global" }),
    );
  });

  it("marks installs whose marketplace was removed and forgets them", () => {
    const mp = api();
    render(<InstalledPane mp={mp} />);
    expect(screen.getByText("Source removed")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Forget installs from removed marketplaces" }));
    expect(mp.forget).toHaveBeenCalledWith("gone");
  });

  it("removes a project install", () => {
    const mp = api();
    render(<InstalledPane mp={mp} />);
    fireEvent.click(screen.getByRole("button", { name: "Remove cmd from api" }));
    expect(mp.uninstall).toHaveBeenCalledWith(
      { marketplace_id: "m1", kind: "command", key: "cmd" },
      { type: "project", project_id: "p1" },
    );
  });

  it("applies now and summarises the result", async () => {
    applyMarketplaceNow.mockResolvedValue([
      { project_id: "p1", report: { installed: ["agent:rev"], updated: [], removed: [], skipped: [], errors: [], finished_at: "" } },
    ]);
    render(<InstalledPane mp={api()} />);
    fireEvent.click(screen.getByRole("button", { name: "Apply now" }));
    await waitFor(() => expect(applyMarketplaceNow).toHaveBeenCalledWith(undefined));
    await waitFor(() => expect(useAppState.getState().toasts[0]).toMatchObject({ kind: "success" }));
    expect(useAppState.getState().toasts[0].message).toContain("1 running project");
  });
});
```

Run: `cd app && npx vitest run src/components/marketplace/InstalledPane.test.tsx`
Expected: FAIL — stub pane has no lists.

- [ ] **Step 4: Implement InstalledPane**

`app/src/components/marketplace/InstalledPane.tsx` (replaces the Task 12 stub):

```tsx
import { useState } from "react";
import type { MarketplaceApi } from "../../hooks/useMarketplace";
import { useAppState } from "../../store/appState";
import { KIND_LABELS } from "../../lib/marketplace";
import { applyMarketplaceNow } from "../../lib/tauri-commands";
import type { InstallScope, ItemUpdate, MarketplaceInstall } from "../../lib/types";
import Button from "../ui/Button";
import UpdateDiffModal from "./UpdateDiffModal";

interface Pending {
  install: MarketplaceInstall;
  update: ItemUpdate;
  scope: InstallScope;
  scopeLabel: string;
}

export default function InstalledPane({ mp }: { mp: MarketplaceApi }) {
  const appSettings = useAppState((s) => s.appSettings);
  const projects = useAppState((s) => s.projects);
  const pushToast = useAppState((s) => s.pushToast);
  const [pending, setPending] = useState<Pending | null>(null);
  const [applying, setApplying] = useState(false);

  const marketplaces = appSettings?.marketplaces ?? [];
  const known = new Set(marketplaces.map((m) => m.id));
  const nameOf = (id: string) => marketplaces.find((m) => m.id === id)?.name ?? id;
  const globalInstalls = appSettings?.global_marketplace_installs ?? [];

  const updateFor = (i: MarketplaceInstall) =>
    mp.updates.find(
      (u) =>
        u.item.marketplace_id === i.marketplace_id &&
        u.item.kind === i.kind &&
        u.item.key === i.key &&
        u.head !== i.commit,
    );

  const removedSources = [
    ...new Set(
      [...globalInstalls, ...projects.flatMap((p) => p.marketplace_installs)]
        .map((i) => i.marketplace_id)
        .filter((id) => !known.has(id)),
    ),
  ];

  const applyNow = async () => {
    setApplying(true);
    try {
      const results = await applyMarketplaceNow(undefined);
      const failed = results.filter((r) => r.report.errors.length > 0);
      if (results.length === 0) {
        pushToast({ kind: "info", message: "No running projects — changes apply when a project starts." });
      } else if (failed.length === 0) {
        pushToast({
          kind: "success",
          message: `Marketplace applied to ${results.length} running project${results.length === 1 ? "" : "s"}. New Claude sessions will use it.`,
        });
      } else {
        pushToast({
          kind: "error",
          message: `Marketplace sync failed for ${failed.length} of ${results.length} running projects`,
          detail: failed.flatMap((r) => r.report.errors).join("\n"),
        });
      }
    } catch (e) {
      pushToast({ kind: "error", message: "Could not apply marketplace changes", detail: String(e) });
    } finally {
      setApplying(false);
    }
  };

  const row = (i: MarketplaceInstall, scope: InstallScope, scopeLabel: string, removeLabel: string) => {
    const upd = updateFor(i);
    const gone = !known.has(i.marketplace_id);
    return (
      <li key={`${i.marketplace_id}/${i.kind}/${i.key}`} className="flex items-center justify-between gap-2 text-xs py-1">
        <div className="min-w-0">
          <span className="font-medium">{i.key}</span>
          <span className="ml-1 text-[var(--text-secondary)]">
            {KIND_LABELS[i.kind].replace(/s$/, "").toLowerCase()} · {nameOf(i.marketplace_id)} · {i.commit.slice(0, 8)}
          </span>
          {gone && <span className="ml-2 text-[var(--warning)]">Source removed</span>}
        </div>
        <div className="flex gap-1 flex-shrink-0">
          {upd && !gone && (
            <Button
              size="sm"
              variant="secondary"
              aria-label={`Review update for ${i.key}`}
              onClick={() => setPending({ install: i, update: upd, scope, scopeLabel })}
            >
              Update available
            </Button>
          )}
          <Button size="sm" variant="ghost" aria-label={removeLabel} onClick={() => void mp.uninstall(i, scope)}>
            Remove
          </Button>
        </div>
      </li>
    );
  };

  return (
    <div className="p-4 space-y-4 max-w-4xl">
      <div className="flex items-center justify-between gap-2">
        <p className="text-xs text-[var(--text-secondary)]">
          Installs are pinned to a commit. Containers pick up changes on their next start, or now for running ones.
          Changes apply to new Claude sessions.
        </p>
        <Button size="md" variant="primary" disabled={applying} onClick={() => void applyNow()}>
          {applying ? "Applying…" : "Apply now"}
        </Button>
      </div>

      {removedSources.length > 0 && (
        <div className="rounded-[var(--radius-control)] border border-[var(--warning)]/40 bg-[var(--warning-muted)] p-2 text-xs space-y-1">
          <p>
            Some installs come from marketplaces that were removed. They are removed from containers at their next
            sync.
          </p>
          <Button
            size="sm"
            variant="secondary"
            aria-label="Forget installs from removed marketplaces"
            onClick={() => removedSources.forEach((id) => void mp.forget(id))}
          >
            Forget
          </Button>
        </div>
      )}

      <section data-testid="installed-global">
        <h3 className="text-xs font-medium mb-1">All projects</h3>
        {globalInstalls.length === 0 ? (
          <p className="text-xs text-[var(--text-secondary)]">Nothing installed for all projects.</p>
        ) : (
          <ul>{globalInstalls.map((i) => row(i, { type: "global" }, "All projects", `Remove ${i.key} from all projects`))}</ul>
        )}
      </section>

      {projects.map((p) => (
        <section key={p.id} data-testid={`installed-project-${p.id}`}>
          <h3 className="text-xs font-medium mb-1">{p.name}</h3>
          {p.marketplace_installs.length === 0 ? (
            <p className="text-xs text-[var(--text-secondary)]">
              No project-only installs
              {p.marketplace_disabled.length > 0 ? ` · opted out of ${p.marketplace_disabled.length} global item(s)` : ""}.
            </p>
          ) : (
            <ul>
              {p.marketplace_installs.map((i) =>
                row(i, { type: "project", project_id: p.id }, p.name, `Remove ${i.key} from ${p.name}`),
              )}
            </ul>
          )}
        </section>
      ))}

      {pending && (
        <UpdateDiffModal
          item={pending.update.item}
          fromCommit={pending.install.commit}
          toCommit={pending.update.head}
          scopeLabel={pending.scopeLabel}
          onClose={() => setPending(null)}
          onAccept={() => mp.update(pending.update.item, pending.scope)}
        />
      )}
    </div>
  );
}
```

Run: `cd app && npx vitest run src/components/marketplace/InstalledPane.test.tsx`
Expected: PASS.

- [ ] **Step 5: Type-check and commit**

Run: `cd app && npx tsc --noEmit -p . && npx vitest run src/components/marketplace`
Expected: no type errors; all marketplace tests pass.

```bash
cd /workspace/triple-c && git add app/src/components/marketplace && git commit -qm "Marketplace UI: installed list, update diff review, apply now

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 15: Accounts — list, test, remove, add (gh on host, gh in container, token)

**Files:**
- Replace: `app/src/components/marketplace/AccountsPane.tsx`
- Create: `app/src/components/marketplace/AddAccountModal.tsx`
- Create: `app/src/components/marketplace/GhContainerLoginModal.tsx`
- Test: `app/src/components/marketplace/AccountsPane.test.tsx`, `AddAccountModal.test.tsx`, `GhContainerLoginModal.test.tsx`

**Interfaces:**
- Consumes: wrappers `testMarketplaceAccount`, `removeMarketplaceAccount`, `addMarketplaceTokenAccount`, `addMarketplaceGhHostAccount`, `marketplaceGhHostAvailable`, `startMarketplaceGhContainerLogin`, `cancelMarketplaceGhLogin`, `openUrlExternal`; events `marketplace-gh-login-code` `{ account_id, code, url }`, `marketplace-gh-login-output` `{ account_id, chunk }`; store `appSettings`, `setAppSettings`, `projects`, `pushToast`; `useSettings().loadSettings`.
- Produces: `AccountsPane({ mp })`, `AddAccountModal({ onClose })`, `GhContainerLoginModal({ label, host, projectId, projectName, onClose, onDone })`.

Event filtering: `startMarketplaceGhContainerLogin` resolves only when the login finishes, so the modal cannot know the new account's id while it runs. It therefore accepts **every** `marketplace-gh-login-*` event while it is open. This is safe because the backend allows one gh login at a time (`MarketplaceManager::set_gh_login_cancel` refuses a second).

The token input is a password field; the value is sent once to `addMarketplaceTokenAccount` and then cleared from component state. It is never logged or shown again.

- [ ] **Step 1: Failing test for GhContainerLoginModal**

`app/src/components/marketplace/GhContainerLoginModal.test.tsx`:

```tsx
import { describe, it, expect, vi, beforeEach } from "vitest";
import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";

const startMarketplaceGhContainerLogin = vi.fn();
const cancelMarketplaceGhLogin = vi.fn();
const openUrlExternal = vi.fn();
vi.mock("../../lib/tauri-commands", () => ({
  startMarketplaceGhContainerLogin: (...a: unknown[]) => startMarketplaceGhContainerLogin(...a),
  cancelMarketplaceGhLogin: () => cancelMarketplaceGhLogin(),
  openUrlExternal: (u: string) => openUrlExternal(u),
}));

const handlers = new Map<string, (e: { payload: unknown }) => void>();
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async (name: string, cb: (e: { payload: unknown }) => void) => {
    handlers.set(name, cb);
    return vi.fn();
  }),
}));

import GhContainerLoginModal from "./GhContainerLoginModal";

describe("GhContainerLoginModal", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    handlers.clear();
  });

  it("shows the device code, opens the URL, and finishes", async () => {
    let resolve!: (v: unknown) => void;
    startMarketplaceGhContainerLogin.mockReturnValue(new Promise((r) => (resolve = r)));
    const onDone = vi.fn();
    render(
      <GhContainerLoginModal label="Work" host="github.com" projectId="p1" projectName="api" onClose={vi.fn()} onDone={onDone} />,
    );
    await waitFor(() => expect(handlers.has("marketplace-gh-login-code")).toBe(true));
    await waitFor(() => expect(startMarketplaceGhContainerLogin).toHaveBeenCalledWith("Work", "github.com", "p1"));

    act(() =>
      handlers.get("marketplace-gh-login-code")!({
        payload: { account_id: "unknown-yet", code: "ABCD-1234", url: "https://github.com/login/device" },
      }),
    );
    expect(screen.getByText("ABCD-1234")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Open GitHub" }));
    expect(openUrlExternal).toHaveBeenCalledWith("https://github.com/login/device");

    await act(async () => resolve({ id: "acc9", label: "Work", host: "github.com", method: "gh_container", username: "me" }));
    await waitFor(() => expect(onDone).toHaveBeenCalled());
  });

  it("refuses to open a non-GitHub URL from the container", async () => {
    startMarketplaceGhContainerLogin.mockReturnValue(new Promise(() => {}));
    render(<GhContainerLoginModal label="W" host="github.com" projectId="p1" projectName="api" onClose={vi.fn()} onDone={vi.fn()} />);
    await waitFor(() => expect(handlers.has("marketplace-gh-login-code")).toBe(true));
    act(() =>
      handlers.get("marketplace-gh-login-code")!({
        payload: { account_id: "x", code: "ABCD-1234", url: "https://evil.example/login" },
      }),
    );
    expect(screen.queryByRole("button", { name: "Open GitHub" })).not.toBeInTheDocument();
  });

  it("cancels", async () => {
    startMarketplaceGhContainerLogin.mockReturnValue(new Promise(() => {}));
    const onClose = vi.fn();
    render(<GhContainerLoginModal label="W" host="github.com" projectId="p1" projectName="api" onClose={onClose} onDone={vi.fn()} />);
    fireEvent.click(await screen.findByRole("button", { name: "Cancel sign-in" }));
    expect(cancelMarketplaceGhLogin).toHaveBeenCalled();
    expect(onClose).toHaveBeenCalled();
  });
});
```

Run: `cd app && npx vitest run src/components/marketplace/GhContainerLoginModal.test.tsx`
Expected: FAIL — module not found.

- [ ] **Step 2: Implement GhContainerLoginModal**

`app/src/components/marketplace/GhContainerLoginModal.tsx`:

```tsx
import { useEffect, useRef, useState } from "react";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import Modal from "../ui/Modal";
import Button from "../ui/Button";
import StatusIndicator from "../ui/StatusIndicator";
import {
  cancelMarketplaceGhLogin,
  openUrlExternal,
  startMarketplaceGhContainerLogin,
} from "../../lib/tauri-commands";
import type { MarketplaceAccount } from "../../lib/types";

interface Props {
  label: string;
  host: string;
  projectId: string;
  projectName: string;
  onClose: () => void;
  onDone: (account: MarketplaceAccount) => void;
}

interface CodeEvent {
  account_id: string;
  code: string;
  url: string;
}
interface OutputEvent {
  account_id: string;
  chunk: string;
}

const MAX_OUTPUT = 8000;

/** Only open device-login pages on the host being signed in to. */
function safeDeviceUrl(url: string, host: string): string | null {
  try {
    const u = new URL(url);
    return u.protocol === "https:" && u.hostname === host ? u.toString() : null;
  } catch {
    return null;
  }
}

/**
 * Drives `gh auth login --web` inside a running container. The command only
 * resolves when the login finishes, so the new account's id is unknown while it
 * runs; the modal accepts every gh-login event while open. The backend allows
 * one gh login at a time, so there is never another flow's event to confuse.
 */
export default function GhContainerLoginModal({ label, host, projectId, projectName, onClose, onDone }: Props) {
  const [code, setCode] = useState<string | null>(null);
  const [url, setUrl] = useState<string | null>(null);
  const [output, setOutput] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [running, setRunning] = useState(true);
  const started = useRef(false);

  useEffect(() => {
    let cancelled = false;
    const unlisteners: UnlistenFn[] = [];
    const register = async <T,>(name: string, handle: (p: T) => void) => {
      const un = await listen<T>(name, (e) => handle(e.payload));
      if (cancelled) un();
      else unlisteners.push(un);
    };

    void (async () => {
      await register<CodeEvent>("marketplace-gh-login-code", (p) => {
        setCode(p.code);
        setUrl(p.url);
      });
      await register<OutputEvent>("marketplace-gh-login-output", (p) =>
        setOutput((prev) => {
          const next = prev + p.chunk;
          return next.length > MAX_OUTPUT ? next.slice(next.length - MAX_OUTPUT) : next;
        }),
      );
      if (cancelled || started.current) return;
      started.current = true;
      try {
        const account = await startMarketplaceGhContainerLogin(label, host, projectId);
        if (!cancelled) {
          setRunning(false);
          onDone(account);
        }
      } catch (e) {
        if (!cancelled) {
          setRunning(false);
          setError(typeof e === "string" ? e : String(e));
        }
      }
    })();

    return () => {
      cancelled = true;
      for (const un of unlisteners) {
        try {
          un();
        } catch {
          /* already gone */
        }
      }
    };
    // Runs once per modal instance; the props do not change while it is open.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const cancel = () => {
    void cancelMarketplaceGhLogin();
    onClose();
  };

  const openable = url ? safeDeviceUrl(url, host) : null;

  return (
    <Modal
      title={`Sign in to ${host} with gh`}
      description={`Running gh auth login in “${projectName}”. The sign-in is not kept in that container.`}
      widthClassName="w-[40rem]"
      dismissible={!running}
      onClose={running ? cancel : onClose}
      footer={
        running ? (
          <Button size="md" variant="ghost" onClick={cancel}>
            Cancel sign-in
          </Button>
        ) : (
          <Button size="md" onClick={onClose}>
            Close
          </Button>
        )
      }
    >
      <div className="space-y-3">
        {running && !code && <StatusIndicator tone="busy" label="Starting gh…" className="text-xs" />}
        {code && running && (
          <div className="space-y-2">
            <p className="text-xs">Enter this code on the GitHub device page:</p>
            <p className="font-mono text-lg tracking-widest select-all">{code}</p>
            {openable ? (
              <Button size="md" variant="primary" onClick={() => void openUrlExternal(openable)}>
                Open GitHub
              </Button>
            ) : (
              url && <p className="text-xs text-[var(--error)]">The sign-in URL did not point at {host}; not opening it.</p>
            )}
          </div>
        )}
        {error && <p role="alert" className="text-xs text-[var(--error)] whitespace-pre-wrap">{error}</p>}
        {output && (
          <pre className="max-h-40 overflow-auto p-2 text-[11px] font-mono whitespace-pre-wrap rounded-[var(--radius-control)] bg-[var(--bg-primary)] border border-[var(--border-color)]">
            {output}
          </pre>
        )}
      </div>
    </Modal>
  );
}
```

Run: `cd app && npx vitest run src/components/marketplace/GhContainerLoginModal.test.tsx`
Expected: PASS.

- [ ] **Step 3: Failing test for AddAccountModal**

`app/src/components/marketplace/AddAccountModal.test.tsx`:

```tsx
import { describe, it, expect, vi, beforeEach } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { useAppState } from "../../store/appState";
import type { Project } from "../../lib/types";

const marketplaceGhHostAvailable = vi.fn();
const addMarketplaceGhHostAccount = vi.fn();
const addMarketplaceTokenAccount = vi.fn();
const getSettings = vi.fn();
vi.mock("../../lib/tauri-commands", () => ({
  marketplaceGhHostAvailable: () => marketplaceGhHostAvailable(),
  addMarketplaceGhHostAccount: (...a: unknown[]) => addMarketplaceGhHostAccount(...a),
  addMarketplaceTokenAccount: (...a: unknown[]) => addMarketplaceTokenAccount(...a),
  getSettings: () => getSettings(),
}));
vi.mock("./GhContainerLoginModal", () => ({
  default: ({ projectId }: { projectId: string }) => <div>container login for {projectId}</div>,
}));

import AddAccountModal from "./AddAccountModal";

const running = { id: "p1", name: "api", status: "running", container_id: "c1" } as unknown as Project;

describe("AddAccountModal", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    getSettings.mockResolvedValue({ marketplace_accounts: [] });
    useAppState.setState({ projects: [running], toasts: [] });
  });

  it("uses host gh when available", async () => {
    marketplaceGhHostAvailable.mockResolvedValue(true);
    addMarketplaceGhHostAccount.mockResolvedValue({ id: "a1" });
    const onClose = vi.fn();
    render(<AddAccountModal onClose={onClose} />);
    expect(await screen.findByText(/gh is installed on this computer/)).toBeInTheDocument();
    fireEvent.change(screen.getByLabelText("Label"), { target: { value: "Personal" } });
    fireEvent.click(screen.getByRole("button", { name: "Add account" }));
    await waitFor(() => expect(addMarketplaceGhHostAccount).toHaveBeenCalledWith("Personal", "github.com"));
    await waitFor(() => expect(onClose).toHaveBeenCalled());
  });

  it("falls back to gh in a running container", async () => {
    marketplaceGhHostAvailable.mockResolvedValue(false);
    render(<AddAccountModal onClose={vi.fn()} />);
    expect(await screen.findByLabelText("Run gh in")).toBeInTheDocument();
    fireEvent.change(screen.getByLabelText("Label"), { target: { value: "Work" } });
    fireEvent.click(screen.getByRole("button", { name: "Sign in" }));
    expect(screen.getByText("container login for p1")).toBeInTheDocument();
  });

  it("adds a token account for any host", async () => {
    marketplaceGhHostAvailable.mockResolvedValue(false);
    addMarketplaceTokenAccount.mockResolvedValue({ id: "a2" });
    render(<AddAccountModal onClose={vi.fn()} />);
    fireEvent.click(await screen.findByRole("radio", { name: "Access token" }));
    fireEvent.change(screen.getByLabelText("Label"), { target: { value: "Gitea" } });
    fireEvent.change(screen.getByLabelText("Host"), { target: { value: "repo.anhonesthost.net" } });
    fireEvent.change(screen.getByLabelText("Token"), { target: { value: "test-token-not-real" } });
    fireEvent.click(screen.getByRole("button", { name: "Add account" }));
    await waitFor(() =>
      expect(addMarketplaceTokenAccount).toHaveBeenCalledWith("Gitea", "repo.anhonesthost.net", "test-token-not-real"),
    );
  });

  it("shows a validation error from the backend", async () => {
    marketplaceGhHostAvailable.mockResolvedValue(false);
    addMarketplaceTokenAccount.mockRejectedValue("The token was rejected by repo.anhonesthost.net (HTTP 401)");
    render(<AddAccountModal onClose={vi.fn()} />);
    fireEvent.click(await screen.findByRole("radio", { name: "Access token" }));
    fireEvent.change(screen.getByLabelText("Label"), { target: { value: "G" } });
    fireEvent.change(screen.getByLabelText("Host"), { target: { value: "repo.anhonesthost.net" } });
    fireEvent.change(screen.getByLabelText("Token"), { target: { value: "test-token-not-real" } });
    fireEvent.click(screen.getByRole("button", { name: "Add account" }));
    expect(await screen.findByText(/HTTP 401/)).toBeInTheDocument();
  });
});
```

(`SegmentedControl` segments are `role="radio"`.)

Run: `cd app && npx vitest run src/components/marketplace/AddAccountModal.test.tsx`
Expected: FAIL — module not found.

- [ ] **Step 4: Implement AddAccountModal**

`app/src/components/marketplace/AddAccountModal.tsx`:

```tsx
import { useEffect, useState } from "react";
import Modal from "../ui/Modal";
import Button from "../ui/Button";
import SegmentedControl from "../ui/SegmentedControl";
import Field, { inputClass, selectClass } from "../ui/Field";
import {
  addMarketplaceGhHostAccount,
  addMarketplaceTokenAccount,
  getSettings,
  marketplaceGhHostAvailable,
} from "../../lib/tauri-commands";
import { useAppState } from "../../store/appState";
import GhContainerLoginModal from "./GhContainerLoginModal";

type Method = "gh" | "token";

interface Props {
  onClose: () => void;
}

export default function AddAccountModal({ onClose }: Props) {
  const projects = useAppState((s) => s.projects);
  const setAppSettings = useAppState((s) => s.setAppSettings);
  const runnable = projects.filter((p) => p.status === "running" && p.container_id);

  const [method, setMethod] = useState<Method>("gh");
  const [hostGh, setHostGh] = useState<boolean | null>(null);
  const [label, setLabel] = useState("");
  const [host, setHost] = useState("github.com");
  const [token, setToken] = useState("");
  const [projectId, setProjectId] = useState(runnable[0]?.id ?? "");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [containerLogin, setContainerLogin] = useState(false);

  useEffect(() => {
    let cancelled = false;
    marketplaceGhHostAvailable()
      .then((v) => {
        if (!cancelled) setHostGh(v);
      })
      .catch(() => {
        if (!cancelled) setHostGh(false);
      });
    return () => {
      cancelled = true;
    };
  }, []);

  const reloadSettings = async () => setAppSettings(await getSettings());

  const finish = async () => {
    await reloadSettings();
    onClose();
  };

  const submit = async () => {
    setError(null);
    if (method === "gh" && !hostGh) {
      setContainerLogin(true);
      return;
    }
    setBusy(true);
    try {
      if (method === "gh") {
        await addMarketplaceGhHostAccount(label.trim(), host.trim());
      } else {
        const t = token.trim();
        setToken("");
        await addMarketplaceTokenAccount(label.trim(), host.trim(), t);
      }
      await finish();
    } catch (e) {
      setError(typeof e === "string" ? e : String(e));
    } finally {
      setBusy(false);
    }
  };

  const hostValid = /^[A-Za-z0-9.-]+(:[0-9]+)?$/.test(host.trim());
  const needsContainer = method === "gh" && hostGh === false;
  const canSubmit =
    !busy &&
    hostGh !== null &&
    label.trim() !== "" &&
    hostValid &&
    (method === "gh" ? !needsContainer || projectId !== "" : token.trim() !== "");

  if (containerLogin) {
    const project = runnable.find((p) => p.id === projectId);
    return (
      <GhContainerLoginModal
        label={label.trim()}
        host={host.trim()}
        projectId={projectId}
        projectName={project?.name ?? projectId}
        onClose={onClose}
        onDone={() => void finish()}
      />
    );
  }

  return (
    <Modal
      title="Add account"
      description="Accounts let Triple-C read private marketplace repositories. Credentials stay on this computer and never enter containers."
      widthClassName="w-[36rem]"
      dismissible={!busy}
      onClose={onClose}
      footer={
        <>
          <Button size="md" variant="ghost" onClick={onClose} disabled={busy}>
            Cancel
          </Button>
          <Button size="md" variant="primary" onClick={() => void submit()} disabled={!canSubmit}>
            {needsContainer ? "Sign in" : busy ? "Checking…" : "Add account"}
          </Button>
        </>
      }
    >
      <div className="space-y-3">
        <SegmentedControl<Method>
          label="Sign-in method"
          value={method}
          onChange={(m) => {
            setMethod(m);
            setError(null);
          }}
          segments={[
            { value: "gh", label: "GitHub via gh" },
            { value: "token", label: "Access token" },
          ]}
        />
        <Field label="Label">
          {(id) => (
            <input id={id} value={label} onChange={(e) => setLabel(e.target.value)} className={inputClass} placeholder="Work GitHub" />
          )}
        </Field>
        <Field label="Host" hint={hostValid ? undefined : "Host name only, e.g. github.com or repo.example.com"}>
          {(id) => <input id={id} value={host} onChange={(e) => setHost(e.target.value)} className={inputClass} />}
        </Field>
        {method === "gh" && hostGh === true && (
          <p className="text-xs text-[var(--text-secondary)]">
            gh is installed on this computer. Triple-C asks it for a token each time it fetches, so signing out of gh
            also signs this account out. If gh is not logged in yet, run <code className="font-mono">gh auth login</code> first.
          </p>
        )}
        {needsContainer &&
          (runnable.length === 0 ? (
            <p className="text-xs text-[var(--warning)]">
              gh is not installed on this computer. Start a project so gh can run in its container, or use an access token.
            </p>
          ) : (
            <Field
              label="Run gh in"
              hint="gh is not installed on this computer, so the sign-in runs in this container. The token is kept in your OS keychain, not in the container."
            >
              {(id) => (
                <select id={id} value={projectId} onChange={(e) => setProjectId(e.target.value)} className={selectClass}>
                  {runnable.map((p) => (
                    <option key={p.id} value={p.id}>
                      {p.name}
                    </option>
                  ))}
                </select>
              )}
            </Field>
          ))}
        {method === "token" && (
          <Field
            label="Token"
            hint="A personal access token with read access to the repository. For GitHub SSO orgs, authorise the token for the org."
          >
            {(id) => (
              <input
                id={id}
                type="password"
                autoComplete="off"
                value={token}
                onChange={(e) => setToken(e.target.value)}
                className={inputClass}
              />
            )}
          </Field>
        )}
        {error && <p role="alert" className="text-xs text-[var(--error)] whitespace-pre-wrap">{error}</p>}
      </div>
    </Modal>
  );
}
```

Run: `cd app && npx vitest run src/components/marketplace/AddAccountModal.test.tsx`
Expected: PASS.

- [ ] **Step 5: Failing test for AccountsPane**

`app/src/components/marketplace/AccountsPane.test.tsx`:

```tsx
import { describe, it, expect, vi, beforeEach } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { useAppState } from "../../store/appState";
import type { AppSettings } from "../../lib/types";
import type { MarketplaceApi } from "../../hooks/useMarketplace";

const testMarketplaceAccount = vi.fn();
const removeMarketplaceAccount = vi.fn();
vi.mock("../../lib/tauri-commands", () => ({
  testMarketplaceAccount: (id: string) => testMarketplaceAccount(id),
  removeMarketplaceAccount: (id: string) => removeMarketplaceAccount(id),
}));
vi.mock("./AddAccountModal", () => ({ default: () => <div>add account modal</div> }));

import AccountsPane from "./AccountsPane";

const settings = {
  marketplace_accounts: [
    { id: "a1", label: "Personal", host: "github.com", method: "gh_host", username: "me" },
    { id: "a2", label: "Gitea", host: "repo.example.com", method: "token", username: "jk" },
  ],
  marketplaces: [{ id: "m1", name: "Team", url: "https://repo.example.com/t/m.git", branch: null, account_id: "a2" }],
  global_marketplace_installs: [],
} as unknown as AppSettings;

describe("AccountsPane", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    useAppState.setState({ appSettings: settings, toasts: [] });
  });

  it("lists accounts with their method and usage", () => {
    render(<AccountsPane mp={{} as MarketplaceApi} />);
    expect(screen.getByText("Personal")).toBeInTheDocument();
    expect(screen.getByText(/gh on this computer/)).toBeInTheDocument();
    expect(screen.getByText(/Used by Team/)).toBeInTheDocument();
  });

  it("tests an account", async () => {
    testMarketplaceAccount.mockResolvedValue("me");
    render(<AccountsPane mp={{} as MarketplaceApi} />);
    fireEvent.click(screen.getByRole("button", { name: "Test Personal" }));
    await waitFor(() => expect(useAppState.getState().toasts[0]).toMatchObject({ kind: "success" }));
    expect(useAppState.getState().toasts[0].message).toContain("me");
  });

  it("confirms before removing an account in use, then removes", async () => {
    removeMarketplaceAccount.mockResolvedValue({ ...settings, marketplace_accounts: [settings.marketplace_accounts[0]] });
    render(<AccountsPane mp={{} as MarketplaceApi} />);
    fireEvent.click(screen.getByRole("button", { name: "Remove Gitea" }));
    expect(screen.getByText(/Team will be fetched without credentials/)).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Remove account" }));
    await waitFor(() => expect(removeMarketplaceAccount).toHaveBeenCalledWith("a2"));
    await waitFor(() => expect(useAppState.getState().appSettings!.marketplace_accounts).toHaveLength(1));
  });

  it("opens the add dialog", () => {
    render(<AccountsPane mp={{} as MarketplaceApi} />);
    fireEvent.click(screen.getByRole("button", { name: "Add account" }));
    expect(screen.getByText("add account modal")).toBeInTheDocument();
  });
});
```

Run: `cd app && npx vitest run src/components/marketplace/AccountsPane.test.tsx`
Expected: FAIL — stub pane has no list.

- [ ] **Step 6: Implement AccountsPane**

`app/src/components/marketplace/AccountsPane.tsx` (replaces the Task 12 stub):

```tsx
import { useState } from "react";
import type { MarketplaceApi } from "../../hooks/useMarketplace";
import { useAppState } from "../../store/appState";
import { removeMarketplaceAccount, testMarketplaceAccount } from "../../lib/tauri-commands";
import type { AccountMethod, MarketplaceAccount } from "../../lib/types";
import Button from "../ui/Button";
import Modal from "../ui/Modal";
import AddAccountModal from "./AddAccountModal";

const METHOD_LABEL: Record<AccountMethod, string> = {
  gh_host: "GitHub — gh on this computer",
  gh_container: "GitHub — signed in via container",
  token: "Access token",
};

export default function AccountsPane(_props: { mp: MarketplaceApi }) {
  const appSettings = useAppState((s) => s.appSettings);
  const setAppSettings = useAppState((s) => s.setAppSettings);
  const pushToast = useAppState((s) => s.pushToast);
  const [adding, setAdding] = useState(false);
  const [confirmRemove, setConfirmRemove] = useState<MarketplaceAccount | null>(null);
  const [testing, setTesting] = useState<string | null>(null);

  const accounts = appSettings?.marketplace_accounts ?? [];
  const marketplaces = appSettings?.marketplaces ?? [];
  const usedBy = (id: string) => marketplaces.filter((m) => m.account_id === id).map((m) => m.name);

  const test = async (a: MarketplaceAccount) => {
    setTesting(a.id);
    try {
      const login = await testMarketplaceAccount(a.id);
      pushToast({ kind: "success", message: `${a.label} works — signed in as ${login}` });
    } catch (e) {
      pushToast({ kind: "error", message: `${a.label} could not sign in`, detail: String(e) });
    } finally {
      setTesting(null);
    }
  };

  const remove = async (a: MarketplaceAccount) => {
    setConfirmRemove(null);
    try {
      setAppSettings(await removeMarketplaceAccount(a.id));
    } catch (e) {
      pushToast({ kind: "error", message: `Could not remove ${a.label}`, detail: String(e) });
    }
  };

  return (
    <div className="p-4 space-y-3 max-w-3xl">
      <div className="flex items-center justify-between">
        <p className="text-xs text-[var(--text-secondary)]">
          Accounts are used to fetch private marketplaces. Tokens are kept in your OS keychain and never enter
          containers.
        </p>
        <Button size="md" variant="secondary" onClick={() => setAdding(true)}>
          Add account
        </Button>
      </div>
      {accounts.length === 0 && <p className="text-xs text-[var(--text-secondary)]">No accounts yet. Public repositories need none.</p>}
      <ul className="space-y-2">
        {accounts.map((a) => {
          const users = usedBy(a.id);
          return (
            <li
              key={a.id}
              className="flex items-center justify-between gap-2 p-2 rounded-[var(--radius-control)] border border-[var(--border-color)]"
            >
              <div className="min-w-0 text-xs">
                <p className="font-medium">{a.label}</p>
                <p className="text-[var(--text-secondary)]">
                  {METHOD_LABEL[a.method]} · {a.host}
                  {a.username ? ` · ${a.username}` : ""}
                </p>
                {users.length > 0 && <p className="text-[var(--text-secondary)]">Used by {users.join(", ")}</p>}
              </div>
              <div className="flex gap-1 flex-shrink-0">
                <Button
                  size="sm"
                  variant="ghost"
                  aria-label={`Test ${a.label}`}
                  disabled={testing === a.id}
                  onClick={() => void test(a)}
                >
                  {testing === a.id ? "Testing…" : "Test"}
                </Button>
                <Button
                  size="sm"
                  variant="ghost"
                  aria-label={`Remove ${a.label}`}
                  onClick={() => (users.length > 0 ? setConfirmRemove(a) : void remove(a))}
                >
                  Remove
                </Button>
              </div>
            </li>
          );
        })}
      </ul>
      {adding && <AddAccountModal onClose={() => setAdding(false)} />}
      {confirmRemove && (
        <Modal
          title={`Remove ${confirmRemove.label}?`}
          onClose={() => setConfirmRemove(null)}
          footer={
            <>
              <Button size="md" variant="ghost" onClick={() => setConfirmRemove(null)}>
                Cancel
              </Button>
              <Button size="md" variant="danger" onClick={() => void remove(confirmRemove)}>
                Remove account
              </Button>
            </>
          }
        >
          <p className="text-xs">
            {usedBy(confirmRemove.id).join(", ")} will be fetched without credentials, which fails for private
            repositories. Installed items keep syncing from the cached copy.
          </p>
        </Modal>
      )}
    </div>
  );
}
```

- [ ] **Step 7: Run and commit**

Run: `cd app && npx vitest run src/components/marketplace && npx tsc --noEmit -p .`
Expected: PASS, no type errors.

```bash
cd /workspace/triple-c && git add app/src/components/marketplace && git commit -qm "Marketplace UI: accounts — gh on host, gh in a container, access tokens

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 16: Project Home → Config → Marketplace section

**Files:**
- Create: `app/src/components/projects/home/config/MarketplaceSection.tsx`
- Create: `app/src/components/projects/home/config/MarketplaceSection.test.tsx`
- Modify: `app/src/components/projects/home/ConfigTab.tsx` (import + render after `RuntimeSection`)

**Interfaces:**
- Consumes: `effectiveInstalls`, `KIND_LABELS`, `formatItemRef` (Task 12); wrappers `setGlobalItemDisabled`, `getMarketplaceSyncReport`; store `appSettings`, `openMarketplace`, `updateProjectInList`, `pushToast`; `ConfigGroup` from `ui/Field`, `Toggle`.
- Produces: `MarketplaceSection({ project })` default export.

Opting out is allowed while the container runs (it only changes what the next sync installs), so this section is not disabled by `STOPPED_ONLY`. It saves through `setGlobalItemDisabled` (which returns the updated `Project`), not through `save()`, because `update_project` is the stopped-only path.

- [ ] **Step 1: Failing test**

`app/src/components/projects/home/config/MarketplaceSection.test.tsx`:

```tsx
import { describe, it, expect, vi, beforeEach } from "vitest";
import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { useAppState, MARKETPLACE_TAB_KEY } from "../../../../store/appState";
import type { AppSettings, Project } from "../../../../lib/types";

const setGlobalItemDisabled = vi.fn();
const getMarketplaceSyncReport = vi.fn();
vi.mock("../../../../lib/tauri-commands", () => ({
  setGlobalItemDisabled: (...a: unknown[]) => setGlobalItemDisabled(...a),
  getMarketplaceSyncReport: (id: string) => getMarketplaceSyncReport(id),
}));

import MarketplaceSection from "./MarketplaceSection";

const A = "a".repeat(40);
const project = {
  id: "p1",
  name: "api",
  status: "running",
  marketplace_installs: [{ marketplace_id: "m1", kind: "command", key: "cmd", commit: A }],
  marketplace_disabled: [{ marketplace_id: "m1", kind: "hook", key: "noisy" }],
} as unknown as Project;

describe("MarketplaceSection", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    useAppState.setState({
      tabOrder: [],
      activeTabKey: null,
      projects: [project],
      toasts: [],
      appSettings: {
        marketplaces: [{ id: "m1", name: "Starter", url: "https://x/y.git", branch: null, account_id: null }],
        marketplace_accounts: [],
        global_marketplace_installs: [
          { marketplace_id: "m1", kind: "agent", key: "rev", commit: A },
          { marketplace_id: "m1", kind: "hook", key: "noisy", commit: A },
        ],
      } as unknown as AppSettings,
    });
    getMarketplaceSyncReport.mockResolvedValue({
      installed: ["agent:rev"],
      updated: [],
      removed: [],
      skipped: [{ item: "command:cmd", reason: "a file you created has the same name" }],
      errors: [],
      finished_at: "2026-09-27T12:00:00Z",
    });
  });

  it("shows effective items with their source and the opted-out global item", async () => {
    render(<MarketplaceSection project={project} />);
    const rev = screen.getByTestId("mp-global-agent-rev");
    expect(within(rev).getByRole("switch")).toBeChecked();
    const noisy = screen.getByTestId("mp-global-hook-noisy");
    expect(within(noisy).getByRole("switch")).not.toBeChecked();
    expect(screen.getByTestId("mp-project-command-cmd")).toHaveTextContent("This project only");
    expect(await screen.findByText(/a file you created has the same name/)).toBeInTheDocument();
  });

  it("opts out of a global item", async () => {
    setGlobalItemDisabled.mockResolvedValue({ ...project, marketplace_disabled: [] });
    render(<MarketplaceSection project={project} />);
    fireEvent.click(within(screen.getByTestId("mp-global-agent-rev")).getByRole("switch"));
    await waitFor(() =>
      expect(setGlobalItemDisabled).toHaveBeenCalledWith("p1", { marketplace_id: "m1", kind: "agent", key: "rev" }, true),
    );
  });

  it("opens the Marketplace filtered to this project", () => {
    render(<MarketplaceSection project={project} />);
    fireEvent.click(screen.getByRole("button", { name: "Open in Marketplace" }));
    expect(useAppState.getState().activeTabKey).toBe(MARKETPLACE_TAB_KEY);
    expect(useAppState.getState().marketplaceFilterProjectId).toBe("p1");
  });
});
```

(`Toggle` is `role="switch"` with `aria-checked`, so `toBeChecked()` works on it.)

Run: `cd app && npx vitest run src/components/projects/home/config/MarketplaceSection.test.tsx`
Expected: FAIL — module not found.

- [ ] **Step 2: Implement MarketplaceSection**

`app/src/components/projects/home/config/MarketplaceSection.tsx`:

```tsx
import { useEffect, useState } from "react";
import { ConfigGroup } from "../../../ui/Field";
import Toggle from "../../../ui/Toggle";
import Button from "../../../ui/Button";
import { useAppState } from "../../../../store/appState";
import { KIND_LABELS } from "../../../../lib/marketplace";
import { getMarketplaceSyncReport, setGlobalItemDisabled } from "../../../../lib/tauri-commands";
import type { MarketplaceItemRef, Project, SyncReport } from "../../../../lib/types";

interface Props {
  project: Project;
}

const kindWord = (k: MarketplaceItemRef["kind"]) => KIND_LABELS[k].replace(/s$/, "").toLowerCase();
const same = (a: MarketplaceItemRef, b: MarketplaceItemRef) =>
  a.marketplace_id === b.marketplace_id && a.kind === b.kind && a.key === b.key;

export default function MarketplaceSection({ project }: Props) {
  const appSettings = useAppState((s) => s.appSettings);
  const openMarketplace = useAppState((s) => s.openMarketplace);
  const updateProjectInList = useAppState((s) => s.updateProjectInList);
  const pushToast = useAppState((s) => s.pushToast);
  const [report, setReport] = useState<SyncReport | null>(null);
  const [busy, setBusy] = useState<string | null>(null);

  const globalInstalls = appSettings?.global_marketplace_installs ?? [];
  const nameOf = (id: string) => appSettings?.marketplaces.find((m) => m.id === id)?.name ?? "removed marketplace";

  useEffect(() => {
    let cancelled = false;
    getMarketplaceSyncReport(project.id)
      .then((r) => {
        if (!cancelled) setReport(r);
      })
      .catch(() => {
        if (!cancelled) setReport(null);
      });
    return () => {
      cancelled = true;
    };
  }, [project.id, project.status]);

  const toggleGlobal = async (ref: MarketplaceItemRef, enabled: boolean) => {
    const id = `${ref.kind}-${ref.key}`;
    setBusy(id);
    try {
      updateProjectInList(await setGlobalItemDisabled(project.id, ref, !enabled));
    } catch (e) {
      pushToast({ kind: "error", message: `Could not change ${ref.key} for “${project.name}”`, detail: String(e) });
    } finally {
      setBusy(null);
    }
  };

  return (
    <ConfigGroup
      title="Marketplace"
      description="Items this project gets from marketplaces. Changes apply on the next container start or with Apply now, in new Claude sessions."
    >
      <div className="space-y-3">
        {globalInstalls.length > 0 && (
          <div>
            <p className="text-xs font-medium mb-1">From “All projects”</p>
            <ul className="space-y-1">
              {globalInstalls.map((g) => {
                const shadowed = project.marketplace_installs.some((p) => same(p, g));
                const enabled = !project.marketplace_disabled.some((d) => same(d, g));
                return (
                  <li
                    key={`${g.marketplace_id}/${g.kind}/${g.key}`}
                    data-testid={`mp-global-${g.kind}-${g.key}`}
                    className="flex items-center justify-between gap-2 text-xs"
                  >
                    <span className="min-w-0 truncate">
                      <span className="font-medium">{g.key}</span>{" "}
                      <span className="text-[var(--text-secondary)]">
                        {kindWord(g.kind)} · {nameOf(g.marketplace_id)}
                        {shadowed ? " · overridden by this project's own install" : ""}
                      </span>
                    </span>
                    <Toggle
                      label={`Use ${g.key} in ${project.name}`}
                      checked={enabled}
                      disabled={busy === `${g.kind}-${g.key}`}
                      onChange={(v) => void toggleGlobal({ marketplace_id: g.marketplace_id, kind: g.kind, key: g.key }, v)}
                    />
                  </li>
                );
              })}
            </ul>
          </div>
        )}
        {project.marketplace_installs.length > 0 && (
          <div>
            <p className="text-xs font-medium mb-1">This project only</p>
            <ul className="space-y-1">
              {project.marketplace_installs.map((i) => (
                <li
                  key={`${i.marketplace_id}/${i.kind}/${i.key}`}
                  data-testid={`mp-project-${i.kind}-${i.key}`}
                  className="text-xs"
                >
                  <span className="font-medium">{i.key}</span>{" "}
                  <span className="text-[var(--text-secondary)]">
                    {kindWord(i.kind)} · {nameOf(i.marketplace_id)} · This project only
                  </span>
                </li>
              ))}
            </ul>
          </div>
        )}
        {globalInstalls.length === 0 && project.marketplace_installs.length === 0 && (
          <p className="text-xs text-[var(--text-secondary)]">Nothing installed from a marketplace.</p>
        )}

        {report && (
          <div className="text-xs space-y-1">
            <p className="font-medium">
              Last sync {report.finished_at ? new Date(report.finished_at).toLocaleString() : ""}
            </p>
            <p className="text-[var(--text-secondary)]">
              {report.installed.length} installed · {report.updated.length} updated · {report.removed.length} removed
            </p>
            {report.skipped.map((s) => (
              <p key={s.item} className="text-[var(--warning)]">
                Skipped {s.item}: {s.reason}
              </p>
            ))}
            {report.errors.map((e) => (
              <p key={e} className="text-[var(--error)] whitespace-pre-wrap">
                {e}
              </p>
            ))}
          </div>
        )}

        <Button size="sm" variant="secondary" onClick={() => openMarketplace(project.id)}>
          Open in Marketplace
        </Button>
      </div>
    </ConfigGroup>
  );
}
```

`updateProjectInList: (project: Project) => void` (`store/appState.ts:121`) replaces the project in the store.

In `app/src/components/projects/home/ConfigTab.tsx` add `import MarketplaceSection from "./config/MarketplaceSection";` and, after the `<RuntimeSection … />` element:

```tsx
      <MarketplaceSection project={project} />
```

- [ ] **Step 3: Run and commit**

Run: `cd app && npx vitest run src/components/projects/home && npx tsc --noEmit -p .`
Expected: PASS, no type errors.

```bash
cd /workspace/triple-c && git add app/src/components/projects/home && git commit -qm "Project Config: Marketplace section with per-project opt-out and last sync report

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 17: Docs, full verification, end-to-end check, PR

**Files:**
- Modify: `CLAUDE.md` (new `### Marketplace` subsection under Key Conventions, after the new-window capability rule, around line 646)
- Modify: `HOW-TO-USE.md` (new `## Marketplace` section; place it after the section that covers Claude authentication / settings — find with `grep -n '^## ' HOW-TO-USE.md`)

**Interfaces:**
- Consumes: everything from Tasks 1–16.
- Produces: an open Gitea PR from `feat/marketplace` into `main`.

- [ ] **Step 1: CLAUDE.md subsection**

Add:

```markdown
### Marketplace

- Code: models in `models/marketplace.rs`; host-side logic in `src/marketplace/` (`git.rs` gix cache + pins, `catalog.rs` repo format, `auth.rs` credentials, `gh_login.rs`, `payload.rs`, `sync.rs`); commands in `commands/marketplace_commands.rs`; UI in `components/marketplace/` and `projects/home/config/MarketplaceSection.tsx`. Spec: `docs/superpowers/specs/2026-09-27-marketplace-design.md`.
- **Tokens never enter containers.** Marketplaces are fetched on the host into `<data_dir>/triple-c/marketplaces/<id>.git`; containers only ever receive a tar of pinned files. Do not add a code path that passes a marketplace credential into an exec, env var, label or file in a container.
- **Sync model:** after every container start (next to `sync_bedrock_credentials`) and on "Apply now", the host builds the project's effective set (`global − disabled ∪ project`), uploads it, and runs the constant script `/usr/local/bin/triple-c-marketplace-sync` (source `container/marketplace-sync.sh`) as `claude`. The script only removes files and hook entries it recorded in `~/.claude/triple-c/marketplace/state.json`; it must never overwrite or delete user-created agents/skills/commands or user hooks. A sync failure must not fail the container start.
- Installs are **pinned** to a commit; nothing updates without the user accepting a diff. Pinned commits are kept alive by `refs/triple-c/pins/*` in the cache.
- Marketplace changes need no container labels or recreation — they are applied by the sync, not at create time.
```

- [ ] **Step 2: HOW-TO-USE.md section**

```markdown
## Marketplace

The marketplace installs Claude Code **agents, skills, commands, hooks and plugins** from git repositories into your containers.

1. **Settings → Marketplace → Open Marketplace** opens the Marketplace tab.
2. **Add a marketplace**: on the Browse tab choose *Add marketplace* and enter an HTTPS clone URL, for example `https://github.com/shadowdao/triple-c-marketplace.git`. For a private repository, pick an account (see below). Triple-C checks it can read the repository before saving.
3. **Install**: select an item to see what it contains. Turn on **All projects** to install it everywhere (including projects you add later), or tick individual projects. A project can opt out of an "All projects" item by unticking it, or from **Project → Config → Marketplace**.
4. **Hooks** run shell commands, so Triple-C shows every command before installing one.
5. **When it applies**: on the container's next start, or straight away for running containers with **Installed → Apply now**. New Claude sessions pick it up; sessions already open keep what they loaded.

**Updates.** Every install is pinned to the commit it came from. When an item changes in its repository, the Installed tab shows *Update available*. Review the diff and accept to move the pin.

**Accounts (private repositories).** On the Accounts tab:
- *GitHub via gh* — if the GitHub CLI is installed and logged in on this computer, Triple-C uses it. If not, it runs `gh auth login` inside a running project's container and keeps only the resulting token in your OS keychain.
- *Access token* — any host (GitHub, Gitea, GitLab). The token is stored in your OS keychain.

Credentials never enter containers. If a private repository in a GitHub organisation cannot be read, the error explains the usual causes: the org has not approved the GitHub CLI, the token is not authorised for the org's SSO, or a fine-grained token belongs to a different owner.

**If an item is skipped**: Triple-C never overwrites an agent, skill or command file you created yourself. If one has the same name as a marketplace item, the sync skips it and the project's Config → Marketplace section says so.
```

- [ ] **Step 3: Full automated verification**

Run each and record the result:

```bash
cd /workspace/triple-c/app && npx vitest run
```
Expected: all test files pass (previous count 75 files / 978 tests plus the new marketplace tests), including `src/test/capabilities.test.ts`.

```bash
cd /workspace/triple-c/app && npm run build
```
Expected: TypeScript and Vite build succeed.

```bash
cd /workspace/triple-c/app/src-tauri && cargo test --lib
```
Expected: all tests pass (previously 677 passed; now more). The sync-script tests skip only where `jq` is missing.

```bash
cd /workspace/triple-c/app/src-tauri && cargo clippy --lib 2>&1 | grep -E "src/(marketplace|models/marketplace|commands/marketplace_commands)" -A6
```
Expected: no output (no clippy warnings in new files). Fix any that appear.

```bash
cd /workspace/triple-c && for f in $(git diff --name-only main... -- 'app/src-tauri/src/**/*.rs'); do rustfmt --edition 2021 --check "$f" >/dev/null 2>&1 || echo "needs fmt: $f"; done
```
Expected: no `needs fmt` lines for files created by this branch (pre-existing files may already be unformatted on `main`; only fix what this branch added).

- [ ] **Step 4: Commit docs**

```bash
cd /workspace/triple-c && git add CLAUDE.md HOW-TO-USE.md && git commit -qm "Docs: marketplace

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

- [ ] **Step 5: Push and open the PR**

```bash
cd /workspace/triple-c && git push -q -u origin feat/marketplace
```

Build the body with python3 and post it with `$TEA_TOKEN` (never echo the token):

```bash
cd /workspace/triple-c && S=/tmp/claude-1000/-workspace/f70b4bb0-c929-434b-af28-62cc3705211a/scratchpad && python3 - <<'EOF' > $S/pr-marketplace.json
import json
body = """## Summary
Adds a Triple-C **marketplace**: git repositories of agents, skills, commands, hooks and plugins that can be installed for all projects or per project.

- Settings → Marketplace section and a full-width Marketplace tab (Browse / Installed / Accounts).
- Hybrid repo format: `agents/`, `skills/`, `commands/`, `hooks/` managed by Triple-C; `plugins/` is a standard Claude Code marketplace installed with `claude plugin`.
- Host-side fetch with `gix` into a bare cache; installs pinned to commits, per-item update detection with a diff review.
- Named accounts: GitHub via host `gh`, GitHub via `gh` in a container (token kept in the keychain, not the container), or an access token for any host. Tokens never enter containers.
- Sync after every container start and on Apply now: constant script in the image, idempotent, never touches user-created files or hooks; failures never block a start.
- Project Home → Config → Marketplace: effective items, per-project opt-out, last sync report.
- Also: the shared-auth button row in Settings now wraps (Check snapshot images no longer overflows).

Spec: `docs/superpowers/specs/2026-09-27-marketplace-design.md` · Plan: `docs/superpowers/plans/2026-09-27-marketplace.md`
Starter marketplace: https://github.com/shadowdao/triple-c-marketplace

## Testing
- `npx vitest run`, `npm run build`, `cargo test --lib` all pass (see CI).
- Manual end-to-end on the preview build: see checklist below.

## Manual end-to-end checklist
- [ ] Add `https://github.com/shadowdao/triple-c-marketplace.git` (no account); all five items listed.
- [ ] Install each kind for All projects; start a project; each appears in a new Claude session (`/agents`, skills, `/example-command`, Stop hook rings, `/plugin` lists example-plugin).
- [ ] Install an item for one project only; a second project does not get it.
- [ ] Opt a project out of a global item from Config → Marketplace; Apply now; it is removed there only.
- [ ] Push a change to the starter repo; Refresh; only that item shows Update available; diff shows the change; accept; Apply now.
- [ ] Create `~/.claude/agents/code-reviewer.md` by hand in a container; sync skips it with a conflict; the file is unchanged.
- [ ] Add a user hook to `~/.claude/settings.json`; install/uninstall the marketplace hook; the user hook is untouched.
- [ ] Hook install shows the confirm dialog with the rendered command.
- [ ] Private repo via an access token; private repo via gh (host, and via container on a machine without gh).
- [ ] Offline refresh keeps the cached items and shows the error; container start still succeeds.

🤖 Generated with [Claude Code](https://claude.com/claude-code)
"""
print(json.dumps({"title": "Marketplace for agents, skills, commands, hooks and plugins", "head": "feat/marketplace", "base": "main", "body": body}))
EOF
curl -s -X POST -H "Authorization: token $TEA_TOKEN" -H "Content-Type: application/json" \
  --data @$S/pr-marketplace.json \
  https://repo.anhonesthost.net/api/v1/repos/CyberCoveLLC/Triple-C/pulls \
  | python3 -c "import json,sys;d=json.load(sys.stdin);print(d.get('number'), d.get('html_url'), d.get('message'))"
```
Expected: a PR number and URL, message `None`.

- [ ] **Step 6: Wait for CI and run the manual checklist**

Check CI on the PR head (the preview build must be green before the manual run):

```bash
cd /workspace/triple-c && SHA=$(git rev-parse HEAD) && curl -s -H "Authorization: token $TEA_TOKEN" \
  https://repo.anhonesthost.net/api/v1/repos/CyberCoveLLC/Triple-C/commits/$SHA/status \
  | python3 -c "import json,sys;d=json.load(sys.stdin);print(d['state']);[print(' ',s['context'],s['status']) for s in d['statuses']]"
```
Expected: `success` once all jobs finish (`pending` while running). Then hand the manual checklist in the PR body to the user for the preview build; tick items as they are confirmed. Do not merge until the user has run it and asked for the merge.
