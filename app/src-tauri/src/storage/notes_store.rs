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
