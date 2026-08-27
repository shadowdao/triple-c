# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

Triple-C (Claude-Code-Container) is a Tauri v2 desktop application that sandboxes Claude Code inside Docker containers. It has two main parts: a React/TypeScript frontend, a Rust backend, and a Docker container image definition.

## Build & Development Commands

All frontend/tauri commands run from the `app/` directory:

```bash
cd app
npm ci                    # Install dependencies (required first time)
npx tauri dev             # Launch app in dev mode with hot reload (Vite on port 1420)
npx tauri build           # Production build (outputs to src-tauri/target/release/bundle/)
npm run build             # Frontend-only build (tsc + vite)
npm run test              # Run Vitest once
npm run test:watch        # Run Vitest in watch mode
```

Rust backend is compiled automatically by `tauri dev`/`tauri build`. To check Rust independently:
```bash
cd app/src-tauri
cargo check               # Type-check without full build
cargo build               # Build Rust backend only
```

Container image:
```bash
docker build -t triple-c-sandbox ./container
```

### Linux Build Dependencies (Ubuntu/Debian)
```bash
sudo apt-get install -y libgtk-3-dev libwebkit2gtk-4.1-dev libayatana-appindicator3-dev librsvg2-dev libsoup-3.0-dev patchelf libssl-dev pkg-config build-essential
```

## Architecture

### Two-Process Model (Tauri IPC)

- **React frontend** (`app/src/`) renders UI in the OS webview
- **Rust backend** (`app/src-tauri/src/`) handles Docker API, credential storage, and terminal I/O
- Communication uses two patterns:
  - `invoke()` — request/response for discrete operations (CRUD, start/stop containers)
  - `emit()`/`listen()` — event streaming for continuous data (terminal I/O)

### Terminal I/O Flow

```
User keystroke → xterm.js onData() → invoke("terminal_input") → mpsc channel → docker exec stdin
docker exec stdout → tokio task → emit("terminal-output-{sessionId}") → listen() → xterm.js write()
```

### Frontend Structure (`app/src/`)

- **`store/appState.ts`** — Single Zustand store for all app state (projects, sessions, UI). The
  main area is a single ordered tab strip holding two tab kinds, keyed `term:<id>` and
  `home:<id>`; `activeSessionId` is *derived* from `activeTabKey` so exactly one thing is current.
  `tabOrder` is user-reorderable (drag, or `Ctrl+Shift+←/→` via `moveActiveTab`) — so **never
  treat a tab's position as identity**: address tabs by key, and index only through `tabOrder`.
  `moveTab` deliberately does not activate what it moves.
  - **The tab drag is pointer events, not HTML5 drag-and-drop, and must stay that way.** Tauri's
    `dragDropEnabled` blocks HTML5 drag inside the webview on Windows, and it cannot simply be
    turned off: `TerminalView` needs Tauri's native drag-drop event because it is the only one
    that carries dropped *file paths*. An HTML5 drag also carries a `DataTransfer`, which the
    default handler types into any text field the drag is released over.
  - **A new app-level shortcut must not swallow a text-editing chord.** `useKeyboardShortcuts`
    binds on `document` in the capture phase, so `inTextField()` guards the arrow bindings —
    excluding xterm's helper textarea, which is an input-method shim rather than a field.
- **`hooks/`** — All Tauri IPC calls are encapsulated in hooks (`useTerminal`, `useProjects`, `useDocker`, `useSettings`)
- **`lib/tauri-commands.ts`** — Typed `invoke()` wrappers; TypeScript types in `lib/types.ts` must match Rust models
- **`components/terminal/TerminalView.tsx`** — xterm.js integration with WebGL rendering, URL detection for OAuth flow
- **`components/layout/`** — TopBar, MainTabs (the unified tab strip), Sidebar, StatusBar
- **`components/projects/`** — `ProjectRow` (select-only list row), `ProjectList`, `AddProjectDialog`,
  and the editors reused by Project Home
