# Triple-C (Claude-Code-Container)

Triple-C is a cross-platform desktop application that sandboxes Claude Code inside Docker containers. When running with `--dangerously-skip-permissions`, Claude only has access to the files and projects you explicitly provide to it.

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
2. **Start**: Container started, entrypoint remaps UID/GID, sets up SSH, configures Docker group, sets up MCP servers
3. **Terminal**: `docker exec` launches Claude Code (or bash shell) with a PTY
4. **Stop**: Container halted (filesystem persists in named volume); MCP containers stopped
5. **Restart**: Existing container restarted; recreated if settings changed (detected via SHA-256 fingerprint)
6. **Reset**: Container removed and recreated from scratch (named volume preserved)

### Mounts

| Target in Container | Source | Type | Notes |
|---|---|---|---|
| `/workspace` | Project directory | Bind | Read-write |
| `/home/claude/.claude` | `triple-c-claude-config-{projectId}` | Named Volume | Persists across container recreation |
| `/tmp/.host-ssh` | SSH key directory | Bind | Read-only; entrypoint copies to `~/.ssh` |
| `/home/claude/.aws` | AWS config directory | Bind | Read-only; for Bedrock auth |
| `/var/run/docker.sock` | Host Docker socket | Bind | If "Allow container spawning" is ON, or auto-enabled by stdio+Docker MCP servers |

### Authentication Modes

Each project can independently use one of:

- **Anthropic** (OAuth): User runs `claude login` inside the terminal on first use. Token persisted in the config volume across restarts and resets.
- **AWS Bedrock**: Per-project AWS credentials (static keys, profile, or bearer token). SSO sessions are validated before launching Claude for Profile auth.

### Container Spawning (Sibling Containers)

When "Allow container spawning" is enabled per-project, the host Docker socket is bind-mounted into the container. This allows Claude Code to create **sibling containers** (not nested Docker-in-Docker) that are visible to the host. The entrypoint detects the socket's GID and adds the `claude` user to the matching group.

If the Docker access setting is toggled after a container already exists, the container is automatically recreated on next start to apply the mount change. The named config volume (keyed by project ID) is preserved across recreation.

### MCP Server Architecture

Triple-C supports [Model Context Protocol (MCP)](https://modelcontextprotocol.io/) servers as a Beta feature. MCP servers extend Claude Code with external tools and data sources.

- **Global library**: MCP servers are defined globally in the MCP sidebar tab and stored in `mcp_servers.json`
- **Per-project toggles**: Each project enables/disables individual servers via checkboxes
- **Docker isolation**: MCP servers can run as isolated Docker containers on a per-project bridge network (`triple-c-net-{projectId}`)
- **Transport types**: Stdio (command-line) and HTTP (network endpoint), each with manual or Docker mode
- **Auto-detection**: Config changes are detected via SHA-256 fingerprints and trigger automatic container recreation

### Mission Control Integration

Optional per-project integration with [Flight Control](https://github.com/msieurthenardier/mission-control) — an AI-first development methodology. When enabled, the repo is cloned into the container, skills are installed, and workflow instructions are injected into CLAUDE.md.

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
| `app/src/components/projects/ProjectCard.tsx` | Project config, auth mode, action buttons |
| `app/src/components/projects/ProjectList.tsx` | Project list in sidebar |
| `app/src/components/projects/FileManagerModal.tsx` | File browser modal (browse, download, upload) |
| `app/src/components/projects/ContainerProgressModal.tsx` | Real-time container operation progress |
| `app/src/components/mcp/McpPanel.tsx` | MCP server library (global configuration) |
| `app/src/components/mcp/McpServerCard.tsx` | Individual MCP server configuration card |
| `app/src/components/settings/SettingsPanel.tsx` | Docker, AWS, timezone, and global settings |
| `app/src/components/terminal/TerminalView.tsx` | xterm.js terminal with WebGL, URL detection, OSC 52 clipboard, image paste |
| `app/src/components/terminal/TerminalTabs.tsx` | Tab bar for multiple terminal sessions (claude + bash) |
| `app/src/hooks/useTerminal.ts` | Terminal session management (claude and bash modes) |
| `app/src/hooks/useFileManager.ts` | File manager operations (list, download, upload) |
| `app/src/hooks/useMcpServers.ts` | MCP server CRUD operations |
| `app/src/hooks/useVoice.ts` | Voice mode audio capture (currently hidden) |
| `app/src-tauri/src/docker/container.rs` | Container creation, mounts, env vars, MCP injection, fingerprinting |
| `app/src-tauri/src/docker/exec.rs` | PTY exec sessions, file upload/download via tar |
| `app/src-tauri/src/docker/image.rs` | Image building/pulling |
| `app/src-tauri/src/docker/network.rs` | Per-project bridge networks for MCP containers |
| `app/src-tauri/src/commands/project_commands.rs` | Start/stop/rebuild Tauri command handlers |
| `app/src-tauri/src/commands/file_commands.rs` | File manager Tauri commands (list, download, upload) |
| `app/src-tauri/src/commands/mcp_commands.rs` | MCP server CRUD Tauri commands |
| `app/src-tauri/src/models/project.rs` | Project struct (auth mode, Docker access, MCP servers, Mission Control) |
| `app/src-tauri/src/models/mcp_server.rs` | MCP server struct (transport, Docker image, env vars) |
| `app/src-tauri/src/models/app_settings.rs` | Global settings (image source, Docker socket, AWS, microphone) |
| `app/src-tauri/src/storage/mcp_store.rs` | MCP server persistence (JSON with atomic writes) |
| `container/Dockerfile` | Ubuntu 24.04 sandbox image with Claude Code + dev tools + clipboard/audio shims |
| `container/entrypoint.sh` | UID/GID remap, SSH setup, Docker group config, MCP injection, Mission Control setup |
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
