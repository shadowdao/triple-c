//! Host-side record of Docker resources `remove_project` could not delete.
//!
//! `remove_project` drops a project's id from `projects.json` unconditionally
//! — see the comment on `ProjectRemovalReport` — so once that happens nothing
//! in the app can name the leftover container, image or volume again by any
//! path a user can reach. This is what keeps it reachable anyway: one JSON
//! file per affected project under `<data_dir>/triple-c/pending-cleanup/`,
//! written *before* the project record is dropped. Startup housekeeping
//! retries every record on the next launch (see
//! `commands::project_commands::retry_pending_cleanup_logged`) and deletes
//! the ones that fully succeed.
//!
//! **This record is written in the same instant its record in `projects.json`
//! is destroyed, and it is the only remaining handle on the leftover
//! resource** — which is a stronger claim on durability than an ordinary
//! write-temp-then-rename gives. `storage::migration_store::save` carries the
//! same reasoning for the migration state file: `fs::write` returns once the
//! bytes are in the page cache, and a rename over them is atomic with respect
//! to other readers, not to power loss. A crash in that window leaves the
//! rename applied and the data half-written, which [`list`] then treats as
//! unparseable and skips — reproducing the exact bug this module exists to
//! close, silently, with only a startup log line as evidence. So `save` here
//! takes the same `File::create` → `write_all` → `sync_all` → `rename` →
//! directory-sync shape `migration_store` does.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingCleanup {
    pub project_id: String,
    /// Kept only so a log line or a future UI can name the project without a
    /// second lookup — the project record itself is already gone by the time
    /// this is read back.
    pub project_name: String,
    /// The project's container, if it could not be removed. Named by its
    /// deterministic `triple-c-{id}` name rather than the (possibly stale)
    /// container id Docker handed out — Docker's remove-container API
    /// accepts either, and the name is the one identifier guaranteed to still
    /// resolve to the same container by the time a retry runs.
    pub container_id: Option<String>,
    pub image: Option<String>,
    pub volumes: Vec<String>,
    pub recorded_at: String,
}

impl PendingCleanup {
    /// True once nothing named here still needs to be removed.
    pub fn is_empty(&self) -> bool {
        self.container_id.is_none() && self.image.is_none() && self.volumes.is_empty()
    }
}

/// `<data_dir>/triple-c/pending-cleanup`, created on demand.
fn dir() -> Result<PathBuf, String> {
    let dir = dirs::data_dir()
        .ok_or_else(|| {
            "Could not determine data directory. Set XDG_DATA_HOME on Linux.".to_string()
        })?
        .join("triple-c")
        .join("pending-cleanup");
    fs::create_dir_all(&dir)
        .map_err(|e| format!("Failed to create pending-cleanup directory: {}", e))?;
    Ok(dir)
}

/// Project ids are UUIDs, but they arrive over IPC, so refuse to let one steer
/// the write anywhere but the pending-cleanup directory. Mirrors
/// `storage::migration_store::sanitize`.
fn sanitize(project_id: &str) -> String {
    project_id
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect()
}

/// Write (or overwrite) a project's pending-cleanup record.
pub fn save(record: &PendingCleanup) -> Result<(), String> {
    save_in(&dir()?, record)
}

/// Remove a project's pending-cleanup record. Missing is success — this is
/// how a fully-succeeded retry (or a record that never existed) is expressed.
pub fn clear(project_id: &str) -> Result<(), String> {
    clear_in(&dir()?, project_id)
}

/// Every pending-cleanup record on disk. An unparseable file is logged and
/// skipped rather than blocking every other project's retry — the same
/// "one bad record can't wedge the rest" reasoning as the migration store.
pub fn list() -> Vec<PendingCleanup> {
    let Ok(dir) = dir() else { return Vec::new() };
    list_in(&dir)
}

fn path_in(dir: &Path, project_id: &str) -> PathBuf {
    dir.join(format!("{}.json", sanitize(project_id)))
}