- **`components/projects/home/`** — **Project Home**, the main-area view for a project:
  Overview / Sessions / Automation / Config / Files. Per-project configuration lives here, not in
  modals — see "UI conventions" below.
  - **The Files pane's host transfers open their dialog from Rust, and that is the whole
    design — do not move it back into the webview.** The tab browses, views (text and image),
    renames and creates folders inside the container (`list_container_files`,
    `read_container_file`, `rename_container_path`, `create_container_directory`), and it
    copies single files in and out (`upload_files_to_container`, `download_container_file`).
    The second pair call `pick_files_to_upload` / `pick_save_path`, which drive
    `tauri-plugin-dialog` from the *backend*: the webview can ask for a picker and that is the
    entirety of its influence — it cannot name a host path as an *input*. The claim stops
    there and should not be widened: host paths still travel outward in error text, canonical
    ones included. What is closed is the direction that produced the criticals.
    That shape is not decoration. Four successive audits found that host filesystem paths
    crossing IPC were where the criticals lived — a caller-named host destination for
    container-controlled bytes, an arbitrary host source read into the container, a `link(2)`
    upload reservation that succeeded against a directory and failed forever on any filesystem
    without hard links. The feature was removed rather than fixed a fifth time, and it came
    back only in the shape that removes the class: a frontend-driven dialog handing Rust a
    string is the exact thing that failed, so re-introducing `open()`/`save()` in `FilesTab`
    would undo the whole point while looking like a simplification.
    None of the reservation machinery came back with it. There is no destination reservation,
    no placeholder rollback and no collision marker — the OS save dialog already asks about
    overwriting, and Docker's archive extractor overwrites on upload the way `cp` does.
  - **Drag-and-drop is still not it.** There is no drop-into-the-Files-pane and no OS
    drag-out; the buttons are the gesture. A file also gets *in* by being dropped on the
    Terminal, and a whole tree comes *out* through "Back up container" — those two predate the
    Files work and their hardening is not to be weakened. `TerminalView`'s `onDragDropEvent`
    is Tauri's native drop event (window-wide, so routed by `lib/dropTarget.ts` — geometry for
    *whose* drop it is, a document-wide `dropIsBlocked` for whether the app should accept one
    at all; keep both halves and keep `PaneVisibility`). Backup is
    `file_commands::download_container_backup`.
  - **`resolve_host_path` applies the full lexical predicate twice — as written, and again
    after canonicalisation.** That includes the general hidden-component rule, which
    deliberately over-catches: a path resolving through `node_modules/.pnpm`, `~/.cache` or
    `~/.local/share` is refused. Do not narrow it back to a list of "credential" directories.
    That was tried, and allow-by-omission let `~/.local/bin` (write there and you own the
    user's next shell command), `~/.password-store`, browser profiles and `~/.pki/nssdb`
    through a planted symlink with a perfectly visible name. Over-refusing is the cheaper
    mistake. Note the cost is real and has grown: of the four callers, the Files pane's two
    are routine, and their path comes from a dialog — so an over-catch refuses a destination a
    person actually chose (`~/.config` is the common one). Accepted, and not a reason to
    narrow the rule, because the terminal drop and `download_container_backup` still take
    their host path over IPC and this predicate is their only boundary.
  - **OS drag-out is not here.** `tauri-plugin-drag`, `stage_container_file_for_drag` and its
    host staging directory were held back for separate hardening and live on
    `hold/disk-and-dragout`. Do not re-add `drag:allow-start-drag` or a staging command
    without taking that work back whole: the plugin has no scope mechanism, so the grant lets
    a compromised webview start a drag on *any* host path the user can read, and the staging
    directory is a host-temp disk leak with a gesture attached unless its exit-clear and
    startup-reap come back with it.
- **`components/settings/`** — Host-level settings: Docker, AWS, Web Terminal, STT, shared auth.
  There is deliberately **no Disk panel** here. The disk survey and its reclaim / destroy /
  compaction surface were held back for separate hardening and live on `hold/disk-and-dragout`;
  one of their IPC commands was a verified arbitrary-DELETE primitive, so if that work returns it
  returns whole, `generate_handler!` entries and typed confirmations included. The *prevention*
  half stayed and is not disk-panel code: the pre-commit scrub in `docker/container.rs`, capped
  container logs, the `triple-c.base` / `triple-c.managed` labels, `sweep_orphaned_snapshots` and
  the startup housekeeping in `lib.rs`, the migration reapers, and `project_lock.rs`.
- **`components/ui/`** — Shared primitives. **Use these; do not hand-roll replacements.**
  `Modal` (the only correct way to build a dialog — it supplies `role="dialog"`, `aria-modal`,
  focus trap and restore), `Button`, `Toggle`, `Field`, `SegmentedControl`, `StatusIndicator`,
  `SaveIndicator`, `OverflowMenu`, `ToastHost`, `Tooltip`

### UI conventions

- **Project config belongs in Project Home's Config tab, not a modal.** Modals are reserved for
  short, genuinely modal tasks (add project, confirm removal, token acquisition). The app
  previously had ~12 hand-rolled modals; they were consolidated deliberately.
- **Never bypass the design tokens.** All colour comes from CSS custom properties in `index.css`.
  Filled buttons use `--accent-emphasis` (not `--accent`, which fails WCAG AA against white).
  Use `--text-disabled` rather than `disabled:opacity-50`.
- **Never write `focus:outline-none`.** A global `:focus-visible` ring is defined in `index.css`.
- **Status must not be encoded in colour alone** — `StatusIndicator` pairs a glyph with a word.
- Keyboard: `Ctrl+T` new terminal, `Ctrl+Shift+W` close tab, `Ctrl+Tab` cycle, `Ctrl+1..9` jump,
  `Ctrl+Shift+←/→` move the active tab. `Ctrl+W` is intentionally left alone — it is readline's
  `kill-word` inside the terminal, and plain `Ctrl+←/→` is its word-wise cursor motion, which is
  why tab-moving takes Shift.

### Backend Structure (`app/src-tauri/src/`)

- **`commands/`** — Tauri command handlers. These are the IPC entry points called by `invoke()`.
  Beyond docker/project/settings/terminal: `inspect_commands.rs` (read-only views into a
  container — Claude sessions, installed capabilities, scheduler tasks), `auth_bridge_commands.rs`,
  `auth_token_commands.rs`.
- **`auth_bridge/`** — Host-side loopback bridge so browser logins run *inside* a container can
  complete against the host browser. Discovers listeners by parsing `/proc/net/tcp{,6}` (the image
  has no `ss`/`netstat`/`lsof`), binds host `127.0.0.1` **only**, and tunnels in over the Docker
  API via `socat`. Opt-in per project.
- **`browser_view/`** — Watch and take over the browser Claude drives with Playwright inside the
  container. Runs Playwright's own dashboard (`browser.bind()` + `playwright-cli show`) in the
  container and fronts it with a **token-gated** loopback proxy. Deliberately does **not** reuse
  the auth bridge's `PortForward`, which binds an unauthenticated port — fine for a throwaway
  OAuth listener, wrong for remote control of a browser. Host ports are confined to
  `47820..=47827` because CSP `frame-src` cannot express a port range and must enumerate them;
  a unit test asserts the Rust range matches `tauri.conf.json`. Opt-in per project.
  - **`popout.rs` puts the same URL in a second OS window** (`WebviewUrl::External`), so the view
    can be watched on another monitor or pinned on top while the main window is used for work.
    Three things it rests on: no capability lists that window, so it has **no IPC surface** — do
    not give it one; the app CSP does not apply, because it is a top-level document rather than a
    frame, and the token gate is what protects the port in both cases; and the window is owned by
    the *session*, so the supervisor's teardown closes it rather than leaving a window onto a
    viewer that no longer exists. It closes with `destroy()`, never `close()`, to stay clear of
    `CloseRequested`. The pane drops its iframe while popped out — two viewers can both *drive*
    the browser.
  - **`page.rs` opens a page, which is the one thing the pane could not do.** A URL plus a
    viewport: launch a browser in the container, `browser.bind()` it so the pane shows it, and
    keep the handle. Serves auth (the OAuth callback listener is *in* the container, so a
    container-side browser closes the loop with no host round trip and no auth bridge) and dev
    servers on container loopback. **Verified: a second client cannot join a bound browser** —
    `chromium.connect()` against the published endpoint times out in every URL form, because that
    socket speaks the dashboard's transport, not the public connect protocol. So whoever launches
    is the only process that can drive, which is why the helper is resident and why live resize
    applies to pages *we* opened and never to `@playwright/mcp`'s (those take `--viewport-size` /
    `PLAYWRIGHT_MCP_VIEWPORT_SIZE` at launch). Control is a polled JSON file in `/tmp` — no port,
    no second listener — and a re-open with a helper already up *navigates* rather than
    relaunching, so a session signed in on one page survives to the next.
  - **Resizing the window does not resize the page.** The viewer is a CDP screencast: a bigger
    window is the same pixels drawn larger. `page.setViewportSize()` is what reflows (measured
    against a `@media (max-width: 900px)` rule), and match-window mode pushes the pop-out's
    settled `Resized` size into it — debounced by generation counter, since a drag emits
    continuously and each one costs a container exec.
  - **`lib.rs`'s `on_window_event` fires for every window and must stay guarded on
    `label() == "main"`.** Without that guard, closing a pop-out runs the app's shutdown: every
    container stopped, process exited.
  - **Detection has to look past `node_modules`.** `claude mcp add … npx @playwright/mcp@latest`
    installs into `~/.npm/_npx/<hash>/node_modules`, not any `node_modules`, so `detect.rs`
    globs that cache as well as `/workspace`, `$HOME/node_modules` and `npm root -g`. It also
    hops from a wrapper `playwright` to its **nested** `playwright-core`: verified that npm does
    not hoist for global installs, and the wrapper ships no `types/types.d.ts`, so reading the
    wrapper alone reports a current build as "predates `browser.bind()`".
  - **`@playwright/mcp` can never satisfy this pane.** It bundles a `playwright-core` that binds,
    but never `@playwright/cli`, which is the viewer. Never offer it as a setup route — only as
    what binds sessions automatically once Playwright is present.
  - **`install.rs` installs into `/workspace`, as `claude`, with `--no-save`.** `/workspace` is
    *not* a bind mount — project directories are mounted at `/workspace/{mount_name}` — so this
    touches nothing of the user's, needs no sudo (npm's prefix is `/usr`, which is root-owned),
    and is on the module resolution path for scripts in the project. Browsers go to
    `~/.cache/ms-playwright` as `claude`, i.e. the home volume.
  - **Current base images ship Chromium's shared libraries; older ones do not** — and a project
    keeps the base image it was first built from until it is migrated, so "older" is the normal
    case. Without them `playwright install chromium` downloads a browser that cannot launch, which
    is why installing Chrome via apt looks like a fix. `install.rs` asks
    `install-deps --dry-run` first and skips the apt step when the answer is "all present",
    *saying so* in the progress stream. Do not decide this by probing for library names: the
    dry-run simulates the same `apt-get install` the fix would run, so check and fix cannot
    disagree about what the dependency set is. Note that `--dry-run` exits **0** both when
    everything is installed and when Playwright has no list for the platform — match on its
    output, not its exit code. Either way the action ends by *actually launching* the browser to
    verify. `@playwright/mcp` wants the `chrome` **channel** specifically, so both browsers are
    offered.
