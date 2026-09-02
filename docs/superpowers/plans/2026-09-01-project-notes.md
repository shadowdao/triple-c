# Project Notes Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a per-project Notes surface — a Project Home sub-tab plus a collapsible side dock — where each note carries a **Send to agent** button that injects it into a running Claude session's prompt.

**Architecture:** Notes live host-side in one JSON file per project (`<data_dir>/triple-c/notes/{project_id}.json`), reached through free functions in `storage/notes_store.rs` and three Tauri commands. The frontend caches them in a `useNotes` hook. Sending a note converts its newlines to `\x1b\r` — the sequence Claude Code's own `/terminal-setup` installs — and pushes it through `useTerminal`'s existing ordered input queue without a trailing CR, so the user presses Enter. The dock takes space **inward**; the OS window is never resized.

**Tech Stack:** Rust (Tauri v2.11, serde, chrono, uuid), React 18 + TypeScript, zustand, Tailwind v4, Vitest + React Testing Library.

**Spec:** `docs/superpowers/specs/2026-09-01-project-notes-design.md` — read it first; it carries the reasoning this plan only executes, including why the dock does not widen the window (§6.1).

## Global Constraints

- **Design tokens only.** All colour comes from CSS custom properties in `app/src/index.css`. Filled buttons use `--accent-emphasis`, never `--accent` (it fails WCAG AA behind white text). Use `--text-disabled`, never `disabled:opacity-50`.
- **Never write `focus:outline-none`.** A global `:focus-visible` ring is defined in `index.css`.
- **Radii:** only `--radius-control` (6px) and `--radius-panel` (8px).
- **Frontend types in `lib/types.ts` must stay in sync with Rust structs in `models/`.** Field names are snake_case on both sides.
- **Every `#[tauri::command]` must be added to `generate_handler![]` in `lib.rs`.** A test at `lib.rs:822-862` scans `src/` for commands and fails the suite if one is unregistered.
- **No `tempfile` crate.** Rust tests build temp dirs with `std::env::temp_dir().join(format!("triple-c-…-{}", uuid::Uuid::new_v4().simple()))` and clean up with `fs::remove_dir_all(&dir).ok()`.
- **Frontend tests mock `lib/tauri-commands`, never `@tauri-apps/api/core`.** No test in the tree mocks `invoke` directly.
- **Timestamps** are `chrono::Utc::now().to_rfc3339()` stored as `String`.
- **Commands return `Result<T, String>`;** `State<'_, AppState>` is always the last parameter when present.
- Run frontend tests with `cd app && npx vitest run <path>`; Rust tests with `cd app/src-tauri && cargo test <filter>`.

---

## File Structure

**Create:**
- `app/src-tauri/src/models/note.rs` — the `Note` record. One responsibility: the shape and its constructor.
- `app/src-tauri/src/storage/notes_store.rs` — per-project JSON persistence. Free functions, no struct, no `AppState` entry.
- `app/src-tauri/src/commands/notes_commands.rs` — the three IPC entry points.
- `app/src/hooks/useNotes.ts` — load/save/delete plus a `SaveState` for the indicator.
- `app/src/lib/sessionName.ts` — the session display-name rule, extracted from its two copies in `MainTabs.tsx`.
- `app/src/lib/claudeInput.ts` — the `\x1b\r` newline rule, in one place.
- `app/src/components/notes/NotesPanel.tsx` — list + editor + empty state. Shared verbatim by the tab and the dock so the two surfaces cannot drift.
- `app/src/components/projects/home/NotesTab.tsx` — the Project Home sub-tab; a thin wrapper over `NotesPanel`.
- `app/src/components/notes/NoteEditor.tsx` — title + body, save on blur.
- `app/src/components/notes/SendToAgentButton.tsx` — target resolution and injection.
- `app/src/components/layout/NotesDock.tsx` — the inward-pushing dock, with a resize separator.
- `app/src/store/dockWidth.test.ts` — tests for the exported width clamp.

**Modify:**
- `app/src-tauri/src/models/mod.rs`, `app/src-tauri/src/storage/mod.rs`, `app/src-tauri/src/commands/mod.rs` — module declarations.
- `app/src-tauri/src/lib.rs` — register three commands.
- `app/src-tauri/src/commands/project_commands.rs` — delete a project's notes on removal.
- `app/src/lib/types.ts` — the `Note` interface.
- `app/src/lib/tauri-commands.ts` — three wrappers.
- `app/src/components/layout/MainTabs.tsx` — call the extracted display-name helper from both existing sites.
- `app/src/components/terminal/TerminalView.tsx` — use the extracted newline constant.
- `app/src/components/projects/home/ProjectHome.tsx` — one `TABS` entry, one panel branch.
- `app/src/store/appState.ts` — dock open/width state, persisted to localStorage.
- `app/src/App.tsx` — render the dock beside `<main>`.
- `app/src/components/layout/StatusBar.tsx` — the dock toggle.

Notes are deliberately **not** a field on `Project`: see spec §1, "Why not a field on `Project`".

---

### Task 1: The `Note` model and `notes_store`

**Files:**
- Create: `app/src-tauri/src/models/note.rs`
- Create: `app/src-tauri/src/storage/notes_store.rs`
- Modify: `app/src-tauri/src/models/mod.rs`
- Modify: `app/src-tauri/src/storage/mod.rs`
- Test: inline `#[cfg(test)]` in `notes_store.rs`

**Interfaces:**
- Consumes: nothing.
- Produces:
  - `crate::models::Note { id: String, title: String, body: String, pinned: bool, created_at: String, updated_at: String }`, with `Note::new(title: String, body: String) -> Note`.
  - `crate::storage::notes_store::load(project_id: &str) -> Result<Vec<Note>, String>`
  - `crate::storage::notes_store::upsert(project_id: &str, note: Note) -> Result<Note, String>`
  - `crate::storage::notes_store::delete(project_id: &str, note_id: &str) -> Result<(), String>`
  - `crate::storage::notes_store::clear(project_id: &str) -> Result<(), String>`

- [ ] **Step 1: Write the model**

Create `app/src-tauri/src/models/note.rs`:

```rust
use serde::{Deserialize, Serialize};

/// One note. A scratchpad entry the user can also fire at a running Claude
/// session.
///
/// Deliberately has no `kind`/`type` field. What makes a note "for the agent"
/// is that the user pressed Send, not a mode chosen when it was written — a
/// classification decision at writing time is one the user is least willing to
/// make, and it would turn one pane into two features.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Note {
    pub id: String,
    pub title: String,
    pub body: String,
    /// Pinned notes sort first, then by `updated_at` descending.
    #[serde(default)]
    pub pinned: bool,
    pub created_at: String,
    pub updated_at: String,
}

impl Note {
    pub fn new(title: String, body: String) -> Self {
        let now = chrono::Utc::now().to_rfc3339();
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            title,
            body,
            pinned: false,
            created_at: now.clone(),
            updated_at: now,
        }
    }
}
```

Add to `app/src-tauri/src/models/mod.rs`, keeping the existing alphabetical `pub mod` / `pub use` pairing:

```rust
pub mod note;
pub use note::*;
```

- [ ] **Step 2: Write the failing tests**

Create `app/src-tauri/src/storage/notes_store.rs` containing **only** the test module for now, so the tests fail to compile against absent functions:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "triple-c-notes-{}-{}",
            tag,
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&dir).expect("temp dir");
        dir
    }

    #[test]
    fn project_ids_cannot_escape_the_notes_directory() {
        // The id arrives over IPC. It must not be able to steer the write.
        assert_eq!(sanitize("../../etc/passwd"), "______etc_passwd");
        assert_eq!(sanitize("a/b"), "a_b");
        assert_eq!(sanitize("a\\b"), "a_b");
        // A real UUID must survive untouched, or every note file would move
        // the first time this function changed.
        assert_eq!(
            sanitize("ab62cd24-51aa-4645-8f5c-17a124062050"),
            "ab62cd24-51aa-4645-8f5c-17a124062050"
        );
    }

    #[test]
    fn a_missing_file_is_an_empty_list_not_an_error() {
        let dir = temp_dir("missing");
        assert_eq!(load_in(&dir, "nobody").unwrap(), Vec::<Note>::new());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn an_upserted_note_round_trips() {
        let dir = temp_dir("roundtrip");
        let note = Note::new("Deploy steps".into(), "one\ntwo".into());
        let saved = upsert_in(&dir, "p1", note.clone()).unwrap();
        assert_eq!(saved.id, note.id);

        let loaded = load_in(&dir, "p1").unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].body, "one\ntwo");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn upserting_an_existing_id_replaces_it_and_keeps_created_at() {
        let dir = temp_dir("replace");
        let mut note = Note::new("Title".into(), "first".into());
        upsert_in(&dir, "p1", note.clone()).unwrap();

        note.body = "second".into();
        note.created_at = "1999-01-01T00:00:00Z".into(); // a client must not rewrite this
        let saved = upsert_in(&dir, "p1", note.clone()).unwrap();

        let loaded = load_in(&dir, "p1").unwrap();
        assert_eq!(loaded.len(), 1, "an upsert must not append a duplicate");
        assert_eq!(loaded[0].body, "second");
        assert_ne!(
            saved.created_at, "1999-01-01T00:00:00Z",
            "created_at is owned by the store, not by whatever the webview sent"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn deleting_a_note_leaves_the_others_and_a_missing_one_is_success() {
        let dir = temp_dir("delete");
        let keep = upsert_in(&dir, "p1", Note::new("keep".into(), "".into())).unwrap();
        let drop = upsert_in(&dir, "p1", Note::new("drop".into(), "".into())).unwrap();

        delete_in(&dir, "p1", &drop.id).unwrap();
        let loaded = load_in(&dir, "p1").unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].id, keep.id);

        // Idempotent: removing what is already gone is not an error, because
        // the UI can retry a delete it never saw the result of.
        delete_in(&dir, "p1", &drop.id).unwrap();
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn an_unreadable_file_is_copied_aside_and_reads_as_empty() {
        // Same reasoning as migration_store: a corrupt file must not make the
        // tab permanently unusable, and the bytes must not be destroyed.
        let dir = temp_dir("corrupt");
        let path = notes_path_in(&dir, "p1");
        std::fs::write(&path, b"{ not json").unwrap();

        assert_eq!(load_in(&dir, "p1").unwrap(), Vec::<Note>::new());
        assert!(path.exists(), "the unreadable file is left in place");

        let copies: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().contains(".corrupt-"))
            .collect();
        assert_eq!(copies.len(), 1, "the bytes must be kept exactly once");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_write_leaves_no_temp_file_behind() {
        let dir = temp_dir("tmp");
        upsert_in(&dir, "p1", Note::new("t".into(), "b".into())).unwrap();
        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "the rename must have consumed the temp file");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn clearing_a_project_removes_its_file_and_missing_is_success() {
        let dir = temp_dir("clear");
        upsert_in(&dir, "p1", Note::new("t".into(), "b".into())).unwrap();
        assert!(notes_path_in(&dir, "p1").exists());

        clear_in(&dir, "p1").unwrap();
        assert!(!notes_path_in(&dir, "p1").exists());
        clear_in(&dir, "p1").unwrap(); // idempotent
        std::fs::remove_dir_all(&dir).ok();
    }
}
```

Add to `app/src-tauri/src/storage/mod.rs`, beside the other non-re-exported free-function modules:

```rust
pub mod notes_store;
```

- [ ] **Step 3: Run the tests to verify they fail**

Run: `cd app/src-tauri && cargo test notes_store`
Expected: FAIL — compile errors, `cannot find function 'sanitize'`, `'load_in'`, `'upsert_in'`, `'delete_in'`, `'clear_in'`, `'notes_path_in'`.

- [ ] **Step 4: Write the implementation**

Prepend to `app/src-tauri/src/storage/notes_store.rs`, above the test module:

```rust
//! Host-side persistence for per-project notes.
//!
//! One JSON file per project under `<data_dir>/triple-c/notes/`, on the same
//! free-function shape as `migration_store` — no struct, nothing in
//! `AppState`, no in-memory copy. `ProjectsStore` holds a `Mutex` because it
//! caches the project list; a store that reads and writes the file per call
//! has nothing to cache and nothing to guard.
//!
//! Deliberately *not* a field on `Project`. `projects.json` is rewritten on
//! every blur by the debounced-nothing save path in `useSaveState`, so notes
//! there would mean the whole project list is rewritten per edit, and a note
//! save racing a Config save would silently drop one of them.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use crate::models::Note;

