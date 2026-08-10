# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

Triple-C (Claude-Code-Container) is a Tauri v2 desktop application that sandboxes Claude Code inside Docker containers. It has two main parts: a React/TypeScript frontend, a Rust backend, and a Docker container image definition.

## Build & Development Commands

All frontend/tauri commands run from the `app/` directory:

```bash
cd app
npm ci                    # Install dependencies (required first time)
npx tauri dev             # Launch app in dev mode with hot reload (Vite on port 1420)
npx tauri build           # Production build (outputs to src-tauri/target/release/bundle/)
npm run build             # Frontend-only build (tsc + vite)
npm run test              # Run Vitest once
npm run test:watch        # Run Vitest in watch mode
```

Rust backend is compiled automatically by `tauri dev`/`tauri build`. To check Rust independently:
```bash
cd app/src-tauri
cargo check               # Type-check without full build
cargo build               # Build Rust backend only
```

Container image:
```bash
docker build -t triple-c-sandbox ./container
```

### Linux Build Dependencies (Ubuntu/Debian)
```bash
sudo apt-get install -y libgtk-3-dev libwebkit2gtk-4.1-dev libayatana-appindicator3-dev librsvg2-dev libsoup-3.0-dev patchelf libssl-dev pkg-config build-essential
```

## Architecture

### Two-Process Model (Tauri IPC)

- **React frontend** (`app/src/`) renders UI in the OS webview
- **Rust backend** (`app/src-tauri/src/`) handles Docker API, credential storage, and terminal I/O
- Communication uses two patterns:
  - `invoke()` — request/response for discrete operations (CRUD, start/stop containers)
  - `emit()`/`listen()` — event streaming for continuous data (terminal I/O)

### Terminal I/O Flow

```
User keystroke → xterm.js onData() → invoke("terminal_input") → mpsc channel → docker exec stdin
docker exec stdout → tokio task → emit("terminal-output-{sessionId}") → listen() → xterm.js write()
```

### Frontend Structure (`app/src/`)

- **`store/appState.ts`** — Single Zustand store for all app state (projects, sessions, UI). The
  main area is a single ordered tab strip holding two tab kinds, keyed `term:<id>` and
  `home:<id>`; `activeSessionId` is *derived* from `activeTabKey` so exactly one thing is current.
- **`hooks/`** — All Tauri IPC calls are encapsulated in hooks (`useTerminal`, `useProjects`, `useDocker`, `useSettings`)
- **`lib/tauri-commands.ts`** — Typed `invoke()` wrappers; TypeScript types in `lib/types.ts` must match Rust models
- **`components/terminal/TerminalView.tsx`** — xterm.js integration with WebGL rendering, URL detection for OAuth flow
- **`components/layout/`** — TopBar, MainTabs (the unified tab strip), Sidebar, StatusBar
- **`components/projects/`** — `ProjectRow` (select-only list row), `ProjectList`, `AddProjectDialog`,
  and the editors reused by Project Home
- **`components/projects/home/`** — **Project Home**, the main-area view for a project:
  Overview / Sessions / Automation / Config / Files. Per-project configuration lives here, not in
  modals — see "UI conventions" below.
- **`components/settings/`** — Host-level settings: Docker, AWS, Web Terminal, STT, shared auth
- **`components/ui/`** — Shared primitives. **Use these; do not hand-roll replacements.**
  `Modal` (the only correct way to build a dialog — it supplies `role="dialog"`, `aria-modal`,
  focus trap and restore), `Button`, `Toggle`, `Field`, `SegmentedControl`, `StatusIndicator`,
  `SaveIndicator`, `OverflowMenu`, `ToastHost`, `Tooltip`

### UI conventions

- **Project config belongs in Project Home's Config tab, not a modal.** Modals are reserved for
  short, genuinely modal tasks (add project, confirm removal, token acquisition). The app
  previously had ~12 hand-rolled modals; they were consolidated deliberately.
- **Never bypass the design tokens.** All colour comes from CSS custom properties in `index.css`.
  Filled buttons use `--accent-emphasis` (not `--accent`, which fails WCAG AA against white).
  Use `--text-disabled` rather than `disabled:opacity-50`.
- **Never write `focus:outline-none`.** A global `:focus-visible` ring is defined in `index.css`.
- **Status must not be encoded in colour alone** — `StatusIndicator` pairs a glyph with a word.
- Keyboard: `Ctrl+T` new terminal, `Ctrl+Shift+W` close tab, `Ctrl+Tab` cycle, `Ctrl+1..9` jump.
  `Ctrl+W` is intentionally left alone — it is readline's `kill-word` inside the terminal.

