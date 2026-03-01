# How to Use Triple-C

Triple-C (Claude-Code-Container) is a desktop application that runs Claude Code inside isolated Docker containers. Each project gets its own sandboxed environment with bind-mounted directories, so Claude only has access to the files you explicitly provide.

---

## Prerequisites

### Docker

Triple-C requires a running Docker daemon. Install one of the following:

| Platform | Option | Link |
|----------|--------|------|
| **Windows** | Docker Desktop | https://docs.docker.com/desktop/install/windows-install/ |
| **macOS** | Docker Desktop | https://docs.docker.com/desktop/install/mac-install/ |
| **Linux** | Docker Engine | https://docs.docker.com/engine/install/ |
| **Linux** | Docker Desktop (alternative) | https://docs.docker.com/desktop/install/linux/ |

After installation, verify Docker is running:

```bash
docker info
```

> **Windows note:** Docker Desktop must be running before launching Triple-C. The app communicates with Docker through the named pipe at `//./pipe/docker_engine`.

> **Linux note:** Your user must have permission to access the Docker socket (`/var/run/docker.sock`). Either add your user to the `docker` group (`sudo usermod -aG docker $USER`, then log out and back in) or run Docker in rootless mode.

### Claude Code Account

You need access to Claude Code through one of:

- **Anthropic account** — Sign up at https://claude.ai and use `claude login` (OAuth) inside the terminal
- **AWS Bedrock** — An AWS account with Bedrock access and Claude models enabled

---

## First Launch

### 1. Get the Container Image

When you first open Triple-C, go to the **Settings** tab in the sidebar. Under **Docker**, you'll see:

- **Docker Status** — Should show "Connected" (green). If it shows "Not Available", make sure Docker is running.
- **Image Status** — Will show "Not Found" on first launch.

Choose an **Image Source**:

| Source | Description | When to Use |
|--------|-------------|-------------|
| **Registry** | Pulls the pre-built image from `repo.anhonesthost.net` | Fastest setup — recommended for most users |
| **Local Build** | Builds the image locally from the embedded Dockerfile | If you can't reach the registry, or want a custom build |
| **Custom** | Use any Docker image you specify | Advanced — bring your own sandbox image |

Click **Pull Image** (for Registry/Custom) or **Build Image** (for Local Build). A progress log will stream below the button. When complete, the status changes to "Ready" (green).

### 2. Create Your First Project

Switch to the **Projects** tab in the sidebar and click the **+** button.

1. **Project Name** — Give it a meaningful name (e.g., "my-web-app").
2. **Folders** — Click **Browse** to select a directory on your host machine. This directory will be mounted into the container at `/workspace/<folder-name>`. You can add multiple folders with the **+** button at the bottom of the folder list.
3. Click **Add Project**.

### 3. Start the Container

Select your project in the sidebar and click **Start**. The status dot changes from gray (stopped) to orange (starting) to green (running).

### 4. Open a Terminal

Click the **Terminal** button (highlighted in accent color) to open an interactive terminal session. A new tab appears in the top bar and an xterm.js terminal loads in the main area.

Claude Code launches automatically with `--dangerously-skip-permissions` inside the sandboxed container.

### 5. Authenticate

**Anthropic (OAuth) — default:**

1. Type `claude login` or `/login` in the terminal.
2. Claude prints an OAuth URL. Triple-C detects long URLs and shows a clickable toast at the top of the terminal — click **Open** to open it in your browser.
3. Complete the login in your browser. The token is saved and persists across container stops and resets.

**AWS Bedrock:**