/// Serialises the read-modify-write half of an upsert or delete.
///
/// Nothing here is cached, so there is no shared state to protect — but an
/// upsert reads the whole file, edits one entry and writes it back, and two of
/// those interleaving would lose whichever note was written first. The read
/// path does not take it.
fn write_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

/// `<data_dir>/triple-c/notes`, created on demand.
pub fn notes_dir() -> Result<PathBuf, String> {
    let dir = dirs::data_dir()
        .ok_or_else(|| {
            "Could not determine data directory. Set XDG_DATA_HOME on Linux.".to_string()
        })?
        .join("triple-c")
        .join("notes");
    fs::create_dir_all(&dir).map_err(|e| format!("Failed to create notes directory: {}", e))?;
    Ok(dir)
}

/// Project ids are UUIDs, but they arrive over IPC, so refuse to let one steer
/// the write anywhere but the notes directory.
fn sanitize(project_id: &str) -> String {
    project_id
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect()
}

fn notes_path_in(dir: &Path, project_id: &str) -> PathBuf {
    dir.join(format!("{}.json", sanitize(project_id)))
}

// ── Public API. Each resolves the real directory, then defers to the `_in`
// variant, which is what the tests exercise against a temp dir. `ProjectsStore`
// hardcodes `dirs::data_dir()` in its constructor and is therefore untestable
// as a unit; this store does not inherit that. ─────────────────────────────

pub fn load(project_id: &str) -> Result<Vec<Note>, String> {
    load_in(&notes_dir()?, project_id)
}

pub fn upsert(project_id: &str, note: Note) -> Result<Note, String> {
    upsert_in(&notes_dir()?, project_id, note)
}

pub fn delete(project_id: &str, note_id: &str) -> Result<(), String> {
    delete_in(&notes_dir()?, project_id, note_id)
}

/// Remove a project's notes file entirely. Missing is success.
pub fn clear(project_id: &str) -> Result<(), String> {
    clear_in(&notes_dir()?, project_id)
}

// ── Implementation ─────────────────────────────────────────────────────────

/// Read a project's notes. A missing file is an empty list.
///
/// **An unparseable file is copied aside and left in place**, then reported as
/// empty. Erroring instead would make the Notes tab permanently unusable for
/// that project with no way out through the UI; deleting instead would destroy
/// the only copy of what the user wrote. The copy is timestamped so a second
/// corruption cannot overwrite the first — which is the one taken before
/// anything rewrote the file, and therefore the one worth having.
fn load_in(dir: &Path, project_id: &str) -> Result<Vec<Note>, String> {
    let path = notes_path_in(dir, project_id);
    if !path.exists() {
        return Ok(Vec::new());
    }
    let data = fs::read_to_string(&path).map_err(|e| format!("Failed to read notes: {}", e))?;
    match serde_json::from_str::<Vec<Note>>(&data) {
        Ok(notes) => Ok(notes),
        Err(e) => {
            keep_corrupt_copy(&path, &chrono::Utc::now());
            log::error!(
                "Failed to parse notes for project {}: {} — treating as empty; the file is \
                 left in place and a copy was kept beside it",
                project_id,
                e
            );
            Ok(Vec::new())
        }
    }
}

fn keep_corrupt_copy(path: &Path, now: &chrono::DateTime<chrono::Utc>) {
    let backup = path.with_extension(format!("json.corrupt-{}.bak", now.format("%Y%m%d-%H%M%S")));
    if backup.exists() {
        return;
    }
    if let Err(e) = fs::copy(path, &backup) {
        log::error!("Could not keep a copy of the unreadable notes file: {}", e);
    }
}

/// Insert or replace one note, leaving the rest untouched.
///
/// `created_at` and `id` are the store's, not the caller's: the webview sends
/// a whole `Note` back and must not be able to rewrite when a note was made.
/// `updated_at` is stamped here for the same reason.
fn upsert_in(dir: &Path, project_id: &str, mut note: Note) -> Result<Note, String> {
    let _guard = write_lock().lock().unwrap_or_else(|e| e.into_inner());
    let mut notes = load_in(dir, project_id)?;
    note.updated_at = chrono::Utc::now().to_rfc3339();
    match notes.iter_mut().find(|n| n.id == note.id) {
        Some(existing) => {
            note.created_at = existing.created_at.clone();
            *existing = note.clone();
        }
        None => notes.push(note.clone()),
    }
    save_all(dir, project_id, &notes)?;
    Ok(note)
}

/// Remove one note. Removing one that is already gone is success — the UI can
/// retry a delete whose result it never saw.
fn delete_in(dir: &Path, project_id: &str, note_id: &str) -> Result<(), String> {
    let _guard = write_lock().lock().unwrap_or_else(|e| e.into_inner());
    let mut notes = load_in(dir, project_id)?;
    let before = notes.len();
    notes.retain(|n| n.id != note_id);
    if notes.len() == before {
        return Ok(());
    }
    save_all(dir, project_id, &notes)
}

fn clear_in(dir: &Path, project_id: &str) -> Result<(), String> {
    let path = notes_path_in(dir, project_id);
    match fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("Failed to remove notes: {}", e)),
    }
}

/// Atomically **and durably** write the whole list.
///
/// Write-temp-then-rename alone is only half of it. `fs::write` returns once
/// the bytes are in the page cache; the rename is atomic with respect to other
/// readers, not to power loss. Losing power in that window leaves the rename
/// applied and the data not written — a truncated file, produced by the code
/// whose job is to prevent one. So the file is fsynced before the rename and
/// the directory after it, since the rename is directory metadata. Notes are
/// prose the user typed and nothing else holds a copy.
fn save_all(dir: &Path, project_id: &str, notes: &[Note]) -> Result<(), String> {
    let path = notes_path_in(dir, project_id);
    let data = serde_json::to_string_pretty(notes)
        .map_err(|e| format!("Failed to serialize notes: {}", e))?;
    let tmp = path.with_extension("json.tmp");

    {
        use std::io::Write;
        let mut file =
            fs::File::create(&tmp).map_err(|e| format!("Failed to write notes: {}", e))?;
        file.write_all(data.as_bytes())
            .map_err(|e| format!("Failed to write notes: {}", e))?;
        file.sync_all()
            .map_err(|e| format!("Failed to flush notes to disk: {}", e))?;
    }

    fs::rename(&tmp, &path).map_err(|e| format!("Failed to commit notes: {}", e))?;
    sync_dir(&path);
    let _ = project_id;
    Ok(())
}

/// fsync the directory holding `path`, so the rename survives power loss.
///
/// Best effort only where it is meaningless: Windows has no directory handle
/// to sync and returns an error for the attempt, so a failure is logged rather
/// than propagated. The file's own `sync_all` carries the data and is not best
/// effort.
fn sync_dir(path: &Path) {
    let Some(dir) = path.parent() else { return };
    if let Err(e) = fs::File::open(dir).and_then(|d| d.sync_all()) {
        log::debug!(
            "Could not fsync the notes directory {}: {} — the file itself was flushed",
            dir.display(),
            e
        );
    }
}
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cd app/src-tauri && cargo test notes_store`
Expected: PASS — 7 tests.

- [ ] **Step 6: Commit**

```bash
git add app/src-tauri/src/models/note.rs app/src-tauri/src/models/mod.rs \
        app/src-tauri/src/storage/notes_store.rs app/src-tauri/src/storage/mod.rs
git commit -m "Add a per-project notes store"
```

---

### Task 2: Tauri commands, registration, and cleanup on project removal

**Files:**
- Create: `app/src-tauri/src/commands/notes_commands.rs`
- Modify: `app/src-tauri/src/commands/mod.rs`
- Modify: `app/src-tauri/src/lib.rs` (the `generate_handler![]` list)
- Modify: `app/src-tauri/src/commands/project_commands.rs` (`remove_project`)

**Interfaces:**
- Consumes: `notes_store::{load, upsert, delete, clear}` and `models::Note` from Task 1.
- Produces the IPC surface later tasks call:
  - `list_notes(projectId: string) -> Note[]`
  - `save_note(projectId: string, note: Note) -> Note`
  - `delete_note(projectId: string, noteId: string) -> void`

- [ ] **Step 1: Write the commands**

Create `app/src-tauri/src/commands/notes_commands.rs`:

```rust
use crate::models::Note;
use crate::storage::notes_store;