- **`docker/`** — Docker API layer using bollard:
  - `client.rs` — Singleton Docker connection via `OnceLock`
  - `container.rs` — Container lifecycle (create, start, stop, remove, inspect)
  - `exec.rs` — Attached exec streaming. `create_attached_exec()` is the **single** place an
    attached exec is opened; terminal sessions and the auth bridge both go through it.
  - `image.rs` — Image build/pull with progress streaming
  - `gateway.rs` — Optional LiteLLM sibling container giving Claude Code an Anthropic-format
    front end for providers that only speak OpenAI (see `gateway-container/`). Mirrors `stt.rs`.
    Its bind address is **detected, never `0.0.0.0`** — unlike STT, *project containers* consume
    it, so loopback alone is not always enough: Docker Desktop gets `127.0.0.1` (containers reach
    it via `host.docker.internal`), native Linux gets the default bridge gateway (`172.17.0.1`).
    `GatewayBinding` derives the bind address and the advertised `base_url` together so they
    cannot drift. A wildcard bind would be LAN-reachable — Docker's rules precede host firewalls —
    in front of a container config holding a billed provider key. It also **always** sets a
    LiteLLM `master_key`, since LiteLLM without one accepts any key.
  - `migration.rs` — Base-image migration: manifest capture via throwaway containers, the pure
    delta computation (dpkg-ownership filter, bind-mount exclusion, verbatim-copy set), and the
    crash-recovery state machine. See "Base-image migration" below.
  - `legacy_cleanup.rs` — One-release migration shim removing leftovers from the deleted MCP
    feature (containers labelled `triple-c.mcp-server`, `triple-c-net-*` networks). Deletable once
    users have migrated.
