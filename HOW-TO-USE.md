# How to Use Triple-C

Triple-C (Claude-Code-Container) is a desktop application that runs Claude Code inside isolated Docker containers. Each project gets its own sandboxed environment with bind-mounted directories, so Claude only has access to the files you explicitly provide.

---

## Table of Contents

- [Prerequisites](#prerequisites)
- [First Launch](#first-launch)
- [The Interface](#the-interface)
- [Project Home](#project-home)
- [Project Management](#project-management)
- [Permission Modes](#permission-modes)
- [Project Configuration](#project-configuration)
- [Shared Claude Authentication](#shared-claude-authentication)
- [Opening URLs in Your Browser (URL Relay)](#opening-urls-in-your-browser-url-relay)
- [Browser Logins Inside the Container (Auth Bridge)](#browser-logins-inside-the-container-auth-bridge)
- [AWS Bedrock Configuration](#aws-bedrock-configuration)
- [Ollama Configuration](#ollama-configuration)
- [llama.cpp Configuration](#llamacpp-configuration)
- [OpenAI Compatible Configuration](#openai-compatible-configuration)
- [Model Aliases and Background Calls](#model-aliases-and-background-calls)
- [Settings](#settings)
- [Web Terminal (Remote Access)](#web-terminal-remote-access)
- [Terminal Features](#terminal-features)
- [Automation & Scheduled Tasks](#automation--scheduled-tasks)
- [Keyboard Shortcuts](#keyboard-shortcuts)
- [What's Inside the Container](#whats-inside-the-container)
- [Claude Code Tips](#claude-code-tips)
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
- **Ollama** — A local or remote Ollama server (best-effort support)
- **llama.cpp** — A local or remote `llama-server` (best-effort support)
- **OpenAI Compatible** — A gateway that implements the **Anthropic Messages API**, such as LiteLLM (best-effort support). A server that only speaks OpenAI's `/v1/chat/completions` will not work — see [OpenAI Compatible Configuration](#openai-compatible-configuration).

---

## First Launch

### 1. Get the Container Image

When you first open Triple-C, go to the **Settings** tab in the sidebar. Under **Docker**, you'll see:

- **Docker Status** — Should show "Connected" (green). If it shows "Not Available", make sure Docker is running.
- **Image Status** — Will show "Not Found" on first launch.

Choose an **Image Source**:

| Source | Description | When to Use |
|--------|-------------|-------------|
| **Registry** | Pulls the pre-built image from `ghcr.io` | Fastest setup — recommended for most users |
| **Local Build** | Builds the image locally from the embedded Dockerfile | If you can't reach the registry, or want a custom build |
| **Custom** | Use any Docker image you specify | Advanced — bring your own sandbox image |

Click **Pull Image** (for Registry/Custom) or **Build Image** (for Local Build). A progress log will stream below the button. When complete, the status changes to "Ready" (green).

### 2. Create Your First Project

Switch to the **Projects** tab in the sidebar and click **+ Add**.

1. **Project Name** — Give it a meaningful name (e.g., "my-web-app").
2. **Folders** — Click **Browse** to select a directory on your host machine. This directory will be mounted into the container at `/workspace/<folder-name>`. You can add multiple folders with the **+** button at the bottom of the folder list.
3. Click **Add Project**.

### 3. Start the Container

Click the project in the sidebar. Its **Project Home** opens as a tab in the main area. Click
**Start** in the Project Home header (or use the play control that appears when you hover the
sidebar row).

Progress is reported inline — the sidebar row and the Project Home header show messages like
"Creating container…" and "Starting container…" while the status moves from Stopped (`○`) to
Starting (`◐`) to Running (`●`). Nothing blocks the rest of the app; if something fails you get a
toast with the full detail behind a **Details** disclosure.

### 4. Open a Terminal

Click **Open Claude Terminal** in the Project Home header, or press **Ctrl+T**. A new tab appears
in the main tab strip and an xterm.js terminal loads.

Claude Code launches automatically. The project's **permission mode** decides how much it asks
before acting — the default is to prompt before each tool call. See
[Permission Modes](#permission-modes).

### 5. Authenticate

**Anthropic — shared token (recommended):**

Run `claude setup-token` once from **Settings → Claude Authentication** in the sidebar, and every
Anthropic-backend project uses that token without its own login. See
[Shared Claude Authentication](#shared-claude-authentication).

**Anthropic — per-container OAuth:**

1. Type `claude login` or `/login` in the terminal.
2. Claude prints an OAuth URL. Triple-C detects long URLs and shows a clickable toast at the top of the terminal — click **Open** to open it in your browser.
3. Complete the login in your browser. The token is saved and persists across container stops, starts and recreations. A **Reset** deletes it — see below.

> If the login hangs after the browser step, the callback could not reach the container. Either
> click **In container** on the toast instead of **Open** — the callback then never has to leave the
> container at all — or turn on the
> [Auth Bridge](#browser-logins-inside-the-container-auth-bridge) in the project's
> **Config → Runtime** section.

**AWS Bedrock:**

1. Stop the container first (most settings can only be changed while stopped).
2. Open the project's **Config** tab and, under **Model**, set **Backend** to **Bedrock**.
3. Fill in your AWS credentials in the same section (see [AWS Bedrock Configuration](#aws-bedrock-configuration) below).
4. Start the container again.

**Ollama:**

1. Stop the container first (most settings can only be changed while stopped).
2. Open the project's **Config** tab and, under **Model**, set **Backend** to **Ollama**.
3. Set the base URL of your Ollama server (defaults to `http://host.docker.internal:11434` for a local instance). Set the **Model** to the model you want to use (required).
4. Make sure the model has been pulled in Ollama (e.g., `ollama pull qwen3.5:27b`) or used via Ollama cloud before starting.
5. Start the container again.

**llama.cpp:**

1. Stop the container first (most settings can only be changed while stopped).
2. Open the project's **Config** tab and, under **Model**, set **Backend** to **llama.cpp**.
3. Set the base URL of your `llama-server` (defaults to `http://host.docker.internal:8080`, `llama-server`'s default port). Set the **Model** to the model it is serving.
4. Start the container again.

**OpenAI Compatible:**

1. Stop the container first (most settings can only be changed while stopped).
2. Open the project's **Config** tab and, under **Model**, set **Backend** to **OpenAI Compatible**.
3. Set the base URL of your gateway (defaults to `http://host.docker.internal:4000`, LiteLLM's default port). Optionally set an API key and model.
4. Start the container again.

---

## The Interface

```
┌──────────────────────────────────────────────────────────────────────┐
│ [⌂ my-app] [▣ my-app ask] [▣ my-app (bash)]   Docker ● Image ● ?     │
├─────────────┬────────────────────────────────────────────────────────┤
│  Sidebar    │  ┌──────────────────────────────────────────────────┐  │
│             │  │ my-app   ● Running · up 2h 5m                    │  │
│  Projects   │  │  [Open Claude Terminal] [Shell] [Files]          │  │
│  Settings   │  │  [Stop] [⋯]                                      │  │
│             │  ├──────────────────────────────────────────────────┤  │
│  ● my-app   │  │ Overview · Sessions · Automation · Config · Files│  │
│  ○ other    │  ├──────────────────────────────────────────────────┤  │
│             │  │                                                  │  │
│             │  │        (Project Home, or a terminal view)        │  │
│             │  │                                                  │  │
│             │  └──────────────────────────────────────────────────┘  │
├─────────────┴────────────────────────────────────────────────────────┤
│  2 project(s) · 1 running · 2 terminal(s)          Jump to Current ↓ │
└──────────────────────────────────────────────────────────────────────┘
```

- **Tab strip (top)** — One strip holds every open tab, in the order you opened them. There are two
  kinds: **Project Home** tabs (`⌂` glyph, project name, status glyph) and **terminal** tabs (`▣`
  glyph, plus a small badge showing the permission mode the terminal was launched with —
  `plan`, `ask`, `edits` or `bypass`). Bash shell tabs show a "(bash)" suffix. Right-click a
  terminal tab to rename it, jump to its project home, or close it; double-click to rename inline.
  There is no separate terminal tab bar and no "+" button — tabs appear when you open a project or
  a terminal.

  **Drag a tab to reorder it.** A line shows where it will land; **Escape** abandons the drag.
  Dropping does not change which tab you are looking at — so you can rearrange the strip without
  pulling focus away from a terminal that is mid-run. `Ctrl+Shift+←` and `Ctrl+Shift+→` move the
  *active* tab the same way without the mouse (they leave text fields alone, where that chord
  still selects by word). The order is per-session: it is not saved when you quit.
- **Status indicators (top right)** — Docker connection and container image availability. Each pairs
  a coloured dot with a word, so status is never conveyed by colour alone. The **?** button opens
  the built-in help.
- **Sidebar** — Toggle between the **Projects** list and the **Settings** panel. It collapses to a
  narrow icon rail with the chevron button, and remembers that choice.
- **Main area** — Shows the active tab: a Project Home view or an xterm.js terminal. With no tabs
  open you get a welcome screen with Docker/image/project readiness checks.
- **StatusBar** — Counts of total projects, running containers and open terminal sessions; the
  **Jump to Current ↓** button when a terminal is scrolled up; and the microphone button when
  speech-to-text is enabled.

---

## Project Home

Clicking a project in the sidebar opens **Project Home** in the main area. The sidebar row is only
for selecting a project and for two quick controls that appear on hover — start/stop, and open a
Claude terminal. Everything else about a project lives in Project Home.

The header shows the project name, its status, how long the container has been up, and the action
buttons. Below that are six tabs:

| Tab | What it's for |
|---|---|
| **Overview** | The permission mode control, a summary of the backend and sandbox settings, capability tiles, recent sessions and scheduled tasks |
| **Sessions** | Past Claude Code conversations stored on this project's config volume, each with a **Resume** button |
| **Automation** | The scheduled tasks running inside this container — see [Automation & Scheduled Tasks](#automation--scheduled-tasks) |
| **Config** | All per-project configuration — see [Project Configuration](#project-configuration) |
| **Files** | Browse, download and upload files inside the container |
| **Browser** | Watch — and take over — the browser Claude is driving with Playwright, see [The Browser Tab](#the-browser-tab) |

### Sessions

Claude Code records each conversation on the project's config volume. The **Sessions** tab lists
them with a name or summary, the session id, its working directory, its age, size and message
count. **Refresh** re-reads the list.

**Resume** opens a new shell tab and runs `claude --resume <session-id>` for you, with the
project's current permission-mode flags applied. The Overview tab shows the four most recent
sessions with the same Resume action.

Sessions can only be read while the container is running, and they are stored on the config volume
— so a **Reset** deletes them.

### Capability Tiles

The Overview tab shows read-only counts of what Claude Code has available **inside this container**:

| Tile | What is counted |
|---|---|
| **Skills** | Directories under `.claude/skills/` that contain a `SKILL.md` |
| **Agents** | `.md` files under `.claude/agents/` |
| **Commands** | `.md` files under `.claude/commands/` |
| **Hooks** | Hook handlers configured in `.claude/settings.json` / `settings.local.json` |
| **Plugins** | Installed and enabled Claude Code plugins |
| **MCP servers** | Servers in `~/.claude.json` and in any `.mcp.json` under `/workspace` |

Both user scope (`/home/claude/.claude`) and project scope (`/workspace/<folder>/.claude`) are
included, and each tile opens a list of what it found.

> **Triple-C does not edit any of this.** Claude Code owns skills, agents, commands, hooks, plugins
> and MCP servers, and it has good built-in tooling for them. The tiles are a window, not an editor:
> **Manage in terminal** opens a terminal in the container so you can use `/agents`, `/hooks`,
> `/plugins`, `/mcp` and friends directly.

The counts are only available while the container is running.

### The Browser Tab

When Claude drives a browser with Playwright inside the container, the **Browser** tab shows you
that browser live — and lets you take it over with your own mouse and keyboard.

It is **off by default and opted into per project**, and it never installs anything on its own.
Opening the tab only *probes* the container, so it can tell you what is missing before you ask for
a view; installing Playwright and downloading a browser are separate, labelled buttons that state
what they cost before you press them. See
[What's Inside the Container](#whats-inside-the-container) for why the browser itself is not
pre-installed.

Press **Start browser view** and the pane fills with Playwright's own dashboard, running inside the
container and reached over a token-gated listener on your machine's loopback address. Nothing is
exposed off the machine.

#### Opening a page yourself

**Open a page…** launches a browser inside the container at a URL and viewport you choose, and
publishes it to this pane. Two uses:

- **A sign-in page.** The callback the tool is waiting for is a listener *inside* the container, so
  a container-side browser completes the login without anything crossing to your host browser.
  When a long URL appears in a terminal, the prompt that offers to open it on your host now also
  offers **In container**, which does the same thing in one click.
- **A dev server.** `http://localhost:5173` inside the container is reachable with no port mapping
  and nothing exposed to your network — which is how you watch a UI Claude is building, and click
  around it yourself.

The **viewport** is the page's own resolution, and it is not the same thing as the window size.
The pane shows a video of the browser, so a bigger window draws the same pixels larger; changing
the viewport is what makes the layout actually reflow. Pick a preset or type a size.

Note the limit, because it is not obvious: a browser Claude opened through `@playwright/mcp` can
be *watched* but not resized — a published browser admits only the client that launched it. Set
its size with `PLAYWRIGHT_MCP_VIEWPORT_SIZE=1920x1080` in the project's environment variables
instead.

#### Watching it while you work

Press **Open in own window** and the view moves out of the tab into a window of its own — put it on
a second monitor, or turn on **Keep on top** and let it float above the app while you work in a
terminal. **Match window** goes further: the page's viewport follows the window as you drag it, so
the pop-out becomes a responsive-design ruler. It applies to pages opened with **Open a page…**,
for the reason above. This is a window change only: the browser and the view keep running throughout, so
popping out and back costs nothing and interrupts nothing.

While the view is in its own window the tab shows a placeholder rather than a second copy of it —
two viewers would both be able to *drive* the browser, and two cursors on one page is not useful.
**Put back in tab**, or just closing the window, brings it back.

The window belongs to the view, not to the tab: closing the project's home tab leaves it open, and
stopping the view — by pressing **Stop**, stopping the container, or removing the project — closes
it, because a window showing a viewer that no longer exists is worse than no window.

---

## Project Management

### Project Status

Each project shows a status glyph paired with a word, so it is readable without relying on colour:

| Glyph | Status | Meaning |
|-------|--------|---------|
| `○` | Stopped | Container is not running |
| `◐` | Starting / Stopping | Container is transitioning (the glyph pulses) |
| `●` | Running | Container is active, ready for terminals |
| `▲` | Error | Something went wrong (check the toast for detail) |

While a container is starting or stopping, the status line is replaced by the live progress message.

### Project Actions

Most actions live in the **Project Home header**; two live behind the **⋯** overflow menu next to
it. The sidebar row carries only the two hover controls.

| Action | Where | When Available | What It Does |
|--------|-------|---------------|--------------|
| **Start** | Project Home header; sidebar hover control | Stopped | Creates (if needed) and starts the container |
| **Stop** | Project Home header; sidebar hover control | Running | Stops the container but preserves its state |
| **Force stop** | Project Home header | Starting / Stopping | Interrupts a transition that is stuck |
| **Open Claude Terminal** | Project Home header; sidebar hover control; `Ctrl+T` | Running | Opens a new Claude Code terminal tab |
| **Shell** | Project Home header | Running | Opens a bash login shell tab in the container (no Claude Code) |
| **Files** | Project Home header, and the **Files** tab | Running | Switches to the Files tab to browse, download and upload files |
| **Config** | The **Config** tab | Always | Per-project configuration (most fields need the container stopped) |
| **Back up container** | **⋯** overflow menu | A container exists | Saves a `.tar.gz` archive of the container to a location you choose |
| **Reset container…** | **⋯** overflow menu | Stopped or Error | Destroys the container, snapshot image and both volumes, then recreates from the base image (wipes `~/.claude`) — asks first |
| **Remove project…** | **⋯** overflow menu | Always | Deletes the project, its container, its volumes and its stored credentials — asks first |

> Both destructive actions confirm before acting, and the Reset dialog spells out what you
> lose: your `claude login`, anything installed inside the container, and every saved
> session transcript. Your mounted project folders live on the host and are not touched.

> The backup archive includes the Claude config volume, which may contain API keys. Keep it private.

### Renaming a Project

Rename a project in its **Config** tab, under **Workspace → Project name**. Press **Enter** to save
and leave the field, or **Escape** to revert. (Double-clicking a *terminal tab* renames that tab —
that is a different thing.)

### Container Lifecycle

Containers use a **stop/start** model. When you stop a container, everything inside it is preserved — installed packages, modified files, downloaded tools. Starting it again resumes where you left off.

**Reset container** removes the container, its snapshot image **and both of its named volumes**
(`triple-c-home-<project-id>` and `triple-c-claude-config-<project-id>`), then creates a fresh one
from the clean base image. This is destructive: `~/.claude` and `~/.claude.json` go with the
volumes, so your per-container OAuth login, any skills or agents you installed, your session
transcripts and your scheduled tasks are all lost.

What Reset keeps: your host folders (they are bind mounts and are never touched), the project's
configuration in Triple-C, and anything stored in your OS keychain — including the shared Claude
authentication token. If the project uses that shared token, it re-authenticates by itself after a
Reset; if it relies on `claude login`, you will need to log in again.

Apart from **Remove project…**, Reset is the only action that deletes the volumes. Stopping and
starting preserves them, and so does the automatic container recreation that happens when you
change a setting that affects the container — in both cases your Claude Code configuration
survives.

**Remove project…** deletes everything Reset does, plus the project record itself and its stored
credentials.

### Container Progress Feedback

When starting, stopping, or resetting a container, progress is shown inline on the project row and in the Project Home header (e.g., "Creating container...", "Starting container..."), so the rest of the app stays usable. If an error occurs it is raised as a toast with the full detail behind a **Details** disclosure. There is no blocking progress modal.

---

## Permission Modes

Every project has a **permission mode** that decides how much Claude Code does without asking. It
is a segmented control on the **Overview** tab (and again under **Config → Runtime**), and it
replaces the old Full Permissions on/off switch.

| Mode | What Claude does | What Triple-C passes to `claude` |
|------|------------------|----------------------------------|
| **Plan** | Proposes a plan and makes no changes | `--permission-mode plan` |
| **Default** | Asks before each tool call | *(nothing — Claude Code's own default)* |
| **Accept Edits** | Auto-approves file edits; other tools still prompt | `--permission-mode acceptEdits` |
| **Bypass** | Auto-approves every tool call | `--dangerously-skip-permissions` |

New projects start in **Default**. Projects created before permission modes existed keep behaving
the way they did: one that had Full Permissions on becomes **Bypass**, one that had it off becomes
**Default**.

> **CAUTION:** In **Bypass**, Claude can execute any command inside the container without asking.
> The container sandbox limits the blast radius, but think carefully — especially if the container
> has Docker socket access or reaches services on your network. The Overview tab tells you whether
> the in-container sandbox is also on.

### When a change takes effect

- **Terminals** — the mode is applied when a terminal is opened, so it affects terminals you open
  from then on. A Claude session that is already running keeps the permissions it started with;
  close the tab and open a new terminal to change it. The badge on each terminal tab shows the mode
  that terminal was launched with (`plan`, `ask`, `edits`, `bypass`).
- **Resumed sessions** — a session resumed from the **Sessions** tab uses the project's current
  mode.
- **Scheduled tasks** — these now honour the permission mode too (they previously always ran with
  `--dangerously-skip-permissions`). The mode reaches them through the container's environment,
  which can only change when the container is recreated, so **stop and start the project** for a
  mode change to reach the scheduler.

> Scheduled tasks run headless (`claude -p`) and cannot answer a permission prompt. In any mode
> other than **Bypass**, a task may simply stop early when Claude Code asks for approval. Its run
> log records which mode it used.

---

## Project Configuration

Open a project's **Config** tab in Project Home. Configuration is grouped into four sections —
**Workspace**, **Model**, **Access** and **Runtime** — plus **Claude instructions** and **Claude
Code settings**.

Changes save automatically when a field loses focus, and a Saved / Saving… / Failed indicator in
the corner tells you what happened. Most settings can only be changed when the container is
**stopped**; a warning chip appears at the top of the tab if it is running. (The project name and
the permission mode can be changed at any time.)

### Mounted Folders

Each project mounts one or more host directories into the container. The mount appears at `/workspace/<mount-name>` inside the container.

- Click **Browse** to change the host path
- Edit the mount name to control where it appears inside `/workspace/`
- Click **+ Add folder** to add more, or **Remove** to drop one (the last remaining folder cannot be removed)
- Mount names must be unique and use only letters, numbers, dashes, underscores, and dots

### SSH Keys

Specify the path to your SSH key directory (typically `~/.ssh`). Keys are mounted read-only and copied into the container with correct permissions. This enables `git clone` via SSH inside the container.

### Git Configuration

- **Git Name / Email** — Sets `git config user.name` and `user.email` inside the container.
- **Git HTTPS Token** — A personal access token (e.g., from GitHub) for HTTPS git operations. Stored securely in your OS keychain — never written to disk in plaintext.

### Allow Container Spawning

When enabled, the host Docker socket is mounted into the container so Claude Code can create sibling containers (e.g., for running databases, test environments). This is **off by default** for security.

> Toggling this requires stopping and restarting the container to take effect.

### VPN Support

When enabled, the container is given the three things a VPN client needs to build a tunnel:
the `NET_ADMIN` capability, the `/dev/net/tun` device, and the `net.ipv4.conf.all.src_valid_mark`
sysctl that WireGuard requires. This is **off by default**.

The `ip`, `wg` and `iptables` commands ship in the container image so there is something able to use
them. If your project's container was created from an older base image it will not have them, and
`wg` will simply not be found — **migrating the project onto the current base image** is what picks
them up. `sudo apt install iproute2 wireguard-tools iptables` works in the meantime, but lives in
the writable layer, so it is undone by a **Reset** and by a migration.

**This setting makes a tunnel possible; it does not make one.** Nothing is connected, no traffic is
redirected, and no tunnel is configured or started on your behalf. Enabling it and expecting the
container's traffic to start leaving through a VPN is the most common misreading of what it does —
configuring a tunnel and routing traffic into it remains yours to do.

To make that second half easier, enabling this also installs a **`pia-vpn` skill** into the
container's `~/.claude/skills/`, so Claude Code can bring up a Private Internet Access tunnel over
WireGuard for you — ask it to connect the VPN and it will. The skill carries the parts that are
easy to get wrong (see the DNS note below), and it is removed again when you turn the setting off.
It needs your PIA credentials in `~/pia-creds`, two lines, username then password. If you use a
different provider, ignore it and set up your own client; nothing else depends on it.

Like the VPN tooling above, the skill ships in the container image, so a project whose container
predates it will not get one by toggling the setting — **migrate the project** and it appears; the
migration pre-flight lists it among what you would gain.

With the setting **off**, a client such as PIA or OpenVPN installs and its daemon starts normally,
but the connection attempt **hangs until it times out** — a default container has no tun device to open
and no permission to add an interface or a route, and most clients report that as a generic timeout
rather than a permissions error.

Things worth knowing:

- Tailscale is the exception: in its `--tun=userspace-networking` mode it needs neither the
  capability nor the device, so leave this off if that is all you want.

- `NET_ADMIN` applies to the container's **own** network namespace — it cannot touch the host's
  interfaces. It is not nothing, though: within that namespace anything in the container can set
  promiscuous mode and add arbitrary addresses, routes and firewall rules on the Docker bridge it
  shares with your other containers, and it can flush firewall rules that sandbox mode relies on.
  Grant it per project, to projects that need it.
- The **Docker host's** kernel must have the `tun` module available. With Docker Desktop that is
  the Linux VM, not your own machine. If it is missing, the container is created but fails to
  **start**, with an error naming `/dev/net/tun` and pointing back at this setting.
- A VPN client's kill switch applies to everything in the container, Claude Code included. If the
  tunnel drops, expect API calls to fail until it reconnects or the kill switch is turned off.
- **No tunnel survives a restart.** The network namespace is built fresh every time the container
  starts, and there is no service manager inside to reconnect anything. Leftover state under `/run`
  makes it *look* like the tunnel is still configured — that directory is in the container's
  writable layer, so it is simply still there after a stop/start, and `docker commit` carries it
  into the snapshot that a recreation is built from. Either way the interface and its routes are
  gone and traffic goes out your real address again, with no error and nothing visibly different.
  Re-establish it after every start, and check rather than assume.
- **A full tunnel breaks DNS unless the client is told to leave private ranges alone.** Your
  resolver is whatever `/etc/resolv.conf` says, and if that address is outside the container's own
  subnet then a default route of `0.0.0.0/0` — or a `0.0.0.0/1` plus `128.0.0.0/1` pair — captures
  it and sends every lookup into a tunnel that cannot carry it. Under Docker Desktop it is
  `192.168.65.7`, which is exactly that case; on a user-defined Docker network it is `127.0.0.11`,
  which is loopback and unaffected. Check yours rather than assuming. The symptom when it bites is
  total: Claude Code reports it cannot connect, because it cannot resolve `api.anthropic.com`.
  Route `10.0.0.0/8`, `172.16.0.0/12`, `192.168.0.0/16` and `169.254.0.0/16` via the original
  gateway — and give the tunnel a resolver it can actually reach, normally the VPN provider's own,
  or you have a tunnel that leaks every DNS query outside itself. Also pin the VPN endpoint's own
  address via the original gateway, or the tunnel's encrypted packets try to route through the
  tunnel. Note that a health check which fetches an IP literal such as `1.1.1.1` passes cleanly
  while DNS is broken — resolve a name instead.
- **Delete a client's key material when you tear a tunnel down.** Anything written under `/run` is
  in the container's writable layer, and recreating or migrating the project runs `docker commit`
  over it — so a WireGuard private key left there gets baked into the project's snapshot image and
  copied forward from then on. This is not hypothetical; it has already happened here.
- **Strip the `DNS =` line from a provider's `.conf` before `wg-quick up`.** Every commercial
  provider ships one, and `wg-quick` hands it to `resolvconf`, which is not installed — so it fails
  at `resolvconf: command not found` and deletes the interface again. This happens before any
  routing, so it takes **split tunnels down too**. Set the resolver another way instead, or drive
  `wg` and `ip route` directly rather than going through `wg-quick`.
- **`wg-quick` full tunnels additionally need `xt_CONNMARK` from the host kernel.** WSL2 kernels
  before 6.6 do not have it and a container cannot load one — on Windows, `wsl --update` moves you
  to a current kernel, which does. Failing that, add the routes yourself with `ip route`, which
  needs no firewall backend on any platform. Note this is the *second* hurdle: clear the `DNS =`
  one above first, or you will not reach this.

> This setting can only be changed when the container is stopped. Capabilities and devices are
> fixed when a container is created, so toggling it recreates the container on the next start.
> Recreation preserves the home and `.claude` volumes — it is not a Reset.

### Mission Control

Toggle **Mission Control** to integrate Flight Control — an AI-first development methodology bundled with Triple-C — into the project. When enabled:

- The bundled Flight Control files are installed into the container
- Flight Control skills are installed to Claude Code's skill directory (`~/.claude/skills/`)
- Project instructions are appended with Flight Control workflow guidance
- The files are symlinked at `/workspace/mission-control`

Available skills include `/mission`, `/flight`, `/leg`, `/agentic-workflow`, `/flight-debrief`, `/mission-debrief`, `/daily-briefing`, and `/init-project`.

> This setting can only be changed when the container is stopped. Toggling it triggers a container recreation on the next start.

### Permission Mode

The **Runtime** section repeats the permission mode control from the Overview tab — see
[Permission Modes](#permission-modes) for what each mode does and when a change takes effect.

### Sandbox Mode

Toggles Claude Code's in-container bubblewrap isolation. The Overview tab shows the current state
next to the permission mode, because the two together decide how contained a Bypass-mode session
really is.

### Environment Variables

Add key-value pairs under **Access → Environment variables**; they are injected into the container.
Per-project variables override global variables with the same key.

> Reserved prefixes (`ANTHROPIC_`, `AWS_`, `GIT_`, `HOST_`, `TRIPLE_C_`) are filtered out to prevent
> conflicts, along with the exact names Triple-C manages itself: `CLAUDE_INSTRUCTIONS`,
> `CLAUDE_CODE_SETTINGS_JSON`, `CLAUDE_CODE_OAUTH_TOKEN`, `MISSION_CONTROL_ENABLED`,
> `TRIPLE_C_PERMISSION_MODE` and `MCP_SERVERS_JSON`. Other `CLAUDE_CODE_*` variables are allowed, so
> you can set Claude Code feature flags directly (e.g., `CLAUDE_CODE_DISABLE_TERMINAL_TITLE=1`).

### Port Mappings

Under **Access → Port mappings**, map host ports to container ports. This is useful when Claude Code starts a web server or other service inside the container and you want to access it from your host browser.

Each mapping specifies:
- **Host Port** — The port on your machine (1-65535)
- **Container Port** — The port inside the container (1-65535)
- **Protocol** — TCP (default) or UDP

### Claude Instructions

The **Claude instructions** editor at the bottom of the Config tab holds per-project instructions for Claude Code. These are written to `~/.claude/CLAUDE.md` inside the container and provide project-specific context. If you also have global instructions (in Settings), the global instructions come first, followed by the per-project instructions.

### Claude Code Settings

The **Claude Code settings** editor, also at the bottom of the Config tab, configures Claude Code CLI behavior for this project. These settings control how Claude Code operates inside the container:

| Setting | What It Does |
|---------|-------------|
| **TUI Mode** | Set to **Fullscreen** for flicker-free alt-screen rendering (uses `CLAUDE_CODE_NO_FLICKER=1`) |
| **Effort Level** | Controls reasoning depth: **Low** (fast, less thorough), **Medium**, **High** (deep reasoning) |
| **Focus Mode** | Collapses tool output to one-line summaries, showing only the prompt and final response |
| **Thinking Summaries** | Shows Claude's thinking process as summaries during responses |
| **Session Recap** | Provides context when returning to a session after being away |
| **Auto-Scroll Disabled** | Disables auto-scroll when in fullscreen TUI mode |
| **Env Scrub** | Strips credentials from subprocess environments for security |
| **Prompt Caching (1h)** | Enables 1-hour prompt cache TTL instead of the default 5 minutes |

Per-project settings override global defaults set in Settings. If all settings are at their defaults, no configuration is injected.

> These settings map to Claude Code environment variables and `~/.claude/settings.json` entries. Changes require stopping and restarting the container to take effect.

### MCP Servers

Triple-C no longer manages [MCP](https://modelcontextprotocol.io/) servers itself. Configure them with Claude Code's own tooling from a terminal inside the container:

- `claude mcp add` — register a server
- `claude mcp list` — show configured servers
- `claude mcp remove` — delete a server
- `/mcp` — slash command inside a Claude Code session for MCP status and authentication
- A project-level `.mcp.json` in `/workspace` — checked into your repo and shared with anyone who opens the project

Your MCP configuration persists across container stop/start because `~/.claude.json` and `~/.claude` live on named Docker volumes. A **Reset** wipes them, so you would need to re-add your servers afterwards.

---

## Shared Claude Authentication

Instead of running `claude login` separately in every container, you can authenticate once and
share the result across projects. Claude Code's `claude setup-token` mints a long-lived token
(roughly a year) that Triple-C stores in your **OS keychain** and injects into containers as
`CLAUDE_CODE_OAUTH_TOKEN`.

This lives in the sidebar under **Settings → Claude Authentication**.

### Signing in

1. Start at least one project whose container is running — the flow borrows that container as a
   place to run the CLI. The token it produces is global, not tied to that project.
2. Start the sign-in. Triple-C runs `claude setup-token` inside the container and streams its
   output.
3. Claude Code prints an authorization URL. Open it, sign in, and Anthropic's page gives you a
   code to copy — this flow finishes on an Anthropic-hosted page, not a local callback.
4. Paste the code back into Triple-C. The token is captured and written straight to the keychain.

The code is long and easy to truncate. If Anthropic refuses it, the dialog says so and lets you
paste another one without restarting the sign-in — the CLI is still waiting. After a few refusals
the flow gives up and reports it rather than sitting there.

Only one sign-in can run at a time, and the whole flow times out after 15 minutes. A long-lived
token requires a Claude subscription; without one, `setup-token` finishes without printing a token
and nothing is stored.

### How the token is used

- It is injected only into projects whose backend is **Anthropic** — it means nothing to Bedrock,
  Ollama, llama.cpp or an OpenAI-compatible gateway.
- Each project can opt out under **Config → Model** ("Use the shared Claude token"). Projects are
  opted **in** by default, so a single sign-in covers your whole fleet; opt a project out if you
  want it pinned to its own `claude login` identity.
- `CLAUDE_CODE_OAUTH_TOKEN` is reserved — you cannot set it yourself as a custom environment
  variable, because a hand-set value would silently outrank the stored token.
- A container picks the token up when it is **next started**: acquiring, re-acquiring, revoking or
  opting out changes an internal marker that triggers a container recreation on the next start.
  Restart your Anthropic-backend containers after signing in.

### Revoking

Revoking deletes the token from your keychain. Containers keep the value they were given until each
is next started, at which point the same recreation clears the variable.

> The token is never shown in the app, never written to a log, and never sent to the frontend.
> While `setup-token` is running, its output is filtered so anything resembling an `sk-ant-`
> secret is masked before it reaches the screen — including a secret split across two chunks of
> output.

---

## Opening URLs in Your Browser (URL Relay)

There is no browser inside the container and no screen to put one on. Any tool that tries to open
a web page therefore fails, usually with something unhelpful like *"Couldn't find a suitable web
browser!"*. The **URL relay** fixes that: when a command inside the container asks for a browser,
the URL is handed to **your** browser on the host.

Nothing is displayed or forwarded from the container — only the URL travels.

It is always on and needs no configuration.

### What you see

A small bar appears at the top of the terminal reading **"Container asked to open a URL"**, with
the URL and an **Open** button. Click **Open** and the page loads in your normal browser, signed
in as you. The prompt disappears on its own after 30 seconds if you ignore it.

Triple-C asks rather than opening pages by itself. The container is sandboxed code — some of it
written by Claude a minute ago — and silently making your logged-in browser visit a URL it chose
is not something to hand over automatically. One click keeps that decision yours.

### Which commands benefit

Anything that opens a browser to authenticate or to show you a page:

| Command | What it wanted a browser for |
|---|---|
| `gh auth login` | GitHub device / OAuth login |
| `aws sso login` | AWS IAM Identity Center login |
| `gcloud auth login` | Google Cloud login |
| `az login` | Azure login |
| `vercel login`, `netlify login`, `fly auth login`, `heroku login`, `wrangler login` | Vendor CLI logins |
| `npm login`, `supabase login`, `doctl auth init` | Token / device flows |
| `xdg-open <url>` in any script | Opening a page directly |
| `python3 -m webbrowser <url>` | Anything using Python's `webbrowser` module |

Under the hood the container provides a stand-in browser at `/usr/local/bin/triple-c-open`,
installed under all the names tools look for — `xdg-open`, `sensible-browser`, `www-browser`,
`x-www-browser`, `gnome-open`, `gvfs-open`, `kde-open`, `open` — and as the `$BROWSER`
environment variable, which most of the CLIs above consult first. You can also call
`triple-c-open <url>` yourself.

It works even when the command is run *by* Claude Code rather than typed by you: the relay talks
to the terminal directly, not through the command's output, so being nested inside a tool call
does not break it.

### When no terminal is attached

The relay rides on the terminal session. If nothing is attached to the container, there is nothing
to relay through:

- **Scheduled tasks** (Automation tab) run from cron with no terminal at all.
- A shell you opened with your own `docker exec`, outside Triple-C.

In those cases the relay does **not** hang or wait. It prints the URL in plain text and returns
immediately:

```
triple-c-open: no Triple-C terminal attached — cannot reach the host browser.
triple-c-open: open this URL manually:
https://github.com/login/device?user_code=WXYZ-1234
```

For a scheduled task that text lands in the task log (**Project Home → Automation → Logs**), so
you can still finish the login yourself afterwards. Practically speaking: don't expect an
unattended scheduled task to complete an interactive browser login. Authenticate once from a
terminal session — the credentials persist in the project's config volume — and let the scheduled
runs use them.

### Security

Requests coming out of the container are treated as untrusted input, because that is what they
are:

- **Only `http://` and `https://` are ever opened.** `file://`, `javascript:`, `data:` and every
  custom protocol handler your OS has registered are rejected outright. A container that could
  make the host open arbitrary URI schemes would have a way out of the sandbox.
- URLs with embedded credentials (`https://github.com@evil.example/`) are rejected — they
  misrepresent which site you are about to visit.
- Control characters, whitespace and oversized payloads are rejected before parsing, so the relay
  cannot be used to smuggle terminal escape sequences into the UI.
- The URL is shown to you in its normalized form: what the prompt displays is exactly what opens.
- Prompts are rate-limited (a handful per ten seconds, with repeats of the same URL collapsed), so
  a runaway loop in the container cannot bury the interface.

The relay only *asks*. Nothing opens without your click.

---

## Browser Logins Inside the Container (Auth Bridge)

Some CLIs log you in by opening a browser and waiting for the browser to call back to a temporary
web server they started on `localhost`. `claude login`, `aws sso login` and Concourse's
`fly login` all work this way. When the CLI runs inside a container, that `localhost` is the
*container's* — the browser on your host calls back into nothing and the login hangs forever.

The **Auth Bridge** fixes this. It is **opt-in per project** and **off by default**.

### Where the switch is

Project Home → **Config** → **Runtime** → **Auth bridge**.

Unlike the rest of that tab, it is **not** greyed out while the container is running — it is a
host-side feature that recreates nothing, and the moment you want it is usually the moment a login
is already hanging in a running container. Switch it on, then retry the login.

Beside the switch is its live state: **Off**, **Watching** (on, nothing to bridge yet — normal,
there is only something to bridge while a login is waiting), **Bridging *n* ports**, **IPv4 only**,
or **Port conflict** with the port and the reason. A conflict means the host port was already taken
and the callback will not arrive; free the port, or use **In container** instead.

### What it does

- Every couple of seconds it looks inside the container for programs listening on the container's
  loopback address, and binds **the same port number** on your host. That is the whole trick: the
  redirect URL the login provider was handed resolves correctly on both sides.
- Connections are carried into the container over the Docker API, which keeps working on Docker
  Desktop where container IP addresses are not reachable from the host.
- It follows whichever address family the container program actually used. This matters in
  practice: Node resolves `localhost` to IPv6 first on Linux, so `claude login` frequently listens
  on `::1` and nothing else.
- Ports you have already configured as port mappings are left alone. If a host port is already
  taken, the bridge reports a conflict and leaves it alone rather than fighting for it — it will
  retry on a later pass.
- The bridge is entirely host-side, so turning it on or off never recreates the container. It stops
  by itself when the container stops.

### Security

The host side binds **loopback only** — `127.0.0.1` and `[::1]`, never a wildcard address. Nothing
on your network can reach a bridged port. Within your own machine, though, a bridged port is
reachable by any local process for as long as the in-container listener exists, and the services
behind it are unauthenticated: they bound loopback precisely because they expected to be reachable
from nowhere else. Only container programs that bound loopback are bridged; anything listening on
all interfaces is deliberately ignored (publishing those is what port mappings are for).

Leave it off unless you need it, and it will not be running.

---

## AWS Bedrock Configuration

To use Claude via AWS Bedrock instead of Anthropic's API, set **Backend** to **Bedrock** under
**Config → Model**.

### Authentication Methods

| Method | Fields | Use Case |
|--------|--------|----------|
| **Static keys** | Access Key ID, Secret Access Key, Session Token (optional) | Direct credentials — simplest setup |
| **Named profile** | AWS Profile name | Uses `~/.aws/config` and `~/.aws/credentials` on the host |
| **Bearer token** | Bearer Token | Temporary bearer token authentication |

With **Named profile**, the SSO session is validated before Claude Code launches, so an expired
session is caught at the start of a terminal rather than mid-task.

### Additional Bedrock Settings

- **AWS Region** — Required. The region where your Bedrock models are deployed (e.g., `us-east-1`).
- **Model ID** — Optional. Override the default Claude model (e.g., `anthropic.claude-sonnet-4-20250514-v1:0`).
- **Service tier** — Optional. Selects a Bedrock service tier.

### Global AWS Defaults

In **Settings > AWS Configuration**, you can set defaults that apply to all Bedrock projects:

- **AWS Config Path** — Path to your `~/.aws` directory. Click **Detect** to auto-find it.
- **Default Profile** — Select from profiles found in your AWS config.
- **Default Region** — Fallback region for projects that don't specify one.

Per-project settings always override these global defaults.

---

## Ollama Configuration

To use Claude Code with a local or remote Ollama server, set **Backend** to **Ollama** under **Config → Model**.

### Settings

- **Base URL** — The URL of your Ollama server. Defaults to `http://host.docker.internal:11434`, which reaches a locally running Ollama instance from inside the container. For a remote server, use its IP or hostname (e.g., `http://192.168.1.100:11434`).
- **Model ID** — **Required.** The model to use (e.g., `qwen3.5:27b`). The model must be pulled in Ollama before use — run `ollama pull <model>` or use it via Ollama cloud so it is available when the container starts.
- **Background model** — Optional. See [Model Aliases and Background Calls](#model-aliases-and-background-calls). Leave blank to reuse the Model ID above.

Global defaults for all three live under **Settings → Backends → Ollama Configuration** and are used whenever the matching per-project field is blank.

### How It Works

Ollama natively implements the Anthropic Messages API at `POST /v1/messages`, which is the only thing Claude Code ever sends. Triple-C sets `ANTHROPIC_BASE_URL` to point Claude Code at your Ollama server instead of Anthropic's API. The `ANTHROPIC_AUTH_TOKEN` is set to `ollama` (required by Claude Code but not used for actual authentication). The `ANTHROPIC_DEFAULT_*_MODEL` aliases are pinned to your model — see [Model Aliases and Background Calls](#model-aliases-and-background-calls).

> **Note:** Ollama support is best-effort. Claude Code is designed for Anthropic models, so some features (tool use, extended thinking, prompt caching, etc.) may not work as expected with non-Anthropic models.

> **Important:** The model must already be available in Ollama before starting the container. If using a local Ollama instance, pull the model first with `ollama pull <model-name>`. If using Ollama's cloud service, ensure the model has been used at least once so it is cached.

---

## llama.cpp Configuration

To use Claude Code with a local or remote `llama-server` (from [llama.cpp](https://github.com/ggml-org/llama.cpp)), set **Backend** to **llama.cpp** under **Config → Model**.

`llama-server` implements the Anthropic Messages API natively — `POST /v1/messages` and `POST /v1/messages/count_tokens` — so Claude Code talks to it directly, with no translation layer in between.

### Settings

- **Base URL** — The URL of your `llama-server`. Defaults to `http://host.docker.internal:8080`; **8080** is `llama-server`'s own default port (`--port PORT | port to listen (default: 8080)`). For a remote server, use its IP or hostname.
- **Model ID** — The model `llama-server` is serving. A `llama-server` process serves one model, so this is mostly the id Claude Code reports — but it is also what the model aliases are pinned to, so setting it matters.
- **Background model** — Optional. See [Model Aliases and Background Calls](#model-aliases-and-background-calls). Leave blank to reuse the Model ID above.

Global defaults for all three live under **Settings → Backends → llama.cpp Configuration** and are used whenever the matching per-project field is blank.

### Starting llama-server

```bash
llama-server -m /path/to/model.gguf --port 8080 --host 0.0.0.0
```

`--host 0.0.0.0` matters: `llama-server` binds `127.0.0.1` by default, which the container cannot reach through `host.docker.internal`.

### How It Works

Triple-C sets `ANTHROPIC_BASE_URL` to your `llama-server`, and `ANTHROPIC_AUTH_TOKEN` to the placeholder `llama.cpp`. `llama-server` only checks the `Authorization` header when it was started with `--api-key` (default: none), so the value is ignored in the usual case — but Claude Code requires *some* credential to be present, so one is always sent.

> **Note:** llama.cpp support is best-effort. Claude Code is designed for Anthropic models, so some features (tool use, extended thinking, prompt caching, etc.) may not work as expected with non-Anthropic models.

---

## OpenAI Compatible Configuration

To route Claude Code through a gateway, set **Backend** to **OpenAI Compatible** under **Config → Model**.

> **The name is misleading, and the distinction matters.** Claude Code only ever sends
> `POST /v1/messages?beta=true` in **Anthropic Messages** format to `ANTHROPIC_BASE_URL`. It never
> calls OpenAI's `/v1/chat/completions`. So this backend requires an endpoint that implements the
> **Anthropic Messages API** — **LiteLLM** does, and works. A server that exposes only an
> OpenAI-compatible API (plain vLLM, text-generation-inference, LocalAI, OpenRouter, …) will
> **not** work here; put an Anthropic-shaped gateway such as LiteLLM in front of it.
> For Ollama and llama.cpp, use their own backends — both implement `/v1/messages` natively.
>
> (The backend name is kept as-is so existing projects keep working.)

### Settings

- **Base URL** — The URL of your gateway. Defaults to `http://host.docker.internal:4000`, LiteLLM's default port (adjust to match your server's address and port).
- **API Key** — Optional. The API key for your endpoint, if authentication is required. Stored securely in your OS keychain.
- **Model ID** — Optional. Override the model to use.
- **Background model** — Optional. See [Model Aliases and Background Calls](#model-aliases-and-background-calls). Leave blank to reuse the Model ID above.

Global defaults for the base URL, model and background model live under **Settings → Backends → OpenAI Compatible Configuration**.

### How It Works

Triple-C sets `ANTHROPIC_BASE_URL` to point Claude Code at your gateway. If an API key is provided, it is set as `ANTHROPIC_AUTH_TOKEN`.

> **Note:** OpenAI Compatible support is best-effort. Claude Code is designed for Anthropic models, so some features (tool use, extended thinking, prompt caching, etc.) may not work as expected when routing to non-Anthropic models through the endpoint.

---

## Model Aliases and Background Calls

Claude Code has four model aliases — `opus`, `sonnet`, `haiku` and `fable`. Left alone they resolve
to **Anthropic's** model IDs. A local server has never heard of those IDs, so every call that goes
through an alias fails, usually with no visible error.

The one that bites hardest is `haiku`: `ANTHROPIC_DEFAULT_HAIKU_MODEL` is documented as *"Model ID
that the `haiku` alias resolves to, also used for background functionality"* — conversation titles,
summaries, and other out-of-band work. If it is wrong, those quietly stop happening.

So for every backend that points at a custom endpoint — **Ollama**, **llama.cpp** and **OpenAI
Compatible** — Triple-C sets all four:

| Variable | Value |
|---|---|
| `ANTHROPIC_DEFAULT_OPUS_MODEL` | your configured **Model ID** |
| `ANTHROPIC_DEFAULT_SONNET_MODEL` | your configured **Model ID** |
| `ANTHROPIC_DEFAULT_HAIKU_MODEL` | your **Background model**, or the **Model ID** if that is blank |
| `ANTHROPIC_DEFAULT_FABLE_MODEL` | your configured **Model ID** |

**Leaving Background model blank is the right default.** A local server almost always serves one
model, and pointing every alias at it is what makes background work succeed.

Set **Background model** only if you serve a second, smaller model you would rather spend on titles
and summaries. It moves the Haiku alias alone; the other three still follow **Model ID**. It is
available per-project (Config → Model) and globally (Settings → Backends), with the usual
per-project-overrides-global rule.

Notes:

- These variables are **not** set for the **Anthropic** or **Bedrock** backends. Those reach
  servers that genuinely host the Anthropic model IDs, so Claude Code's own defaults are correct.
- All four names are reserved — you cannot set them yourself as custom environment variables.
- Changing a model or a Background model **recreates the container on the next start**, because
  environment variables can only change at creation time.
- `ANTHROPIC_SMALL_FAST_MODEL`, the deprecated predecessor of the Haiku variable, is not used.

---

## Settings

Access global settings via the **Settings** tab in the sidebar. The panel is a set of collapsible
sections: **General**, **Claude Authentication**, **Backends**, **Container**, **Certificates**,
**Git / SSH**, **Tools** and **Updates**.

### Claude Authentication

Acquire or revoke the shared Claude authentication token — see
[Shared Claude Authentication](#shared-claude-authentication).

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

### Default SSH Key Directory

Path to your SSH key directory (typically `~/.ssh`). This is mounted into **all** containers that don't have a per-project SSH path set. Per-project SSH paths take precedence.

### Corporate CA Certificate

If your organisation's network inspects TLS (a corporate proxy, a VPN that terminates HTTPS at the
edge), containers need your organisation's root certificate or **every** HTTPS call inside them
fails — `npm install`, `pip`, `git clone` over HTTPS, `curl`, the browser-view pane, and Claude
Code's own calls to the API.

Point this at either a **single certificate file** or a **folder** of them. It is mounted read-only
into every container and applied on every start, so it survives container recreation, base-image
migration and Reset — unlike a certificate you install by hand inside a running container, which is
lost the first time any of those happens.

The status line under the field tells you how many certificates were found and the names they will
be installed as inside the container. That rename matters: the container's trust store only reads
files ending in `.crt`, so a `.pem` is renamed rather than merely copied, which is the step that is
easiest to get wrong by hand.

Inside the container the certificate is trusted by:

| Consumer | How |
|---|---|
| curl, git, apt, wget | the system trust store (`update-ca-certificates`) |
| Node, npm, **Claude Code itself** | `NODE_EXTRA_CA_CERTS` |
| Python, pip, requests | `REQUESTS_CA_BUNDLE` and `SSL_CERT_FILE` |
| Chrome / Chromium (browser view) | its own NSS database at `~/.pki/nssdb` |

A per-project override lives in **Project Home → Config → Access**; leave it blank to use this
global setting. Changing either recreates the project's container on its next start — replacing the
certificate file in place counts as a change, so a rotated CA is picked up too.

### Default Git Name / Email

Sets `git user.name` and `git user.email` inside all containers. Per-project Git Name / Email settings take precedence. This is useful so you don't have to set the same name and email on every project.

### Claude Code Settings (Global Defaults)

Default Claude Code CLI settings applied to all projects. See [Claude Code Settings](#claude-code-settings) in the Project Configuration section for a description of each setting. Per-project settings override these global defaults.

### Web Terminal

Enable remote access to your project terminals from any device on the local network (tablets, phones, other computers).

- **Toggle** — Click ON/OFF to start or stop the web terminal server.
- **URL** — When running, shows the full URL including the access token. Click **Copy URL** to copy it to your clipboard, then open it in a browser on your tablet or phone.
- **Token** — An access token is auto-generated on first enable. Click **Copy** to copy the token, or **Regenerate** to create a new one (this disconnects existing web sessions).
- **Port** — Defaults to 7681. Configurable in `settings.json` if needed.

The web terminal server auto-starts on app launch if it was previously enabled, and stops when the app closes.

### Updates

- **Current Version** — The installed version of Triple-C.
- **Auto-check** — Toggle automatic update checks (every 24 hours).
- **Check now** — Manually check for updates.

When an update is available, a pulsing **Update** button appears in the top bar. Click it to see release notes and download links.

---

## Web Terminal (Remote Access)

The web terminal lets you access your running project terminals from a tablet, phone, or any other device on the local network — no app installation required, just a web browser.

### Setup

1. Go to **Settings** in the sidebar.
2. Find the **Web Terminal** section and click the toggle to **ON**.
3. A URL appears (e.g., `http://192.168.1.100:7681?token=...`). Click **Copy URL**.
4. Open the URL in a browser on your tablet or other device.

### Using the Web Terminal

The web terminal UI mirrors the desktop app's terminal experience:

- **Project picker** — Select a running project from the dropdown at the top.
- **Claude / Bash buttons** — Open a new Claude Code or bash session for the selected project.
- **Tab bar** — Switch between multiple open sessions. Click the **x** on a tab to close it.
- **Input bar** — A text input at the bottom optimized for mobile/tablet keyboards. Characters are sent immediately without waiting for autocomplete. Helper buttons for **Enter**, **Tab**, and **^C** (Ctrl+C) are provided for keys that are awkward on virtual keyboards.
- **Scroll to bottom** — A floating arrow button appears when you scroll up, letting you jump back to the latest output.

### Security

- Access requires a token in the URL query string. Without the correct token, connections are rejected.
- The token is auto-generated (32 bytes, base64url-encoded) and can be regenerated at any time from Settings.
- The server only listens on port 7681 (configurable) — make sure this port is not exposed to the public internet.
- All sessions opened from a browser tab are automatically cleaned up when the tab is closed or the WebSocket disconnects.

### Tips

- **Bookmark the URL** on your tablet for quick access.
- The web terminal works best in landscape orientation on tablets.
- If the connection drops (e.g., Wi-Fi interruption), the web terminal auto-reconnects after 2 seconds.
- Regenerating the token invalidates all existing browser sessions — you'll need to update bookmarks with the new URL.

---

## Terminal Features

### Multiple Sessions

You can open multiple terminal sessions (even for the same project). Each session gets its own tab
in the main tab strip, alongside any open Project Home tabs. Click a tab to switch, or click the
**×** on a tab to close it. Tabs show the project name (or a custom session name if you set one),
with a "(bash)" suffix for shell sessions and a badge for the permission mode the session was
launched with.

Right-click a terminal tab for **Rename tab**, **Reset name**, **Open project home** and
**Close tab**; double-click it to rename inline.

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

### Files

The **Files** tab of Project Home browses inside a running container. You can:

- **Browse** the container filesystem, starting at `/workspace`, with breadcrumb navigation
- **Download** any file to your host machine via the **Download** button on each file entry
- **Upload file** from your host into the current container directory
- **Refresh** the directory listing at any time

The listing shows file names, sizes, and modification dates.

### Terminal Rendering

The terminal uses WebGL for hardware-accelerated rendering of the active tab. Inactive tabs fall back to canvas rendering to conserve GPU resources. The terminal automatically resizes when you resize the window.

---

## Automation & Scheduled Tasks

Each container can run Claude Code on a schedule — recurring or one-time — through a small
scheduler called `triple-c-scheduler` that lives inside the image. Tasks run as separate,
headless Claude Code invocations (`claude -p "<your prompt>"`) driven by cron.

The **Automation** tab in Project Home is the place to watch and control them; tasks are created
from inside the container with the `triple-c-scheduler` CLI.

### The Automation Tab

With the container running, the Automation tab lists every task the scheduler knows about. For each
one you get its name, whether it is recurring or one-time, its cron expression or scheduled time,
and when it last ran. For each task you can:

| Control | What it does |
|---------|--------------|
| **Toggle** | Enable or disable the task without deleting it |
| **Run now** | Trigger the task immediately, outside its schedule |
| **Log** | Show the tail of that task's run log (last 200 lines) |
| **Remove** | Delete the task, after a confirmation |

**Refresh** re-reads everything from the container.

When tasks finish they leave **notifications**. If any are waiting, a panel appears at the top of
the tab listing each one with its task name, whether it succeeded or failed, how long ago it ran
and a summary. **Clear all** dismisses them. The Overview tab also shows a notification count and
the next few scheduled tasks.

### Permission mode

Scheduled runs use the project's [permission mode](#permission-modes) — they no longer always run
with `--dangerously-skip-permissions`. Because the mode travels into the container as an
environment variable, **stop and start the project** after changing it for the scheduler to see the
change. Remember that a headless run cannot answer a permission prompt, so in any mode other than
**Bypass** a task may stop early when Claude Code asks for approval; the run log records the mode
that was used.

### Creating Tasks (In the Container)

There is no "add task" form in the app. Create tasks from a terminal in the container — either type
the commands yourself in a **Shell** session, or just ask Claude to do it.

### Create a Recurring Task

```bash
triple-c-scheduler add --name "daily-review" --schedule "0 9 * * *" --prompt "Review open issues and summarize"
```

### Create a One-Time Task

```bash
triple-c-scheduler add --name "migrate-db" --at "2026-03-05 14:00" --prompt "Run database migrations"
```

One-time tasks automatically remove themselves after execution.

### Manage Tasks From the CLI

The CLI still works, and does the same things the Automation tab does:

```bash
triple-c-scheduler list                    # List all tasks
triple-c-scheduler enable --id abc123      # Enable a task
triple-c-scheduler disable --id abc123     # Disable a task
triple-c-scheduler remove --id abc123      # Delete a task
triple-c-scheduler run --id abc123         # Trigger a task now, streaming its log
triple-c-scheduler status                  # What is running right now, and for how long
triple-c-scheduler status --id abc123 -w   # Watch one task until its run finishes
triple-c-scheduler logs --id abc123        # View logs for a task
triple-c-scheduler logs --tail 20          # View last 20 log entries (all tasks)
triple-c-scheduler notifications           # View completion notifications
triple-c-scheduler notifications --clear   # Clear notifications
```

`list` carries a status column, and the Automation tab marks a task **Running** with
its elapsed time, so a triggered run is visible rather than silent.

Note that a log which has stopped growing is not evidence of a stall: `claude -p`
writes its answer in one go when it finishes, so a healthy run shows nothing but its
header for as long as it is thinking. `status` is what distinguishes a slow run from
a dead one — it reports the run only while the runner's process is genuinely alive.

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

> Scheduled tasks live on the project's config volume, so a **Reset** deletes them along with
> everything else on that volume.

---

## Keyboard Shortcuts

### Application

| Shortcut | Action |
|----------|--------|
| **Ctrl+T** | Open a new Claude terminal for the current project (nothing happens unless its container is running) |
| **Ctrl+Shift+W** | Close the active tab |
| **Ctrl+Tab** | Switch to the next tab |
| **Ctrl+Shift+Tab** | Switch to the previous tab |
| **Ctrl+1** … **Ctrl+9** | Jump to the first through ninth tab |
| **Ctrl+Shift+←** / **Ctrl+Shift+→** | Move the active tab one place along the strip (the mouse equivalent is dragging it) |

> **Why Ctrl+Shift+W and not Ctrl+W?** `Ctrl+W` is readline's `kill-word` — it deletes the word
> before the cursor, and it is used constantly in the terminal this app is built around. Binding it
> to "close tab" would make the shell unusable, so Triple-C deliberately leaves `Ctrl+W` alone.

### In the Terminal

| Shortcut | Action |
|----------|--------|
| **Ctrl+Shift+C** | Copy the selection, with trailing whitespace trimmed |
| **Ctrl+Shift+Alt+C** | Copy the selection exactly as-is |
| **Ctrl+Shift+V** | Paste |
| **Ctrl+V** | Paste an image from the clipboard into the container |
| **Ctrl+Shift+M** | Toggle speech-to-text recording (when enabled) |
| **Shift+Enter** | Insert a newline in Claude Code's prompt instead of submitting it |
| **Alt+Enter** | The same thing, and it has always worked — it was simply never written down |

Everything else goes straight through to the program running in the container.

> **Shift+Enter** sends `ESC` + `CR`, the same bytes Claude Code's own `/terminal-setup` installs
> for VS Code, Cursor, Alacritty and Zed — so there is nothing to run and no tip to follow. It is
> bound in **Claude** tabs only: in a **bash** tab that sequence means nothing to readline, and
> Shift+Enter there submits the line as it always has.
>
> In the [Web Terminal](#web-terminal-remote-access) the same chord works, and there is an **↵+**
> key beside **Enter** on the mobile key row for devices with no Shift.

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
| iproute2, WireGuard tools, iptables | Latest | Building a tunnel (when VPN Support is enabled) |
| git | Latest | Version control |
| GitHub CLI (gh) | Latest | GitHub integration |
| AWS CLI | v2 | AWS services and Bedrock |
| ripgrep | Latest | Fast code search |
| build-essential | — | C/C++ compiler toolchain |
| openssh-client | — | SSH for git and remote access |

The container also includes **clipboard shims** (`xclip`, `xsel`, `pbcopy`) that forward copy operations to the host via OSC 52, a **browser shim** (`triple-c-open`, installed as `xdg-open`, `sensible-browser`, `www-browser`, `x-www-browser` and `$BROWSER`) that relays URLs to your host browser — see [Opening URLs in Your Browser](#opening-urls-in-your-browser-url-relay) — and an **audio shim** (`rec`, `arecord`) for future voice mode support.

It also ships the **system libraries a browser needs to run** (`libnss3`, `libgbm1`, `libatk*`, `libasound2t64`, `libcups2t64`, `libpango`, `libdrm2`, fonts, and the rest of the set Playwright asks for). So `npx playwright install chromium` gives you a browser that actually starts. Before these were baked in, that download succeeded and the browser then died with *"Host system is missing dependencies: libnss3.so"*, which is why `sudo apt install google-chrome-stable` looked like the cure — apt was quietly installing the same libraries as Chrome's own dependencies.

The **browsers themselves are not pre-installed** — they are hundreds of megabytes and tied to the Playwright version you use. Install one with the Browser tab's setup buttons, or `npx playwright install chromium` in a terminal. They land in `~/.cache/ms-playwright`, which is on the home volume, so a browser survives container recreation and base-image migration and is only lost on a project **Reset**.

If your project's container was created from an older base image, it won't have the libraries — the Browser tab's install action detects that and installs them for you first, and says so while it does. That install lives in the container's writable layer, so it is undone by a **Reset** and by a base-image migration; migrating the project onto the current base image is what picks the libraries up for good.

You can install additional tools at runtime with `sudo apt install`, `pip install`, `npm install -g`, etc. Installed packages persist across container stops (but not across resets).

---

## Claude Code Tips

These features are built into Claude Code and work inside Triple-C containers with no extra configuration:

| Feature | How to Use |
|---------|-----------|
| **Focus Mode** | Run `/focus` or press `Ctrl+O` in the terminal to toggle collapsed tool output |
| **Session Recap** | Run `/recap` to get a summary of what happened in the current session |
| **Session Color** | Run `/color red` (or any color) to color-code your terminal prompt bar |
| **Recurring Tasks** | Run `/loop 5m check the deploy` to repeat a prompt every 5 minutes |
| **Interactive Lessons** | Run `/powerup` to learn Claude Code features with animated demos |
| **Team Onboarding** | Run `/team-onboarding` to generate a teammate ramp-up guide |
| **Bedrock Setup** | Select "3rd-party platform" on the login screen for an interactive Bedrock setup wizard |
| **Vertex AI Setup** | Select "3rd-party platform" on the login screen for an interactive Vertex AI setup wizard |
| **MCP Elicitation** | MCP servers can now request structured user input mid-task — works automatically |

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
- Read the error toast — the full message is behind its **Details** disclosure.

### OAuth Login URL Not Opening

- Triple-C detects long URLs printed by `claude login` and shows a toast with an **Open** button.
- If the toast doesn't appear, try scrolling up in the terminal — the URL may have already been printed.
- You can also manually copy the URL from the terminal output and paste it into your browser.

### "Couldn't find a suitable web browser" / a Command Won't Open a Page

The [URL relay](#opening-urls-in-your-browser-url-relay) should catch this. If a command still
complains, check from a terminal session in that project:

```bash
echo "$BROWSER"                       # /usr/local/bin/triple-c-open
triple-c-open https://example.com/    # should raise the prompt in the terminal
```

If `$BROWSER` is empty or `triple-c-open` is missing, the container is running an **older image**.
Rebuild it (Project Home → **Reset**, or pull/build the image again from Settings) — the relay is
part of the container image, not something the app can inject into a running container.

If you see *"no Triple-C terminal attached"*, the command is running somewhere with no terminal —
a scheduled task, or a shell you opened with your own `docker exec`. The URL is printed instead;
copy it into your browser. See
[When no terminal is attached](#when-no-terminal-is-attached).

If the prompt says the URL was refused, the command asked for a scheme the relay will not open on
your machine (anything that isn't `http`/`https`).

### A Browser Login Never Completes

You opened the URL, signed in successfully, and the CLI in the terminal is still waiting. The
callback from your browser is landing on your host's `localhost` while the CLI is listening on the
*container's*.

Two ways out, in order of least effort:

1. Dismiss and re-trigger the login, then click **In container** on the toast rather than **Open**.
   The page opens in a browser *inside* the container, so the callback never has to cross to the
   host. This needs no auth bridge — only a running container with Playwright installed (Project
   Home → **Browser**). For a recognised Anthropic sign-in link this is already the default button.
2. Turn on the [Auth Bridge](#browser-logins-inside-the-container-auth-bridge) — Project Home →
   **Config** → **Runtime** → **Auth bridge** — and try again. It can be switched on while the
   container is running. Check the indicator beside it: **Port conflict** means the host port was
   already taken and the callback still will not arrive.

For Claude specifically, the simpler answer is usually
[Shared Claude Authentication](#shared-claude-authentication), which finishes on an Anthropic-hosted
page and needs no callback at all.

### A Scheduled Task Stopped Part-Way Through

Scheduled tasks run headless and cannot answer a permission prompt. If the project is not in
**Bypass** mode, a task will stop when Claude Code asks for approval. Check the task's **Log** in
the Automation tab — it records the permission mode the run used.

### A Permission Mode Change Didn't Apply

- **In a terminal:** the mode is set when the terminal opens. Close the tab and open a new one.
- **For scheduled tasks:** the mode reaches the scheduler through the container's environment. Stop
  the project and start it again.

### File Permission Issues

- Triple-C automatically remaps the container user's UID/GID to match your host user, so files created inside the container should have the correct ownership on your host.
- If you see permission errors, try resetting the container: stop it, then choose **Reset container** from the **⋯** menu in the Project Home header. Note that this wipes `~/.claude`.

### Settings Won't Save

- Most project settings can only be changed when the container is **stopped**. Stop the container first, make your changes, then start it again.
- Some changes (like toggling Docker access, Mission Control, or changing mounted folders) trigger an automatic container recreation on the next start.

### "Failed to install Anthropic marketplace" Error

If Claude Code shows **"Failed to install Anthropic marketplace - Will retry on next startup"** repeatedly, the marketplace metadata in `~/.claude.json` may be corrupted. To fix this, open a **Shell** session in the project and run:

```bash
cp ~/.claude.json ~/.claude.json.bak && jq 'with_entries(select(.key | startswith("officialMarketplace") | not))' ~/.claude.json.bak > ~/.claude.json
```

This backs up your config and removes the corrupted marketplace entries. Claude Code will re-download them cleanly on the next startup.
