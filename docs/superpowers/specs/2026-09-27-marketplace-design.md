# Triple-C marketplace — design

Date: 2026-09-27
Status: approved in conversation (user), section by section; awaiting review of this written spec

## Goal

Users add one or more **marketplace** git repos in Settings, browse the agents, skills, commands,
hooks and plugins they contain, and install any item either **for all projects** or **for
individual projects**. Private repos are supported through named sign-in accounts. Installed items
are pinned to a commit and only change when the user accepts an update.

A public starter marketplace, `shadowdao/triple-c-marketplace` (local clone at
`/workspace/projects/triple-c-marketplace`, its own git repo), is created with one example of each
item type. It is also where marketplace items built later will be published.

Non-goals (this version): per-marketplace auto-update, a registered Triple-C GitHub/OAuth app,
publishing to a marketplace from inside Triple-C, a separate OS window for the marketplace.

## Decisions (user-approved)

| Topic | Decision |
|---|---|
| Repo format | **Hybrid**: Triple-C-managed `agents/`, `skills/`, `commands/`, `hooks/`; `plugins/` is a standard Claude Code marketplace installed with `claude plugin` |
| Auth | **Named accounts**, one chosen per marketplace. GitHub sign-in reuses `gh` (host first, else inside a running container); any host accepts a pasted token. No Triple-C OAuth app |
| Scope | **Global list + per-project additions + per-project opt-out** of global items; global items reach projects created later |
| Updates | **Pinned on install**; "update available" per item; the user reviews a diff and accepts |
| Fetching | **On the host**, into an app cache, with `gix`; files copied into containers. Tokens never enter containers |
| Where the UI lives | Settings sidebar section + a full-width **Marketplace** main-area tab (like Project Home), not an OS window |

## Current state (verified against `292fc90`)

- Settings is `components/settings/SettingsPanel.tsx`, rendered in the sidebar, sections as
  `AccordionSection`s. Main-area tabs are Zustand-driven (`store/appState.ts` `tabOrder` /
  `activeTabKey`; keys `home:<projectId>` and session ids), rendered by `App.tsx`, strip in
  `layout/MainTabs.tsx`. No router.
- Global settings: `models/app_settings.rs` `AppSettings` → `<data_dir>/triple-c/settings.json`.
  Per project: `models/project.rs` `Project` → `projects.json`. TS mirror in `lib/types.ts`,
  wrappers in `lib/tauri-commands.ts`. Settings export/import in `models/settings_export.rs`.
- Keychain: `storage/secure.rs`; global single-value entries use `read_entry`/`delete_entry`
  (shared Claude token, gateway keys). Project secrets are restricted to `PROJECT_SECRET_KEYS`.
- Container start: `commands/project_commands.rs` `start_project_container` runs
  `docker::sync_bedrock_credentials` after start (≈:1448) — the pattern the marketplace sync follows.
- Exec/upload: `docker/exec.rs` `upload_bytes_to_container(container_id, dest_dir, file_name, data, mode)`,
  `exec_oneshot_streams_as(container_id, user, cmd, env)`, `create_attached_exec_as(…, tty, user)`.
  Container user is addressed as `"claude"`. Constant-script + env-data rule: header of
  `commands/inspect_commands.rs`.
- `container/entrypoint.sh` merges `CLAUDE_CODE_SETTINGS_JSON` into `~/.claude/settings.json`
  (≈:408-447), runs `claude update` under `flock /tmp/.triple-c-claude-update.lock` (≈:649),
  prints `Triple-C container ready.` and execs `su -s /bin/bash claude -c "exec sleep infinity"`
  (≈:654-655). Nothing marks readiness on disk; that final process is the observable signal
  (verified on a live container, where `jq`, `flock` and `tar` are also present).
- `commands/inspect_commands.rs` `list_container_capabilities` already inventories agents, skills,
  commands, hooks and plugins in a container (read-only); `CapabilityTiles.tsx` shows it.
- The container image has `gh`, `git`, `jq`. Claude Code 2.1.283 supports
  `claude plugin marketplace add <path>` / `update` / `remove` and
  `claude plugin install|uninstall <plugin>@<marketplace>`.
- No host-side git or GitHub auth exists today (no `git2`/`gix`; `reqwest` with rustls is present).
  `gix` 0.88 is current on crates.io.

## 1. Marketplace repo format

```
<repo>/
├── README.md
├── agents/<name>.md                    # Claude Code agent file (front matter: name, description)
├── skills/<name>/SKILL.md (+ files)    # Claude Code skill folder
├── commands/<name>.md                  # Claude Code slash command
├── hooks/<name>/hook.json (+ scripts)  # Triple-C hook manifest
└── plugins/
    ├── .claude-plugin/marketplace.json # standard Claude Code marketplace catalog
    └── <plugin>/…                      # standard Claude Code plugins
```