### Backend Structure (`app/src-tauri/src/`)

- **`commands/`** — Tauri command handlers. These are the IPC entry points called by `invoke()`.
  Beyond docker/project/settings/terminal: `inspect_commands.rs` (read-only views into a
  container — Claude sessions, installed capabilities, scheduler tasks), `auth_bridge_commands.rs`,
  `auth_token_commands.rs`.
- **`auth_bridge/`** — Host-side loopback bridge so browser logins run *inside* a container can
  complete against the host browser. Discovers listeners by parsing `/proc/net/tcp{,6}` (the image
  has no `ss`/`netstat`/`lsof`), binds host `127.0.0.1` **only**, and tunnels in over the Docker
  API via `socat`. Opt-in per project.
- **`browser_view/`** — Watch and take over the browser Claude drives with Playwright inside the
  container. Runs Playwright's own dashboard (`browser.bind()` + `playwright-cli show`) in the
  container and fronts it with a **token-gated** loopback proxy. Deliberately does **not** reuse
  the auth bridge's `PortForward`, which binds an unauthenticated port — fine for a throwaway
  OAuth listener, wrong for remote control of a browser. Host ports are confined to
  `47820..=47827` because CSP `frame-src` cannot express a port range and must enumerate them;
  a unit test asserts the Rust range matches `tauri.conf.json`. Opt-in per project.
  - **Detection has to look past `node_modules`.** `claude mcp add … npx @playwright/mcp@latest`
    installs into `~/.npm/_npx/<hash>/node_modules`, not any `node_modules`, so `detect.rs`
    globs that cache as well as `/workspace`, `$HOME/node_modules` and `npm root -g`. It also
    hops from a wrapper `playwright` to its **nested** `playwright-core`: verified that npm does
    not hoist for global installs, and the wrapper ships no `types/types.d.ts`, so reading the
    wrapper alone reports a current build as "predates `browser.bind()`".
  - **`@playwright/mcp` can never satisfy this pane.** It bundles a `playwright-core` that binds,
    but never `@playwright/cli`, which is the viewer. Never offer it as a setup route — only as
    what binds sessions automatically once Playwright is present.
  - **`install.rs` installs into `/workspace`, as `claude`, with `--no-save`.** `/workspace` is
    *not* a bind mount — project directories are mounted at `/workspace/{mount_name}` — so this
    touches nothing of the user's, needs no sudo (npm's prefix is `/usr`, which is root-owned),
    and is on the module resolution path for scripts in the project. Browsers go to
    `~/.cache/ms-playwright` as `claude`, i.e. the home volume.
  - **Current base images ship Chromium's shared libraries; older ones do not** — and a project
    keeps the base image it was first built from until it is migrated, so "older" is the normal
    case. Without them `playwright install chromium` downloads a browser that cannot launch, which
    is why installing Chrome via apt looks like a fix. `install.rs` asks
    `install-deps --dry-run` first and skips the apt step when the answer is "all present",
    *saying so* in the progress stream. Do not decide this by probing for library names: the
    dry-run simulates the same `apt-get install` the fix would run, so check and fix cannot
    disagree about what the dependency set is. Note that `--dry-run` exits **0** both when
    everything is installed and when Playwright has no list for the platform — match on its
    output, not its exit code. Either way the action ends by *actually launching* the browser to
    verify. `@playwright/mcp` wants the `chrome` **channel** specifically, so both browsers are
    offered.
- **`docker/`** — Docker API layer using bollard:
  - `client.rs` — Singleton Docker connection via `OnceLock`
  - `container.rs` — Container lifecycle (create, start, stop, remove, inspect)
  - `exec.rs` — Attached exec streaming. `create_attached_exec()` is the **single** place an
    attached exec is opened; terminal sessions and the auth bridge both go through it.
  - `image.rs` — Image build/pull with progress streaming
  - `gateway.rs` — Optional LiteLLM sibling container giving Claude Code an Anthropic-format
    front end for providers that only speak OpenAI (see `gateway-container/`). Mirrors `stt.rs`.
    Its bind address is **detected, never `0.0.0.0`** — unlike STT, *project containers* consume
    it, so loopback alone is not always enough: Docker Desktop gets `127.0.0.1` (containers reach
    it via `host.docker.internal`), native Linux gets the default bridge gateway (`172.17.0.1`).
    `GatewayBinding` derives the bind address and the advertised `base_url` together so they
    cannot drift. A wildcard bind would be LAN-reachable — Docker's rules precede host firewalls —
    in front of a container config holding a billed provider key. It also **always** sets a
    LiteLLM `master_key`, since LiteLLM without one accepts any key.
  - `migration.rs` — Base-image migration: manifest capture via throwaway containers, the pure
    delta computation (dpkg-ownership filter, bind-mount exclusion, verbatim-copy set), and the
    crash-recovery state machine. See "Base-image migration" below.
  - `legacy_cleanup.rs` — One-release migration shim removing leftovers from the deleted MCP
    feature (containers labelled `triple-c.mcp-server`, `triple-c-net-*` networks). Deletable once
    users have migrated.
