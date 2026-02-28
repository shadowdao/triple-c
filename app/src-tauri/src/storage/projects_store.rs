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

        let projects = if file_path.exists() {
            match fs::read_to_string(&file_path) {
                Ok(data) => match serde_json::from_str(&data) {
                    Ok(parsed) => parsed,
                    Err(e) => {
                        log::error!("Failed to parse projects.json: {}. Starting with empty list.", e);
                        // Back up the corrupted file
                        let backup = file_path.with_extension("json.bak");
                        if let Err(be) = fs::copy(&file_path, &backup) {
                            log::error!("Failed to back up corrupted projects.json: {}", be);
                        }
                        Vec::new()
                    }
                },
                Err(e) => {
                    log::error!("Failed to read projects.json: {}", e);
                    Vec::new()
                }
            }
        } else {
            Vec::new()
        };

        Ok(Self {
            projects: Mutex::new(projects),
            file_path,
        })
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
