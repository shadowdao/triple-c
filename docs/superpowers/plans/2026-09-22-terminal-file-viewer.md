# Terminal File Viewer Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** File paths in terminal output become clickable; a click opens the file from the project's container in its own OS window with a CodeMirror 6 editor that highlights the target line, live-reloads on a 2 s poll, and saves with hash-based conflict detection.

**Architecture:** A pure matcher plus an xterm `ILinkProvider` find paths in the terminal buffer and call one Rust command, `open_file_viewer`, which probes the candidate container paths in a single exec and opens a `file-viewer-<n>` window on a second Vite entry (`viewer.html`). A managed `ViewerRegistry` maps window labels to targets; every viewer command takes `window: tauri::Window` and operates only on its caller's entry, so no viewer command ever accepts a path. Reads use the existing archive-API fetch; polls are one `sha256sum` exec; saves stage bytes in `/tmp` via the existing tar upload and then let a `sh` script running as the container user do the hash check, `chmod --reference` and `mv -f`.

**Tech Stack:** Rust (tauri 2.11, bollard 0.18, sha2, base64, uuid), React 19 + TypeScript, @xterm/xterm 5.5, CodeMirror 6 (`@codemirror/*` 6.x, `@lezer/highlight`), Vite 6 multi-page, Vitest + jsdom.

**Spec:** `docs/superpowers/specs/2026-09-22-terminal-file-viewer-design.md` — read it first, including "Decisions made during review", which records why each mechanism below is the one chosen.

## Global Constraints

- **User-approved decisions are fixed:** one window per click, editable, CodeMirror 6, probe-roots resolution, 2 s polling, no autosave, "Changed on disk" conflict banner, 20-window cap, AppManifest lockdown deferred.
- **Design tokens only.** All colour from CSS custom properties in `app/src/index.css` (`--bg-primary`, `--bg-secondary`, `--border-color`, `--text-primary`, `--text-secondary`, `--text-disabled`, `--accent`, `--accent-emphasis`, `--warning`, `--warning-muted`, `--success`, `--radius-control`, `--radius-panel`). Filled buttons use `--accent-emphasis`. Never `focus:outline-none`, never `disabled:opacity-50`.
- **Use `components/ui` primitives** (`Button`, `StatusIndicator`) — do not hand-roll replacements. Status is never colour alone.
- **Frontend types in `lib/types.ts` must match Rust structs field-for-field** (snake_case both sides).
- **Every `#[tauri::command]` goes into `generate_handler![]` in `lib.rs`, one fully-qualified path per line.** The test `every_command_is_registered_exactly_once` (`lib.rs:759`) fails otherwise.
- **No viewer command accepts a path or a label argument.** Commands take `window: tauri::Window` and gate on `window.label()`.
- **`viewer.html` must not contain an inline `<style>` element** (Tauri would add a style nonce, which disables `'unsafe-inline'`, which CodeMirror needs). CSP in `tauri.conf.json` is unchanged.
- **No `codemirror` meta-package, no `@codemirror/language-data`, no `@codemirror/theme-one-dark`.** Direct packages only (list in Task 6).
- **Commands return `Result<T, String>`** with user-readable sentences; container-authored text goes through `clip_container_text`.
- **Frontend tests mock `lib/tauri-commands`, never `@tauri-apps/api/core`.**
- **No `tempfile` crate.** Rust tests needing a directory use `std::env::temp_dir().join(format!("triple-c-…-{}", uuid::Uuid::new_v4().simple()))`.
- **Commit after every task** with a `feat:`/`test:`/`docs:` message. Do not push.

## What works in this environment (verified 2026-09-22)

| Command | Status |
|---|---|
| `cd app && npx vitest run [path]` | works (817 tests, ~5 s) |
| `cd app && npx tsc --noEmit` | works |
| `cd app && npm run build` | works (tsc + vite build; see Task 6 for the multi-page check) |
| `cd app && npm install <pkg>` | works (registry reachable; CodeMirror 6 packages resolved) |
| `cd app/src-tauri && cargo check --offline` | works (~7 s incremental; GTK/WebKitGTK 4.1/libsoup3 present) |
| `cd app/src-tauri && cargo test --offline [filter]` | works (598 tests listed; ~25 s to build) |
| `cd app/src-tauri && cargo clippy --offline` | works |
| `npx tauri dev` / `npx tauri build` | **not verifiable here** (no display). The manual checklist in Task 12 runs on the user's machine. |
| `docker` | daemon reachable, but no Triple-C container exists; the write script can be exercised against any `ubuntu:24.04` container (Task 3 shows how). |

`--offline` is used because the crate cache is complete; drop it if a new crate is ever needed (none is: `sha2`, `base64`, `uuid`, `serde`, `log` are already dependencies).

---

## File Structure

**Create (Rust, `app/src-tauri/src/`):**
- `file_viewer/mod.rs` — module root: constants (`MAX_VIEWER_WINDOWS`, `VIEWER_LABEL_PREFIX`), `is_viewer_label`.
- `file_viewer/registry.rs` — `ViewerRegistry`, `ViewerTarget`, `ViewerTargetState`, `Location`. Reserve/get/set/remove, dedupe lookup, cap.
- `file_viewer/resolve.rs` — pure candidate generation, the probe script, its output parser, and the async probe.
- `file_viewer/poll.rs` — the poll script and its parser (`ViewerPoll`).
- `file_viewer/write.rs` — `sha256_hex`, the write script, exit-code classification, the async write.
- `file_viewer/window.rs` — builds the `file-viewer-<n>` window and hooks `Destroyed`.
- `commands/file_viewer_commands.rs` — the six IPC entry points and the label gates.

**Create (frontend, `app/src/`):**
- `lib/filePathLinks.ts` — pure matcher `findFilePathLinks`.
- `lib/xtermLineJoin.ts` — wrapped-row joining and string-index → cell mapping over an `IBuffer`.
- `components/terminal/filePathLinkProvider.ts` — the `ILinkProvider` built from the two above.
- `viewer/main.tsx` — entry for `viewer.html`.
- `viewer/ViewerApp.tsx` — loads state, routes to Choose / NotFound / EditorPane.
- `viewer/viewerState.ts` — pure reducer for clean/dirty/changed-on-disk/gone/container-down.
- `viewer/editability.ts` — text/image/binary classification + editable decision from a `ViewerFile`.
- `viewer/languages.ts` — extension → lazily loaded CodeMirror language.
- `viewer/viewerTheme.ts` — `EditorView.theme` + `HighlightStyle` from CSS tokens.
- `viewer/highlightLine.ts` — line-range highlight `StateField` + `setHighlight` effect.
- `viewer/CodeEditor.tsx` — CodeMirror React wrapper.
- `viewer/useViewerPolling.ts` — visibility-gated interval.
- `viewer/EditorPane.tsx` — header, banners, save, close confirmation.
- `app/viewer.html` — second Vite entry.

**Modify:**
- `app/vite.config.ts` — `build.rollupOptions.input`.
- `app/package.json` — CodeMirror dependencies.
- `app/src-tauri/capabilities/file-viewer.json` (new) and `capabilities/default.json` (description census).
- `app/src-tauri/src/lib.rs` — `mod file_viewer;`, `.manage(ViewerRegistry::default())`, six handler entries.
- `app/src-tauri/src/commands/mod.rs` — `pub mod file_viewer_commands;`.
- `app/src-tauri/src/commands/file_commands.rs` — `pub(crate)` on `validate_container_path`, `validate_container_write_path`, `require_running`, `clip_container_text`, `fetch_container_file`, `FetchedFile` and its fields.
- `app/src/lib/types.ts`, `app/src/lib/tauri-commands.ts` — viewer types and wrappers.
- `app/src/components/terminal/TerminalView.tsx` — register the provider; OSC 8 scheme dispatch.
- `app/src/components/terminal/TerminalView.test.tsx` — OSC 8 dispatch tests.
- `CLAUDE.md` — structure notes.

## Interfaces (the contract every task codes against)

**Rust → TS types** (`app/src/lib/types.ts`, added in Task 0; Rust in Tasks 2, 3, 8):

```ts
export interface ViewerLocation {
  line: number | null;
  col: number | null;
  end_line: number | null;
}
export type ViewerTargetState =
  | { kind: "resolved"; container_path: string }
  | { kind: "choose"; candidates: string[] }
  | { kind: "not_found"; tried: string[] };
export interface ViewerState {
  project_id: string;
  project_name: string;
  raw_path: string;
  state: ViewerTargetState;
  initial: ViewerLocation;
}
export interface ViewerFile {
  contents_base64: string;
  truncated: boolean;
  size: number;
  /** SHA-256 hex of the returned bytes; equals the file's hash when `truncated` is false. */
  hash: string;
  editable: boolean;
  readonly_reason: string | null;
}
export interface ViewerPoll {
  exists: boolean;
  hash: string | null;
  size: number | null;
}
```

**Commands** (`app/src/lib/tauri-commands.ts`, Task 0; Rust signatures, Task 8):

| TS wrapper | Rust command | Caller |
|---|---|---|
| `openFileViewer(projectId, path, line?, col?, endLine?): Promise<void>` | `open_file_viewer(project_id: String, path: String, line: Option<u32>, col: Option<u32>, end_line: Option<u32>, window: tauri::Window, app: tauri::AppHandle, registry: State<ViewerRegistry>, state: State<AppState>) -> Result<(), String>` | main only |
| `viewerGetState(): Promise<ViewerState>` | `viewer_get_state(window, registry) -> Result<ViewerState, String>` | viewer only |
| `viewerReadFile(maxBytes): Promise<ViewerFile>` | `viewer_read_file(max_bytes: u64, window, registry, state) -> Result<ViewerFile, String>` | viewer only |
| `viewerPollFile(): Promise<ViewerPoll>` | `viewer_poll_file(window, registry, state) -> Result<ViewerPoll, String>` | viewer only |
| `viewerWriteFile(contentsBase64, baseHash): Promise<string>` | `viewer_write_file(contents_base64: String, base_hash: String, window, registry, state) -> Result<String, String>` (new hash) | viewer only |
| `viewerChooseFile(index): Promise<ViewerState>` | `viewer_choose_file(index: usize, window, registry) -> Result<ViewerState, String>` | viewer only |

**Error prefixes** `viewer_write_file` returns, which the frontend matches on: `"conflict: "` (disk changed since `base_hash`), `"gone: "` (file no longer exists). Anything else is a generic failure sentence.

**Events:** Rust → viewer window `"file-viewer-goto"` with payload `ViewerLocation`, emitted with `app.emit_to(label, …)`; the viewer subscribes with `getCurrentWindow().listen("file-viewer-goto", …)`.

**Window label:** `file-viewer-<n>`, `n` from a process-wide counter starting at 1.

---

## Task 0: Interfaces — TS types, command wrappers, Rust module skeleton

Run first, alone (minutes). Everything in Group A compiles against this, and it is the only task that touches `file_commands.rs` visibility or `file_viewer/mod.rs`'s module list, so the group can genuinely run in parallel.

**Files:**
- Modify: `app/src/lib/types.ts` (append at end)
- Modify: `app/src/lib/tauri-commands.ts` (append at end)
- Create: `app/src-tauri/src/file_viewer/mod.rs`, plus placeholder `registry.rs`, `resolve.rs`, `poll.rs`, `write.rs`, `window.rs` in the same directory (each containing only `//! Filled in by Task N.`)
- Modify: `app/src-tauri/src/lib.rs` — add `pub mod file_viewer;` beside the other `mod` lines
- Modify: `app/src-tauri/src/commands/file_commands.rs` — change to `pub(crate)`: `const MAX_READ_BYTES` (line 49), `fn validate_container_path` (355), `fn validate_container_write_path` (397), `struct FetchedFile` and its three fields (1181–1187), `async fn fetch_container_file` (1205), `async fn require_running` (1459), `fn clip_container_text` (2014)

**Interfaces:** Produces exactly the TS block in "Interfaces" above, and on the Rust side `file_viewer::{MAX_VIEWER_WINDOWS, VIEWER_LABEL_PREFIX, is_viewer_label}` and the `pub(crate)` helpers named above.

- [ ] **Step 0: Create `file_viewer/mod.rs`** (and the five one-line placeholder files):

```rust
//! The terminal file viewer: one OS window per clicked path.
//!
//! Every window is a `file-viewer-<n>` label registered in [`registry::ViewerRegistry`];
//! the commands in `commands/file_viewer_commands.rs` gate on the label and act only on
//! the caller's own entry, which is why nothing here takes a path from a window.

pub mod poll;
pub mod registry;
pub mod resolve;
pub mod window;
pub mod write;

/// Spec §3: the 21st click is refused with a toast.
pub const MAX_VIEWER_WINDOWS: usize = 20;
pub const VIEWER_LABEL_PREFIX: &str = "file-viewer-";

pub fn is_viewer_label(label: &str) -> bool {
    label
        .strip_prefix(VIEWER_LABEL_PREFIX)
        .is_some_and(|rest| !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_numbered_viewer_labels_pass() {
        assert!(is_viewer_label("file-viewer-1"));
        assert!(is_viewer_label("file-viewer-20"));
        assert!(!is_viewer_label("file-viewer-"));
        assert!(!is_viewer_label("file-viewer-x"));
        assert!(!is_viewer_label("main"));
        assert!(!is_viewer_label("browser-view-abc"));
    }
}
```

Then `cd app/src-tauri && cargo test --offline file_viewer` → the one test passes (a `dead_code` warning on the now-unused `pub(crate)` items is expected until Group A lands; do not `#[allow]` it).

- [ ] **Step 1: Add the types** to the end of `app/src/lib/types.ts`:

```ts
// ---- Terminal file viewer (commands/file_viewer_commands.rs) ----

export interface ViewerLocation {
  line: number | null;
  col: number | null;
  end_line: number | null;
}

export type ViewerTargetState =
  | { kind: "resolved"; container_path: string }
  | { kind: "choose"; candidates: string[] }
  | { kind: "not_found"; tried: string[] };

export interface ViewerState {
  project_id: string;
  project_name: string;
  /** What was clicked, for the title and the not-found message. */
  raw_path: string;
  state: ViewerTargetState;
  initial: ViewerLocation;
}

export interface ViewerFile {
  contents_base64: string;
  truncated: boolean;
  size: number;
  /** SHA-256 hex of the returned bytes; equals the file's hash when `truncated` is false. */
  hash: string;
  editable: boolean;
  readonly_reason: string | null;
}

export interface ViewerPoll {
  exists: boolean;
  hash: string | null;
  size: number | null;
}
```

- [ ] **Step 2: Add the wrappers** to the end of `app/src/lib/tauri-commands.ts` (add `ViewerFile, ViewerPoll, ViewerState` to the existing `import type { … } from "./types"`):

```ts
// ---- Terminal file viewer ----

export const openFileViewer = (
  projectId: string,
  path: string,
  line?: number,
  col?: number,
  endLine?: number,
) => invoke<void>("open_file_viewer", { projectId, path, line, col, endLine });

export const viewerGetState = () => invoke<ViewerState>("viewer_get_state");
export const viewerReadFile = (maxBytes: number) =>
  invoke<ViewerFile>("viewer_read_file", { maxBytes });
export const viewerPollFile = () => invoke<ViewerPoll>("viewer_poll_file");
export const viewerWriteFile = (contentsBase64: string, baseHash: string) =>
  invoke<string>("viewer_write_file", { contentsBase64, baseHash });
export const viewerChooseFile = (index: number) =>
  invoke<ViewerState>("viewer_choose_file", { index });
```

- [ ] **Step 3: Verify** — `cd app && npx tsc --noEmit` → exit 0.

- [ ] **Step 4: Commit** — `git add app/src/lib/types.ts app/src/lib/tauri-commands.ts app/src-tauri/src/file_viewer app/src-tauri/src/lib.rs app/src-tauri/src/commands/file_commands.rs && git commit -m "feat(viewer): IPC types, wrappers and Rust module skeleton for the file viewer"`

---

## Parallel group A (no dependencies between them; all need only Task 0 or nothing)

### Task 1: Rust — candidate generation and probe parsing (`file_viewer/resolve.rs`)

**Files:**
- Replace: `app/src-tauri/src/file_viewer/resolve.rs` (Task 0's placeholder)

**Interfaces:**
- Consumes `validate_container_path` (`pub(crate)` since Task 0) and `docker::exec::exec_oneshot_streams_as`.
- Produces `pub fn candidate_paths(raw: &str, mount_names: &[String]) -> Result<Vec<String>, String>`, `pub const PROBE_SCRIPT: &str`, `pub fn parse_probe_output(stdout: &str) -> Vec<String>`, `pub async fn probe_candidates(container_id: &str, candidates: &[String]) -> Result<Vec<String>, String>`, `pub const MAX_CANDIDATES: usize = 16`.

- [ ] **Step 1: Write the failing tests** in `file_viewer/resolve.rs` (the file starts as tests + `use` lines only):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn mounts(names: &[&str]) -> Vec<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn an_absolute_path_is_its_own_only_candidate() {
        let c = candidate_paths("/workspace/api/src/main.rs", &mounts(&["api"])).unwrap();
        assert_eq!(c, vec!["/workspace/api/src/main.rs"]);
    }

    #[test]
    fn a_relative_path_probes_workspace_then_each_mount() {
        let c = candidate_paths("src/main.rs", &mounts(&["api", "web"])).unwrap();
        assert_eq!(
            c,
            vec!["/workspace/src/main.rs", "/workspace/api/src/main.rs", "/workspace/web/src/main.rs"]
        );
    }

    #[test]
    fn dot_prefix_and_duplicate_slashes_are_normalised_and_candidates_deduped() {
        let c = candidate_paths("./src//main.rs", &mounts(&["api", "api", ""])).unwrap();
        assert_eq!(c, vec!["/workspace/src/main.rs", "/workspace/api/src/main.rs"]);
    }

    #[test]
    fn traversal_nul_and_oversize_are_refused() {
        assert!(candidate_paths("../etc/passwd", &[]).is_err());
        assert!(candidate_paths("src/../../x", &[]).is_err());
        assert!(candidate_paths("/workspace/../etc/passwd", &[]).is_err());
        assert!(candidate_paths("a\0b", &[]).is_err());
        assert!(candidate_paths("", &[]).is_err());
        assert!(candidate_paths(&"a".repeat(5000), &[]).is_err());
    }

    #[test]
    fn candidate_list_is_capped() {
        let many: Vec<String> = (0..40).map(|i| format!("m{}", i)).collect();
        let c = candidate_paths("x.rs", &many).unwrap();
        assert_eq!(c.len(), MAX_CANDIDATES);
    }

    #[test]
    fn probe_output_keeps_valid_resolved_regular_files_only() {
        let out = "/workspace/api/src/main.rs\n/workspace/api/src/main.rs\n\nrelative/junk\n/etc/../x\n/workspace/web/src/main.rs\n";
        assert_eq!(
            parse_probe_output(out),
            vec!["/workspace/api/src/main.rs", "/workspace/web/src/main.rs"]
        );
    }

    #[test]
    fn the_probe_script_prints_resolved_paths_of_regular_files() {
        // Shape assertions: the script is data handed to `sh -c`, and these are the
        // three things a later edit must not lose.
        assert!(PROBE_SCRIPT.contains("test -f"));
        assert!(PROBE_SCRIPT.contains("realpath -e --"));
        assert!(PROBE_SCRIPT.contains("for c in \"$@\""));
    }
}
```

- [ ] **Step 2: Run to verify failure** — `cd app/src-tauri && cargo test --offline file_viewer::resolve` → compile error (functions undefined).

- [ ] **Step 3: Implement** (top of `file_viewer/resolve.rs`, above the tests):

```rust
//! Turning what Claude printed into a container path that exists.
//!
//! Relative paths are the common case (Claude prints project-relative paths). The
//! terminal exec's cwd is `/workspace`, and each project path is mounted at
//! `/workspace/<mount_name>`, so those are the roots probed, in that order. The probe
//! is one exec as the container user and prints `realpath -e` of every candidate that
//! is a regular file: `fetch_container_file` refuses a symlink, so the registry must
//! hold the resolved path, not the one that was clicked.