/// Every project's notes, oldest concept first: pinned notes, then most
/// recently edited.
///
/// Sorted here rather than in the webview so the dock and the tab — two views
/// of the same list — cannot drift into two different orders.
#[tauri::command]
pub async fn list_notes(project_id: String) -> Result<Vec<Note>, String> {
    let mut notes = notes_store::load(&project_id)?;
    notes.sort_by(|a, b| {
        b.pinned
            .cmp(&a.pinned)
            .then_with(|| b.updated_at.cmp(&a.updated_at))
    });
    Ok(notes)
}

/// Insert or replace one note.
///
/// There is deliberately no whole-list setter. A bulk write is exactly the
/// clobbering this store's per-project file exists to avoid, and every caller
/// here is editing one note.
#[tauri::command]
pub async fn save_note(project_id: String, note: Note) -> Result<Note, String> {
    notes_store::upsert(&project_id, note)
}

#[tauri::command]
pub async fn delete_note(project_id: String, note_id: String) -> Result<(), String> {
    notes_store::delete(&project_id, &note_id)
}
```

Add to `app/src-tauri/src/commands/mod.rs`, alphabetically:

```rust
pub mod notes_commands;
```

Note these take no `State<'_, AppState>`: the store is free functions with nothing held in app state, so there is nothing to borrow.

- [ ] **Step 2: Run the registration test to verify it fails**

Run: `cd app/src-tauri && cargo test commands_are_registered`
Expected: FAIL — the `lib.rs:822-862` test reports `["list_notes", "save_note", "delete_note"]` as defined but unregistered. (If the test name differs, `cargo test --lib` surfaces it; the assertion message is "these commands exist but are not registered, so the frontend cannot call them".)

- [ ] **Step 3: Register the commands**

In `app/src-tauri/src/lib.rs`, inside `generate_handler![...]`, after the `// Projects` block:

```rust
            // Notes
            commands::notes_commands::list_notes,
            commands::notes_commands::save_note,
            commands::notes_commands::delete_note,
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cd app/src-tauri && cargo test`
Expected: PASS — including the registration test.

- [ ] **Step 5: Write the failing test for removal cleanup**

Add to the `#[cfg(test)] mod tests` in `app/src-tauri/src/storage/notes_store.rs`:

```rust
    #[test]
    fn clearing_is_what_project_removal_calls_and_it_never_fails_on_absence() {
        // `remove_project` must not be able to fail because a project simply
        // never had any notes — an orphaned notes file is harmless, a project
        // that cannot be removed is not.
        let dir = temp_dir("removal");
        assert!(clear_in(&dir, "never-had-notes").is_ok());
        std::fs::remove_dir_all(&dir).ok();
    }
```

- [ ] **Step 6: Run it**

Run: `cd app/src-tauri && cargo test notes_store`
Expected: PASS (the behaviour already exists from Task 1; this pins it as a contract for the next step).

- [ ] **Step 7: Wire cleanup into `remove_project`**

In `app/src-tauri/src/commands/project_commands.rs`, in `remove_project`, immediately after the `purge_migration_artifacts` call:

```rust
    // A project's notes are the one piece of its state that is purely the
    // user's prose, so removal takes them with it rather than leaving an
    // orphan file keyed by an id nothing will ever look up again. Logged and
    // not propagated: an orphaned notes file is harmless, and a project that
    // cannot be removed is not.
    if let Err(e) = crate::storage::notes_store::clear(&project_id) {
        log::warn!("Could not remove notes for project {}: {}", project_id, e);
    }
```

- [ ] **Step 8: Verify the whole Rust suite still passes**

Run: `cd app/src-tauri && cargo test`
Expected: PASS.

- [ ] **Step 9: Commit**

```bash
git add app/src-tauri/src/commands/notes_commands.rs app/src-tauri/src/commands/mod.rs \
        app/src-tauri/src/lib.rs app/src-tauri/src/commands/project_commands.rs \
        app/src-tauri/src/storage/notes_store.rs
git commit -m "Expose notes over IPC and drop them with the project"
```

---

### Task 3: Frontend types, command wrappers, and the `useNotes` hook

**Files:**
- Modify: `app/src/lib/types.ts`
- Modify: `app/src/lib/tauri-commands.ts`
- Create: `app/src/hooks/useNotes.ts`
- Test: `app/src/hooks/useNotes.test.ts`

**Interfaces:**
- Consumes: the three commands from Task 2.
- Produces:
  - `Note` in `lib/types.ts` — `{ id, title, body, pinned, created_at, updated_at }`, all snake_case to match Rust.
  - `commands.listNotes(projectId)`, `commands.saveNote(projectId, note)`, `commands.deleteNote(projectId, noteId)`.
  - `useNotes(projectId)` returning `{ notes: Note[]; loading: boolean; saveState: SaveState; createNote(): Promise<Note | null>; saveNote(note: Note): Promise<boolean>; deleteNote(id: string): Promise<boolean>; }`.

- [ ] **Step 1: Add the type and wrappers**

In `app/src/lib/types.ts`, beside the other record interfaces:

```ts
/** One project note. Mirrors `models::Note` — field names are the Rust ones. */
export interface Note {
  id: string;
  title: string;
  body: string;
  pinned: boolean;
  created_at: string;
  updated_at: string;
}
```

Add `Note` to the type import list at the top of `app/src/lib/tauri-commands.ts`, then append a wrapper block after the `// Projects` group:

```ts
// Notes — per-project, host-side, readable with the container stopped.
export const listNotes = (projectId: string) =>
  invoke<Note[]>("list_notes", { projectId });
/** Insert or replace one note. `created_at` and `id` are owned by the backend. */
export const saveNote = (projectId: string, note: Note) =>
  invoke<Note>("save_note", { projectId, note });
export const deleteNote = (projectId: string, noteId: string) =>
  invoke<void>("delete_note", { projectId, noteId });
```

- [ ] **Step 2: Write the failing test**

Create `app/src/hooks/useNotes.test.ts`:

```ts
import { describe, it, expect, vi, beforeEach } from "vitest";
import { renderHook, act, waitFor } from "@testing-library/react";
import { useNotes } from "./useNotes";
import type { Note } from "../lib/types";

const listNotes = vi.fn();
const saveNote = vi.fn();
const deleteNote = vi.fn();

vi.mock("../lib/tauri-commands", () => ({
  listNotes: (p: string) => listNotes(p),
  saveNote: (p: string, n: Note) => saveNote(p, n),
  deleteNote: (p: string, id: string) => deleteNote(p, id),
}));

const pushToast = vi.fn();
vi.mock("../store/appState", () => ({
  useAppState: Object.assign(
    (selector: (s: unknown) => unknown) => selector({ pushToast }),
    { getState: () => ({ pushToast }) },
  ),
}));

const note = (over: Partial<Note> = {}): Note => ({
  id: "n1",
  title: "Deploy",
  body: "one\ntwo",
  pinned: false,
  created_at: "2026-09-01T00:00:00Z",
  updated_at: "2026-09-01T00:00:00Z",
  ...over,
});

beforeEach(() => {
  vi.clearAllMocks();
  listNotes.mockResolvedValue([note()]);
  saveNote.mockImplementation(async (_p: string, n: Note) => n);
  deleteNote.mockResolvedValue(undefined);
});

describe("useNotes", () => {
  it("loads a project's notes on mount", async () => {
    const { result } = renderHook(() => useNotes("p1"));
    await waitFor(() => expect(result.current.loading).toBe(false));
    expect(listNotes).toHaveBeenCalledWith("p1");
    expect(result.current.notes).toHaveLength(1);
  });

  it("reports a failed save instead of swallowing it", async () => {
    // Silent save failure is data loss: the user sees their text on screen and
    // believes it is stored. Same reason `useSaveState` exists.
    saveNote.mockRejectedValueOnce(new Error("disk full"));
    const { result } = renderHook(() => useNotes("p1"));
    await waitFor(() => expect(result.current.loading).toBe(false));

    let ok: boolean | undefined;
    await act(async () => {
      ok = await result.current.saveNote(note({ body: "edited" }));
    });

    expect(ok).toBe(false);
    expect(result.current.saveState.status).toBe("failed");
    expect(pushToast).toHaveBeenCalled();
  });

  it("replaces the saved note in place rather than appending", async () => {
    const { result } = renderHook(() => useNotes("p1"));
    await waitFor(() => expect(result.current.loading).toBe(false));

    await act(async () => {
      await result.current.saveNote(note({ body: "edited" }));
    });

    expect(result.current.notes).toHaveLength(1);
    expect(result.current.notes[0].body).toBe("edited");
  });

  it("drops a deleted note from the list", async () => {
    const { result } = renderHook(() => useNotes("p1"));
    await waitFor(() => expect(result.current.loading).toBe(false));

    await act(async () => {
      await result.current.deleteNote("n1");
    });

    expect(deleteNote).toHaveBeenCalledWith("p1", "n1");
    expect(result.current.notes).toHaveLength(0);
  });

  it("does not load anything for an empty project id", async () => {
    // The dock renders with no project selected; it must not fire a command
    // for the empty string.
    renderHook(() => useNotes(""));
    await waitFor(() => expect(listNotes).not.toHaveBeenCalled());
  });
});
```

- [ ] **Step 3: Run it to verify it fails**

Run: `cd app && npx vitest run src/hooks/useNotes.test.ts`
Expected: FAIL — `Failed to resolve import "./useNotes"`.

- [ ] **Step 4: Write the hook**

Create `app/src/hooks/useNotes.ts`:

```ts
import { useCallback, useEffect, useRef, useState } from "react";
import * as commands from "../lib/tauri-commands";
import type { Note } from "../lib/types";
import type { SaveState } from "./useSaveState";
import { useAppState } from "../store/appState";

/** A blank note, ordered to the top so the user can start typing immediately. */
function draft(): Note {
  const now = new Date().toISOString();
  return {
    // The backend owns the real id; this one only has to be unique enough to
    // key the list until the first save returns.
    id: crypto.randomUUID(),
    title: "",
    body: "",
    pinned: false,
    created_at: now,
    updated_at: now,
  };
}

/**
 * A project's notes, cached from the backend.
 *
 * The backend is the source of truth and this is a cache — every mutation goes
 * through a command and the returned record replaces the local one, so the
 * list can never drift from the file. `saveState` mirrors `useProjectSave` so
 * `ui/SaveIndicator` can report the outcome: a save that fails silently is a
 * user staring at text they believe is stored.
 */
export function useNotes(projectId: string) {
  const [notes, setNotes] = useState<Note[]>([]);
  const [loading, setLoading] = useState(true);
  const [saveState, setSaveState] = useState<SaveState>({ status: "idle", error: null });
  const pushToast = useAppState((s) => s.pushToast);
  const resetTimer = useRef<ReturnType<typeof setTimeout> | null>(null);

  useEffect(() => {
    if (!projectId) {
      setNotes([]);
      setLoading(false);
      return;
    }
    let cancelled = false;
    setLoading(true);
    commands
      .listNotes(projectId)
      .then((loaded) => {
        if (!cancelled) setNotes(loaded);
      })
      .catch((e) => {
        if (cancelled) return;
        pushToast({
          kind: "error",
          message: "Could not load notes for this project",
          detail: String(e),
        });
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [projectId, pushToast]);

  useEffect(
    () => () => {
      if (resetTimer.current) clearTimeout(resetTimer.current);
    },
    [],
  );

  const succeeded = useCallback(() => {
    setSaveState({ status: "saved", error: null });
    if (resetTimer.current) clearTimeout(resetTimer.current);
    resetTimer.current = setTimeout(
      () => setSaveState({ status: "idle", error: null }),
      2500,
    );
  }, []);

  const saveNote = useCallback(
    async (note: Note) => {
      if (resetTimer.current) clearTimeout(resetTimer.current);
      setSaveState({ status: "saving", error: null });
      try {
        const saved = await commands.saveNote(projectId, note);
        setNotes((current) => {
          const index = current.findIndex((n) => n.id === saved.id);
          if (index === -1) return [saved, ...current];
          const next = [...current];
          next[index] = saved;
          return next;
        });
        succeeded();
        return true;
      } catch (e) {
        const message = String(e);
        setSaveState({ status: "failed", error: message });
        pushToast({ kind: "error", message: "Could not save note", detail: message });
        return false;
      }
    },
    [projectId, pushToast, succeeded],
  );

  const createNote = useCallback(async () => {
    const note = draft();
    // Held locally first so the editor can focus it immediately; the save
    // happens on blur like every other edit.
    setNotes((current) => [note, ...current]);
    return note;
  }, []);

  const deleteNote = useCallback(
    async (noteId: string) => {
      try {
        await commands.deleteNote(projectId, noteId);
        setNotes((current) => current.filter((n) => n.id !== noteId));
        return true;
      } catch (e) {
        pushToast({ kind: "error", message: "Could not delete note", detail: String(e) });
        return false;
      }
    },
    [projectId, pushToast],
  );

  return { notes, loading, saveState, createNote, saveNote, deleteNote };
}
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cd app && npx vitest run src/hooks/useNotes.test.ts`
Expected: PASS — 5 tests.

- [ ] **Step 6: Commit**

```bash
git add app/src/lib/types.ts app/src/lib/tauri-commands.ts \
        app/src/hooks/useNotes.ts app/src/hooks/useNotes.test.ts
git commit -m "Add the notes hook and its IPC wrappers"
```

---

### Task 4: Shared helpers — the Claude newline rule and the session display name

Both of these already exist in the codebase as knowledge that is either about to be
duplicated or already is. This task puts each in one place before anything else depends on
it.

**Files:**
- Create: `app/src/lib/claudeInput.ts`
- Create: `app/src/lib/claudeInput.test.ts`
- Create: `app/src/lib/sessionName.ts`
- Create: `app/src/lib/sessionName.test.ts`
- Modify: `app/src/components/terminal/TerminalView.tsx` (use the constant)
- Modify: `app/src/components/layout/MainTabs.tsx` (call the helper from both sites)

**Interfaces:**
- Consumes: `TerminalSession` and `Project` from `lib/types.ts`.
- Produces:
  - `CLAUDE_SOFT_NEWLINE: "\x1b\r"`
  - `toClaudePayload(text: string): string`
  - `sessionDisplayName(session: TerminalSession, project?: Project): string`

- [ ] **Step 1: Write the failing tests**

Create `app/src/lib/claudeInput.test.ts`:

```ts
import { describe, it, expect } from "vitest";
import { CLAUDE_SOFT_NEWLINE, toClaudePayload } from "./claudeInput";

describe("toClaudePayload", () => {
  it("is ESC+CR, the sequence Claude Code's own /terminal-setup installs", () => {
    expect(CLAUDE_SOFT_NEWLINE).toBe("\x1b\r");
  });

  it("replaces every newline so the note arrives as one prompt", () => {
    // Typed raw, each \n submits — the note would arrive as three truncated
    // messages instead of one.
    expect(toClaudePayload("one\ntwo\nthree")).toBe("one\x1b\rtwo\x1b\rthree");
  });

  it("normalises CRLF, which is what a paste from Windows carries", () => {
    expect(toClaudePayload("one\r\ntwo")).toBe("one\x1b\rtwo");
  });

  it("leaves single-line text untouched", () => {
    expect(toClaudePayload("just one line")).toBe("just one line");
  });

  it("never appends a terminator", () => {
    // The note lands in the prompt unsubmitted; the user presses Enter. An
    // unsent prompt is recoverable, a sent one is not.
    expect(toClaudePayload("text").endsWith("\r")).toBe(false);
    expect(toClaudePayload("text\n")).toBe("text\x1b\r");
  });
});
```

Create `app/src/lib/sessionName.test.ts`:

```ts
import { describe, it, expect } from "vitest";
import { sessionDisplayName } from "./sessionName";
import type { Project, TerminalSession } from "./types";

const session = (over: Partial<TerminalSession> = {}): TerminalSession => ({
  id: "s1",
  projectId: "p1",
  projectName: "api",
  sessionType: "claude",
  sessionName: null,
  ...over,
});

const project = (renamed: Record<string, string> = {}) =>
  ({ id: "p1", name: "api", renamed_session_names: renamed }) as unknown as Project;

describe("sessionDisplayName", () => {
  it("prefers a user-set custom name, prefixed with the project", () => {
    expect(sessionDisplayName(session(), project({ s1: "release work" }))).toBe(
      "api: release work",
    );
  });

  it("falls back to the session name when there is no custom one", () => {
    expect(sessionDisplayName(session({ sessionName: "review" }), project())).toBe("review");
  });

  it("falls back to the project name when there is no session name", () => {
    expect(sessionDisplayName(session(), project())).toBe("api");
  });

  it("marks bash sessions", () => {
    expect(sessionDisplayName(session({ sessionType: "bash" }), project())).toBe("api (bash)");
  });

  it("works with no project, which is how a closing tab renders", () => {
    expect(sessionDisplayName(session())).toBe("api");
  });

  it("does not mark bash when a custom name is set, matching the existing rule", () => {
    expect(
      sessionDisplayName(session({ sessionType: "bash" }), project({ s1: "logs" })),
    ).toBe("api: logs");
  });
});
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cd app && npx vitest run src/lib/claudeInput.test.ts src/lib/sessionName.test.ts`
Expected: FAIL — both modules unresolved.

- [ ] **Step 3: Write the helpers**

Create `app/src/lib/claudeInput.ts`:

```ts
/**
 * The bytes that insert a newline in Claude Code's prompt without submitting
 * it: ESC then CR.
 *
 * These are the in-band bytes, not a guess — they are exactly what Claude
 * Code's own `/terminal-setup` writes into the VS Code, Cursor, Alacritty and
 * Zed keymaps, and `TerminalView`'s Shift+Enter handler has sent them since
 * that feature landed. **This must not be "simplified" to `\n`:** Claude Code
 * accepts `\n` too, but a shell would *run* the line, so the two session types
 * would quietly diverge.
 *
 * That last sentence is also why anything sending this must first check the
 * session is a Claude one. `bash -l`'s readline has no binding for `\e\r` and
 * answers with a bell.
 */
export const CLAUDE_SOFT_NEWLINE = "\x1b\r";

/**
 * Turn multi-line text into something that arrives in a Claude prompt as one
 * message.
 *
 * Sent as raw keystrokes, every `\n` submits, so an N-line note would arrive
 * as N truncated prompts. Deliberately appends no terminator: the text lands
 * in the prompt and the user presses Enter, which is what speech-to-text does
 * for the same reason — an unsent prompt is recoverable and a sent one is not.
 */
export function toClaudePayload(text: string): string {
  return text.replace(/\r?\n/g, CLAUDE_SOFT_NEWLINE);
}
```

Create `app/src/lib/sessionName.ts`:

```ts
import type { Project, TerminalSession } from "./types";

/**
 * What a terminal session is called on screen.
 *
 * The rule used to be written twice inside `MainTabs.tsx` — once in `tabLabel`
 * for the drag ghost, once inline in `renderTab` — both local and neither
 * exported, so the two could disagree the moment either was edited. It is here
 * because a third caller (the note send-target picker) would have made that
 * three.
 *
 * A user-set name wins and is prefixed with the project, because a custom name
 * is usually about the work rather than the project and needs the context. The
 * `(bash)` marker only appears on the fallback: a session someone bothered to
 * name does not need to be told apart from its neighbours.
 */
export function sessionDisplayName(
  session: TerminalSession,
  project?: Project,
): string {
  const custom = project?.renamed_session_names?.[session.id];
  if (custom) return `${session.projectName}: ${custom}`;
  return (
    (session.sessionName ?? session.projectName) +
    (session.sessionType === "bash" ? " (bash)" : "")
  );
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cd app && npx vitest run src/lib/claudeInput.test.ts src/lib/sessionName.test.ts`
Expected: PASS — 11 tests.

- [ ] **Step 5: Rewire the three existing call sites**

In `app/src/components/terminal/TerminalView.tsx`, add the import:

```ts
import { CLAUDE_SOFT_NEWLINE } from "../../lib/claudeInput";
```

and in the Shift+Enter branch (~line 418) replace the literal, leaving every surrounding
comment exactly as it is — that comment is the reason the constant exists:

```ts
          sendInput(sessionId, CLAUDE_SOFT_NEWLINE);
```

In `app/src/components/layout/MainTabs.tsx`, add:

```ts
import { sessionDisplayName } from "../../lib/sessionName";
```

Replace the body of `tabLabel` (~lines 192-203) with:

```tsx
  const tabLabel = (key: string): string => {
    if (isHomeTab(key)) {
      return projects.find((p) => p.id === tabKeyId(key))?.name ?? "";
    }
    const session = sessions.find((s) => s.id === tabKeyId(key));
    if (!session) return "";
    return sessionDisplayName(
      session,
      projects.find((p) => p.id === session.projectId),
    );
  };
```

and in `renderTab` (~lines 361-367) replace the `customName` / `baseLabel` / `displayLabel`
trio with:

```tsx
    const customName = getCustomName(session.projectId, session.id);
    const displayLabel = sessionDisplayName(session, project);
```

`customName` is still needed — the tab's context menu uses it to decide whether to offer
"Reset name" — so only the two label locals collapse.

- [ ] **Step 6: Verify nothing regressed**

Run: `cd app && npx vitest run`
Expected: PASS — the whole frontend suite, including any existing `MainTabs` tests.

- [ ] **Step 7: Commit**

```bash
git add app/src/lib/claudeInput.ts app/src/lib/claudeInput.test.ts \
        app/src/lib/sessionName.ts app/src/lib/sessionName.test.ts \
        app/src/components/terminal/TerminalView.tsx \
        app/src/components/layout/MainTabs.tsx
git commit -m "Extract the Claude newline sequence and the session display name"
```

---

### Task 5: The Send to agent button

**Files:**
- Create: `app/src/components/notes/SendToAgentButton.tsx`
- Create: `app/src/components/notes/SendToAgentButton.test.tsx`

**Interfaces:**
- Consumes: `toClaudePayload` and `sessionDisplayName` (Task 4); `useTerminal().sendInput` and `.sessions`; `useAppState`'s `projects`, `setActiveTabKey`, `terminalTabKey`, `pushToast`.
- Produces: `<SendToAgentButton projectId={string} body={string} />`.

`useTerminal()` is a plain hook callable from anywhere — its ordering queue is module scope
precisely so several components can share it — so this component needs no props beyond its
own data and no store additions.

- [ ] **Step 1: Write the failing test**

Create `app/src/components/notes/SendToAgentButton.test.tsx`:

```tsx
import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, fireEvent, waitFor } from "@testing-library/react";
import SendToAgentButton from "./SendToAgentButton";
import type { Project, TerminalSession } from "../../lib/types";

const sendInput = vi.fn(async () => {});
let sessions: TerminalSession[] = [];

vi.mock("../../hooks/useTerminal", () => ({
  useTerminal: () => ({ sessions, sendInput }),
}));

const setActiveTabKey = vi.fn();
const pushToast = vi.fn();
let projects: Project[] = [];

vi.mock("../../store/appState", () => ({
  useAppState: Object.assign(
    (selector: (s: unknown) => unknown) =>
      selector({ projects, setActiveTabKey, pushToast }),
    { getState: () => ({ projects, setActiveTabKey, pushToast }) },
  ),
  terminalTabKey: (id: string) => `term:${id}`,
}));

const session = (over: Partial<TerminalSession> = {}): TerminalSession => ({
  id: "s1",
  projectId: "p1",
  projectName: "api",
  sessionType: "claude",
  sessionName: null,
  ...over,
});

beforeEach(() => {
  vi.clearAllMocks();
  sessions = [];
  projects = [{ id: "p1", name: "api", renamed_session_names: {} } as unknown as Project];
});

describe("SendToAgentButton", () => {
  it("is disabled when the project has no running session", () => {
    render(<SendToAgentButton projectId="p1" body="hello" />);
    expect(screen.getByRole("button", { name: /send to agent/i })).toBeDisabled();
  });

  it("is disabled when the only session belongs to another project", () => {
    sessions = [session({ projectId: "other" })];
    render(<SendToAgentButton projectId="p1" body="hello" />);
    expect(screen.getByRole("button", { name: /send to agent/i })).toBeDisabled();
  });

  it("is disabled when the only session is a bash tab", () => {
    // `bash -l`'s readline has no binding for ESC+CR and just bells, so a
    // shell is never a target.
    sessions = [session({ sessionType: "bash" })];
    render(<SendToAgentButton projectId="p1" body="hello" />);
    expect(screen.getByRole("button", { name: /send to agent/i })).toBeDisabled();
  });

  it("sends straight to the one session, with newlines converted and no terminator", async () => {
    sessions = [session()];
    render(<SendToAgentButton projectId="p1" body={"one\ntwo"} />);

    fireEvent.click(screen.getByRole("button", { name: /send to agent/i }));

    await waitFor(() => expect(sendInput).toHaveBeenCalledWith("s1", "one\x1b\rtwo"));
    expect(sendInput.mock.calls[0][1].endsWith("\r")).toBe(false);
  });

  it("focuses the terminal it sent to, so the user watches it land", async () => {
    sessions = [session()];
    render(<SendToAgentButton projectId="p1" body="hi" />);
    fireEvent.click(screen.getByRole("button", { name: /send to agent/i }));
    await waitFor(() => expect(setActiveTabKey).toHaveBeenCalledWith("term:s1"));
  });

  it("offers a menu of display names when several sessions are open", async () => {
    sessions = [session(), session({ id: "s2", sessionName: "review" })];
    projects = [
      { id: "p1", name: "api", renamed_session_names: { s1: "release" } } as unknown as Project,
    ];
    render(<SendToAgentButton projectId="p1" body="hi" />);

    fireEvent.click(screen.getByRole("button", { name: /send to agent/i }));
    expect(sendInput).not.toHaveBeenCalled();

    fireEvent.click(await screen.findByRole("menuitem", { name: "api: release" }));
    await waitFor(() => expect(sendInput).toHaveBeenCalledWith("s1", "hi"));
  });

  it("reports a failed send rather than looking like it worked", async () => {
    sessions = [session()];
    sendInput.mockRejectedValueOnce(new Error("session closed"));
    render(<SendToAgentButton projectId="p1" body="hi" />);
    fireEvent.click(screen.getByRole("button", { name: /send to agent/i }));
    await waitFor(() => expect(pushToast).toHaveBeenCalled());
  });

  it("does nothing for an empty note", () => {
    sessions = [session()];
    render(<SendToAgentButton projectId="p1" body="   " />);
    expect(screen.getByRole("button", { name: /send to agent/i })).toBeDisabled();
  });
});
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cd app && npx vitest run src/components/notes/SendToAgentButton.test.tsx`
Expected: FAIL — `Failed to resolve import "./SendToAgentButton"`.

- [ ] **Step 3: Write the component**

Create `app/src/components/notes/SendToAgentButton.tsx`:

```tsx
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useShallow } from "zustand/react/shallow";
import { useTerminal } from "../../hooks/useTerminal";
import { useAppState, terminalTabKey } from "../../store/appState";
import { toClaudePayload } from "../../lib/claudeInput";
import { sessionDisplayName } from "../../lib/sessionName";
import Button from "../ui/Button";

interface Props {
  projectId: string;
  body: string;
}

/**
 * Puts a note into a running Claude session's prompt.
 *
 * Three behaviours by target count: none disables the button, one sends
 * straight there, several ask which. It never guesses — the note goes to a
 * session the user named, or to the only one there is.
 *
 * Only `claude` sessions are offered. A bash tab would receive ESC+CR as an
 * unbound readline key and answer with a bell (see `lib/claudeInput.ts`).
 */
export default function SendToAgentButton({ projectId, body }: Props) {
  const { sessions, sendInput } = useTerminal();
  const { projects, setActiveTabKey, pushToast } = useAppState(
    useShallow((s) => ({
      projects: s.projects,
      setActiveTabKey: s.setActiveTabKey,
      pushToast: s.pushToast,
    })),
  );
  const [menuOpen, setMenuOpen] = useState(false);
  const rootRef = useRef<HTMLDivElement>(null);

  const targets = useMemo(
    () =>
      sessions.filter(
        (s) => s.projectId === projectId && s.sessionType === "claude",
      ),
    [sessions, projectId],
  );

  const project = projects.find((p) => p.id === projectId);
  const hasBody = body.trim().length > 0;
  const disabled = targets.length === 0 || !hasBody;

  // Same dismissal contract as `ui/OverflowMenu` and the tab context menu.
  useEffect(() => {
    if (!menuOpen) return;
    const onDocClick = (e: MouseEvent) => {
      if (!rootRef.current?.contains(e.target as Node)) setMenuOpen(false);
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") setMenuOpen(false);
    };
    document.addEventListener("mousedown", onDocClick);
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("mousedown", onDocClick);
      document.removeEventListener("keydown", onKey);
    };
  }, [menuOpen]);

  const send = useCallback(
    async (sessionId: string) => {
      setMenuOpen(false);
      try {
        // No trailing CR: the note lands in the prompt and the user presses
        // Enter. Newlines become ESC+CR so it arrives as one message rather
        // than one prompt per line.
        await sendInput(sessionId, toClaudePayload(body));
        // A courtesy, not part of the send: if the tab cannot be focused the
        // text still went.
        setActiveTabKey(terminalTabKey(sessionId));
      } catch (e) {
        pushToast({
          kind: "error",
          message: "Could not send the note to the agent",
          detail: String(e),
        });
      }
    },
    [body, sendInput, setActiveTabKey, pushToast],
  );

  const onClick = useCallback(() => {
    // The target is resolved at click time and pinned for the whole send, the
    // hazard `useSTT` guards against by capturing its session at record start:
    // the list can change while the request is in flight.
    if (targets.length === 1) {
      void send(targets[0].id);
      return;
    }
    setMenuOpen((open) => !open);
  }, [targets, send]);

  const title = !hasBody
    ? "Nothing to send — this note is empty"
    : targets.length === 0
      ? "No running Claude session for this project"
      : "Put this note into the agent's prompt (you press Enter)";

  return (
    <div ref={rootRef} className="relative inline-block">
      <Button
        variant="secondary"
        disabled={disabled}
        onClick={onClick}
        aria-haspopup={targets.length > 1 ? "menu" : undefined}
        aria-expanded={targets.length > 1 ? menuOpen : undefined}
        title={title}
      >
        Send to agent
      </Button>
      {menuOpen && targets.length > 1 && (
        <div
          role="menu"
          className="absolute right-0 z-40 mt-1 min-w-[12rem] py-1 bg-[var(--bg-overlay)] border border-[var(--border-color)] rounded-[var(--radius-panel)] text-xs"
          style={{ boxShadow: "var(--shadow-overlay)" }}
        >
          {targets.map((s) => (
            <button
              key={s.id}
              type="button"
              role="menuitem"
              onClick={() => void send(s.id)}
              className="w-full text-left px-3 py-1.5 text-[var(--text-primary)] hover:bg-[var(--bg-tertiary)] transition-colors"
            >
              {sessionDisplayName(s, project)}
            </button>
          ))}
        </div>
      )}
    </div>
  );
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cd app && npx vitest run src/components/notes/SendToAgentButton.test.tsx`
Expected: PASS — 8 tests.

