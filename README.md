<picture>
  <source media="(prefers-color-scheme: dark)" srcset="branding/triple-c-lockup-dark.svg">
  <img src="branding/triple-c-lockup-light.svg" alt="Triple-C — Coding Container" width="429" height="112">
</picture>

# Triple-C (Claude-Code-Container)

Triple-C is a cross-platform desktop application that sandboxes Claude Code inside Docker containers. Each project chooses its own **permission mode** — from Plan (read-only) through to Bypass (`--dangerously-skip-permissions`), which gives Claude unrestricted access within the sandbox.

This file is the architectural tour: what each subsystem is and why it works the way it does.

| Document | For |
|---|---|
| [HOW-TO-USE.md](HOW-TO-USE.md) | Using the app — first launch, projects, settings, troubleshooting |
| [BUILDING.md](BUILDING.md) | Building from source on Linux, macOS and Windows |
| [TECHNICAL.md](TECHNICAL.md) | Technology choices and the dependency inventory |
| [ROADMAP.md](ROADMAP.md) | Claude Code feature parity, gaps and sequencing |
| [CLAUDE.md](CLAUDE.md) | Working *on* this repo, for Claude Code |
| [branding/](branding/README.md) | The mark, the palette, and how the icons are generated |

## Contents

- [Architecture](#architecture) — layout, tabs, shortcuts, Project Home
- [Permission Modes](#permission-modes)
- [Containers](#containers) — lifecycle, base-image migration, mounts, CA certificates, sibling containers
- [Models and Authentication](#models-and-authentication) — backends, model aliases, gateway, shared token
- [Bridges to the Host](#bridges-to-the-host) — URL relay, auth bridge, browser view
- [Inside a Project](#inside-a-project) — capability tiles, Mission Control, web terminal, speech-to-text
- [Key Files](#key-files) · [CSS / Styling Notes](#css--styling-notes) · [Container Image](#container-image)

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

Tabs are user-reorderable — drag one, or move the active tab with `Ctrl+Shift+←/→`. A tab's
position is therefore never its identity: tabs are addressed by key, and indexed only through
`tabOrder`. The drag is built on pointer events rather than HTML5 drag-and-drop, deliberately:
Tauri's `dragDropEnabled` blocks HTML5 drag inside the webview on Windows, and it cannot simply be
switched off because `TerminalView` needs Tauri's native drag-drop event — the only one that
carries dropped *file paths*.

### Keyboard Shortcuts

Implemented in `hooks/useKeyboardShortcuts.ts` (document-level, capture phase):

| Shortcut | Action |
|---|---|
| `Ctrl+T` | New Claude terminal for the current project (no-op unless it is running) |
| `Ctrl+Shift+W` | Close the active tab |
| `Ctrl+Tab` / `Ctrl+Shift+Tab` | Cycle tabs forward / backward |
| `Ctrl+1` … `Ctrl+9` | Jump to the nth tab |
| `Ctrl+Shift+←` / `Ctrl+Shift+→` | Move the active tab left / right |

`Ctrl+W` is deliberately **not** bound: it is readline's `kill-word`, used constantly in the
terminal this app is built around. Plain `Ctrl+←/→` is readline's word-wise cursor motion, which is
why moving a tab takes Shift as well.

Terminal-scoped keys are handled in `TerminalView.tsx`:

| Shortcut | Action |
|---|---|
| `Ctrl+Shift+C` / `Ctrl+Shift+Alt+C` | Copy the selection, trimmed / exactly as-is |
| `Ctrl+Shift+M` | Toggle speech-to-text recording |
| `Shift+Enter` | Insert a newline in Claude Code's prompt instead of submitting |
| `Alt+Enter` | The same thing — xterm.js already ESC-prefixes on Alt, so this has always worked |

`Shift+Enter` sends `ESC` + `CR`, which is what Claude Code's own `/terminal-setup` installs for
VS Code, Cursor, Alacritty and Zed. It is bound in Claude sessions only: in a bash tab those bytes
are unbound in readline. The web terminal does the same, and adds an `↵+` key beside Enter for
devices with no Shift.

### Project Home

Clicking a project row in the sidebar opens **Project Home** in the main area — the per-project
view, with tabs **Overview · Sessions · Automation · Config · Files · Browser**. The sidebar row
itself is select-only (plus hover controls for start/stop and opening a terminal); it holds no
configuration. Per-project configuration lives in the Config tab rather than in modals.

| Tab | Contents |
|---|---|
| **Overview** | Permission mode control, sandbox/backend/Docker-access summary, capability tiles, recent sessions, scheduled tasks, base-image staleness banner |
| **Sessions** | Past Claude Code conversations read from the config volume, with **Resume** |
| **Automation** | The container's `triple-c-scheduler` tasks — create, edit, enable/disable, run now, read logs, remove, and completion notifications |
| **Config** | Workspace (name, folders), Model (backend), Access (SSH, git, env vars, port mappings), Runtime (permission mode, sandbox, Docker access, Mission Control, instructions, Claude Code settings) |
| **Files** | Browse, download and upload files inside the container |
| **Browser** | Watch and take over the Playwright browser inside the container — see [Browser View](#browser-view) |

Container start/stop progress is reported inline (on the sidebar row and in the Project Home
header) via the `container-progress` event, and failures surface as toasts. There is no blocking
progress modal.

## Permission Modes

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

## Containers

### Container Lifecycle

1. **Create**: New container created with bind mounts, named volumes, env vars, and labels
2. **Start**: Container started, entrypoint remaps UID/GID, sets up SSH, configures Docker group, installs any CA certificates, injects Claude Code settings, rebuilds the scheduler crontab
3. **Terminal**: `docker exec` launches Claude Code (with the project's permission-mode flags) or a bash login shell, with a PTY
4. **Stop**: Container halted (its filesystem layer and both named volumes persist)
5. **Restart**: Existing container restarted; if any `triple-c.*` label no longer matches the project's settings, the container is committed to a snapshot image, removed, and recreated from that snapshot — so installed packages survive
6. **Migrate**: The project is moved onto a newer base image without losing its volumes — see below

Each recreation moves the `triple-c-snapshot-{projectId}:latest` tag, leaving the image it pointed
at before untagged but still on disk — multiple gigabytes per recreation. `sweep_orphaned_snapshots`
clears those after a recreation and after a migration is accepted. It only ever removes images that
are **both** untagged *and* labelled `triple-c.managed=true`, so a live snapshot tag and a
migration's `pre-migration-*` rollback pin are structurally out of reach, and removal is unforced so
Docker itself refuses while any container — including a stopped project's — is still built from the
image.
7. **Reset**: Container, snapshot image **and both named volumes** all removed, then recreated from the clean base image. `remove_project_volumes` deletes `triple-c-home-{projectId}` and `triple-c-claude-config-{projectId}`, so `~/.claude`, `~/.claude.json`, the OAuth login, installed skills, session transcripts and the scheduler's tasks are all lost.

### Base-Image Migration

A container is created from `triple-c-snapshot-{projectId}:latest` whenever that image exists, and
every recreation re-commits it. So without an explicit act, a project stays on the base image it was
first built from **forever** — it never picks up a new `/usr/local/bin` shim, a new `socat`, or a
security update. **Update container base…** (Project Home → overflow menu) is the non-destructive
way out; Reset is the destructive one. `docker/migration.rs` owns it.

- **Staleness is surfaced, not acted on.** `triple-c.base-image-id` records the lineage and
  `get_container_staleness` reports it as a banner, but it is deliberately *not* compared in
  `container_needs_recreation`. Comparing it there would recreate every project *from its own
  snapshot* on the next base bump: churn on the old base, and the "you should migrate" signal
  consumed without migrating. A missing lineage label means "unknown, probe instead" — never
  "stale".
- **What comes across**: the apt package delta and user-authored files, computed by diffing two
  filesystem manifests through dpkg ownership and presence-in-the-new-base. (`docker diff` is
  useless here — on a snapshot-derived container it only reports changes since the last commit.
  Measured on a real project, manifest diffing turned 8,677 raw path differences into 2 genuinely
  user-authored ones.) Both named volumes are untouched at every step, so `$HOME`, the OAuth login,
  skills, transcripts and scheduler tasks simply re-attach.
- **What does not**: `/etc` is reported but never copied — the old lineage has
  `/etc/apt/sources.list.d/nodesource.sources` where the current base has `nodesource.list`, and
  having both breaks every `apt-get update`. `/var` is not copied either, and that is the one way
  migration is *more* destructive than an ordinary recreate: a database under `/var/lib` rides along
  on a recreate, but a migration builds from the base and the apt replay hands back an empty
  cluster. `unpreserved_data()` names those directories in the pre-flight, the banner and the final
  report.
- **Crash-safety**: `:latest` keeps pointing at the old lineage until the final commit, so any
  failure before that self-heals — the next start just recreates from the old snapshot. After the
  container swap, a `triple-c.migration-state=in-progress` label plus a persisted state file let the
  app offer **resume** or **rollback**. Rollback restores the system layer only; work done in
  `$HOME` during a migrated session survives it.

### Mounts

| Target in Container | Source | Type | Notes |
|---|---|---|---|
| `/workspace/<mount-name>` | Each configured project folder | Bind | Read-write; one per folder |
| `/home/claude` | `triple-c-home-{projectId}` | Named Volume | Home directory; survives stop/start and recreation |
| `/home/claude/.claude` | `triple-c-claude-config-{projectId}` | Named Volume | Nested inside the home volume; Docker gives the more specific mount precedence |
| `/tmp/.host-ssh` | SSH key directory | Bind | Read-only; entrypoint copies to `~/.ssh` |
| `/tmp/.host-aws` | AWS config directory | Bind | Read-only; entrypoint copies to `~/.aws`; for Bedrock auth |
| `/tmp/.host-ca` | CA certificate file or directory | Bind | Read-only; entrypoint installs into the system and NSS stores |
| `/var/run/docker.sock` | Host Docker socket | Bind | If "Allow container spawning" is ON |

These two named volumes are the only ones a project owns. Both are removed by Reset and by project
removal, and by nothing else.

### Corporate CA Certificates

A global **Certificates** setting (`AppSettings::ca_cert_path`) with a per-project override
(`Project::ca_cert_path`), accepting a single certificate file **or** a directory. It follows the
SSH/AWS host-mount pattern — read-only bind mount at `/tmp/.host-ca`, applied by the entrypoint on
every start — so it survives recreation, migration and Reset.

- **Certificates are renamed to `.crt`.** `update-ca-certificates` globs `*.crt`, case-sensitively;
  a `.pem` merely copied into `/usr/local/share/ca-certificates/` is ignored in total silence.
  `container_cert_name()` in Rust does the renaming, mirrored in a few lines of shell in the
  entrypoint. A single-file mount lands at `/tmp/.host-ca/<name>.crt`, so the entrypoint only ever
  sees a directory.
- **The system store is not enough.** Only curl, git and apt read it. Node — and therefore Claude
  Code itself — needs `NODE_EXTRA_CA_CERTS`; Python and requests need
  `REQUESTS_CA_BUNDLE`/`SSL_CERT_FILE`; Chromium reads neither and wants its own NSS database at
  `~/.pki/nssdb`, seeded with `certutil` (from `libnss3-tools`). The NSS step warns and continues
  rather than failing the start.
- **Those env vars are set from Rust at creation, never exported by the entrypoint.** A terminal
  session is a `docker exec`, which inherits the container's configured env and sees nothing the
  entrypoint exported — the same lesson that made `$BROWSER` an image-level `ENV`. They are emitted
  **empty** when no CA is configured, because `docker commit` bakes env into the snapshot image.
- **`triple-c.ca-fingerprint` covers the certificate bytes, not the path.** Replacing a rotated CA
  at the same location still forces the recreation that copies it in. Clearing the setting actively
  **removes** `triple-c-*.crt` from the container — `/usr/local/share` rides the project's snapshot,
  so turning the feature off has to undo, not merely stop.

### Container Spawning (Sibling Containers)

When "Allow container spawning" is enabled per-project, the host Docker socket is bind-mounted into the container. This allows Claude Code to create **sibling containers** (not nested Docker-in-Docker) that are visible to the host. The entrypoint detects the socket's GID and adds the `claude` user to the matching group.

If the Docker access setting is toggled after a container already exists, the container is automatically recreated on next start to apply the mount change. The named config volume (keyed by project ID) is preserved across recreation.

### Docker Socket Path

The socket path is OS-aware:
- **Linux/macOS**: `/var/run/docker.sock`
- **Windows**: `//./pipe/docker_engine`

Users can override this in Settings via the global `docker_socket_path` option.

## Models and Authentication

### Authentication Modes

Each project can independently use one of:

- **Anthropic** (OAuth or shared token): either the shared `claude setup-token` token injected as `CLAUDE_CODE_OAUTH_TOKEN` (see below), or a per-container `claude login`. An interactive login's token lives in the config volume and survives container stop/start and recreation — but **not** a Reset, which deletes the volumes.
- **AWS Bedrock**: Per-project AWS credentials (static keys, profile, or bearer token). SSO sessions are validated before launching Claude for Profile auth.
- **Ollama**: Connect to a local or remote Ollama server via `ANTHROPIC_BASE_URL` (e.g., `http://host.docker.internal:11434`). Requires a model ID, and the model must be pulled (or used via Ollama cloud) before starting the container.
- **llama.cpp**: Connect to a local or remote `llama-server` via `ANTHROPIC_BASE_URL` (e.g., `http://host.docker.internal:8080` — 8080 is `llama-server`'s default port). `ANTHROPIC_AUTH_TOKEN` is set to a placeholder; `llama-server` ignores it unless it was started with `--api-key`.
- **OpenAI Compatible**: Connect through a gateway that implements the **Anthropic Messages API**, via `ANTHROPIC_BASE_URL` + `ANTHROPIC_AUTH_TOKEN`. API key stored securely in OS keychain. Triple-C can run that gateway for you — see [Model Gateway](#model-gateway-litellm-sibling-container).

> **The endpoint must speak the Anthropic Messages API.** Claude Code only ever sends
> `POST /v1/messages?beta=true` in Anthropic Messages format to `ANTHROPIC_BASE_URL` — it never
> speaks OpenAI's `/v1/chat/completions`. So a server that exposes *only* an OpenAI-compatible API
> (plain vLLM, text-generation-inference, LocalAI, OpenRouter, …) will **not** work behind any of
> these backends. What does work: **LiteLLM**, which exposes an Anthropic-shaped route, and
> **Ollama** and **llama.cpp**, both of which implement `POST /v1/messages` natively — which is why
> they get first-class backends of their own rather than going through a translation layer.

#### Model alias variables

The `opus` / `sonnet` / `haiku` / `fable` aliases in Claude Code resolve to Anthropic model IDs by
default. Against a local server those IDs do not exist, so anything that uses an alias fails —
most visibly the **background** calls (conversation titles, summaries), which use `haiku`.

For every backend that points at a custom endpoint (Ollama, llama.cpp, OpenAI Compatible),
Triple-C therefore sets all four:

| Variable | Value |
|---|---|
| `ANTHROPIC_DEFAULT_OPUS_MODEL` | the backend's configured model ID |
| `ANTHROPIC_DEFAULT_SONNET_MODEL` | the backend's configured model ID |
| `ANTHROPIC_DEFAULT_HAIKU_MODEL` | the **Background model** override, else the configured model ID |
| `ANTHROPIC_DEFAULT_FABLE_MODEL` | the backend's configured model ID |

A local server usually serves exactly one model, so pointing every alias at it is the right
default. If you run a second, smaller model for cheap background work, set **Background model**
(Config → Model, and in global Backend settings) and only the Haiku alias moves.

These are *not* set for the Anthropic or Bedrock backends, which reach servers that really do host
the Anthropic model IDs. Triple-C manages all four names, so they cannot be set as custom
environment variables. (`ANTHROPIC_SMALL_FAST_MODEL` is deprecated and is not used.)

> **Note:** Ollama, llama.cpp and OpenAI Compatible support is best-effort. Claude Code is designed for Anthropic models, so some features (tool use, extended thinking, prompt caching, etc.) may not work as expected with non-Anthropic models behind these backends.

### Model Gateway (LiteLLM sibling container)

For providers that only speak OpenAI's API, Triple-C can run **LiteLLM** as a sibling container
(`docker/gateway.rs`, `gateway-container/`) that gives Claude Code the Anthropic-format front end it
requires. Settings → Gateway configures the provider prefix (`openai`, `azure`, `gemini`, `groq`,
…), an optional API base override, the models to serve, and the host port (default `4000`). A
project then consumes it with the OpenAI Compatible backend. It mirrors the STT container's
lifecycle, including auto-start with the app.

Its bind address is **detected, never `0.0.0.0`**. Unlike STT, the consumers are *project
containers*, so loopback alone is not always enough: Docker Desktop binds `127.0.0.1` and advertises
`host.docker.internal`; native Linux binds the default bridge gateway (`172.17.0.1`) and advertises
the same literal. `GatewayBinding` derives the bind address and the advertised `base_url` together
so the two cannot drift. A wildcard bind would be LAN-reachable — Docker's rules precede host
firewalls — in front of a config file holding a billed provider key. A LiteLLM `master_key` is
**always** set, because LiteLLM without one accepts any key.

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

## Bridges to the Host

### URL Relay (host browser)

There is no browser and no display inside the container, so any CLI that wants to open a web page
— `gh auth login`, `aws sso login`, `gcloud auth login`, `az login`, vendor CLIs, `xdg-open`,
Python's `webbrowser` — simply fails. The URL relay forwards the *request* to the host, where the
user's real browser is. Nothing is rendered or forwarded from the container; only the URL travels.
It complements the Auth Bridge below: the relay gets the login page open, the bridge lets the
callback land.

**Transport — an OSC escape sequence, following `osc52-clipboard`.** `container/triple-c-open`
writes

```
ESC ] 7777 ; open ; <base64(url)> BEL
```

to **`/dev/tty`**, and `TerminalView.tsx` picks it up with `term.parser.registerOscHandler(7777, …)`.
`/dev/tty` rather than stdout is the whole point: the shim usually runs as a grandchild of
something that captures its children's output (Claude Code invoking `gh auth login` as a tool
call), so a printed sentinel line — the `###TRIPLE_C_SSO_REFRESH###` approach — would be swallowed
by the intermediate process and never reach the terminal. A control sequence on the controlling
terminal always arrives, and is invisible to terminals that don't know it. Base64 keeps a `;`,
`BEL` or `ESC` inside the URL from breaking out of the sequence.

**Container side** — `container/triple-c-open`, installed as `xdg-open`, `sensible-browser`,
`www-browser`, `x-www-browser`, `gnome-open`, `gvfs-open`, `kde-open`, `open`, and exported as
`$BROWSER`. Ubuntu 24.04 ships a real `/usr/bin/sensible-browser` (from `sensible-utils`), so that
one is `dpkg-divert`ed rather than merely shadowed by a `/usr/local/bin` symlink; `www-browser` and
`x-www-browser` are registered through `update-alternatives` and pinned with `--set`, because
`sensible-browser` probes them by absolute path and because a later `apt install firefox` must not
be able to steal them. `xdg-open` is diverted pre-emptively so installing `xdg-utils` inside the
container cannot displace the relay. `BROWSER` is an image-level `ENV` — terminal sessions are
separate `docker exec`s and never see what the entrypoint exported — and the entrypoint also
forwards it into the scheduler's cron environment file.

**No terminal attached** (cron-driven scheduled tasks, or a plain `docker exec` from outside
Triple-C): there is no handshake and nothing to wait for, so the shim never blocks. The write to
`/dev/tty` fails, and it prints the URL in plain text on its own line and exits 0 — which lands in
the scheduler task log where a human can still act on it.

**Security posture — the container is the untrusted side.** `app/src/lib/urlRelay.ts` validates
before anything reaches `openUrl`: `http:`/`https:` only (`file:`, `javascript:`, `data:` and every
registered protocol handler rejected), no embedded credentials, no control characters or
whitespace, length-capped, and returned WHATWG-normalized so the prompt shows exactly what will
open. Nothing opens automatically — the user confirms in the existing `UrlToast`, and prompts are
rate-limited (5 per 10 s, repeats of the same URL collapsed) so a loop in the container cannot bury
the UI.

**Web terminal** — deliberately *not* a copy of the desktop behaviour. The browser there belongs to
a remote viewer, possibly on a phone across a tunnel, so `terminal.html` renders the relayed URL as
a tap-to-open link banner with the same scheme allowlist and rate limit, and opens nothing by
itself. The OSC handler is registered regardless so the sequence is consumed rather than painted as
garbage.

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

### Browser View

Watch — and take over — the browser Claude is driving with Playwright inside the container. The
**Browser** tab runs Playwright's own dashboard (`browser.bind()` plus `playwright-cli show`) in the
container and fronts it with a **token-gated** loopback proxy on the host (`browser_view/`). Opt-in
per project.

- **It deliberately does not reuse the auth bridge's `PortForward`**, which binds an
  unauthenticated port — fine for a throwaway OAuth listener, wrong for remote control of a browser.
  Host ports are confined to `47820..=47827` because CSP `frame-src` cannot express a port range and
  has to enumerate them; a unit test asserts the Rust range matches `tauri.conf.json`.
- **Pop out** puts the same URL in a second OS window (`popout.rs`), so the view can be watched on
  another monitor or pinned on top while the main window is used for work. No capability lists that
  window, so it has **no IPC surface**; the app CSP does not apply to it either, because it is a
  top-level document rather than a frame — the token gate is what protects the port in both cases.
  The window is owned by the *session*, so the supervisor's teardown closes it. The pane drops its
  iframe while popped out, and both viewers can drive the browser.
- **Open page…** launches a browser in the container at a URL and viewport you choose and binds it,
  so the pane shows it (`page.rs`). This is what serves container-side auth — the OAuth callback
  listener is *in* the container, so a container-side browser closes the loop with no host round
  trip and no auth bridge — and dev servers on container loopback. Re-opening with a helper already
  up *navigates* rather than relaunching, so a session signed in on one page survives to the next.
- **Resizing the window does not resize the page.** The viewer is a CDP screencast: a bigger window
  is the same pixels drawn larger. `page.setViewportSize()` is what reflows, and match-window mode
  pushes the pop-out's settled size into it, debounced by generation counter because a drag emits
  continuously and each event costs a container exec.
- **Setup is two clicks, and nothing installs itself.** Detection has to look past `node_modules` —
  `claude mcp add … npx @playwright/mcp@latest` installs into `~/.npm/_npx/<hash>/node_modules` — and
  hops from a wrapper `playwright` to its **nested** `playwright-core`, because npm does not hoist
  for global installs and the wrapper ships no type definitions to read a version from. Installing
  puts Playwright in `/workspace` with `--no-save` (not a bind mount, so it touches nothing of
  yours) and browsers in `~/.cache/ms-playwright`, which is inside the home volume and so survives
  recreation *and* migration.
- **`@playwright/mcp` can never satisfy this pane** on its own: it bundles a `playwright-core` that
  binds, but never `@playwright/cli`, which is the viewer. It is what binds sessions automatically
  once Playwright is present — not a setup route.

## Inside a Project

### Container Introspection (Capability Tiles)

`list_container_capabilities` (`commands/inspect_commands.rs`) runs a read-only `find`/`jq` script
inside a running container and returns counts plus item lists for **skills, agents, commands, hooks,
plugins and MCP servers**, at user scope (`/home/claude/.claude`) and project scope
(`/workspace/*/.claude`, `/workspace/*/.mcp.json`). Overview renders these as tiles.

Triple-C does not create or edit any of them — Claude Code owns that configuration, and the tiles
link out to a terminal where `/agents`, `/hooks`, `/plugins` and `/mcp` do the real work.

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
- **Input device**: Selectable in Settings when the host exposes more than one microphone
- **Language**: Optional language hint for transcription
- **Auto-start**: When STT is enabled in Settings, the container starts automatically with the app — no need to manually start it after each restart
- **On-demand fallback**: If not auto-started, the container starts automatically when you first click the mic button

**How it works**: Audio is captured in the browser via the Web Audio API, encoded as WAV, and sent to the Faster Whisper container's `/transcribe` endpoint. The transcribed text is inserted directly into the active terminal. The STT container uses a named Docker volume (`triple-c-stt-model-cache`) to cache Whisper models across restarts.

## Key Files

### Frontend — layout and projects

| File | Purpose |
|---|---|
| `app/src/App.tsx` | Root layout (TopBar + Sidebar + Main + StatusBar + ToastHost) |
| `app/src/index.css` | Global CSS variables, dark theme, `color-scheme: dark`, `:focus-visible` ring |
| `app/src/components/layout/TopBar.tsx` | Hosts MainTabs + Docker/Image status indicators + Help |
| `app/src/components/layout/MainTabs.tsx` | The single main-area tab strip (Project Home + terminal tabs), pointer-event drag reordering |
| `app/src/components/layout/Sidebar.tsx` | Responsive sidebar (25% width, min 224px, max 320px), collapsible to an icon rail |
| `app/src/components/layout/StatusBar.tsx` | Project/terminal counts, Jump to Current, STT mic |
| `app/src/components/projects/ProjectRow.tsx` | Select-only sidebar row; opens Project Home, with hover start/stop and terminal controls |
| `app/src/components/projects/ProjectList.tsx` | Project list in sidebar |
| `app/src/components/projects/PermissionModeControl.tsx` | Plan / Default / Accept Edits / Bypass segmented control |
| `app/src/components/ui/` | Shared primitives: `Modal`, `Button`, `Toggle`, `Field`, `SegmentedControl`, `StatusIndicator`, `SaveIndicator`, `OverflowMenu`, `ToastHost`, `Tooltip` |
| `app/src/hooks/useKeyboardShortcuts.ts` | `Ctrl+T`, `Ctrl+Shift+W`, `Ctrl+Tab`, `Ctrl+1..9`, `Ctrl+Shift+←/→` |
| `app/src/hooks/useContainerProgress.ts` | `container-progress` event → inline progress lines |

### Frontend — Project Home

| File | Purpose |
|---|---|
| `app/src/components/projects/home/ProjectHome.tsx` | Project Home shell: header actions, overflow menu, tab strip |
| `app/src/components/projects/home/OverviewTab.tsx` | Permission mode, summary, capability tiles, recent sessions and tasks |
| `app/src/components/projects/home/SessionsTab.tsx` | Past Claude sessions with Resume |
| `app/src/components/projects/home/AutomationTab.tsx` | Scheduler tasks: create, toggle, run now, logs, remove, notifications |
| `app/src/components/projects/home/TaskEditorModal.tsx` | Create/edit a scheduled task; `taskValidation.ts` holds the cron and schedule rules |
| `app/src/components/projects/home/ConfigTab.tsx` | Config sections (Workspace, Model, Access, Runtime) |
| `app/src/components/projects/home/FilesTab.tsx` | File browser (browse, download, upload) |
| `app/src/components/projects/home/BrowserTab.tsx` | Browser view pane: detect, install, watch, take over, pop out |
| `app/src/components/projects/home/OpenPageDialog.tsx` | Open a URL in the container's browser at a chosen viewport |
| `app/src/components/projects/home/ContainerMigrationBanner.tsx` | Base-image staleness banner, migration progress, resume/rollback |
| `app/src/components/projects/home/CapabilityTiles.tsx` | Read-only skills/agents/commands/hooks/plugins/MCP counts |
| `app/src/components/projects/ClaudeCodeSettingsEditor.tsx` | Claude Code CLI settings (TUI mode, effort, focus, caching) |

### Frontend — settings, terminal and hooks

| File | Purpose |
|---|---|
| `app/src/components/settings/SettingsPanel.tsx` | Docker, AWS, timezone, certificates, gateway, web terminal, STT, shared auth and global settings |
| `app/src/components/settings/CertificateSettings.tsx` | Corporate CA certificate path (global), with `CaCertPathInput` |
| `app/src/components/settings/GatewaySettings.tsx` | LiteLLM gateway: provider, API base, models, port, container controls |
| `app/src/components/settings/SharedAuthSettings.tsx` | Acquire / revoke the shared Claude authentication token |
| `app/src/components/settings/WebTerminalSettings.tsx` | Web terminal toggle, URL, token management |
| `app/src/components/settings/SttSettings.tsx` | STT settings panel (model, port, language, device, container controls) |
| `app/src/components/settings/UpdateDialog.tsx` | New-release notice with download links (`update_commands.rs`) |
| `app/src/components/terminal/TerminalView.tsx` | xterm.js terminal with WebGL, URL detection, OSC 52 clipboard, OSC 7777 URL relay, image paste |
| `app/src/components/terminal/SttButton.tsx` | Mic button with on-demand STT container start |
| `app/src/hooks/useTerminal.ts` | Terminal session management (claude and bash modes) |
| `app/src/hooks/useProjectActions.ts` | Start/stop/reset/backup and terminal-opening helpers |
| `app/src/hooks/useContainerMigration.ts` | Staleness polling, migration run, resume and rollback |
| `app/src/hooks/useFileManager.ts` | File manager operations (list, download, upload) |
| `app/src/hooks/useClaudeAuth.ts` | Shared-token status and acquisition |
| `app/src/hooks/useSTT.ts` | Speech-to-text recording, transcription, and container management |
| `app/src/lib/urlRelay.ts` | Host-side relay validation: OSC 7777 parsing, http/https allowlist, rate limiting |
| `app/src/lib/wav.ts` | WAV audio encoding for STT transcription |

### Backend (Rust)

| File | Purpose |
|---|---|
| `app/src-tauri/src/docker/container.rs` | Container creation, mounts, env vars, labels, recreation checks, `remove_project_volumes` |
| `app/src-tauri/src/docker/exec.rs` | `create_attached_exec()` — the single attached-exec path; file upload/download via tar |
| `app/src-tauri/src/docker/image.rs` | Image building/pulling |
| `app/src-tauri/src/docker/migration.rs` | Base-image migration: manifest capture, delta computation, crash-recovery state machine |
| `app/src-tauri/src/docker/ca_certs.rs` | CA certificate discovery, `.crt` renaming, fingerprinting |
| `app/src-tauri/src/docker/gateway.rs` | LiteLLM sibling container: binding detection, config rendering, lifecycle |
| `app/src-tauri/src/docker/stt.rs` | Speech-to-text container lifecycle |
| `app/src-tauri/src/docker/legacy_cleanup.rs` | One-release migration shim removing leftovers from the deleted MCP feature |
| `app/src-tauri/src/auth_bridge/` | Loopback callback bridge (`mod.rs`, `proc_net.rs`, `tunnel.rs`) |
| `app/src-tauri/src/browser_view/` | Browser view: `detect.rs`, `install.rs`, `page.rs`, `popout.rs`, `proxy.rs`, `commands.rs` |
| `app/src-tauri/src/commands/project_commands.rs` | Start/stop/rebuild Tauri command handlers |
| `app/src-tauri/src/commands/migration_commands.rs` | Staleness, migrate, confirm, rollback, reconcile, `is_migrating` |
| `app/src-tauri/src/commands/inspect_commands.rs` | Read-only container views: sessions, capabilities, scheduler tasks |
| `app/src-tauri/src/commands/auth_token_commands.rs` | `claude setup-token` flow, redaction, keychain storage |
| `app/src-tauri/src/commands/auth_bridge_commands.rs` | Auth bridge enable/status commands |
| `app/src-tauri/src/commands/file_commands.rs` | File manager Tauri commands (list, download, upload) |
| `app/src-tauri/src/commands/stt_commands.rs` | STT start/stop/transcribe Tauri commands |
| `app/src-tauri/src/commands/web_terminal_commands.rs` | Web terminal start/stop/status Tauri commands |
| `app/src-tauri/src/models/project.rs` | Project struct (backend, `PermissionMode`, Docker access, Claude Code settings, Mission Control, auth bridge, browser view, CA path, shared-token opt-out) |
| `app/src-tauri/src/models/app_settings.rs` | Global settings (image source, Docker socket, AWS, CA path, Claude Code settings, web terminal, STT, gateway) |
| `app/src-tauri/src/models/gateway_settings.rs` | Gateway provider, models, port and API base |
| `app/src-tauri/src/web_terminal/server.rs` | Axum HTTP+WS server for remote terminal access |
| `app/src-tauri/src/web_terminal/ws_handler.rs` | WebSocket connection handler and session management |
| `app/src-tauri/src/web_terminal/terminal.html` | Embedded web UI (xterm.js, project picker, tabs) |
| `app/src-tauri/src/storage/secure.rs` | OS keychain access (per-project secrets, shared token, gateway keys, rotation id) |

### Container and packaging

| File | Purpose |
|---|---|
| `container/Dockerfile` | Ubuntu 24.04 sandbox image with Claude Code + dev tools + clipboard/audio shims + browser runtime libraries |
| `container/entrypoint.sh` | UID/GID remap, SSH setup, CA installation, Docker group config, Claude Code settings injection, Mission Control setup |
| `container/osc52-clipboard` | Clipboard shim (xclip/xsel/pbcopy via OSC 52) |
| `container/triple-c-open` | URL relay shim (xdg-open/`$BROWSER`/sensible-browser via OSC 7777); prints the URL when no terminal is attached |
| `container/audio-shim` | Audio capture shim (rec/arecord via FIFO) for voice mode |
| `container/triple-c-scheduler` | Bash CLI managing scheduled task JSON and the crontab |
| `container/triple-c-task-runner` | Cron entry point; maps `TRIPLE_C_PERMISSION_MODE` to flags and runs `claude -p` |
| `container/triple-c-sso-refresh` | AWS SSO session refresh helper |
| `gateway-container/` | LiteLLM image and rendered `config.yaml` for the model gateway |
| `stt-container/Dockerfile` | Faster Whisper STT container image (Python 3.11 + FastAPI) |
| `stt-container/server.py` | STT HTTP server (POST /transcribe endpoint) |
| `branding/` | Logo sources, palette, and `build-icons.py`, which generates every packaged icon |

## CSS / Styling Notes

- Uses **Tailwind CSS v4** with the Vite plugin (`@tailwindcss/vite`)
- All colors use CSS custom properties defined in `index.css` `:root`
- `color-scheme: dark` is set on `:root` so native form controls (select dropdowns, scrollbars) render in dark mode
- **Do not** add a global `* { padding: 0 }` reset — Tailwind v4 uses CSS `@layer`, and unlayered CSS overrides all layered utilities. Tailwind's built-in Preflight handles resets.

## Container Image

**Base**: Ubuntu 24.04

**Pre-installed tools**: Claude Code, Node.js 22 LTS + pnpm, Python 3.12 + uv + ruff, Rust (stable), Docker CLI, git + gh, AWS CLI v2, ripgrep, openssh-client, build-essential, `libnss3-tools` (for `certutil`, used to seed Chromium's CA store)

**Shims**: `xclip`/`xsel`/`pbcopy` (OSC 52 clipboard forwarding), `xdg-open`/`sensible-browser`/`www-browser`/`x-www-browser`/`$BROWSER` (OSC 7777 URL relay to the host browser), `rec`/`arecord` (audio FIFO for voice mode)

**Browser runtime libraries**: the shared libraries Chromium links against (`libnss3`, `libgbm1`,
`libatk*`, `libasound2t64`, `libcups2t64`, `libpango`, `libdrm2`, … plus fonts) are baked in, via
`npx playwright install-deps chromium` at build time. Without them `playwright install chromium`
downloads a browser that then dies at launch with *"Host system is missing dependencies:
libnss3.so"* — which is why installing `google-chrome-stable` used to look like the fix (apt was
pulling the libraries in as *its* dependencies). Measured cost of the layer: +99 packages,
**+334 MiB unpacked / +119 MiB compressed** (2950 → 3284 MiB unpacked, 759 → 878 MiB compressed).
Two thirds of that is not avoidable by trimming — `libgbm1`, which Chromium needs, depends on
`mesa-libgallium`, which depends on `libllvm20`. The list is taken from Playwright rather than
hand-written so it cannot rot against Ubuntu 24.04's `t64` renames or a future Chromium dependency,
and the `install-deps --dry-run` that follows it is a build-time assertion: on a platform
Playwright has no list for, `install-deps` installs nothing and still exits 0.

**Browser binaries are deliberately not baked.** They are large, they are version-coupled to
whatever Playwright the user installs, and they already persist: `~/.cache/ms-playwright` is inside
the home volume, so a downloaded browser survives container recreation *and* base-image migration.
The libraries are the opposite — a runtime `apt-get install` lands in the container's writable
layer, is re-paid after every Reset, and is lost on migration (which replays apt from a manifest
against the new base). Baking one and not the other puts each half where it already persists.

**`/home/claude` in the image is seed-only.** It is the mount point of the `triple-c-home-{projectId}`
volume, so after a project's *first* start the image's copy of that directory is masked permanently.
A change made under `/home/claude` in the Dockerfile reaches **new projects only** — with or without
a base-image migration. Anything that must stay upgradable belongs in `/usr/local/bin` or `/opt`, or
must be seeded by `entrypoint.sh` on every start.

**Default user**: `claude` (UID/GID 1000, remapped by entrypoint to match host)
