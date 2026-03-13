# How to Use Triple-C

Triple-C (Claude-Code-Container) is a desktop application that runs Claude Code inside isolated Docker containers. Each project gets its own sandboxed environment with bind-mounted directories, so Claude only has access to the files you explicitly provide.

---

## Table of Contents

- [Prerequisites](#prerequisites)
- [First Launch](#first-launch)
- [The Interface](#the-interface)
- [Project Management](#project-management)
- [Project Configuration](#project-configuration)
- [MCP Servers (Beta)](#mcp-servers-beta)
- [AWS Bedrock Configuration](#aws-bedrock-configuration)
- [Ollama Configuration](#ollama-configuration)
- [OpenAI Compatible Configuration](#openai-compatible-configuration)
- [Settings](#settings)
- [Terminal Features](#terminal-features)
- [Scheduled Tasks (Inside the Container)](#scheduled-tasks-inside-the-container)
- [What's Inside the Container](#whats-inside-the-container)
- [Troubleshooting](#troubleshooting)

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
- **Ollama** — A local or remote Ollama server running an Anthropic-compatible model (best-effort support)
- **OpenAI Compatible** — Any OpenAI API-compatible endpoint (LiteLLM, OpenRouter, vLLM, text-generation-inference, LocalAI, etc.) (best-effort support)

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

Select your project in the sidebar and click **Start**. A progress modal appears showing real-time status as the container starts. The status dot changes from gray (stopped) to orange (starting) to green (running). The modal auto-closes on success.

### 4. Open a Terminal

Click the **Terminal** button to open an interactive terminal session. A new tab appears in the top bar and an xterm.js terminal loads in the main area.

Claude Code launches automatically. By default, it runs in standard permission mode and will ask for your approval before executing commands or editing files. To enable auto-approval of all actions within the sandbox, enable **Full Permissions** in the project configuration.

### 5. Authenticate

**Anthropic (OAuth) — default:**

1. Type `claude login` or `/login` in the terminal.
2. Claude prints an OAuth URL. Triple-C detects long URLs and shows a clickable toast at the top of the terminal — click **Open** to open it in your browser.
3. Complete the login in your browser. The token is saved and persists across container stops and resets.

**AWS Bedrock:**

1. Stop the container first (settings can only be changed while stopped).
2. In the project card, switch the backend to **Bedrock**.
3. Expand the **Config** panel and fill in your AWS credentials (see [AWS Bedrock Configuration](#aws-bedrock-configuration) below).
4. Start the container again.

**Ollama:**

1. Stop the container first (settings can only be changed while stopped).
2. In the project card, switch the backend to **Ollama**.
3. Expand the **Config** panel and set the base URL of your Ollama server (defaults to `http://host.docker.internal:11434` for a local instance). Set the **Model ID** to the model you want to use (required).
4. Make sure the model has been pulled in Ollama (e.g., `ollama pull qwen3.5:27b`) or used via Ollama cloud before starting.
5. Start the container again.

**OpenAI Compatible:**

1. Stop the container first (settings can only be changed while stopped).
2. In the project card, switch the backend to **OpenAI Compatible**.
3. Expand the **Config** panel and set the base URL of your OpenAI-compatible endpoint (defaults to `http://host.docker.internal:4000` as an example). Optionally set an API key and model ID.
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
│  MCP       │                                        │
│  Settings  │                                        │
├────────────┴────────────────────────────────────────┤
│  StatusBar   X projects · X running · X terminals   │
└─────────────────────────────────────────────────────┘
```

- **TopBar** — Terminal tabs for switching between sessions. Bash shell tabs show a "(bash)" suffix. Status dots on the right show Docker connection (green = connected) and image availability (green = ready).
- **Sidebar** — Toggle between the **Projects** list, **MCP** server configuration, and **Settings** panel.
- **Terminal View** — Interactive terminal powered by xterm.js with WebGL rendering. Includes a **Jump to Current** button that appears when you scroll up, so you can quickly return to the latest output.
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
| **Terminal** | Running | Opens a new Claude Code terminal session |
| **Shell** | Running | Opens a bash login shell in the container (no Claude Code) |
| **Files** | Running | Opens the file manager to browse, download, and upload files |
| **Reset** | Stopped | Destroys and recreates the container from scratch |
| **Config** | Always | Toggles the configuration panel |
| **Remove** | Stopped | Deletes the project and its container (with confirmation) |

### Renaming a Project

Double-click the project name in the sidebar to rename it inline. Press **Enter** to confirm or **Escape** to cancel.

### Container Lifecycle

Containers use a **stop/start** model. When you stop a container, everything inside it is preserved — installed packages, modified files, downloaded tools. Starting it again resumes where you left off.

**Reset** removes the container and creates a fresh one. However, your Claude Code configuration (including OAuth tokens from `claude login`) is stored in a separate Docker volume and survives resets.

Only **Remove** deletes everything, including the config volume and any stored credentials.

### Container Progress Feedback

When starting, stopping, or resetting a container, a progress modal shows real-time status messages (e.g., "Setting up MCP network...", "Starting MCP containers...", "Creating container..."). If an error occurs, the modal displays the error with a **Close** button. A **Force Stop** option is available if the operation stalls. The modal auto-closes on success.

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

### Mission Control

Toggle **Mission Control** to integrate [Flight Control](https://github.com/msieurthenardier/mission-control) — an AI-first development methodology — into the project. When enabled:

- The Flight Control repository is automatically cloned into the container
- Flight Control skills are installed to Claude Code's skill directory (`~/.claude/skills/`)
- Project instructions are appended with Flight Control workflow guidance
- The repository is symlinked at `/workspace/mission-control`

Available skills include `/mission`, `/flight`, `/leg`, `/agentic-workflow`, `/flight-debrief`, `/mission-debrief`, `/daily-briefing`, and `/init-project`.

> This setting can only be changed when the container is stopped. Toggling it triggers a container recreation on the next start.

### Full Permissions

Toggle **Full Permissions** to allow Claude Code to run with `--dangerously-skip-permissions` inside the container. This is **off by default**.

When **enabled**, Claude auto-approves all tool calls (file edits, shell commands, etc.) without prompting you. This is the fastest workflow since you won't be interrupted for approvals, and the Docker container provides isolation.

When **disabled** (default), Claude prompts you for approval before executing each action, giving you fine-grained control over what it does.

> **CAUTION:** Enabling full permissions means Claude can execute any command inside the container without asking. While the container sandbox limits the blast radius, make sure you understand the implications — especially if the container has Docker socket access or network connectivity.

> This setting can only be changed when the container is stopped. It takes effect the next time you open a terminal session.

### Environment Variables

Click **Edit** to open the environment variables modal. Add key-value pairs that will be injected into the container. Per-project variables override global variables with the same key.

> Reserved prefixes (`ANTHROPIC_`, `AWS_`, `GIT_`, `HOST_`, `CLAUDE_`, `TRIPLE_C_`) are filtered out to prevent conflicts with internal variables.

### Port Mappings

Click **Edit** to map host ports to container ports. This is useful when Claude Code starts a web server or other service inside the container and you want to access it from your host browser.

Each mapping specifies:
- **Host Port** — The port on your machine (1-65535)
- **Container Port** — The port inside the container (1-65535)
- **Protocol** — TCP (default) or UDP

### Claude Instructions

Click **Edit** to write per-project instructions for Claude Code. These are written to `~/.claude/CLAUDE.md` inside the container and provide project-specific context. If you also have global instructions (in Settings), the global instructions come first, followed by the per-project instructions.

---

## MCP Servers (Beta)

Triple-C supports [Model Context Protocol (MCP)](https://modelcontextprotocol.io/) servers, which extend Claude Code with access to external tools and data sources. MCP servers are configured in a **global library** and **enabled per-project**.

### How It Works

There are two dimensions to MCP server configuration:

| | **Manual** (no Docker image) | **Docker** (Docker image specified) |
|---|---|---|
| **Stdio** | Command runs inside the project container | Command runs in a separate MCP container via `docker exec` |
| **HTTP** | Connects to a URL you provide | Runs in a separate container, reached by hostname on a shared Docker network |

**Docker images are pulled automatically** if not already present when the project starts.

### Accessing MCP Configuration

Click the **MCP** tab in the sidebar to open the MCP server library. This is where you define all available MCP servers.

### Adding an MCP Server

1. Type a name in the input field and click **Add**.
2. Expand the server card and configure it.

The key decision is whether to set a **Docker Image**:
- **With Docker image** — The MCP server runs in its own isolated container. Best for servers that need specific dependencies or system-level packages.
- **Without Docker image** (manual) — The command runs directly inside your project container. Best for lightweight npx-based servers that just need Node.js.

Then choose the **Transport Type**:
- **Stdio** — The MCP server communicates over stdin/stdout. This is the most common type.
- **HTTP** — The MCP server exposes an HTTP endpoint (streamable HTTP transport).

### Configuration Examples

#### Example 1: Filesystem Server (Stdio, Manual)

A simple npx-based server that runs inside the project container. No Docker image needed since Node.js is already installed.

| Field | Value |
|-------|-------|
| **Docker Image** | *(empty)* |
| **Transport** | Stdio |
| **Command** | `npx` |
| **Arguments** | `-y @modelcontextprotocol/server-filesystem /workspace` |

This gives Claude Code access to browse and read files via MCP. The command runs directly inside the project container using the pre-installed Node.js.

#### Example 2: GitHub Server (Stdio, Manual)

Another npx-based server, with an environment variable for authentication.

| Field | Value |
|-------|-------|
| **Docker Image** | *(empty)* |
| **Transport** | Stdio |
| **Command** | `npx` |
| **Arguments** | `-y @modelcontextprotocol/server-github` |
| **Environment Variables** | `GITHUB_PERSONAL_ACCESS_TOKEN` = `ghp_your_token` |

#### Example 3: Custom MCP Server (HTTP, Docker)

An MCP server packaged as a Docker image that exposes an HTTP endpoint.

| Field | Value |
|-------|-------|
| **Docker Image** | `myregistry/my-mcp-server:latest` |
| **Transport** | HTTP |
| **Container Port** | `8080` |
| **Environment Variables** | `API_KEY` = `your_key` |

Triple-C will:
1. Pull the image automatically if not present
2. Start the container on the project's bridge network
3. Configure Claude Code to reach it at `http://triple-c-mcp-{id}:8080/mcp`

The hostname is the MCP container's name on the Docker network — **not** `localhost`.

#### Example 4: Database Server (Stdio, Docker)

An MCP server that needs its own runtime environment, communicating over stdio.

| Field | Value |
|-------|-------|
| **Docker Image** | `mcp/postgres-server:latest` |
| **Transport** | Stdio |
| **Command** | `node` |
| **Arguments** | `dist/index.js` |
| **Environment Variables** | `DATABASE_URL` = `postgresql://user:pass@host:5432/db` |

Triple-C will:
1. Pull the image and start it on the project network
2. Configure Claude Code to communicate via `docker exec -i triple-c-mcp-{id} node dist/index.js`
3. Automatically enable Docker socket access on the project container (required for `docker exec`)

### Enabling MCP Servers Per-Project

In a project's configuration panel (click **Config**), the **MCP Servers** section shows checkboxes for all globally defined servers. Toggle each server on or off for that project. Changes take effect on the next container start.

### How Docker-Based MCP Works

When a project with Docker-based MCP servers starts:

1. Missing Docker images are **automatically pulled** (progress shown in the progress modal)
2. A dedicated **bridge network** is created for the project (`triple-c-net-{projectId}`)
3. Each enabled Docker MCP server gets its own container on that network
4. The main project container is connected to the same network
5. MCP server configuration is written to `~/.claude.json` inside the container

**Networking**: Docker-based MCP containers are reached by their container name as a hostname (e.g., `triple-c-mcp-{serverId}`), not by `localhost`. Docker DNS resolves these names automatically on the shared bridge network.

**Stdio + Docker**: The project container uses `docker exec` to communicate with the MCP container over stdin/stdout. This automatically enables Docker socket access on the project container.

**HTTP + Docker**: The project container connects to the MCP container's HTTP endpoint using the container hostname and port (e.g., `http://triple-c-mcp-{serverId}:3000/mcp`).

**Manual (no Docker image)**: Stdio commands run directly inside the project container. HTTP URLs connect to wherever you point them (could be an external service or something running on the host).

### Configuration Change Detection

MCP server configuration is tracked via SHA-256 fingerprints stored as Docker labels. If you add, remove, or modify MCP servers for a project, the container is automatically recreated on the next start to apply the new configuration. The container filesystem is snapshotted first, so installed packages are preserved.

---

## AWS Bedrock Configuration

To use Claude via AWS Bedrock instead of Anthropic's API, switch the backend to **Bedrock** on the project card.

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

## Ollama Configuration

To use Claude Code with a local or remote Ollama server, switch the backend to **Ollama** on the project card.

### Settings

- **Base URL** — The URL of your Ollama server. Defaults to `http://host.docker.internal:11434`, which reaches a locally running Ollama instance from inside the container. For a remote server, use its IP or hostname (e.g., `http://192.168.1.100:11434`).
- **Model ID** — **Required.** The model to use (e.g., `qwen3.5:27b`). The model must be pulled in Ollama before use — run `ollama pull <model>` or use it via Ollama cloud so it is available when the container starts.

### How It Works

Triple-C sets `ANTHROPIC_BASE_URL` to point Claude Code at your Ollama server instead of Anthropic's API. The `ANTHROPIC_AUTH_TOKEN` is set to `ollama` (required by Claude Code but not used for actual authentication).

> **Note:** Ollama support is best-effort. Claude Code is designed for Anthropic models, so some features (tool use, extended thinking, prompt caching, etc.) may not work as expected with non-Anthropic models.

> **Important:** The model must already be available in Ollama before starting the container. If using a local Ollama instance, pull the model first with `ollama pull <model-name>`. If using Ollama's cloud service, ensure the model has been used at least once so it is cached.

---

## OpenAI Compatible Configuration

To use Claude Code through any OpenAI API-compatible endpoint, switch the backend to **OpenAI Compatible** on the project card. This works with any server that exposes an OpenAI-compatible API, including LiteLLM, OpenRouter, vLLM, text-generation-inference, LocalAI, and others.

### Settings

- **Base URL** — The URL of your OpenAI-compatible endpoint. Defaults to `http://host.docker.internal:4000` as an example (adjust to match your server's address and port).
- **API Key** — Optional. The API key for your endpoint, if authentication is required. Stored securely in your OS keychain.
- **Model ID** — Optional. Override the model to use.

### How It Works

Triple-C sets `ANTHROPIC_BASE_URL` to point Claude Code at your OpenAI-compatible endpoint. If an API key is provided, it is set as `ANTHROPIC_AUTH_TOKEN`.

> **Note:** OpenAI Compatible support is best-effort. Claude Code is designed for Anthropic models, so some features (tool use, extended thinking, prompt caching, etc.) may not work as expected when routing to non-Anthropic models through the endpoint.

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

You can open multiple terminal sessions (even for the same project). Each session gets its own tab in the top bar. Click a tab to switch, or click the **x** on a tab to close it. Tabs show the project name, with a "(bash)" suffix for shell sessions.

### Bash Shell Sessions

In addition to Claude Code terminals, you can open a plain **bash login shell** in any running container by clicking the **Shell** button. This is useful for manual inspection, package installation, debugging, or running commands that don't need Claude Code.

### URL Detection

When Claude Code prints a long URL (e.g., during `claude login`), Triple-C detects it and shows a toast notification at the top of the terminal with an **Open** button. Clicking it opens the URL in your default browser. The toast auto-dismisses after 30 seconds.

Shorter URLs in terminal output are also clickable directly.

### Copying and Pasting

Use **Ctrl+Shift+C** (or **Cmd+C** on macOS) to copy selected text from the terminal, and **Ctrl+Shift+V** (or **Cmd+V** on macOS) to paste. This follows standard terminal emulator conventions since Ctrl+C is reserved for sending SIGINT.

### Clipboard Support (OSC 52)

Programs inside the container can copy text to your host clipboard. When a container program uses `xclip`, `xsel`, or `pbcopy`, the text is transparently forwarded to your host clipboard via OSC 52 escape sequences. No additional configuration is required — this works out of the box.

### Image Paste

You can paste images from your clipboard into the terminal (Ctrl+V / Cmd+V). The image is uploaded to the container as `/tmp/clipboard_<timestamp>.png` and the file path is injected into the terminal input so Claude Code can reference it. A toast notification confirms the upload.

### Jump to Current

When you scroll up in the terminal to review previous output, a **Jump to Current** button appears in the bottom-right corner. Click it to scroll back to the latest output.

### File Manager

Click the **Files** button on a running project to open the file manager modal. You can:

- **Browse** the container filesystem starting from `/workspace`, with breadcrumb navigation
- **Download** any file to your host machine via the download button on each file entry
- **Upload** files from your host into the current container directory
- **Refresh** the directory listing at any time

The file manager shows file names, sizes, and modification dates.

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

The container also includes **clipboard shims** (`xclip`, `xsel`, `pbcopy`) that forward copy operations to the host via OSC 52, and an **audio shim** (`rec`, `arecord`) for future voice mode support.

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
- Look at the error message displayed in the progress modal.

### OAuth Login URL Not Opening

- Triple-C detects long URLs printed by `claude login` and shows a toast with an **Open** button.
- If the toast doesn't appear, try scrolling up in the terminal — the URL may have already been printed.
- You can also manually copy the URL from the terminal output and paste it into your browser.

### File Permission Issues

- Triple-C automatically remaps the container user's UID/GID to match your host user, so files created inside the container should have the correct ownership on your host.
- If you see permission errors, try resetting the container (stop, then click **Reset**).

### Settings Won't Save

- Most project settings can only be changed when the container is **stopped**. Stop the container first, make your changes, then start it again.
- Some changes (like toggling Docker access, Mission Control, or changing mounted folders) trigger an automatic container recreation on the next start.

### MCP Containers Not Starting

- Ensure the Docker image for the MCP server exists (pull it first if needed).
- Check that Docker socket access is available (stdio + Docker MCP servers auto-enable this).
- Try resetting the project container to force a clean recreation.

### "Failed to install Anthropic marketplace" Error

If Claude Code shows **"Failed to install Anthropic marketplace - Will retry on next startup"** repeatedly, the marketplace metadata in `~/.claude.json` may be corrupted. To fix this, open a **Shell** session in the project and run:

```bash
cp ~/.claude.json ~/.claude.json.bak && jq 'with_entries(select(.key | startswith("officialMarketplace") | not))' ~/.claude.json.bak > ~/.claude.json
```

This backs up your config and removes the corrupted marketplace entries. Claude Code will re-download them cleanly on the next startup.