- **`web_terminal/`** — Remote terminal access via axum HTTP+WebSocket server:
  - `server.rs` — Axum server lifecycle (start/stop), serves embedded HTML and handles WS upgrades
  - `ws_handler.rs` — Per-connection WebSocket handler with JSON protocol, session management, cleanup on disconnect
  - `terminal.html` — Self-contained xterm.js web UI embedded via `include_str!()`
- **`models/`** — Serde structs (`Project`, `Backend`, `BedrockConfig`, `OllamaConfig`, `LlamaCppConfig`, `OpenAiCompatibleConfig`, `ClaudeCodeSettings`, `ContainerInfo`, `AppSettings`, `WebTerminalSettings`). These define the IPC contract with the frontend.
- **`storage/`** — Persistence: `projects_store.rs` (JSON file with atomic writes), `secure.rs` (OS keychain via `keyring` crate), `settings_store.rs`

### Container (`container/`)

- **`Dockerfile`** — Ubuntu 24.04 base with Claude Code, Node.js 22, Python 3.12, Rust, Docker CLI, git, gh, AWS CLI v2, ripgrep, pnpm, uv, ruff pre-installed, plus the shared
  libraries a browser links against (see below) and the VPN tooling the `vpn_support_enabled`
  toggle grants capability for (`iproute2`, `wireguard-tools`, `iptables`)
- **Browser runtime libraries are baked in; browser *binaries* are not.** A layer runs
  `npx --yes playwright@latest install-deps chromium` as root, so Playwright names its own
  dependencies and the list cannot rot against Ubuntu 24.04's `t64` renames or a new Chromium
  dependency. Measured: +99 packages, +334 MiB unpacked / +119 MiB compressed, on both arches. Do
  not replace it with a hand-written apt list without pinning the Playwright version you derived
  it from — a `chromium`-only list saves ~94 MiB (Playwright's `tools` group: xvfb and the CJK
  fonts) and nothing more, because `libgbm1` → `mesa-libgallium` → `libllvm20` is ~213 MiB that
  no trimming removes.
  - The `install-deps --dry-run` call after it is a **build-time assertion, not decoration**: on a
    platform Playwright's table does not cover, `install-deps` prints a warning and returns having
    installed nothing **with exit status 0**. Without the assertion that ships a broken image
    behind a clean build log.
  - Baking the libraries but not the browsers is the whole point of the split. Browsers live in
    `~/.cache/ms-playwright` (home volume) and already survive recreation *and* migration; a
    runtime `apt-get install` of the libraries lands in the writable layer, is re-paid after every
    Reset, and is **lost on base-image migration**, which replays apt from a manifest. The runtime
    approach converges on the worst state: a 400 MB browser present with its libraries gone.
  - The layer sits immediately after Node (npx is its only prerequisite) and well above the shim
    `COPY`s, so editing a shim does not re-run a multi-hundred-megabyte apt install.
- **`entrypoint.sh`** — UID/GID remapping to match host user, SSH key setup, git config, docker socket permissions, Claude Code settings.json injection, then `sleep infinity`
- **`triple-c-scheduler`** — Bash-based scheduled task system for recurring Claude Code invocations

**`/home/claude` in the image is seed-only.** It is the mount point of the named volume
`triple-c-home-{projectId}`, so after a project's *first* start the image's copy of that directory
is masked permanently and can never be updated again. A change you make under `/home/claude` in
the `Dockerfile` or in `entrypoint.sh`'s "copy this into the home dir" style reaches **new
projects only** — existing ones will never see it, with or without a base-image migration.

So: **anything that must stay upgradable belongs in `/usr/local/bin` or `/opt`, or must be seeded
by `entrypoint.sh` at runtime** (i.e. written on every start, from a source outside the home
volume, the way `CLAUDE_INSTRUCTIONS` → `~/.claude/CLAUDE.md` and the Mission Control skill copy
already are). Putting it in the image's `/home/claude` and expecting an image update to deliver it
is the mistake.

The flip side is the useful half of the same fact: Claude Code itself (`~/.local/bin`), cargo, uv,
ruff, the OAuth login, `~/.claude.json`, skills, transcripts, scheduler tasks and SSH keys all
re-attach for free when a container is recreated from a *different* image — which is what makes
base-image migration cheap.