use crate::commands::file_commands::validate_container_path;
use crate::docker::exec::exec_oneshot_streams_as;

pub const MAX_CANDIDATES: usize = 16;
const MAX_RAW_LEN: usize = 4096;

/// `$@` are the candidates. For each regular file, print its resolved path.
pub const PROBE_SCRIPT: &str = r#"for c in "$@"; do if test -f "$c"; then realpath -e -- "$c" 2>/dev/null; fi; done; exit 0"#;

pub fn candidate_paths(raw: &str, mount_names: &[String]) -> Result<Vec<String>, String> {
    if raw.is_empty() {
        return Err("The path is empty.".into());
    }
    if raw.len() > MAX_RAW_LEN {
        return Err("The path is too long.".into());
    }
    if raw.contains('\0') {
        return Err("The path contains a NUL byte.".into());
    }
    if raw.split('/').any(|seg| seg == "..") {
        return Err(format!("{} climbs out of its folder with `..`; refusing.", raw));
    }

    if raw.starts_with('/') {
        let normalised = collapse(raw);
        validate_container_path("File", &normalised)?;
        return Ok(vec![normalised]);
    }

    let rel = collapse(raw.strip_prefix("./").unwrap_or(raw));
    let rel = rel.trim_start_matches("./");
    if rel.is_empty() {
        return Err("The path is empty.".into());
    }

    let mut out: Vec<String> = Vec::new();
    let mut push = |candidate: String| {
        if out.len() < MAX_CANDIDATES && !out.contains(&candidate) {
            out.push(candidate);
        }
    };
    push(format!("/workspace/{}", rel));
    for mount in mount_names {
        if mount.is_empty() || mount.contains('/') || mount == "." || mount == ".." {
            continue;
        }
        push(format!("/workspace/{}/{}", mount, rel));
    }
    for c in &out {
        validate_container_path("File", c)?;
    }
    Ok(out)
}

/// `a//b/./c` → `a/b/c`. Never touches `..` (rejected before this runs).
fn collapse(path: &str) -> String {
    let absolute = path.starts_with('/');
    let joined = path
        .split('/')
        .filter(|seg| !seg.is_empty() && *seg != ".")
        .collect::<Vec<_>>()
        .join("/");
    if absolute { format!("/{}", joined) } else { joined }
}

/// One resolved path per line; anything that is not an absolute, valid container path is
/// dropped (the script's own diagnostics go to stderr, but a hostile `realpath` output is
/// still container-authored text).
pub fn parse_probe_output(stdout: &str) -> Vec<String> {
    let mut seen: Vec<String> = Vec::new();
    for line in stdout.lines() {
        let line = line.trim();
        if line.is_empty() || validate_container_path("File", line).is_err() {
            continue;
        }
        if !seen.iter().any(|s| s == line) {
            seen.push(line.to_string());
        }
    }
    seen
}

pub async fn probe_candidates(
    container_id: &str,
    candidates: &[String],
) -> Result<Vec<String>, String> {
    let mut cmd: Vec<String> = vec!["sh".into(), "-c".into(), PROBE_SCRIPT.into(), "probe".into()];
    cmd.extend(candidates.iter().cloned());
    let (stdout, _stderr, _code) =
        exec_oneshot_streams_as(container_id, "claude", cmd, Vec::new()).await?;
    Ok(parse_probe_output(&stdout))
}
```

Note the `"probe"` argument after the script: with `sh -c SCRIPT NAME ARGS…`, the first word is `$0`, so the candidates land in `$@`.

- [ ] **Step 4: Run tests** — `cargo test --offline file_viewer::resolve` → all pass. `cargo clippy --offline` clean.

- [ ] **Step 5: Commit** — `git add app/src-tauri/src/file_viewer/resolve.rs && git commit -m "feat(viewer): candidate paths and container probe for the file viewer"`

---

### Task 2: Rust — the window registry (`file_viewer/registry.rs`)

**Files:**
- Replace: `app/src-tauri/src/file_viewer/registry.rs` (Task 0's placeholder)

**Interfaces:** Produces

```rust
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct Location { pub line: Option<u32>, pub col: Option<u32>, pub end_line: Option<u32> }

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ViewerTargetState {
    Resolved { container_path: String },
    Choose { candidates: Vec<String> },
    NotFound { tried: Vec<String> },
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct ViewerTarget {
    pub project_id: String, pub project_name: String, pub raw_path: String,
    pub state: ViewerTargetState, pub initial: Location,
}

#[derive(Default)]
pub struct ViewerRegistry { /* Mutex<HashMap<String, ViewerTarget>>, AtomicU64 */ }
impl ViewerRegistry {
    pub fn reserve(&self, target: ViewerTarget) -> Result<String, String>;       // label, or cap error
    pub fn get(&self, label: &str) -> Option<ViewerTarget>;
    pub fn set_state(&self, label: &str, state: ViewerTargetState) -> Result<ViewerTarget, String>;
    pub fn remove(&self, label: &str);
    pub fn find_open(&self, project_id: &str, container_path: &str) -> Option<String>; // label
    pub fn len(&self) -> usize;
}
```

- [ ] **Step 1: Write the failing tests** (bottom of `registry.rs`):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn target(project: &str, path: &str) -> ViewerTarget {
        ViewerTarget {
            project_id: project.into(),
            project_name: "Demo".into(),
            raw_path: path.into(),
            state: ViewerTargetState::Resolved { container_path: path.into() },
            initial: Location { line: Some(3), col: None, end_line: None },
        }
    }

    #[test]
    fn labels_are_sequential_and_never_reused() {
        let r = ViewerRegistry::default();
        let a = r.reserve(target("p", "/workspace/a")).unwrap();
        let b = r.reserve(target("p", "/workspace/b")).unwrap();
        assert_eq!(a, "file-viewer-1");
        assert_eq!(b, "file-viewer-2");
        r.remove(&a);
        let c = r.reserve(target("p", "/workspace/c")).unwrap();
        assert_eq!(c, "file-viewer-3");
        assert_eq!(r.len(), 2);
    }

    #[test]
    fn the_cap_refuses_the_twenty_first_window() {
        let r = ViewerRegistry::default();
        for i in 0..MAX_VIEWER_WINDOWS {
            r.reserve(target("p", &format!("/workspace/{}", i))).unwrap();
        }
        let err = r.reserve(target("p", "/workspace/one-more")).unwrap_err();
        assert!(err.contains("20"), "{}", err);
        assert_eq!(r.len(), MAX_VIEWER_WINDOWS);
    }

    #[test]
    fn an_open_resolved_file_is_found_by_project_and_path() {
        let r = ViewerRegistry::default();
        let label = r.reserve(target("p", "/workspace/a")).unwrap();
        assert_eq!(r.find_open("p", "/workspace/a"), Some(label.clone()));
        assert_eq!(r.find_open("other", "/workspace/a"), None);
        // A window still choosing is not "open on" any path.
        r.set_state(&label, ViewerTargetState::Choose { candidates: vec!["/workspace/a".into()] }).unwrap();
        assert_eq!(r.find_open("p", "/workspace/a"), None);
        r.remove(&label);
        assert_eq!(r.get(&label), None);
    }

    #[test]
    fn set_state_on_an_unknown_label_is_an_error() {
        let r = ViewerRegistry::default();
        assert!(r.set_state("file-viewer-9", ViewerTargetState::NotFound { tried: vec![] }).is_err());
    }

    #[test]
    fn target_state_serialises_with_a_kind_tag() {
        let s = serde_json::to_string(&ViewerTargetState::NotFound { tried: vec!["/x".into()] }).unwrap();
        assert_eq!(s, r#"{"kind":"not_found","tried":["/x"]}"#);
    }
}
```

- [ ] **Step 2: Run to verify failure** — `cargo test --offline file_viewer::registry` → compile error.

- [ ] **Step 3: Implement** (top of `registry.rs`):

```rust
//! Which viewer window is looking at what.
//!
//! Managed with `app.manage(ViewerRegistry::default())` rather than as a field on
//! `AppState`, like the browser view keeps its own state. A label is reserved *before*
//! the window is built so two concurrent clicks cannot both pass the cap check.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use super::{MAX_VIEWER_WINDOWS, VIEWER_LABEL_PREFIX};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct Location {
    pub line: Option<u32>,
    pub col: Option<u32>,
    pub end_line: Option<u32>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ViewerTargetState {
    Resolved { container_path: String },
    Choose { candidates: Vec<String> },
    NotFound { tried: Vec<String> },
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct ViewerTarget {
    pub project_id: String,
    pub project_name: String,
    pub raw_path: String,
    pub state: ViewerTargetState,
    pub initial: Location,
}

#[derive(Default)]
pub struct ViewerRegistry {
    entries: Mutex<HashMap<String, ViewerTarget>>,
    next: AtomicU64,
}

impl ViewerRegistry {
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, ViewerTarget>> {
        self.entries.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn reserve(&self, target: ViewerTarget) -> Result<String, String> {
        let mut entries = self.lock();
        if entries.len() >= MAX_VIEWER_WINDOWS {
            return Err(format!(
                "{} file windows are already open — close one before opening another.",
                MAX_VIEWER_WINDOWS
            ));
        }
        let n = self.next.fetch_add(1, Ordering::SeqCst) + 1;
        let label = format!("{}{}", VIEWER_LABEL_PREFIX, n);
        entries.insert(label.clone(), target);
        Ok(label)
    }

    pub fn get(&self, label: &str) -> Option<ViewerTarget> {
        self.lock().get(label).cloned()
    }

    pub fn set_state(&self, label: &str, state: ViewerTargetState) -> Result<ViewerTarget, String> {
        let mut entries = self.lock();
        let entry = entries
            .get_mut(label)
            .ok_or_else(|| "This file window is no longer registered.".to_string())?;
        entry.state = state;
        Ok(entry.clone())
    }

    pub fn remove(&self, label: &str) {
        self.lock().remove(label);
    }

    pub fn find_open(&self, project_id: &str, container_path: &str) -> Option<String> {
        self.lock()
            .iter()
            .find(|(_, t)| {
                t.project_id == project_id
                    && matches!(&t.state, ViewerTargetState::Resolved { container_path: p } if p == container_path)
            })
            .map(|(label, _)| label.clone())
    }

    pub fn len(&self) -> usize {
        self.lock().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}
```

- [ ] **Step 4: Run tests** — `cargo test --offline file_viewer::registry` → pass; `cargo clippy --offline` clean.

- [ ] **Step 5: Commit** — `git add app/src-tauri/src/file_viewer && git commit -m "feat(viewer): window registry with cap, dedupe and sequential labels"`

---

### Task 3: Rust — hash, poll and write scripts (`file_viewer/poll.rs`, `file_viewer/write.rs`)

**Files:**
- Replace: `app/src-tauri/src/file_viewer/poll.rs`, `app/src-tauri/src/file_viewer/write.rs` (Task 0's placeholders)

**Interfaces:** Produces

```rust
// poll.rs
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct ViewerPoll { pub exists: bool, pub hash: Option<String>, pub size: Option<u64> }
pub const POLL_SCRIPT: &str;
pub fn parse_poll_output(code: i64, stdout: &str) -> ViewerPoll;
pub async fn poll_file(container_id: &str, container_path: &str) -> Result<ViewerPoll, String>;

// write.rs
pub const MAX_WRITE_BYTES: usize = 1024 * 1024;
pub fn sha256_hex(bytes: &[u8]) -> String;
pub fn is_sha256_hex(s: &str) -> bool;
pub const WRITE_SCRIPT: &str;
pub enum WriteOutcome { Saved(String), Conflict, Gone, Failed(String) }
pub fn classify_write(code: i64, stdout: &str, stderr: &str) -> WriteOutcome;
pub async fn write_file(container_id: &str, exec_manager: &ExecSessionManager, target: &str, bytes: &[u8], base_hash: &str) -> Result<String, String>;
```

- [ ] **Step 1: Write the failing tests** for `poll.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_present_file_yields_hash_and_size() {
        let out = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855  /workspace/x\n42\n";
        assert_eq!(
            parse_poll_output(0, out),
            ViewerPoll {
                exists: true,
                hash: Some("e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855".into()),
                size: Some(42)
            }
        );
    }

    #[test]
    fn exit_four_means_gone() {
        assert_eq!(parse_poll_output(4, ""), ViewerPoll { exists: false, hash: None, size: None });
    }

    #[test]
    fn garbage_is_not_a_hash() {
        let p = parse_poll_output(0, "not a hash  /x\nabc\n");
        assert_eq!(p, ViewerPoll { exists: true, hash: None, size: None });
    }

    #[test]
    fn the_script_tests_existence_before_hashing() {
        assert!(POLL_SCRIPT.contains("test -f \"$1\" || exit 4"));
        assert!(POLL_SCRIPT.contains("sha256sum -- \"$1\""));
        assert!(POLL_SCRIPT.contains("stat -c %s -- \"$1\""));
    }
}
```

And for `write.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_matches_coreutils() {
        // `printf 'hello\n' | sha256sum`
        assert_eq!(
            sha256_hex(b"hello\n"),
            "5891b5b522d5df086d0ff0b110fbd9d21bb4fc7163af34d08286a2e846f6be03"
        );
        assert!(is_sha256_hex(&sha256_hex(b"")));
        assert!(!is_sha256_hex("ABC"));
        assert!(!is_sha256_hex(&"g".repeat(64)));
    }

    #[test]
    fn exit_codes_map_to_outcomes() {
        let h = "5891b5b522d5df086d0ff0b110fbd9d21bb4fc7163af34d08286a2e846f6be03";
        assert!(matches!(classify_write(0, &format!("{}  /x\n", h), ""), WriteOutcome::Saved(s) if s == h));
        assert!(matches!(classify_write(3, "", ""), WriteOutcome::Conflict));
        assert!(matches!(classify_write(4, "", ""), WriteOutcome::Gone));
        assert!(matches!(classify_write(1, "", "cp: Permission denied"), WriteOutcome::Failed(m) if m.contains("Permission denied")));
        // Success without a parseable hash is still a failure: the editor's base would be wrong.
        assert!(matches!(classify_write(0, "junk", ""), WriteOutcome::Failed(_)));
    }

    #[test]
    fn the_write_script_checks_then_swaps_and_always_cleans_up() {
        for needle in [
            "test -f \"$target\" || exit 4",
            "exit 3",
            "chmod --reference=\"$target\"",
            "mv -f --",
            "cat -- \"$tmp\" > \"$target\"",
            "trap 'rm -f -- \"$tmp\"' EXIT",
        ] {
            assert!(WRITE_SCRIPT.contains(needle), "missing: {}", needle);
        }
    }
}
```

- [ ] **Step 2: Run to verify failure** — `cargo test --offline file_viewer::poll file_viewer::write` → compile errors.

- [ ] **Step 3: Implement `poll.rs`**

```rust
//! One cheap exec per tick: the file's full hash and size, or "gone".
//!
//! This is what the 2 s poll asks, instead of re-downloading up to 1 MiB of archive per
//! window per tick. The hash is coreutils `sha256sum`, which equals `write::sha256_hex`
//! of the bytes whenever the read was not truncated — the only case in which the
//! editor uses a hash as its save base.

use serde::Serialize;

use crate::docker::exec::exec_oneshot_streams_as;

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct ViewerPoll {
    pub exists: bool,
    pub hash: Option<String>,
    pub size: Option<u64>,
}

pub const POLL_SCRIPT: &str =
    r#"test -f "$1" || exit 4; sha256sum -- "$1" && stat -c %s -- "$1""#;

pub fn parse_poll_output(code: i64, stdout: &str) -> ViewerPoll {
    if code == 4 {
        return ViewerPoll { exists: false, hash: None, size: None };
    }
    let mut lines = stdout.lines();
    let hash = lines
        .next()
        .and_then(|l| l.split_whitespace().next())
        .filter(|h| super::write::is_sha256_hex(h))
        .map(str::to_string);
    let size = lines.next().and_then(|l| l.trim().parse::<u64>().ok());
    ViewerPoll { exists: true, hash, size }
}

pub async fn poll_file(container_id: &str, container_path: &str) -> Result<ViewerPoll, String> {
    let cmd = vec![
        "sh".to_string(),
        "-c".to_string(),
        POLL_SCRIPT.to_string(),
        "poll".to_string(),
        container_path.to_string(),
    ];
    let (stdout, stderr, code) =
        exec_oneshot_streams_as(container_id, "claude", cmd, Vec::new()).await?;
    if code != 0 && code != 4 {
        return Err(format!(
            "Could not check the file: {}",
            crate::commands::file_commands::clip_container_text(&stderr)
        ));
    }
    Ok(parse_poll_output(code, &stdout))
}
```

- [ ] **Step 4: Implement `write.rs`**

```rust
//! Saving: stage in `/tmp`, then swap in as the container user.
//!
//! The Docker archive API writes as root, so it is used for exactly one thing — landing
//! the payload at `/tmp/triple-c-viewer-<uuid>`, owned by the container user (the
//! existing `write_file_to_container`). Everything that touches the *target directory*
//! runs in an exec as `claude`, so a save can do nothing the user's own shell could not.
//! A non-root process cannot `chown`, so the saved file is owned by the container user,
//! as it would be after Claude Code edited it; mode is kept with `chmod --reference`.

use sha2::{Digest, Sha256};

use crate::commands::file_commands::clip_container_text;
use crate::docker::exec::{exec_oneshot_streams_as, ExecSessionManager};

/// Spec §4/§5: only untruncated (≤ 1 MiB) text is editable, so nothing larger is saved.
pub const MAX_WRITE_BYTES: usize = 1024 * 1024;

pub fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|b| format!("{:02x}", b)).collect()
}

pub fn is_sha256_hex(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// `$1` target, `$2` staged payload in /tmp, `$3` the hash the editor loaded from.
/// Exit 3 = changed on disk, 4 = gone; stdout on success is `sha256sum` of the target.
pub const WRITE_SCRIPT: &str = r#"target=$1; tmp=$2; expect=$3
trap 'rm -f -- "$tmp"' EXIT
test -f "$target" || exit 4
actual=$(sha256sum -- "$target" | cut -d' ' -f1) || exit 1
[ "$actual" = "$expect" ] || exit 3
dir=$(dirname -- "$target"); name=$(basename -- "$target")
if [ -w "$dir" ]; then
  staged="$dir/.$name.triple-c-$$"
  cp -- "$tmp" "$staged" || exit 1
  chmod --reference="$target" "$staged" 2>/dev/null
  mv -f -- "$staged" "$target" || { rm -f -- "$staged"; exit 1; }
else
  cat -- "$tmp" > "$target" || exit 1
fi
sha256sum -- "$target""#;

pub enum WriteOutcome {
    Saved(String),
    Conflict,
    Gone,
    Failed(String),
}

pub fn classify_write(code: i64, stdout: &str, stderr: &str) -> WriteOutcome {
    match code {
        3 => WriteOutcome::Conflict,
        4 => WriteOutcome::Gone,
        0 => match stdout.split_whitespace().next().filter(|h| is_sha256_hex(h)) {
            Some(h) => WriteOutcome::Saved(h.to_string()),
            None => WriteOutcome::Failed("The container did not report the saved file's hash.".into()),
        },
        _ => WriteOutcome::Failed(clip_container_text(stderr)),
    }
}

pub async fn write_file(
    container_id: &str,
    exec_manager: &ExecSessionManager,
    target: &str,
    bytes: &[u8],
    base_hash: &str,
) -> Result<String, String> {
    if bytes.len() > MAX_WRITE_BYTES {
        return Err("Files over 1 MiB are read-only in the viewer.".into());
    }
    if !is_sha256_hex(base_hash) {
        return Err("The editor's base hash is malformed; reload the file.".into());
    }
    let tmp_name = format!("triple-c-viewer-{}", uuid::Uuid::new_v4().simple());
    let tmp_path = exec_manager
        .write_file_to_container(container_id, &tmp_name, bytes)
        .await?;
    let cmd = vec![
        "sh".to_string(),
        "-c".to_string(),
        WRITE_SCRIPT.to_string(),
        "save".to_string(),
        target.to_string(),
        tmp_path,
        base_hash.to_string(),
    ];
    let (stdout, stderr, code) =
        exec_oneshot_streams_as(container_id, "claude", cmd, Vec::new()).await?;
    match classify_write(code, &stdout, &stderr) {
        WriteOutcome::Saved(hash) => Ok(hash),
        WriteOutcome::Conflict => Err("conflict: the file changed on disk since it was loaded.".into()),
        WriteOutcome::Gone => Err("gone: the file no longer exists.".into()),
        WriteOutcome::Failed(msg) => Err(format!("Could not save the file: {}", msg)),
    }
}
```

- [ ] **Step 5: Run tests + clippy** — `cargo test --offline file_viewer` → pass; `cargo clippy --offline` clean.

- [ ] **Step 6 (optional, Docker present here): exercise the script against a stock container** — copy `WRITE_SCRIPT` to the scratchpad as `w.sh`, then:
  ```bash
  docker run --rm -v "$PWD":/s -w /tmp ubuntu:24.04 sh -c '
    printf "old\n" > t.txt; printf "new\n" > payload; h=$(sha256sum t.txt | cut -d" " -f1)
    sh /s/w.sh save /tmp/t.txt /tmp/payload "$h"; echo "exit=$?"; cat t.txt; ls payload 2>&1
    sh /s/w.sh save /tmp/t.txt /tmp/payload "$h"; echo "stale-base exit=$?"'
  ```
  Expected: first run prints the new hash and `exit=0`, `t.txt` is `new`, `payload` is gone; the second run (payload gone, hash stale) exits 3 or 1 — both non-zero, neither touches the file.

- [ ] **Step 7: Commit** — `git add app/src-tauri/src/file_viewer && git commit -m "feat(viewer): poll and save scripts run as the container user"`

---

### Task 4: Frontend — the path matcher (`lib/filePathLinks.ts`)

**Files:**
- Create: `app/src/lib/filePathLinks.ts`, `app/src/lib/filePathLinks.test.ts`

**Interfaces:** Produces

```ts
export interface FilePathMatch {
  /** Indices into the input string; `end` exclusive. Covers the path + any :line suffix, not the wrapping. */
  start: number; end: number;
  path: string;
  line?: number; col?: number; endLine?: number;
}
export function findFilePathLinks(text: string): FilePathMatch[];
```

- [ ] **Step 1: Write the failing tests** (`filePathLinks.test.ts`):

```ts
import { describe, expect, it } from "vitest";
import { findFilePathLinks } from "./filePathLinks";

const one = (text: string) => {
  const m = findFilePathLinks(text);
  expect(m, text).toHaveLength(1);
  return m[0];
};

describe("findFilePathLinks — what is a path", () => {
  it.each([
    ["src/foo.ts", "src/foo.ts"],
    ["/workspace/x/README.md", "/workspace/x/README.md"],
    ["./scripts/build.sh", "./scripts/build.sh"],
    ["../other/Cargo.toml", "../other/Cargo.toml"],
    ["Makefile", "Makefile"],
    ["Dockerfile", "Dockerfile"],
    ["CLAUDE.md", "CLAUDE.md"],
    [".gitignore", ".gitignore"],
    ["app/src-tauri/src/lib.rs", "app/src-tauri/src/lib.rs"],
    ["my-dir/some_file.test.tsx", "my-dir/some_file.test.tsx"],
  ])("matches %s", (text, path) => {
    expect(one(text).path).toBe(path);
  });

  it.each([
    "1.2.3",
    "v2.11.0",
    "example.com",
    "claude.ai",
    "e.g.",
    "https://example.com/a/b.ts",
    "http://localhost:1420/viewer.html",
    "foo",
    "a.b",
    "10.0.0.1",
  ])("does not match %s", (text) => {
    expect(findFilePathLinks(text)).toEqual([]);
  });

  it("matches a slash-less token only with a known source/doc extension", () => {
    expect(one("index.ts").path).toBe("index.ts");
    expect(one("notes.md").path).toBe("notes.md");
    expect(findFilePathLinks("archive.xyz")).toEqual([]);
    // With a slash, any extension will do.
    expect(one("dist/archive.xyz").path).toBe("dist/archive.xyz");
  });
});

describe("findFilePathLinks — line and column suffixes", () => {
  it("parses :line", () => {
    expect(one("src/foo.ts:42")).toMatchObject({ path: "src/foo.ts", line: 42 });
  });
  it("parses :line:col", () => {
    expect(one("src/foo.ts:42:7")).toMatchObject({ path: "src/foo.ts", line: 42, col: 7 });
  });
  it("parses :start-end", () => {
    expect(one("app/src/lib/urlRelay.ts:139-150")).toMatchObject({ path: "app/src/lib/urlRelay.ts", line: 139, endLine: 150 });
  });
  it("parses #L42 and #L40-L50", () => {
    expect(one("README.md#L42")).toMatchObject({ path: "README.md", line: 42 });
    expect(one("README.md#L40-L50")).toMatchObject({ path: "README.md", line: 40, endLine: 50 });
  });
  it("does not read a trailing colon as a line", () => {
    expect(one("Edited src/foo.ts:")).toMatchObject({ path: "src/foo.ts", line: undefined });
  });
});

describe("findFilePathLinks — markdown wrapping and offsets", () => {
  it.each([
    ["`src/foo.ts`", 1, 11],
    ["(src/foo.ts)", 1, 11],
    ["[src/foo.ts]", 1, 11],
    ['"src/foo.ts"', 1, 11],
    ["'src/foo.ts'", 1, 11],
    ["see src/foo.ts.", 4, 14],
    ["see src/foo.ts, then", 4, 14],
    ["see src/foo.ts;", 4, 14],
  ])("strips wrapping in %s", (text, start, end) => {
    expect(one(text)).toMatchObject({ path: "src/foo.ts", start, end });
  });

  it("keeps the :line suffix inside the span", () => {
    // "at `" is 4 characters; the span covers `src/foo.ts:42` (13 chars).
    expect(one("at `src/foo.ts:42`")).toMatchObject({ path: "src/foo.ts", line: 42, start: 4, end: 17 });
  });

  it("finds several paths in one line, in order", () => {
    const m = findFilePathLinks("Read src/a.ts and src/b.rs:3, wrote docs/c.md");
    expect(m.map((x) => x.path)).toEqual(["src/a.ts", "src/b.rs", "docs/c.md"]);
    expect(m[1].line).toBe(3);
  });

  it("skips anything inside a URL", () => {
    expect(findFilePathLinks("see https://github.com/o/r/blob/main/src/foo.ts:12 now")).toEqual([]);
    expect(one("see https://x.io/a and src/foo.ts").path).toBe("src/foo.ts");
  });

  it("ignores a Claude tool header like ⏺ Read(src/foo.ts) except for the path", () => {
    expect(one("⏺ Read(src/foo.ts)").path).toBe("src/foo.ts");
  });
});
```

- [ ] **Step 2: Run to verify failure** — `cd app && npx vitest run src/lib/filePathLinks.test.ts` → fails (module not found).

- [ ] **Step 3: Implement** `app/src/lib/filePathLinks.ts`:

```ts
/**
 * Finds file paths in a line of terminal text.
 *
 * Pure: the xterm glue (`components/terminal/filePathLinkProvider.ts`) turns
 * buffer rows into a string and string offsets back into cells; this decides
 * what a path is. Deliberately conservative — a false link is an annoying
 * underline, a missed one is a copy-paste — so a token needs either a `/` or
 * a known extension, and never sits inside a URL.
 */