- **`web_terminal/`** — Remote terminal access via axum HTTP+WebSocket server:
  - `server.rs` — Axum server lifecycle (start/stop), serves embedded HTML and handles WS upgrades
  - `ws_handler.rs` — Per-connection WebSocket handler with JSON protocol, session management, cleanup on disconnect
  - `terminal.html` — Self-contained xterm.js web UI embedded via `include_str!()`
- **`models/`** — Serde structs (`Project`, `Backend`, `BedrockConfig`, `OllamaConfig`, `LlamaCppConfig`, `OpenAiCompatibleConfig`, `ClaudeCodeSettings`, `ContainerInfo`, `AppSettings`, `WebTerminalSettings`). These define the IPC contract with the frontend.
- **`storage/`** — Persistence: `projects_store.rs` (JSON file with atomic writes), `secure.rs` (OS keychain via `keyring` crate), `settings_store.rs`

### Container (`container/`)

- **`Dockerfile`** — Ubuntu 24.04 base with Claude Code, Node.js 22, Python 3.12, Rust, Docker CLI, git, gh, AWS CLI v2, ripgrep, pnpm, uv, ruff pre-installed, plus the shared
  libraries a browser links against (see below)
- **Browser runtime libraries are baked in; browser *binaries* are not.** A layer runs
  `npx --yes playwright@latest install-deps chromium` as root, so Playwright names its own
  dependencies and the list cannot rot against Ubuntu 24.04's `t64` renames or a new Chromium
  dependency. Measured: +99 packages, +334 MiB unpacked / +119 MiB compressed, on both arches. Do
  not replace it with a hand-written apt list without pinning the Playwright version you derived
  it from — a `chromium`-only list saves ~94 MiB (Playwright's `tools` group: xvfb and the CJK
  fonts) and nothing more, because `libgbm1` → `mesa-libgallium` → `libllvm20` is ~213 MiB that
  no trimming removes.
  - The `install-deps --dry-run` call after it is a **build-time assertion, not decoration**: on a
    platform Playwright's table does not cover, `install-deps` prints a warning and returns having
    installed nothing **with exit status 0**. Without the assertion that ships a broken image
    behind a clean build log.
  - Baking the libraries but not the browsers is the whole point of the split. Browsers live in
    `~/.cache/ms-playwright` (home volume) and already survive recreation *and* migration; a
    runtime `apt-get install` of the libraries lands in the writable layer, is re-paid after every
    Reset, and is **lost on base-image migration**, which replays apt from a manifest. The runtime
    approach converges on the worst state: a 400 MB browser present with its libraries gone.
  - The layer sits immediately after Node (npx is its only prerequisite) and well above the shim
    `COPY`s, so editing a shim does not re-run a multi-hundred-megabyte apt install.
- **`entrypoint.sh`** — UID/GID remapping to match host user, SSH key setup, git config, docker socket permissions, Claude Code settings.json injection, then `sleep infinity`
- **`triple-c-scheduler`** — Bash-based scheduled task system for recurring Claude Code invocations

**`/home/claude` in the image is seed-only.** It is the mount point of the named volume
`triple-c-home-{projectId}`, so after a project's *first* start the image's copy of that directory
is masked permanently and can never be updated again. A change you make under `/home/claude` in
the `Dockerfile` or in `entrypoint.sh`'s "copy this into the home dir" style reaches **new
projects only** — existing ones will never see it, with or without a base-image migration.

So: **anything that must stay upgradable belongs in `/usr/local/bin` or `/opt`, or must be seeded
by `entrypoint.sh` at runtime** (i.e. written on every start, from a source outside the home
volume, the way `CLAUDE_INSTRUCTIONS` → `~/.claude/CLAUDE.md` and the Mission Control skill copy
already are). Putting it in the image's `/home/claude` and expecting an image update to deliver it
is the mistake.