### Corporate CA certificates (`docker/ca_certs.rs`, `entrypoint.sh`)

A global `AppSettings::ca_cert_path` with a per-project `Project::ca_cert_path` override, accepting
a single certificate file **or** a directory. Follows the SSH/AWS host-mount pattern: read-only
bind mount at `/tmp/.host-ca`, applied by the entrypoint on every start, so it survives recreation,
migration and Reset. Four things here are not obvious:

- **`update-ca-certificates` globs `*.crt`, case-sensitively.** A `.pem` that is merely copied into
  `/usr/local/share/ca-certificates/` is ignored in total silence. Certificates are *renamed* —
  `container_cert_name()` in Rust, mirrored in a few lines of shell in `entrypoint.sh` (the Rust
  side carries the unit tests). A single-file mount lands at `/tmp/.host-ca/<name>.crt` so the
  entrypoint only ever sees a directory and the file keeps a recognisable name.
- **The system store is not enough.** Only curl/git/apt read it. Node — and therefore Claude Code
  itself — needs `NODE_EXTRA_CA_CERTS`; Python/requests need `REQUESTS_CA_BUNDLE`/`SSL_CERT_FILE`;
  Chrome/Chromium read neither and want their own NSS database at `~/.pki/nssdb`, seeded with
  `certutil` (`libnss3-tools`, added to the image for this). The NSS step warns and continues if
  `certutil` is missing rather than failing the start.
- **Those env vars are set from Rust at creation, never exported by the entrypoint.** A terminal
  session is a `docker exec`, which inherits the container's configured env and sees nothing the
  entrypoint exported — the same lesson that made `$BROWSER` an image-level `ENV`. The bundle path
  is deterministic (`/etc/ssl/certs/ca-certificates.crt`), so Rust can set them up front. They are
  emitted **empty** when no CA is configured, for the `MANAGED_AUTH_KEYS` reason: `docker commit`
  bakes env into the snapshot image. Empty is safe — verified on Ubuntu 24.04 that curl, `openssl
  s_client` and Python's `ssl` behave exactly as with the vars unset.
- **`triple-c.ca-fingerprint` covers the certificate *bytes*, not just the path.** Replacing a
  rotated CA at the same location must recreate the container; the copy inside is made once, at
  start, so nothing else would notice. The entrypoint is stamped/idempotent on restart, and
  actively **removes** `triple-c-*.crt` when the setting is cleared — `/usr/local/share` rides the
  project's snapshot image, so turning the feature off has to undo, not merely stop.

### VPN support (`vpn_support_enabled`, `docker/container.rs`)

An opt-in per-project switch granting the container what a VPN client needs to build a tunnel.
`vpn_host_config()` is the single definition of what that means, and it is unit-tested because a
container is created once by a very long function where a dropped capability is invisible.

- **All three pieces or none.** `CAP_NET_ADMIN` (Docker's default set has `net_raw` but *not*
  `net_admin`, so a client can ping but never connect), the `/dev/net/tun` device (absent
  entirely from a default container — nothing to open even with the capability), and
  `net.ipv4.conf.all.src_valid_mark=1` (WireGuard's `wg-quick` sets it and cannot from inside a
  container, since `/proc/sys` is read-only, so handshake packets die to reverse-path filtering).
  Any two without the third still presents as a connection that hangs to a timeout, which is why
  the tests assert the whole set.
- **The device is passed through from the host, never `mknod`-ed inside.** The kernel's `tun`
  module has to back it.
- **A missing device fails at `start`, not `create` — verified against Docker 29.7.** `docker
  create --device /dev/does-not-exist` succeeds and prints an id; runc resolves the device (and
  validates sysctls) only when it builds the container. So the guard belongs on the start path:
  `explain_container_failure()` covers both and is called from `start_container`, where it has a
  container id and no project — which is why it keys off the error naming `/dev/net/tun` rather
  than off `vpn_support_enabled`. Nothing else in Triple-C requests a device, so that is
  unambiguous. A version of this check wired to `create` alone is dead code that looks correct.
- **`NET_ADMIN` here is not user-namespaced.** Docker does not enable userns remapping by default,
  so only the *network* namespace confines it: no reach onto host interfaces, but promiscuous
  mode, arbitrary addresses/routes/NAT on the shared `docker0` segment (sibling containers, the
  LiteLLM gateway among them, are ARP-spoofable), netlink-triggered host module auto-load, and
  enough authority to flush in-container netfilter rules that sandbox mode may rely on. Keep the
  code comments honest about this — an earlier draft claimed it "confers no authority" outside the
  container, which is too strong.
- **`triple-c.vpn-support` is written unconditionally, including `false`.** The usual
  `docker commit` reason: a `true` stamped once would ride the snapshot image into every future
  container and make the switch impossible to turn off.
- Off is byte-identical to a container created before the feature existed, and a missing label
  reads as `false`, so no existing project is churned.
- **The toggle grants capability and stops there — it routes nothing.** `vpn_host_config()` returns
  a cap, a device and a sysctl; no client is installed, no route is touched, no tunnel is started
  or restored. Users read the name as "turn the VPN on" and report the default network not routing
  through it as a bug. It isn't, and the docs say so explicitly; keep it that way.
