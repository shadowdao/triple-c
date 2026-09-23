# Terminal file viewer/editor — design

Date: 2026-09-22
Status: approved in conversation (user); reviewed against the code 2026-09-22 (see "Decisions
made during review" at the end); plan at `docs/superpowers/plans/2026-09-22-terminal-file-viewer.md`

## Goal

File locations Claude prints in a terminal tab (`src/foo.ts:42`, `/workspace/x/README.md`,
`app/src/lib/urlRelay.ts:139-150`) become clickable. A click opens the file in its **own OS
window**, scrolled to and highlighting the line/range, syntax-highlighted, live-reloading while
Claude changes it, and **editable** so specs and similar files can be read, edited and saved
from inside the app.

Non-goals: tabs inside a viewer window, creating new files, diff/merge views, opening files in a
host editor, locking the viewer window out of every other app command (that is the follow-up
AppManifest spec, `2026-09-22-app-manifest-lockdown-design.md`).

## Current state (verified against the tree at `3537b23`)

- Versions: tauri 2.11.0 / tauri-utils 2.9.0 / @tauri-apps/api 2.11.0, wry 0.55, Vite 6.4.1,
  @xterm/xterm 5.5.0, @xterm/addon-web-links 0.12.0, React 19, bollard 0.18.1.
- `TerminalView.tsx` (props `{ sessionId, active }`; `projectId` is derived from the session in
  the store): `WebLinksAddon` (http/https only, its own `registerLinkProvider` inside the addon),
  OSC 8 via `linkHandler` (`createOsc8LinkHandler(getHost, readState)`; xterm's
  `OscLinkProvider` drops any target that is not `http:`/`https:` **or that fails `new URL()`**
  because `allowNonHttpProtocols` is unset), OSC 7777 URL relay. Nothing calls
  `terminal.registerLinkProvider` directly. `opensOnClick(event, ctx, modifierPromised)` is a
  module-private function in `TerminalView.tsx`; toasts are `useAppState.getState().pushToast`.
- `read_container_file(project_id, path, max_bytes)` in `commands/file_commands.rs` reads via
  bollard `download_from_container` (`fetch_container_file`). **The archive endpoint does not
  follow a final symlink and the helper refuses links** ("is a link — open its target instead"),
  so any path the viewer reads must already be resolved to a regular file. It caps at
  `MAX_READ_BYTES` = 8 MiB, returns `FileContents { contents_base64, truncated, size }`, and does
  **not** call `require_running`. `validate_container_path` (absolute, no NUL, no `..` segment,
  ≤ 4096 bytes; it rejects rather than normalises) and `validate_container_write_path` (that plus
  `is_under_root` against `CONTAINER_WRITE_ROOTS = /workspace, /home/claude, /tmp`, compared by
  whole segments) are private `fn`s in `file_commands.rs`.
- Execs: the container user is addressed by **name**, `"claude"` (never `uid:gid`);
  `docker::exec::exec_oneshot_streams_as(container_id, "claude", cmd, env) -> (stdout, stderr,
  exit_code)` runs without stdin, cwd `/workspace`, 8 MiB output cap, lossy UTF-8. There is no
  helper that feeds bytes to an exec's stdin. `ExecSessionManager::write_file_to_container
  (container_id, file_name, bytes)` lands bytes at `/tmp/<file_name>` owned by the container
  user (`container_user_ids`) with mode 0644 via the archive API. `upload_host_file_with_ids`
  is the host-file variant of the same tar path. `resolve_container_dir` runs `realpath -m` as
  `claude` and re-validates the result against the write roots (allowing the literal path when
  `realpath` fails).
- `FileViewerModal` + `components/projects/home/filePreview.ts` (Files tab): `previewKind`,
  `imageMimeFor`, `previewLimit`, `decodeBase64`, `looksBinary`, `TEXT_PREVIEW_LIMIT` 1 MiB,
  `IMAGE_PREVIEW_LIMIT` 5 MiB. Reuse the classification and limits.
- Only second window today is the browser-view pop-out (`browser_view/popout.rs`), a
  remote-origin window with no capability and no IPC. It builds `WebviewWindowBuilder::new(app,
  &label, WebviewUrl::External(url))` straight from an **async** command (no
  `run_on_main_thread`; Tauri documents that windows must be created from async commands, not
  sync ones), hooks `window.on_window_event` for `Destroyed`, and closes with `destroy()`, never
  `close()`. `lib.rs`'s `on_window_event` returns early for any label but `main`.
- `build.rs` is a bare `tauri_build::build()`: **every app command is callable from every local
  window**. `capabilities/default.json` gates plugin commands only and lists `windows: ["main"]`.
  App commands need no capability entry (CLAUDE.md, "Key Conventions"). *(Historical snapshot at
  `3537b23`. Closed 2026-09-22 by the AppManifest lockdown
  (`2026-09-22-app-manifest-lockdown-design.md`) — see §6 below.)*
- The frontend does not know a terminal's cwd. Terminal execs start in `/workspace`; each project
  path is bind-mounted at `/workspace/<mount_name>` (`ProjectPath { host_path, mount_name }`,
  rows with an empty `mount_name` are skipped at container creation). `/workspace` itself is not a
  mount.
- Tauri multi-window facts that the design rests on:
  - `WebviewUrl::App("viewer.html".into())` is `Url::join`ed onto `build.devUrl` in dev
    (`http://localhost:1420/viewer.html`) and onto `tauri://localhost/` in a bundle
    (`http://tauri.localhost/` on Windows); both are `Origin::Local`, so capability files apply.
    **If `viewer.html` is missing, both Vite's dev server and Tauri's asset lookup silently fall
    back to `index.html`** — the main app opens in the viewer window. A test guards against this.
  - Capability `windows` entries are `glob::Pattern`s, so `"file-viewer-*"` matches.
  - `getCurrentWindow().onCloseRequested(cb)` listens on `tauri://close-requested` and then calls
    `destroy()` itself; Rust calls `prevent_close()` whenever a JS listener exists. So the
    viewer needs `core:window:allow-destroy` or **the X button stops working** the moment the
    listener is registered. `listen`/`unlisten` need `core:event:allow-listen`/`allow-unlisten`.
    Rust→window emits need no grant on the receiving side.
  - `app.emit_to(label, …)` targets one label, but a bare `listen()` in the main window
    (`EventTarget::Any`) still receives it. The viewer listens through
    `getCurrentWindow().listen(...)`, and the main window never listens to viewer event names.
  - The `app.security.csp` applies to every `.html` Tauri serves, `viewer.html` included. Tauri
    adds a `'nonce-…'` to `style-src` only when the entry HTML contains a literal `<style>`
    element, and a nonce disables `'unsafe-inline'` — which CodeMirror's `style-mod` needs for its
    runtime `<style>` injection. **`viewer.html` must not contain an inline `<style>`.**

## Design

### 1. Path detection in the terminal (`app/src/lib/filePathLinks.ts`, `TerminalView.tsx`)

- A pure matcher `findFilePathLinks(lineText) -> FilePathMatch[]`, where
  `FilePathMatch = { start, end, path, line?, col?, endLine? }` (`start` inclusive, `end`
  exclusive, string indices into `lineText`), used by a new `ILinkProvider` registered with
  `terminal.registerLinkProvider` right after `term.loadAddon(webLinksAddon)` (providers are
  asked in registration order; the web-links one runs first, so an `http://` span never reaches
  the file matcher as a candidate — the matcher additionally refuses any span overlapping `://`).
- Matches tokens that look like paths **with a file extension** (or a known extensionless name:
  `Makefile`, `Dockerfile`, `CLAUDE.md`-style names are already covered by the extension rule),
  optionally absolute, optionally `./`/`../` prefixed, followed by optional `:line`,
  `:line:col`, or `:start-end`. Also accepts `#L42` / `#L40-L50` suffixes.
- Strips wrapping that Claude's markdown rendering leaves: backticks, parentheses, brackets,
  quotes, trailing `.,;:` punctuation.
- Must not match inside URLs (http links are already handled; skip spans overlapping `://`),
  bare version numbers (`1.2.3`), or domain names (`example.com` with no `/` — require either a
  `/` in the token or an extension from an allowlist of common source/doc extensions for
  slash-less tokens).
- Wrapped rows: v1 joins a buffer row with its `isWrapped` continuation rows the way
  `WebLinksAddon`'s private `LinkComputer._getWindowedLineStrings` does (walk up while the row
  `isWrapped`, walk down while the next row `isWrapped`, 2048-char budget, `translateToString
  (true)`), then maps string indices back to `{x, y}` cells. `LinkComputer` is not exported from
  the built addon, so the walk is re-implemented in `app/src/lib/xtermLineJoin.ts`
  (`joinWrappedRows(buffer, rowIndex0) -> { text, firstRow0, rowStarts }` plus
  `stringIndexToCell`). `urlDetector.ts` works on the byte stream, not the buffer, so it has
  nothing reusable here. `ILink.range` is 1-based on both axes with an **inclusive** end column;
  `provideLinks(bufferLineNumber)` is 1-based and `buffer.active.getLine(y)` is 0-based.