/// Durable write: fsync the file before the rename, and fsync the directory
/// after it — see the module doc comment for why a plain
/// write-temp-then-rename is not enough here. Mirrors
/// `storage::migration_store::save`/`sync_dir`.
fn save_in(dir: &Path, record: &PendingCleanup) -> Result<(), String> {
    let path = path_in(dir, &record.project_id);
    let data = serde_json::to_string_pretty(record)
        .map_err(|e| format!("Failed to serialize pending cleanup record: {}", e))?;
    let tmp = path.with_extension("json.tmp");

    {
        use std::io::Write;
        let mut file = fs::File::create(&tmp)
            .map_err(|e| format!("Failed to write pending cleanup record: {}", e))?;
        file.write_all(data.as_bytes())
            .map_err(|e| format!("Failed to write pending cleanup record: {}", e))?;
        file.sync_all()
            .map_err(|e| format!("Failed to flush pending cleanup record to disk: {}", e))?;
    }

    fs::rename(&tmp, &path)
        .map_err(|e| format!("Failed to commit pending cleanup record: {}", e))?;
    sync_dir(&path);
    Ok(())
}

fn clear_in(dir: &Path, project_id: &str) -> Result<(), String> {
    let path = path_in(dir, project_id);
    match fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("Failed to remove pending cleanup record: {}", e)),
    }
}

fn list_in(dir: &Path) -> Vec<PendingCleanup> {
    let Ok(entries) = fs::read_dir(dir) else { return Vec::new() };

    entries
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "json"))
        .filter_map(|e| {
            let path = e.path();
            let data = fs::read_to_string(&path).ok()?;
            match serde_json::from_str::<PendingCleanup>(&data) {
                Ok(record) => Some(record),
                Err(err) => {
                    // Moved aside rather than left in place: a record nothing
                    // ever repairs would otherwise warn on every single
                    // startup forever, same as an ordinary `.json` file it
                    // would keep looking like one to `list_in` on the next
                    // call too. One aside-copy is enough here — this only
                    // ever holds names to retry removing, not the class of
                    // once-in-a-lifetime crash evidence `migration_store`
                    // keeps multiple timestamped backups of.
                    let corrupt = path.with_extension("json.corrupt");
                    let moved = !corrupt.exists() && fs::rename(&path, &corrupt).is_ok();
                    log::warn!(
                        "Could not parse pending cleanup record {}: {}{}",
                        path.display(),
                        err,
                        if moved {
                            format!(" — moved aside to {}", corrupt.display())
                        } else {
                            " — leaving it in place".to_string()
                        }
                    );
                    None
                }
            }
        })
        .collect()
}