Every folder is optional; a repo with only `plugins/` is valid.

- **Agent** — any `agents/*.md`. Name/description from YAML front matter (`name` falls back to the
  file stem). Installs to `~/.claude/agents/<file>`.
- **Skill** — any `skills/<dir>/` containing `SKILL.md`; name/description from its front matter.
  Installs to `~/.claude/skills/<dir>/`.
- **Command** — any `commands/*.md`; description from front matter `description` if present,
  otherwise the first non-empty line. Installs to `~/.claude/commands/<file>`.
- **Hook** — `hooks/<dir>/hook.json`:
  ```json
  { "name": "notify-on-stop",
    "description": "Desktop ping when Claude finishes",
    "hooks": { "Stop": [{ "hooks": [{ "type": "command",
               "command": "${HOOK_DIR}/notify.sh" }] }] } }
  ```
  `hooks` is Claude Code's `settings.json` hooks object, verbatim. `${HOOK_DIR}` is substituted
  with the install folder `~/.claude/triple-c/hooks/<dir>` (absolute path). The whole folder is
  copied; files keep their executable bit from the git tree mode.
- **Plugin** — entries of `plugins/.claude-plugin/marketplace.json`; each `source` must be a
  relative path inside `plugins/` (remote sources are listed as invalid: they would fetch from
  inside the container, bypassing pinning and host-side auth).

Item identity: `(marketplace_id, kind, key)` where `key` is the file stem / folder name / plugin
name. Keys must match `^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$`.

Invalid items (bad front matter, unparsable `hook.json`, unknown hook event, key failing the
pattern, plugin source missing or outside `plugins/`, symlinks anywhere in an item) are listed with
the reason and cannot be installed; they never stop the rest of the marketplace from loading.

Limits (defensive, per item): 2 MiB total, 200 files.

## 2. Data model and storage

`AppSettings` (all `#[serde(default)]`):

```rust
pub marketplace_accounts: Vec<MarketplaceAccount>,
pub marketplaces: Vec<Marketplace>,
pub global_marketplace_installs: Vec<MarketplaceInstall>,

pub struct MarketplaceAccount {
    pub id: String,            // uuid
    pub label: String,         // user-facing, e.g. "Work GitHub"
    pub host: String,          // e.g. "github.com", "repo.anhonesthost.net"
    pub method: AccountMethod, // GhHost | GhContainer | Token
    pub username: Option<String>, // resolved at sign-in, display only
}
pub struct Marketplace {
    pub id: String,            // uuid; slug "mp-<id8>" used in container paths
    pub name: String,          // display only (renaming never changes the slug)
    pub url: String,           // https URL only
    pub branch: Option<String>,// None = remote default branch
    pub account_id: Option<String>, // None = anonymous
}
pub struct MarketplaceInstall {
    pub marketplace_id: String,
    pub kind: ItemKind,        // Agent | Skill | Command | Hook | Plugin
    pub key: String,
    pub commit: String,        // full hex object id
}
```

`Project` (all `#[serde(default)]`):

```rust
pub marketplace_installs: Vec<MarketplaceInstall>,     // project-only additions
pub marketplace_disabled: Vec<MarketplaceItemRef>,     // (marketplace_id, kind, key) opted out
```

**Secrets.** Token and GhContainer accounts keep their token in the keychain, one service per
account (`triple-c-marketplace-account-<id>`; new global helpers in `secure.rs` alongside the
gateway ones). GhHost accounts store nothing: every fetch runs `gh auth token --hostname <host>`
so a later `gh auth refresh`/logout on the host is honoured. Deleting an account deletes its entry.

**Effective set for a project** (pure function, unit-tested):
`(global − project.marketplace_disabled) ∪ project.marketplace_installs`, keyed by
`(marketplace_id, kind, key)`; on a key clash the project's entry (and pin) wins.

**Cache.** `<data_dir>/triple-c/marketplaces/<id>.git` — a bare `gix` clone. Content is read
from git objects at each install's pinned commit, never from a worktree, so different pins of the
same repo coexist. Pinned commits are protected from pruning by writing a ref per pin
(`refs/triple-c/pins/<commit>`), refreshed after every install/update/remove.

Removing a marketplace deletes its cache and its account link; installs that still reference it
stay in the lists and are shown as **source removed**. With no cache there is nothing to copy, so
the next sync removes those items from containers; the UI says so before the marketplace is
removed and offers "Forget" to drop the stale entries.

