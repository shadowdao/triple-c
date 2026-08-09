# Triple-C (Claude-Code-Container)

Triple-C is a cross-platform desktop application that sandboxes Claude Code inside Docker containers. Each project chooses its own **permission mode** — from Plan (read-only) through to Bypass (`--dangerously-skip-permissions`), which gives Claude unrestricted access within the sandbox.

## Architecture

- **Frontend**: React 19 + TypeScript + Tailwind CSS v4 + Zustand state management
- **Backend**: Rust (Tauri v2 framework)
- **Terminal**: xterm.js with WebGL rendering
- **Docker API**: bollard (pure Rust Docker client)

### Layout Structure

```
┌─────────────────────────────────────────────────────┐
│  TopBar (MainTabs strip + Docker/Image status + ?)  │
├────────────┬────────────────────────────────────────┤
│  Sidebar   │  Main Content                          │
│  (25% w,   │   · Project Home views, or             │
│  responsive│   · terminal views (xterm.js)          │
│  min/max)  │                                        │
├────────────┴────────────────────────────────────────┤
│  StatusBar (project/terminal counts, STT, scroll)   │
└─────────────────────────────────────────────────────┘
```

The main area is driven by **one ordered tab strip** (`components/layout/MainTabs.tsx`) holding
two tab kinds: `home:<projectId>` (Project Home) and `term:<sessionId>` (a terminal). There is no
separate terminal tab bar. `activeSessionId` is derived from the active tab key, so exactly one
thing is current at a time.

### Keyboard Shortcuts

Implemented in `hooks/useKeyboardShortcuts.ts` (document-level, capture phase):

| Shortcut | Action |
|---|---|
| `Ctrl+T` | New Claude terminal for the current project (no-op unless it is running) |
| `Ctrl+Shift+W` | Close the active tab |
| `Ctrl+Tab` / `Ctrl+Shift+Tab` | Cycle tabs forward / backward |
| `Ctrl+1` … `Ctrl+9` | Jump to the nth tab |

`Ctrl+W` is deliberately **not** bound: it is readline's `kill-word`, used constantly in the
terminal this app is built around. Terminal-scoped keys (`Ctrl+Shift+C`, `Ctrl+Shift+Alt+C`,
`Ctrl+Shift+M`) are handled in `TerminalView.tsx`.

### Project Home

Clicking a project row in the sidebar opens **Project Home** in the main area — the per-project
view, with tabs **Overview · Sessions · Automation · Config · Files**. The sidebar row itself is
select-only (plus hover controls for start/stop and opening a terminal); it holds no configuration.
Per-project configuration lives in the Config tab rather than in modals.

| Tab | Contents |
|---|---|
| **Overview** | Permission mode control, sandbox/backend/Docker-access summary, capability tiles, recent sessions, scheduled tasks |
| **Sessions** | Past Claude Code conversations read from the config volume, with **Resume** |
| **Automation** | The container's `triple-c-scheduler` tasks — enable/disable, run now, read logs, remove, and completion notifications |
| **Config** | Workspace (name, folders), Model (backend), Access (SSH, git, env vars, port mappings), Runtime (permission mode, sandbox, Docker access, Mission Control, instructions, Claude Code settings) |
| **Files** | Browse, download and upload files inside the container |

Container start/stop progress is reported inline (on the sidebar row and in the Project Home
header) via the `container-progress` event, and failures surface as toasts. There is no blocking
progress modal.

### Permission Modes

`PermissionMode` in `models/project.rs` replaces the old `full_permissions` boolean. Four states,
mapped to CLI flags by `PermissionMode::cli_args()`:

| Mode | Serialized | CLI args passed to `claude` |
|---|---|---|
| **Plan** | `plan` | `--permission-mode plan` |
| **Default** | `default` | *(none)* |
| **Accept Edits** | `acceptEdits` | `--permission-mode acceptEdits` |
| **Bypass** | `bypass` | `--dangerously-skip-permissions` |

`Project.permission_mode` is `Option<PermissionMode>`; `effective_permission_mode()` falls back to
the legacy `full_permissions` flag (`true` → Bypass) for records written before the change. Changing
the mode affects terminals opened **from then on** — a running `claude` process keeps the argv it
was launched with.