- **The tooling is baked, not installed at runtime.** `iproute2` and `wireguard-tools` are in
  `container/Dockerfile` because a runtime install lands in the writable layer and is lost on
  base-image migration — leaving a project holding the capability with nothing able to exercise it,
  and no error that points at why. `iptables` is included and `nftables` deliberately is not; see
  the Dockerfile comment for why that way round.
- **Anything built on this fails open.** The network namespace is rebuilt on every start and no
  service manager runs inside, so a tunnel never survives stop/start or recreation — while leftover
  `/run` state makes it look as though it did. Note the two different mechanisms: `/run` is in the
  writable layer, so on a stop/start it is simply the same container's files, and on a recreation
  `docker commit` has carried it into the snapshot. Traffic silently reverts to the real address.
  Any future autostart or killswitch work starts here.
- **`/run` riding the snapshot means a VPN client's key material can end up in an image.** Verified:
  a fresh container off the whp snapshot already contained the `wg.priv` a previous tunnel left in
  `/run`. Anything writing key material there inherits the problem — the same `docker commit`
  hazard as `triple-c.git-token-hash` and the custom-env fingerprint, in a directory that looks
  ephemeral and is not. A VPN client that does this should delete its key on teardown.
- **`iptables` is baked, and picking `nftables` instead would have been wrong.** `Recommends:
  nftables | iptables` is stripped by `--no-install-recommends`, and `wg-quick` needs a backend for
  any `AllowedIPs = 0.0.0.0/0`. `nftables` is the tempting choice — preferred by `wg-quick`, half
  the size — but `wg-quick` picks nft *unconditionally* when present, and its nft ruleset needs
  `nft_fib_ipv4`, which LinuxKit (Docker Desktop for Mac) does not build while it *does* build
  `xt_CONNMARK`. Shipping nftables would therefore have forfeited Mac. See the Dockerfile comment;
  the kernel-config evidence is quoted there.
- **Two `wg-quick` failures remain, and only one is ours to fix.** Full tunnels still need
  `xt_CONNMARK`, which WSL2 before 6.6 lacks — nothing installable changes that. And every
  provider's stock config carries a `DNS =` line that fails in `set_dns()` before any routing, so it
  breaks split tunnels too; `openresolv` has no candidate on noble and `resolvconf` drags in
  systemd-resolved, so that one is documented rather than fixed. Driving `wg` and `ip route`
  directly avoids both, which is what the skill does.
- **The `pia-vpn` skill is installed *and removed* from `VPN_SUPPORT_ENABLED`.** `container/skills/`
  is baked to `/opt/triple-c-skills` and `install_feature_skill()` in `entrypoint.sh` copies it into
  `~/.claude/skills/` on every start — refreshed each time, so a fix reaches any project whose base
  image has the source, and `rm -rf`'d first, so files dropped from a later version do not linger.
  The removal branch matters as much as the install: `~/.claude` is a persisted volume, so a skill
  left behind after the toggle goes off would keep instructing an agent to use a capability the
  container no longer has. Which is also why the variable is sent as `0` rather than omitted (see
  `vpn_env_var`, tested), and why it is in `RESERVED_ENV_EXACT` — a custom env var of that name
  could otherwise claim the skill without the capability behind it.
- **Both halves of that live in the base image, so neither reaches an existing project.** A
  recreation builds from the project's *own snapshot*, which has no `/opt/triple-c-skills` and no
  updated `entrypoint.sh`; only a migration or a Reset delivers them. The install path says so out
  loud rather than returning silently, and `/opt/triple-c-skills` is in `FEATURE_PROBES` so the
  migration pre-flight lists it as missing. Worth knowing before adding anything else behind an
  existing toggle: the label fingerprints *the setting*, not the set of things the setting drives,
  so a project already at `true` gets no recreation at all on upgrade.

### Container Lifecycle

Containers use a **stop/start** model (not create/destroy). Installed packages persist across stops. The `.claude` config dir uses a named Docker volume (`triple-c-claude-config-{projectId}`), nested inside the home volume (`triple-c-home-{projectId}`), so OAuth tokens and Claude Code config survive container stop/start *and* container recreation.

**Reset is the exception and it is destructive.** `rebuild_project_container` calls
`remove_project_volumes`, which deletes *both* volumes — so a Reset wipes `~/.claude`,
`~/.claude.json`, the OAuth credential, installed skills, and session transcripts. That is
intentional (Reset exists to get back to a clean base image), but do not describe Reset as
preserving credentials.

### Base-image migration (`docker/migration.rs`, `commands/migration_commands.rs`)

A container is created from `triple-c-snapshot-{projectId}:latest` whenever that image exists, and
every recreation re-commits it — so without an explicit act, a project stays on the base image it
was first built from **forever** and never picks up a new `socat`, a new `/usr/local/bin` shim or a
security update. Migration is the non-destructive way out; Reset is the destructive one.

- **Staleness is a surfaced signal, not an automatic trigger.** `triple-c.base-image-id` records
  the lineage but is deliberately **not** compared in `container_needs_recreation` — see the long
  comment there. Comparing it would recreate every project *from its own snapshot* on the next base
  bump: churn on the old base, and it would consume the "you should migrate" signal without
  migrating. `get_container_staleness` surfaces it; `migrate_project_to_base` acts on it.
