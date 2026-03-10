# Triple-C Technical Architecture

## Overview

Triple-C (Claude-Code-Container) sandboxes Claude Code inside Docker containers so that when running with `--dangerously-skip-permissions`, Claude only has access to files and projects you explicitly provide. The project consists of two components: a **Docker container image** pre-loaded with development tools, and a **cross-platform desktop application** for managing project containers, terminal sessions, and authentication.

---

## Why These Technologies

### Tauri v2 (Desktop Application Framework)

**Chosen over:** Electron, native GUI toolkits (Qt, GTK), web-only approach

Tauri uses a Rust backend paired with a web-based frontend rendered by the OS-native webview (WebKitGTK on Linux, WebKit on macOS, WebView2 on Windows). This gives us:

- **Small binary size** — Tauri apps ship at ~5-10 MB vs. Electron's ~150+ MB because there's no bundled Chromium. The OS webview is reused.
- **Native performance** — The backend is compiled Rust. Docker API calls, PTY streaming, and file I/O all happen in native code, not in a JavaScript runtime.
- **Cross-platform from one codebase** — Builds for Linux, macOS, and Windows from the same source. Tauri handles platform differences (file dialogs, system tray, window management).
- **Security model** — Tauri v2 uses a capabilities system where frontend code must be explicitly granted permission to access system features (filesystem, events, shell). This prevents the webview from doing anything not listed in `capabilities/default.json`.
- **Mature plugin ecosystem** — First-party plugins for OS dialog pickers (`tauri-plugin-dialog`), secure storage (`tauri-plugin-store`), and URL opening (`tauri-plugin-opener`) saved significant development time.

### React 19 + TypeScript (Frontend)

**Chosen over:** Svelte, Vue, Solid, vanilla JS

- **Ecosystem maturity** — React has the largest library ecosystem. The xterm.js terminal emulator, which is central to our app, has well-documented React integration patterns.
- **TypeScript** — Enforces type safety across the frontend, particularly important for the Tauri IPC boundary where `invoke()` calls must match Rust command signatures exactly.
- **Hooks-based architecture** — React hooks (`useTerminal`, `useProjects`, `useDocker`, `useSettings`) encapsulate all Tauri IPC calls, keeping components focused on rendering.
- **Concurrent rendering** — React 19's concurrent features prevent terminal I/O from blocking UI updates in the sidebar or settings panels.

### Zustand (State Management)

**Chosen over:** Redux, React Context, Jotai, MobX

- **Minimal boilerplate** — A single `create()` call defines the entire store. No providers, reducers, or action creators needed.
- **Direct mutation-style API** — `set({ projects })` is simpler than Redux dispatch patterns, which matters when state updates come from both user actions and async Tauri events.
- **No context provider** — Zustand stores live outside the React tree, so any component can access state without prop drilling or provider nesting. Terminal sessions, project lists, and UI state all share one store without performance penalties.
- **Small footprint** — ~1 KB gzipped. The app is already bundling xterm.js (~300 KB), so keeping other dependencies small matters.

### Tailwind CSS v4 (Styling)

**Chosen over:** CSS modules, styled-components, vanilla CSS

- **Rapid iteration** — Utility classes (`flex`, `gap-4`, `rounded-lg`) allow UI adjustments without switching between files. Padding, spacing, and layout changes happen inline.
- **Dark theme via CSS variables** — The app uses CSS custom properties (`--bg-primary`, `--text-secondary`, `--accent`) defined in `index.css`. Tailwind's arbitrary value syntax (`bg-[var(--bg-primary)]`) bridges utility classes with the theme system.
- **No runtime cost** — Tailwind v4 compiles to static CSS at build time. No JavaScript style injection at runtime.
- **Consistent spacing/sizing** — Tailwind's spacing scale (`p-6` = 24px, `gap-4` = 16px) enforces visual consistency without manual pixel calculations.

### xterm.js (Terminal Emulator)

**Chosen over:** Building a custom terminal renderer, using an iframe-based terminal

- **Full VT100/xterm compatibility** — Claude Code uses ANSI escape sequences for colors, cursor movement, line clearing, and interactive prompts. xterm.js handles all of these correctly, including 256-color and truecolor support.
- **WebGL renderer** — The `@xterm/addon-webgl` addon renders the terminal using WebGL for hardware-accelerated text drawing. This is critical for smooth scrolling when Claude outputs large amounts of text.
- **Fit addon** — `@xterm/addon-fit` automatically calculates terminal dimensions (cols/rows) from the container element size. Combined with a `ResizeObserver`, the terminal re-fits when the window or panel is resized, and the backend `docker exec` session is resized to match via `resize_exec()`.
- **Web links addon** — `@xterm/addon-web-links` makes URLs in terminal output clickable. Combined with `tauri-plugin-opener`, clicked URLs open in the host browser — essential for the `claude login` OAuth flow where Claude prints an authentication URL that must be opened on the host.
- **Bidirectional data flow** — xterm.js exposes `term.onData()` for user keystrokes and `term.write()` for incoming data. This maps directly to our Tauri event-based streaming architecture.