export interface FilePathMatch {
  /** Indices into the input; `end` exclusive. Covers path + suffix, not wrapping. */
  start: number;
  end: number;
  path: string;
  line?: number;
  col?: number;
  endLine?: number;
}

/** Extensions that make a slash-less token (`index.ts`, `notes.md`) a path. */
const KNOWN_EXTENSIONS = new Set([
  "md", "markdown", "txt", "rst", "json", "jsonc", "yaml", "yml", "toml", "ini", "cfg", "conf",
  "env", "lock", "js", "jsx", "mjs", "cjs", "ts", "tsx", "rs", "py", "rb", "go", "java", "kt",
  "c", "h", "cc", "cpp", "hpp", "cs", "php", "swift", "scala", "lua", "sh", "bash", "zsh",
  "fish", "ps1", "html", "htm", "xml", "svelte", "vue", "css", "scss", "sass", "less", "sql",
  "graphql", "proto", "diff", "patch", "csv", "tsv", "log", "svg", "png", "jpg", "jpeg", "gif",
  "webp",
]);

/** Extensionless names that are files by convention. */
const KNOWN_BASENAMES = new Set([
  "Makefile", "Dockerfile", "Rakefile", "Gemfile", "Procfile", "Vagrantfile", "LICENSE",
  "README", "CHANGELOG", "PKGBUILD",
]);

/**
 * A candidate token: path characters, optionally starting with `/`, `./`, `../`
 * or `.` (dotfile). Excludes the wrapping characters the surrounding markdown
 * leaves (`(`, `)`, `[`, `]`, backtick, quotes) and whitespace.
 */
