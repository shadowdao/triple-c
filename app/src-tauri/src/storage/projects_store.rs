use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::models::Project;

/// The sticky marker for `projects.json`: `projects.json.corrupt`, beside it.
///
/// Derived from the file rather than from `dirs::data_dir()` so the marker
/// always lands in the directory the store is actually using — and so the
/// writer can be tested against a temp directory.
fn corrupt_marker_for(file_path: &Path) -> PathBuf {
    file_path.with_extension("json.corrupt")
}

/// `<data_dir>/triple-c/projects.json.corrupt`, whether or not it exists.
pub fn corrupt_marker_path() -> Option<PathBuf> {
    dirs::data_dir().map(|d| corrupt_marker_for(&d.join("triple-c").join("projects.json")))
}

/// When this data directory last loaded a `projects.json` it could not parse,
/// as the RFC3339 instant recorded in the marker.
///
/// ## Why this outlives the load that wrote it
///
/// A corrupt load is *recoverable for the app* — the list starts empty and
/// everything keeps working — and that recovery is precisely what makes it
/// dangerous for anything that reasons about which projects exist. The
/// in-memory symptom does not survive: the first [`ProjectsStore::save`] after
/// the failure, which is as little as starting one project (`update_status`),
/// writes `[{that one project}]` over the file. From then on `projects.json`
/// parses, holds one id, and looks exactly like a user with one project — while
/// every *other* project's home and config volume is on the daemon claimed by
/// nobody.
///
/// The guard in `project_store_trust` keyed on "the list is empty and the file
/// exists", which that write silently ends. So the fact is recorded on disk
/// instead of inferred from the list's shape, and it is **sticky**: nothing in
/// this app clears it, because nothing in this app can reconstruct what the
/// unreadable file held. The refusal names the marker so a user who has
/// restored their list — or accepted the loss — can delete it deliberately.
pub fn corrupt_since() -> Option<String> {
    let raw = fs::read_to_string(corrupt_marker_path()?).ok()?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        // The marker's presence is the signal; an empty one still means a
        // corrupt load happened, it just cannot say when.
        return Some("an unknown time".to_string());
    }
    Some(trimmed.lines().next().unwrap_or(trimmed).to_string())
}

/// Keep the bytes of an unparseable `projects.json`, and record that it
/// happened.
///
/// **The existing `.bak` is never overwritten.** A second corruption used to
/// clobber the first, and the first is the valuable one: it was taken before
/// the app rewrote the file with whatever it had in memory, so it is the only
/// copy that can still hold the full project list. Later ones are copies of an
/// already-degraded file and get a timestamped name.
fn record_corrupt_load(file_path: &Path, now: &chrono::DateTime<chrono::Utc>) {
    let first = file_path.with_extension("json.bak");
    let backup = if first.exists() {
        file_path.with_extension(format!("json.corrupt-{}.bak", now.format("%Y%m%d-%H%M%S")))
    } else {
        first
    };
    if !backup.exists() {
        if let Err(e) = fs::copy(file_path, &backup) {
            log::error!("Failed to back up corrupted projects.json: {}", e);
        } else {
            log::error!(
                "A copy of the unreadable projects.json was kept at {}",
                backup.display()
            );
        }
    }

    let marker = corrupt_marker_for(file_path);
    if marker.exists() {
        // Sticky: the *first* corruption is the one that dates the loss.
        return;
    }
    if let Err(e) = fs::write(&marker, now.to_rfc3339()) {
        log::error!(
            "Could not record the corrupt projects.json load at {}: {} — orphan detection will \
             not know the project list is incomplete",
            marker.display(),
            e
        );
    }
}

pub struct ProjectsStore {
    projects: Mutex<Vec<Project>>,
    file_path: PathBuf,
}