/// fsync the directory holding `path`, so a rename into it survives power
/// loss. Best effort only on the platforms where it is meaningless: Windows
/// has no directory handle to sync and errors on the attempt, so failure is
/// logged rather than propagated — the file's own `sync_all` above is what
/// carries the data. Mirrors `storage::migration_store::sync_dir`, which is
/// private to that module, so this is a small deliberate duplicate rather
/// than a shared dependency between two otherwise-independent stores.
fn sync_dir(path: &Path) {
    let Some(dir) = path.parent() else { return };
    match fs::File::open(dir).and_then(|d| d.sync_all()) {
        Ok(()) => {}
        Err(e) => log::debug!(
            "Could not fsync the pending-cleanup directory {}: {} — the record itself was flushed",
            dir.display(),
            e
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "triple-c-pending-cleanup-{}-{}",
            name,
            uuid::Uuid::new_v4().simple()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn record(project_id: &str) -> PendingCleanup {
        PendingCleanup {
            project_id: project_id.to_string(),
            project_name: "Some Project".to_string(),
            container_id: Some("triple-c-abc".to_string()),
            image: Some("triple-c-snapshot-abc:latest".to_string()),
            volumes: vec!["triple-c-home-abc".to_string()],
            recorded_at: "2026-08-25T00:00:00Z".to_string(),
        }
    }

    #[test]
    fn project_ids_cannot_escape_the_pending_cleanup_directory() {
        assert_eq!(sanitize("../../etc/passwd"), "______etc_passwd");
        assert_eq!(sanitize("a/b"), "a_b");
        assert_eq!(
            sanitize("ab62cd24-51aa-4645-8f5c-17a124062050"),
            "ab62cd24-51aa-4645-8f5c-17a124062050"
        );
    }

    #[test]
    fn is_empty_reflects_whatever_still_needs_removing() {
        let mut r = record("p1");
        assert!(!r.is_empty());

        r.container_id = None;
        r.image = None;
        assert!(!r.is_empty(), "a leftover volume alone still counts");

        r.volumes.clear();
        assert!(r.is_empty());
    }

    /// Exercises the real `save_in`/`list_in`/`clear_in` — not a
    /// re-implementation of their bodies — against a temp directory standing
    /// in for `dir()`.
    #[test]
    fn a_saved_record_round_trips_and_clearing_removes_it() {
        let dir = temp_dir("roundtrip");
        let rec = record("proj-1");

        save_in(&dir, &rec).expect("save");
        let found = list_in(&dir);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].project_id, "proj-1");
        assert_eq!(found[0].volumes, vec!["triple-c-home-abc".to_string()]);

        clear_in(&dir, "proj-1").expect("clear");
        assert!(list_in(&dir).is_empty());

        fs::remove_dir_all(&dir).ok();
    }

    /// A second `save` for the same project overwrites rather than appending
    /// — a retry that narrows the leftovers must not leave the old, wider
    /// record behind it.
    #[test]
    fn saving_the_same_project_twice_overwrites_not_appends() {
        let dir = temp_dir("overwrite");
        let mut rec = record("proj-1");
        save_in(&dir, &rec).expect("save");

        rec.container_id = None;
        rec.image = None;
        save_in(&dir, &rec).expect("save again");

        let found = list_in(&dir);
        assert_eq!(found.len(), 1, "one file per project, not one per save");
        assert!(found[0].container_id.is_none());
        assert_eq!(found[0].volumes, vec!["triple-c-home-abc".to_string()]);

        fs::remove_dir_all(&dir).ok();
    }

    /// A record that fails to parse must not poison the rest of the listing.
    #[test]
    fn an_unparseable_record_is_skipped_not_fatal() {
        let dir = temp_dir("corrupt");
        fs::write(dir.join("bad.json"), "{ not json").unwrap();
        save_in(&dir, &record("proj-2")).expect("save");

        let found = list_in(&dir);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].project_id, "proj-2");

        fs::remove_dir_all(&dir).ok();
    }

    /// `list_in` must not pick up the `.json.tmp` staging file `save_in`
    /// leaves behind if a crash lands between the write and the rename — the
    /// whole point of the temp-then-rename dance is that only the renamed
    /// file is ever a complete record.
    #[test]
    fn a_leftover_tmp_file_is_not_listed() {
        let dir = temp_dir("tmp-leftover");
        fs::write(dir.join("proj-3.json.tmp"), "not a complete record").unwrap();
        assert!(list_in(&dir).is_empty());

        fs::remove_dir_all(&dir).ok();
    }

    /// Clearing by project id must remove exactly the file that id maps to
    /// under `sanitize`, and nothing else.
    #[test]
    fn clearing_one_project_does_not_touch_another() {
        let dir = temp_dir("clear-scoped");
        save_in(&dir, &record("proj-a")).unwrap();
        save_in(&dir, &record("proj-b")).unwrap();

        clear_in(&dir, "proj-a").unwrap();

        let found = list_in(&dir);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].project_id, "proj-b");

        fs::remove_dir_all(&dir).ok();
    }
}