- [ ] **Step 5: Commit**

```bash
git add app/src/components/notes/SendToAgentButton.tsx \
        app/src/components/notes/SendToAgentButton.test.tsx
git commit -m "Add the send-to-agent button"
```

---

### Task 6: The note editor and the Notes tab

**Files:**
- Create: `app/src/components/notes/NoteEditor.tsx`
- Create: `app/src/components/notes/NotesPanel.tsx`
- Create: `app/src/components/notes/NotesPanel.test.tsx`
- Create: `app/src/components/projects/home/NotesTab.tsx`
- Modify: `app/src/components/projects/home/ProjectHome.tsx`

`NotesPanel` holds the list, the editor and the empty state, and is shared verbatim by the
tab and by the dock in Task 7 — the two surfaces must not drift into two behaviours.

**Interfaces:**
- Consumes: `useNotes` (Task 3), `SendToAgentButton` (Task 5), `ui/SaveIndicator`, `ui/Button`.
- Produces:
  - `<NotesPanel projectId={string} />`
  - `<NotesTab project={Project} />`
  - `"notes"` added to `ProjectHomeTabId`.

- [ ] **Step 1: Write the failing test**

Create `app/src/components/notes/NotesPanel.test.tsx`:

```tsx
import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, fireEvent, waitFor } from "@testing-library/react";
import NotesPanel from "./NotesPanel";
import type { Note } from "../../lib/types";

const saveNote = vi.fn(async () => true);
const deleteNote = vi.fn(async () => true);
const createNote = vi.fn();
let notes: Note[] = [];
let loading = false;

vi.mock("../../hooks/useNotes", () => ({
  useNotes: () => ({
    notes,
    loading,
    saveState: { status: "idle", error: null },
    createNote,
    saveNote,
    deleteNote,
  }),
}));

vi.mock("./SendToAgentButton", () => ({
  default: ({ body }: { body: string }) => (
    <button type="button" data-testid="send">{`send:${body}`}</button>
  ),
}));

const note = (over: Partial<Note> = {}): Note => ({
  id: "n1",
  title: "Deploy steps",
  body: "one\ntwo",
  pinned: false,
  created_at: "2026-09-01T00:00:00Z",
  updated_at: "2026-09-01T00:00:00Z",
  ...over,
});

beforeEach(() => {
  vi.clearAllMocks();
  notes = [];
  loading = false;
});

describe("NotesPanel", () => {
  it("invites the user to start when there are no notes", () => {
    render(<NotesPanel projectId="p1" />);
    expect(screen.getByText(/no notes yet/i)).toBeInTheDocument();
  });

  it("lists notes by title and selects the first", () => {
    notes = [note(), note({ id: "n2", title: "Gotchas" })];
    render(<NotesPanel projectId="p1" />);
    expect(screen.getByRole("button", { name: /deploy steps/i })).toBeInTheDocument();
    expect(screen.getByLabelText("Note body")).toHaveValue("one\ntwo");
  });

  it("shows an untitled note under a placeholder rather than a blank row", () => {
    notes = [note({ title: "" })];
    render(<NotesPanel projectId="p1" />);
    expect(screen.getByRole("button", { name: /untitled note/i })).toBeInTheDocument();
  });

  it("switches the editor when another note is selected", () => {
    notes = [note(), note({ id: "n2", title: "Gotchas", body: "beware" })];
    render(<NotesPanel projectId="p1" />);
    fireEvent.click(screen.getByRole("button", { name: /gotchas/i }));
    expect(screen.getByLabelText("Note body")).toHaveValue("beware");
  });

  it("saves on blur, not on every keystroke", async () => {
    notes = [note()];
    render(<NotesPanel projectId="p1" />);
    const body = screen.getByLabelText("Note body");

    fireEvent.change(body, { target: { value: "edited" } });
    expect(saveNote).not.toHaveBeenCalled();

    fireEvent.blur(body);
    await waitFor(() => expect(saveNote).toHaveBeenCalledWith(
      expect.objectContaining({ id: "n1", body: "edited" }),
    ));
  });

  it("does not save on blur when nothing changed", async () => {
    // Clicking through notes to read them must not write the file.
    notes = [note()];
    render(<NotesPanel projectId="p1" />);
    fireEvent.blur(screen.getByLabelText("Note body"));
    await waitFor(() => expect(saveNote).not.toHaveBeenCalled());
  });

  it("hands the live editor text to the send button, not the last saved copy", () => {
    // Sending what is on screen is the whole contract: no transform on the way
    // out except the newline substitution.
    notes = [note()];
    render(<NotesPanel projectId="p1" />);
    fireEvent.change(screen.getByLabelText("Note body"), { target: { value: "fresh" } });
    expect(screen.getByTestId("send")).toHaveTextContent("send:fresh");
  });

  it("deletes the selected note and falls back to another", async () => {
    notes = [note(), note({ id: "n2", title: "Gotchas" })];
    render(<NotesPanel projectId="p1" />);
    fireEvent.click(screen.getByRole("button", { name: /delete note/i }));
    await waitFor(() => expect(deleteNote).toHaveBeenCalledWith("n1"));
  });
});
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cd app && npx vitest run src/components/notes/NotesPanel.test.tsx`
Expected: FAIL — `Failed to resolve import "./NotesPanel"`.

- [ ] **Step 3: Write the editor**

Create `app/src/components/notes/NoteEditor.tsx`:

```tsx
import type { Note } from "../../lib/types";
import SendToAgentButton from "./SendToAgentButton";
import Button from "../ui/Button";

interface Props {
  projectId: string;
  note: Note;
  title: string;
  body: string;
  onTitleChange: (value: string) => void;
  onBodyChange: (value: string) => void;
  onCommit: () => void;
  onDelete: () => void;
}

/**
 * Title and body, saved when a field loses focus.
 *
 * Plain text on purpose. There is no markdown rendering and no view/edit split,
 * so there is no moment where the text on screen is not the text that would be
 * sent — which is what makes "the agent gets exactly what you see" true rather
 * than nearly true.
 */
export default function NoteEditor({
  projectId,
  note,
  title,
  body,
  onTitleChange,
  onBodyChange,
  onCommit,
  onDelete,
}: Props) {
  return (
    <div className="flex flex-col h-full min-h-0 gap-2 p-3">
      <div className="flex items-center gap-2">
        <input
          value={title}
          onChange={(e) => onTitleChange(e.target.value)}
          onBlur={onCommit}
          placeholder="Note title"
          aria-label="Note title"
          className="flex-1 min-w-0 px-2 h-8 bg-[var(--bg-primary)] border border-[var(--border-color)] rounded-[var(--radius-control)] text-[13px] text-[var(--text-primary)] focus:border-[var(--accent)] transition-colors"
        />
        {/* The live editor text, not `note.body` — what is on screen is what
            gets sent. */}
        <SendToAgentButton projectId={projectId} body={body} />
        <Button variant="danger" onClick={onDelete} aria-label={`Delete note ${title || "untitled"}`}>
          Delete
        </Button>
      </div>
      <textarea
        value={body}
        onChange={(e) => onBodyChange(e.target.value)}
        onBlur={onCommit}
        placeholder="Reminders, gotchas, a prompt worth keeping…"
        aria-label="Note body"
        className="flex-1 min-h-0 w-full px-3 py-2 bg-[var(--bg-primary)] border border-[var(--border-color)] rounded-[var(--radius-control)] text-[13px] text-[var(--text-primary)] focus:border-[var(--accent)] resize-none font-mono transition-colors"
      />
      <p className="text-xs text-[var(--text-secondary)]">
        Notes save when a field loses focus. Sending puts the note in the agent&rsquo;s
        prompt — you press Enter.
      </p>
    </div>
  );
}
```

- [ ] **Step 4: Write the panel**

Create `app/src/components/notes/NotesPanel.tsx`:

```tsx
import { useEffect, useMemo, useState } from "react";
import { useNotes } from "../../hooks/useNotes";
import NoteEditor from "./NoteEditor";
import Button from "../ui/Button";
import SaveIndicator from "../ui/SaveIndicator";

interface Props {
  projectId: string;
}

const UNTITLED = "Untitled note";

/**
 * The notes surface itself, shared by the Project Home tab and the dock so the
 * two cannot drift into different behaviour.
 *
 * Master/detail: titles on the left, one editor on the right. The editor holds
 * draft text locally and commits on blur, which is how every other editable
 * field in the app behaves (`ClaudeInstructionsEditor`, the Config tab).
 */
export default function NotesPanel({ projectId }: Props) {
  const { notes, loading, saveState, createNote, saveNote, deleteNote } =
    useNotes(projectId);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [title, setTitle] = useState("");
  const [body, setBody] = useState("");

  const selected = useMemo(
    () => notes.find((n) => n.id === selectedId) ?? notes[0] ?? null,
    [notes, selectedId],
  );

  // Load the selected note's stored text into the draft. Keyed on the id, not
  // the note object, so a save round trip does not stomp what is being typed.
  useEffect(() => {
    if (!selected) {
      setTitle("");
      setBody("");
      return;
    }
    setTitle(selected.title);
    setBody(selected.body);
  }, [selected?.id]); // eslint-disable-line react-hooks/exhaustive-deps

  const commit = () => {
    if (!selected) return;
    // Reading is not editing: clicking through notes must not rewrite the file.
    if (title === selected.title && body === selected.body) return;
    void saveNote({ ...selected, title, body });
  };

  const onCreate = async () => {
    const note = await createNote();
    if (note) setSelectedId(note.id);
  };

  if (loading) {
    return (
      <p className="p-4 text-xs text-[var(--text-secondary)]">Loading notes…</p>
    );
  }

  return (
    <div className="flex flex-col h-full min-h-0">
      <div className="flex items-center justify-between gap-2 px-3 py-2 border-b border-[var(--border-color)]">
        <Button variant="primary" onClick={onCreate}>
          New note
        </Button>
        <SaveIndicator state={saveState} />
      </div>

      {notes.length === 0 ? (
        <div className="flex-1 flex items-center justify-center p-4">
          <p className="text-[13px] text-[var(--text-secondary)] text-center">
            No notes yet. Keep reminders here, and send any of them straight to a
            running Claude session.
          </p>
        </div>
      ) : (
        <div className="flex-1 min-h-0 flex">
          <ul className="w-48 flex-shrink-0 overflow-y-auto border-r border-[var(--border-color)] py-1">
            {notes.map((n) => (
              <li key={n.id}>
                <button
                  type="button"
                  onClick={() => setSelectedId(n.id)}
                  className={`w-full text-left px-3 py-1.5 text-xs truncate transition-colors ${
                    selected?.id === n.id
                      ? "bg-[var(--bg-tertiary)] text-[var(--text-primary)]"
                      : "text-[var(--text-secondary)] hover:text-[var(--text-primary)]"
                  }`}
                >
                  {n.title.trim() || UNTITLED}
                </button>
              </li>
            ))}
          </ul>
          <div className="flex-1 min-w-0">
            {selected && (
              <NoteEditor
                projectId={projectId}
                note={selected}
                title={title}
                body={body}
                onTitleChange={setTitle}
                onBodyChange={setBody}
                onCommit={commit}
                onDelete={() => void deleteNote(selected.id)}
              />
            )}
          </div>
        </div>
      )}
    </div>
  );
}
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cd app && npx vitest run src/components/notes/NotesPanel.test.tsx`
Expected: PASS — 8 tests.

- [ ] **Step 6: Add the tab**

Create `app/src/components/projects/home/NotesTab.tsx`:

```tsx
import type { Project } from "../../../lib/types";
import NotesPanel from "../../notes/NotesPanel";

interface Props {
  project: Project;
}

/**
 * Notes as a Project Home sub-tab.
 *
 * The same panel the dock shows. This is the roomy view for writing; the dock
 * is the one that stays visible while the agent works.
 */
export default function NotesTab({ project }: Props) {
  return (
    <div className="h-full min-h-0">
      <NotesPanel projectId={project.id} />
    </div>
  );
}
```

In `app/src/components/projects/home/ProjectHome.tsx`, import it beside the other tabs:

```tsx
import NotesTab from "./NotesTab";
```

Add the entry **last** in `TABS` (a companion to the work, not a step in it):

```tsx
  { id: "browser", label: "Browser" },
  { id: "notes", label: "Notes" },
] as const;
```

And add the panel branch after the `browser` one:

```tsx
        {tab === "notes" && <NotesTab project={project} />}
```

`ProjectHomeTabId` is derived from `TABS` with `as const`, so the union extends itself and
`openProjectHomeTab(projectId, "notes")` works from anywhere with no store change.

- [ ] **Step 7: Verify the suite**

Run: `cd app && npx vitest run && npx tsc --noEmit`
Expected: PASS, and no type errors.

- [ ] **Step 8: Commit**

```bash
git add app/src/components/notes/ app/src/components/projects/home/NotesTab.tsx \
        app/src/components/projects/home/ProjectHome.tsx
git commit -m "Add the Notes tab"
```

---

### Task 7: The dock

**Files:**
- Modify: `app/src/store/appState.ts`
- Create: `app/src/components/layout/NotesDock.tsx`
- Create: `app/src/components/layout/NotesDock.test.tsx`
- Modify: `app/src/App.tsx`
- Modify: `app/src/components/layout/StatusBar.tsx`

**Interfaces:**
- Consumes: `NotesPanel` (Task 6), the store's `activeTabKey` / `sessions` / `projects`.
- Produces: store fields `notesDockOpen: boolean`, `setNotesDockOpen(open: boolean)`, `toggleNotesDock()`, `notesDockWidth: number`, `setNotesDockWidth(width: number)`; the exported `clampDockWidth(value: number): number` plus `NOTES_DOCK_MIN_WIDTH` / `NOTES_DOCK_MAX_WIDTH` / `NOTES_DOCK_DEFAULT_WIDTH`; and `<NotesDock />`.

**The dock takes space inward and never resizes the OS window.** Spec §6.1 has the
evidence: growing the window is honoured under XWayland and silently corrupts under native
Wayland, where `outer_position()` returns a plausible `Ok(0,0)` for a window that is
elsewhere. Do not add `set_size`, `set_position`, or monitor arithmetic to this feature.

- [ ] **Step 1: Add the store fields**

In `app/src/store/appState.ts`, beside the sidebar-collapsed persistence at the top:

```ts
const NOTES_DOCK_KEY = "triple-c.notes.dock";
const NOTES_DOCK_WIDTH_KEY = "triple-c.notes.dock.width";

/** Wide enough for a note, narrow enough to leave a usable terminal. */
export const NOTES_DOCK_MIN_WIDTH = 260;
export const NOTES_DOCK_MAX_WIDTH = 720;
export const NOTES_DOCK_DEFAULT_WIDTH = 352;

function loadNotesDockOpen(): boolean {
  try {
    return localStorage.getItem(NOTES_DOCK_KEY) === "1";
  } catch {
    return false;
  }
}

function persistNotesDockOpen(value: boolean) {
  try {
    localStorage.setItem(NOTES_DOCK_KEY, value ? "1" : "0");
  } catch {
    // ignore — storage may be unavailable
  }
}

/** Clamped on the way in as well as out: a stored value can be anything a
 *  previous version, a hand edit, or a different screen left behind. */
export function clampDockWidth(value: number): number {
  if (!Number.isFinite(value)) return NOTES_DOCK_DEFAULT_WIDTH;
  return Math.min(NOTES_DOCK_MAX_WIDTH, Math.max(NOTES_DOCK_MIN_WIDTH, Math.round(value)));
}

function loadNotesDockWidth(): number {
  try {
    const raw = localStorage.getItem(NOTES_DOCK_WIDTH_KEY);
    return raw === null ? NOTES_DOCK_DEFAULT_WIDTH : clampDockWidth(Number(raw));
  } catch {
    return NOTES_DOCK_DEFAULT_WIDTH;
  }
}

function persistNotesDockWidth(value: number) {
  try {
    localStorage.setItem(NOTES_DOCK_WIDTH_KEY, String(value));
  } catch {
    // ignore — storage may be unavailable
  }
}
```

In the `AppState` interface, beside the other UI state:

```ts
  /** The notes dock, visible over any tab including a terminal. */
  notesDockOpen: boolean;
  setNotesDockOpen: (open: boolean) => void;
  toggleNotesDock: () => void;
  /** Dock width in CSS px, clamped and persisted per machine. */
  notesDockWidth: number;
  setNotesDockWidth: (width: number) => void;
```

And in the store body:

```ts
  notesDockOpen: loadNotesDockOpen(),
  setNotesDockOpen: (open) => {
    persistNotesDockOpen(open);
    set({ notesDockOpen: open });
  },
  toggleNotesDock: () =>
    set((state) => {
      const open = !state.notesDockOpen;
      persistNotesDockOpen(open);
      return { notesDockOpen: open };
    }),
  notesDockWidth: loadNotesDockWidth(),
  setNotesDockWidth: (width) => {
    const clamped = clampDockWidth(width);
    persistNotesDockWidth(clamped);
    set({ notesDockWidth: clamped });
  },
```

- [ ] **Step 2: Write the failing test**

Create `app/src/components/layout/NotesDock.test.tsx`:

```tsx
import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen } from "@testing-library/react";
import NotesDock from "./NotesDock";
import type { Project, TerminalSession } from "../../lib/types";

vi.mock("../notes/NotesPanel", () => ({
  default: ({ projectId }: { projectId: string }) => (
    <div data-testid="panel">{`panel:${projectId}`}</div>
  ),
}));

let state: Record<string, unknown> = {};
vi.mock("../../store/appState", () => ({
  useAppState: Object.assign(
    (selector: (s: unknown) => unknown) => selector(state),
    { getState: () => state },
  ),
  isHomeTab: (k: string) => k.startsWith("home:"),
  isTerminalTab: (k: string) => k.startsWith("term:"),
  tabKeyId: (k: string) => k.slice(k.indexOf(":") + 1),
}));

const session: TerminalSession = {
  id: "s1",
  projectId: "p9",
  projectName: "api",
  sessionType: "claude",
  sessionName: null,
};

beforeEach(() => {
  state = {
    notesDockOpen: true,
    setNotesDockOpen: vi.fn(),
    toggleNotesDock: vi.fn(),
    activeTabKey: null,
    sessions: [session],
    projects: [{ id: "p9", name: "api" } as unknown as Project],
  };
});

describe("NotesDock", () => {
  it("renders nothing when closed", () => {
    state.notesDockOpen = false;
    const { container } = render(<NotesDock />);
    expect(container).toBeEmptyDOMElement();
  });

  it("follows a project home tab", () => {
    state.activeTabKey = "home:p1";
    render(<NotesDock />);
    expect(screen.getByTestId("panel")).toHaveTextContent("panel:p1");
  });

  it("follows the project of the active terminal tab", () => {
    // The dock exists to be visible while the agent runs, so a terminal tab
    // must resolve to its project, not to nothing.
    state.activeTabKey = "term:s1";
    render(<NotesDock />);
    expect(screen.getByTestId("panel")).toHaveTextContent("panel:p9");
  });

  it("explains itself when no project is active", () => {
    state.activeTabKey = null;
    render(<NotesDock />);
    expect(screen.queryByTestId("panel")).not.toBeInTheDocument();
    expect(screen.getByText(/open a project/i)).toBeInTheDocument();
  });

  it("shows nothing for a terminal whose session has gone", () => {
    state.activeTabKey = "term:vanished";
    render(<NotesDock />);
    expect(screen.queryByTestId("panel")).not.toBeInTheDocument();
  });

  it("renders at the stored width", () => {
    state.activeTabKey = "home:p1";
    state.notesDockWidth = 420;
    render(<NotesDock />);
    expect(screen.getByLabelText("Notes")).toHaveStyle({ width: "420px" });
  });

  it("has a keyboard-reachable resize handle", () => {
    // Drag is a mouse gesture; a separator that only responds to pointer
    // events is unusable without one.
    state.activeTabKey = "home:p1";
    render(<NotesDock />);
    const handle = screen.getByRole("separator", { name: /resize notes/i });
    fireEvent.keyDown(handle, { key: "ArrowLeft" });
    expect(state.setNotesDockWidth).toHaveBeenCalled();
  });
});
```

