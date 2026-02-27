use std::fs;
use std::path::PathBuf;
use std::sync::Mutex;

use crate::models::AppSettings;

pub struct SettingsStore {
    settings: Mutex<AppSettings>,
    file_path: PathBuf,
}

impl SettingsStore {
    pub fn new() -> Self {
        let data_dir = dirs::data_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("triple-c");

        fs::create_dir_all(&data_dir).ok();

        let file_path = data_dir.join("settings.json");

        let settings = if file_path.exists() {
            match fs::read_to_string(&file_path) {
                Ok(data) => match serde_json::from_str(&data) {
                    Ok(parsed) => parsed,
                    Err(e) => {
                        log::error!("Failed to parse settings.json: {}. Using defaults.", e);
                        let backup = file_path.with_extension("json.bak");
                        if let Err(be) = fs::copy(&file_path, &backup) {
                            log::error!("Failed to back up corrupted settings.json: {}", be);
                        }
                        AppSettings::default()
                    }
                },
                Err(e) => {
                    log::error!("Failed to read settings.json: {}", e);
                    AppSettings::default()
                }
            }
        } else {
            AppSettings::default()
        };

        Self {
            settings: Mutex::new(settings),
            file_path,
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, AppSettings> {
        self.settings.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn save(&self, settings: &AppSettings) -> Result<(), String> {
        let data = serde_json::to_string_pretty(settings)
            .map_err(|e| format!("Failed to serialize settings: {}", e))?;

        let tmp_path = self.file_path.with_extension("json.tmp");
        fs::write(&tmp_path, data)
            .map_err(|e| format!("Failed to write temp settings file: {}", e))?;
        fs::rename(&tmp_path, &self.file_path)
            .map_err(|e| format!("Failed to rename settings file: {}", e))?;
        Ok(())
    }

    pub fn get(&self) -> AppSettings {
        self.lock().clone()
    }

    pub fn update(&self, new_settings: AppSettings) -> Result<AppSettings, String> {
        let mut settings = self.lock();
        *settings = new_settings.clone();
        self.save(&settings)?;
        Ok(new_settings)
    }
}