- **A missing lineage label means "unknown, probe instead", never "stale".**
- **`:latest` keeps pointing at the old lineage until the final commit.** That is what makes every
  crash before that point self-heal — `start_project_container` just recreates from the old
  snapshot. After the container swap, the new container's `triple-c.migration-state=in-progress`
  label plus the persisted state file let `reconcile_project_statuses` offer resume or rollback.
- **Rollback restores the system layer only.** The volumes are never touched at any point, so work
  done in `$HOME` during a migrated session survives a rollback. Say so in any UI copy.
- **`/var` is never copied either, and that is the one way migration is *more* destructive than
  the ordinary recreate.** A recreate builds from the project's snapshot, so `/var/lib/postgresql`
  rides along; a migration builds from the base and the apt replay hands back an empty cluster.
  Copying a live database's files onto a different base's version of the same package is a
  corruption risk, not a fix — so the answer is disclosure. `unpreserved_data()` reports
  first-level directories under `/var/lib` and `/var/www` that the base does not ship *and* that
  hold non-dpkg-owned files (which is what keeps `/var/lib/apt` and `/var/lib/dpkg` out of it),
  and the pre-flight, the banner and the finished report all name them. Do not make this silent.
- **The rollback pin is not best-effort.** After `commit_container_snapshot` the commit is the only
  copy of the old system layer, so a `docker tag` that fails — or succeeds without the reference
  resolving — aborts the migration before `remove_container`. Same rule in reverse for
  `rollback_migration`: the image is confirmed to exist before the container is destroyed.
- **`resume` must check the container's `triple-c.migration-state` label**, exactly as
  `reconcile_migration` does. Without it a record left behind by a failed commit "resumes" into
  the *old, unmigrated* container and commits it as migrated.
- **Anything that stops, removes or recreates a project's container consults
  `migration_commands::is_migrating`.** The window between `remove_container` and the create that
  follows looks exactly like "no container" to Start, and Reset would delete the volumes out from
  under a live run.
- **`/etc` is never copied**, only reported: the snapshot lineage has
  `/etc/apt/sources.list.d/nodesource.sources` where the current base has `nodesource.list`, and
  having both breaks every `apt-get update` on a duplicate source. Verified, not theoretical.
- **`docker diff` is useless here** — on a snapshot-derived container it reports only changes since
  the last commit. Migration diffs two filesystem manifests instead, filtered through dpkg
  ownership and presence-in-the-new-base. Measured on a real project, that turns 8,677 raw path
  differences into 2 genuinely user-authored ones.

### Authentication

Per-project, independently configured:
- **Anthropic (OAuth)** — `claude login` in terminal, token persists in config volume
- **AWS Bedrock** — Static keys, profile, or bearer token injected as env vars
- **Ollama** — Connect to a local or remote Ollama server via `ANTHROPIC_BASE_URL` (e.g., `http://host.docker.internal:11434`)
- **llama.cpp** — Connect to a local or remote `llama-server` via `ANTHROPIC_BASE_URL` (e.g., `http://host.docker.internal:8080`, its default port)
- **OpenAI Compatible** — Connect through a gateway implementing the **Anthropic Messages API** (LiteLLM) via `ANTHROPIC_BASE_URL` + `ANTHROPIC_AUTH_TOKEN`

**Claude Code only ever speaks the Anthropic Messages API** (`POST /v1/messages?beta=true`) to
`ANTHROPIC_BASE_URL` — never OpenAI's `/v1/chat/completions`. Ollama and llama.cpp implement
`/v1/messages` natively, which is why each gets a plain base-URL backend with no translation shim.
A server that only exposes an OpenAI-shaped API does not work behind any backend.

For every backend pointing at a custom endpoint (`Backend::uses_custom_endpoint`), all four
`ANTHROPIC_DEFAULT_{OPUS,SONNET,HAIKU,FABLE}_MODEL` vars are pinned to the backend's configured
model id, with an optional per-backend Haiku override. Without this, Claude Code's background
calls resolve `haiku` to an Anthropic model id the local server does not have and fail silently.
Anthropic and Bedrock deliberately keep Claude Code's own defaults.
`ANTHROPIC_SMALL_FAST_MODEL` is deprecated and must not be used.

## Styling

- **Tailwind CSS v4** with the Vite plugin (`@tailwindcss/vite`). No separate tailwind config file.
- All colors use CSS custom properties in `index.css` `:root` (e.g., `--bg-primary`, `--text-secondary`, `--accent`)
- `color-scheme: dark` is set on `:root` for native dark-mode controls
- **Do not** add a global `* { padding: 0 }` reset — Tailwind v4 uses CSS `@layer`, and unlayered CSS overrides all layered utilities

## Key Conventions

- Frontend types in `lib/types.ts` must stay in sync with Rust structs in `models/`
- Tauri commands are registered in `lib.rs` via `.invoke_handler(tauri::generate_handler![...])`
- `capabilities/default.json` grants permissions for **plugin** commands only (`core:`, `dialog:`,
  `store:`, `opener:`). Application commands registered through `generate_handler!` do **not**
  need an entry there — adding one is not required and none exists for any app command.
- The `projects.json` file uses atomic writes (write to `.tmp`, then `rename()`). Corrupted files are backed up to `.bak`.
- **Adding project state that changes the container?** `container_needs_recreation()` is entirely
  **label-based** — it does not diff the container's env. If a new setting affects the container's
  environment or configuration, you must also write a corresponding `triple-c.*` label at creation
  and compare it there, or the change will silently not take effect until some unrelated setting
  forces a rebuild. Never put a secret in a label; labels are readable via `docker inspect`.
  (`triple-c.base-image-id` is the one deliberate exception — it is written but not compared; the
  reasoning is in the comment beside the check.)