**Export/import.** Accounts (without secrets), marketplaces and install lists go into the existing
export as ordinary `AppSettings` fields; account tokens follow the existing encrypted-secrets
policy of `settings_export.rs` (`ExportedSecrets::marketplace_account_tokens`, keyed by account
id, restored to the keychain before the settings replace). Because the import is untrusted input
and not merely a restore, imported marketplaces and installs are validated on import the same way
the add-marketplace/install commands validate them (host, key pattern, pinned-commit shape), and
the confirmation preview warns whenever the import contains a global hook or global plugin install
— those skip the hook-confirm step an install from the Marketplace tab shows.

## 3. Fetching and signing in

**Fetch** (`marketplace/git.rs`): `gix` over HTTPS only (reject other URL schemes at add time).
Credentials are supplied through gix's credential callback, never written to disk: username
`x-access-token` for github.com, otherwise the account's `username` (falling back to `oauth2`),
password = token. Shallow fetch is not used (pins need history for diff/ancestry).

- **Add marketplace** = test fetch of the chosen branch; failures surface immediately.
- **Refresh**: when the Marketplace tab opens and the last fetch is > 15 min old, on the Refresh
  button, and once at app start (background, errors logged not toasted).
- **Offline / fetch error**: the last cache stays usable; UI shows "last fetched <time>" and the error.

**Update detection** compares each installed item's own tree (item folder / file blob id) at
its pin vs. the branch head; only a changed item shows "update available".
**Update** shows a text diff of the item's files (pinned → head) and, on accept, moves the pin.
Hooks' diffs always show the rendered commands. Install and update both carry the commit the user
reviewed (the head the item was read at, the head of the accepted diff); the backend pins exactly
that commit and refuses with "changed since you reviewed this item — review it again" if the
marketplace's head has moved since.

**Accounts** (`marketplace/auth.rs`):