const TOKEN = /(?:\.{1,2}\/|\/)?[A-Za-z0-9_.\-~+@]+(?:\/[A-Za-z0-9_.\-~+@]+)*\/?/g;
const URL_SCHEME = /[a-z][a-z0-9+.-]*:\/\//gi;
const LINE_SUFFIX = /^(?::(\d+)(?::(\d+))?(?:-(\d+))?|#L(\d+)(?:-L?(\d+))?)/;
const VERSION_LIKE = /^v?\d+(\.\d+)+$/;
const TRAILING_PUNCT = /[.,;:]+$/;

function isPathLike(token: string): boolean {
  if (VERSION_LIKE.test(token)) return false;
  const base = token.slice(token.lastIndexOf("/") + 1);
  if (base === "" || base === "." || base === "..") return false;
  if (token.includes("/")) {
    // With a slash the token is a path unless it is only dots and digits.
    return /[A-Za-z_]/.test(token);
  }
  if (KNOWN_BASENAMES.has(base)) return true;
  const dot = base.lastIndexOf(".");
  if (dot === 0) return KNOWN_EXTENSIONS.has(base.slice(1).toLowerCase()) || base === ".gitignore" || base === ".env";
  if (dot < 0) return false;
  return KNOWN_EXTENSIONS.has(base.slice(dot + 1).toLowerCase());
}

function urlSpans(text: string): Array<[number, number]> {
  const spans: Array<[number, number]> = [];
  for (const m of text.matchAll(URL_SCHEME)) {
    const start = m.index ?? 0;
    // A URL runs to the next whitespace or closing bracket/quote.
    const rest = text.slice(start);
    const len = rest.search(/[\s)\]'"`>]/);
    spans.push([start, len < 0 ? text.length : start + len]);
  }
  return spans;
}

export function findFilePathLinks(text: string): FilePathMatch[] {
  const urls = urlSpans(text);
  const insideUrl = (i: number) => urls.some(([s, e]) => i >= s && i < e);
  const out: FilePathMatch[] = [];

  for (const m of text.matchAll(TOKEN)) {
    let start = m.index ?? 0;
    let token = m[0];
    if (insideUrl(start)) continue;

    // Trailing sentence punctuation is not part of the name.
    const trimmed = token.replace(TRAILING_PUNCT, "");
    if (trimmed !== token) token = trimmed;
    if (token.endsWith("/")) token = token.slice(0, -1);
    if (!token || !isPathLike(token)) continue;

    let end = start + token.length;
    const match: FilePathMatch = { start, end, path: token };

    // The suffix sits right after the *trimmed* token: `TOKEN` may have
    // consumed a trailing `.` that `TRAILING_PUNCT` then removed, so search
    // from `start + token.length`, not from the end of the raw match.
    const after = text.slice(start + token.length);
    const s = LINE_SUFFIX.exec(after);
    if (s) {
      if (s[1] !== undefined) {
        match.line = Number(s[1]);
        if (s[2] !== undefined) match.col = Number(s[2]);
        if (s[3] !== undefined) match.endLine = Number(s[3]);
      } else if (s[4] !== undefined) {
        match.line = Number(s[4]);
        if (s[5] !== undefined) match.endLine = Number(s[5]);
      }
      end += s[0].length;
      match.end = end;
    }
    out.push(match);
  }
  return out;
}
```

**Why this works:** the `:42` in `src/foo.ts:42` is not consumed by `TOKEN` because `:` is not a token character, so the suffix regex sees it; `#L42` likewise (`#` is not a token character). For `Edited src/foo.ts:` the suffix regex needs a digit after `:` and finds none, so `line` stays undefined and the span ends before the colon. `let start` is `let` only because `m.index` is typed optional; it is never reassigned — make it `const` if the linter asks.

- [ ] **Step 4: Run tests** — `npx vitest run src/lib/filePathLinks.test.ts` → all pass. Adjust `TOKEN`/`isPathLike` until the table passes; do not weaken the negative cases.

- [ ] **Step 5: Commit** — `git add app/src/lib/filePathLinks.ts app/src/lib/filePathLinks.test.ts && git commit -m "feat(viewer): pure file-path matcher for terminal text"`

---

### Task 5: Frontend — wrapped-row joining (`lib/xtermLineJoin.ts`)

**Files:**
- Create: `app/src/lib/xtermLineJoin.ts`, `app/src/lib/xtermLineJoin.test.ts`

**Interfaces:** Produces

```ts
export interface JoinedLine {
  text: string;
  /** 0-based index of the first buffer row that contributed. */
  firstRow: number;
  /** For each contributed row (in order), the string offset at which it starts. */
  rowStarts: number[];
}
/** Minimal slice of xterm's IBuffer this needs. */
export interface RowSource {
  getLine(y: number): { isWrapped: boolean; translateToString(trimRight?: boolean): string } | undefined;
}
export function joinWrappedRows(buffer: RowSource, row: number): JoinedLine;
/** String offset → 1-based {x, y} cell (y is the buffer row + 1). */
export function offsetToCell(joined: JoinedLine, offset: number): { x: number; y: number };
export const MAX_JOINED_LENGTH = 2048;
```

- [ ] **Step 1: Write the failing tests**

```ts
import { describe, expect, it } from "vitest";
import { joinWrappedRows, offsetToCell, type RowSource } from "./xtermLineJoin";

/** rows[i] = [text, isWrapped] */
const buffer = (rows: Array<[string, boolean]>): RowSource => ({
  getLine: (y) =>
    rows[y] ? { isWrapped: rows[y][1], translateToString: (trim?: boolean) => (trim ? rows[y][0].trimEnd() : rows[y][0]) } : undefined,
});

describe("joinWrappedRows", () => {
  it("returns a single unwrapped row as-is", () => {
    const j = joinWrappedRows(buffer([["hello src/a.ts", false]]), 0);
    expect(j).toEqual({ text: "hello src/a.ts", firstRow: 0, rowStarts: [0] });
  });

  it("walks up to the row that started the wrap and down through continuations", () => {
    const b = buffer([
      ["unrelated", false],
      ["/workspace/very/long/pa", false],
      ["th/to/file.ts:12 and mo", true],
      ["re text", true],
      ["next line", false],
    ]);
    const fromMiddle = joinWrappedRows(b, 2);
    expect(fromMiddle.text).toBe("/workspace/very/long/path/to/file.ts:12 and more text");
    expect(fromMiddle.firstRow).toBe(1);
    expect(fromMiddle.rowStarts).toEqual([0, 23, 46]);
    expect(joinWrappedRows(b, 1)).toEqual(fromMiddle);
    expect(joinWrappedRows(b, 3)).toEqual(fromMiddle);
  });

  it("stops at the length budget", () => {
    const rows: Array<[string, boolean]> = [["a".repeat(1000), false]];
    for (let i = 0; i < 5; i++) rows.push(["b".repeat(1000), true]);
    const j = joinWrappedRows(buffer(rows), 0);
    expect(j.text.length).toBeLessThanOrEqual(3000);
    expect(j.rowStarts.length).toBeLessThanOrEqual(3);
  });
});

describe("offsetToCell", () => {
  it("maps offsets to 1-based cells on the right row", () => {
    const j = { text: "abcdefgh", firstRow: 4, rowStarts: [0, 3, 6] };
    expect(offsetToCell(j, 0)).toEqual({ x: 1, y: 5 });
    expect(offsetToCell(j, 2)).toEqual({ x: 3, y: 5 });
    expect(offsetToCell(j, 3)).toEqual({ x: 1, y: 6 });
    expect(offsetToCell(j, 7)).toEqual({ x: 2, y: 7 });
  });
});
```

- [ ] **Step 2: Run to verify failure** — `npx vitest run src/lib/xtermLineJoin.test.ts` → module not found.

- [ ] **Step 3: Implement**

```ts
/**
 * Joins an xterm buffer row with its wrapped continuations.
 *
 * `WebLinksAddon` does the same in its private `LinkComputer`, which the
 * built package does not export — so the walk is repeated here, with the same
 * 2048-character budget. Rows are read with `translateToString(true)`, which
 * trims the right edge; a wrap never ends in trailing spaces xterm would keep,
 * so the join is exact for the text a path can occur in.
 *
 * Wide characters (CJK, emoji) occupy two cells but one string index, so a
 * column computed from a string offset drifts right of the glyph on such rows.
 * The addon corrects this with `getCell`; v1 accepts the drift (underline
 * lands a cell early; the click still resolves the same link).
 */

export const MAX_JOINED_LENGTH = 2048;

export interface RowSource {
  getLine(y: number): { isWrapped: boolean; translateToString(trimRight?: boolean): string } | undefined;
}

export interface JoinedLine {
  text: string;
  firstRow: number;
  rowStarts: number[];
}

export function joinWrappedRows(buffer: RowSource, row: number): JoinedLine {
  let top = row;
  while (top > 0 && buffer.getLine(top)?.isWrapped) top--;

  const parts: string[] = [];
  let length = 0;
  let y = top;
  for (;;) {
    const line = buffer.getLine(y);
    if (!line) break;
    if (y !== top && !line.isWrapped) break;
    const text = line.translateToString(true);
    if (length + text.length > MAX_JOINED_LENGTH && parts.length > 0) break;
    parts.push(text);
    length += text.length;
    y++;
  }

  const rowStarts: number[] = [];
  let offset = 0;
  for (const p of parts) {
    rowStarts.push(offset);
    offset += p.length;
  }
  return { text: parts.join(""), firstRow: top, rowStarts };
}

export function offsetToCell(joined: JoinedLine, offset: number): { x: number; y: number } {
  let rowIdx = 0;
  for (let i = 0; i < joined.rowStarts.length; i++) {
    if (joined.rowStarts[i] <= offset) rowIdx = i;
  }
  return { x: offset - joined.rowStarts[rowIdx] + 1, y: joined.firstRow + rowIdx + 1 };
}
```

- [ ] **Step 4: Run tests** → pass. `npx tsc --noEmit` → exit 0.

- [ ] **Step 5: Commit** — `git add app/src/lib/xtermLineJoin.ts app/src/lib/xtermLineJoin.test.ts && git commit -m "feat(viewer): join wrapped xterm rows for link matching"`

---

### Task 6: Second Vite entry, capability file, CodeMirror dependencies

**Files:**
- Create: `app/viewer.html`, `app/src/viewer/main.tsx`, `app/src/viewer/ViewerApp.tsx` (placeholder that Task 11 replaces), `app/src-tauri/capabilities/file-viewer.json`
- Modify: `app/vite.config.ts`, `app/package.json` (+ lockfile via `npm install`), `app/src-tauri/src/file_viewer/mod.rs` (test)

**Interfaces:** Produces the entry and the window's capability. Task 8 opens `WebviewUrl::App("viewer.html")`; Task 10/11 fill `src/viewer/`.

- [ ] **Step 1: Install CodeMirror** (from `app/`):

```bash
npm install @codemirror/state @codemirror/view @codemirror/commands @codemirror/search @codemirror/language @codemirror/lang-markdown @codemirror/lang-javascript @codemirror/lang-rust @codemirror/lang-python @codemirror/lang-json @codemirror/lang-yaml @codemirror/lang-css @codemirror/lang-html @codemirror/legacy-modes @lezer/highlight
```

Expected versions (as of review): state 6.7.x, view 6.43.x, commands 6.11.x, search 6.7.x, language 6.12.x, legacy-modes 6.5.x, @lezer/highlight 1.2.x. Verify with `grep -rl "new Function\|eval(" node_modules/@codemirror/*/dist/index.js node_modules/@lezer/*/dist/index.js` → no output (the only known hit is `legacy-modes/mode/pug.js`, which is never imported).

- [ ] **Step 2: Create `app/viewer.html`** (no inline `<style>`, no inline script — see Global Constraints):

```html
<!DOCTYPE html>
<html lang="en">
  <head>
    <meta charset="UTF-8" />
    <link rel="icon" type="image/svg+xml" href="/favicon.svg" />
    <meta name="viewport" content="width=device-width, initial-scale=1.0" />
    <title>Triple-C — file</title>
  </head>
  <body>
    <div id="root"></div>
    <script type="module" src="/src/viewer/main.tsx"></script>
  </body>
</html>
```

- [ ] **Step 3: Create `app/src/viewer/main.tsx`** and a placeholder `ViewerApp.tsx`:

```tsx
// main.tsx
import React from "react";
import ReactDOM from "react-dom/client";
import ViewerApp from "./ViewerApp";
import "../index.css";

ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    <ViewerApp />
  </React.StrictMode>,
);
```

```tsx
// ViewerApp.tsx (placeholder; Task 11 replaces the body)
export default function ViewerApp() {
  return <div className="p-4 text-sm text-[var(--text-secondary)]">Loading…</div>;
}
```

- [ ] **Step 4: Register the entry in `app/vite.config.ts`** — add `import { fileURLToPath } from "node:url";` at the top and a `build` block inside `defineConfig({ … })`:

```ts
  build: {
    rollupOptions: {
      input: {
        main: fileURLToPath(new URL("index.html", import.meta.url)),
        viewer: fileURLToPath(new URL("viewer.html", import.meta.url)),
      },
    },
  },
```

- [ ] **Step 5: Create `app/src-tauri/capabilities/file-viewer.json`**

```json
{
  "identifier": "file-viewer",
  "description": "The terminal file viewer windows (`file-viewer-<n>`, opened by `open_file_viewer` on the app's own `viewer.html`). Same rules as `default.json`: app commands need no entry here and are gated by label inside `commands/file_viewer_commands.rs`; this file is the plugin-command surface a compromised viewer webview could reach, and it is the smallest one that lets the window work. `core:event:allow-listen`/`allow-unlisten` are for `file-viewer-goto` (Rust → this window; the viewer subscribes through `getCurrentWindow().listen`, because a bare `listen()` in *any* window receives an `emit_to`). `core:window:allow-destroy` is not optional: `getCurrentWindow().onCloseRequested` in @tauri-apps/api 2.11 makes Rust `prevent_close()` whenever a JS listener exists and then calls `destroy()` itself, so without this grant the window's X button does nothing once the unsaved-changes guard is installed. `allow-close` is deliberately absent — nothing calls it, and `destroy` is the only exit. No `set-title`/`set-focus`/`unminimize`: those are done from Rust when a second click targets an already-open file. `core:webview:allow-internal-toggle-devtools` is the same dev-only convenience `default.json` carries.",
  "windows": ["file-viewer-*"],
  "permissions": [
    "core:event:allow-listen",
    "core:event:allow-unlisten",
    "core:window:allow-destroy",
    "core:webview:allow-internal-toggle-devtools"
  ]
}
```

- [ ] **Step 6: Add the fallback-trap test** to `app/src-tauri/src/file_viewer/mod.rs`'s test module:

```rust
    /// Both Vite's dev server and Tauri's asset lookup fall back to `index.html`
    /// when `viewer.html` is missing, so a broken entry opens the *main app* in
    /// the viewer window with no error anywhere. Pin the two files the entry needs.
    #[test]
    fn the_viewer_entry_exists_and_is_a_vite_input() {
        let app_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let html = std::fs::read_to_string(app_dir.join("viewer.html")).expect("app/viewer.html");
        assert!(html.contains("/src/viewer/main.tsx"));
        assert!(!html.contains("<style"), "an inline <style> makes Tauri add a style nonce, which disables 'unsafe-inline' and breaks CodeMirror");
        let vite = std::fs::read_to_string(app_dir.join("vite.config.ts")).expect("vite.config.ts");
        assert!(vite.contains("viewer.html"), "vite.config.ts must list viewer.html in build.rollupOptions.input");
        let cap = std::fs::read_to_string(app_dir.join("src-tauri/capabilities/file-viewer.json")).expect("capability");
        assert!(cap.contains("\"file-viewer-*\"") && cap.contains("core:window:allow-destroy"));
    }
```

- [ ] **Step 7: Verify** — `cd app && npm run build` → `dist/viewer.html` exists (`ls dist/viewer.html`); `npx tsc --noEmit` → 0; `cd src-tauri && cargo test --offline file_viewer::tests` → pass (the capability file is validated by `tauri_build` at `cargo check` time — a typo in a permission identifier fails the build).

- [ ] **Step 8: Commit** — `git add app/viewer.html app/src/viewer app/vite.config.ts app/package.json app/package-lock.json app/src-tauri/capabilities/file-viewer.json app/src-tauri/src/file_viewer/mod.rs && git commit -m "feat(viewer): second Vite entry, viewer capability, CodeMirror deps"`

---

### Task 7: Frontend — viewer state machine, editability, languages

**Files:**
- Create: `app/src/viewer/viewerState.ts` + `.test.ts`, `app/src/viewer/editability.ts` + `.test.ts`, `app/src/viewer/languages.ts` + `.test.ts`

**Interfaces:** Produces

```ts
// viewerState.ts
export type DocStatus = "clean" | "dirty";
export type DiskStatus = "same" | "changed" | "gone";
export interface ViewerDocState {
  doc: DocStatus;
  disk: DiskStatus;
  /** Hash the buffer was loaded from / last saved as. */
  baseHash: string | null;
  /** Last known full-file hash on disk (null until known). */
  diskHash: string | null;
  containerDown: boolean;
  /** Set for one render after a clean reload; UI shows "Reloaded". */
  justReloaded: boolean;
  /** True when the user chose "Overwrite on save" after a disk change. */
  overwrite: boolean;
}
export type ViewerAction =
  | { type: "loaded"; hash: string; truncated: boolean }
  | { type: "edited" }
  | { type: "polled"; poll: ViewerPoll }
  | { type: "poll_failed" }
  | { type: "reloaded"; hash: string }
  | { type: "overwrite_on_save" }
  | { type: "saved"; hash: string }
  | { type: "save_conflict" }
  | { type: "save_gone" };
export const initialViewerState: ViewerDocState;
export function reduceViewer(state: ViewerDocState, action: ViewerAction): ViewerDocState;
/** What EditorPane does after a poll: nothing, reload silently, or show the banner. */
export function pollEffect(before: ViewerDocState, after: ViewerDocState): "none" | "reload" | "banner";
export function canSave(state: ViewerDocState, editable: boolean): boolean;

// editability.ts
export type ViewerKind = "text" | "image" | "binary";
export interface Editability { kind: ViewerKind; editable: boolean; reason: string | null }
export function classifyViewerFile(path: string, file: ViewerFile, bytes: Uint8Array): Editability;

// languages.ts
export function languageFor(path: string): Promise<Extension | null>;
export function wrapsLines(path: string): boolean;
```

- [ ] **Step 1: Write the failing reducer tests** (`viewerState.test.ts`):

```ts
import { describe, expect, it } from "vitest";
import { canSave, initialViewerState, pollEffect, reduceViewer, type ViewerDocState } from "./viewerState";

const H1 = "1".repeat(64);
const H2 = "2".repeat(64);
const loaded = (truncated = false): ViewerDocState =>
  reduceViewer(initialViewerState, { type: "loaded", hash: H1, truncated });
const poll = (s: ViewerDocState, hash: string | null, exists = true) =>
  reduceViewer(s, { type: "polled", poll: { exists, hash, size: exists ? 1 : null } });

describe("reduceViewer", () => {
  it("seeds both hashes from an untruncated load", () => {
    expect(loaded()).toMatchObject({ doc: "clean", disk: "same", baseHash: H1, diskHash: H1 });
  });
  it("leaves diskHash unknown after a truncated load, so the first poll seeds it silently", () => {
    const s = loaded(true);
    expect(s.diskHash).toBeNull();
    const after = poll(s, H2);
    expect(after).toMatchObject({ disk: "same", diskHash: H2 });
    expect(pollEffect(s, after)).toBe("none");
  });
  it("an unchanged poll is a no-op", () => {
    const s = loaded();
    expect(pollEffect(s, poll(s, H1))).toBe("none");
  });
  it("a changed poll on a clean doc reloads", () => {
    const s = loaded();
    const after = poll(s, H2);
    expect(after).toMatchObject({ disk: "changed", diskHash: H2, doc: "clean" });
    expect(pollEffect(s, after)).toBe("reload");
    const reloaded = reduceViewer(after, { type: "reloaded", hash: H2 });
    expect(reloaded).toMatchObject({ disk: "same", baseHash: H2, diskHash: H2, justReloaded: true });
  });
  it("a changed poll on a dirty doc shows the banner and never reloads", () => {
    const s = reduceViewer(loaded(), { type: "edited" });
    const after = poll(s, H2);
    expect(after).toMatchObject({ doc: "dirty", disk: "changed" });
    expect(pollEffect(s, after)).toBe("banner");
    expect(pollEffect(after, poll(after, H2))).toBe("none");
  });
  it("overwrite-on-save adopts the disk hash as the base", () => {
    const s = poll(reduceViewer(loaded(), { type: "edited" }), H2);
    const o = reduceViewer(s, { type: "overwrite_on_save" });
    expect(o).toMatchObject({ baseHash: H2, disk: "same", overwrite: true, doc: "dirty" });
    expect(canSave(o, true)).toBe(true);
  });
  it("a save clears dirty and aligns hashes; a conflict marks disk changed", () => {
    const s = reduceViewer(loaded(), { type: "edited" });
    expect(reduceViewer(s, { type: "saved", hash: H2 })).toMatchObject({ doc: "clean", disk: "same", baseHash: H2, diskHash: H2, overwrite: false });
    expect(reduceViewer(s, { type: "save_conflict" })).toMatchObject({ doc: "dirty", disk: "changed" });
    expect(reduceViewer(s, { type: "save_gone" })).toMatchObject({ disk: "gone" });
  });
  it("a gone file disables saving but keeps the buffer state", () => {
    const s = reduceViewer(loaded(), { type: "edited" });
    const gone = poll(s, null, false);
    expect(gone).toMatchObject({ disk: "gone", doc: "dirty" });
    expect(canSave(gone, true)).toBe(false);
    expect(pollEffect(s, gone)).toBe("banner");
  });
  it("a failed poll flags the container down and a good one clears it", () => {
    const down = reduceViewer(loaded(), { type: "poll_failed" });
    expect(down.containerDown).toBe(true);
    expect(canSave(down, true)).toBe(false);
    expect(poll(down, H1).containerDown).toBe(false);
  });
  it("canSave needs dirty + editable + disk in sync", () => {
    expect(canSave(loaded(), true)).toBe(false);
    const dirty = reduceViewer(loaded(), { type: "edited" });
    expect(canSave(dirty, true)).toBe(true);
    expect(canSave(dirty, false)).toBe(false);
    expect(canSave(poll(dirty, H2), true)).toBe(false);
  });
});
```

- [ ] **Step 2: Write the failing editability tests** (`editability.test.ts`):

```ts
import { describe, expect, it } from "vitest";
import { classifyViewerFile } from "./editability";
import type { ViewerFile } from "../lib/types";

const file = (over: Partial<ViewerFile> = {}): ViewerFile => ({
  contents_base64: "", truncated: false, size: 10, hash: "0".repeat(64), editable: true, readonly_reason: null, ...over,
});
const text = new TextEncoder().encode("hello\n");

describe("classifyViewerFile", () => {
  it("text in a write root is editable", () => {
    expect(classifyViewerFile("/workspace/a/x.md", file(), text)).toEqual({ kind: "text", editable: true, reason: null });
  });
  it("a truncated file is read-only and says why", () => {
    const r = classifyViewerFile("/workspace/a/big.log", file({ truncated: true }), text);
    expect(r.editable).toBe(false);
    expect(r.reason).toMatch(/1 MiB/);
  });
  it("Rust's refusal wins and is quoted", () => {
    const r = classifyViewerFile("/etc/hosts", file({ editable: false, readonly_reason: "Only /workspace, /home/claude and /tmp can be written." }), text);
    expect(r).toEqual({ kind: "text", editable: false, reason: "Only /workspace, /home/claude and /tmp can be written." });
  });
  it("images and binaries are never editable", () => {
    expect(classifyViewerFile("/workspace/a/x.png", file(), new Uint8Array([137, 80]))).toMatchObject({ kind: "image", editable: false });
    expect(classifyViewerFile("/workspace/a/x.bin", file(), new Uint8Array([0, 1, 2]))).toMatchObject({ kind: "binary", editable: false });
  });
});
```

- [ ] **Step 3: Write the failing language tests** (`languages.test.ts`):

```ts
import { describe, expect, it } from "vitest";
import { languageFor, wrapsLines } from "./languages";

describe("languageFor", () => {
  it.each(["a.ts", "a.tsx", "a.js", "a.jsx", "a.mjs", "a.rs", "a.py", "a.json", "a.yaml", "a.yml", "a.toml", "a.sh", "a.bash", "a.css", "a.html", "a.md", "Dockerfile", "Cargo.lock", "README"])(
    "resolves %s without throwing", async (name) => { await expect(languageFor(`/workspace/${name}`)).resolves.toBeDefined(); });
  it("returns null for an unknown extension", async () => {
    await expect(languageFor("/workspace/x.xyz")).resolves.toBeNull();
  });
  it("returns an extension for markdown", async () => {
    await expect(languageFor("/workspace/x.md")).resolves.not.toBeNull();
  });
});

describe("wrapsLines", () => {
  it("wraps prose, not code", () => {
    expect(wrapsLines("x.md")).toBe(true);
    expect(wrapsLines("x.txt")).toBe(true);
    expect(wrapsLines("x.rs")).toBe(false);
  });
});
```

- [ ] **Step 4: Run to verify failure** — `npx vitest run src/viewer` → three modules missing.

- [ ] **Step 5: Implement `viewerState.ts`**

```ts
/**
 * The viewer's reload/dirty/conflict rules as a pure reducer (spec §5).
 *
 * Two hashes, deliberately: `baseHash` is what the buffer was loaded from or
 * last saved as — the save's precondition. `diskHash` is the last full-file
 * hash the poll reported. They differ only for a truncated (read-only) load,
 * where the read's hash covers a prefix and can never equal `sha256sum`; the
 * poll then seeds `diskHash` without triggering a reload.
 */
import type { ViewerPoll } from "../lib/types";

export type DocStatus = "clean" | "dirty";
export type DiskStatus = "same" | "changed" | "gone";

export interface ViewerDocState {
  doc: DocStatus;
  disk: DiskStatus;
  baseHash: string | null;
  diskHash: string | null;
  containerDown: boolean;
  justReloaded: boolean;
  overwrite: boolean;
}

export type ViewerAction =
  | { type: "loaded"; hash: string; truncated: boolean }
  | { type: "edited" }
  | { type: "polled"; poll: ViewerPoll }
  | { type: "poll_failed" }
  | { type: "reloaded"; hash: string }
  | { type: "overwrite_on_save" }
  | { type: "saved"; hash: string }
  | { type: "save_conflict" }
  | { type: "save_gone" };

export const initialViewerState: ViewerDocState = {
  doc: "clean",
  disk: "same",
  baseHash: null,
  diskHash: null,
  containerDown: false,
  justReloaded: false,
  overwrite: false,
};

export function reduceViewer(state: ViewerDocState, action: ViewerAction): ViewerDocState {
  const s = { ...state, justReloaded: false };
  switch (action.type) {
    case "loaded":
      return { ...initialViewerState, baseHash: action.hash, diskHash: action.truncated ? null : action.hash };
    case "edited":
      return { ...s, doc: "dirty" };
    case "polled": {
      if (!action.poll.exists) return { ...s, disk: "gone", containerDown: false };
      const hash = action.poll.hash;
      if (hash === null) return { ...s, containerDown: false };
      if (s.diskHash === null) return { ...s, diskHash: hash, disk: s.disk === "gone" ? "same" : s.disk, containerDown: false };
      if (hash === s.diskHash) return { ...s, disk: s.disk === "gone" ? "same" : s.disk, containerDown: false };
      // Changed on disk. "Overwrite on save" adopted a base; a further change
      // on disk invalidates it again.
      return { ...s, diskHash: hash, disk: "changed", overwrite: false, containerDown: false };
    }
    case "poll_failed":
      return { ...s, containerDown: true };
    case "reloaded":
      return { ...s, doc: "clean", disk: "same", baseHash: action.hash, diskHash: action.hash, justReloaded: true, overwrite: false };
    case "overwrite_on_save":
      return { ...s, baseHash: s.diskHash, disk: "same", overwrite: true };
    case "saved":
      return { ...s, doc: "clean", disk: "same", baseHash: action.hash, diskHash: action.hash, overwrite: false };
    case "save_conflict":
      return { ...s, disk: "changed", overwrite: false };
    case "save_gone":
      return { ...s, disk: "gone" };
  }
}

export function pollEffect(before: ViewerDocState, after: ViewerDocState): "none" | "reload" | "banner" {
  if (after.disk === "gone") return before.disk === "gone" ? "none" : "banner";
  if (after.disk !== "changed" || after.diskHash === before.diskHash) return "none";
  return after.doc === "clean" ? "reload" : "banner";
}

export function canSave(state: ViewerDocState, editable: boolean): boolean {
  return editable && state.doc === "dirty" && state.disk === "same" && !state.containerDown;
}
```

- [ ] **Step 6: Implement `editability.ts`**

```ts
import { imageMimeFor, looksBinary, TEXT_PREVIEW_LIMIT } from "../components/projects/home/filePreview";
import type { ViewerFile } from "../lib/types";

export type ViewerKind = "text" | "image" | "binary";
export interface Editability { kind: ViewerKind; editable: boolean; reason: string | null }

const MIB = TEXT_PREVIEW_LIMIT / (1024 * 1024);

export function classifyViewerFile(path: string, file: ViewerFile, bytes: Uint8Array): Editability {
  if (imageMimeFor(path)) return { kind: "image", editable: false, reason: "Images are shown, not edited." };
  if (looksBinary(bytes)) return { kind: "binary", editable: false, reason: "This file is not text." };
  if (file.truncated) return { kind: "text", editable: false, reason: `Files over ${MIB} MiB are read-only.` };
  if (!file.editable) return { kind: "text", editable: false, reason: file.readonly_reason ?? "This location is read-only." };
  return { kind: "text", editable: true, reason: null };
}
```

- [ ] **Step 7: Implement `languages.ts`**

```ts
/**
 * Extension → CodeMirror language, loaded on demand so a window only pays for
 * the grammar it shows. Dynamic `import()` becomes a same-origin chunk, fine
 * under `script-src 'self'`.
 */
import type { Extension } from "@codemirror/state";
import { extensionOf } from "../components/projects/home/filePreview";

type Loader = () => Promise<Extension>;

const BY_EXTENSION: Record<string, Loader> = {
  md: () => import("@codemirror/lang-markdown").then((m) => m.markdown()),
  markdown: () => import("@codemirror/lang-markdown").then((m) => m.markdown()),
  js: () => import("@codemirror/lang-javascript").then((m) => m.javascript()),
  mjs: () => import("@codemirror/lang-javascript").then((m) => m.javascript()),
  cjs: () => import("@codemirror/lang-javascript").then((m) => m.javascript()),
  jsx: () => import("@codemirror/lang-javascript").then((m) => m.javascript({ jsx: true })),
  ts: () => import("@codemirror/lang-javascript").then((m) => m.javascript({ typescript: true })),
  tsx: () => import("@codemirror/lang-javascript").then((m) => m.javascript({ jsx: true, typescript: true })),
  rs: () => import("@codemirror/lang-rust").then((m) => m.rust()),
  py: () => import("@codemirror/lang-python").then((m) => m.python()),
  json: () => import("@codemirror/lang-json").then((m) => m.json()),
  jsonc: () => import("@codemirror/lang-json").then((m) => m.json()),
  yaml: () => import("@codemirror/lang-yaml").then((m) => m.yaml()),
  yml: () => import("@codemirror/lang-yaml").then((m) => m.yaml()),
  css: () => import("@codemirror/lang-css").then((m) => m.css()),
  html: () => import("@codemirror/lang-html").then((m) => m.html()),
  htm: () => import("@codemirror/lang-html").then((m) => m.html()),
  toml: () => stream("toml"),
  lock: () => stream("toml"),
  sh: () => stream("shell"),
  bash: () => stream("shell"),
  zsh: () => stream("shell"),
};

const BY_BASENAME: Record<string, Loader> = {
  dockerfile: () => stream("shell"),
  makefile: () => stream("shell"),
};

async function stream(mode: "toml" | "shell"): Promise<Extension> {
  const { StreamLanguage } = await import("@codemirror/language");
  const parser = mode === "toml"
    ? (await import("@codemirror/legacy-modes/mode/toml")).toml
    : (await import("@codemirror/legacy-modes/mode/shell")).shell;
  return StreamLanguage.define(parser);
}

export function languageFor(path: string): Promise<Extension | null> {
  const ext = extensionOf(path);
  const base = path.slice(path.lastIndexOf("/") + 1).toLowerCase();
  const loader = BY_EXTENSION[ext] ?? BY_BASENAME[base];
  return loader ? loader() : Promise.resolve(null);
}

const PROSE = new Set(["md", "markdown", "txt", "rst", "log", ""]);
export function wrapsLines(path: string): boolean {
  return PROSE.has(extensionOf(path));
}
```

- [ ] **Step 8: Run tests** — `npx vitest run src/viewer` → all pass. `npx tsc --noEmit` → 0.

- [ ] **Step 9: Commit** — `git add app/src/viewer && git commit -m "feat(viewer): reload/conflict reducer, editability rules, language loading"`

---

## Group B (each lists its dependencies)

### Task 8: Rust — commands, window creation, registration (depends on Tasks 1, 2, 3, 6)

**Files:**
- Replace: `app/src-tauri/src/file_viewer/window.rs` (Task 0's placeholder)
- Create: `app/src-tauri/src/commands/file_viewer_commands.rs`
- Modify: `app/src-tauri/src/commands/mod.rs` (`pub mod file_viewer_commands;`), `app/src-tauri/src/lib.rs` (`.manage(file_viewer::registry::ViewerRegistry::default())` right after the `.manage(AppState {…})` call; six `generate_handler!` lines after the `// Files` block under a `// Terminal file viewer` comment).

**Interfaces:** Consumes Tasks 1–3. Produces the six commands with the exact signatures in "Interfaces", plus:

```rust
// window.rs
pub fn open_viewer_window(app: &AppHandle, label: &str, title: &str) -> Result<(), String>;
// file_viewer_commands.rs (private helpers, tested)
fn require_main(window_label: &str) -> Result<(), String>;
fn require_viewer(window_label: &str) -> Result<String, String>;
fn viewer_state_of(label: &str, target: ViewerTarget) -> ViewerState;   // the serialisable shape
fn window_title(raw_path: &str, project_name: &str) -> String;
```

- [ ] **Step 1: Write the failing tests** (bottom of `file_viewer_commands.rs`):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_is_main_only_and_viewer_commands_are_viewer_only() {
        assert!(require_main("main").is_ok());
        assert!(require_main("file-viewer-1").is_err());
        assert!(require_main("browser-view-x").is_err());
        assert_eq!(require_viewer("file-viewer-7").unwrap(), "file-viewer-7");
        assert!(require_viewer("main").is_err());
        assert!(require_viewer("file-viewer-").is_err());
    }

    #[test]
    fn the_title_is_basename_then_project() {
        assert_eq!(window_title("app/src/lib/urlRelay.ts", "Triple-C"), "urlRelay.ts — Triple-C");
        assert_eq!(window_title("/workspace/x/README.md", "x"), "README.md — x");
        assert_eq!(window_title("Makefile", "p"), "Makefile — p");
    }

    #[test]
    fn viewer_state_serialises_the_ipc_shape() {
        let target = ViewerTarget {
            project_id: "pid".into(),
            project_name: "P".into(),
            raw_path: "src/a.rs".into(),
            state: ViewerTargetState::Resolved { container_path: "/workspace/p/src/a.rs".into() },
            initial: Location { line: Some(3), col: Some(2), end_line: None },
        };
        let json = serde_json::to_value(viewer_state_of("file-viewer-1", target)).unwrap();
        assert_eq!(json["project_id"], "pid");
        assert_eq!(json["state"]["kind"], "resolved");
        assert_eq!(json["state"]["container_path"], "/workspace/p/src/a.rs");
        assert_eq!(json["initial"]["line"], 3);
        assert!(json["initial"]["end_line"].is_null());
    }
}
```

- [ ] **Step 2: Run to verify failure** — `cargo test --offline file_viewer_commands` → compile error.

- [ ] **Step 3: Implement `file_viewer/window.rs`**

```rust
//! The viewer window itself. Mirrors `browser_view/popout.rs`, with two differences:
//! the URL is the app's own second entry (`WebviewUrl::App`), so the capability in
//! `capabilities/file-viewer.json` applies; and the registry entry is removed on
//! `Destroyed`, which fires for both the X button (after JS calls `destroy()`) and a
//! Rust-side `destroy()`.

use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindowBuilder, WindowEvent};

