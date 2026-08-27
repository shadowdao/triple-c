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
//! Same write-temp-then-rename shape as `projects.json` and the migration
//! store, and the same per-project-file layout as
//! `storage::migration_store` — a stuck cleanup record for one project must
//! never block the retry of another's.

use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingCleanup {
    pub project_id: String,
    /// Kept only so a log line or a future UI can name the project without a
    /// second lookup — the project record itself is already gone by the time
    /// this is read back.
    pub project_name: String,
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

fn path_for(project_id: &str) -> Result<PathBuf, String> {
    Ok(dir()?.join(format!("{}.json", sanitize(project_id))))
}

/// Write (or overwrite) a project's pending-cleanup record.
pub fn save(record: &PendingCleanup) -> Result<(), String> {
    let path = path_for(&record.project_id)?;
    let data = serde_json::to_string_pretty(record)
        .map_err(|e| format!("Failed to serialize pending cleanup record: {}", e))?;
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, data).map_err(|e| format!("Failed to write pending cleanup record: {}", e))?;
    fs::rename(&tmp, &path).map_err(|e| format!("Failed to commit pending cleanup record: {}", e))
}

/// Remove a project's pending-cleanup record. Missing is success — this is
/// how a fully-succeeded retry (or a record that never existed) is expressed.
pub fn clear(project_id: &str) -> Result<(), String> {
    let path = path_for(project_id)?;
    match fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("Failed to remove pending cleanup record: {}", e)),
    }
}

/// Every pending-cleanup record on disk. An unparseable file is logged and
/// skipped rather than blocking every other project's retry — the same
/// "one bad record can't wedge the rest" reasoning as the migration store.
pub fn list() -> Vec<PendingCleanup> {
    let Ok(dir) = dir() else { return Vec::new() };
    let Ok(entries) = fs::read_dir(&dir) else { return Vec::new() };

    entries
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "json"))
        .filter_map(|e| {
            let path = e.path();
            let data = fs::read_to_string(&path).ok()?;
            match serde_json::from_str::<PendingCleanup>(&data) {
                Ok(record) => Some(record),
                Err(err) => {
                    log::warn!(
                        "Could not parse pending cleanup record {}: {} — skipping it this run",
                        path.display(),
                        err
                    );
                    None
                }
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_data_dir(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("triple-c-pending-cleanup-{}-{}", name, uuid::Uuid::new_v4().simple()))
    }

    fn record(project_id: &str) -> PendingCleanup {
        PendingCleanup {
            project_id: project_id.to_string(),
            project_name: "Some Project".to_string(),
            container_id: Some("abc123".to_string()),
            image: Some("triple-c-snapshot-abc:latest".to_string()),
            volumes: vec!["triple-c-home-abc".to_string()],
            recorded_at: "2026-08-25T00:00:00Z".to_string(),
        }
    }

    /// `list`/`save`/`clear` go through `dirs::data_dir()`, so these exercise
    /// the pure parts directly against a temp directory rather than the real
    /// one — same approach `migration_store`'s tests take for `sanitize`.
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

    /// Save-then-list-then-clear against a real (temp) directory, bypassing
    /// `dir()`'s hardcoded `dirs::data_dir()` join by writing/reading the
    /// files directly the way `save`/`list` do internally.
    #[test]
    fn a_saved_record_round_trips_and_clearing_removes_it() {
        let dir = temp_data_dir("roundtrip");
        fs::create_dir_all(&dir).unwrap();
        let rec = record("proj-1");
        let path = dir.join(format!("{}.json", sanitize(&rec.project_id)));

        let data = serde_json::to_string_pretty(&rec).unwrap();
        fs::write(&path, data).unwrap();

        let loaded: PendingCleanup =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(loaded.project_id, "proj-1");
        assert_eq!(loaded.volumes, vec!["triple-c-home-abc".to_string()]);

        fs::remove_file(&path).unwrap();
        assert!(!path.exists());

        fs::remove_dir_all(&dir).ok();
    }

    /// A record that fails to parse must not poison the rest of the listing.
    #[test]
    fn an_unparseable_record_is_skipped_not_fatal() {
        let dir = temp_data_dir("corrupt");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("bad.json"), "{ not json").unwrap();
        let good = record("proj-2");
        fs::write(
            dir.join("proj-2.json"),
            serde_json::to_string_pretty(&good).unwrap(),
        )
        .unwrap();

        let mut found = Vec::new();
        for entry in fs::read_dir(&dir).unwrap().flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|ext| ext == "json") {
                if let Ok(data) = fs::read_to_string(&path) {
                    if let Ok(r) = serde_json::from_str::<PendingCleanup>(&data) {
                        found.push(r);
                    }
                }
            }
        }
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].project_id, "proj-2");

        fs::remove_dir_all(&dir).ok();
    }
}