- **GitHub via `gh` on host**: detect `gh` on PATH; `gh auth status --hostname <host>`; if not
  logged in, tell the user to run `gh auth login` (we do not drive the host's gh interactively).
  `gh api user --jq .login` for the display name.
- **GitHub via `gh` in a container** (no host `gh`): user picks a running project; Triple-C runs
  `gh auth login --hostname <host> --web --git-protocol ssh --skip-ssh-key --scopes repo` in an
  attached pty exec, with `GH_CONFIG_DIR` and `GIT_CONFIG_GLOBAL` both pointed at a temp dir
  (`$(mktemp -d)`) — `--git-protocol ssh --skip-ssh-key` avoids gh's "Authenticate Git with your
  GitHub credentials?" prompt, which under `https` would otherwise write a credential helper into
  `~/.gitconfig`. It surfaces the one-time code and URL in a dialog (same shape as
  `ClaudeAuthModal`), then runs `gh auth token` with the same config dir, stores the token in the
  keychain and `rm -rf`s the dir. Cancel tears the exec down. Nothing persists in the container,
  so Claude in that container is not logged into the user's GitHub.
- **Token**: pasted once, validated via the host's "who am I" API
  (GitHub `GET /user`, Gitea `GET /api/v1/user`, GitLab `GET /api/v4/user`; unknown host → test
  `ls-remote`-equivalent fetch), stored in the keychain. The token is never returned to the frontend.

**Access errors** (401/403/404 on fetch): message names the account used and lists the likely
org causes — org has not approved the GitHub CLI / third-party app restrictions, token not
SSO-authorised for the org, fine-grained token owned by a different owner — with the GitHub
settings link for each.

## 4. Installing into containers

**When.** (a) After every container start, in `start_project_container` next to
`sync_bedrock_credentials`; (b) on **Apply now** for running containers after the lists change.
The sync is idempotent; no container labels or recreation are involved. A container reset wipes
the volumes and the next start re-syncs.

**Readiness.** The entrypoint's last act is `exec su -s /bin/bash claude -c "exec sleep infinity"`,
so that process existing means the settings.json merge and `claude update` are done. The sync polls
`pgrep -x -f 'su -s /bin/bash claude -c exec sleep infinity'` (up to 180 s) before touching
anything. No entrypoint change is needed, which matters: image and entrypoint changes reach an
existing project only through a base-image migration or a Reset (CLAUDE.md "`/home/claude` in the
image is seed-only" and the VPN notes), so a marker written by a new entrypoint would never appear
in existing projects. Plugin commands additionally run under `flock /tmp/.triple-c-claude-update.lock`.

**Payload.** The host builds one tar of the effective set, read from the cache at each pin:

```
agents/<file>  skills/<dir>/…  commands/<file>  hooks/<dir>/…
plugins/.claude-plugin/marketplace.json   # generated: only selected plugins
plugins/<plugin>/…                         # each at its own pin
manifest.json                              # effective set: kind, key, marketplace, commit, hook JSON;
                                           # `held`: ids the host could not build this time (kept as is)
```

uploaded to `~/.claude/triple-c/marketplace/incoming/` together with the sync script itself and
applied by that **constant** script (data only via env and files) run as `claude`. The script lives
in the app (`app/src-tauri/src/marketplace/sync.sh`, embedded with `include_str!`) and is uploaded
on every sync rather than baked into the image, for the same reason as the readiness check: every
existing project gets it immediately and it is always the version that matches the app. Results
come back as JSON on stdout.

**Script behaviour.** State lives in `~/.claude/triple-c/marketplace/state.json` (what Triple-C
installed last time, including the exact hook entries it inserted).

- Agents / skills / commands: copy into place; delete the ones in state but no longer in the
  manifest. A destination that exists but is **not** in state is the user's own file → skip and
  report a conflict, never overwrite.
- Hooks: copy folder to `~/.claude/triple-c/hooks/<dir>/`; with `jq`, remove from
  `~/.claude/settings.json` exactly the entries recorded in state, then append the new rendered
  entries and record them. User-authored hook entries are never touched.
- Plugins: marketplace name `triple-c-<slug>`; copy the generated tree to
  `~/.claude/triple-c/plugins/<slug>/`; `claude plugin marketplace add` it the first time, else
  `claude plugin marketplace update triple-c-<slug>`; `install` newly selected, `uninstall`
  removed; drop the marketplace registration when it has no plugins left. Plugin state is kept
  per marketplace (`plugin:<slug>/<key>`), so two marketplaces may ship a plugin of the same
  name; older `plugin:<key>` records are migrated using their recorded slug.
- Emits a report: `{installed, updated, removed, skipped: [{item, reason}], errors: [...]}`.

**Failure handling.** A sync failure never fails the container start; it is logged, stored as the
project's last sync report, and toasted. UI copy states that changes apply to **new** Claude
sessions.

## 5. UI

**Settings sidebar → Marketplace section** (new `AccordionSection`): counts (marketplaces, global
installs, updates available) and **Open Marketplace**, which opens/focuses the singleton main-area
tab `marketplace`.

**Marketplace tab** (`components/marketplace/`), sub-tabs:

- **Browse** — marketplace list (Add, account, last fetched, Refresh) on the left; kind filter
  (Agents / Skills / Commands / Hooks / Plugins) and search; item detail pane with the content
  preview (agent/command/skill markdown, hook commands, plugin component list) and install
  controls: an **All projects** switch plus a per-project checkbox list showing each project's
  state (inherited, opted out, project-only, pinned to a different commit). Installing a hook
  requires a confirm step listing every command it will run.
- **Installed** — every install (global and per project) with update badges; **Update** opens
  the diff; **Apply now** syncs running containers.
- **Accounts** — add GitHub (`gh`) or token account, test, remove.

**Project Home → Config → Marketplace section**: the project's effective set with source
(global / project), switches to opt out of global items, last sync report, and a link that opens
the Marketplace tab filtered to this project.

All new commands follow the three-step new-command convention and are main-window only
(`capabilities/default.json`); no new window, so no AppManifest/`expected_windows` changes.

## Testing

- **Rust unit**: repo parsing (valid + each invalid case per kind), effective-set merge,
  per-item change detection, `${HOOK_DIR}` rendering, tar building, pin refs, `gix` fetch against a
  local fixture repo created in the test, credential selection per host/method, error mapping for
  401/403/404.
- **Sync script**: driven from a Rust test with a temp `HOME` and a stub `claude` on `PATH`:
  install, update, removal, conflict with a user-owned agent, user hooks left intact, plugin
  add/update/uninstall calls. Skipped when `jq` is unavailable.
- **Vitest**: Marketplace tab (browse/filter/install controls/states), Config section opt-out,
  account dialogs, hook confirm step.
- **End to end** on a preview build against `shadowdao/triple-c-marketplace`.

## Starter marketplace repo

`/workspace/projects/triple-c-marketplace` → public `github.com/shadowdao/triple-c-marketplace`:
README documenting the format above; `agents/code-reviewer.md`; `skills/example-skill/SKILL.md`;
`commands/example-command.md`; `hooks/notify-on-stop/` (hook.json + script); `plugins/` with a
catalog and one single-skill plugin. Also used as the end-to-end fixture.

## Also in this branch

`SharedAuthSettings.tsx`: the action button row wraps (`flex-wrap`) so "Check snapshot images"
stays inside the sidebar (commit `ece0d74`).
