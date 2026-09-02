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

use serde::{Deserialize, Serialize};

use crate::models::Note;

/// The version stamped into every notes file this build writes.
const NOTES_FORMAT_VERSION: u32 = 1;

/// What is actually on disk: a version envelope around the notes.
///
/// The list is wrapped rather than written bare because the wrapper costs
/// nothing today and cannot be added cheaply later — once files exist in the
/// field, every reader has to sniff two shapes forever. `version` is written
/// and read back but nothing branches on it yet: it is the hook a future
/// format change hangs off, and its value is only useful if it has been there
/// since the first file.
///
/// Not in `models/` and not exposed over IPC: the frontend receives
/// `Vec<Note>` from `list_notes` and never sees the envelope, so this is a
/// storage detail rather than part of the IPC contract.
#[derive(Debug, Serialize, Deserialize)]
struct ProjectNotes {
    version: u32,
    #[serde(default)]
    notes: Vec<Note>,
}

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
/// anything rewrote the file, and therefore the one worth having — and capped,
/// because `list_notes` runs on *every* panel mount. See [`keep_corrupt_copy`].
fn load_in(dir: &Path, project_id: &str) -> Result<Vec<Note>, String> {
    let path = notes_path_in(dir, project_id);
    if !path.exists() {
        return Ok(Vec::new());
    }
    let data = fs::read_to_string(&path).map_err(|e| format!("Failed to read notes: {}", e))?;
    match parse(&data) {
        Ok(notes) => Ok(notes),
        Err(e) => {
            let kept = keep_corrupt_copy(&path, &chrono::Utc::now());
            log::error!(
                "Failed to parse notes for project {}: {} — treating as empty; the file is \
                 left in place{}",
                project_id,
                e,
                kept.describe()
            );
            Ok(Vec::new())
        }
    }
}

/// Parse a notes file: the versioned envelope, or a bare array.
///
/// The bare array is what this store wrote before [`ProjectNotes`] existed —
/// only ever on a development build, but a developer's own notes are still
/// prose nothing else holds a copy of, and the alternative is `load_in`
/// declaring a perfectly readable file corrupt. It is read, never written: the
/// first save rewrites the file with an envelope.
fn parse(data: &str) -> Result<Vec<Note>, serde_json::Error> {
    match serde_json::from_str::<ProjectNotes>(data) {
        Ok(file) => Ok(file.notes),
        // Report the envelope's error, not the array's — the envelope is the
        // shape this store writes, so its message is the one that describes
        // what is actually wrong with the file.
        Err(envelope_err) => serde_json::from_str::<Vec<Note>>(data).map_err(|_| envelope_err),
    }
}

/// How many timestamped copies of one project's corrupt notes file are kept.
///
/// Timestamping fixes "a second corruption overwrote the first" and introduces
/// its opposite: `load_in` runs on every `list_notes`, which is every panel
/// mount — every project switch, every dock-follows-tab change, every sub-tab
/// toggle. A file that is *persistently* unparseable (the normal case, since
/// nothing repairs it) would otherwise mint a fresh full copy of the user's
/// prose every time the clock's second changed. Nothing ever reads them back
/// and nothing ever removed them.
///
/// Four is enough for the only use there is: a human looking at what the file
/// held. Same constant, same reasoning as `migration_store`.
const MAX_CORRUPT_BACKUPS: usize = 4;

/// What [`keep_corrupt_copy`] did, so the log line can tell the truth about
/// whether a file exists.
///
/// Three outcomes, and they must not be conflated. Folding "already kept
/// enough" into success and then saying "a copy was kept" names a file that
/// was never created — which is what someone reads before going to look for
/// their data.
enum Kept {
    Copied(PathBuf),
    /// This exact second's copy was already on disk.
    AlreadyThere(PathBuf),
    /// The cap is reached; the earlier copies are kept and this one is not.
    EnoughAlready(usize),
    Failed(String),
}

impl Kept {
    fn describe(&self) -> String {
        match self {
            Kept::Copied(p) | Kept::AlreadyThere(p) => format!(" (a copy is at {})", p.display()),
            // The earliest copies are the ones worth having, so the cap keeps
            // those and drops this one. Say so, rather than implying a file
            // exists.
            Kept::EnoughAlready(n) => format!(
                " (no copy kept — {} earlier copies of this file are already saved alongside it)",
                n
            ),
            Kept::Failed(e) => format!(" (could not keep a copy: {})", e),
        }
    }
}

/// Where a copy of an unreadable notes file is kept.
fn corrupt_backup_path(path: &Path, now: &chrono::DateTime<chrono::Utc>) -> PathBuf {
    path.with_extension(format!("json.corrupt-{}.bak", now.format("%Y%m%d-%H%M%S")))
}

/// Whether [`MAX_CORRUPT_BACKUPS`] copies of this project's file already exist.
///
/// Asked *before* the copy rather than pruning after it, so the cap is not
/// implemented by writing a file and deleting it again on every pass — and so
/// the copies that survive are the oldest, which are the ones taken closest to
/// whatever produced the corruption.
///
/// A directory that cannot be listed answers "not full": failing open costs at
/// most one extra file, and failing closed would drop the very first copy of
/// prose nothing else has kept.
fn corrupt_backups_full(path: &Path) -> bool {
    let (Some(dir), Some(stem)) = (path.parent(), path.file_stem()) else {
        return false;
    };
    // `{stem}.json.corrupt-` — the same shape `corrupt_backup_path` builds, so
    // this can never match another project's copies or an unrelated `.bak`.
    let prefix = format!("{}.json.corrupt-", stem.to_string_lossy());
    let Ok(entries) = fs::read_dir(dir) else {
        return false;
    };
    entries
        .flatten()
        .filter(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            name.starts_with(&prefix) && name.ends_with(".bak")
        })
        .count()
        >= MAX_CORRUPT_BACKUPS
}