### bollard (Docker API)

**Chosen over:** Shelling out to the `docker` CLI, dockerode (Node.js), docker-api (Python)

- **Native Rust** — bollard is a pure Rust Docker API client. It communicates directly with the Docker daemon over the Unix socket (`/var/run/docker.sock`) or Windows named pipe (`//./pipe/docker_engine`). No subprocess spawning, no CLI output parsing.
- **Async/streaming** — Container creation, image building, and exec sessions are all async. Image pulls and builds stream progress via `futures::Stream`, which we forward to the frontend as real-time status updates.
- **Type-safe** — Docker API responses are deserialized into Rust structs. Container configs, mount options, and exec parameters are all checked at compile time.
- **Exec with PTY** — bollard supports `docker exec` with `tty: true` and `attach_stdin/stdout/stderr`, giving us a full interactive pseudoterminal inside the container. This is the core mechanism that makes the terminal work.

### keyring (Secure Credential Storage)

**Chosen over:** Storing API keys in a config file, using environment variables, Tauri plugin-store

- **OS-native security** — `keyring` uses macOS Keychain, Windows Credential Manager, and Linux Secret Service (GNOME Keyring / KWallet). API keys never touch the filesystem in plaintext.
- **Simple API** — `Entry::new("triple-c", "anthropic-api-key")?.set_password(key)?` is the entire storage operation. No encryption key management needed.
- **Cross-platform** — One crate handles all three OS credential stores with feature flags (`apple-native`, `windows-native`, `linux-native`).

### Ubuntu 24.04 (Container Base Image)

**Chosen over:** Alpine, Debian, Fedora, distroless

- **Claude Code compatibility** — Claude Code's installer (`curl -fsSL https://claude.ai/install.sh | bash`) targets glibc-based systems. Alpine's musl libc causes compatibility issues with Node.js native modules and some Claude Code dependencies.
- **Package availability** — Ubuntu 24.04 has up-to-date packages for all pre-installed tools (Python 3.12, Git 2.43, etc.) without requiring third-party repositories for most things.
- **Developer familiarity** — Claude Code will run `apt install` to add tools at runtime. Ubuntu/Debian's package manager is the most widely documented, so Claude's suggestions will work correctly.
- **LTS support** — Ubuntu 24.04 is supported until 2029, providing a stable base that won't require frequent image rebuilds.

---

## Architecture

### System Diagram

```
┌─────────────────────────────────────────────────────────┐
│                    Desktop Application                   │
│  ┌──────────────────────┐  ┌──────────────────────────┐ │
│  │    React Frontend     │  │      Rust Backend        │ │
│  │                       │  │                          │ │
│  │  Zustand Store        │  │  Tauri Command Handlers  │ │
│  │  xterm.js Terminal(s) │  │  ExecSessionManager      │ │
│  │  Project Management   │◄─┤  ProjectsStore           │ │
│  │  Settings UI          │  │  bollard Docker Client   │ │
│  │                       │  │  keyring Credential Mgr  │ │
│  └───────────┬───────────┘  └────────────┬─────────────┘ │
│              │  Tauri IPC (invoke/emit)   │               │
│              └───────────┬───────────────┘               │
└──────────────────────────┼───────────────────────────────┘
                           │ Docker Socket
                           ▼
┌──────────────────────────────────────────────────────────┐
│                 Docker Container (per project)            │
│                                                          │
│  /workspace ←── bind mount ──► Host project directory    │
│  /home/claude/.claude ←── named volume (persists config) │
│  /tmp/.host-ssh ←── read-only bind mount (SSH keys)      │
│  /var/run/docker.sock ←── optional (sibling containers)  │
│                                                          │
│  Pre-installed: Claude Code, Node.js, Python, Rust,      │
│  Docker CLI, git, gh, ripgrep, uv, ruff, pnpm, AWS CLI  │
│                                                          │
│  User: claude (UID/GID remapped to match host)           │
│  Entrypoint: UID/GID remap → SSH setup → git config →   │
│              docker socket perms → sleep infinity        │
└──────────────────────────────────────────────────────────┘
```

