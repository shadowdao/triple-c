# Triple-C (Claude-Code-Container)

Triple-C is a cross-platform desktop application that sandboxes Claude Code inside Docker containers. Each project can optionally enable full permissions mode (`--dangerously-skip-permissions`), giving Claude unrestricted access within the sandbox.

## Architecture

- **Frontend**: React 19 + TypeScript + Tailwind CSS v4 + Zustand state management
- **Backend**: Rust (Tauri v2 framework)
- **Terminal**: xterm.js with WebGL rendering
- **Docker API**: bollard (pure Rust Docker client)

### Layout Structure

```
┌─────────────────────────────────────────────────────┐
│  TopBar (terminal tabs + Docker/Image status)       │
├────────────┬────────────────────────────────────────┤
│  Sidebar   │  Main Content (terminal views)         │
│  (25% w,   │                                        │
│  responsive│                                        │
│  min/max)  │                                        │
├────────────┴────────────────────────────────────────┤
│  StatusBar (project/terminal counts)                │
└─────────────────────────────────────────────────────┘
```

### Container Lifecycle

1. **Create**: New container created with bind mounts, env vars, and labels
2. **Start**: Container started, entrypoint remaps UID/GID, sets up SSH, configures Docker group, injects Claude Code settings
3. **Terminal**: `docker exec` launches Claude Code (or bash shell) with a PTY
4. **Stop**: Container halted (filesystem persists in named volume)
5. **Restart**: Existing container restarted; recreated if settings changed (detected via SHA-256 fingerprint)
6. **Reset**: Container removed and recreated from scratch (named volume preserved)

### Mounts

| Target in Container | Source | Type | Notes |
|---|---|---|---|
| `/workspace` | Project directory | Bind | Read-write |
| `/home/claude/.claude` | `triple-c-claude-config-{projectId}` | Named Volume | Persists across container recreation |
| `/tmp/.host-ssh` | SSH key directory | Bind | Read-only; entrypoint copies to `~/.ssh` |
| `/home/claude/.aws` | AWS config directory | Bind | Read-only; for Bedrock auth |
| `/var/run/docker.sock` | Host Docker socket | Bind | If "Allow container spawning" is ON |

### Authentication Modes

Each project can independently use one of:

- **Anthropic** (OAuth): User runs `claude login` inside the terminal on first use. Token persisted in the config volume across restarts and resets.
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

Triple-C includes optional speech-to-text powered by [Faster Whisper](https://github.com/SYSTRAN/faster-whisper) running in a separate Docker container. When enabled, a microphone button appears in the bottom-left corner of each terminal view.

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
| `app/src/App.tsx` | Root layout (TopBar + Sidebar + Main + StatusBar) |
| `app/src/index.css` | Global CSS variables, dark theme, `color-scheme: dark` |
| `app/src/components/layout/TopBar.tsx` | Terminal tabs + Docker/Image status indicators |
| `app/src/components/layout/Sidebar.tsx` | Responsive sidebar (25% width, min 224px, max 320px) |
| `app/src/components/layout/StatusBar.tsx` | Running project/terminal counts |
| `app/src/components/projects/ProjectCard.tsx` | Project config, backend selector, action buttons |
| `app/src/components/projects/ClaudeCodeSettingsModal.tsx` | Claude Code CLI settings modal (TUI mode, effort, focus, caching) |
| `app/src/components/projects/ProjectList.tsx` | Project list in sidebar |
| `app/src/components/projects/FileManagerModal.tsx` | File browser modal (browse, download, upload) |
| `app/src/components/projects/ContainerProgressModal.tsx` | Real-time container operation progress |
| `app/src/components/settings/SettingsPanel.tsx` | Docker, AWS, timezone, web terminal, and global settings |
| `app/src/components/settings/WebTerminalSettings.tsx` | Web terminal toggle, URL, token management |
| `app/src/components/settings/SttSettings.tsx` | STT settings panel (model, port, language, container controls) |
| `app/src/components/terminal/TerminalView.tsx` | xterm.js terminal with WebGL, URL detection, OSC 52 clipboard, image paste |
| `app/src/components/terminal/SttButton.tsx` | Mic button overlay with on-demand container start |
| `app/src/components/terminal/TerminalTabs.tsx` | Tab bar for multiple terminal sessions (claude + bash) |
| `app/src/hooks/useTerminal.ts` | Terminal session management (claude and bash modes) |
| `app/src/hooks/useFileManager.ts` | File manager operations (list, download, upload) |
| `app/src/hooks/useSTT.ts` | Speech-to-text recording, transcription, and container management |
| `app/src-tauri/src/docker/container.rs` | Container creation, mounts, env vars, fingerprinting |
| `app/src-tauri/src/docker/exec.rs` | PTY exec sessions, file upload/download via tar |
| `app/src-tauri/src/docker/image.rs` | Image building/pulling |
| `app/src-tauri/src/commands/project_commands.rs` | Start/stop/rebuild Tauri command handlers |
| `app/src-tauri/src/commands/file_commands.rs` | File manager Tauri commands (list, download, upload) |
| `app/src-tauri/src/models/project.rs` | Project struct (backend, Docker access, Claude Code settings, Mission Control) |
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