fn keep_corrupt_copy(path: &Path, now: &chrono::DateTime<chrono::Utc>) -> Kept {
    let backup = corrupt_backup_path(path, now);
    if backup.exists() {
        return Kept::AlreadyThere(backup);
    }
    if corrupt_backups_full(path) {
        return Kept::EnoughAlready(MAX_CORRUPT_BACKUPS);
    }
    match fs::copy(path, &backup) {
        Ok(_) => Kept::Copied(backup),
        Err(e) => Kept::Failed(e.to_string()),
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
    let _guard = write_lock().lock().unwrap_or_else(|e| e.into_inner());
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
    let file = ProjectNotes {
        version: NOTES_FORMAT_VERSION,
        notes: notes.to_vec(),
    };
    let data = serde_json::to_string_pretty(&file)
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

    fn corrupt_copies(dir: &std::path::Path) -> Vec<String> {
        std::fs::read_dir(dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| n.contains(".corrupt-"))
            .collect()
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

        assert_eq!(
            corrupt_copies(&dir).len(),
            1,
            "the bytes must be kept exactly once"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn what_is_written_is_a_version_envelope_not_a_bare_array() {
        // The envelope costs nothing now and cannot be added cheaply once
        // files exist in the field, so the very first file has to carry it.
        let dir = temp_dir("envelope");
        upsert_in(&dir, "p1", Note::new("t".into(), "b".into())).unwrap();

        let raw = std::fs::read_to_string(notes_path_in(&dir, "p1")).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(parsed["version"], NOTES_FORMAT_VERSION);
        assert_eq!(parsed["notes"].as_array().unwrap().len(), 1);
        assert_eq!(parsed["notes"][0]["body"], "b");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_pre_envelope_bare_array_still_reads_and_is_not_called_corrupt() {
        // Only a development build ever wrote this shape, but declaring a
        // perfectly readable file corrupt is the one outcome this store exists
        // to avoid. It is read, never written back.
        let dir = temp_dir("legacy");
        let note = Note::new("Deploy".into(), "one\ntwo".into());
        std::fs::write(
            notes_path_in(&dir, "p1"),
            serde_json::to_string(&vec![note.clone()]).unwrap(),
        )
        .unwrap();

        let loaded = load_in(&dir, "p1").unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].body, "one\ntwo");
        let copies = corrupt_copies(&dir);
        assert!(copies.is_empty(), "a readable file must not be copied aside");

        // The next write upgrades it in place.
        upsert_in(&dir, "p1", note).unwrap();
        let raw = std::fs::read_to_string(notes_path_in(&dir, "p1")).unwrap();
        assert!(raw.contains("\"version\""));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn corrupt_copies_are_capped_rather_than_one_per_second() {
        // `list_notes` runs on every panel mount, so an unrepaired file would
        // otherwise mint a full copy of the user's prose every time the
        // clock's second changed.
        let dir = temp_dir("cap");
        let path = notes_path_in(&dir, "p1");
        std::fs::write(&path, b"{ not json").unwrap();

        let base = chrono::Utc::now();
        for i in 0..MAX_CORRUPT_BACKUPS as i64 + 3 {
            let at = base + chrono::Duration::seconds(i);
            let kept = keep_corrupt_copy(&path, &at);
            if i < MAX_CORRUPT_BACKUPS as i64 {
                assert!(matches!(kept, Kept::Copied(_)), "copy {} should be kept", i);
            } else {
                assert!(
                    matches!(kept, Kept::EnoughAlready(MAX_CORRUPT_BACKUPS)),
                    "copy {} should be refused by the cap",
                    i
                );
            }
        }
        assert_eq!(corrupt_copies(&dir).len(), MAX_CORRUPT_BACKUPS);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_second_read_in_the_same_second_does_not_re_copy() {
        let dir = temp_dir("samesecond");
        let path = notes_path_in(&dir, "p1");
        std::fs::write(&path, b"{ not json").unwrap();

        let at = chrono::Utc::now();
        assert!(matches!(keep_corrupt_copy(&path, &at), Kept::Copied(_)));
        assert!(matches!(
            keep_corrupt_copy(&path, &at),
            Kept::AlreadyThere(_)
        ));
        assert_eq!(corrupt_copies(&dir).len(), 1);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_log_line_never_claims_a_backup_that_was_not_written() {
        // A message that invents a backup is worse than no message: it is what
        // someone reads before going to look for their data.
        let dir = temp_dir("honesty");
        let path = notes_path_in(&dir, "p1");
        std::fs::write(&path, b"{ not json").unwrap();

        let copied = keep_corrupt_copy(&path, &chrono::Utc::now()).describe();
        assert!(copied.contains("a copy is at"));

        let refused = Kept::EnoughAlready(MAX_CORRUPT_BACKUPS).describe();
        assert!(refused.contains("no copy kept"));
        assert!(!refused.contains("a copy is at"));

        let failed = Kept::Failed("permission denied".into()).describe();
        assert!(failed.contains("could not keep a copy"));
        assert!(!failed.contains("a copy is at"));
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

    #[test]
    fn clearing_is_what_project_removal_calls_and_it_never_fails_on_absence() {
        // `remove_project` must not be able to fail because a project simply
        // never had any notes — an orphaned notes file is harmless, a project
        // that cannot be removed is not.
        let dir = temp_dir("removal");
        assert!(clear_in(&dir, "never-had-notes").is_ok());
        std::fs::remove_dir_all(&dir).ok();
    }
}