### Communication Flow

The application uses two IPC mechanisms between the React frontend and Rust backend:

**Request/Response** (`invoke()`): Used for discrete operations — starting containers, saving settings, listing projects. The frontend calls `invoke("command_name", { args })` and awaits a typed result.

**Event Streaming** (`emit()`/`listen()`): Used for continuous data — terminal I/O. When a terminal session is opened, the Rust backend spawns two tokio tasks:
1. **Output reader** — Reads from the Docker exec stdout stream and emits `terminal-output-{sessionId}` events to the frontend.
2. **Input writer** — Listens on an `mpsc::unbounded_channel` for data sent from the frontend via `invoke("terminal_input")` and writes it to the Docker exec stdin.

```
User keystroke → xterm.js onData() → invoke("terminal_input") → mpsc channel → exec stdin
exec stdout → tokio task → emit("terminal-output-{id}") → listen() → xterm.js write()
```

Terminal resize follows the same pattern: `ResizeObserver` detects container size changes, `FitAddon.fit()` recalculates cols/rows, and `invoke("terminal_resize")` calls `bollard::Docker::resize_exec()`.

### Container Lifecycle

Containers follow a **stop/start** model, not create/destroy:

1. **First start**: A new container is created with bind mounts, environment variables, and labels. The entrypoint remaps UID/GID, configures SSH and git, then runs `sleep infinity` to keep the container alive.
2. **Terminal open**: `docker exec` launches `claude --dangerously-skip-permissions` with a PTY in the running container.
3. **Stop**: `docker stop` halts the container but preserves its filesystem. Any packages Claude installed via `apt`, `pip`, `cargo`, etc. survive.
4. **Restart**: `docker start` resumes the existing container. All installed tools and configuration persist.
5. **Reset**: The container is removed and recreated from the image. This is a clean slate — the nuclear option when the container state is corrupted.

The `.claude` configuration directory uses a **named Docker volume** (`triple-c-claude-config-{projectId}`) so OAuth tokens from `claude login` persist even across container resets.

### Authentication Modes

Each project independently chooses one of two authentication methods:

| Mode | How It Works | When to Use |
|------|-------------|-------------|
| **Anthropic (OAuth)** | User runs `claude login` or `/login` inside the terminal. OAuth URL opens in host browser via URL detection. Token persists in the `.claude` config volume. | Default — personal and team use |
| **AWS Bedrock** | Per-project AWS credentials (static keys, profile, or bearer token) injected as env vars. `~/.aws` config optionally bind-mounted read-only. | Enterprise environments using Bedrock |

### UID/GID Remapping

A common Docker pain point: files created inside the container have the container user's UID (1000 by default), which may not match the host user. This causes permission errors on bind-mounted project directories.

The entrypoint solves this by:
1. Reading `HOST_UID` and `HOST_GID` environment variables (set by the Rust backend using `id -u`/`id -g`).
2. Running `usermod`/`groupmod` to change the `claude` user's UID/GID to match.
3. Relocating any existing system user/group that conflicts with the target UID/GID.
4. Fixing ownership of `/home/claude` after the change.

This runs as root in the entrypoint, then the final `exec su -s /bin/bash claude -c "exec sleep infinity"` drops to the remapped user.

### SSH Key Handling

Host SSH keys are mounted **read-only** at `/tmp/.host-ssh` (a staging directory), not directly at `/home/claude/.ssh`. The entrypoint copies them to the correct location and fixes permissions:

- Private keys: `chmod 600`
- Public keys: `chmod 644`
- `.ssh` directory: `chmod 700`
- `known_hosts` is populated with GitHub, GitLab, and Bitbucket host keys, deduplicated with `sort -u`

This avoids the common Docker problem where bind-mount permissions can't be changed (the mount reflects the host filesystem's permissions, and `chmod` on a read-only mount fails).

### Data Persistence

| Data | Storage | Location |
|------|---------|----------|
| Project configurations | JSON file (atomic writes) | `~/.local/share/triple-c/projects.json` |
| API keys | OS keychain | macOS Keychain / Windows Credential Manager / Linux Secret Service |
| App settings | Tauri plugin-store | App data directory |
| Claude config/tokens | Named Docker volume | `triple-c-claude-config-{projectId}` |
| Container filesystem | Docker container layer | Preserved across stop/start, cleared on reset |

The projects store uses **atomic writes** (write to `.json.tmp`, then `rename()`) to prevent data corruption if the app crashes mid-write. Corrupted files are backed up to `.json.bak` before being replaced.

### URL Detection for OAuth