use super::registry::ViewerRegistry;

pub fn open_viewer_window(app: &AppHandle, label: &str, title: &str) -> Result<(), String> {
    let window = WebviewWindowBuilder::new(app, label, WebviewUrl::App("viewer.html".into()))
        .title(title)
        .inner_size(900.0, 700.0)
        .min_inner_size(480.0, 320.0)
        .build()
        .map_err(|e| format!("Could not open the file window: {}", e))?;

    let app_for_event = app.clone();
    let label_owned = label.to_string();
    window.on_window_event(move |event| {
        if let WindowEvent::Destroyed = event {
            app_for_event.state::<ViewerRegistry>().remove(&label_owned);
        }
    });
    Ok(())
}
```

- [ ] **Step 4: Implement `commands/file_viewer_commands.rs`**

```rust
//! IPC for the terminal file viewer. Every command here is gated on the calling
//! window's label and reads its target from the registry — no path, no label, no
//! project id crosses IPC from a viewer window. See spec §6.

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State};

use crate::commands::file_commands::{
    fetch_container_file, require_running, validate_container_write_path,
};
use crate::file_viewer::poll::{poll_file, ViewerPoll};
use crate::file_viewer::registry::{Location, ViewerRegistry, ViewerTarget, ViewerTargetState};
use crate::file_viewer::resolve::{candidate_paths, probe_candidates};
use crate::file_viewer::window::open_viewer_window;
use crate::file_viewer::write::{sha256_hex, write_file, MAX_WRITE_BYTES};
use crate::file_viewer::is_viewer_label;
use crate::AppState;

pub const GOTO_EVENT: &str = "file-viewer-goto";

#[derive(Clone, Debug, Serialize)]
pub struct ViewerState {
    pub project_id: String,
    pub project_name: String,
    pub raw_path: String,
    pub state: ViewerTargetState,
    pub initial: Location,
}

#[derive(Clone, Debug, Serialize)]
pub struct ViewerFile {
    pub contents_base64: String,
    pub truncated: bool,
    pub size: u64,
    pub hash: String,
    pub editable: bool,
    pub readonly_reason: Option<String>,
}

fn require_main(window_label: &str) -> Result<(), String> {
    if window_label == "main" {
        Ok(())
    } else {
        Err("Only the main window can open files.".into())
    }
}

fn require_viewer(window_label: &str) -> Result<String, String> {
    if is_viewer_label(window_label) {
        Ok(window_label.to_string())
    } else {
        Err("This command belongs to a file window.".into())
    }
}

fn viewer_state_of(_label: &str, target: ViewerTarget) -> ViewerState {
    ViewerState {
        project_id: target.project_id,
        project_name: target.project_name,
        raw_path: target.raw_path,
        state: target.state,
        initial: target.initial,
    }
}

fn window_title(raw_path: &str, project_name: &str) -> String {
    let base = raw_path.trim_end_matches('/').rsplit('/').next().unwrap_or(raw_path);
    format!("{} — {}", base, project_name)
}

/// The caller's registry entry, or a sentence.
fn own_target(window: &tauri::Window, registry: &ViewerRegistry) -> Result<(String, ViewerTarget), String> {
    let label = require_viewer(window.label())?;
    let target = registry
        .get(&label)
        .ok_or_else(|| "This file window is no longer registered.".to_string())?;
    Ok((label, target))
}

fn resolved_path(target: &ViewerTarget) -> Result<String, String> {
    match &target.state {
        ViewerTargetState::Resolved { container_path } => Ok(container_path.clone()),
        _ => Err("Choose a file first.".into()),
    }
}

async fn running_container_of(state: &State<'_, AppState>, project_id: &str) -> Result<String, String> {
    let project = state
        .projects_store
        .get(project_id)
        .ok_or_else(|| "This project no longer exists.".to_string())?;
    let container_id = project
        .container_id
        .ok_or_else(|| "Start the project before opening files — they live in its container.".to_string())?;
    require_running(&container_id, "opening files").await?;
    Ok(container_id)
}

#[tauri::command]
pub async fn open_file_viewer(
    project_id: String,
    path: String,
    line: Option<u32>,
    col: Option<u32>,
    end_line: Option<u32>,
    window: tauri::Window,
    app: AppHandle,
    registry: State<'_, ViewerRegistry>,
    state: State<'_, AppState>,
) -> Result<(), String> {
    require_main(window.label())?;
    let project = state
        .projects_store
        .get(&project_id)
        .ok_or_else(|| "This project no longer exists.".to_string())?;
    let container_id = running_container_of(&state, &project_id).await?;

    let mounts: Vec<String> = project.paths.iter().map(|p| p.mount_name.clone()).collect();
    let candidates = candidate_paths(&path, &mounts)?;
    let matches = probe_candidates(&container_id, &candidates).await?;
    let initial = Location { line, col, end_line };

    let target_state = match matches.len() {
        0 => ViewerTargetState::NotFound { tried: candidates },
        1 => {
            let container_path = matches[0].clone();
            if let Some(label) = registry.find_open(&project_id, &container_path) {
                if let Some(existing) = app.get_webview_window(&label) {
                    let _ = existing.unminimize();
                    let _ = existing.set_focus();
                    let _ = app.emit_to(label.as_str(), GOTO_EVENT, initial);
                    return Ok(());
                }
                registry.remove(&label);
            }
            ViewerTargetState::Resolved { container_path }
        }
        _ => ViewerTargetState::Choose { candidates: matches },
    };

    let title = window_title(&path, &project.name);
    let label = registry.reserve(ViewerTarget {
        project_id,
        project_name: project.name.clone(),
        raw_path: path,
        state: target_state,
        initial,
    })?;
    if let Err(e) = open_viewer_window(&app, &label, &title) {
        registry.remove(&label);
        return Err(e);
    }
    Ok(())
}

