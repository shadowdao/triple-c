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
2. **Start**: Container started, entrypoint remaps UID/GID, sets up SSH, configures Docker group
3. **Terminal**: `docker exec` launches Claude Code with a PTY
4. **Stop**: Container halted (filesystem persists in named volume)
5. **Restart**: Existing container restarted; recreated if settings changed (e.g., Docker access toggled)
6. **Reset**: Container removed and recreated from scratch (named volume preserved)

### Mounts

| Target in Container | Source | Type | Notes |
|---|---|---|---|
| `/workspace` | Project directory | Bind | Read-write |
| `/home/claude/.claude` | `triple-c-claude-config-{projectId}` | Named Volume | Persists across container recreation |
| `/tmp/.host-ssh` | SSH key directory | Bind | Read-only; entrypoint copies to `~/.ssh` |
| `/home/claude/.aws` | AWS config directory | Bind | Read-only; for Bedrock auth |
| `/var/run/docker.sock` | Host Docker socket | Bind | Only if "Allow container spawning" is ON |

### Authentication Modes

Each project can independently use one of:

- **`/login`** (OAuth): User runs `claude login` inside the terminal. Token persisted in the config volume.
- **API Key**: Stored in the OS keychain, injected as `ANTHROPIC_API_KEY` env var.
- **AWS Bedrock**: Per-project AWS credentials (static keys, profile, or bearer token).

### Container Spawning (Sibling Containers)

When "Allow container spawning" is enabled per-project, the host Docker socket is bind-mounted into the container. This allows Claude Code to create **sibling containers** (not nested Docker-in-Docker) that are visible to the host. The entrypoint detects the socket's GID and adds the `claude` user to the matching group.

If the Docker access setting is toggled after a container already exists, the container is automatically recreated on next start to apply the mount change. The named config volume (keyed by project ID) is preserved across recreation.

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
| `app/src/components/settings/SettingsPanel.tsx` | API key, Docker, AWS settings |
| `app/src/components/terminal/TerminalView.tsx` | xterm.js terminal with WebGL, URL detection |
| `app/src/components/terminal/TerminalTabs.tsx` | Tab bar for multiple terminal sessions |
| `app/src-tauri/src/docker/container.rs` | Container creation, mounts, env vars, inspection |
| `app/src-tauri/src/docker/exec.rs` | PTY exec sessions for terminal interaction |
| `app/src-tauri/src/docker/image.rs` | Image building/pulling |
| `app/src-tauri/src/commands/project_commands.rs` | Start/stop/rebuild Tauri command handlers |
| `app/src-tauri/src/models/project.rs` | Project struct (auth mode, Docker access, etc.) |
| `app/src-tauri/src/models/app_settings.rs` | Global settings (image source, Docker socket, AWS) |
| `container/Dockerfile` | Ubuntu 24.04 sandbox image with Claude Code + dev tools |
| `container/entrypoint.sh` | UID/GID remap, SSH setup, Docker group config |

## CSS / Styling Notes

- Uses **Tailwind CSS v4** with the Vite plugin (`@tailwindcss/vite`)
- All colors use CSS custom properties defined in `index.css` `:root`
- `color-scheme: dark` is set on `:root` so native form controls (select dropdowns, scrollbars) render in dark mode
- **Do not** add a global `* { padding: 0 }` reset — Tailwind v4 uses CSS `@layer`, and unlayered CSS overrides all layered utilities. Tailwind's built-in Preflight handles resets.

## Container Image

**Base**: Ubuntu 24.04

**Pre-installed tools**: Claude Code, Node.js 22 LTS + pnpm, Python 3.12 + uv + ruff, Rust (stable), Docker CLI, git + gh, AWS CLI v2, ripgrep, openssh-client, build-essential

**Default user**: `claude` (UID/GID 1000, remapped by entrypoint to match host)