Scheduled tasks honour it too. The mode is injected as `TRIPLE_C_PERMISSION_MODE` (via
`as_env_value()`) and written as the `triple-c.permission-mode` container label; the entrypoint
snapshots it into `~/.claude/scheduler/.env`, and `container/triple-c-task-runner` translates it
back into flags for its headless `claude -p` run. Because it travels as container env, a mode change
only reaches the scheduler after the container is recreated on its next start (the label mismatch
forces that).

### Container Introspection (Capability Tiles)

`list_container_capabilities` (`commands/inspect_commands.rs`) runs a read-only `find`/`jq` script
inside a running container and returns counts plus item lists for **skills, agents, commands, hooks,
plugins and MCP servers**, at user scope (`/home/claude/.claude`) and project scope
(`/workspace/*/.claude`, `/workspace/*/.mcp.json`). Overview renders these as tiles.

Triple-C does not create or edit any of them — Claude Code owns that configuration, and the tiles
link out to a terminal where `/agents`, `/hooks`, `/plugins` and `/mcp` do the real work.

### Auth Bridge

Browser-based logins run *inside* a container (`claude login`, `aws sso login`, Concourse
`fly login`) start an ephemeral HTTP listener on the container's loopback and expect the host
browser's redirect to reach it. `auth_bridge/` closes that gap:

- Listeners are discovered by parsing `/proc/net/tcp{,6}` every 2 seconds — the image ships no
  `ss`, `netstat` or `lsof`. Only `TCP_LISTEN` rows bound to loopback are considered; wildcard
  binds are deliberately ignored (that is the port-mappings feature's job).
- Each discovered port is bound on the host at **the same port number**, on `127.0.0.1` (required)
  and `[::1]` (best effort) — never a wildcard address. Node resolves `localhost` to IPv6 first, so
  `claude login` often binds `::1` alone; the bridge follows the family it actually finds.
- Traffic is carried in over the Docker API by an attached exec running `socat`, because container
  IPs are not routable from the host on Docker Desktop.
- Ports already covered by the project's port mappings are skipped, and a host port that is already
  in use is reported as a conflict rather than fought over.

Opt-in per project (`auth_bridge_enabled`, default `false`), purely host-side, so toggling it never
recreates the container. The poller stops on its own when the container stops.

**Security posture:** the host side binds loopback only. Everything reachable through it is an
unauthenticated service inside the container, so widening those addresses would publish container
internals to the LAN. Nothing else on the network can reach a bridged port.

### Shared Claude Authentication Token

Rather than running `claude login` in every container, `claude setup-token` can be run once
(`commands/auth_token_commands.rs`). The flow borrows a running container, runs the CLI on a PTY,
and the long-lived token it prints is stored in the OS keychain — it is never returned to the
frontend and never logged. Streamed output passes through a chunk-boundary-safe redactor that masks
anything resembling an `sk-ant-` secret.

The token is injected as `CLAUDE_CODE_OAUTH_TOKEN` into every project where the backend is
Anthropic, the project has not opted out (`use_shared_auth_token`, default `true`), and a token is
actually stored. It is a reserved env key, so it cannot be hand-set as a custom variable.

Rotation is tracked with a random id (not a hash of the token) mirrored into the
`triple-c.claude-token-version` label — a hash in a `docker inspect`-readable label would be an
offline verification oracle. Acquiring, rotating, revoking or opting out changes that label, which
forces a container recreation on the next start; that is when a container picks the token up or has
it cleared.

### Container Lifecycle

1. **Create**: New container created with bind mounts, named volumes, env vars, and labels
2. **Start**: Container started, entrypoint remaps UID/GID, sets up SSH, configures Docker group, injects Claude Code settings, rebuilds the scheduler crontab
3. **Terminal**: `docker exec` launches Claude Code (with the project's permission-mode flags) or a bash login shell, with a PTY
4. **Stop**: Container halted (its filesystem layer and both named volumes persist)
5. **Restart**: Existing container restarted; if any `triple-c.*` label no longer matches the project's settings, the container is committed to a snapshot image, removed, and recreated from that snapshot — so installed packages survive
6. **Reset**: Container, snapshot image **and both named volumes** all removed, then recreated from the clean base image. `remove_project_volumes` deletes `triple-c-home-{projectId}` and `triple-c-claude-config-{projectId}`, so `~/.claude`, `~/.claude.json`, the OAuth login, installed skills, session transcripts and the scheduler's tasks are all lost.

### Mounts

| Target in Container | Source | Type | Notes |
|---|---|---|---|
| `/workspace/<mount-name>` | Each configured project folder | Bind | Read-write; one per folder |
| `/home/claude` | `triple-c-home-{projectId}` | Named Volume | Home directory; survives stop/start and recreation |
| `/home/claude/.claude` | `triple-c-claude-config-{projectId}` | Named Volume | Nested inside the home volume; Docker gives the more specific mount precedence |
| `/tmp/.host-ssh` | SSH key directory | Bind | Read-only; entrypoint copies to `~/.ssh` |
| `/home/claude/.aws` | AWS config directory | Bind | Read-only; for Bedrock auth |
| `/var/run/docker.sock` | Host Docker socket | Bind | If "Allow container spawning" is ON |

These two named volumes are the only ones a project owns. Both are removed by Reset and by project
removal, and by nothing else.

### Authentication Modes

Each project can independently use one of:

- **Anthropic** (OAuth or shared token): either the shared `claude setup-token` token injected as `CLAUDE_CODE_OAUTH_TOKEN` (see below), or a per-container `claude login`. An interactive login's token lives in the config volume and survives container stop/start and recreation — but **not** a Reset, which deletes the volumes.
- **AWS Bedrock**: Per-project AWS credentials (static keys, profile, or bearer token). SSO sessions are validated before launching Claude for Profile auth.
- **Ollama**: Connect to a local or remote Ollama server via `ANTHROPIC_BASE_URL` (e.g., `http://host.docker.internal:11434`). Requires a model ID, and the model must be pulled (or used via Ollama cloud) before starting the container.
- **OpenAI Compatible**: Connect through any OpenAI API-compatible endpoint (LiteLLM, OpenRouter, vLLM, text-generation-inference, LocalAI, etc.) via `ANTHROPIC_BASE_URL` + `ANTHROPIC_AUTH_TOKEN`. API key stored securely in OS keychain.

> **Note:** Ollama and OpenAI Compatible support is best-effort. Claude Code is designed for Anthropic models, so some features (tool use, extended thinking, prompt caching, etc.) may not work as expected with non-Anthropic models behind these backends.

### Container Spawning (Sibling Containers)

When "Allow container spawning" is enabled per-project, the host Docker socket is bind-mounted into the container. This allows Claude Code to create **sibling containers** (not nested Docker-in-Docker) that are visible to the host. The entrypoint detects the socket's GID and adds the `claude` user to the matching group.

If the Docker access setting is toggled after a container already exists, the container is automatically recreated on next start to apply the mount change. The named config volume (keyed by project ID) is preserved across recreation.

### Mission Control Integration

Optional per-project integration with Flight Control — an AI-first development methodology bundled with Triple-C. When enabled, the bundled files are installed into the container, skills are installed, and workflow instructions are injected into CLAUDE.md.

### Web Terminal (Remote Access)

Triple-C includes an optional web terminal server for accessing project terminals from tablets, phones, or other devices on the local network. When enabled in Settings, an axum HTTP+WebSocket server starts inside the Tauri process, serving a standalone xterm.js-based terminal UI.

- **URL**: `http://<LAN_IP>:7681?token=...` (port configurable)
- **Authentication**: Token-based (auto-generated, copyable from Settings)
- **Protocol**: JSON over WebSocket with base64-encoded terminal data
- **Features**: Project picker, multiple tabs (Claude + bash sessions), mobile-optimized input bar, scroll-to-bottom button
- **Session cleanup**: All terminal sessions are closed when the browser disconnects

The web terminal shares the existing `ExecSessionManager` via `Arc`-wrapped stores — same Docker exec sessions, different transport (WebSocket instead of Tauri IPC events).

### Speech-to-Text (Voice Mode)

Triple-C includes optional speech-to-text powered by [Faster Whisper](https://github.com/SYSTRAN/faster-whisper) running in a separate Docker container. When enabled, a microphone button appears in the StatusBar whenever a terminal session is active.

- **Hotkey**: `Ctrl+Shift+M` to toggle recording
- **Models**: `tiny`, `small`, or `medium` (configurable in Settings)
- **Port**: Default `9876` (configurable)
- **Language**: Optional language hint for transcription
- **Auto-start**: When STT is enabled in Settings, the container starts automatically with the app — no need to manually start it after each restart
- **On-demand fallback**: If not auto-started, the container starts automatically when you first click the mic button

**How it works**: Audio is captured in the browser via the Web Audio API, encoded as WAV, and sent to the Faster Whisper container's `/transcribe` endpoint. The transcribed text is inserted directly into the active terminal. The STT container uses a named Docker volume (`triple-c-stt-model-cache`) to cache Whisper models across restarts.

### Docker Socket Path

The socket path is OS-aware:
- **Linux/macOS**: `/var/run/docker.sock`
- **Windows**: `//./pipe/docker_engine`

Users can override this in Settings via the global `docker_socket_path` option.

## Key Files

| File | Purpose |
|---|---|
| `app/src/App.tsx` | Root layout (TopBar + Sidebar + Main + StatusBar + ToastHost) |
| `app/src/index.css` | Global CSS variables, dark theme, `color-scheme: dark`, `:focus-visible` ring |
| `app/src/components/layout/TopBar.tsx` | Hosts MainTabs + Docker/Image status indicators + Help |
| `app/src/components/layout/MainTabs.tsx` | The single main-area tab strip (Project Home + terminal tabs) |
| `app/src/components/layout/Sidebar.tsx` | Responsive sidebar (25% width, min 224px, max 320px), collapsible to an icon rail |
| `app/src/components/layout/StatusBar.tsx` | Project/terminal counts, Jump to Current, STT mic |
| `app/src/components/projects/ProjectRow.tsx` | Select-only sidebar row; opens Project Home, with hover start/stop and terminal controls |
| `app/src/components/projects/ProjectList.tsx` | Project list in sidebar |
| `app/src/components/projects/PermissionModeControl.tsx` | Plan / Default / Accept Edits / Bypass segmented control |
| `app/src/components/projects/home/ProjectHome.tsx` | Project Home shell: header actions, overflow menu, tab strip |
| `app/src/components/projects/home/OverviewTab.tsx` | Permission mode, summary, capability tiles, recent sessions and tasks |
| `app/src/components/projects/home/SessionsTab.tsx` | Past Claude sessions with Resume |
| `app/src/components/projects/home/AutomationTab.tsx` | Scheduler tasks: toggle, run now, logs, remove, notifications |
| `app/src/components/projects/home/ConfigTab.tsx` | Config sections (Workspace, Model, Access, Runtime) |
| `app/src/components/projects/home/FilesTab.tsx` | File browser (browse, download, upload) |
| `app/src/components/projects/home/CapabilityTiles.tsx` | Read-only skills/agents/commands/hooks/plugins/MCP counts |
| `app/src/components/projects/ClaudeCodeSettingsEditor.tsx` | Claude Code CLI settings (TUI mode, effort, focus, caching) |
| `app/src/components/ui/` | Shared primitives: `Modal`, `Button`, `Toggle`, `Field`, `SegmentedControl`, `StatusIndicator`, `SaveIndicator`, `OverflowMenu`, `ToastHost`, `Tooltip` |
| `app/src/hooks/useKeyboardShortcuts.ts` | `Ctrl+T`, `Ctrl+Shift+W`, `Ctrl+Tab`, `Ctrl+1..9` |
| `app/src/hooks/useContainerProgress.ts` | `container-progress` event → inline progress lines |
| `app/src/components/settings/SettingsPanel.tsx` | Docker, AWS, timezone, web terminal, shared auth, and global settings |
| `app/src/components/settings/SharedAuthSettings.tsx` | Acquire / revoke the shared Claude authentication token |
| `app/src/components/settings/WebTerminalSettings.tsx` | Web terminal toggle, URL, token management |
| `app/src/components/settings/SttSettings.tsx` | STT settings panel (model, port, language, container controls) |
| `app/src/components/terminal/TerminalView.tsx` | xterm.js terminal with WebGL, URL detection, OSC 52 clipboard, image paste |
| `app/src/components/terminal/SttButton.tsx` | Mic button with on-demand STT container start |
| `app/src/hooks/useTerminal.ts` | Terminal session management (claude and bash modes) |
| `app/src/hooks/useProjectActions.ts` | Start/stop/reset/backup and terminal-opening helpers |
| `app/src/hooks/useFileManager.ts` | File manager operations (list, download, upload) |
| `app/src/hooks/useClaudeAuth.ts` | Shared-token status and acquisition |
| `app/src/hooks/useSTT.ts` | Speech-to-text recording, transcription, and container management |
| `app/src-tauri/src/docker/container.rs` | Container creation, mounts, env vars, labels, recreation checks, `remove_project_volumes` |
| `app/src-tauri/src/docker/exec.rs` | `create_attached_exec()` — the single attached-exec path; file upload/download via tar |
| `app/src-tauri/src/docker/image.rs` | Image building/pulling |
| `app/src-tauri/src/docker/stt.rs` | Speech-to-text container lifecycle |
| `app/src-tauri/src/docker/legacy_cleanup.rs` | One-release migration shim removing leftovers from the deleted MCP feature |
| `app/src-tauri/src/auth_bridge/` | Loopback callback bridge (`mod.rs`, `proc_net.rs`, `tunnel.rs`) |
| `app/src-tauri/src/commands/project_commands.rs` | Start/stop/rebuild Tauri command handlers |
| `app/src-tauri/src/commands/inspect_commands.rs` | Read-only container views: sessions, capabilities, scheduler tasks |
| `app/src-tauri/src/commands/auth_token_commands.rs` | `claude setup-token` flow, redaction, keychain storage |
| `app/src-tauri/src/commands/auth_bridge_commands.rs` | Auth bridge enable/status commands |
| `app/src-tauri/src/commands/file_commands.rs` | File manager Tauri commands (list, download, upload) |
| `app/src-tauri/src/models/project.rs` | Project struct (backend, `PermissionMode`, Docker access, Claude Code settings, Mission Control, auth bridge, shared-token opt-out) |
| `app/src-tauri/src/models/app_settings.rs` | Global settings (image source, Docker socket, AWS, Claude Code settings, web terminal, STT) |
| `app/src-tauri/src/web_terminal/server.rs` | Axum HTTP+WS server for remote terminal access |
| `app/src-tauri/src/web_terminal/ws_handler.rs` | WebSocket connection handler and session management |
| `app/src-tauri/src/web_terminal/terminal.html` | Embedded web UI (xterm.js, project picker, tabs) |
| `app/src-tauri/src/commands/stt_commands.rs` | STT start/stop/transcribe Tauri commands |
| `app/src-tauri/src/commands/web_terminal_commands.rs` | Web terminal start/stop/status Tauri commands |
| `app/src-tauri/src/docker/stt.rs` | STT Docker container lifecycle (create, start, stop, build, pull) |
| `app/src/lib/wav.ts` | WAV audio encoding for STT transcription |
| `stt-container/Dockerfile` | Faster Whisper STT container image (Python 3.11 + FastAPI) |
| `stt-container/server.py` | STT HTTP server (POST /transcribe endpoint) |
| `container/Dockerfile` | Ubuntu 24.04 sandbox image with Claude Code + dev tools + clipboard/audio shims |
| `container/entrypoint.sh` | UID/GID remap, SSH setup, Docker group config, Claude Code settings injection, Mission Control setup |
| `container/osc52-clipboard` | Clipboard shim (xclip/xsel/pbcopy via OSC 52) |
| `container/audio-shim` | Audio capture shim (rec/arecord via FIFO) for voice mode |
| `container/triple-c-scheduler` | Bash CLI managing scheduled task JSON and the crontab |
| `container/triple-c-task-runner` | Cron entry point; maps `TRIPLE_C_PERMISSION_MODE` to flags and runs `claude -p` |
| `container/triple-c-sso-refresh` | AWS SSO session refresh helper |
| `app/src-tauri/src/storage/secure.rs` | OS keychain access (per-project secrets, shared token, rotation id) |

## CSS / Styling Notes

- Uses **Tailwind CSS v4** with the Vite plugin (`@tailwindcss/vite`)
- All colors use CSS custom properties defined in `index.css` `:root`
- `color-scheme: dark` is set on `:root` so native form controls (select dropdowns, scrollbars) render in dark mode
- **Do not** add a global `* { padding: 0 }` reset — Tailwind v4 uses CSS `@layer`, and unlayered CSS overrides all layered utilities. Tailwind's built-in Preflight handles resets.

## Container Image

**Base**: Ubuntu 24.04

**Pre-installed tools**: Claude Code, Node.js 22 LTS + pnpm, Python 3.12 + uv + ruff, Rust (stable), Docker CLI, git + gh, AWS CLI v2, ripgrep, openssh-client, build-essential

**Shims**: `xclip`/`xsel`/`pbcopy` (OSC 52 clipboard forwarding), `rec`/`arecord` (audio FIFO for voice mode)

**Default user**: `claude` (UID/GID 1000, remapped by entrypoint to match host)