#[tauri::command]
pub async fn viewer_get_state(
    window: tauri::Window,
    registry: State<'_, ViewerRegistry>,
) -> Result<ViewerState, String> {
    let (label, target) = own_target(&window, &registry)?;
    Ok(viewer_state_of(&label, target))
}

#[tauri::command]
pub async fn viewer_choose_file(
    index: usize,
    window: tauri::Window,
    registry: State<'_, ViewerRegistry>,
) -> Result<ViewerState, String> {
    let (label, target) = own_target(&window, &registry)?;
    let chosen = match &target.state {
        ViewerTargetState::Choose { candidates } => candidates
            .get(index)
            .cloned()
            .ok_or_else(|| "That choice is no longer available.".to_string())?,
        _ => return Err("This window is not choosing a file.".into()),
    };
    let updated = registry.set_state(&label, ViewerTargetState::Resolved { container_path: chosen })?;
    Ok(viewer_state_of(&label, updated))
}

#[tauri::command]
pub async fn viewer_read_file(
    max_bytes: u64,
    window: tauri::Window,
    registry: State<'_, ViewerRegistry>,
    state: State<'_, AppState>,
) -> Result<ViewerFile, String> {
    let (_label, target) = own_target(&window, &registry)?;
    let path = resolved_path(&target)?;
    let container_id = running_container_of(&state, &target.project_id).await?;
    let cap = max_bytes.clamp(1, crate::commands::file_commands::MAX_READ_BYTES);
    let fetched = fetch_container_file(&container_id, &path, cap).await?;
    let (editable, readonly_reason) = match validate_container_write_path("File", &path) {
        Ok(()) => (true, None),
        Err(reason) => (false, Some(reason)),
    };
    Ok(ViewerFile {
        hash: sha256_hex(&fetched.bytes),
        contents_base64: BASE64.encode(&fetched.bytes),
        truncated: fetched.truncated,
        size: fetched.size,
        editable,
        readonly_reason,
    })
}

#[tauri::command]
pub async fn viewer_poll_file(
    window: tauri::Window,
    registry: State<'_, ViewerRegistry>,
    state: State<'_, AppState>,
) -> Result<ViewerPoll, String> {
    let (_label, target) = own_target(&window, &registry)?;
    let path = resolved_path(&target)?;
    let container_id = running_container_of(&state, &target.project_id).await?;
    poll_file(&container_id, &path).await
}

#[tauri::command]
pub async fn viewer_write_file(
    contents_base64: String,
    base_hash: String,
    window: tauri::Window,
    registry: State<'_, ViewerRegistry>,
    state: State<'_, AppState>,
) -> Result<String, String> {
    let (_label, target) = own_target(&window, &registry)?;
    let path = resolved_path(&target)?;
    validate_container_write_path("File", &path)?;
    if contents_base64.len() > MAX_WRITE_BYTES * 4 / 3 + 4 {
        return Err("Files over 1 MiB are read-only in the viewer.".into());
    }
    let bytes = BASE64
        .decode(contents_base64.as_bytes())
        .map_err(|_| "The editor sent malformed content.".to_string())?;
    let container_id = running_container_of(&state, &target.project_id).await?;
    write_file(&container_id, &state.exec_manager, &path, &bytes, &base_hash).await
}
```

- [ ] **Step 5: Register** — in `lib.rs`: `pub mod file_viewer;` is already there from Task 0; add `.manage(file_viewer::registry::ViewerRegistry::default())` after the existing `.manage(AppState { … })` block; add to `generate_handler!`:

```rust
            // Terminal file viewer
            commands::file_viewer_commands::open_file_viewer,
            commands::file_viewer_commands::viewer_get_state,
            commands::file_viewer_commands::viewer_choose_file,
            commands::file_viewer_commands::viewer_read_file,
            commands::file_viewer_commands::viewer_poll_file,
            commands::file_viewer_commands::viewer_write_file,
```

- [ ] **Step 6: Verify** — `cargo test --offline` (whole suite: `every_command_is_registered_exactly_once` plus the new tests) → pass; `cargo clippy --offline` → clean. Tauri's IPC arg names: JS sends `maxBytes`, `contentsBase64`, `baseHash`, `endLine`, `projectId` — Tauri maps camelCase to the snake_case parameters automatically.

- [ ] **Step 7: Commit** — `git add app/src-tauri && git commit -m "feat(viewer): label-gated viewer commands and window creation"`

---

### Task 9: Terminal integration — link provider and OSC 8 dispatch (depends on Tasks 0, 4, 5)

**Files:**
- Create: `app/src/components/terminal/filePathLinkProvider.ts` + `.test.ts`
- Modify: `app/src/components/terminal/TerminalView.tsx` (`createOsc8LinkHandler` ~line 401; the `WebLinksAddon` comment + registration ~lines 918–953; cleanup ~line 1238)
- Modify: `app/src/components/terminal/TerminalView.test.tsx` (mock `openFileViewer`; new tests in the `createOsc8LinkHandler` describe)

**Interfaces:**
- Consumes `findFilePathLinks`, `joinWrappedRows`, `offsetToCell`, `openFileViewer`.
- Produces `createFilePathLinkProvider(term: Pick<Terminal,"buffer">, onOpen: (m: FilePathMatch) => void, gate: (event: MouseEvent) => boolean, hover?: {show(path: string): void; hide(): void}): ILinkProvider` and a third parameter on `createOsc8LinkHandler(getHost, readState, onOpenFile?: (path: string) => void)`.

- [ ] **Step 1: Write the failing provider test** (`filePathLinkProvider.test.ts`):

```ts
import { describe, expect, it, vi } from "vitest";
import { createFilePathLinkProvider } from "./filePathLinkProvider";

const fakeTerm = (rows: Array<[string, boolean]>) => ({
  buffer: {
    active: {
      getLine: (y: number) => rows[y] && { isWrapped: rows[y][1], translateToString: () => rows[y][0] },
    },
  },
}) as unknown as Parameters<typeof createFilePathLinkProvider>[0];

describe("createFilePathLinkProvider", () => {
  it("reports 1-based inclusive ranges and activates through the gate", () => {
    const onOpen = vi.fn();
    const gate = vi.fn(() => true);
    const provider = createFilePathLinkProvider(fakeTerm([["Edited src/foo.ts:42 today", false]]), onOpen, gate);
    const links = vi.fn();
    provider.provideLinks(1, links);
    const [list] = links.mock.calls[0];
    expect(list).toHaveLength(1);
    expect(list[0].range).toEqual({ start: { x: 8, y: 1 }, end: { x: 20, y: 1 } });
    expect(list[0].text).toBe("src/foo.ts:42");
    list[0].activate(new MouseEvent("click"), list[0].text);
    expect(onOpen).toHaveBeenCalledWith(expect.objectContaining({ path: "src/foo.ts", line: 42 }));
  });

  it("does nothing when the gate refuses", () => {
    const onOpen = vi.fn();
    const provider = createFilePathLinkProvider(fakeTerm([["src/foo.ts", false]]), onOpen, () => false);
    const links = vi.fn();
    provider.provideLinks(1, links);
    links.mock.calls[0][0][0].activate(new MouseEvent("click"), "src/foo.ts");
    expect(onOpen).not.toHaveBeenCalled();
  });

  it("spans a wrapped path across rows", () => {
    const provider = createFilePathLinkProvider(fakeTerm([["see /workspace/p/", false], ["src/foo.ts:7", true]]), vi.fn(), () => true);
    const links = vi.fn();
    provider.provideLinks(2, links);
    expect(links.mock.calls[0][0][0].range).toEqual({ start: { x: 5, y: 1 }, end: { x: 12, y: 2 } });
  });

  it("answers undefined for a row with nothing", () => {
    const provider = createFilePathLinkProvider(fakeTerm([["plain words", false]]), vi.fn(), () => true);
    const links = vi.fn();
    provider.provideLinks(1, links);
    expect(links).toHaveBeenCalledWith(undefined);
  });
});
```

- [ ] **Step 2: Run to verify failure** — `npx vitest run src/components/terminal/filePathLinkProvider.test.ts` → module missing.

- [ ] **Step 3: Implement `filePathLinkProvider.ts`**

```ts
/**
 * xterm `ILinkProvider` for file paths in the buffer.
 *
 * Registered after `WebLinksAddon` so URLs are claimed first; `findFilePathLinks`
 * also refuses anything inside a `scheme://` span, so the two never overlap.
 * Ranges are 1-based on both axes with an *inclusive* end column (xterm's
 * contract), and `provideLinks`' row is 1-based while `getLine` is 0-based.
 */
import type { ILink, ILinkProvider, Terminal } from "@xterm/xterm";
import { findFilePathLinks, type FilePathMatch } from "../../lib/filePathLinks";
import { joinWrappedRows, offsetToCell } from "../../lib/xtermLineJoin";

export interface FilePathHover {
  show(path: string): void;
  hide(): void;
}

export function createFilePathLinkProvider(
  term: Pick<Terminal, "buffer">,
  onOpen: (match: FilePathMatch) => void,
  gate: (event: MouseEvent) => boolean,
  hover?: FilePathHover,
): ILinkProvider {
  return {
    provideLinks(bufferLineNumber, callback) {
      const row = bufferLineNumber - 1;
      const line = term.buffer.active.getLine(row);
      if (!line) return callback(undefined);
      const joined = joinWrappedRows(term.buffer.active, row);
      const matches = findFilePathLinks(joined.text);
      if (matches.length === 0) return callback(undefined);
      const links: ILink[] = matches
        .map((m): ILink => ({
          range: { start: offsetToCell(joined, m.start), end: offsetToCell(joined, m.end - 1) },
          text: joined.text.slice(m.start, m.end),
          decorations: { pointerCursor: true, underline: true },
          activate: (event) => {
            if (!gate(event)) return;
            onOpen(m);
          },
          hover: () => hover?.show(m.path),
          leave: () => hover?.hide(),
        }))
        // Only links that touch the row being asked about (xterm asks per row).
        .filter((l) => l.range.start.y <= bufferLineNumber && l.range.end.y >= bufferLineNumber);
      callback(links.length ? links : undefined);
    },
  };
}
```

- [ ] **Step 4: Wire it into `TerminalView.tsx`**

  1. Imports: `import { openFileViewer } from "../../lib/tauri-commands";` (extend the existing import), `import { createFilePathLinkProvider } from "./filePathLinkProvider";`, `import type { FilePathMatch } from "../../lib/filePathLinks";`.
  2. Add a module-level helper next to `reportOpenFailure`:
     ```ts
     function reportViewerFailure(e: unknown): void {
       useAppState.getState().pushToast({
         kind: "error",
         message: "Could not open the file",
         detail: e instanceof Error ? e.message : String(e),
         dedupeKey: "file-viewer-open",
       });
     }
     ```
  3. `createOsc8LinkHandler` gains a third parameter `onOpenFile?: (path: string) => void` and sets `allowNonHttpProtocols: true`. In `activate`, replace the body after the gate with:
     ```ts
     let parsed: URL;
     try { parsed = new URL(text); } catch { console.warn("Refusing a hyperlink that is not a URL"); return; }
     if (parsed.protocol === "file:") {
       if (!onOpenFile) return;
       onOpenFile(decodeURIComponent(parsed.pathname));
       return;
     }
     if (parsed.protocol !== "http:" && parsed.protocol !== "https:") {
       console.warn("Refusing to open a link with an unsupported scheme");
       return;
     }
     const safe = sanitizeRelayUrl(text);
     if (!safe) { console.warn("Refusing to open a link that failed validation"); return; }
     openUrlExternal(safe).catch(reportOpenFailure);
     ```
     In `hover`, before building the card: parse the same way; return (no card) unless `file:` or `http(s):`. For `file:` the card's bold span is `"Open in viewer"` and the rest is the decoded path; the hint line is unchanged.
  4. At the `new Terminal({ linkHandler: … })` site, pass the third argument:
     ```ts
     (path) => { if (projectIdRef.current) openFileViewer(projectIdRef.current, path).catch(reportViewerFailure); },
     ```
     where `projectIdRef` is a new `useRef<string | undefined>(projectId)` kept current with `useEffect(() => { projectIdRef.current = projectId; }, [projectId]);` (the mount effect is keyed on `sessionId` only, so it cannot close over `projectId`).
  5. After `term.loadAddon(webLinksAddon);`:
     ```ts
     const filePathLinks = term.registerLinkProvider(
       createFilePathLinkProvider(
         term,
         (m: FilePathMatch) => {
           const pid = projectIdRef.current;
           if (!pid) return;
           openFileViewer(pid, m.path, m.line, m.col, m.endLine).catch(reportViewerFailure);
         },
         (event) => opensOnClick(event, readClickContext(term)),
         {
           show: (path) => osc8LinkHandlerRef.current?.hover?.(new MouseEvent("mousemove"), `file://${path}`, { start: { x: 1, y: 1 }, end: { x: 1, y: 1 } }),
           hide: () => osc8LinkHandlerRef.current?.dismiss(),
         },
       ),
     );
     ```
     and in the cleanup, before `term.dispose()`: `filePathLinks.dispose();`.
  6. Update the `WebLinksAddon` comment paragraph that begins "It is also the only gate on a real bypass of the OSC 8 one" to say: `allowNonHttpProtocols` is now **on**, so `OscLinkProvider` hands every target to `createOsc8LinkHandler`, which parses and refuses anything but `file:` and `http(s):` itself; this branch remains the only handler for plain-text URLs.

- [ ] **Step 5: Add tests to `TerminalView.test.tsx`** — add `openFileViewer: vi.fn(async () => {}),` to the `vi.mock("../../lib/tauri-commands", …)` factory and import it; in the `createOsc8LinkHandler` describe add:

```ts
  it("routes a file: target to the viewer and never to the opener", () => {
    const onOpenFile = vi.fn();
    const h = createOsc8LinkHandler(() => host, () => state, onOpenFile);
    h.activate(click(), "file:///workspace/p/src/a.ts", range);
    expect(onOpenFile).toHaveBeenCalledWith("/workspace/p/src/a.ts");
    expect(openUrlExternal).not.toHaveBeenCalled();
  });

  it("with non-http targets now delivered, still refuses javascript: and garbage", () => {
    const onOpenFile = vi.fn();
    const h = createOsc8LinkHandler(() => host, () => state, onOpenFile);
    h.activate(click(), "javascript:alert(1)", range);
    h.activate(click(), "not a url", range);
    h.hover?.(new MouseEvent("mousemove"), "javascript:alert(1)", range);
    expect(onOpenFile).not.toHaveBeenCalled();
    expect(openUrlExternal).not.toHaveBeenCalled();
    expect(hoverCard()).toBeNull();
  });

  it("the file: hover card names the viewer and the path", () => {
    const h = createOsc8LinkHandler(() => host, () => state, vi.fn());
    h.hover?.(new MouseEvent("mousemove"), "file:///workspace/p/README.md", range);
    expect(hoverCard()?.textContent).toContain("Open in viewer");
    expect(hoverCard()?.textContent).toContain("/workspace/p/README.md");
  });

  it("declares allowNonHttpProtocols so file: targets reach it", () => {
    expect(createOsc8LinkHandler(() => host, () => state).allowNonHttpProtocols).toBe(true);
  });
```

The existing test `refuses a target that fails validation, without reaching the opener` still passes: `file:///etc/passwd` with no `onOpenFile` goes nowhere.

- [ ] **Step 6: Verify** — `npx vitest run src/components/terminal` → all pass (82 existing + new); `npx tsc --noEmit` → 0.

- [ ] **Step 7: Commit** — `git add app/src/components/terminal && git commit -m "feat(viewer): clickable file paths and file: hyperlinks in the terminal"`

---

### Task 10: Viewer editor — CodeMirror wrapper, theme, line highlight (depends on Task 6)

**Files:**
- Create: `app/src/viewer/viewerTheme.ts`, `app/src/viewer/highlightLine.ts` + `.test.ts`, `app/src/viewer/CodeEditor.tsx`

**Interfaces:** Produces

```ts
// highlightLine.ts
export const setHighlight: StateEffectType<{ from: number; to: number } | null>;  // 1-based line numbers, inclusive
export const highlightLineField: StateField<DecorationSet>;
export function highlightExtension(): Extension;
export function lineRangeToPositions(doc: Text, from: number, to: number): { from: number; to: number } | null; // clamped

// CodeEditor.tsx
export interface CodeEditorHandle {
  getDoc(): string;
  /** Replace the whole document, keeping scroll and a clamped cursor. Does not mark dirty. */
  setDoc(text: string): void;
  goTo(loc: ViewerLocation): void;
  focus(): void;
}
export interface CodeEditorProps {
  initialDoc: string;
  readOnly: boolean;
  language: Extension | null;
  lineWrapping: boolean;
  initialLocation: ViewerLocation;
  onDocChanged(): void;
  onSave(): void;
}
export const CodeEditor: React.ForwardRefExoticComponent<CodeEditorProps & React.RefAttributes<CodeEditorHandle>>;
```

- [ ] **Step 1: Write the failing highlight tests** (`highlightLine.test.ts`):

```ts
import { describe, expect, it } from "vitest";
import { EditorState, Text } from "@codemirror/state";
import { highlightExtension, highlightLineField, lineRangeToPositions, setHighlight } from "./highlightLine";

describe("lineRangeToPositions", () => {
  const doc = Text.of(["one", "two", "three"]);
  it("maps 1-based inclusive lines to document offsets", () => {
    expect(lineRangeToPositions(doc, 2, 2)).toEqual({ from: 4, to: 4 });
    expect(lineRangeToPositions(doc, 1, 3)).toEqual({ from: 0, to: 8 });
  });
  it("clamps past the end and refuses nonsense", () => {
    expect(lineRangeToPositions(doc, 2, 99)).toEqual({ from: 4, to: 8 });
    expect(lineRangeToPositions(doc, 99, 100)).toEqual({ from: 8, to: 8 });
    expect(lineRangeToPositions(doc, 0, 1)).toEqual({ from: 0, to: 0 });
    expect(lineRangeToPositions(doc, 3, 1)).toEqual({ from: 8, to: 8 });
  });
});

describe("highlightLineField", () => {
  it("decorates every line in the range and clears on null", () => {
    let state = EditorState.create({ doc: "a\nb\nc\nd", extensions: [highlightExtension()] });
    state = state.update({ effects: setHighlight.of({ from: 2, to: 3 }) }).state;
    let count = 0;
    state.field(highlightLineField).between(0, state.doc.length, () => { count++; });
    expect(count).toBe(2);
    state = state.update({ effects: setHighlight.of(null) }).state;
    count = 0;
    state.field(highlightLineField).between(0, state.doc.length, () => { count++; });
    expect(count).toBe(0);
  });
});
```

