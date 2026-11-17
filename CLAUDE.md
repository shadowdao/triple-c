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
npx tauri build           # Production build (outputs to src-tauri/target/release/bundle/);
                          # run ../scripts/build-ssm-plugin.sh first (bundled AWS plugin, see Packaging)
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
  - **Copy paths** (`copyCleanup.ts`, `osc52.ts`). OSC 52 base64 is UTF-8 *bytes* — never hand
    `atob()`'s output to the clipboard (that was the `â€™` mojibake); OSC 52 text then gets only
    `sanitizeInvisibles`. Ctrl+Shift+C / "Copy trimmed" run `cleanSelection`, which also undoes
    Claude Code's hard wraps (it wraps its own output and moves the cursor, so rows are never
    `isWrapped`) using `term.cols` and the selection's start column. Ctrl+Shift+Alt+C stays raw.
- **`viewer/`** — the terminal file viewer's window (second Vite entry `viewer.html` →
  `src/viewer/main.tsx`; CodeMirror 6). `lib/filePathLinks.ts` decides what a path is;
  `components/terminal/filePathLinkProvider.ts` registers it with xterm. The OSC 8 handler now
  runs with `allowNonHttpProtocols` on and dispatches `file:` to the viewer, so every other scheme
  must be refused *there*. `viewer.html` must never carry an inline `<style>` — Tauri would add a
  style nonce and CodeMirror's injected styles would stop applying. A missing or broken
  `viewer.html` Vite entry is not caught by Tauri at build time — both Vite dev and Tauri's asset
  lookup silently fall back to `index.html`, so the window just opens the *main app*, full UI and
  all, with no error anywhere; `file_viewer::tests::the_viewer_entry_exists_and_is_a_vite_input`
  in `file_viewer/mod.rs` is the only thing pinning this.
  **Claude Code wraps its own output, so xterm's `isWrapped` misses most wrapped paths.** A word
  longer than the line (a `/tmp/claude-<uid>/…` scratchpad path) is broken mid-word and the rest
  drawn on the next row with cursor moves. `lib/xtermLineJoin.ts` bridges that when given `cols`
  (row runs to the edge, next row continues with path characters, and the two runs could not
  have fit on one line — the last test is what keeps prose from being glued together).
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
- **`file_viewer/`** — one window per click (`file-viewer-<n>`), a managed `ViewerRegistry`,
  resolution by probing `/workspace/<p>` then `/workspace/<mount>/<p>` in one exec as `claude`
  (`~/p` is `/home/claude/p`; a relative miss gets a second exec at `/home/claude/<p>`, `/tmp/<p>`),
  polling by `sha256sum`, saves staged in `/tmp` and swapped in by a `sh` script as the container
  user (spec §5 says why the archive API never writes to the target directory). Commands take
  `window: tauri::Window`, gate on the label and act on the caller's own registry entry — no
  viewer command accepts a path. Which window may *call* each command is the ACL's job: the
  `file-viewer-*` capability grants exactly the five `viewer_*` commands (see `build.rs`).
