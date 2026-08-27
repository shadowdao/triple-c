# Triple-C Technical Architecture

## Overview

Triple-C (Claude-Code-Container) sandboxes Claude Code inside Docker containers so that even in its most permissive mode — `--dangerously-skip-permissions` — Claude only has access to files and projects you explicitly provide. The project consists of two components: a **Docker container image** pre-loaded with development tools, and a **cross-platform desktop application** for managing project containers, terminal sessions, and authentication.

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

#### Terminal Layout & StatusBar Controls

Implementation gotchas for the terminal view and its global controls (merged in PR #7, `terminal-layout-statusbar`):

- **xterm padding lives on a wrapper, never the host.** FitAddon measures the same element that `term.open()` mounts into, so any padding on that host element makes the grid overhang and clip its rightmost column / bottom row. Padding must live on a **wrapper `div`**; the xterm host fills it with no padding of its own. Do not reintroduce padding on the host element in `TerminalView.tsx`.
- **STT mic and "Jump to Current" live in the global `StatusBar`, not per-terminal overlays.** There is a single `useSTT` instance in `App.tsx` bound to the active session. `Ctrl+Shift+M` routes through the Zustand store (`sttToggle`).
- **Recording is pinned to where it started.** The STT transcript targets `recordingSessionIdRef` (the session recording began in), **not** the live active session — switching tabs mid-recording must not misroute the transcript.
- **"Jump to Current" state is written only by the active terminal.** The active `TerminalView` surfaces `terminalAtBottom` and `scrollActiveToBottom` through the store; only the active terminal writes them, and they are cleared on its unmount.
- **Set store function values via object-merge, not the updater form** — `set({ fn: value })`, not `set(state => ...)` — when publishing action callbacks (like `scrollActiveToBottom`) into the Zustand store.

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
│  └───────────┬───────────┘  │  Web Terminal Server     │ │
│              │              └────────────┬─────────────┘ │
│              │  Tauri IPC (invoke/emit)   │               │
│              └───────────┬───────────────┘               │
│                                    ▲                      │
│                        axum HTTP+WS│(port 7681)           │
│                                    │                      │
└──────────────────────────┼───────────────────────────────┘
                           │ Docker Socket
                           ▼
┌──────────────────────────────────────────────────────────┐
│                 Docker Container (per project)            │
│                                                          │
│  /workspace/<name> ←─ bind mount ─► Host project folder  │
│  /home/claude ←── named volume (home dir)                │
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

**WebSocket Streaming** (Web Terminal): Used for remote terminal access from browsers on the local network. An axum HTTP+WebSocket server runs inside the Tauri process, sharing the same `ExecSessionManager` via `Arc`-wrapped stores. The WebSocket uses a JSON protocol with base64-encoded terminal data. Each browser connection can open multiple terminal sessions; all sessions are cleaned up when the WebSocket disconnects.

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

1. **First start**: A new container is created with bind mounts, named volumes, environment variables, and labels. The entrypoint remaps UID/GID, configures SSH and git, rebuilds the scheduler crontab, then runs `sleep infinity` to keep the container alive.
2. **Terminal open**: `docker exec` launches `claude` with a PTY in the running container, with the permission-mode flags from `PermissionMode::cli_args()` (or `bash -l` for a shell session).
3. **Stop**: `docker stop` halts the container but preserves its filesystem. Any packages Claude installed via `apt`, `pip`, `cargo`, etc. survive.
4. **Restart**: `docker start` resumes the existing container — unless `container_needs_recreation()` finds a `triple-c.*` label that no longer matches the project's settings, in which case the container is committed to a snapshot image (`triple-c-snapshot-{projectId}:latest`), removed, and recreated from that snapshot. Installed tools survive; the named volumes are untouched.
5. **Reset**: `rebuild_project_container` closes live exec sessions, removes the container, removes the snapshot image, calls `remove_project_volumes` to delete **both** named volumes, then starts fresh from the clean base image.

Two named volumes exist per project and they are the only ones it owns:

| Volume | Mount point | Purpose |
|---|---|---|
| `triple-c-home-{projectId}` | `/home/claude` | Home directory — `~/.claude.json`, `~/.local`, `~/.ssh`, `~/.aws` |
| `triple-c-claude-config-{projectId}` | `/home/claude/.claude` | Claude Code config: OAuth credential, settings, skills/agents/commands, session transcripts, scheduler state. Nested inside the home volume; Docker gives the more specific mount precedence. |

`remove_project_volumes` names those two volumes explicitly (no prefix sweep) and is called from
exactly two places: `remove_project` and `rebuild_project_container`. Ordinary container removal
passes `v: false`, so stop/start and recreation never touch the volumes — **only Reset and project
removal delete them.** A Reset therefore destroys the `claude login` credential, installed skills,
session transcripts and scheduled tasks; it does not touch host bind mounts, the project record, or
host keychain secrets.

### Permission Modes

`PermissionMode` (`models/project.rs`) is a four-state enum replacing the earlier `full_permissions`
boolean. It reaches Claude Code by two different routes:

| Mode | `cli_args()` — interactive terminals | `as_env_value()` — scheduler |
|---|---|---|
| `Plan` | `--permission-mode plan` | `plan` |
| `Default` | *(no flag)* | `default` |
| `AcceptEdits` | `--permission-mode acceptEdits` | `acceptEdits` |
| `Bypass` | `--dangerously-skip-permissions` | `bypass` |

`Project.permission_mode` is `Option<PermissionMode>`, and `effective_permission_mode()` resolves
`None` from the legacy `full_permissions` flag, so records written before the change keep behaving
the same way.

**Interactive path.** `build_terminal_cmd()` evaluates `cli_args()` when a session is created, so
the flags are fixed for the life of that `claude` process. Changing the mode affects terminals
opened afterwards, not running ones. The same applies to `resume_session_command`, which builds
`claude <flags> --resume <id>` server-side.

**Scheduler path.** Cron jobs run with a minimal environment, so the mode travels as
`TRIPLE_C_PERMISSION_MODE` in the container's env; the entrypoint snapshots the allowlisted
variables into `~/.claude/scheduler/.env`, and `triple-c-task-runner` sources that file and maps the
value back to flags for its `claude -p` run. Container env can only change at create time, so
`container_needs_recreation()` compares a `triple-c.permission-mode` label and forces a recreation
on the next start. A mode change therefore reaches new terminals immediately but the scheduler only
after a stop/start. `TRIPLE_C_PERMISSION_MODE` is a reserved env key so it cannot be hand-set.

### Authentication Modes

Each project independently chooses one backend:

| Backend | How It Works | When to Use |
|------|-------------|-------------|
| **Anthropic** | Either the shared `CLAUDE_CODE_OAUTH_TOKEN` injected from the OS keychain, or a per-container `claude login` whose credential persists in the `.claude` config volume. The OAuth URL opens in the host browser via URL detection. | Default — personal and team use |
| **AWS Bedrock** | Per-project AWS credentials (static keys, named profile, or bearer token) injected as env vars. `~/.aws` config optionally bind-mounted read-only; SSO sessions are validated before launching Claude for profile auth. | Enterprise environments using Bedrock |
| **Ollama** | `ANTHROPIC_BASE_URL` points at an Ollama server; `ANTHROPIC_AUTH_TOKEN` is set to the placeholder `ollama`. Ollama implements `POST /v1/messages` natively. | Local models (best-effort) |
| **llama.cpp** | `ANTHROPIC_BASE_URL` points at a `llama-server` (default port 8080); `ANTHROPIC_AUTH_TOKEN` is set to the placeholder `llama.cpp`, which `llama-server` ignores unless started with `--api-key`. `llama-server` implements `POST /v1/messages` and `/v1/messages/count_tokens` natively. | Local models (best-effort) |
| **OpenAI Compatible** | `ANTHROPIC_BASE_URL` plus `ANTHROPIC_AUTH_TOKEN` point at a gateway. **Despite the name, the endpoint must implement the Anthropic Messages API** — Claude Code only ever sends `POST /v1/messages?beta=true`, never `/v1/chat/completions`. LiteLLM works; a bare OpenAI-only server does not. | Anthropic-shaped gateways (best-effort) |

#### Model aliases on custom endpoints

`Backend::uses_custom_endpoint()` (Ollama, llama.cpp, OpenAI Compatible) gates the emission of
`ANTHROPIC_DEFAULT_{OPUS,SONNET,HAIKU,FABLE}_MODEL`, computed by
`docker::container::compute_model_aliases`. All four default to the backend's resolved model id;
each backend carries an optional `haiku_model_id` override, because the Haiku alias is what Claude
Code uses for background work. Anthropic and Bedrock emit none of them and keep Claude Code's
defaults; the four names are in `MANAGED_AUTH_KEYS`, so switching away from a custom endpoint
blanks the values baked into the snapshot image. The resolved alias set is folded into each
backend's `triple-c.*-fingerprint` label, since `container_needs_recreation` is label-based and
never diffs env. `ANTHROPIC_SMALL_FAST_MODEL` is deprecated and unused.

### Shared Claude Authentication Token

`commands/auth_token_commands.rs` runs `claude setup-token` on a PTY inside a running container.
Contrary to the loopback pattern most CLI logins use, `setup-token` redirects to an Anthropic-hosted
page and then blocks on a stdin paste prompt, so the flow needs a way to feed the pasted code back
in — hence `submit_claude_token_code`. The flow is single-flight (the token is global, so two
concurrent logins would race to overwrite each other's keychain entry) and times out after 15
minutes.

- **Storage** — the OS keychain, under a dedicated service name; the token is never returned to the
  frontend, never written to a log, and no command accepts or returns it.
- **The sign-in URL comes from the OSC 8 parameter, not the screen.** The CLI emits the URL as a
  hyperlink and slices the *visible* text of it to the terminal width — measured against 2.1.226, a
  346-character URL arrives at 80 columns as five separate hyperlink emissions, each carrying the
  whole URL in its parameter and 80 characters of it on screen. Scraping the visible text yields a
  URL that parses, points at `claude.com`, and cannot authorise anything, so the ANSI stripper
  surfaces the hyperlink target and `claude-token-link` carries it to the UI. The frontend applies
  the `ANTHROPIC_SIGN_IN_HOSTS` allowlist to it before display and again before `openUrl` — an OSC 8
  parameter is container output that is never rendered, which makes it the *easier* place to hide a
  hostile host, not a trusted one. `stty cols 400` (up from 200, which the URL still overflowed)
  removes wrapping as a variable elsewhere, but it is not the fix: that line fails silently.
- **A rejected code is recoverable, not a hang.** On a bad paste the CLI prints
  `OAuth error: Invalid code…` / `Press Enter to retry.` and blocks on stdin rather than exiting.
  The streamed output is scanned for that, `claude-token-code-rejected` reopens the input with an
  explanation, and the Enter is sent so the next code has a prompt to land in — bounded by
  `MAX_CODE_ATTEMPTS`, after which the flow reports a failure. Without this the exec sat until the
  15-minute timeout with the UI still saying "Finishing sign-in".
- **Redaction** — streamed output is stripped of ANSI sequences and passed through a stateful
  redactor that masks anything matching `sk-ant-` with a plausible body, withholding any tail that
  could still grow into a secret across a chunk boundary. A credential split across a hard line
  wrap is reassembled by both the parser and the redactor from the same `scan_credential_body`, so
  the two cannot disagree about where a credential ends — previously a wrapped token was rejected
  as too short *and* its second line, which carries no `sk-ant-` marker, was printed to the UI in
  clear. A run is only joined across a break that sits at a plausible terminal margin and is not
  already long enough to be a whole credential; otherwise a repainting TUI would weld one frame's
  token onto the next frame's first word.
- **Injection** — `CLAUDE_CODE_OAUTH_TOKEN` is set only when the backend is Anthropic, the project
  has not opted out (`use_shared_auth_token`, default `true`), and a non-blank token is stored. When
  those conditions do not hold, the variable is explicitly set to empty rather than omitted, so a
  value baked into a snapshot image by `docker commit` is actively cleared.
- **Rotation** — a random UUID minted on each store is mirrored into the
  `triple-c.claude-token-version` label. It is deliberately *not* a hash of the token: labels are
  readable by anything that can run `docker inspect`, and a hash would be an offline verification
  oracle. A label mismatch forces container recreation on the next start, which is when a container
  picks up or loses the token.

### Auth Bridge

CLIs that log in through a browser (`claude login`, `aws sso login`, `fly login`) start an ephemeral
HTTP listener on an unpredictable loopback port and hand the provider a `http://localhost:<port>/…`
redirect. Run inside a container, that listener is unreachable from the host browser and nothing can
be pre-published at container-creation time. `auth_bridge/` bridges it at runtime:

- **Discovery** (`proc_net.rs`) — a `docker exec` reads `/proc/net/tcp` and `/proc/net/tcp6` every
  two seconds. The image ships no `ss`, `netstat` or `lsof`. Only rows in state `0A` (`TCP_LISTEN`)
  bound to loopback are kept; wildcard binds are ignored on purpose, since publishing those is the
  port-mappings feature's job.
- **Family handling** — a `::1`-only listener genuinely cannot be reached over `127.0.0.1`, and Node
  resolves `localhost` to IPv6 first on Linux, so `claude login` frequently binds `::1` alone. The
  socat target follows the family actually observed; IPv4-mapped rows in `/proc/net/tcp6` are
  treated as IPv4.
- **Host bind** (`tunnel.rs`) — the same port number is bound on the host: `127.0.0.1` is required,
  `[::1]` is best-effort. **The host side binds loopback only, never a wildcard address** —
  everything behind it is an unauthenticated in-container service that bound loopback precisely
  because it expected to be unreachable.
- **Transport** — each accepted connection is proxied by an attached exec running
  `socat - TCP:127.0.0.1:<port>`, because container IPs are not routable from the host under Docker
  Desktop. It goes through the same `create_attached_exec()` helper as terminal sessions, with
  `tty: false` so socat's stderr is demultiplexed away from the proxied byte stream.
- **Policy** — ports appearing in the project's port mappings are skipped, and a host bind failure
  is recorded as a conflict and retried later rather than fought over.
- **Lifecycle** — opt-in per project (`auth_bridge_enabled`, default `false`). It is purely
  host-side, so it deliberately has no container-recreation label. The poller stops itself when the
  project is gone, the flag is cleared, or the container is no longer running, and `stop()` awaits
  it so host ports are provably released.

### Container Introspection

`list_container_capabilities` (`commands/inspect_commands.rs`) executes a read-only shell script in
a running container and returns counts and item lists for skills, agents, commands, hooks, plugins
and MCP servers, across user scope (`/home/claude/.claude`) and project scope
(`/workspace/*/.claude`, `/workspace/*/.mcp.json`). Everything is computed in-container with
`find`/`awk`/`jq`; only the JSON summary crosses the wire, and a stopped container yields zeros
rather than an error.

The script writes nothing. Claude Code owns this configuration and has its own tooling for it
(`/agents`, `/hooks`, `/plugins`, `/mcp`); Triple-C surfaces counts and opens a terminal rather than
rebuilding those editors as forms. `list_claude_sessions` and the scheduler commands
(`list_scheduled_tasks`, `get_scheduled_task_log`, `set_scheduled_task_enabled`,
`run_scheduled_task_now`, `remove_scheduled_task`, `clear_scheduler_notifications`) live in the same
module; the mutating ones shell out to `triple-c-scheduler` rather than editing its state files.

### Main-Area Tab Model

The frontend keeps a single ordered `tabOrder` array in the Zustand store holding two tab kinds,
`home:<projectId>` and `term:<sessionId>`, rendered by `components/layout/MainTabs.tsx`.
`activeSessionId` is *derived* from `activeTabKey`, so exactly one thing is current and a Project
Home tab and a terminal cannot both claim focus. Project configuration is a main-area view
(`components/projects/home/`), not a modal; the sidebar row is select-only.

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
| API keys and per-project secrets | OS keychain | macOS Keychain / Windows Credential Manager / Linux Secret Service |
| Shared Claude token + rotation id | OS keychain | Separate service entries; never on disk, never in a label |
| App settings | Tauri plugin-store | App data directory |
| Claude config, sessions, scheduler state | Named Docker volume | `triple-c-claude-config-{projectId}` |
| Container home directory | Named Docker volume | `triple-c-home-{projectId}` |
| Container filesystem | Docker container layer, preserved into `triple-c-snapshot-{projectId}:latest` on recreation | Survives stop/start and recreation; destroyed by Reset |

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
├── HOW-TO-USE.md                  # User guide (also served by the in-app Help dialog)
├── BUILDING.md                    # Build instructions
├── CLAUDE.md                      # Claude Code instructions
├── DESIGN-REVIEW.md               # UI/UX review notes
├── ROADMAP.md                     # Planned work
│
├── container/                     # Sandbox image
│   ├── Dockerfile                 # Ubuntu 24.04 + all dev tools + Claude Code
│   ├── entrypoint.sh              # UID/GID remap, SSH setup, git config, settings injection,
│   │                              # scheduler env snapshot + crontab rebuild
│   ├── osc52-clipboard            # Clipboard shim (xclip/xsel/pbcopy via OSC 52)
│   ├── audio-shim                 # Audio capture shim (rec/arecord via FIFO)
│   ├── triple-c-scheduler         # Bash-based cron task system
│   ├── triple-c-task-runner       # Cron entry point; permission mode → flags → `claude -p`
│   ├── triple-c-sso-refresh       # AWS SSO session refresh helper
│   └── mission-control/           # Bundled Flight Control methodology (skills, docs, templates)
│
├── stt-container/                 # Speech-to-text image
│   ├── Dockerfile                 # Faster Whisper (Python 3.11 + FastAPI)
│   └── server.py                  # POST /transcribe endpoint
│
├── .gitea/
│   └── workflows/
│       ├── build-app.yml            # Build Tauri app (Linux/macOS/Windows); mirrors releases to GitHub inline
│       ├── build-app-preview.yml    # Preview builds
│       ├── build.yml                # Build container image (multi-arch)
│       ├── build-stt.yml            # Build the STT image
│       ├── backfill-releases.yml    # Bulk copy releases to GitHub
│       ├── cleanup-releases.yml     # Prune old releases
│       └── publish-arch-package.yml # Build triple-c-bin, attach it to the GitHub release (packaging/arch/)
│
├── packaging/
│   └── arch/                      # triple-c-bin Arch package — see packaging/arch/README.md
│       ├── PKGBUILD
│       └── README.md
│
└── app/                           # Tauri v2 desktop application
    ├── package.json               # React, xterm.js, zustand, tailwindcss
    ├── vite.config.ts             # Vite bundler config
    ├── vitest.config.ts           # Vitest (jsdom) config
    ├── index.html                 # HTML entry point
    │
    ├── src/                       # React frontend
    │   ├── main.tsx               # React DOM root
    │   ├── App.tsx                # Top-level layout + welcome screen
    │   ├── index.css              # CSS variables, dark theme, focus ring, scrollbars
    │   ├── store/
    │   │   └── appState.ts        # Zustand store (projects, sessions, tab strip, toasts)
    │   ├── hooks/
    │   │   ├── useClaudeAuth.ts   # Shared token status + acquisition
    │   │   ├── useContainerProgress.ts # container-progress events → inline progress
    │   │   ├── useDocker.ts       # Docker status, image build/pull
    │   │   ├── useFileManager.ts  # File browser operations + host transfers
    │   │   ├── useInstallHelper.ts # Guided Docker installation
    │   │   ├── useKeyboardShortcuts.ts # Ctrl+T / Ctrl+Shift+W / Ctrl+Tab / Ctrl+1..9
    │   │   ├── useProjectActions.ts # Start/stop/reset/backup, open terminals
    │   │   ├── useProjects.ts     # Project CRUD operations
    │   │   ├── useSaveState.ts    # Saved / Saving / Failed indicator state
    │   │   ├── useSettings.ts     # App settings
    │   │   ├── useSTT.ts          # Speech-to-text recording and container control
    │   │   ├── useTerminal.ts     # Terminal I/O, resize, session events
    │   │   ├── useUpdates.ts      # App update checking
    │   │   └── useVoice.ts        # Voice mode audio capture
    │   ├── lib/
    │   │   ├── types.ts           # TypeScript interfaces matching Rust models
    │   │   ├── tauri-commands.ts  # Typed invoke() wrappers
    │   │   ├── urlDetector.ts     # Long-URL reassembly for OAuth flows
    │   │   ├── wav.ts             # WAV encoding for STT
    │   │   └── constants.ts       # App-wide constants
    │   └── components/
    │       ├── DockerInstallDialog.tsx # First-run Docker setup
    │       ├── layout/            # TopBar, MainTabs (the unified tab strip),
    │       │                      # Sidebar, StatusBar, HelpDialog
    │       ├── projects/
    │       │   ├── home/                 # Project Home — the main-area project view
    │       │   │   ├── ProjectHome.tsx   # Header, actions, overflow menu, tab strip
    │       │   │   ├── OverviewTab.tsx   # Permission mode, summary, recent activity
    │       │   │   ├── SessionsTab.tsx   # Past Claude sessions + Resume
    │       │   │   ├── AutomationTab.tsx # Scheduler tasks + notifications
    │       │   │   ├── ConfigTab.tsx     # Config section host
    │       │   │   ├── FilesTab.tsx      # In-container file browser, upload / save to host
    │       │   │   ├── CapabilityTiles.tsx # Read-only capability counts
    │       │   │   ├── format.ts         # Age / size / uptime formatting
    │       │   │   └── config/           # WorkspaceSection, ModelSection,
    │       │   │                         # AccessSection, RuntimeSection
    │       │   ├── ProjectRow.tsx        # Select-only sidebar row
    │       │   ├── ProjectList.tsx       # Sidebar project list
    │       │   ├── AddProjectDialog.tsx  # New-project dialog
    │       │   ├── PermissionModeControl.tsx # Plan/Default/Accept Edits/Bypass
    │       │   ├── ConfirmRemoveModal.tsx    # Project removal confirmation
    │       │   └── *Editor.tsx / *Modal.tsx  # EnvVars, PortMappings,
    │       │                                 # ClaudeInstructions, ClaudeCodeSettings —
    │       │                                 # editors reused by Project Home
    │       ├── settings/          # SettingsPanel, DockerSettings, AwsSettings,
    │       │                      # OllamaSettings, LlamaCppSettings,
    │       │                      # OpenAiCompatibleSettings,
    │       │                      # SharedAuthSettings, ClaudeAuthModal,
    │       │                      # WebTerminalSettings, SttSettings,
    │       │                      # MicrophoneSettings, UpdateDialog, ImageUpdateDialog
    │       ├── terminal/          # TerminalView (xterm.js), TerminalContextMenu,
    │       │                      # SttButton, UrlToast, trimSelection
    │       └── ui/                # Shared primitives: Modal, Button, Toggle, Field,
    │                              # SegmentedControl, StatusIndicator, SaveIndicator,
    │                              # OverflowMenu, ToastHost, Tooltip, AccordionSection
    │
    └── src-tauri/                 # Rust backend
        ├── Cargo.toml             # Rust dependencies
        ├── tauri.conf.json        # Tauri app configuration
        ├── build.rs               # Tauri build script
        ├── capabilities/
        │   └── default.json       # Tauri v2 plugin permission grants
        └── src/
            ├── lib.rs             # App builder, plugin + command registration
            ├── main.rs            # Entry point
            ├── logging.rs         # Log configuration
            ├── commands/          # Tauri command handlers
            │   ├── auth_bridge_commands.rs  # Enable/status for the loopback bridge
            │   ├── auth_token_commands.rs   # claude setup-token flow, redaction, keychain
            │   ├── aws_commands.rs          # AWS profile/region discovery
            │   ├── docker_commands.rs       # Docker status, image ops
            │   ├── file_commands.rs         # File browser + host transfers (Rust-opened dialogs)
            │   ├── help_commands.rs         # Serves HOW-TO-USE.md to the Help dialog
            │   ├── inspect_commands.rs      # Sessions, capabilities, scheduler tasks
            │   ├── install_helper_commands.rs # Guided Docker installation
            │   ├── project_commands.rs      # Start/stop/rebuild/backup containers
            │   ├── settings_commands.rs     # Settings CRUD
            │   ├── stt_commands.rs          # STT start/stop/transcribe
            │   ├── terminal_commands.rs     # Terminal I/O, resize
            │   ├── update_commands.rs       # App update checking
            │   └── web_terminal_commands.rs # Web terminal start/stop/status
            ├── auth_bridge/       # Host-side loopback callback bridge
            │   ├── mod.rs         # Per-project poller, status, lifecycle
            │   ├── proc_net.rs    # /proc/net/tcp{,6} parsing, loopback filtering
            │   └── tunnel.rs      # Host loopback bind + socat tunnel over the Docker API
            ├── web_terminal/      # Remote terminal access
            │   ├── mod.rs         # Module root
            │   ├── server.rs      # Axum HTTP+WS server lifecycle
            │   ├── ws_handler.rs  # WebSocket connection handler
            │   └── terminal.html  # Embedded xterm.js web UI
            ├── install_helper/    # Docker installation assistance
            │   ├── mod.rs         # Install orchestration
            │   └── platform.rs    # Per-OS install strategies
            ├── docker/            # Docker API layer
            │   ├── client.rs      # bollard singleton connection
            │   ├── container.rs   # Create/start/stop/remove, labels, recreation checks,
            │   │                  # remove_project_volumes, snapshot commit
            │   ├── exec.rs        # create_attached_exec() — the single attached-exec path
            │   ├── image.rs       # Build from Dockerfile, pull from registry
            │   ├── stt.rs         # Speech-to-text container lifecycle
            │   └── legacy_cleanup.rs # Migration shim for the removed MCP feature
            ├── models/            # Data structures
            │   ├── project.rs     # Project, Backend, PermissionMode, BedrockConfig, …
            │   ├── app_settings.rs # Global settings (image source, AWS, STT, web terminal)
            │   ├── container_config.rs # Image name resolution
            │   └── update_info.rs # Update metadata
            └── storage/           # Persistence
                ├── projects_store.rs  # JSON file with atomic writes
                ├── settings_store.rs  # App settings (Tauri plugin-store)
                └── secure.rs          # OS keychain via keyring (secrets, shared token)
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
| `log` / `fern` | 0.4 / 0.7 | Date-based file logging |
| `include_dir` | 0.7 | Embeds the container build context in the binary |
| `reqwest` | 0.12 | HTTPS (rustls) for update checks, help content, STT uploads |
| `iana-time-zone` | 0.1 | Host timezone detection for container `TZ` |
| `sha2` | 0.10 | Settings fingerprints |
| `axum` | 0.8 | HTTP+WebSocket server for web terminal |
| `tower-http` | 0.6 | CORS middleware for web terminal |
| `base64` | 0.22 | Terminal data encoding over WebSocket |
| `rand` | 0.9 | Access token generation |
| `local-ip-address` | 0.6 | LAN IP detection for web terminal URL |

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
| `vitest` | 4.x | Test runner (jsdom environment) |
| `@testing-library/react` | 16.x | Component tests |

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