1. Stop the container first (settings can only be changed while stopped).
2. In the project card, switch the auth mode to **Bedrock**.
3. Expand the **Config** panel and fill in your AWS credentials (see [AWS Bedrock Configuration](#aws-bedrock-configuration) below).
4. Start the container again.

---

## The Interface

```
┌─────────────────────────────────────────────────────┐
│  TopBar  [ Terminal Tabs ]           Docker ● Image ●│
├────────────┬────────────────────────────────────────┤
│  Sidebar   │                                        │
│            │          Terminal View                  │
│  Projects  │         (xterm.js)                     │
│  Settings  │                                        │
│            │                                        │
├────────────┴────────────────────────────────────────┤
│  StatusBar   X projects · X running · X terminals   │
└─────────────────────────────────────────────────────┘
```

- **TopBar** — Terminal tabs for switching between sessions. Status dots on the right show Docker connection (green = connected) and image availability (green = ready).
- **Sidebar** — Toggle between the **Projects** list and **Settings** panel.
- **Terminal View** — Interactive terminal powered by xterm.js with WebGL rendering.
- **StatusBar** — Counts of total projects, running containers, and open terminal sessions.

---

## Project Management

### Project Status

Each project shows a colored status dot:

| Color | Status | Meaning |
|-------|--------|---------|
| Gray | Stopped | Container is not running |
| Orange | Starting / Stopping | Container is transitioning |
| Green | Running | Container is active, ready for terminals |
| Red | Error | Something went wrong (check error message) |

### Project Actions

Select a project in the sidebar to see its action buttons:

| Button | When Available | What It Does |
|--------|---------------|--------------|
| **Start** | Stopped | Creates (if needed) and starts the container |
| **Stop** | Running | Stops the container but preserves its state |
| **Terminal** | Running | Opens a new terminal session in this container |
| **Reset** | Stopped | Destroys and recreates the container from scratch |
| **Config** | Always | Toggles the configuration panel |
| **Remove** | Stopped | Deletes the project and its container (with confirmation) |

### Container Lifecycle

Containers use a **stop/start** model. When you stop a container, everything inside it is preserved — installed packages, modified files, downloaded tools. Starting it again resumes where you left off.

**Reset** removes the container and creates a fresh one. However, your Claude Code configuration (including OAuth tokens from `claude login`) is stored in a separate Docker volume and survives resets.

Only **Remove** deletes everything, including the config volume and any stored credentials.

---

## Project Configuration

Click **Config** on a selected project to expand the configuration panel. Settings can only be changed when the container is **stopped** (an orange warning box appears if the container is running).

### Mounted Folders

Each project mounts one or more host directories into the container. The mount appears at `/workspace/<mount-name>` inside the container.

- Click **Browse** ("...") to change the host path
- Edit the mount name to control where it appears inside `/workspace/`
- Click **+** to add more folders, or **x** to remove one
- Mount names must be unique and use only letters, numbers, dashes, underscores, and dots

### SSH Keys

Specify the path to your SSH key directory (typically `~/.ssh`). Keys are mounted read-only and copied into the container with correct permissions. This enables `git clone` via SSH inside the container.

### Git Configuration

- **Git Name / Email** — Sets `git config user.name` and `user.email` inside the container.
- **Git HTTPS Token** — A personal access token (e.g., from GitHub) for HTTPS git operations. Stored securely in your OS keychain — never written to disk in plaintext.

### Allow Container Spawning

When enabled, the host Docker socket is mounted into the container so Claude Code can create sibling containers (e.g., for running databases, test environments). This is **off by default** for security.

> Toggling this requires stopping and restarting the container to take effect.

### Environment Variables

Click **Edit** to open the environment variables modal. Add key-value pairs that will be injected into the container. Per-project variables override global variables with the same key.

> Reserved prefixes (`ANTHROPIC_`, `AWS_`, `GIT_`, `HOST_`, `CLAUDE_`, `TRIPLE_C_`) are filtered out to prevent conflicts with internal variables.

### Port Mappings

Click **Edit** to map host ports to container ports. This is useful when Claude Code starts a web server or other service inside the container and you want to access it from your host browser.

Each mapping specifies:
- **Host Port** — The port on your machine (1–65535)
- **Container Port** — The port inside the container (1–65535)
- **Protocol** — TCP (default) or UDP

### Claude Instructions

Click **Edit** to write per-project instructions for Claude Code. These are written to `~/.claude/CLAUDE.md` inside the container and provide project-specific context. If you also have global instructions (in Settings), the global instructions come first, followed by the per-project instructions.

---

## AWS Bedrock Configuration

To use Claude via AWS Bedrock instead of Anthropic's API, switch the auth mode to **Bedrock** on the project card.

### Authentication Methods

| Method | Fields | Use Case |
|--------|--------|----------|
| **Keys** | Access Key ID, Secret Access Key, Session Token (optional) | Direct credentials — simplest setup |
| **Profile** | AWS Profile name | Uses `~/.aws/config` and `~/.aws/credentials` on the host |
| **Token** | Bearer Token | Temporary bearer token authentication |

### Additional Bedrock Settings

- **AWS Region** — Required. The region where your Bedrock models are deployed (e.g., `us-east-1`).
- **Model ID** — Optional. Override the default Claude model (e.g., `anthropic.claude-sonnet-4-20250514-v1:0`).

### Global AWS Defaults

In **Settings > AWS Configuration**, you can set defaults that apply to all Bedrock projects:

- **AWS Config Path** — Path to your `~/.aws` directory. Click **Detect** to auto-find it.
- **Default Profile** — Select from profiles found in your AWS config.
- **Default Region** — Fallback region for projects that don't specify one.

Per-project settings always override these global defaults.

---

## Settings

Access global settings via the **Settings** tab in the sidebar.

### Docker Settings

- **Docker Status** — Connection status to the Docker daemon.
- **Image Source** — Where to get the sandbox container image (Registry, Local Build, or Custom).
- **Pull / Build Image** — Download or build the image. Progress streams in real time.
- **Refresh** — Re-check Docker and image status.

### Container Timezone

Set the timezone for all containers (IANA format, e.g., `America/New_York`, `Europe/London`, `UTC`). Auto-detected from your host on first launch. This affects scheduled task timing inside containers.

### Global Claude Instructions

Instructions applied to **all** projects. Written to `~/.claude/CLAUDE.md` in every container, before any per-project instructions.

### Global Environment Variables

Environment variables applied to **all** project containers. Per-project variables with the same key take precedence.

### Updates

- **Current Version** — The installed version of Triple-C.
- **Auto-check** — Toggle automatic update checks (every 24 hours).
- **Check now** — Manually check for updates.

When an update is available, a pulsing **Update** button appears in the top bar. Click it to see release notes and download links.

---

## Terminal Features

### Multiple Sessions

You can open multiple terminal sessions (even for the same project). Each session gets its own tab in the top bar. Click a tab to switch, or click the **x** on a tab to close it.

### URL Detection

When Claude Code prints a long URL (e.g., during `claude login`), Triple-C detects it and shows a toast notification at the top of the terminal with an **Open** button. Clicking it opens the URL in your default browser. The toast auto-dismisses after 30 seconds.

Shorter URLs in terminal output are also clickable directly.

### Image Paste

You can paste images from your clipboard into the terminal (Ctrl+V / Cmd+V). The image is uploaded to the container and the file path is injected into the terminal input so Claude Code can reference it.

### Terminal Rendering

The terminal uses WebGL for hardware-accelerated rendering of the active tab. Inactive tabs fall back to canvas rendering to conserve GPU resources. The terminal automatically resizes when you resize the window.

---

## Scheduled Tasks (Inside the Container)

Once inside a running container terminal, you can set up recurring or one-time tasks using `triple-c-scheduler`. Tasks run as separate Claude Code sessions.

### Create a Recurring Task

```bash
triple-c-scheduler add --name "daily-review" --schedule "0 9 * * *" --prompt "Review open issues and summarize"
```

### Create a One-Time Task

```bash
triple-c-scheduler add --name "migrate-db" --at "2026-03-05 14:00" --prompt "Run database migrations"
```

One-time tasks automatically remove themselves after execution.

### Manage Tasks

```bash
triple-c-scheduler list                    # List all tasks
triple-c-scheduler enable --id abc123      # Enable a task
triple-c-scheduler disable --id abc123     # Disable a task
triple-c-scheduler remove --id abc123      # Delete a task
triple-c-scheduler run --id abc123         # Trigger a task immediately
triple-c-scheduler logs --id abc123        # View logs for a task
triple-c-scheduler logs --tail 20          # View last 20 log entries (all tasks)
triple-c-scheduler notifications           # View completion notifications
triple-c-scheduler notifications --clear   # Clear notifications
```

### Cron Schedule Format

Standard 5-field cron: `minute hour day-of-month month day-of-week`

| Example | Meaning |
|---------|---------|
| `*/30 * * * *` | Every 30 minutes |
| `0 9 * * 1-5` | 9:00 AM on weekdays |
| `0 */2 * * *` | Every 2 hours |
| `0 0 1 * *` | Midnight on the 1st of each month |

### Working Directory

By default, tasks run in `/workspace`. Use `--working-dir` to specify a different directory:

```bash
triple-c-scheduler add --name "test" --schedule "0 */6 * * *" --prompt "Run tests" --working-dir /workspace/my-project
```

---

## What's Inside the Container

The sandbox container (Ubuntu 24.04) comes pre-installed with:

| Tool | Version | Purpose |
|------|---------|---------|
| Claude Code | Latest | AI coding assistant (the tool being sandboxed) |
| Node.js | 22 LTS | JavaScript/TypeScript development |
| pnpm | Latest | Fast Node.js package manager |
| Python | 3.12 | Python development |
| uv | Latest | Fast Python package manager |
| ruff | Latest | Python linter/formatter |
| Rust | Stable | Rust development (via rustup) |
| Docker CLI | Latest | Container management (when spawning is enabled) |
| git | Latest | Version control |
| GitHub CLI (gh) | Latest | GitHub integration |
| AWS CLI | v2 | AWS services and Bedrock |
| ripgrep | Latest | Fast code search |
| build-essential | — | C/C++ compiler toolchain |
| openssh-client | — | SSH for git and remote access |

You can install additional tools at runtime with `sudo apt install`, `pip install`, `npm install -g`, etc. Installed packages persist across container stops (but not across resets).

---

## Troubleshooting

### Docker is "Not Available"

- **Is Docker running?** Start Docker Desktop or the Docker daemon (`sudo systemctl start docker`).
- **Permissions?** On Linux, ensure your user is in the `docker` group or the socket is accessible.
- **Custom socket path?** If your Docker socket is not at the default location, set it in Settings. The app expects `/var/run/docker.sock` on Linux/macOS or `//./pipe/docker_engine` on Windows.

### Image is "Not Found"

- Click **Pull Image** or **Build Image** in Settings > Docker.
- If pulling fails, check your network connection and whether you can reach the registry.
- Try switching to **Local Build** as an alternative.

### Container Won't Start

- Check that the Docker image is "Ready" in Settings.
- Verify that the mounted folder paths exist on your host.
- Look at the error message displayed in red below the project card.

### OAuth Login URL Not Opening

- Triple-C detects long URLs printed by `claude login` and shows a toast with an **Open** button.
- If the toast doesn't appear, try scrolling up in the terminal — the URL may have already been printed.
- You can also manually copy the URL from the terminal output and paste it into your browser.

### File Permission Issues

- Triple-C automatically remaps the container user's UID/GID to match your host user, so files created inside the container should have the correct ownership on your host.
- If you see permission errors, try resetting the container (stop, then click **Reset**).

### Settings Won't Save

- Most project settings can only be changed when the container is **stopped**. Stop the container first, make your changes, then start it again.
- Some changes (like toggling Docker access or changing mounted folders) trigger an automatic container recreation on the next start.