The flip side is the useful half of the same fact: Claude Code itself (`~/.local/bin`), cargo, uv,
ruff, the OAuth login, `~/.claude.json`, skills, transcripts, scheduler tasks and SSH keys all
re-attach for free when a container is recreated from a *different* image — which is what makes
base-image migration cheap.

### Container Lifecycle

Containers use a **stop/start** model (not create/destroy). Installed packages persist across stops. The `.claude` config dir uses a named Docker volume (`triple-c-claude-config-{projectId}`), nested inside the home volume (`triple-c-home-{projectId}`), so OAuth tokens and Claude Code config survive container stop/start *and* container recreation.

**Reset is the exception and it is destructive.** `rebuild_project_container` calls
`remove_project_volumes`, which deletes *both* volumes — so a Reset wipes `~/.claude`,
`~/.claude.json`, the OAuth credential, installed skills, and session transcripts. That is
intentional (Reset exists to get back to a clean base image), but do not describe Reset as
preserving credentials.

### Base-image migration (`docker/migration.rs`, `commands/migration_commands.rs`)

A container is created from `triple-c-snapshot-{projectId}:latest` whenever that image exists, and
every recreation re-commits it — so without an explicit act, a project stays on the base image it
was first built from **forever** and never picks up a new `socat`, a new `/usr/local/bin` shim or a
security update. Migration is the non-destructive way out; Reset is the destructive one.

- **Staleness is a surfaced signal, not an automatic trigger.** `triple-c.base-image-id` records
  the lineage but is deliberately **not** compared in `container_needs_recreation` — see the long
  comment there. Comparing it would recreate every project *from its own snapshot* on the next base
  bump: churn on the old base, and it would consume the "you should migrate" signal without
  migrating. `get_container_staleness` surfaces it; `migrate_project_to_base` acts on it.
- **A missing lineage label means "unknown, probe instead", never "stale".**
- **`:latest` keeps pointing at the old lineage until the final commit.** That is what makes every
  crash before that point self-heal — `start_project_container` just recreates from the old
  snapshot. After the container swap, the new container's `triple-c.migration-state=in-progress`
  label plus the persisted state file let `reconcile_project_statuses` offer resume or rollback.
- **Rollback restores the system layer only.** The volumes are never touched at any point, so work
  done in `$HOME` during a migrated session survives a rollback. Say so in any UI copy.
- **`/var` is never copied either, and that is the one way migration is *more* destructive than
  the ordinary recreate.** A recreate builds from the project's snapshot, so `/var/lib/postgresql`
  rides along; a migration builds from the base and the apt replay hands back an empty cluster.
  Copying a live database's files onto a different base's version of the same package is a
  corruption risk, not a fix — so the answer is disclosure. `unpreserved_data()` reports
  first-level directories under `/var/lib` and `/var/www` that the base does not ship *and* that
  hold non-dpkg-owned files (which is what keeps `/var/lib/apt` and `/var/lib/dpkg` out of it),
  and the pre-flight, the banner and the finished report all name them. Do not make this silent.
- **The rollback pin is not best-effort.** After `commit_container_snapshot` the commit is the only
  copy of the old system layer, so a `docker tag` that fails — or succeeds without the reference
  resolving — aborts the migration before `remove_container`. Same rule in reverse for
  `rollback_migration`: the image is confirmed to exist before the container is destroyed.
- **`resume` must check the container's `triple-c.migration-state` label**, exactly as
  `reconcile_migration` does. Without it a record left behind by a failed commit "resumes" into
  the *old, unmigrated* container and commits it as migrated.
- **Anything that stops, removes or recreates a project's container consults
  `migration_commands::is_migrating`.** The window between `remove_container` and the create that
  follows looks exactly like "no container" to Start, and Reset would delete the volumes out from
  under a live run.
- **`/etc` is never copied**, only reported: the snapshot lineage has
  `/etc/apt/sources.list.d/nodesource.sources` where the current base has `nodesource.list`, and
  having both breaks every `apt-get update` on a duplicate source. Verified, not theoretical.
- **`docker diff` is useless here** — on a snapshot-derived container it reports only changes since
  the last commit. Migration diffs two filesystem manifests instead, filtered through dpkg
  ownership and presence-in-the-new-base. Measured on a real project, that turns 8,677 raw path
  differences into 2 genuinely user-authored ones.