- **`build.rs` + `src/command_census.rs`** — the build declares a Tauri `AppManifest` from the
  `generate_handler!` list and refuses to build unless every command has exactly one bare
  `allow-*` grant in the capability file its name says it belongs to. The parser and rules are
  in `command_census.rs`, compiled into both the build script and the test build, so they are
  unit-tested; `the_generated_app_manifest_matches_the_handler_list` reads back what tauri
  embedded. Design: `docs/superpowers/specs/2026-09-22-app-manifest-lockdown-design.md`.
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
  - `exec.rs` — Docker's implementation of everything `host::ContainerTarget` does inside a
    container. `create_attached_exec_spec()` is the **single** place an attached exec is opened.
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
- **`host/`** — Where a project's container runs, and the only way into it. `ContainerTarget`
  (`Docker`, and `Agent` — reached through the in-container `triple-c-agent` by `agent.rs`, per
  `docs/superpowers/specs/2026-11-13-triple-c-agent-protocol.md`; AWS host, designed in
  `docs/superpowers/specs/2026-11-13-aws-ecs-host-design.md`)
  wraps execs, file transfer and `is_running`; `sessions.rs` holds `ExecSessionManager`.
  `ssm.rs` is the AWS transport to an `Agent` target: one SSM port-forward per task, run through
  AWS's `session-manager-plugin` as a child process and reopened **on the same loopback port**
  when it drops, so an `AgentEndpoint` stays valid. The `StartSession` token goes to the plugin
  through its environment, never argv, and the plugin's environment is stripped of every
  inherited `AWS_*` variable before the session's own credentials are added.
  `aws_host.rs` is `ProjectHost::Aws` (an ECS task per project, Fargate or, on the user's own instance, EC2; a running task is
  attached to with the token read back from its secret, never rotated; a task a failed start ran
  is stopped again) and `tunnels.rs` the registry of open tunnels by project — an AWS project's
  `ContainerTarget::for_project` is its tunnel's endpoint, its stored `container_id` is that
  endpoint's URL, and the AWS death watch confirms exits through the registry. Shutdown closes
  every tunnel (`for_project` is `None` while none is open; `for_project_waiting` covers the
  background re-attach after an app restart). AWS SDK calls stay in `aws/` (`runtime.rs`,
  `tasks.rs`, `workspace.rs`).
  `port_forwards.rs` is an AWS project's port mappings: each TCP mapping is a **loopback-only**
  auth-bridge `PortForward::bind_mapping` over socat-over-attach. Two deliberate differences from
  Docker: it reaches the workspace's **loopback** (Docker publishes to eth0), so the agent port,
  the browser-view ranges and — with `gateway_sidecar` on — the sidecar's 4000 are refused; and it
  has **no gate** (no first-byte wait, no cross-site check, unlike the bridge). Validated on save
  (rows the save adds/edits; everything when newly on AWS or the sidecar is switched); a stored row
  that would be refused is skipped with a warning at connect. Opens are serialised per project and
  re-read the stored mappings under that lock; unchanged forwards are kept with their connections.
  Opened after every connect and on a save while connected; closed by every `tunnels::close*`
  path. A failing mapping is a `workspace-warning` (`topic: "ports"`), and so is a Running project
  whose mappings could not be opened at all (no tunnel / tunnel replaced) — never silent; a stored
  row missing a port (0) is skipped with one too ("incomplete, both ports are needed").
  `PortMappingsEditor` saves only complete rows (half-filled ones stay local, marked), adopts a
  stored value only when it differs by value and no field in it has focus, and uses word
  placeholders — a "8080" placeholder once read as a typed 8080 → 8080 that was saved as 0 → 0. In the
  workspace each connection picks `127.0.0.1` or `[::1]` from `/proc/net/tcp{,6}`
  (`tunnel::mapping_command`; no probe connection), and the first socat "Connection refused" per
  mapping is a `ports` warning ("nothing is listening on port N"). An idle listener makes no
  agent request; a connection carrying traffic is activity (keeps the workspace from idling).
  **`persistent.rs` is persistent AWS terminals** (Docker projects never reach it): every tab on an
  AWS project — Claude and bash, desktop and web terminal — runs its **unchanged** launch command
  (update prelude and all, or `bash -l`) inside **its own tmux server**,
  `tmux -L triple-c-<session id> -f /etc/triple-c/tmux.conf new-session -A -s <claude|bash> -- <cmd>`.
  Rules, each for a reason:
  - **One server per tab, never one shared server**: a server takes its environment from the
    client that started it, so each tab keeps exactly its own `ExecSpec.env` (copy-at-connect
    `terminal_env` pairs). Never `new-session -e` — that puts the values on argv (`ps`). A later
    connect's new credentials do not reach a session already running (like any running process).
    The client gets `TERM=xterm-256color` (the agent would default a tty to `xterm`, 8 colours).
  - **The tab's session id is the tmux key** (validated `[A-Za-z0-9-]{1,64}`), remembered per
    project in `storage/terminal_store.rs` (`terminal-sessions.json` beside `projects.json`; a
    hint pruned by the workspace's own listing, cleared on Stop/Remove; not a `Project` field, for
    the reason notes aren't). Same id after a restart, so `renamed_session_names` still applies.
  - **Closing a tab ends the session** (`kill-server`); app exit, a dropped tunnel and the agent's
    SIGHUP on a vanished client only **detach** (the server daemonised out of the client's
    process group). **Reattach is `attach-session`, never `new-session -A`**: a session that
    ended while the app was away must not come back as a fresh `claude` (and the update prelude
    runs once, inside, never again on reattach).
  - **Stop ends every tmux server** (`persistent::END_ALL_SCRIPT`: the panes' pids are taken
    before `kill-server`, then a bounded 5 s wait for them and the servers) in the same
    `close_first` step that closes the tabs, **before** the stop guard's push snapshot. The
    entrypoint's TERM hook (`aws_end_terminals`) and the idle watcher's lifetime-cap push run the
    same script verbatim (a test pins all three). The store is cleared on Stop, Remove, a move to
    Docker and a death-watch stop.
  - **The probe** (`command -v tmux` + the config) runs once per tunnel, cached by target id and
    forgotten in `tunnels::register*`, and is **three-state**: yes → persistent; a real no (an
    image without tmux) → the old launch plus a one-time notice; **unanswered → `Err`**, never
    "no" — read as no, a listing would prune every remembered tab and a reattach would end tabs
    whose Claude still runs. Nothing is pruned or ended on an `Err`; the frontend asks again.
    Every tmux exec except the attach goes through `exec_oneshot_background` (never idle-stop
    activity). `TMUX`/`TMUX_TMPDIR` are dropped from the client env and reserved custom names.
  - **A tab closed while disconnected** is queued (`terminal_store` `pending_end`) and ended at the
    next connect (`list_workspace_terminals` runs it first); a reattach of a closed or queued id
    is `Ended`, and a reattach that finishes after its tab closed does not remember it again.
  - **The web terminal's exec ids are `web-<tmux key>`**, never the bare key the desktop uses:
    sharing one `ExecSessionManager` id would let either front end's attach supersede the
    other's exec (no exit callback, a frozen tab) or its close/disconnect end the other's.
  - **Cost, by ruling:** a detached `claude` still counts for the idle watcher — that *is* the
    feature (remote control keeps working with the app closed) — so a Claude left running keeps
    the workspace up and billed until it ends, it is ended (Project Home → Sessions flags
    detached Claude sessions, with End), Stop, or the lifetime cap. HostSection's notes say so
    (`PERSISTENT_TERMINALS_NOTE`). A detached shell does not keep it up.
  - Frontend: a persistent tab reattaches on exit (retrying while the tunnel comes back) and shows
    "[Session ended]" only when the workspace says the session is gone or the project stopped;
    `usePersistentTerminals` reopens this app's sessions once per connection (`id|container_id`)
    without taking focus; a reopened tab calls `reattach_terminal_session` only after its output
    *and* exit listeners are registered (history replay + tmux's first draw would be lost). Project
    Home → Sessions lists every session in the workspace (web terminal and other machines too:
    those are never auto-opened) with Reattach / End. `ExecSessionManager` ends an exec whose
    session id was taken over without its exit callback (else a reattach loops on its own exit).
  - The live check is `host::persistent::live_tests` (ignored; an agent image with tmux,
    `TRIPLEC_PERSIST_E2E_IMAGE`).
  `agent.rs` reuses `docker/exec.rs`'s `OneshotOutput`, host-file reader and uid probe so the two
  variants cannot drift; an `Agent` target's token never appears in `Debug` or error text.
  **Container-interior code must go through `ContainerTarget`, never `docker::exec`, bollard or
  `get_docker()`** — `host::tests::no_container_interior_bollard_outside_the_host_layer` fails
  the build on it. `lifecycle.rs`'s `ProjectHost` owns create/start/stop/find/remove and
  discarding a project's persistent state; the project commands keep only host-neutral steps
  (validation, status, sessions, auth bridge, syncs). `watch.rs` gives `container_watch` its
  death reports and positive-exit check. `no_container_lifecycle_or_bare_docker_targets_outside_the_host_layer`
  fails on lifecycle calls or `ContainerTarget::docker(id)` in non-test code outside `host/` and
  `docker/`; get a target from `ContainerTarget::for_project`, `ProjectHost`, or a session
  (`ExecSessionManager::get_target`). Base-image migration is the one Docker-only exception
  (`DOCKER_ONLY`); shrink it, never grow it.
- **`aws/`** — AWS connections (Settings → Hosts) and the permission check. `connection.rs`
  builds a saved `HostConnection`'s `SdkConfig` **by hand, never with
  `aws_config::defaults().load()`**: a profile is read through `ProfileFileCredentialsProvider`
  for credentials only, so the app's own `AWS_*` variables, `AWS_ENDPOINT_URL*` and a profile's
  endpoint/service settings can't redirect or re-credential a connection's signed requests.
  `identity.rs` is `GetCallerIdentity` and the principal to simulate (an assumed role's ARN is
  recovered with `iam:GetRole`, because SSO roles have an `/aws-reserved/…` path the session ARN
  lacks); `error.rs` classifies SDK errors (SSO expired, access denied naming the action, …);
  `policies.rs` embeds `aws/policies/*.json` (the tier policies — both tiers span several
  files, `policy_files` lists them — and the permissions boundary) and the table of checks the
  simulator is asked; `permissions.rs` runs it
  (`SimulatePrincipalPolicy`, adaptive retries — it throttles — or read-only probes when the
  caller can't simulate itself; a probed run never claims the admin tier). `policies.rs`'s tests
  are the drift guard: every allowed action has a check, every check is allowed by a small IAM
  evaluator (`policy_eval.rs`, test-only; `tofu_modules/tests.rs` also asks it whether the admin
  tier allows every call `MODULE_CALLS` lists for the Terraform modules), owner-scoped checks are
  denied for someone else, admin destruction is tag-scoped, the boundary fences hold, every
  Resource is an ARN and every policy fits a 6,144-character managed policy. **Change a policy
  JSON, its checks and `aws/README.md` together.** The evaluator
  is not IAM: `live_tests.rs` (ignored; `TRIPLEC_AWS_E2E_PROFILE`, `TRIPLEC_AWS_E2E_REGION`,
  admin credentials for a disposable account; `cargo test aws::live_tests -- --ignored
  --test-threads=1`) runs the check and real calls against throwaway `triple-c-e2e-*` roles,
  secrets and keys, and cleans them up. Re-run it after any policy change; the results are in
  `aws/README.md` ("Live verification"). Its `environment` tests apply the real modules as a role
  holding only the admin files (`cargo build` first: the runner they start is the built app
  binary), in a throwaway state bucket.
  **A Fargate workspace's resources** (`workspace.rs`, `tasks.rs`, `runtime.rs`). **No project
  data in a task definition (P1):** every workspace user has `ecs:Describe*` on `*`, so
  `environment`, tags, labels and `overrides` are public to the account. The task definition carries
  `TRIPLEC_HOST=aws`, `COLORTERM`, the three ownership tags, a fingerprint docker label (a hash,
  `FINGERPRINT_LABEL`) and `valueFrom` references into the project's **one secret**
  (`triple-c/<env>/<owner_key>/<project-id>`, JSON, 64 KB cap: files, repos, instructions, settings,
  custom env, the agent token). The token rotates each start; a running task is attached to by reading
  it back (the task role has no Secrets Manager access). **Anyone may register a revision into any
  `triple-c-*` family** (IAM can't scope it), so a revision or task is reused only when it carries the
  caller's own owner tag + this project's tag (+ the fingerprint), and `RunTask` always names the exact
  `family:revision` ARN, never "latest". Names come only from `tofu_modules.rs`, built from `owner_key`
  (`aws:userid` contains `:`). Workspace users can't deregister or delete task definitions
  (`DeregisterTaskDefinition` is `*`-only in IAM): removal leaves revisions ACTIVE for an admin
  (`aws/README.md`). Stop: the stop guard (`commands/stop_guard.rs`) dry-runs `triple-c-wip-push`
  through the agent, closes the project's terminals and auth bridge (so nothing is written after the
  push's snapshot), may push first (under an in-container `timeout`, since the agent does not kill an
  exec the client gave up on), then writes `pushed` or `discard` to `/run/triple-c/stop-reason`
  (root-owned; the entrypoint's TERM hook reads it, and the idle watcher writes `idle` or `lifetime`) so the hook
  doesn't push twice; the marker is cleared if StopTask fails. **Claude Code's sandbox is forced off
  on every AWS project, Fargate and EC2** in the generated settings (`sandbox_allowed` is false; the
  Runtime toggle is locked off, `sandboxEffective` in `awsWorkspace.ts`): bubblewrap fails on Fargate
  and under Docker's default seccomp profile on EC2 too ("No permissions to create new namespace",
  verified live), so a `sandbox_allowed` that said yes would only break every command.
  **LLM gateway sidecar** (`gateway_sidecar`, Q7): a second, `essential: false` container `gateway`
  (`docker::gateway::registry_image_ref`, digest-pinned, amd64+arm64; `memoryReservation` 512; no
  `environment`, no `dependsOn`) whose `secrets` are exactly `TRIPLEC_GATEWAY_{CONFIG,API_KEY,MASTER_KEY}`
  — the workspace container gets every key but those. The config is `sidecar_config` (Docker's
  `render_config` with `os.environ/…` key references, so it holds no key); `/bin/sh -c` writes it to a
  0600 `/tmp/litellm.yaml` and `exec`s LiteLLM on **127.0.0.1:4000** only (the image's own entrypoint
  can't write a file). The master key is **random per task start** (the Docker gateway's never leaves
  the machine); the payload overrides `ANTHROPIC_BASE_URL`/`ANTHROPIC_AUTH_TOKEN` with the sidecar's,
  and under copy-at-connect that token stays in the secret too (R1; terminals don't override it).
  Log streams: `<prefix>/gateway/<task-id>` (the tail reads `workspace` only). Needs the
  OpenAI-compatible backend, a gateway model and ≥ 1 vCPU / 2 GiB; off = the request is unchanged.
  **Cost estimate** (`pricing.rs`, Q5): `pricing:GetProducts` (us-east-1, `AmazonECS`, filters in
  `FILTERS`) → per-region x86/ARM rates cached at `<data_dir>/triple-c/aws-pricing/<region>.json` for
  24 h; `aws_cost_estimate(connection_id, region, config)` never errors — denied/offline/slow (8 s) is
  `None` ("Estimate unavailable"), a failed region isn't re-asked for 5 min, a stale cache still answers.
  EC2 projects (E10) pass `instance_type` (the user's, from `aws_ec2_capacity`): the `AmazonEC2`
  on-demand Linux/shared-tenancy hourly price (`EC2_FILTERS`), cached as `<region>--ec2-<type>.json`
  with the same rules — the instance's price, not the reservations — plus the 50 GiB gp3 root volume
  (`GP3_FILTERS`, `<region>--ebs-gp3.json`, best effort: without it the note says it's missing;
  `EC2_ROOT_VOLUME_GIB` is pinned to `ec2.tf`). A public IPv4 address ($0.005/h) is only in the note.
  **ECS on EC2 (phase 7; `aws/terraform/user-onboard/ec2.tf`, `workspace.rs` `ec2_plan`, `aws_host.rs`).**
  Opt-in per onboarding (`ec2_instance_type`); a project's `launch_type` is `Fargate | Ec2`. Why the
  rules are what they are:
  - **Per-user cluster `triple-c-<env>--u-<tag>`**, named by the principal tag `triple-c-user`
    (`user_ec2_stem` in `tofu_modules.rs`, the only place). One capacity provider per cluster,
    because `PutClusterCapacityProviders` replaces the list and two modules would undo each other on
    a shared cluster. Environment names and tags never contain `--`, so `triple-c-*--u-*` matches
    exactly the per-user clusters. **Two Denies in `workspace-user-tasks.json`** (`RunTask`/`StartTask`
    on a `--u-*` cluster other than the caller's own, and on any when the caller has **no**
    `triple-c-user` principal tag) are the whole isolation between EC2 users: don't loosen them, and
    keep the missing-tag Deny (a variable that resolves to nothing must not fall through). A denial
    maps to `EC2_TAG_MISMATCH` ("different triple-c-user value").
  - **The instance role is scoped by its own cluster tag.** `triple-c-<env>-<owner_key>-instance`
    (owner key, not user tag: role names are account-global) has only the ECS agent's calls, and the
    **boundary** allows them on `cluster/${aws:PrincipalTag/triple-c:cluster}` (must match `--u-*`).
    A Docker-access project is root on its instance and holds that role, so the tag is what stops it
    registering the instance into someone else's cluster. No SSM core and **no log rights** on the
    instance (awslogs writes with the execution role: `ECS_ENABLE_AWSLOGS_EXECUTIONROLE_OVERRIDE`).
    The task and execution roles `PutLogEvents` only into `log-stream:<owner_key>/*` of the shared
    log group, so `log_stream_prefix` must keep starting with the owner key. The boundary
    is versioned (2): an account admin re-applies it once before anyone onboards with EC2.
  - **Launch templates and groups are pinned to a numeric template version, and the admin policy
    denies `$Latest`/`$Default`**: the admin's `iam:PassRole` and `ec2:RunInstances` checks look at the
    template when it is created, and a version edited afterwards would carry any instance type or
    role (a PassRole bypass). The type list (`ec2_instance_spec` in `models/project.rs`) lives in the
    model table, the HCL variable and the policy (`ec2:InstanceType` in the compute file,
    `autoscaling:InstanceTypes` twice in the scaling file); change all together. A template-only group
    create sends no `autoscaling:InstanceTypes`, so don't add a `Null` condition on it (live-verified).
  - **Bridge networking, not awsvpc (R2):** the instance is the user's alone, awsvpc tasks on EC2 get
    no public IP, and bridge keeps IMDS out of reach with hop limit 1 (the agent on the host network
    still reaches it). The ECS agent's introspection port (172.17.0.1:51678) stays reachable;
    accepted. The gateway sidecar is refused on EC2 (no shared loopback).
  - **Cold-start rerun + orphan rule.** ECS turns ECS Exec **off** for a task that has to wait for an
    instance (the agent tunnel needs it), so the first RunTask only brings the instance up; once
    placed, the app stops it, waits for STOPPED and runs again via `find_or_run` (so a concurrent
    start never doubles a task: `START_RACE`), requiring exec on (`EXEC_OFF`). A found exec-off task
    that is still waiting for an instance is another start's (`INSTANCE_RACE`) while younger than
    `ec2_wait + ORPHAN_MARGIN`; older is an orphan, stopped (`REPLACED_REASON`) and replaced. Keep
    both launches going through `find_or_run`.
  - **Docker access = the instance's own socket**, mounted at `/var/run/docker.sock` (root on the
    user's own instance, acceptable only because nobody else's task can land there); VPN =
    `NET_ADMIN` + `/dev/net/tun` + `src_valid_mark`. A Fargate project can't keep either: switching
    to Fargate clears both flags (`launchTypePatch`, R1).
  - **Collision check before an apply** (`ec2_collision_refusal`, `prepare_onboarding`): the module
    would adopt another user's cluster of the same name, so the app refuses a name owned by someone
    else (resources' `triple-c:owner` tag, the user record `/triple-c/users/<env>/<owner_key>` as
    fallback). Admin bundles carry the same check in `README.txt`.
  - Scale to zero (min 0): the first start after ~17 min idle is cold (~3.5-4 min), warm ~20-30 s.
    Re-applying onboarding without EC2 drains and deletes the group even with an instance running.
  **Environments are OpenTofu modules** (`aws/terraform/{environment,user-onboard,permissions-boundary}`;
  spec D2–D10). `tofu_modules.rs` embeds them and is the **only** place the shared names are built
  (environment, `/triple-c/environments/<env>` record, owner key — `sha256("<region>:<aws:userid>")`,
  12 hex, regional because role names are account-global — per-user roles, state bucket
  `triple-c-tfstate-<account>-<region>`, state keys); its tests pin the HCL to those names, the
  versions and tags, and `MODULE_CALLS` (every provider call) to the admin policy.
  `tofu.rs` downloads the pinned OpenTofu (SHA-256 checked **before** extraction), writes a work
  folder (`materialize`), builds tofu's environment from scratch (`child_env`: no inherited
  `AWS_*`/`TF_*`, keys only in the environment) and runs `init` + `apply|destroy` **detached
  (D10)**: the app binary re-executed with `--tofu-runner` (`main.rs` checks that first), in its
  own session, logging to `apply.log`; liveness is the runner's `apply.lock`, never a PID; closing
  the app never stops a run and the next start re-follows the log. `environments.rs` is detection
  (records + state objects + locks; records are untrusted and capped; an empty state, which
  `tofu destroy` leaves behind, is no deployment) and `ensure_state_bucket`;
  `commands/environment_commands.rs` sequences ensure_tofu → ensure_state_bucket → materialize →
  start → follow. **D2: strict tenancy gives each user their own EFS file system** (its policy
  admits only their task role) from their onboarding module; the environment keeps only the shared
  pieces. The app destroys only incomplete deployments (D6); a working environment is removed with
  `tofu destroy` from an admin bundle (D5).
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
- **`TRIPLEC_HOST=aws` mode** (`entrypoint.sh`, the `aws_*` blocks; inert without it). There are no
  bind mounts: `TRIPLEC_FILES` (JSON of base64 files) is decoded into `/tmp/.host-ssh` / `/tmp/.host-ca`
  so the SSH merge and CA install run unchanged, and `TRIPLEC_REPOS` is kept in an unexported shell
  variable; **both are unset before the agent-token block snapshots the environment** (that snapshot is
  every terminal's env and the files hold private keys). Repos clone in the background after the agent
  starts (terminals are usable before they arrive). Hybrid storage mounts EFS at `/mnt/triple-c-home`
  and symlinks `~/.claude`, `~/.claude.json`, `~/.ssh` into it (mounting over `/home/claude` would hide
  the image's tooling); root never follows those links outside the mount (`aws_hybrid_ssh_dir`). EFS storage
  is Hybrid plus a second access point (root `/triple-c-workspaces/<owner_key>/<project>`, a sibling of
  the home roots) mounted straight at `/workspace`; the entrypoint links home state the same way
  (`aws_storage_links_home`) and never creates or chowns `/workspace`. Because `/workspace` then
  outlives the task, `clone_repos` clones into `/workspace/.triple-c-clone/<folder>` and renames
  into place only on success; it never deletes anything at a final path, empties scratch at each
  start, and `triple-c-wip-push` skips scratch.
  Switching EFS → Hybrid/Ephemeral keeps `workspace_access_point_id` (store-owned) until "Remove AWS
  resources" with discard deletes both access points. On
  TERM the hook pushes unsaved work to the run's `triple-c/wip/<run-id>-<folder>` branch via a
  temporary index (the user's index is untouched; needs the task definition's `stopTimeout` raised,
  120 s) unless the stop marker says the app handled it.
  **One WIP branch per task run and folder:** `aws_prepare_run` (right after the agent-token block)
  writes `/run/triple-c/run-id` (`<UTC stamp>-<task id 8 | local>`, task id from ECS task metadata)
  and `start-epoch`; `/run/triple-c` is root 0711 (the stop guard keeps that mode — at 0700 `claude`
  could not read the run id). The folder is sanitised to `[A-Za-z0-9._-]` (a name that changed gets
  `-<8 hex of sha256(raw name)>`, so `a b` and `a_b` never share a ref) and the ref checked with
  `git check-ref-format`; later pushes force (`+`) only that run's own branch, and a tree equal to
  `/run/triple-c/wip-last/<folder>` (claude-owned dir, written after a successful push) is skipped.
  No usable run id (Docker) keeps a per-push `<stamp>-<folder>`, never forced.
  **Setup script** (`TRIPLEC_SETUP_SCRIPT`, unset before the snapshot like `TRIPLEC_FILES`): written
  to `/run/triple-c-setup/script` (root 0755 dir; root opens `log` and hands the descriptor over, so
  claude writes nothing there), run as `claude`, `bash -e`, stdin `/dev/null`,
  `timeout -k 10 1800`, chained after the background clone's `done` in the same background
  subshell; `status` is `running` from the start until `ok` / `failed <code>` (a chain that dies
  early writes `failed`, so the idle stop is never blocked). `triple-c-workspace-status` (python,
  no secrets) prints `{clone:{done,repos},setup:{state,code,tail}}`. Edited in HostSection's
  "Setup script" textarea (saved on blur, blank → `null`; the 32 KiB UTF-8 limit is checked in TS
  with Rust's own message, `setupScriptProblem`).
  **The start does not wait for them** (it ends after "Connecting…": Running, lock released);
  `aws_host::spawn_follow` follows in a background task (`host/aws_host/status.rs`: background exec
  every 2 s, at most 35 min, holds no project lock; ended by `tunnels::cancel_follow` on Stop/Remove
  and by any tunnel close or app exit). Its only output is a `workspace-warning` event (warning
  toast) for a failure it saw happen or one on a task this start ran; progress shows in Project
  Home's Overview card, which polls `aws_workspace_status` every 5 s while Running and visible.
  At load, a Starting/Stopping AWS project becomes Error (not Stopped — a crash mid-start may have
  left a billed task), and `reconcile_aws_projects` also checks a Stopped one still holding a task ARN.
  **Lifetime cap** (`TRIPLEC_MAX_LIFETIME_MINUTES`, advisory — sudo can kill it): the idle watcher
  (started when idle stop *or* a non-zero cap is set) pushes best effort, records `lifetime` and
  sends TERM; the hook exits 76 (`aws::tasks::LIFETIME_EXIT_CODE` → `StopKind::LifetimeCap` →
  `lifetime` toast). HostSection shows a non-zero cap ("stops workspaces after N h (advisory)") from
  `aws_environment_max_hours` (reads the environment record; `None` when it can't, never an error).
  The idle watcher runs as root under a supervisor and signals the
  entrypoint's own PID (not PID 1, which is docker-init), so idle and ECS stops share one trap.
  Idle = the agent's `last_activity_unix` older than N minutes (open-but-silent attaches/tunnels
  don't count; requests with `X-Triple-C-Background: 1` — the auth bridge's and the file viewer's polls — never do),
  no `claude`, no scheduler job, no running clone or setup script, and no unsaved git work: with the WIP push on the
  watcher pushes *before* signalling and stays up if that fails; with it off, dirty repos keep the
  workspace up; activity during the watcher's push sends it back to watching. A repo the background
  clone is still writing (`/run/triple-c-clone/repos` last says `cloning`, no `done`) is skipped by
  `triple-c-wip-push` (dry run: `"cloning": true`). The stop trap is set before any background work
  starts. An idle stop exits 75 (`aws::tasks::IDLE_EXIT_CODE`) so the death watch can tell it
  from a crash (`classify_stop` → the `ContainerExited` toast).
  **Copy-at-connect** (`secrets_delivery: copy_at_connect`, Q2): the secret keeps settings, repos and
  the agent token but no `TRIPLEC_FILES` and no credential pairs (`SECRET_ENV_KEYS` + every custom
  variable; `TRIPLEC_CREDENTIALS=connect` instead — the gateway sidecar's `TRIPLEC_GATEWAY_*` keys
  stay, R1). After every connect (start, attach, background re-attach) `aws_host::connect`
  uploads the files and `connect.env` (`KEY=<base64>` lines) as root into this connect's own
  `/dev/shm/triple-c/connect/<nonce>/` (root 0700 tmpfs; `--prepare` prints the nonce, so two
  connects at once never mix) and runs `triple-c-apply-connect <nonce>`: CA install as root, SSH merge and git
  credential helper **as `claude`** (root never walks a live `~/.ssh`), `/run/triple-c/connected`,
  then a background re-clone of `failed` repos only (GIT_TOKEN in that clone's env only). The
  first clone waits up to 5 min for `connected` (`aws_wait_connect`) — when there are repos **or a
  setup script** (the script runs after the clone and needs the CA, SSH and Git credentials). The
  CA files are swapped in (`connect-ca.new` → `connect-ca`, old removed last), so a failed move
  keeps the CA installed before. Terminals the app opens add
  the pairs to their own `ExecSpec.env` (`aws_host::terminal_env`, pairs only — no file read);
  nothing reaches the agent's snapshot, so cron/scheduled jobs and the setup script never get them.
  A failed push is a `workspace-warning`. Hybrid/EFS do **not** link `~/.ssh` to EFS under
  copy-at-connect (R6: keys vanish with the task). When a copy-at-connect start finds the secret
  already there (CreateSecret's ResourceExists; a new project's secret is never read first) and it
  `holds_credentials`, it is force-deleted and created again (R7: `PutSecretValue` would keep the
  old values as AWSPREVIOUS); `TRIPLE_C_CUSTOM_ENV` is left out too. A replace whose re-create
  times out (AWS still finishing the forced delete) leaves no secret; every secret write retries
  while the name is pending deletion (≤ 30 s), then says to start again in a minute.
  **Copy-at-connect limits** (also listed in HostSection's notes): there is no corporate CA until
  the first connect (Claude Code's self-update may fail behind a TLS-inspecting proxy until then);
  SSH keys, a Git token or variables removed from the project stay in the running task until it
  stops (a connect only adds/overwrites them; a removed CA does go at the next connect); the setup script is not re-run
  after a later connect's re-clone of a failed repo.
  The re-clone closes the apply lock's fd (a later connect never waits for it); `--prepare` takes it.
  The functions both scripts use live in `container/entrypoint-lib.sh`
  (`/usr/local/lib/triple-c/`), sourced by the entrypoint first; it only defines things.
  **Persistent terminals** (`host/persistent.rs`): the image has `tmux` (in the Python apt layer)
  and `/etc/triple-c/tmux.conf` (`container/triple-c-tmux.conf`, outside the seed-only home): no
  prefix key (Ctrl+B reaches Claude/readline), no status line, `escape-time 0` (ESC+CR stays one
  Meta-Enter), no mouse, `window-size latest`, and three outer-terminal overrides that were each
  **measured by feeding tmux's real output into xterm.js** — keep all three: `smcup@:rmcup@`
  (draw in xterm.js's normal screen, so its scrollback, copy cleanup and link detection work),
  `indn@` (tmux's CSI S scroll is a *delete* in xterm.js: 4 of 300 fast lines reached scrollback
  with it, 300 without) and `clear=\E[H` (tmux sends `clear` when its client exits, which wiped
  the session's last screen from the tab; its full redraws repaint every row anyway). The idle
  watcher still sees `claude` inside tmux (`pgrep -x claude`; tmux's processes are `tmux:
  server`/`tmux: client`), so a running Claude keeps the workspace up and a detached shell does
  not; the lifetime cap still applies.
  **The Docker-socket group step** (`# >>> docker_socket`) runs in aws mode as well as Docker mode: an
  EC2 project with Docker access mounts the instance's `/var/run/docker.sock`, and the step adds
  `claude` to a group with the socket's gid (`docker-host` if none exists). Fargate never has the
  socket, so it is inert there.
  **The `TRIPLEC_` env prefix is reserved** on both hosts (`RESERVED_ENV_PREFIXES`; `workspace.rs`
  drops custom variables in it): the entrypoint's settings live there.
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
- **`entrypoint.sh`** (+ `entrypoint-lib.sh`, the CA/SSH/git-credential/clone functions it sources) — UID/GID remapping to match host user, SSH key setup (`sync_host_ssh` *merges* the host dir into `~/.ssh`; never wipe it — container-made keys live there on the home volume), git config, docker socket permissions, Claude Code settings.json injection, then stays resident **as root** waiting on a root-owned
  `triple-c-keepalive` sleep (respawned if killed; SIGTERM trap so `docker stop` is immediate).
  **Never hand the container's lifetime to a process the `claude` user owns** — it used to end in
  `exec su … claude -c "exec sleep infinity"`, and any `pkill sleep` / `kill -9 -1` a Claude
  session ran killed the whole container. `container_watch.rs` (one Docker `die`-event
  subscription) is what flips a project whose container exits on its own to Stopped and emits
  `project-container-exited` with a `reason` (`exited` toasts; `found_stopped`, the first look of a run, is silent); `useProjectStatusEvents` applies it
- **`triple-c-scheduler`** — Bash-based scheduled task system for recurring Claude Code invocations
- **`triple-c-agent/`** — the in-container agent (its own Rust crate; protocol in
  `docs/superpowers/specs/2026-11-13-triple-c-agent-protocol.md`). Built static by the Dockerfile's
  `agent-build` stage, started by `entrypoint.sh` **only when a token is configured**, last, as root
  under a supervisor loop. **Its contract is Docker parity**: an `ExecSpec` must behave through the
  agent exactly as through `docker exec`. Any change to exec semantics on either side is checked by
  `host::agent::live_tests::the_agent_answers_exactly_as_docker_exec_does` (ignored; needs an image
  with the agent — see the crate README). The token never stays in the environment and never touches the
  writable layer: the entrypoint moves it to a root-only file on `/dev/shm` (tmpfs — a file in the
  writable layer would ride `docker commit` into the snapshot image) before anything runs, and the
  agent refuses a token file others can read. The agent is started from an env/cwd snapshot taken
  at the top of the entrypoint and held **in memory only** (it contains every API key in the env),
  so execs see the container's configured env, not the script's. The app treats the agent as
  untrusted: no redirects, capped bodies, timeouts on every small call (`host/agent.rs`).

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

### Keeping Claude Code current

`claude update` runs in **two** places, and both are needed:

- `container/entrypoint.sh` runs it once per container start, before any session exists.
- `commands/terminal_commands.rs` (and its twin in `web_terminal/ws_handler.rs`) prepend it to the
  command every Claude session launches with, because containers use a stop/start model and a
  long-lived one would otherwise never re-check.

Both are `timeout`-bounded and `|| echo`'d, so an offline or slow network delays a tab rather than
failing it, and **both take the same `flock` on `/tmp/.triple-c-claude-update.lock`**. That lock is
not tidiness: the entrypoint prints "container ready" only after its own update finishes, so
starting a project and immediately opening a tab — or opening two tabs at once — otherwise runs two
updaters against the same `~/.claude/bin`, and `|| echo` would hide a half-written install behind a
friendly message one line before `exec claude` ran it. `-E 0` makes losing the race a success,
because the holder just did the work. The per-session copy is what forced the non-Bedrock path from a bare `["claude", ...]`
argv into a `bash -c` wrapper — the flags and the session name are interpolated into a shell
string now, so **anything added there must go through `shell_quote_arg`**. Bash sessions are
deliberately untouched. On AWS that whole command (not a copy of it) becomes the argv of a tmux
`new-session` (`host::persistent::new_session_cmd`), so it runs once per session, never on a
reattach; tmux reads an argument that *ends* in `;` as a command separator, so the scripts keep
ending in a newline (`new_session_cmd` refuses otherwise).

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
- **The snapshot image is not a checkpoint — never read its absence as "nothing to inspect".**
  `commit_container_snapshot` runs only before a container is destroyed (a config-change recreate)
  or inside a migration. **Never on stop.** So a project in daily use for a year can legitimately
  have no `triple-c-snapshot-{id}:latest` at all, and one that has is stale by everything installed
  since. `pick_probe_source` therefore reads a *stopped* container directly — commit its writable
  layer to a unique `triple-c-probe-*` image, probe that, drop it — and ranks it **above** the snapshot,
  for the same reason a running container already outranked it. Assuming a snapshot existed is what
  made a stopped, never-recreated project report "no container or snapshot image yet" with its
  container sitting right there, and left Update disabled on the projects furthest behind.
- **`bollard` never gives you the image id back from a commit.** Its `Commit` response model
  deserialises `"ID"`; the daemon sends `"Id"`, so `commit_container` returns `id: None` every time
  (verified: bollard 0.18.1, Engine 29.6). Neither long-standing commit site notices because both
  discard the response — but it means any commit you need a *reference* to has to be **tagged**.
- **A tagged leftover is the one orphan no sweep can reach, so the probe image has its own reaper.**
  `sweep_orphaned_snapshots` collects `dangling` + `triple-c.managed=true`; `reap_stale_migration_pins`
  and `scrub_secrets_from_snapshots` both filter `triple-c-snapshot-*`. A `triple-c-probe-*` image is
  tagged and so matches none of them, which would make a crashed probe a permanent multi-gigabyte
  leak with no UI to find it. `reap_probe_images` runs at startup beside `reap_probe_containers` and
  is **load-bearing, not tidying** — it is also what makes the probe image's unscrubbed writable
  layer acceptable. Two rules it earned the hard way:
  - **Age-gate it** (`PROBE_REAP_MIN_AGE_SECS`, same as the container reaper). `reference=` is
    daemon-wide, so a second copy of the app has live probe images matching the glob.
  - **Remove by tag, never by image id.** A `force` removal by id untags an image *everywhere*; a
    fixture that tagged `alpine:latest` into this namespace deleted the user's alpine that way.
- **Probe image names are unique per call, and must stay that way.** A stable per-container name was
  tried: container ids do not survive a recreate, so most leftovers were stranded permanently, and
  two concurrent probes fought over one tag — whichever finished first force-removed the image the
  other was still reading, reporting a bogus `probe_error` on a healthy project. `get_container_staleness`
  takes no `project_lock` claim (the migration banner needs it to answer *during* a migration), so
  uniqueness is what makes overlapping probes safe.
- **The stopped-container probe is cached per stop, and that is not an optimisation you may drop.**
  `getContainerStaleness` is called from a `useEffect` that fires whenever the container settles, so
  merely opening a stopped project's Overview probes it. Uncached that is a `docker commit` of the
  whole writable layer per visit — measured at 44 s on a real project, against ~3 s for the snapshot
  probe it replaced. `STOPPED_MANIFEST_CACHE` is keyed on the container's `FinishedAt`, which is
  exact rather than merely plausible: nothing can write to a stopped container's writable layer, and
  `FinishedAt` moves on every stop. A live test asserts the restart case, because a cache that
  failed to invalidate would plan a migration against a filesystem the project no longer has.
- **Do not "skip the probe when the project is not stale" to save that cost.** It was tried. The
  deltas would be empty while `probeSettled` (`!probing && staleness && !probe_error`) stayed *true*,
  which leaves the migrate action in the project menu enabled — that action is not gated on the
  banner — so the pre-flight would report nothing to copy while the backend was told to copy
  nothing. That is the exact hazard `ProjectHome.tsx`'s `canMigrate` comment already warns about.
- **A failed stopped-container probe falls back to the snapshot whenever one exists.** Before this
  feature a stopped project read its snapshot directly, so surfacing a commit failure where the
  snapshot could have answered would make the banner *worse* than it was — and the failure modes are
  exactly the ones where the fallback earns its keep: a full disk (the commit allocates the whole
  writable layer; the snapshot probe allocates nothing) and a 409 from a concurrent claim.
- **`get_container_staleness` never commits while the project is claimed.** It takes no
  `project_lock` claim itself, deliberately — the banner has to answer *during* a migration — so it
  reads `project_lock::held` instead and probes the snapshot rather than the container. The
  collision is not symmetric: the probe losing is a retryable `probe_error`, but
  `start_project_container` removes the old container with a hard `?`, so a remove that raced a
  commit would fail the user's Start with an opaque error.
- **An image's `Created` is the image's own, not its tag's.** Tagging an existing image gives you
  that image's age; BuildKit stamps `docker build` output with a fixed epoch. Only `docker commit`
  stamps *now* — which is what real probe images do, and what any fixture for them must do.
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
- **`/workspace` is reported two levels deep, never file by file** (`SUMMARISED_ROOTS`). One
  `find -xdev` pass is piped through `awk`, which turns each depth-2 directory into one `S` entry
  sized as the sum of the files beneath it (the same number the per-file sum gave the disk gate),
  or a plain `d` if it holds no file at any depth (so empty scaffolds stay empty to the filter). A
  user's worktrees and `node_modules` made a flat walk 8.6 MB — past the exec cap, so every
  staleness check failed on start and stop — and pruning `node_modules`/`.git` by name only
  reached 6.3 MB; now the whole manifest is ~1 MB. Summaries lose nothing: below `/workspace/<x>`
  every per-path filter is vacuous, and `tar` copies a directory recursively. Three rules:
  - **A failed `awk` is an error, never an empty `/workspace`.** The pipeline prints
    `###SUMMARISED-OK` only on success and `check_manifest_complete` refuses a manifest without
    it — otherwise the swap would destroy what the walk failed to see.
  - **A summary is exempt from presence-in-the-base**, because it hides which paths are inside;
    `--skip-old-files` on restore is the per-file answer. That rests on the base shipping nothing
    under `/workspace` — pinned by the ignored live test
    `the_base_ships_nothing_under_a_summarised_root`, and logged as a warning if a base breaks it.
  - **Only `/workspace`.** The other roots stay per file: dpkg ownership and presence-in-the-base
    are exactly what make their delta honest (`/usr/local/lib/node_modules`, base content in
    `/opt`), and they are under 1 MB together.

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
- **A new command needs three things:** `#[tauri::command]`, a `generate_handler!` entry in
  `lib.rs`, and a bare `allow-<name-with-dashes>` entry in the one capability file for the
  window that calls it — `viewer_*` commands in `capabilities/file-viewer.json`, everything else
  in `capabilities/default.json`. `build.rs` declares a Tauri `AppManifest` from the handler list
  (without one, tauri 2.11 does not apply the ACL to app commands at all) and fails `cargo
  check`/`tauri build` on a missing, misspelled, duplicated or misfiled grant, a `deny-*`, or a
  hand-written file under `permissions/`. `src/test/capabilities.test.ts` fails if code that runs
  in a window imports a `tauri-commands.ts` wrapper that window is not granted. Only `_` becomes
  `-` in the identifier; `permissions/autogenerated/` is generated and ignored, and
  `gen/schemas/*.json` is regenerated by every build and committed.
- **A new window needs its own top-level `capabilities/*.json`; never `webviews`/`remote`;
  never inline.** `build.rs` only vouches for what `src/command_census.rs` reads — a top-level
  `capabilities/*.json` file with a `windows` list — so it refuses to build on anything tauri
  would load that the census can't check: a capability under a subdirectory or written as
  `.toml`/`.json5`, a `webviews` or `remote` key in a capability file (either widens grants past
  what `windows` says), `app.security.capabilities` declared inline in `tauri.conf.json`/any
  `tauri.<platform>.conf.json`/`TAURI_CONFIG`, or a tauri config in a format it can't parse
  (JSON5, TOML). OS/editor junk (`.DS_Store`, `Thumbs.db`, swap files) is recognised and skipped
  rather than refused. Each failure names the check that failed, not just "capabilities do not
  match generate_handler!". **Known limit:** adding a new `tauri.<platform>.conf.json` to a tree
  that has already been built once only takes effect on a clean build or in CI — cargo's
  incremental build has no reason to notice a file that did not exist on the previous build.
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
- A new local window needs its own capability file (`capabilities/file-viewer.json` is the
  model), and `lib.rs`'s `on_window_event` stays guarded on `label() == "main"`.

### Marketplace

- Code: models in `models/marketplace.rs`; host-side logic in `src/marketplace/` (`git.rs` gix
  cache + pins, `catalog.rs` repo format, `auth.rs` credentials, `gh_login.rs`, `payload.rs`,
  `sync.rs`); commands in `commands/marketplace_commands.rs`; UI in `components/marketplace/` and
  `projects/home/config/MarketplaceSection.tsx`. Spec:
  `docs/superpowers/specs/2026-09-27-marketplace-design.md`.
- **Tokens never enter containers.** Marketplaces are fetched on the host into
  `<data_dir>/triple-c/marketplaces/<id>.git`; containers only ever receive a tar of pinned
  files. Do not add a code path that passes a marketplace credential into an exec, env var, label
  or file in a container.
- **Sync model:** after every container start (next to `sync_bedrock_credentials`) and on "Apply
  now", the host builds the project's effective set (`global − disabled ∪ project`), then uploads
  `payload.tar` **and the app-embedded script `src/marketplace/sync.sh`** (`include_str!`, not a
  file in `container/`) to `~/.claude/triple-c/marketplace/incoming/` and runs it as `claude`,
  once the entrypoint has finished (`READY_PROBE`: a root `triple-c-keepalive infinity` process
  on current images, `su -s /bin/bash claude -c exec sleep infinity` on older ones — keep both).
  The script is re-uploaded on every sync rather than baked into the image, so every
  existing project always gets the version that matches the running app — `container/` is never
  touched for this feature. The script only removes files and hook entries it recorded in
  `~/.claude/triple-c/marketplace/state.json`; it must never overwrite or delete user-created
  agents/skills/commands or user hooks. A sync failure must not fail the container start.
- Installs are **pinned** to a commit; nothing updates without the user accepting a diff. Pinned
  commits are kept alive by `refs/triple-c/pins/*` in the cache.
- Marketplace changes need no container labels or recreation — they are applied by the sync, not
  at create time.

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
(triple-c#35). Exports the *host* environment — global `AppSettings` plus the global secrets that
live in the OS keychain instead: the shared Claude Code OAuth login, the model gateway's two keys,
and every marketplace account's token. Per-project settings, per-project secrets, and anything in
a project's Docker volumes are deliberately out of scope — this is not a project backup.

- **`AppSettings` is not entirely the non-secret shape it looks like, and a review of this feature
  caught the one place that isn't.** `WebTerminalSettings::access_token` is a live bearer
  credential for a server that binds every interface — exporting `AppSettings` wholesale would
  have carried it along as if it were as inert as a port number, and importing it would have
  applied `web_terminal.enabled` and the token together with no more warning than any other
  setting, letting a crafted export silently stand up a LAN-listening terminal on the next launch.
  `export_settings`/`apply_settings_import` carve this one field out into `ExportedSecrets`
  instead, with the same "only overwrite what the import actually has" treatment as the other
  three secrets — except "leave it alone" has to be done by hand in `apply_settings_import`, since
  unlike the keychain secrets this one lives inside the `AppSettings` blob that gets replaced
  wholesale. `SettingsImportPreview::enables_web_terminal` also exists because of this: `enabled`
  and the token are independent fields, and "this turns on a listening service" must not hide
  inside a generic "settings replaced" summary. Read this as the standing example of the class of
  thing to keep checking for in this feature, not a one-off fixed bug — any other field that looks
  like config but is actually a live credential would have the same problem.
- **AWS connection keys travel in `ExportedSecrets::aws_connection_keys`** (keyed by connection id),
  exported and restored like the marketplace tokens. `hosts` are **store-owned**: `update_settings`
  restores them via `restore_host_fields`, and `apply_settings_import` re-applies them through
  `StoreOwned`. `validate_imported_hosts` runs before any keychain write. The import preview
  discloses AWS connections, key counts, overwrites (naming the local connection), failed key
  checks and removals.
- **Marketplace account tokens travel in `ExportedSecrets`, not in `AppSettings`.** `Token` and
  `GhContainer` accounts' tokens live in the keychain (`triple-c-marketplace-account-<id>`), so
  they follow the same "carve out of the keychain, restore before the settings replace, only
  overwrite what the file actually has" treatment as the other three secrets
  (`ExportedSecrets::marketplace_account_tokens`, keyed by account id). Marketplaces and install
  lists themselves are ordinary `AppSettings` fields and travel with the settings replace, but are
  **validated** on import the same way the add-marketplace/install commands validate them
  (`validate_imported_marketplace_state`) — an import is untrusted input, not a trusted restore.
  The preview warns whenever the import carries one or more **global hook installs or global
  plugin installs**, in addition to the base-URL and custom-image warnings above: a hook runs
  commands in every project container, and a plugin can carry its own hooks, MCP/LSP servers and
  commands into one. In the Marketplace tab both kinds have a confirm step before they install
  (`HookConfirmModal` lists a hook's commands, `PluginConfirmModal` lists everything a plugin
  brings that runs); an import installs them without either, so the preview warning is the only
  place that confirmation happens for an import.
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
  a path ever crossing back over IPC. It also pins a hash of the file's ciphertext next to that
  path, and `apply_settings_import` refuses to proceed if the file on disk no longer matches it —
  otherwise confirming a preview would not actually be binding on what gets applied, which matters
  given this feature's own threat model: a file shared between people may sit in a synced or
  otherwise shared directory that changes between the two calls.
- **The decrypted payload is not cached between preview and apply — only the password is reused.**
  The frontend holds the password in React state and passes it to both calls; nothing in Rust
  holds decrypted plaintext — secrets included — in memory for longer than one command's
  execution, so `apply_settings_import` always re-decrypts rather than reusing anything
  `preview_settings_import` computed. `preview_settings_import` returns counts and presence flags
  only (`SettingsImportPreview`), never a secret value, so it's safe to hand to the frontend and
  render directly.
- **Import replaces settings wholesale, but only writes secrets actually present in the file.**
  An import is "restore this environment," so the settings half is a full replace, not a
  field-by-field merge. Secrets are different on purpose: an absent secret in the export means
  "the source machine never had this configured," not "delete this on import" — a user who wants
  to clear a secret already has dedicated UI for that (signing out of shared auth, clearing the
  gateway key). Secrets are restored *before* the settings replace runs, not after — replacing
  settings is what triggers `reconcile_gateway`, and restoring the other way round leaves a real
  window where a gateway recreation happens against the destination's old keys.
- **A restored gateway secret nudges a running gateway container to recreate itself, even when
  nothing about the gateway's *shape* changed.** `reconcile_gateway`'s `gateway_shape_changed` only
  compares port/provider/base URL/models — deliberately, since that's what's rendered into the
  container's config — so a secret-only change (same shape, new key) is invisible to it. Left
  alone, a running container would keep serving the old key material indefinitely after an import
  that restored a new one. `apply_settings_import` tracks whether either gateway secret was
  actually written and, if the gateway is enabled and its container both exists and is running,
  calls `docker::gateway::ensure_gateway_running` directly afterward — its own fingerprint already
  includes the secret rotation id (`storage::secure::get_gateway_secret_version`), so it recreates
  exactly when it should and no more.
- **A keychain write failing during import is reported back, not only logged.** Each of the three
  `secure::store_*` calls collects its error into `SettingsImportOutcome::secret_restore_warnings`
  in addition to logging it — an import that silently restores two of three secrets but not the
  third must not read as unqualified success just because the settings half of the import (which
  runs after, and is validated before any of this) went through. `apply_settings_import` returns
  `SettingsImportOutcome { settings, secret_restore_warnings }` rather than bare `AppSettings` for
  this reason; `ImportSettingsModal` shows any warnings alongside the "Settings imported" message.
- **The imported settings are validated *before* any secret is written, not just before the
  settings replace.** `apply_settings_import` calls
  `settings_commands::validate_settings_update(&current, &settings)` — the same checks
  `update_settings` runs internally, pulled out into its own function specifically so this caller
  can run them first — and only proceeds to the three keychain writes if that passes. A review
  caught the earlier ordering: writing secrets first meant a rejected import (a bad env var name, a
  disallowed host path) still left the keychain overwritten with the file's secrets while the
  settings themselves stayed unchanged, a silently half-applied state the error message gave no
  hint of.
- **`read_and_decrypt` checks `format_version` before attempting to parse the full payload, not
  after.** A version bump that isn't deserialize-compatible is exactly the case that check exists
  for, and parsing the full struct first would fail on the shape mismatch before the version check
  ever ran. Neither error path interpolates what `serde_json` actually says into the message
  shown to the user — its type-mismatch errors quote the offending value inline, and the plaintext
  here can hold a live credential.
- **The 8-character password minimum is enforced in `export_settings` itself, not only in the
  export modal.** The frontend minimum is a UX nudge; the Rust command is the actual boundary a
  weak password has to cross, and Argon2id's memory-hardness buys little against an attacker who
  can just try a short password directly. Measured with `.chars().count()` (Unicode scalar values)
  rather than `.len()` (bytes), to stay as close as this pair of languages allows to the frontend's
  `.length` check (UTF-16 code units) — the two only diverge on astral-plane characters. The
  derived key and both plaintext buffers — the payload built for export, and whatever `decrypt`
  recovers on import — are wrapped in `zeroize::Zeroizing` for the same reason every other secret
  in this codebase gets handled carefully — cheap insurance (`zeroize` is already pulled in
  transitively via `aes-gcm`) for material that exists only to hold or produce live credentials.
- **The preview also discloses non-blank custom base URLs** (`global_ollama`, `global_llamacpp`,
  `global_openai_compatible`, `gateway.api_base`) so an import that would redirect model traffic to
  a different server is visible in the confirmation dialog rather than discovered later — these are
  endpoints, not secrets, so `SettingsImportPreview` carries and `describeImport` renders the actual
  URL rather than just a presence flag. `describeImportWarnings` additionally calls out a web
  terminal token that arrives with the terminal left *off*: `start_web_terminal` only mints a fresh
  token when none is already set, so a planted token would otherwise activate silently the next
  time someone turns the terminal on, with no import-time signal that it wasn't freshly generated.
- **The preview also discloses a custom Docker image, and warns on one every time — not just on
  change.** `custom_image_name`/`image_source` weren't in scope for the base-URL disclosure above,
  but a review pointed out they're a sharper version of the same problem: this is the image *every*
  project container is created from (`models::container_config::resolve_image_name`), so a crafted
  export pointing it at an attacker-controlled image is a path to running arbitrary code with
  whatever a project's containers are allowed to reach, not merely a redirected API endpoint.
  `describeImportWarnings` fires on `image_source == Custom` unconditionally rather than only when
  it differs from the destination's current value, since re-importing the same risky configuration
  is still worth surfacing every time a user confirms an import.
- **Every free-form string a preview surfaces is sanitized and length-capped before it's built.**
  `SettingsImportPreview::from_payload`'s `sanitize_for_preview` strips control characters and caps
  at 100 characters (`MAX_PREVIEW_STRING_LEN`) for every base URL and the custom image name — a
  review noted that, unlike the count- and boolean-derived fields the preview started with, these
  are verbatim strings from a not-yet-trusted decrypted payload rendered directly into the
  confirmation dialog. Unbounded, a single pathological value (very long, or holding embedded
  newlines) could push the security warnings above the scroll fold in the dialog that exists
  specifically to make them unmissable — the frontend's `<li>`/warning boxes also get `break-all`
  as a second layer against the same failure mode.

## Packaging

### Releases are cut by hand

**Merging to `main` does not release anything users get.** A release is cut by dispatching
`build-app.yml` on `main` (Actions -> Build App -> Run workflow). That one run publishes the Gitea
and GitHub releases for all three platforms, the `vX.Y.Z` tag pinned to the dispatched commit, and
the Linux update channel. Its last step dispatches `build.yml` and `build-stt.yml` with the new tag,
and those rebuild that tag and move `:latest`, which is what installed apps pull.

- Dispatching `build-app.yml` on any other branch is a **dry run**. It builds, signs and verifies,
  and publishes nothing. Every publishing step and job is gated on
  `gitea.event_name == 'workflow_dispatch' && gitea.ref == 'refs/heads/main'`, so give any new
  publishing step the same `if:`.
- A push to `main` that touches `container/` or `stt-container/` publishes only `:edge` and
  `:<sha>`, never `:latest`. The weekly scheduled sandbox rebuild (OS security fixes) rebuilds the
  **newest release tag**, not main.
- This exists so that main can carry dormant work (a feature landing in phases) without each
  merge shipping. Until the gate, every merge to main touching `app/` released, and every merge
  touching `container/` moved `:latest`.

Linux ships as **AppImage only**, built by `build-app.yml` (releases) and
`build-app-preview.yml` (the PR check). The `.deb` and `.rpm` were dropped: two more artifacts to
build and publish for an audience the AppImage already serves, and neither could self-update. The
Linux job passes `--bundles appimage`; `tauri.conf.json` still says `"targets": "all"` so macOS and
Windows are untouched.

`scripts/finalize-appimage.sh` post-processes every AppImage, and both things it does are
load-bearing. **It demotes the bundled `libwayland-client.so.0`** off the loader path, keeping it as
a fallback for a host that has none: `libEGL_mesa.so.0` has a hard `DT_NEEDED` on that library, so a
bundled copy older than the host's Mesa stops the EGL driver loading at all and the window comes up
blank — measured on wayland 1.26 / Mesa 26.2.1 against a 22.04-built image. Do not "fix" this by
bundling a newer wayland: the floor is set by the user's Mesa, which moves independently of our
releases, so this is a host-coupled library like libGL and libdrm. **It also embeds AppStream
metadata and update information**, without which an AppImage manager can adopt the app but never
update it. The update URL points at a fixed `linux-latest` tag on the GitHub mirror
(`scripts/publish-update-channel.sh`), never `releases/latest` — that follows whichever release is
newest, and the backfill creates a GitHub release per Gitea tag including the `-win` and `-mac` ones
that carry no AppImage. The script's post-repack assertions are the only test any of this has.

**There is deliberately no Arch package.** A
`triple-c-bin` `PKGBUILD` and a `publish-arch-package.yml` existed and were removed; they live on
`hold/arch-packaging`. Do not re-add them without the piece that was always missing: the package
was never on the AUR, so it was a manual `pacman -U` of a downloaded file — the same gesture as
the AppImage, for a second artifact to keep working. Being `workflow_dispatch`-only it also
reached 1 release in 28, while `HOW-TO-USE.md` told Arch users to download it from every release.
An AUR account and its SSH key as a repo secret are what would make it worth having; until then
the AppImage is the Arch story.

`scripts/install-appimage.sh` is the desktop-integration half, and it exists because an AppImage
has no installer: it extracts the bundled icons into `~/.local/share/icons/hicolor` and writes a
`.desktop` entry. It **rewrites** the `Exec` line rather than copying the bundled entry — the
bundled one is `Exec=triple-c`, which resolves only inside the AppImage's own mount, so a
verbatim copy yields a launcher entry that starts nothing. It keeps `StartupWMClass` exactly as
the bundle sets it, which is what lets the shell match the window to the entry. Extraction uses
`--appimage-extract`, which needs no FUSE, so the script works before `fuse2` is installed.

### The bundled AWS Session Manager plugin

AWS workspaces connect through AWS's `session-manager-plugin` (`host/ssm.rs`), and the app
**bundles it as a Tauri sidecar** (`bundle.externalBin: binaries/session-manager-plugin`) so users
need not install it. Tauri copies it beside the app binary with the `-<target-triple>` suffix
stripped — `usr/bin/session-manager-plugin` in the AppImage, `Contents/MacOS/` on macOS, the
install directory on Windows (`session-manager-plugin.exe`), and `target/<profile>/` under
`tauri dev`. `find_plugin()` resolves `TRIPLEC_SSM_PLUGIN` → **that sidecar (beside
`current_exe()`)** → `PATH` → AWS's install locations → the install hint; the order is
unit-tested (`host/ssm/tests.rs`, `plugin_resolution`).

- **We build it from source; we never ship AWS's binaries.** AWS distributes its prebuilt plugin
  as "AWS Content" under the AWS Customer Agreement; the source (github.com/aws/session-manager-plugin)
  is Apache-2.0. `scripts/build-ssm-plugin.sh <triple>…` (Linux/macOS; any triple, it
  cross-compiles) and `scripts/build-ssm-plugin.ps1` (the Windows runner, which has no bash) take
  the tag's **commit** as GitHub's archive (pinned SHA-256, checked before extraction; the
  vendored SDK's Java `codegen/` and test `snapshot/` trees are not extracted — they reach 258
  characters, past Windows' MAX_PATH, so the `.ps1` also works under a short `C:\ssm-<8hex>` root)
  and a
  **pinned Go** from go.dev (go.dev's published SHA-256), and run the repo's own build — GOPATH
  mode over its vendored deps, `-ldflags "-s -w"`, `sessionmanagerplugin-main` — with `VERSION`
  stamped to the tag (the repo's file says `1.3.0.0`; AWS's pipeline stamps the real one, so
  `--version` would otherwise lie), plus `-trimpath`, no VCS stamp, empty build id, `CGO_ENABLED=0`
  and a scrubbed Go environment. **The output must equal a pinned SHA-256 per target**: Go builds
  are reproducible across hosts, so every runner must produce the exact bytes recorded in the
  scripts (verified locally: Linux-hosted builds of every target, repeated in fresh directories,
  and the `.ps1` flow, are identical). A mismatch fails the build and means the source, toolchain
  or flags changed — never "update the hash" without knowing which.
- **Each platform job builds its own targets** before `tauri build` (Linux: `x86_64-unknown-linux-gnu`;
  macOS: `universal-apple-darwin`, i.e. both thin slices — tauri-build checks the arch it compiles —
  plus the `lipo`'d universal file the bundler wants, whose slices must thin back out byte-identical
  to the pins; Windows: the `.ps1`). Not one Linux job feeding the others: workflow artifacts do
  not work on this Gitea (see `build-app-preview.yml`'s header), and with reproducible output and
  pinned hashes nothing is gained by building once.
- **Bumping** the plugin or Go: new pins in **both** scripts, rebuild every target, record the new
  output hashes, and update `app/src-tauri/THIRD_PARTY_NOTICES` (the scripts refuse to run unless
  its `Version:`/`Commit:` lines match) including its dependency licences.
- **Debug builds get a placeholder; release builds refuse one.** tauri-build fails *any* build
  whose externalBin file is missing, `cargo test` included. `build.rs` (`ensure_ssm_sidecar`)
  writes a marked placeholder (`src/sidecar_placeholder.rs`) for a missing file when
  `PROFILE=debug`, so `cargo test`/`cargo check`/`tauri dev` and CI's `test` job need neither Go
  nor the network, and `find_plugin()` skips it (falling back to an installed plugin). With
  `PROFILE=release` a missing file **or** a placeholder stops the build, so a local `cargo build
  --release` needs the build script first. **build.rs rejects only a missing file or the
  placeholder, not a stale or wrong binary**: re-run `scripts/build-ssm-plugin.sh` before every
  local release build (it is a no-op when the file already matches the pin, and rebuilds it
  otherwise — e.g. after a pin bump, or after a Windows release build signed it in place). `finalize-appimage.sh` asserts the shipped
  `usr/bin/session-manager-plugin` is an ELF that runs `--version`.
- **Windows: our build is unsigned, so the release signs it with our certificate.** Tauri runs
  `signtool verify /pa` on each sidecar and hands the unsigned one to the sign command, in place
  under `app\src-tauri\binaries` before any bundle (both MSI and NSIS pack that file);
  `windows-sign.ps1`'s allowlist includes it (one more metered signature per release: 6).
  `windows-verify-signatures.ps1` requires exactly one `session-manager-plugin.exe` in the MSI and
  the file NSIS packed to be validly signed, timestamped, **by the same signing identity
  (subject + issuer) as the installer** — not the thumbprint: Artifact Signing certificates are
  short-lived and rotate — and the signing log to show it. The MSI must also carry
  `THIRD_PARTY_NOTICES`; on macOS a workflow step checks the `.app` holds the two-slice plugin in
  `Contents/MacOS` and the notices in `Contents/Resources`. Previews ship it unsigned, like the app.
- **macOS: signed exactly like the app.** The app is not code-signed (no `APPLE_*`/`signingIdentity`
  in the workflows); Go's linker ad-hoc signs the arm64 slice (needed to run on Apple Silicon). If
  signing is ever configured, Tauri signs externalBin with the app's identity, inside-out.
- **Licence:** `app/src-tauri/THIRD_PARTY_NOTICES` (bundled as a resource; `usr/lib/Triple-C/` in
  the AppImage, asserted by `finalize-appimage.sh`) carries the plugin's Apache-2.0 LICENSE and
  NOTICE, the commit we build, the repo's own `THIRD-PARTY` file verbatim, the licence of every
  vendored module linked in (`go list -deps` for linux/darwin/windows) and Go's own. Note that the
  repo's `THIRD-PARTY` file opens with the same "AWS Content" sentence as the binary packages.

### Windows code signing

Windows **releases** (`build-app.yml`) are signed with **Azure Artifact Signing**: the app binary,
the bundled `session-manager-plugin`, the MSI, the NSIS installer and its uninstaller. Three scripts do it, and "Verify signatures"
fails the job if any of them is unsigned or untimestamped, so an unsigned installer cannot ship
quietly. **PR previews are deliberately not signed**, and `build-app-preview.yml` must not
reference the signing secrets. Two reasons: signing is metered (about 1000 signatures a month,
against roughly 50 preview builds a month), and a PR's workflow runs the PR's own code, so a
secret available there is available to whoever can push a branch. To exercise signing before a
merge, dispatch `build-app.yml` on the branch. Off `main` it builds, signs and verifies
without releasing (see Releases above).

- `scripts/windows-signing-setup.ps1` runs once per job. It downloads the signing client
  (`Microsoft.ArtifactSigning.Client`) and a .NET runtime into `.code-signing/` in the workspace,
  **each pinned by version and hash**, writes the dlib's `metadata.json`, and writes a Tauri
  config file with `bundle.windows.signCommand` that the build passes as
  `cargo tauri build --config`. Nothing is installed on the build VM. To bump
  a pin, take the hash from nuget.org / the .NET `releases.json`, never from your own download.
- `scripts/windows-sign.ps1` is the sign command: `signtool sign /dlib` with SHA-256 and the
  Microsoft timestamp server, retried. Credentials never reach a command line — the dlib reads
  `AZURE_TENANT_ID` / `AZURE_CLIENT_ID` / `AZURE_CLIENT_SECRET` from the environment.
  **It signs only an allowlist of what ships**: 6 signatures per release (the app binary twice,
  because Tauri re-patches it between the MSI and NSIS bundles; the MSI; the NSIS installer;
  its uninstaller; and the `session-manager-plugin` sidecar we build from source). Tauri also presents build-time tools, the WiX extension DLLs and NSIS
  plugins, and signing those would more than double the metered count for no user-visible
  benefit. If the app ever ships resource DLLs or sidecars, extend the
  allowlist, or they will go out unsigned (as `session-manager-plugin`'s entry does; see the
  section above). Tauri reports a failed sign command only as
  "failed to run powershell", so the script keeps a transcript (`.code-signing/sign-output.log`,
  `signtool /debug` included), and the job prints it on failure.
- `scripts/windows-verify-signatures.ps1` checks `signtool verify /pa` plus a timestamp on the
  installers, and on the binaries **inside** the MSI (an administrative `msiexec /a` extract).
  It deliberately does not check `target\release\triple-c.exe`: Tauri patches that file again
  after packaging, so the loose copy is unsigned by design and is not what ships. For the NSIS
  installer, which cannot be unpacked that way, it requires the signing log to show the app
  binary and the uninstaller were signed.

Secrets (repository): the three `AZURE_*` above plus `ARTIFACT_SIGNING_ENDPOINT`,
`ARTIFACT_SIGNING_ACCOUNT_NAME`, `ARTIFACT_SIGNING_PROFILE_NAME`. They are referenced only by the
two Windows steps of `build-app.yml` that need them ("Prepare code signing" and "Build Tauri
app"), never by the preview workflow, never echoed, and never on a command line. The repo is
public, so its Actions logs are too. Gitea masks the secret values, and the signing dlib's
`/debug` output carries no tokens (checked against its strings). Anyone who can push to this
repo can reach the secrets through a workflow file, so repo write access is the boundary.
`main` is branch-protected (no direct or force pushes; changes land by merging a PR), and only a
dispatch on `main` publishes, so a signed release only ever comes from a merged, visible change. The
Azure side should hold the rest: an app registration with only the signer role on this one
certificate profile, and a client secret with an expiry. Four things are load-bearing:

- **`metadata.json` excludes every credential but `EnvironmentCredential`.** The dlib uses
  `DefaultAzureCredential`, whose chain ends in `InteractiveBrowserCredential`; the runners run as
  SYSTEM, where that waits forever for a browser.
- **The sign command goes in through `--config`, never `TAURI_CONFIG`.** The v2 CLI does not read
  that variable — it only sets it, for tauri-build — so a config put there is dropped without an
  error. The Windows jobs set an inline `TAURI_CONFIG` for years and it never applied;
  "Verify signatures" is what exposed it, and it is what would catch a regression.
- **The signing files and the job's `%TEMP%` live in the workspace.** The NSIS uninstaller is
  written to `%TEMP%` and signed from inside 32-bit `makensis`, under 32-bit PowerShell; WOW64
  redirects SYSTEM's own `%TEMP%` (under System32) for those processes but not for the x64
  signtool, so they would disagree about where the file is. The workspace is under
  `systemprofile\.cache`, which the VM junctions so both views resolve. makensis also ignores
  the sign command's exit code for the uninstaller, so `windows-sign.ps1` logs every file it
  signs and the verify step requires a logged signature under that temp directory.
- **The build VM is `WindowsBuilder` (VM 110 on the Proxmox host `pve4`)**, carrying both the
  `winvm-builder` and `virtual-builder` runners in host mode. It has the Windows SDK's
  `signtool` (10.0.26100) but no .NET — hence the job-local runtime.

## Testing

Frontend tests use Vitest with jsdom environment and React Testing Library. Setup file at `src/test/setup.ts`. Run a single test file:
```bash
cd app
npx vitest run src/path/to/test.test.ts
```

CI runs both suites on every PR: the `test` job in `.gitea/workflows/build-app-preview.yml` does
`npm run build`, `npx vitest run` and `cargo test --locked`, in parallel with the platform builds.
It is the only place `cargo test` runs on merge, which matters most for the app-command ACL
census — an ungranted command compiles and only fails at runtime. `build-app.yml` (releases
from `main`) deliberately does not repeat it. The runner is root, so the few Rust tests that
exercise file permissions skip themselves there.
