use std::fs;
use std::path::PathBuf;
use std::sync::Mutex;

use crate::models::Project;

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
                                    let backup = file_path.with_extension("json.bak");
                                    if let Err(be) = fs::copy(&file_path, &backup) {
                                        log::error!("Failed to back up corrupted projects.json: {}", be);
                                    }
                                    (Vec::new(), false)
                                }
                            }
                        }
                        Err(e) => {
                            log::error!("Failed to parse projects.json: {}. Starting with empty list.", e);
                            let backup = file_path.with_extension("json.bak");
                            if let Err(be) = fs::copy(&file_path, &backup) {
                                log::error!("Failed to back up corrupted projects.json: {}", be);
                            }
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

        let store = Self {
            projects: Mutex::new(projects),
            file_path,
        };

        // Persist migrated format back to disk
        if needs_save {
            log::info!("Migrated projects.json from single-path to multi-path format");
            let projects = store.lock();
            if let Err(e) = store.save(&projects) {
                log::error!("Failed to save migrated projects: {}", e);
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