### Authentication

Per-project, independently configured:
- **Anthropic (OAuth)** — `claude login` in terminal, token persists in config volume
- **AWS Bedrock** — Static keys, profile, or bearer token injected as env vars
- **Ollama** — Connect to a local or remote Ollama server via `ANTHROPIC_BASE_URL` (e.g., `http://host.docker.internal:11434`)
- **llama.cpp** — Connect to a local or remote `llama-server` via `ANTHROPIC_BASE_URL` (e.g., `http://host.docker.internal:8080`, its default port)
- **OpenAI Compatible** — Connect through a gateway implementing the **Anthropic Messages API** (LiteLLM) via `ANTHROPIC_BASE_URL` + `ANTHROPIC_AUTH_TOKEN`

**Claude Code only ever speaks the Anthropic Messages API** (`POST /v1/messages?beta=true`) to
`ANTHROPIC_BASE_URL` — never OpenAI's `/v1/chat/completions`. Ollama and llama.cpp implement
`/v1/messages` natively, which is why each gets a plain base-URL backend with no translation shim.
A server that only exposes an OpenAI-shaped API does not work behind any backend.

For every backend pointing at a custom endpoint (`Backend::uses_custom_endpoint`), all four
`ANTHROPIC_DEFAULT_{OPUS,SONNET,HAIKU,FABLE}_MODEL` vars are pinned to the backend's configured
model id, with an optional per-backend Haiku override. Without this, Claude Code's background
calls resolve `haiku` to an Anthropic model id the local server does not have and fail silently.
Anthropic and Bedrock deliberately keep Claude Code's own defaults.
`ANTHROPIC_SMALL_FAST_MODEL` is deprecated and must not be used.

## Styling

- **Tailwind CSS v4** with the Vite plugin (`@tailwindcss/vite`). No separate tailwind config file.
- All colors use CSS custom properties in `index.css` `:root` (e.g., `--bg-primary`, `--text-secondary`, `--accent`)
- `color-scheme: dark` is set on `:root` for native dark-mode controls
- **Do not** add a global `* { padding: 0 }` reset — Tailwind v4 uses CSS `@layer`, and unlayered CSS overrides all layered utilities

## Key Conventions

- Frontend types in `lib/types.ts` must stay in sync with Rust structs in `models/`
- Tauri commands are registered in `lib.rs` via `.invoke_handler(tauri::generate_handler![...])`
- `capabilities/default.json` grants permissions for **plugin** commands only (`core:`, `dialog:`,
  `store:`, `opener:`). Application commands registered through `generate_handler!` do **not**
  need an entry there — adding one is not required and none exists for any app command.
- The `projects.json` file uses atomic writes (write to `.tmp`, then `rename()`). Corrupted files are backed up to `.bak`.
- **Adding project state that changes the container?** `container_needs_recreation()` is entirely
  **label-based** — it does not diff the container's env. If a new setting affects the container's
  environment or configuration, you must also write a corresponding `triple-c.*` label at creation
  and compare it there, or the change will silently not take effect until some unrelated setting
  forces a rebuild. Never put a secret in a label; labels are readable via `docker inspect`.
  (`triple-c.base-image-id` is the one deliberate exception — it is written but not compared; the
  reasoning is in the comment beside the check.)
- **Always write a `triple-c.*` label explicitly, even when the value is empty.** Docker merges an
  image's labels into a container's at creation, and `docker commit` copies container labels onto
  the snapshot image — so a label stamped once rides that snapshot into *every* future container
  forever. Verified on this host, and it is not hypothetical: `triple-c.mcp-fingerprint` has not
  been written by any code since the MCP feature was removed, yet a snapshot image was found still
  carrying a non-empty one, which made its one-shot recreation shim recreate that project on every
  single start. Writing the key explicitly overrides the inherited value — the same defence
  `MANAGED_AUTH_KEYS` applies to env vars.
- **New model fields need an explicit serde default when the correct default isn't the zero value.**
  `#[serde(default)]` on a `bool` yields `false`; follow the `default_full_permissions` pattern in
  `models/project.rs` for anything that should default to true.
- Cross-platform paths: Docker socket is `/var/run/docker.sock` on Linux/macOS, `//./pipe/docker_engine` on Windows

## Testing

Frontend tests use Vitest with jsdom environment and React Testing Library. Setup file at `src/test/setup.ts`. Run a single test file:
```bash
cd app
npx vitest run src/path/to/test.test.ts
```