Claude Code's `login` command prints an OAuth URL that can exceed 200 characters. Terminal emulators hard-wrap long lines, splitting the URL across multiple lines with `\r\n` characters. The xterm.js WebLinksAddon only joins soft-wrapped lines (detected via the `isWrapped` flag on buffer lines), so the URL match is truncated.

The `TerminalView` component works around this with a **URL accumulator**:
1. All terminal output is buffered (capped at 8 KB).
2. After 150ms of silence (debounced), the buffer is stripped of ANSI escape codes and hard newlines.
3. If the reassembled text contains a URL longer than 80 characters, it's written back to the terminal as a single clickable line.
4. The WebLinksAddon detects the clean URL and `tauri-plugin-opener` opens it in the host browser when clicked.

---

## Project Structure

```
triple-c/
├── README.md                      # Architecture overview
├── TECHNICAL.md                   # This document
├── HOW-TO-USE.md                  # User guide
├── BUILDING.md                    # Build instructions
├── CLAUDE.md                      # Claude Code instructions
│
├── container/
│   ├── Dockerfile                 # Ubuntu 24.04 + all dev tools + Claude Code
│   ├── entrypoint.sh              # UID/GID remap, SSH setup, git config, MCP injection
│   ├── osc52-clipboard            # Clipboard shim (xclip/xsel/pbcopy via OSC 52)
│   ├── audio-shim                 # Audio capture shim (rec/arecord via FIFO)
│   ├── triple-c-scheduler         # Bash-based cron task system
│   └── triple-c-task-runner       # Task execution runner for scheduler
│
├── .gitea/
│   └── workflows/
│       ├── build-app.yml          # Build Tauri app (Linux/macOS/Windows)
│       ├── build.yml              # Build container image (multi-arch)
│       ├── sync-release.yml       # Mirror releases to GitHub
│       └── backfill-releases.yml  # Bulk copy releases to GitHub
│
└── app/                           # Tauri v2 desktop application
    ├── package.json               # React, xterm.js, zustand, tailwindcss
    ├── vite.config.ts             # Vite bundler config
    ├── index.html                 # HTML entry point
    │
    ├── src/                       # React frontend
    │   ├── main.tsx               # React DOM root
    │   ├── App.tsx                # Top-level layout
    │   ├── index.css              # CSS variables, dark theme, scrollbars
    │   ├── store/
    │   │   └── appState.ts        # Zustand store (projects, sessions, MCP, UI)
    │   ├── hooks/
    │   │   ├── useDocker.ts       # Docker status, image build/pull
    │   │   ├── useFileManager.ts  # File manager operations
    │   │   ├── useMcpServers.ts   # MCP server CRUD
    │   │   ├── useProjects.ts     # Project CRUD operations
    │   │   ├── useSettings.ts     # App settings
    │   │   ├── useTerminal.ts     # Terminal I/O, resize, session events
    │   │   ├── useUpdates.ts      # App update checking
    │   │   └── useVoice.ts        # Voice mode audio capture
    │   ├── lib/
    │   │   ├── types.ts           # TypeScript interfaces matching Rust models
    │   │   ├── tauri-commands.ts  # Typed invoke() wrappers
    │   │   └── constants.ts       # App-wide constants
    │   └── components/
    │       ├── layout/            # Sidebar, TopBar, StatusBar
    │       ├── mcp/               # McpPanel, McpServerCard
    │       ├── projects/          # ProjectCard, ProjectList, AddProjectDialog,
    │       │                      # FileManagerModal, ContainerProgressModal, modals
    │       ├── settings/          # SettingsPanel, DockerSettings, AwsSettings,
    │       │                      # UpdateDialog
    │       └── terminal/          # TerminalView (xterm.js), TerminalTabs, UrlToast
    │
    └── src-tauri/                 # Rust backend
        ├── Cargo.toml             # Rust dependencies
        ├── tauri.conf.json        # Tauri app configuration
        ├── capabilities/
        │   └── default.json       # Tauri v2 permission grants
        └── src/
            ├── lib.rs             # App builder, plugin + command registration
            ├── main.rs            # Entry point
            ├── logging.rs         # Log configuration
            ├── commands/          # Tauri command handlers
            │   ├── docker_commands.rs   # Docker status, image ops
            │   ├── file_commands.rs     # File manager (list/download/upload)
            │   ├── mcp_commands.rs      # MCP server CRUD
            │   ├── project_commands.rs  # Start/stop/rebuild containers
            │   ├── settings_commands.rs # Settings CRUD
            │   ├── terminal_commands.rs # Terminal I/O, resize
            │   └── update_commands.rs   # App update checking
            ├── docker/            # Docker API layer
            │   ├── client.rs      # bollard singleton connection
            │   ├── container.rs   # Create, start, stop, remove, fingerprinting
            │   ├── exec.rs        # PTY exec sessions with bidirectional streaming
            │   ├── image.rs       # Build from Dockerfile, pull from registry
            │   └── network.rs     # Per-project bridge networks for MCP
            ├── models/            # Data structures
            │   ├── project.rs     # Project, AuthMode, BedrockConfig
            │   ├── mcp_server.rs  # MCP server configuration
            │   ├── app_settings.rs # Global settings (image source, AWS, etc.)
            │   ├── container_config.rs # Image name resolution
            │   └── update_info.rs # Update metadata
            └── storage/           # Persistence
                ├── projects_store.rs  # JSON file with atomic writes
                ├── mcp_store.rs       # MCP server persistence
                ├── settings_store.rs  # App settings (Tauri plugin-store)
                └── secure.rs          # OS keychain via keyring
```

