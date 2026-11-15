use std::fs;
use std::path::PathBuf;
use std::sync::Mutex;

use crate::models::AppSettings;

pub struct SettingsStore {
    settings: Mutex<AppSettings>,
    file_path: PathBuf,
}

impl SettingsStore {
    pub fn new() -> Result<Self, String> {
        let data_dir = dirs::data_dir()
            .ok_or_else(|| "Could not determine data directory. Set XDG_DATA_HOME on Linux.".to_string())?
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

        Ok(Self {
            settings: Mutex::new(settings),
            file_path,
        })
    }

    /// A store over an explicit file, for tests (no data-dir lookup).
    #[cfg(test)]
    pub(crate) fn at_path(file_path: PathBuf) -> Self {
        Self { settings: Mutex::new(AppSettings::default()), file_path }
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
        // Save first: a failed save must not leave the unsaved copy in memory.
        self.save(&new_settings)?;
        *settings = new_settings.clone();
        Ok(new_settings)
    }

    /// Read-modify-save under the store's lock, so concurrent callers cannot
    /// lose each other's updates. `f` edits a copy; if it fails, or the save
    /// fails, memory and disk are unchanged. Returns the saved settings and
    /// whatever `f` returned.
    pub fn modify<R, F>(&self, f: F) -> Result<(AppSettings, R), String>
    where
        F: FnOnce(&mut AppSettings) -> Result<R, String>,
    {
        let mut settings = self.lock();
        let mut copy = settings.clone();
        let out = f(&mut copy)?;
        self.save(&copy)?;
        *settings = copy.clone();
        Ok((copy, out))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modify_saves_and_returns_the_closure_result() {
        let dir = tempfile::tempdir().unwrap();
        let store = SettingsStore::at_path(dir.path().join("settings.json"));
        let (saved, n) = store
            .modify(|s| {
                s.hosts.clear();
                Ok(7)
            })
            .unwrap();
        assert_eq!(n, 7);
        assert_eq!(
            serde_json::to_value(&saved).unwrap(),
            serde_json::to_value(store.get()).unwrap()
        );
        assert!(dir.path().join("settings.json").exists());
    }

    #[test]
    fn modify_does_not_change_memory_when_save_fails() {
        let dir = tempfile::tempdir().unwrap();
        let store = SettingsStore::at_path(dir.path().join("missing/settings.json"));
        let before = serde_json::to_value(store.get()).unwrap();
        let err = store.modify(|s| {
            s.hosts.push(crate::models::HostConnection {
                id: "x".into(),
                name: "x".into(),
                kind: crate::models::HostKind::Aws {
                    auth: crate::models::AwsAuth::Profile { name: "p".into() },
                    default_region: "us-west-2".into(),
                },
            });
            Ok(())
        });
        assert!(err.is_err());
        assert_eq!(serde_json::to_value(store.get()).unwrap(), before);
    }

    #[test]
    fn modify_does_not_save_when_the_closure_fails() {
        let dir = tempfile::tempdir().unwrap();
        let store = SettingsStore::at_path(dir.path().join("settings.json"));
        let err = store.modify(|_| -> Result<(), String> { Err("no".into()) }).unwrap_err();
        assert_eq!(err, "no");
        assert!(!dir.path().join("settings.json").exists());
    }
}