- **Always write a `triple-c.*` label explicitly, even when the value is empty.** Docker merges an
  image's labels into a container's at creation, and `docker commit` copies container labels onto
  the snapshot image — so a label stamped once rides that snapshot into *every* future container
  forever. Verified on this host, and it is not hypothetical: `triple-c.mcp-fingerprint` has not
  been written by any code since the MCP feature was removed, yet a snapshot image was found still
  carrying a non-empty one, which made its one-shot recreation shim recreate that project on every
  single start. Writing the key explicitly overrides the inherited value — the same defence
  `MANAGED_AUTH_KEYS` applies to env vars.
- **New model fields need an explicit serde default when the correct default isn't the zero value.**
  `#[serde(default)]` on a `bool` yields `false`; follow the `default_full_permissions` pattern in
  `models/project.rs` for anything that should default to true.
- Cross-platform paths: Docker socket is `/var/run/docker.sock` on Linux/macOS, `//./pipe/docker_engine` on Windows

## Secrets

**`scripts/scan-secrets.sh` refuses a commit that adds something shaped like a live
credential.** Enable the hook once per clone with `npm run hooks` (from `app/`), which sets
`core.hooksPath` to `.githooks`. A repository cannot configure its own hooks path — cloning it
would then be enough to run its code — so this is opt-in everywhere, and `--no-verify` skips it.
The `Secret Scan` workflow is the half nobody can bypass; it carries **no `paths:` filter**, on
purpose, because the incident that prompted all this lived in `app/**` and `build.yml` only runs
for `container/**`.

Three rules, and the second half of the third is what keeps it usable: vendor-prefixed tokens
(`ghp_`, `sk-`, `AKIA`, `xox`, …), `BEGIN … PRIVATE KEY` blocks, and an opaque literal assigned to
a secret-shaped name. That last one needs **both** halves — the identifier must read as a
credential *and* the whole literal must be hex or base64 with no word structure. Name-proximity
alone flags `secure::get_project_secret(&id, "aws-secret-access-key")`, which is a keychain key
name; the literal test is what excludes it. Measured against the tree: 0 false positives, and it
catches the real incident (`9b2f4fe`) when replayed.

A line ending `pragma: allowlist secret` is skipped. Make a fixture obviously fake before reaching
for it.

**Why this exists:** `the_custom_env_fingerprint_never_carries_the_value` used the maintainer's
real Gitea **site-admin** token as its fixture — a test about secrets not escaping, leaking one. It
survived 92 commits and fourteen days in the public GitHub mirror, past five audit rounds and two
independent reviews, because every one of them read the code under change and this sat in a test
nobody had reason to open. Fixtures are never live values; there is no case where they need to be.

## Settings export/import

`commands::settings_export_commands`, `storage::settings_crypto`, `models::settings_export`
(triple-c#35). Exports the *host* environment — global `AppSettings` (already the non-secret
shape persisted to `settings.json`) plus the global secrets that live in the OS keychain instead:
the shared Claude Code OAuth login and the model gateway's two keys. Per-project settings,
per-project secrets, and anything in a project's Docker volumes are deliberately out of scope —
this is not a project backup.

- **Encrypted because it can carry live credentials, not for appearance's sake.** Argon2id derives
  a 256-bit key from the user's password (memory-hard — meaningfully resistant to GPU/ASIC
  brute-forcing, unlike PBKDF2 at any reasonable iteration count), AES-256-GCM does the actual
  encryption. A wrong password fails GCM's authentication tag rather than producing silent
  garbage. The salt and nonce are not secret and are written in the clear in the file's own
  header — the salt's job is only to make two exports of the same password derive different keys,
  and the nonce's only requirement is per-encryption uniqueness, which a fresh random draw on
  every export already gives it.
- **The save/open dialogs are opened from Rust**, the same boundary `file_commands.rs`'s
  `pick_save_path`/`pick_files_to_upload` draw and document at length: a frontend-driven dialog
  handing Rust a host path string is the exact shape of bug that produced this app's past
  criticals. `preview_settings_import` resolves the chosen path itself and remembers it
  (`AppState::pending_settings_import`) so `apply_settings_import` re-reads the same file without
  a path ever crossing back over IPC.
- **The password is re-entered, not cached, between preview and apply.** Nothing here holds
  decrypted plaintext — secrets included — in memory for longer than one command's execution.
  `preview_settings_import` returns counts and presence flags only (`SettingsImportPreview`),
  never a secret value, so it's safe to hand to the frontend and render directly.
- **Import replaces settings wholesale, but only writes secrets actually present in the file.**
  An import is "restore this environment," so the settings half is a full replace, not a
  field-by-field merge. Secrets are different on purpose: an absent secret in the export means
  "the source machine never had this configured," not "delete this on import" — a user who wants
  to clear a secret already has dedicated UI for that (signing out of shared auth, clearing the
  gateway key).

## Testing

Frontend tests use Vitest with jsdom environment and React Testing Library. Setup file at `src/test/setup.ts`. Run a single test file:
```bash
cd app
npx vitest run src/path/to/test.test.ts
```