---

## Key Dependencies

### Rust (Backend)

| Crate | Version | Purpose |
|-------|---------|---------|
| `tauri` | 2.x | Application framework, IPC, window management |
| `tauri-plugin-store` | 2.x | JSON settings persistence |
| `tauri-plugin-dialog` | 2.x | Native file/directory picker dialogs |
| `tauri-plugin-opener` | 2.x | Open URLs in host browser |
| `bollard` | 0.18 | Docker Engine API client |
| `keyring` | 3.x | OS keychain (macOS/Windows/Linux) |
| `tokio` | 1.x | Async runtime (exec streaming, channels) |
| `futures-util` | 0.3 | Stream processing for Docker API responses |
| `uuid` | 1.x | Project and session ID generation (v4) |
| `chrono` | 0.4 | Timestamps for project metadata |
| `tar` | 0.4 | In-memory tar archives for Docker build context |
| `dirs` | 6.x | Cross-platform app data directory paths |
| `serde` / `serde_json` | 1.x | Serialization for IPC and persistence |

### JavaScript (Frontend)

| Package | Version | Purpose |
|---------|---------|---------|
| `react` / `react-dom` | 19.x | UI framework |
| `@tauri-apps/api` | 2.x | Tauri IPC bridge (`invoke`, `emit`, `listen`) |
| `@tauri-apps/plugin-dialog` | 2.x | Frontend bindings for directory picker |
| `@tauri-apps/plugin-opener` | 2.x | Frontend bindings for URL opener |
| `@tauri-apps/plugin-store` | 2.x | Frontend bindings for settings store |
| `@xterm/xterm` | 5.x | Terminal emulator |
| `@xterm/addon-fit` | 0.10.x | Auto-resize terminal to container |
| `@xterm/addon-webgl` | 0.18.x | Hardware-accelerated terminal rendering |
| `@xterm/addon-web-links` | 0.12.x | Clickable URLs in terminal output |
| `zustand` | 5.x | Lightweight state management |
| `tailwindcss` | 4.x | Utility-first CSS framework |
| `vite` | 6.x | Frontend build tool and dev server |

### Container Image

| Tool | Purpose |
|------|---------|
| Claude Code | AI coding assistant (the core tool being sandboxed) |
| Node.js 22 LTS + pnpm | JavaScript/TypeScript development |
| Python 3.12 + uv + ruff | Python development with fast package management |
| Rust (stable) + cargo | Rust development |
| Docker CLI | Sibling container spawning (when enabled per-project) |
| git + gh (GitHub CLI) | Version control and GitHub integration |
| AWS CLI v2 | AWS Bedrock authentication and management |
| ripgrep | Fast code search (used by Claude Code internally) |
| build-essential | C/C++ compilation (required by many native dependencies) |
| openssh-client | Git SSH authentication |

---

## Cross-Platform Considerations

| Concern | Linux | macOS | Windows |
|---------|-------|-------|---------|
| Docker socket | `/var/run/docker.sock` | `/var/run/docker.sock` | `//./pipe/docker_engine` |
| Credential storage | Secret Service (GNOME Keyring) | Keychain | Credential Manager |
| Webview engine | WebKitGTK | WebKit | WebView2 |
| UID/GID remapping | Entrypoint `usermod`/`groupmod` | Entrypoint `usermod`/`groupmod` | Skipped (Docker Desktop VM handles it) |
| App data directory | `~/.local/share/triple-c/` | `~/Library/Application Support/triple-c/` | `%APPDATA%\triple-c\` |
