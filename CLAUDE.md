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

- **`store/appState.ts`** — Single Zustand store for all app state (projects, sessions, UI)
- **`hooks/`** — All Tauri IPC calls are encapsulated in hooks (`useTerminal`, `useProjects`, `useDocker`, `useSettings`)
- **`lib/tauri-commands.ts`** — Typed `invoke()` wrappers; TypeScript types in `lib/types.ts` must match Rust models
- **`components/terminal/TerminalView.tsx`** — xterm.js integration with WebGL rendering, URL detection for OAuth flow
- **`components/layout/`** — TopBar (tabs + status), Sidebar (project list), StatusBar
- **`components/projects/`** — ProjectCard, ProjectList, AddProjectDialog
- **`components/settings/`** — Settings panels for API keys, Docker, AWS

### Backend Structure (`app/src-tauri/src/`)

- **`commands/`** — Tauri command handlers (docker, project, settings, terminal). These are the IPC entry points called by `invoke()`.
- **`docker/`** — Docker API layer using bollard:
  - `client.rs` — Singleton Docker connection via `OnceLock`
  - `container.rs` — Container lifecycle (create, start, stop, remove, inspect)
  - `exec.rs` — PTY exec sessions with bidirectional stdin/stdout streaming
  - `image.rs` — Image build/pull with progress streaming
- **`models/`** — Serde structs (`Project`, `AuthMode`, `BedrockConfig`, `OllamaConfig`, `LiteLlmConfig`, `ContainerInfo`, `AppSettings`). These define the IPC contract with the frontend.
- **`storage/`** — Persistence: `projects_store.rs` (JSON file with atomic writes), `secure.rs` (OS keychain via `keyring` crate), `settings_store.rs`

### Container (`container/`)

- **`Dockerfile`** — Ubuntu 24.04 base with Claude Code, Node.js 22, Python 3.12, Rust, Docker CLI, git, gh, AWS CLI v2, ripgrep, pnpm, uv, ruff pre-installed
- **`entrypoint.sh`** — UID/GID remapping to match host user, SSH key setup, git config, docker socket permissions, then `sleep infinity`
- **`triple-c-scheduler`** — Bash-based scheduled task system for recurring Claude Code invocations

### Container Lifecycle

Containers use a **stop/start** model (not create/destroy). Installed packages persist across stops. The `.claude` config dir uses a named Docker volume (`triple-c-claude-config-{projectId}`) so OAuth tokens survive even container resets.

### Authentication

Per-project, independently configured:
- **Anthropic (OAuth)** — `claude login` in terminal, token persists in config volume
- **AWS Bedrock** — Static keys, profile, or bearer token injected as env vars
- **Ollama** — Connect to a local or remote Ollama server via `ANTHROPIC_BASE_URL` (e.g., `http://host.docker.internal:11434`)
- **LiteLLM** — Connect through a LiteLLM proxy gateway via `ANTHROPIC_BASE_URL` + `ANTHROPIC_AUTH_TOKEN` to access 100+ model providers

## Styling

- **Tailwind CSS v4** with the Vite plugin (`@tailwindcss/vite`). No separate tailwind config file.
- All colors use CSS custom properties in `index.css` `:root` (e.g., `--bg-primary`, `--text-secondary`, `--accent`)
- `color-scheme: dark` is set on `:root` for native dark-mode controls
- **Do not** add a global `* { padding: 0 }` reset — Tailwind v4 uses CSS `@layer`, and unlayered CSS overrides all layered utilities

## Key Conventions

- Frontend types in `lib/types.ts` must stay in sync with Rust structs in `models/`
- Tauri commands are registered in `lib.rs` via `.invoke_handler(tauri::generate_handler![...])`
- Tauri v2 permissions are declared in `capabilities/default.json` — new IPC commands need permission grants there
- The `projects.json` file uses atomic writes (write to `.tmp`, then `rename()`). Corrupted files are backed up to `.bak`.
- Cross-platform paths: Docker socket is `/var/run/docker.sock` on Linux/macOS, `//./pipe/docker_engine` on Windows

## Testing

Frontend tests use Vitest with jsdom environment and React Testing Library. Setup file at `src/test/setup.ts`. Run a single test file:
```bash
cd app
npx vitest run src/path/to/test.test.ts
```