- [ ] **Step 2: Run to verify failure** — `npx vitest run src/viewer/highlightLine.test.ts` → module missing.

- [ ] **Step 3: Implement `highlightLine.ts`**

```ts
import { StateEffect, StateField, type Extension, type Text } from "@codemirror/state";
import { Decoration, EditorView, type DecorationSet } from "@codemirror/view";

export const setHighlight = StateEffect.define<{ from: number; to: number } | null>();

const lineMark = Decoration.line({ class: "cm-triple-c-target" });

export function lineRangeToPositions(doc: Text, from: number, to: number): { from: number; to: number } | null {
  const clamp = (n: number) => Math.min(Math.max(1, Math.floor(n)), doc.lines);
  const a = clamp(from);
  const b = Math.max(a, clamp(to));
  return { from: doc.line(a).from, to: doc.line(b).from };
}

export const highlightLineField = StateField.define<DecorationSet>({
  create: () => Decoration.none,
  update(value, tr) {
    let next = value.map(tr.changes);
    for (const e of tr.effects) {
      if (!e.is(setHighlight)) continue;
      if (e.value === null) { next = Decoration.none; continue; }
      const range = lineRangeToPositions(tr.state.doc, e.value.from, e.value.to);
      if (!range) { next = Decoration.none; continue; }
      const marks = [];
      for (let pos = range.from; pos <= range.to; ) {
        const line = tr.state.doc.lineAt(pos);
        marks.push(lineMark.range(line.from));
        if (line.to + 1 > tr.state.doc.length) break;
        pos = line.to + 1;
      }
      next = Decoration.set(marks, true);
    }
    return next;
  },
  provide: (f) => EditorView.decorations.from(f),
});

export function highlightExtension(): Extension {
  return [highlightLineField];
}
```

- [ ] **Step 4: Implement `viewerTheme.ts`** (tokens only; adjust the highlight tags to taste but keep the set small):

```ts
import { EditorView } from "@codemirror/view";
import { HighlightStyle, syntaxHighlighting } from "@codemirror/language";
import { tags as t } from "@lezer/highlight";
import type { Extension } from "@codemirror/state";

export const viewerTheme: Extension = [
  EditorView.theme(
    {
      "&": { backgroundColor: "var(--bg-primary)", color: "var(--text-primary)", height: "100%", fontSize: "13px" },
      ".cm-content": { fontFamily: "'JetBrains Mono', 'Fira Code', 'Cascadia Code', Menlo, Monaco, monospace", caretColor: "var(--accent)" },
      ".cm-scroller": { overflow: "auto" },
      ".cm-gutters": { backgroundColor: "var(--bg-secondary)", color: "var(--text-secondary)", borderRight: "1px solid var(--border-color)" },
      ".cm-activeLine": { backgroundColor: "var(--accent-muted)" },
      ".cm-activeLineGutter": { backgroundColor: "var(--accent-muted)" },
      ".cm-triple-c-target": { backgroundColor: "var(--warning-muted)", outline: "1px solid var(--warning)" },
      "&.cm-focused .cm-selectionBackground, .cm-selectionBackground": { backgroundColor: "var(--accent-muted)" },
      ".cm-panels": { backgroundColor: "var(--bg-secondary)", color: "var(--text-primary)", borderBottom: "1px solid var(--border-color)" },
      ".cm-searchMatch": { backgroundColor: "var(--warning-muted)", outline: "1px solid var(--warning)" },
      ".cm-searchMatch.cm-searchMatch-selected": { backgroundColor: "var(--success-muted)" },
    },
    { dark: true },
  ),
  syntaxHighlighting(
    HighlightStyle.define([
      { tag: [t.keyword, t.modifier, t.operatorKeyword], color: "#ff7b72" },
      { tag: [t.string, t.special(t.string)], color: "#a5d6ff" },
      { tag: [t.comment, t.lineComment, t.blockComment], color: "var(--text-secondary)", fontStyle: "italic" },
      { tag: [t.number, t.bool, t.null, t.atom], color: "#79c0ff" },
      { tag: [t.function(t.variableName), t.function(t.propertyName)], color: "#d2a8ff" },
      { tag: [t.typeName, t.className, t.namespace], color: "#ffa657" },
      { tag: [t.propertyName, t.attributeName], color: "#7ee787" },
      { tag: t.heading, fontWeight: "bold", color: "var(--accent)" },
      { tag: t.emphasis, fontStyle: "italic" },
      { tag: t.strong, fontWeight: "bold" },
      { tag: t.link, color: "var(--accent)", textDecoration: "underline" },
      { tag: t.invalid, color: "#ff7b72", textDecoration: "underline wavy" },
    ]),
  ),
];
```

(The literal hex values are the GitHub-dark syntax palette the terminal theme in `TerminalView.tsx` already uses; they are syntax colours, not UI chrome, so they do not go through `index.css` tokens.)

- [ ] **Step 5: Implement `CodeEditor.tsx`**

```tsx
import { forwardRef, useEffect, useImperativeHandle, useRef } from "react";
import { Annotation, EditorState, Compartment, EditorSelection, type Extension } from "@codemirror/state";
import { EditorView, keymap, lineNumbers, highlightActiveLine, highlightActiveLineGutter, drawSelection, highlightSpecialChars } from "@codemirror/view";
import { defaultKeymap, history, historyKeymap, indentWithTab } from "@codemirror/commands";
import { search, searchKeymap, gotoLine } from "@codemirror/search";
import { bracketMatching, indentOnInput } from "@codemirror/language";
import type { ViewerLocation } from "../lib/types";
import { viewerTheme } from "./viewerTheme";
import { highlightExtension, setHighlight } from "./highlightLine";

export interface CodeEditorHandle {
  getDoc(): string;
  setDoc(text: string): void;
  goTo(loc: ViewerLocation): void;
  focus(): void;
}

export interface CodeEditorProps {
  initialDoc: string;
  readOnly: boolean;
  language: Extension | null;
  lineWrapping: boolean;
  initialLocation: ViewerLocation;
  onDocChanged(): void;
  onSave(): void;
}

/** A `dispatch` from `setDoc` is a reload, not a user edit; the listener must not mark it dirty. */
const reloadTag = Annotation.define<boolean>();

export const CodeEditor = forwardRef<CodeEditorHandle, CodeEditorProps>(function CodeEditor(props, ref) {
  const host = useRef<HTMLDivElement>(null);
  const view = useRef<EditorView | null>(null);
  const readOnlyCompartment = useRef(new Compartment());
  const languageCompartment = useRef(new Compartment());
  const wrapCompartment = useRef(new Compartment());
  const callbacks = useRef(props);
  callbacks.current = props;

  useEffect(() => {
    if (!host.current) return;
    const v = new EditorView({
      parent: host.current,
      state: EditorState.create({
        doc: props.initialDoc,
        extensions: [
          lineNumbers(),
          highlightActiveLine(),
          highlightActiveLineGutter(),
          highlightSpecialChars(),
          drawSelection(),
          history(),
          bracketMatching(),
          indentOnInput(),
          search({ top: true }),
          highlightExtension(),
          viewerTheme,
          keymap.of([
            { key: "Mod-s", run: () => { callbacks.current.onSave(); return true; } },
            { key: "Mod-g", run: gotoLine },
            ...defaultKeymap, ...historyKeymap, ...searchKeymap, indentWithTab,
          ]),
          readOnlyCompartment.current.of([EditorState.readOnly.of(props.readOnly), EditorView.editable.of(!props.readOnly)]),
          languageCompartment.current.of(props.language ?? []),
          wrapCompartment.current.of(props.lineWrapping ? EditorView.lineWrapping : []),
          EditorView.updateListener.of((u) => {
            if (u.docChanged && !u.transactions.some((tr) => tr.annotation(reloadTag))) callbacks.current.onDocChanged();
          }),
        ],
      }),
    });
    view.current = v;
    goTo(v, props.initialLocation);
    return () => { v.destroy(); view.current = null; };
    // The editor is created once per mount; later prop changes go through compartments below.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  useEffect(() => {
    view.current?.dispatch({ effects: readOnlyCompartment.current.reconfigure([EditorState.readOnly.of(props.readOnly), EditorView.editable.of(!props.readOnly)]) });
  }, [props.readOnly]);
  useEffect(() => {
    view.current?.dispatch({ effects: languageCompartment.current.reconfigure(props.language ?? []) });
  }, [props.language]);
  useEffect(() => {
    view.current?.dispatch({ effects: wrapCompartment.current.reconfigure(props.lineWrapping ? EditorView.lineWrapping : []) });
  }, [props.lineWrapping]);

  useImperativeHandle(ref, () => ({
    getDoc: () => view.current?.state.doc.toString() ?? "",
    setDoc: (text) => {
      const v = view.current;
      if (!v) return;
      const scrollTop = v.scrollDOM.scrollTop;
      const head = Math.min(v.state.selection.main.head, text.length);
      v.dispatch({
        changes: { from: 0, to: v.state.doc.length, insert: text },
        selection: EditorSelection.single(head),
        annotations: reloadTag.of(true),
      });
      v.scrollDOM.scrollTop = scrollTop;
    },
    goTo: (loc) => { if (view.current) goTo(view.current, loc); },
    focus: () => view.current?.focus(),
  }));

  return <div ref={host} className="h-full min-h-0" data-testid="code-editor" />;
});

function goTo(v: EditorView, loc: ViewerLocation): void {
  if (loc.line === null) return;
  const from = loc.line;
  const to = loc.end_line ?? loc.line;
  const lineNo = Math.min(Math.max(1, from), v.state.doc.lines);
  const line = v.state.doc.line(lineNo);
  const pos = Math.min(line.from + Math.max(0, (loc.col ?? 1) - 1), line.to);
  v.dispatch({
    selection: EditorSelection.cursor(pos),
    effects: [setHighlight.of({ from, to }), EditorView.scrollIntoView(pos, { y: "center" })],
  });
}
```

The `Mod-s` binding is what makes Ctrl/Cmd+S save inside the editor; `EditorPane` also binds it on `document` for when focus is outside the editor. `reloadTag` is how `setDoc` (a reload from disk) is told apart from a user edit in `updateListener`.

- [ ] **Step 6: Verify** — `npx vitest run src/viewer` → pass; `npx tsc --noEmit` → 0; `npm run build` → `dist/assets/` contains separate chunks for `lang-*` (dynamic imports worked).

- [ ] **Step 7: Commit** — `git add app/src/viewer && git commit -m "feat(viewer): CodeMirror editor, theme and target-line highlight"`

---

### Task 11: Viewer app — load, poll, save, close guard, choose/not-found (depends on Tasks 0, 7, 10; Task 8 for a live run)

**Files:**
- Create: `app/src/viewer/useViewerPolling.ts` + `.test.ts`, `app/src/viewer/EditorPane.tsx`, `app/src/viewer/EditorPane.test.tsx`
- Replace: `app/src/viewer/ViewerApp.tsx`

**Interfaces:** Consumes everything above. Produces the running window.

```ts
// useViewerPolling.ts
export function useViewerPolling(intervalMs: number, tick: () => Promise<void>, enabled: boolean): void;
```

- [ ] **Step 1: Write the failing polling-hook test** (`useViewerPolling.test.ts`):

```ts
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { renderHook } from "@testing-library/react";
import { useViewerPolling } from "./useViewerPolling";

describe("useViewerPolling", () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => vi.useRealTimers());

  const setVisibility = (state: DocumentVisibilityState) => {
    Object.defineProperty(document, "visibilityState", { value: state, configurable: true });
    document.dispatchEvent(new Event("visibilitychange"));
  };

  it("ticks on the interval only while visible, and once immediately on becoming visible", async () => {
    setVisibility("visible");
    const tick = vi.fn(async () => {});
    renderHook(() => useViewerPolling(2000, tick, true));
    expect(tick).toHaveBeenCalledTimes(1); // initial
    await vi.advanceTimersByTimeAsync(4000);
    expect(tick).toHaveBeenCalledTimes(3);
    setVisibility("hidden");
    await vi.advanceTimersByTimeAsync(6000);
    expect(tick).toHaveBeenCalledTimes(3);
    setVisibility("visible");
    expect(tick).toHaveBeenCalledTimes(4);
  });

  it("does not overlap ticks and stops when disabled", async () => {
    setVisibility("visible");
    let resolve: () => void = () => {};
    const tick = vi.fn(() => new Promise<void>((r) => { resolve = r; }));
    const { rerender } = renderHook(({ on }) => useViewerPolling(1000, tick, on), { initialProps: { on: true } });
    await vi.advanceTimersByTimeAsync(3000);
    expect(tick).toHaveBeenCalledTimes(1);
    resolve();
    await vi.advanceTimersByTimeAsync(1000);
    expect(tick).toHaveBeenCalledTimes(2);
    rerender({ on: false });
    resolve();
    await vi.advanceTimersByTimeAsync(5000);
    expect(tick).toHaveBeenCalledTimes(2);
  });
});
```

- [ ] **Step 2: Implement `useViewerPolling.ts`**

```ts
import { useEffect, useRef } from "react";

/** A visibility-gated interval that never overlaps its own ticks (spec §5). */
export function useViewerPolling(intervalMs: number, tick: () => Promise<void>, enabled: boolean): void {
  const tickRef = useRef(tick);
  tickRef.current = tick;

  useEffect(() => {
    if (!enabled) return;
    let disposed = false;
    let inFlight = false;
    let timer: ReturnType<typeof setInterval> | null = null;

    const run = async () => {
      if (disposed || inFlight || document.visibilityState !== "visible") return;
      inFlight = true;
      try { await tickRef.current(); } finally { inFlight = false; }
    };
    const start = () => { if (timer === null) timer = setInterval(run, intervalMs); };
    const stop = () => { if (timer !== null) { clearInterval(timer); timer = null; } };
    const onVisibility = () => {
      if (document.visibilityState === "visible") { void run(); start(); } else { stop(); }
    };

    document.addEventListener("visibilitychange", onVisibility);
    onVisibility();
    return () => {
      disposed = true;
      stop();
      document.removeEventListener("visibilitychange", onVisibility);
    };
  }, [intervalMs, enabled]);
}
```

- [ ] **Step 3: Write the failing `EditorPane` test** (`EditorPane.test.tsx`) — mocks the command module and `@tauri-apps/api/window`, and drives the banner through a poll:

```tsx
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, render, screen, fireEvent } from "@testing-library/react";
import EditorPane from "./EditorPane";
import type { ViewerState } from "../lib/types";

const H1 = "1".repeat(64);
const H2 = "2".repeat(64);
const b64 = (s: string) => btoa(s);

const commands = vi.hoisted(() => ({
  viewerReadFile: vi.fn(),
  viewerPollFile: vi.fn(),
  viewerWriteFile: vi.fn(),
}));
vi.mock("../lib/tauri-commands", () => commands);

const windowApi = vi.hoisted(() => ({ closeRequested: null as null | ((e: { preventDefault(): void }) => Promise<void> | void), destroy: vi.fn(), listeners: new Map<string, (e: { payload: unknown }) => void>() }));
vi.mock("@tauri-apps/api/window", () => ({
  getCurrentWindow: () => ({
    onCloseRequested: async (cb: typeof windowApi.closeRequested) => { windowApi.closeRequested = cb; return () => {}; },
    listen: async (name: string, cb: (e: { payload: unknown }) => void) => { windowApi.listeners.set(name, cb); return () => {}; },
    destroy: windowApi.destroy,
  }),
}));

const state: ViewerState = {
  project_id: "p", project_name: "Demo", raw_path: "notes.md",
  state: { kind: "resolved", container_path: "/workspace/demo/notes.md" },
  initial: { line: 1, col: null, end_line: null },
};

describe("EditorPane", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    Object.defineProperty(document, "visibilityState", { value: "visible", configurable: true });
    commands.viewerReadFile.mockResolvedValue({ contents_base64: b64("hello\n"), truncated: false, size: 6, hash: H1, editable: true, readonly_reason: null });
    commands.viewerPollFile.mockResolvedValue({ exists: true, hash: H1, size: 6 });
    commands.viewerWriteFile.mockResolvedValue(H2);
    windowApi.destroy.mockReset();
  });
  afterEach(() => vi.useRealTimers());

  it("loads the file and shows the path", async () => {
    render(<EditorPane state={state} />);
    expect(await screen.findByText("/workspace/demo/notes.md")).toBeInTheDocument();
    expect(commands.viewerReadFile).toHaveBeenCalledWith(1024 * 1024);
  });

  it("a changed poll on a clean document reloads silently", async () => {
    render(<EditorPane state={state} />);
    await screen.findByText("/workspace/demo/notes.md");
    commands.viewerPollFile.mockResolvedValue({ exists: true, hash: H2, size: 8 });
    commands.viewerReadFile.mockResolvedValue({ contents_base64: b64("changed\n"), truncated: false, size: 8, hash: H2, editable: true, readonly_reason: null });
    await act(async () => { await vi.advanceTimersByTimeAsync(2100); });
    expect(await screen.findByText(/Reloaded/)).toBeInTheDocument();
    expect(screen.queryByText(/Changed on disk/)).toBeNull();
  });

  it("a gone file shows the banner and disables Save", async () => {
    render(<EditorPane state={state} />);
    await screen.findByText("/workspace/demo/notes.md");
    commands.viewerPollFile.mockResolvedValue({ exists: false, hash: null, size: null });
    await act(async () => { await vi.advanceTimersByTimeAsync(2100); });
    expect(await screen.findByText(/no longer exists/)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /save/i })).toBeDisabled();
  });

  it("a save conflict shows the Changed on disk banner with both choices", async () => {
    commands.viewerWriteFile.mockRejectedValue(new Error("conflict: the file changed on disk since it was loaded."));
    render(<EditorPane state={state} />);
    await screen.findByText("/workspace/demo/notes.md");
    // Simulate an edit through the pane's test hook.
    fireEvent(document, new CustomEvent("triple-c-test-edit"));
    await act(async () => { fireEvent.click(screen.getByRole("button", { name: /save/i })); });
    expect(await screen.findByText(/Changed on disk/)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /Reload/ })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /Overwrite on save/ })).toBeInTheDocument();
  });

  it("closing with unsaved edits is intercepted", async () => {
    render(<EditorPane state={state} />);
    await screen.findByText("/workspace/demo/notes.md");
    fireEvent(document, new CustomEvent("triple-c-test-edit"));
    const prevent = vi.fn();
    await act(async () => { await windowApi.closeRequested?.({ preventDefault: prevent }); });
    expect(prevent).toHaveBeenCalled();
    expect(await screen.findByText(/Unsaved changes/)).toBeInTheDocument();
    await act(async () => { fireEvent.click(screen.getByRole("button", { name: /Discard/ })); });
    expect(windowApi.destroy).toHaveBeenCalled();
  });
});
```