Create `app/src/store/dockWidth.test.ts`:

```ts
import { describe, it, expect } from "vitest";
import {
  clampDockWidth,
  NOTES_DOCK_MIN_WIDTH,
  NOTES_DOCK_MAX_WIDTH,
  NOTES_DOCK_DEFAULT_WIDTH,
} from "./appState";

describe("clampDockWidth", () => {
  it("keeps a sensible width", () => {
    expect(clampDockWidth(400)).toBe(400);
  });

  it("refuses to squeeze the dock into uselessness", () => {
    expect(clampDockWidth(10)).toBe(NOTES_DOCK_MIN_WIDTH);
  });

  it("refuses to squeeze the terminal into uselessness", () => {
    expect(clampDockWidth(5000)).toBe(NOTES_DOCK_MAX_WIDTH);
  });

  it("falls back for a stored value that is not a number", () => {
    // localStorage holds strings and can carry anything a previous version,
    // a hand edit, or a different screen left behind.
    expect(clampDockWidth(Number("banana"))).toBe(NOTES_DOCK_DEFAULT_WIDTH);
  });

  it("rounds, because a fractional px width blurs the border", () => {
    expect(clampDockWidth(400.6)).toBe(401);
  });
});
```

Add `fireEvent` to the `@testing-library/react` import in `NotesDock.test.tsx`, and
`notesDockWidth` / `setNotesDockWidth` to its `beforeEach` state:

```ts
  state = {
    notesDockOpen: true,
    setNotesDockOpen: vi.fn(),
    toggleNotesDock: vi.fn(),
    notesDockWidth: 352,
    setNotesDockWidth: vi.fn(),
    activeTabKey: null,
    sessions: [session],
    projects: [{ id: "p9", name: "api" } as unknown as Project],
  };
```

- [ ] **Step 3: Run them to verify they fail**

Run: `cd app && npx vitest run src/components/layout/NotesDock.test.tsx src/store/dockWidth.test.ts`
Expected: FAIL — `Failed to resolve import "./NotesDock"`, and `clampDockWidth` not exported.

- [ ] **Step 4: Write the dock**

Create `app/src/components/layout/NotesDock.tsx`:

```tsx
import { useShallow } from "zustand/react/shallow";
import {
  useAppState,
  isHomeTab,
  isTerminalTab,
  tabKeyId,
  NOTES_DOCK_MIN_WIDTH,
  NOTES_DOCK_MAX_WIDTH,
} from "../../store/appState";
import NotesPanel from "../notes/NotesPanel";
import Button from "../ui/Button";

/**
 * Notes beside whatever is on screen.
 *
 * Project Home and Terminal are sibling top-level tabs, so notes living only
 * in a sub-tab would be hidden exactly when the agent is running — which is
 * when a note is worth sending. The dock is the answer to that.
 *
 * **It takes space from inside the window and never resizes it.** Growing the
 * OS window was tried and rejected on evidence: honoured under XWayland,
 * silently corrupting under native Wayland, where `outer_position()` returns a
 * confident `Ok(0,0)` for a window that is somewhere else. See the design doc,
 * §6.1. Narrowing the terminal instead costs nothing — `TerminalView`'s
 * ResizeObserver already reflows xterm and resizes the container PTY.
 */
export default function NotesDock() {
  const {
    notesDockOpen,
    setNotesDockOpen,
    notesDockWidth,
    setNotesDockWidth,
    activeTabKey,
    sessions,
  } = useAppState(
    useShallow((s) => ({
      notesDockOpen: s.notesDockOpen,
      setNotesDockOpen: s.setNotesDockOpen,
      notesDockWidth: s.notesDockWidth,
      setNotesDockWidth: s.setNotesDockWidth,
      activeTabKey: s.activeTabKey,
      sessions: s.sessions,
    })),
  );

  // Dragging the separator. Pointer capture rather than window listeners, so
  // the drag survives the pointer crossing the terminal — which swallows
  // events — and ends correctly if the button is released outside the window.
  const onPointerDown = (e: React.PointerEvent<HTMLDivElement>) => {
    e.preventDefault();
    const handle = e.currentTarget;
    handle.setPointerCapture(e.pointerId);
    const startX = e.clientX;
    const startWidth = notesDockWidth;
    // The dock is on the right, so dragging left widens it.
    const onMove = (move: PointerEvent) =>
      setNotesDockWidth(startWidth + (startX - move.clientX));
    const onUp = () => {
      handle.releasePointerCapture(e.pointerId);
      handle.removeEventListener("pointermove", onMove);
      handle.removeEventListener("pointerup", onUp);
    };
    handle.addEventListener("pointermove", onMove);
    handle.addEventListener("pointerup", onUp);
  };

  const onHandleKeyDown = (e: React.KeyboardEvent<HTMLDivElement>) => {
    const step = e.shiftKey ? 64 : 16;
    if (e.key === "ArrowLeft") {
      e.preventDefault();
      setNotesDockWidth(notesDockWidth + step);
    } else if (e.key === "ArrowRight") {
      e.preventDefault();
      setNotesDockWidth(notesDockWidth - step);
    }
  };

  if (!notesDockOpen) return null;

  // Follow whatever is in front: a home tab is its own project, a terminal tab
  // is the project it belongs to.
  let projectId: string | null = null;
  if (activeTabKey && isHomeTab(activeTabKey)) {
    projectId = tabKeyId(activeTabKey);
  } else if (activeTabKey && isTerminalTab(activeTabKey)) {
    projectId =
      sessions.find((s) => s.id === tabKeyId(activeTabKey))?.projectId ?? null;
  }

  return (
    <aside
      aria-label="Notes"
      style={{ width: `${notesDockWidth}px` }}
      className="relative flex-shrink-0 flex flex-col min-h-0 bg-[var(--bg-secondary)] border border-[var(--border-color)] rounded-[var(--radius-panel)] overflow-hidden"
    >
      {/* Separator, not decoration: it carries a role and arrow keys, because
          a resize that only answers to a drag is unavailable to anyone not
          using a mouse. */}
      <div
        role="separator"
        aria-label="Resize notes panel"
        aria-orientation="vertical"
        aria-valuenow={notesDockWidth}
        aria-valuemin={NOTES_DOCK_MIN_WIDTH}
        aria-valuemax={NOTES_DOCK_MAX_WIDTH}
        tabIndex={0}
        onPointerDown={onPointerDown}
        onKeyDown={onHandleKeyDown}
        className="absolute left-0 top-0 h-full w-1.5 cursor-col-resize hover:bg-[var(--accent-muted)] transition-colors"
      />
      <div className="flex items-center justify-between gap-2 px-3 h-9 flex-shrink-0 border-b border-[var(--border-color)]">
        <h2 className="text-[13px] font-semibold text-[var(--text-primary)]">Notes</h2>
        <Button variant="ghost" onClick={() => setNotesDockOpen(false)} aria-label="Close notes">
          Close
        </Button>
      </div>
      <div className="flex-1 min-h-0">
        {projectId ? (
          <NotesPanel projectId={projectId} />
        ) : (
          <p className="p-4 text-[13px] text-[var(--text-secondary)]">
            Open a project or a terminal to see its notes.
          </p>
        )}
      </div>
    </aside>
  );
}
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cd app && npx vitest run src/components/layout/NotesDock.test.tsx src/store/dockWidth.test.ts`
Expected: PASS — 12 tests.

- [ ] **Step 6: Mount it and add the toggle**

In `app/src/App.tsx`, import it:

```tsx
import NotesDock from "./components/layout/NotesDock";
```

and render it as a sibling of `<main>`, so it takes width from the row rather than covering
it — closing `</main>` on the existing line, then:

```tsx
          </main>
          <NotesDock />
        </div>
```

In `app/src/components/layout/StatusBar.tsx`, add `notesDockOpen` and `toggleNotesDock` to
the `useShallow` selector, then add a control to the right-aligned cluster, before the STT
button:

```tsx
          <button
            onClick={toggleNotesDock}
            aria-pressed={notesDockOpen}
            className="text-[var(--accent)] hover:text-[var(--accent-hover)] cursor-pointer"
            title="Show or hide the notes panel beside the current tab"
          >
            Notes
          </button>
```

- [ ] **Step 7: Verify everything**

Run: `cd app && npx vitest run && npx tsc --noEmit`
Expected: PASS, no type errors.

Then check it by hand, because the two things that matter here are not unit-testable:
1. Open a Claude terminal, open the dock, and confirm the terminal **reflows** — xterm
   re-wraps and the prompt stays usable — rather than being clipped.
2. Send a multi-line note and confirm it arrives as **one** prompt with real line breaks,
   unsubmitted, with the cursor left in it. `TerminalView`'s own comment records that jsdom
   never synthesizes the follow-up keypress, so this family of bug is invisible to the test
   suite and must be seen in Chromium.

- [ ] **Step 8: Commit**

```bash
git add app/src/store/appState.ts app/src/store/dockWidth.test.ts \
        app/src/components/layout/NotesDock.tsx \
        app/src/components/layout/NotesDock.test.tsx app/src/App.tsx \
        app/src/components/layout/StatusBar.tsx
git commit -m "Add the notes dock"
```

---

## Done when

- `cd app/src-tauri && cargo test` passes, including the command-registration test.
- `cd app && npx vitest run` passes and `npx tsc --noEmit` is clean.
- A note survives an app restart, and is readable with the container **stopped**.
- A multi-line note sent to a Claude session arrives as one unsubmitted prompt.
- The send button is disabled with no running Claude session, sends directly with one, and
  offers named targets with several.
- Removing a project removes its notes file.
- The dock's width survives a restart, and can be changed with the keyboard as well as a drag.