impl ProjectsStore {
    pub fn new() -> Result<Self, String> {
        let data_dir = dirs::data_dir()
            .ok_or_else(|| "Could not determine data directory. Set XDG_DATA_HOME on Linux.".to_string())?
            .join("triple-c");

        fs::create_dir_all(&data_dir).ok();

        let file_path = data_dir.join("projects.json");

        let (projects, needs_save) = if file_path.exists() {
            match fs::read_to_string(&file_path) {
                Ok(data) => {
                    // First try to parse as Vec<Value> to run migration
                    match serde_json::from_str::<Vec<serde_json::Value>>(&data) {
                        Ok(raw_values) => {
                            let mut migrated = false;
                            let migrated_values: Vec<serde_json::Value> = raw_values
                                .into_iter()
                                .map(|v| {
                                    let has_path = v.as_object().map_or(false, |o| o.contains_key("path") && !o.contains_key("paths"));
                                    if has_path {
                                        migrated = true;
                                    }
                                    crate::models::Project::migrate_from_value(v)
                                })
                                .collect();

                            // Now deserialize the migrated values
                            let json_str = serde_json::to_string(&migrated_values).unwrap_or_default();
                            match serde_json::from_str::<Vec<crate::models::Project>>(&json_str) {
                                Ok(parsed) => (parsed, migrated),
                                Err(e) => {
                                    log::error!("Failed to parse migrated projects.json: {}. Starting with empty list.", e);
                                    record_corrupt_load(&file_path, &chrono::Utc::now());
                                    (Vec::new(), false)
                                }
                            }
                        }
                        Err(e) => {
                            log::error!("Failed to parse projects.json: {}. Starting with empty list.", e);
                            record_corrupt_load(&file_path, &chrono::Utc::now());
                            (Vec::new(), false)
                        }
                    }
                }
                Err(e) => {
                    log::error!("Failed to read projects.json: {}", e);
                    (Vec::new(), false)
                }
            }
        } else {
            (Vec::new(), false)
        };

        // Reconcile stale transient statuses: on a cold app start no Docker
        // operations can be in flight, so Starting/Stopping are always stale.
        // Running/Error are left as-is and reconciled against Docker later
        // via the reconcile_project_statuses command.
        let mut projects = projects;
        let mut needs_save = needs_save;
        for p in projects.iter_mut() {
            match p.status {
                crate::models::ProjectStatus::Starting | crate::models::ProjectStatus::Stopping => {
                    log::warn!(
                        "Reconciling stale '{}' status for project '{}' ({}) → Stopped",
                        serde_json::to_string(&p.status).unwrap_or_default().trim_matches('"'),
                        p.name,
                        p.id
                    );
                    p.status = crate::models::ProjectStatus::Stopped;
                    p.updated_at = chrono::Utc::now().to_rfc3339();
                    needs_save = true;
                }
                _ => {}
            }
        }

        let store = Self {
            projects: Mutex::new(projects),
            file_path,
        };

        // Persist migrated/reconciled format back to disk
        if needs_save {
            log::info!("Saving reconciled/migrated projects.json to disk");
            let projects = store.lock();
            if let Err(e) = store.save(&projects) {
                log::error!("Failed to save projects: {}", e);
            }
        }

        Ok(store)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<Project>> {
        self.projects.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn save(&self, projects: &[Project]) -> Result<(), String> {
        let data = serde_json::to_string_pretty(projects)
            .map_err(|e| format!("Failed to serialize projects: {}", e))?;

        // Atomic write: write to temp file, then rename
        let tmp_path = self.file_path.with_extension("json.tmp");
        fs::write(&tmp_path, data)
            .map_err(|e| format!("Failed to write temp projects file: {}", e))?;
        fs::rename(&tmp_path, &self.file_path)
            .map_err(|e| format!("Failed to rename projects file: {}", e))?;
        Ok(())
    }

    pub fn list(&self) -> Vec<Project> {
        self.lock().clone()
    }

    pub fn get(&self, id: &str) -> Option<Project> {
        self.lock().iter().find(|p| p.id == id).cloned()
    }

    pub fn add(&self, project: Project) -> Result<Project, String> {
        let mut projects = self.lock();
        let cloned = project.clone();
        projects.push(project);
        self.save(&projects)?;
        Ok(cloned)
    }

    pub fn update(&self, updated: Project) -> Result<Project, String> {
        let mut projects = self.lock();
        if let Some(p) = projects.iter_mut().find(|p| p.id == updated.id) {
            *p = updated.clone();
            self.save(&projects)?;
            Ok(updated)
        } else {
            Err(format!("Project {} not found", updated.id))
        }
    }

    pub fn remove(&self, id: &str) -> Result<(), String> {
        let mut projects = self.lock();
        let initial_len = projects.len();
        projects.retain(|p| p.id != id);
        if projects.len() == initial_len {
            return Err(format!("Project {} not found", id));
        }
        self.save(&projects)?;
        Ok(())
    }

    pub fn update_status(&self, id: &str, status: crate::models::ProjectStatus) -> Result<(), String> {
        let mut projects = self.lock();
        if let Some(p) = projects.iter_mut().find(|p| p.id == id) {
            p.status = status;
            p.updated_at = chrono::Utc::now().to_rfc3339();
            self.save(&projects)?;
            Ok(())
        } else {
            Err(format!("Project {} not found", id))
        }
    }

    /// Granular setter for the auth bridge opt-in, so toggling it can't clobber
    /// concurrent edits to the rest of the project record.
    pub fn set_auth_bridge_enabled(&self, project_id: &str, enabled: bool) -> Result<(), String> {
        let mut projects = self.lock();
        if let Some(p) = projects.iter_mut().find(|p| p.id == project_id) {
            p.auth_bridge_enabled = enabled;
            p.updated_at = chrono::Utc::now().to_rfc3339();
            self.save(&projects)?;
            Ok(())
        } else {
            Err(format!("Project {} not found", project_id))
        }
    }

    pub fn set_container_id(&self, project_id: &str, container_id: Option<String>) -> Result<(), String> {
        let mut projects = self.lock();
        if let Some(p) = projects.iter_mut().find(|p| p.id == project_id) {
            p.container_id = container_id;
            p.updated_at = chrono::Utc::now().to_rfc3339();
            self.save(&projects)?;
            Ok(())
        } else {
            Err(format!("Project {} not found", project_id))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "triple-c-store-{}-{}",
            tag,
            uuid::Uuid::new_v4().simple()
        ));
        fs::create_dir_all(&dir).expect("temp dir");
        dir
    }

    #[test]
    fn a_corrupt_load_leaves_a_marker_the_next_write_cannot_erase() {
        // H-3, the whole chain in one test. `ProjectsStore::new()` swallows an
        // unparseable file into an empty list *without rewriting it*, and the
        // first `save()` after that — as little as `update_status()` — writes
        // `[{one project}]` over it. Everything the old guard keyed on ("the
        // list is empty and the file exists") is gone at that point, while
        // every *other* project's volumes are still on the daemon claimed by
        // nobody.
        let dir = temp_dir("corrupt");
        let file = dir.join("projects.json");
        fs::write(&file, "{ this is not a project list").unwrap();

        let now = chrono::Utc::now();
        record_corrupt_load(&file, &now);

        let marker = corrupt_marker_for(&file);
        assert!(marker.exists(), "the corrupt load must be recorded on disk");
        assert_eq!(fs::read_to_string(&marker).unwrap(), now.to_rfc3339());
        assert!(
            dir.join("projects.json.bak").exists(),
            "the unreadable bytes must be kept"
        );

        // The write that used to erase the evidence. The marker is a separate
        // file, so it does not care.
        fs::write(&file, r#"[{"id":"the-one-project-started-since"}]"#).unwrap();
        assert!(marker.exists());

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_second_corruption_keeps_the_first_copy_and_the_first_date() {
        // The `.bak` used to be a fixed name, so a second corruption clobbered
        // the first — and the first is the only copy taken before the app
        // rewrote the file with whatever it had in memory, i.e. the only one
        // that can still hold the full project list.
        let dir = temp_dir("second");
        let file = dir.join("projects.json");
        fs::write(&file, "original bytes").unwrap();
        let first = chrono::DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        record_corrupt_load(&file, &first);

        fs::write(&file, "degraded bytes").unwrap();
        let second = chrono::DateTime::parse_from_rfc3339("2026-06-01T00:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        record_corrupt_load(&file, &second);

        assert_eq!(
            fs::read_to_string(dir.join("projects.json.bak")).unwrap(),
            "original bytes",
            "the first copy must survive the second corruption"
        );
        assert_eq!(
            fs::read_to_string(dir.join("projects.json.corrupt-20260601-000000.bak")).unwrap(),
            "degraded bytes"
        );
        // And the marker still dates the loss from the first failure, which is
        // when the project list actually stopped being complete.
        assert_eq!(
            fs::read_to_string(corrupt_marker_for(&file)).unwrap(),
            first.to_rfc3339()
        );

        fs::remove_dir_all(&dir).ok();
    }
}