`triple-c-test-edit` is a `document` event the pane listens to **only** under `import.meta.env.MODE === "test"` (`if (import.meta.env.MODE === "test") document.addEventListener("triple-c-test-edit", () => dispatch({ type: "edited" }))`); jsdom has no layout, so driving CodeMirror's contenteditable in a test is not reliable.

- [ ] **Step 4: Implement `EditorPane.tsx`**

```tsx
import { useCallback, useEffect, useMemo, useReducer, useRef, useState } from "react";
import { getCurrentWindow } from "@tauri-apps/api/window";
import type { Extension } from "@codemirror/state";
import Button from "../components/ui/Button";
import { decodeBase64, imageMimeFor, previewLimit } from "../components/projects/home/filePreview";
import { viewerPollFile, viewerReadFile, viewerWriteFile } from "../lib/tauri-commands";
import type { ViewerLocation, ViewerState } from "../lib/types";
import { CodeEditor, type CodeEditorHandle } from "./CodeEditor";
import { classifyViewerFile, type Editability } from "./editability";
import { languageFor, wrapsLines } from "./languages";
import { useViewerPolling } from "./useViewerPolling";
import { canSave, initialViewerState, pollEffect, reduceViewer } from "./viewerState";

const POLL_MS = 2000;
export const GOTO_EVENT = "file-viewer-goto";

type Loaded =
  | { kind: "loading" }
  | { kind: "error"; message: string }
  | { kind: "text"; doc: string; editability: Editability; language: Extension | null }
  | { kind: "image"; url: string; editability: Editability }
  | { kind: "binary"; editability: Editability };

export default function EditorPane({ state }: { state: ViewerState }) {
  const path = state.state.kind === "resolved" ? state.state.container_path : "";
  const [loaded, setLoaded] = useState<Loaded>({ kind: "loading" });
  const [doc, dispatch] = useReducer(reduceViewer, initialViewerState);
  const [closing, setClosing] = useState(false);
  const [saveError, setSaveError] = useState<string | null>(null);
  const editor = useRef<CodeEditorHandle>(null);
  const docRef = useRef(doc);
  docRef.current = doc;

  const load = useCallback(async (): Promise<void> => {
    try {
      const file = await viewerReadFile(previewLimit(path));
      const bytes = decodeBase64(file.contents_base64);
      const editability = classifyViewerFile(path, file, bytes);
      dispatch({ type: "loaded", hash: file.hash, truncated: file.truncated });
      if (editability.kind === "image") {
        const url = URL.createObjectURL(new Blob([bytes], { type: imageMimeFor(path) ?? "image/png" }));
        setLoaded((prev) => { if (prev.kind === "image") URL.revokeObjectURL(prev.url); return { kind: "image", url, editability }; });
      } else if (editability.kind === "binary") {
        setLoaded({ kind: "binary", editability });
      } else {
        const text = new TextDecoder().decode(bytes);
        const language = await languageFor(path);
        setLoaded({ kind: "text", doc: text, editability, language });
        editor.current?.setDoc(text);
      }
    } catch (e) {
      setLoaded({ kind: "error", message: e instanceof Error ? e.message : String(e) });
    }
  }, [path]);

  useEffect(() => { void load(); }, [load]);

  // Poll (spec §5). A reload replaces the document only when the reducer says so.
  useViewerPolling(POLL_MS, async () => {
    let poll;
    try { poll = await viewerPollFile(); } catch { dispatch({ type: "poll_failed" }); return; }
    const before = docRef.current;
    const after = reduceViewer(before, { type: "polled", poll });
    dispatch({ type: "polled", poll });
    if (pollEffect(before, after) === "reload") {
      try {
        const file = await viewerReadFile(previewLimit(path));
        const text = new TextDecoder().decode(decodeBase64(file.contents_base64));
        editor.current?.setDoc(text);
        dispatch({ type: "reloaded", hash: file.hash });
      } catch { dispatch({ type: "poll_failed" }); }
    }
  }, loaded.kind === "text" || loaded.kind === "image" || loaded.kind === "binary");

  const editable = loaded.kind === "text" && loaded.editability.editable;
  const saveEnabled = canSave(doc, editable);

  const save = useCallback(async () => {
    if (!saveEnabled || !editor.current || !docRef.current.baseHash) return;
    setSaveError(null);
    const text = editor.current.getDoc();
    const b64 = btoa(String.fromCharCode(...new TextEncoder().encode(text)));
    try {
      const hash = await viewerWriteFile(b64, docRef.current.baseHash);
      dispatch({ type: "saved", hash });
      if (closing) await getCurrentWindow().destroy();
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      if (msg.startsWith("conflict:")) dispatch({ type: "save_conflict" });
      else if (msg.startsWith("gone:")) dispatch({ type: "save_gone" });
      else setSaveError(msg);
    }
  }, [saveEnabled, closing]);

  // Ctrl/Cmd+S outside the editor; the editor's keymap covers inside.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => { if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "s") { e.preventDefault(); void save(); } };
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, [save]);

  // Close guard + goto (spec §3/§5).
  useEffect(() => {
    const win = getCurrentWindow();
    const unlisten: Array<() => void> = [];
    void win.onCloseRequested((event) => {
      if (docRef.current.doc === "dirty") { event.preventDefault(); setClosing(true); }
    }).then((u) => unlisten.push(u));
    void win.listen<ViewerLocation>(GOTO_EVENT, (e) => editor.current?.goTo(e.payload)).then((u) => unlisten.push(u));
    return () => unlisten.forEach((u) => u());
  }, []);

  useEffect(() => {
    if (import.meta.env.MODE !== "test") return;
    const onEdit = () => dispatch({ type: "edited" });
    document.addEventListener("triple-c-test-edit", onEdit);
    return () => document.removeEventListener("triple-c-test-edit", onEdit);
  }, []);

  const reloadDiscarding = useCallback(async () => {
    try {
      const file = await viewerReadFile(previewLimit(path));
      editor.current?.setDoc(new TextDecoder().decode(decodeBase64(file.contents_base64)));
      dispatch({ type: "reloaded", hash: file.hash });
    } catch (e) { setSaveError(e instanceof Error ? e.message : String(e)); }
  }, [path]);

  const badge = useMemo(() => {
    if (doc.containerDown) return "Container not running";
    if (doc.disk === "gone") return "File no longer exists";
    if (loaded.kind === "text" && !loaded.editability.editable) return `Read-only — ${loaded.editability.reason}`;
    if (doc.disk === "changed") return "Changed on disk";
    if (doc.doc === "dirty") return "Unsaved";
    if (doc.justReloaded) return "Reloaded";
    return "Saved";
  }, [doc, loaded]);

  return (
    <div className="flex h-screen flex-col bg-[var(--bg-primary)] text-[var(--text-primary)]">
      <header className="flex items-center gap-3 border-b border-[var(--border-color)] bg-[var(--bg-secondary)] px-3 py-2 text-xs">
        <span className="truncate font-mono" title={path}>{path}</span>
        <span className="text-[var(--text-secondary)]">{state.project_name}</span>
        <span className="ml-auto rounded-[var(--radius-control)] border border-[var(--border-color)] px-2 py-0.5" aria-live="polite">{badge}</span>
        <Button variant="primary" size="sm" onClick={() => void save()} disabled={!saveEnabled}>Save</Button>
      </header>

      {doc.containerDown && <Banner text="Container not running — the file cannot be read or saved until the project starts again." />}
      {doc.disk === "gone" && <Banner text="This file no longer exists in the container. Your text is kept so you can copy it; saving is disabled." />}
      {doc.disk === "changed" && doc.doc === "dirty" && (
        <Banner text="Changed on disk while you were editing.">
          <Button size="sm" onClick={() => void reloadDiscarding()}>Reload (discard mine)</Button>
          <Button size="sm" onClick={() => dispatch({ type: "overwrite_on_save" })}>Overwrite on save</Button>
        </Banner>
      )}
      {saveError && <Banner text={saveError} />}
      {closing && (
        <Banner text="Unsaved changes — save before closing?">
          <Button variant="primary" size="sm" onClick={() => void save()} disabled={!saveEnabled}>Save</Button>
          <Button variant="danger" size="sm" onClick={() => void getCurrentWindow().destroy()}>Discard</Button>
          <Button size="sm" onClick={() => setClosing(false)}>Cancel</Button>
        </Banner>
      )}

      <main className="min-h-0 flex-1">
        {loaded.kind === "loading" && <p className="p-4 text-sm text-[var(--text-secondary)]">Loading…</p>}
        {loaded.kind === "error" && <p className="p-4 text-sm">{loaded.message}</p>}
        {loaded.kind === "binary" && <p className="p-4 text-sm">{loaded.editability.reason}</p>}
        {loaded.kind === "image" && <img src={loaded.url} alt={path} className="max-h-full max-w-full object-contain p-4" />}
        {loaded.kind === "text" && (
          <CodeEditor
            ref={editor}
            initialDoc={loaded.doc}
            readOnly={!loaded.editability.editable}
            language={loaded.language}
            lineWrapping={wrapsLines(path)}
            initialLocation={state.initial}
            onDocChanged={() => dispatch({ type: "edited" })}
            onSave={() => void save()}
          />
        )}
      </main>
    </div>
  );
}

function Banner({ text, children }: { text: string; children?: React.ReactNode }) {
  return (
    <div role="status" className="flex flex-wrap items-center gap-2 border-b border-[var(--warning)] bg-[var(--warning-muted)] px-3 py-2 text-xs">
      <span>{text}</span>
      {children}
    </div>
  );
}
```

- [ ] **Step 5: Replace `ViewerApp.tsx`**

```tsx
import { useEffect, useState } from "react";
import Button from "../components/ui/Button";
import { viewerChooseFile, viewerGetState } from "../lib/tauri-commands";
import type { ViewerState } from "../lib/types";
import EditorPane from "./EditorPane";

export default function ViewerApp() {
  const [state, setState] = useState<ViewerState | { error: string } | null>(null);

  useEffect(() => {
    viewerGetState().then(setState, (e) => setState({ error: e instanceof Error ? e.message : String(e) }));
  }, []);

  if (state === null) return <p className="p-4 text-sm text-[var(--text-secondary)]">Loading…</p>;
  if ("error" in state) return <p className="p-4 text-sm">{state.error}</p>;

  switch (state.state.kind) {
    case "resolved":
      return <EditorPane state={state} />;
    case "not_found":
      return (
        <div className="p-4 text-sm">
          <p>Could not find <span className="font-mono">{state.raw_path}</span> in the container. Looked in:</p>
          <ul className="mt-2 list-disc pl-6 font-mono text-xs text-[var(--text-secondary)]">
            {state.state.tried.map((p) => <li key={p}>{p}</li>)}
          </ul>
        </div>
      );
    case "choose":
      return (
        <div className="p-4 text-sm">
          <p>Several files match <span className="font-mono">{state.raw_path}</span>. Open which?</p>
          <ul className="mt-2 flex flex-col gap-1">
            {state.state.candidates.map((p, i) => (
              <li key={p}>
                <Button size="sm" onClick={() => viewerChooseFile(i).then(setState, (e) => setState({ error: String(e) }))}>
                  <span className="font-mono">{p}</span>
                </Button>
              </li>
            ))}
          </ul>
        </div>
      );
  }
}
```

- [ ] **Step 6: Verify** — `npx vitest run src/viewer` → pass; `npx vitest run` → whole suite passes; `npx tsc --noEmit` → 0; `npm run build` → 0.

- [ ] **Step 7: Commit** — `git add app/src/viewer && git commit -m "feat(viewer): viewer window UI with live reload, save and close guard"`

---

## Group C

### Task 12: Documentation, threat-model census, manual verification (depends on all)

**Files:**
- Modify: `CLAUDE.md` (Frontend Structure, Backend Structure, Key Conventions), `app/src-tauri/capabilities/default.json` (description)

- [ ] **Step 1: `default.json` description** — append one sentence before "On the CSP side": *"A second capability file, `file-viewer.json`, covers the `file-viewer-*` windows the terminal file viewer opens on `viewer.html`; it is the only other local-origin window, its grants are listed and justified there, and its one non-obvious grant (`core:window:allow-destroy`) exists because `onCloseRequested` cannot close a window without it. App commands stay ungated by capability in both files; the viewer's are gated by label in `commands/file_viewer_commands.rs`."*

- [ ] **Step 2: `CLAUDE.md`** — add:
  - Under Frontend Structure, after the `components/terminal/TerminalView.tsx` bullet: *"**`viewer/`** — the terminal file viewer's window (second Vite entry `viewer.html` → `src/viewer/main.tsx`; CodeMirror 6). `lib/filePathLinks.ts` decides what a path is; `components/terminal/filePathLinkProvider.ts` registers it with xterm. The OSC 8 handler now runs with `allowNonHttpProtocols` on and dispatches `file:` to the viewer, so every other scheme must be refused *there*. `viewer.html` must never carry an inline `<style>` — Tauri would add a style nonce and CodeMirror's injected styles would stop applying."*
  - Under Backend Structure, a `file_viewer/` bullet: *"one window per click (`file-viewer-<n>`), a managed `ViewerRegistry`, resolution by probing `/workspace/<p>` then `/workspace/<mount>/<p>` in one exec as `claude`, polling by `sha256sum`, saves staged in `/tmp` and swapped in by a `sh` script as the container user (spec §5 says why the archive API never writes to the target directory). Commands take `window: tauri::Window`, gate on the label and act on the caller's own registry entry — no viewer command accepts a path. The residual risk that any local window can call any app command is deliberate and documented; the AppManifest lockdown spec closes it."*
  - Under Key Conventions: *"A new local window needs its own capability file (`capabilities/file-viewer.json` is the model), and `lib.rs`'s `on_window_event` stays guarded on `label() == \"main\"`."*

- [ ] **Step 3: Full verification** — `cd app && npx tsc --noEmit && npx vitest run && npm run build && cd src-tauri && cargo test --offline && cargo clippy --offline` → all clean.

- [ ] **Step 4: Commit** — `git add CLAUDE.md app/src-tauri/capabilities/default.json && git commit -m "docs: terminal file viewer structure and capability census"`

- [ ] **Step 5: Manual verification checklist** (needs `npx tauri dev` on a machine with a display and a running project container; not possible in the planning container):

  1. In a Claude session, ask for a file listing; click `src/…` paths, `/workspace/...` absolute paths, a `path:line` and a `path:start-end`. Each opens its own window titled `<basename> — <project>`, scrolled to and highlighting the line/range.
  2. Click the same path again → the existing window is focused and re-highlights; no duplicate.
  3. Open 20 windows; the 21st click shows the "20 file windows are already open" toast.
  4. Edit, `Ctrl+S`: file changes in the container (`cat` it in a bash tab); mode preserved (`stat -c %a`); owner is the container user.
  5. While a window is open, have Claude edit the file: clean window reloads with "Reloaded"; a dirty window shows "Changed on disk" with both buttons; "Overwrite on save" then Save succeeds; "Reload (discard mine)" drops the edits.
  6. Save while the file changed between polls → "Changed on disk" banner, no data written.
  7. Delete the file in a bash tab → "File no longer exists", text still copyable, Save disabled.
  8. Stop the project with a window open → "Container not running" banner; start it → banner clears, polling resumes.
  9. Click a path under `/etc` or a `file:///etc/hosts` OSC 8 link → read-only badge with the write-roots reason.
  10. Close a dirty window with the X → Save / Discard / Cancel bar; Cancel keeps it open; Discard closes.
  11. Close the *main* window with viewers open → app exits, viewers close.
  12. Ctrl/Cmd+Shift+I opens devtools in a viewer in dev; in a release build the CSP console shows no violations (CodeMirror styles apply).
  13. A wrapped long path (narrow the terminal) underlines across the wrap and opens.
  14. Image (`.png`) opens read-only; a `.bin`/binary shows "not text".

---

## Dependency summary

```
Task 0 ──┬────────────────────────────────┐
         │                                │
 Group A (parallel): 1  2  3  4  5  6  7  │   (4, 5, 7 use Task 0's types; 1–3, 6 do not)
         │  │  │  │  │  │  │              │
         └──┴──┴──┼──┼──┼──┼── Task 8 (Rust commands; needs 1, 2, 3, 6)
                  └──┴──┼──┼── Task 9 (terminal integration; needs 0, 4, 5)
                        └──┼── Task 10 (editor; needs 6)
                           └── Task 11 (viewer app; needs 0, 7, 10; 8 to run live)
 Task 12 (docs + manual) after all.
```

Parallel groups: **{0}** → **{1, 2, 3, 4, 5, 6, 7}** → **{8, 9, 10}** → **{11}** → **{12}**. Task 0 owns every shared file (`file_viewer/mod.rs`'s module list, `lib.rs`'s `mod` line, the `pub(crate)` changes in `file_commands.rs`), so Group A tasks each replace one placeholder file or create new files and never edit the same file; the one exception is Task 6, which *appends* a test to `file_viewer/mod.rs` — a clean append, no conflict with Task 0's content.