- OSC 8 `file://` targets: set `allowNonHttpProtocols: true` on the handler and dispatch by
  scheme in `createOsc8LinkHandler`. With the flag on, xterm hands **every** OSC 8 target to
  `activate`/`hover`, including unparseable ones and `javascript:`; so the handler parses with
  `new URL()` itself: `file:` → `onOpenFile(pathname decoded, no line)`, `http(s):` → the
  existing `sanitizeRelayUrl` → `openUrlExternal` path unchanged, anything else → refused
  (`console.warn`, nothing opened). A refused target (`javascript:`, any other scheme,
  unparseable text) keeps the existing refusal card ("This link will not be opened — it failed
  the URL safety check"), which never echoes the target into the DOM; this is the ruled
  behaviour (Task 9), not a new hover card. The hover card for a `file:` target shows the path and "Open
  in viewer". The `WebLinksAddon` comment that describes the old `allowNonHttpProtocols`
  behaviour is updated, not left stale.
- Activation uses the same click gating as web links (`opensOnClick`: no selection drag,
  single click; `modifierPromised` for OSC 8 hovers). A click invokes
  `openFileViewer(projectId, rawPath, line, col, endLine)` → `open_file_viewer`. No
  confirmation toast — nothing leaves the app and nothing is written without an explicit save.
  A rejected open (cap, container not running, project gone) is a `pushToast({ kind: "error" })`.
- Hover: underline + pointer cursor (`decorations`), and the same bottom-left card the OSC 8
  handler draws, reading "Open in viewer".

### 2. Path resolution (Rust, `commands/file_viewer_commands.rs` + `file_viewer/resolve.rs`)

- Pure candidate generation `candidate_paths(raw, mount_names) -> Result<Vec<String>, String>`:
  - Absolute: exactly one candidate, after `validate_container_path` (which rejects `..`).
  - Relative `p`: strip a leading `./`, collapse repeated `/`, reject any `..` segment or NUL or
    length > 4096; candidates in order `/workspace/<p>`, then `/workspace/<mount>/<p>` for each
    non-empty `mount_name`, de-duplicated. Relative paths are the common case (Claude prints
    project-relative paths) and `/workspace/<p>` is first because the terminal's exec cwd is
    `/workspace`.
- Existence is checked in **one** exec as `claude`: `sh -c` with the candidates as `$1..$n`,
  printing `realpath -e -- "$c"` for each candidate that is a regular file (`test -f`, which
  follows symlinks). Output lines are the **resolved** paths, which is what the registry stores
  (`fetch_container_file` refuses a link, so the resolved path is the only one that reads). The
  resolved path is re-checked with `validate_container_path`; a candidate whose resolution
  escapes to an invalid path is dropped. Candidate lists are capped at 16 entries; output lines
  are de-duplicated (two candidates may resolve to one file).
- 0 matches → window opens in a "not found" state listing the candidates tried.
- 1 match → open it.
- \>1 match → window opens in a "choose file" state listing matches; choosing one calls
  `viewer_choose_file(index)`; the choice is from the Rust-held candidate list, never a path from
  the window.
- Container must be running (`require_running`, via `project.container_id`); otherwise error
  toast in the main window, no window opened.

### 3. One window per click (`src-tauri/src/file_viewer/{mod.rs,registry.rs,window.rs}`)

- Each successful `open_file_viewer` creates a `WebviewWindow` with label
  `file-viewer-<counter>` (monotonic `AtomicU64`, never reused within a process), loading the
  app's own bundle at a second Vite entry (`viewer.html` → `src/viewer/main.tsx`;
  `build.rollupOptions.input = { main: "index.html", viewer: "viewer.html" }`), title
  `<basename> — <project name>` (set from Rust), default 900×700, min 480×320, resizable. The
  command is `async` (Tauri's requirement for window creation from a command).
- A registry `ViewerRegistry { entries: Mutex<HashMap<String, ViewerTarget>>, next: AtomicU64 }`
  managed with `app.manage(...)` (separate from `AppState`, like the browser view keeps its own
  state), where
  `ViewerTarget { project_id, project_name, state: Resolved{container_path} | Choose{candidates}
  | NotFound{tried}, initial: Location { line, col, end_line } }`.
  Entry removed on `WindowEvent::Destroyed`.
- If a window for the same `(project_id, container_path)` is already open (Resolved entries
  only): `unminimize()`, `set_focus()` and `app.emit_to(label, "file-viewer-goto", Location)` to
  that window instead of opening a duplicate; the viewer listens with
  `getCurrentWindow().listen("file-viewer-goto", ...)`.
- Cap: 20 viewer windows (`MAX_VIEWER_WINDOWS`); the 21st click returns an error the main window
  shows as a toast. The cap counts registry entries, and an entry is reserved **before** the
  window is built so two concurrent clicks cannot both pass the check.
- When the project's container stops/is removed, open viewer windows stay open but their reads
  fail and they show a "container not running" banner; they are not force-closed (unsaved text
  must not be destroyed). The registry stores `project_id`, not `container_id`, so a recreated
  container is picked up on the next poll.
- `lib.rs`'s shutdown teardown does nothing for viewers: the process exits and the OS closes
  them. Unsaved edits in a viewer are lost on app quit (accepted; it is what the main window's
  close does to a terminal too).

### 4. Editor (`app/src/viewer/`)

- CodeMirror 6, direct packages only (no `codemirror` meta-package, no
  `@codemirror/language-data` — it hard-depends on 13 more grammars, one of which,
  `legacy-modes/mode/pug`, calls `Function(...)`): `@codemirror/state`, `view`, `commands`,
  `search`, `language`, `lang-markdown`, `lang-javascript`, `lang-rust`, `lang-python`,
  `lang-json`, `lang-yaml`, `lang-css`, `lang-html`, `legacy-modes` (toml, shell via
  `StreamLanguage.define`), plus `@lezer/highlight` for the tag-based highlight style. Language
  packs are chosen by extension in `viewer/languages.ts` and loaded with dynamic `import()`
  (same-origin chunks, fine under `script-src 'self'`). Verified: none of these packages uses
  `eval`/`new Function`/Workers; `style-mod` injects a `<style>` element, allowed by
  `style-src 'unsafe-inline'`. No CSP change.
- Theme: `EditorView.theme` + `HighlightStyle` in `viewer/viewerTheme.ts`, colours from the
  app's CSS custom properties (`index.css` is imported by the viewer entry so the tokens exist).
- Line numbers, search (Ctrl/Cmd+F, `searchKeymap`), go to line (`gotoLine`), history,
  `indentWithTab`, `lineWrapping` for markdown/plain text, highlight of the target line/range
  (a `Decoration.line` `StateField` driven by a `setHighlight` `StateEffect`), scroll target
  into view centred (`EditorView.scrollIntoView(pos, { y: "center" })`) on open and on
  `file-viewer-goto`.
- Header: container path, project name, state badges (Read-only reason / Unsaved / Saved /
  Changed on disk / Container not running), Save button.
- **Editable** only when: text (per `filePreview.ts` classification and `looksBinary`), not
  truncated (≤ 1 MiB), and Rust says `editable: true` (the resolved path is inside
  `CONTAINER_WRITE_ROOTS`). Otherwise the editor is read-only (`EditorState.readOnly` +
  `EditorView.editable` both false) with a badge stating why. Images render read-only as in
  `FileViewerModal` (Blob + object URL, `IMAGE_PREVIEW_LIMIT`).

### 5. Save + live reload

- Content identity: SHA-256 hex of file bytes. Two producers, one definition — the hash of the
  file's full bytes: Rust computes it (`sha2`, already a dependency) over the bytes it fetched,
  and `sha256sum` inside the container computes it for polls and for the save check. The two
  agree exactly when the read was not truncated, which is the only case in which the hash is
  used as a save base.
- `viewer_read_file(max_bytes)` → `ViewerFile { contents_base64, truncated, size, hash,
  editable, readonly_reason: Option<String> }` for the caller's own target (`Resolved` only).
  `hash` is over the returned bytes. `editable` is `validate_container_write_path(resolved)`
  succeeding; `readonly_reason` is its message otherwise.
- `viewer_poll_file()` → `ViewerPoll { exists, hash: Option<String>, size: Option<u64> }` via one
  exec as `claude`: `sha256sum -- "$1"` + `stat -c %s -- "$1"`. Missing file → `exists: false`.
  This is one small exec per window per tick, not a 1 MiB archive download.
- Poll every 2 s while `document.visibilityState === "visible"`; pause otherwise; poll once
  immediately on `visibilitychange` back to visible. The reducer tracks `diskHash` (last known
  full-file hash; seeded from the read's `hash` when `truncated` is false, otherwise from the
  first poll) and `baseHash` (the hash the buffer was loaded from).
  - Poll hash == `diskHash` → nothing.
  - Poll hash changed, editor clean → `viewer_read_file` again, replace document, preserve
    scroll position and cursor (clamped), brief "Reloaded" indicator.
  - Poll hash changed, editor dirty → banner "Changed on disk" with **Reload (discard mine)** and
    **Overwrite on save**. No auto-merge. "Overwrite on save" sets `baseHash` to the polled hash
    so the next save succeeds.
  - `exists: false` → banner "File no longer exists"; content kept, editable text retained so the
    user can copy it; saving is disabled (no file creation).
  - Poll error (container not running) → banner "Container not running", editor keeps its state,
    polling continues so the banner clears when the container is back.
- Save: Ctrl/Cmd+S or Save button → `viewer_write_file(contents_base64, base_hash)` → `Ok(hash)`.
  - Mechanism (**all mutation of the target directory happens as the container user**; the
    Docker archive API, which writes as root, only ever lands the payload in `/tmp`):
    1. Rust: `Resolved` entry only; decode; refuse if `!editable`, if content > 1 MiB, or if
       `base_hash` is not 64 hex chars.
    2. `ExecSessionManager::write_file_to_container(container_id, "triple-c-viewer-<uuid>",
       bytes)` → `/tmp/triple-c-viewer-<uuid>`, owned by the container user, mode 0644.
    3. One `sh` script as `claude` (`exec_oneshot_streams_as`), args `target tmp base_hash`:
       `test -f target` else exit 4 (gone); `sha256sum target` ≠ `base_hash` → exit 3
       (conflict); if the directory is writable: `cp tmp dir/.<name>.triple-c-tmp`, `chmod
       --reference=target staged`, `mv -f staged target`; else `cat tmp > target` (in-place
       fallback for a writable file in a read-only directory); `rm -f tmp` in every branch
       (`trap`); print `sha256sum target` on success.
    4. Exit 3 → `Err("conflict: …")` (the window shows the "Changed on disk" banner); exit 4 →
       `Err("gone: …")`; other non-zero → the clipped stderr.
  - Ownership: a non-root user cannot `chown`, so the file ends up owned by the container user
    — the same thing Claude Code's own edits produce. Mode is preserved via `chmod --reference`.
  - Symlinks: the registry already holds the `realpath -e` target (§2), and `editable` was
    computed on it, so a link into a non-writable root is read-only.
  - Returns the new hash; editor becomes clean, `baseHash = diskHash = new hash`.
- Closing with unsaved edits → the window's own confirm bar (Save / Discard / Cancel) driven by
  `onCloseRequested` (`event.preventDefault()` when dirty); Discard calls `destroy()`.

### 6. Security

- New capability file `capabilities/file-viewer.json`, `windows: ["file-viewer-*"]`, granting
  exactly: `core:event:allow-listen`, `core:event:allow-unlisten`, `core:window:allow-destroy`
  (required for `onCloseRequested` to be able to close the window at all — see "Current state"),
  and `core:webview:allow-internal-toggle-devtools` (dev-only, as in `default.json`). No
  `allow-close`, no `set-title`/`set-focus`/`is-minimized` (title and focus are set from Rust;
  polling pauses on `visibilityState`). `default.json` stays `windows: ["main"]`. Update the
  threat-model census in `default.json`'s description to name the second file and why the viewer
  gets `allow-destroy`.
- Viewer commands take `window: tauri::Window` and check the label:
  - `open_file_viewer` — `window.label() == "main"` only.
  - `viewer_get_state`, `viewer_read_file`, `viewer_poll_file`, `viewer_write_file`,
    `viewer_choose_file` — `file-viewer-*` only, and they operate solely on the registry entry
    for **the caller's own label**. No viewer command accepts a path or a label as an argument.
- Rendering: CodeMirror renders text as DOM text nodes; nothing uses `innerHTML` /
  `dangerouslySetInnerHTML` on file content. CSP unchanged.
- Closed by the AppManifest follow-up (`2026-09-22-app-manifest-lockdown-design.md`, implemented):
  `build.rs` now declares a Tauri `AppManifest` from `generate_handler!`, so a compromised viewer
  window can invoke only the five `allow-viewer-*` app commands granted in
  `capabilities/file-viewer.json`, not any other app command.
- Update CLAUDE.md (frontend/backend structure, Key Conventions note about the viewer window and
  label-gated commands).

### 7. Error handling

All Rust errors are sentences suitable for display. Container-authored text is clipped via
`clip_container_text` (made `pub(crate)`). The main window shows `open_file_viewer` failures as
toasts; the viewer shows read/write failures as banners and never loses the editor buffer on
failure.

### 8. Testing

- Vitest: `filePathLinks` matcher (positive/negative table incl. markdown wrapping, URLs,
  versions, `:l:c`, ranges, `#L`), `xtermLineJoin` on a fake buffer, OSC 8 scheme dispatch (in
  `TerminalView.test.tsx`, which already tests `createOsc8LinkHandler`), the viewer
  reload/dirty/conflict state machine as a pure reducer (`viewer/viewerState.ts`), editability
  rules, language selection.
- Rust unit tests: candidate generation and normalization, label gating, registry
  dedupe/cap/removal, write script argument shape, size cap, `viewer.html` presence + Vite
  input assertion, and `every_command_is_registered_exactly_once` (existing) for the new
  commands.
- `npx tsc --noEmit`, `npx vitest run`, `cargo test`, `cargo clippy` clean.
- Manual (`npx tauri dev`): click paths in Claude output, multiple windows, edit + save,
  edit while Claude edits the same file (conflict banner), container stop with window open.

## Decisions made during review

Facts were checked against tauri 2.11.0 / tauri-utils 2.9.0 / @tauri-apps/api 2.11.0 sources,
xterm 5.5.0, Vite 6.4.1 and the tree at `3537b23`. The user-approved decisions (one window per
click, editable, CodeMirror 6, probe-roots resolution, 2 s polling, no autosave, conflict banner,
20-window cap, AppManifest deferred) are unchanged. What changed:

1. **Write mechanism chosen (§5).** No exec helper feeds stdin, and whether a half-close of the
   hijacked exec connection reaches the process is unverified. Instead: stage bytes at `/tmp`
   with the existing `write_file_to_container` (container-user-owned), then one `sh` script as
   `claude` does hash-check + `chmod --reference` + `mv -f` in the target directory. Every write
   into the target directory is therefore subject to the container user's permissions; the
   root-privileged archive API never touches it. `chown --reference` was dropped: a non-root
   exec cannot chown, so the spec now says the saved file is owned by the container user.
2. **Polling is an exec (`sha256sum` + `stat`), not a re-download (§5).** A 2 s tick per window
   that streams a 1 MiB archive was the alternative. The full-file hash from `sha256sum` equals
   the Rust-side hash exactly when the read was untruncated, which is the only editable case;
   the reducer keeps `diskHash` separate from `baseHash` so read-only (truncated) files still
   detect change without a reload loop.
3. **Resolution stores the `realpath -e` result (§2).** `fetch_container_file` refuses a
   symlink, so the raw candidate path could not be read; probing already needs an exec, so it
   resolves in the same one. Candidate normalisation *rejects* `..` (matching
   `validate_container_path`, which does not normalise) rather than resolving it.
4. **Exact capability set (§6).** `onCloseRequested` in @tauri-apps/api 2.11 calls
   `this.destroy()`, and Rust `prevent_close()`s whenever a JS listener exists, so
   `core:window:allow-destroy` is mandatory; `allow-close` is not needed and not granted.
   Focus/title/unminimize are done from Rust, so no `core:window:allow-set-*` grants.
5. **OSC 8 with `allowNonHttpProtocols: true` receives unparseable and `javascript:` targets
   (§1).** The handler now parses and dispatches itself, and refuses everything but `file:` and
   `http(s):` before drawing a file or web hover card; a refused target gets only the existing
   refusal card, which never echoes the target. The stale comment in the `WebLinksAddon` branch is
   updated as part of the change.
6. **Wrapped-row joining is re-implemented (§1).** `WebLinksAddon`'s `LinkComputer` is not
   exported from the built package and `urlDetector.ts` never touches the buffer.
7. **CodeMirror package list pinned (§4).** Direct packages only; `@codemirror/language-data`
   and the `codemirror` meta-package are excluded (extra grammars, one with `Function(...)`).
   A home-grown theme from CSS tokens rather than `theme-one-dark`.
8. **`viewer.html` fallback trap (Current state, §8).** Both Vite dev and Tauri's asset lookup
   fall back to `index.html` when the entry is missing; a Rust test asserts the file and the
   Vite input entry exist.
9. **Registry is its own managed state (§3)** rather than a field on `AppState`, with a
   reserve-before-build cap check; label counter is a process-wide `AtomicU64`.
10. **Event delivery (§3).** `emit_to(label, …)` still reaches a bare `listen()` in the main
    window, so the viewer subscribes via `getCurrentWindow().listen` and the main window has no
    listener for viewer event names.

## Manual verification checklist

Not runnable inside the planning/implementation container: it needs `npx tauri dev` on a machine
with a display, plus a running project container. Run this after every task on this feature has
landed, including any review fix rounds (in particular Task 11's CRLF/BOM save fix, item 16
below).

- [ ] 1. In a Claude session, ask for a file listing; click `src/…` paths, `/workspace/...`
      absolute paths, a `path:line` and a `path:start-end`. Each opens its own window titled
      `<basename> — <project>`, scrolled to and highlighting the line/range.
- [ ] 2. Click the same path again → the existing window is focused and re-highlights; no
      duplicate.
- [ ] 3. Open 20 windows; the 21st click shows the "20 file windows are already open" toast.
- [ ] 4. Edit, `Ctrl+S`: file changes in the container (`cat` it in a bash tab); mode preserved
      (`stat -c %a`); owner is the container user.
- [ ] 5. While a window is open, have Claude edit the file: clean window reloads with "Reloaded";
      a dirty window shows "Changed on disk" with both buttons; "Overwrite on save" then Save
      succeeds; "Reload (discard mine)" drops the edits.
- [ ] 6. Save while the file changed between polls → "Changed on disk" banner, no data written.
- [ ] 7. Delete the file in a bash tab → "File no longer exists", text still copyable, Save
      disabled.
- [ ] 8. Stop the project with a window open → "Container not running" banner; start it → banner
      clears, polling resumes.
- [ ] 9. Click a path under `/etc` or a `file:///etc/hosts` OSC 8 link → read-only badge with the
      write-roots reason.
- [ ] 10. Close a dirty window with the X → Save / Discard / Cancel bar; Cancel keeps it open;
      Discard closes.
- [ ] 11. Close the *main* window with viewers open → app exits, viewers close.
- [ ] 12. Ctrl/Cmd+Shift+I opens devtools in a viewer in dev; in a release build the CSP console
      shows no violations (CodeMirror styles apply).
- [ ] 13. A wrapped long path (narrow the terminal) underlines across the wrap and opens.
- [ ] 14. Image (`.png`) opens read-only; a `.bin`/binary shows "not text".
- [ ] 15. **File-path hover key-hint wording.** Hover a plain-text file-path link in the terminal
      (not an OSC 8 hyperlink) while a foreground program is holding the mouse (e.g. an
      interactive TUI like `vim`/`htop`/`claude`'s own REPL) and confirm the hover card's key hint
      reads correctly for the platform/mode: "Shift+click to open" on non-Mac while the program
      tracks the mouse, "Click to open" when nothing tracks the mouse, "Option+click to open" on
      Mac with `macOptionClickForcesSelection`, or "Not clickable while a program holds the mouse"
      on Mac without it (`openHintLabel` / `fillFileCard` in `TerminalView.tsx`). Then confirm the
      *click itself* is held to whatever the card promised — the modifier actually required to
      activate the link matches the hint shown, even if the program changes its mouse-tracking
      mode between the hover and the click.
- [ ] 16. **CRLF/BOM file round-trip save.** Open a file in the container that has Windows line
      endings (CRLF) and/or a UTF-8 BOM (e.g.
      `printf '\xEF\xBB\xBF\r\nfoo\r\nbar\r\n' > /workspace/<project>/crlf.txt` in a bash tab),
      open it in the viewer, make a small text edit, and Save. Then `cat -A` (or `xxd`) the file
      in the container and confirm the CRLF line endings and the BOM are still present/unchanged
      apart from the edit — i.e. the save did not silently normalize them to LF or strip the BOM.
