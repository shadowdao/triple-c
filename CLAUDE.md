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
- **`docker/`** — Docker API layer using bollard:
  - `client.rs` — Singleton Docker connection via `OnceLock`
  - `container.rs` — Container lifecycle (create, start, stop, remove, inspect)
  - `exec.rs` — Attached exec streaming. `create_attached_exec()` is the **single** place an
    attached exec is opened; terminal sessions and the auth bridge both go through it.
  - `image.rs` — Image build/pull with progress streaming
  - `gateway.rs` — Optional LiteLLM sibling container giving Claude Code an Anthropic-format
    front end for providers that only speak OpenAI (see `gateway-container/`). Mirrors `stt.rs`.
    Binds `0.0.0.0` — unlike STT — because *project containers*, not the host process, consume
    it; it therefore **always** sets a LiteLLM `master_key`, since LiteLLM without one accepts
    any key.
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

- **`Dockerfile`** — Ubuntu 24.04 base with Claude Code, Node.js 22, Python 3.12, Rust, Docker CLI, git, gh, AWS CLI v2, ripgrep, pnpm, uv, ruff pre-installed
- **`entrypoint.sh`** — UID/GID remapping to match host user, SSH key setup, git config, docker socket permissions, Claude Code settings.json injection, then `sleep infinity`
- **`triple-c-scheduler`** — Bash-based scheduled task system for recurring Claude Code invocations

### Container Lifecycle

Containers use a **stop/start** model (not create/destroy). Installed packages persist across stops. The `.claude` config dir uses a named Docker volume (`triple-c-claude-config-{projectId}`), nested inside the home volume (`triple-c-home-{projectId}`), so OAuth tokens and Claude Code config survive container stop/start *and* container recreation.

**Reset is the exception and it is destructive.** `rebuild_project_container` calls
`remove_project_volumes`, which deletes *both* volumes — so a Reset wipes `~/.claude`,
`~/.claude.json`, the OAuth credential, installed skills, and session transcripts. That is
intentional (Reset exists to get back to a clean base image), but do not describe Reset as
preserving credentials.

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
