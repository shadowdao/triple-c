use tauri::State;

use crate::docker;
use crate::models::AppSettings;
use crate::AppState;

#[tauri::command]
pub async fn get_settings(state: State<'_, AppState>) -> Result<AppSettings, String> {
    Ok(state.settings_store.get())
}

#[tauri::command]
pub async fn update_settings(
    settings: AppSettings,
    state: State<'_, AppState>,
) -> Result<AppSettings, String> {
    state.settings_store.update(settings)
}

#[tauri::command]
pub async fn pull_image(
    image_name: String,
    app_handle: tauri::AppHandle,
) -> Result<(), String> {
    use tauri::Emitter;
    docker::pull_image(&image_name, move |msg| {
        let _ = app_handle.emit("image-pull-progress", msg);
    })
    .await
}

#[tauri::command]
pub async fn detect_aws_config() -> Result<Option<String>, String> {
    if let Some(home) = dirs::home_dir() {
        let aws_dir = home.join(".aws");
        if aws_dir.exists() {
            return Ok(Some(aws_dir.to_string_lossy().to_string()));
        }
    }
    Ok(None)
}

#[tauri::command]
pub async fn list_aws_profiles() -> Result<Vec<String>, String> {
    let mut profiles = Vec::new();

    let home = match dirs::home_dir() {
        Some(h) => h,
        None => return Ok(profiles),
    };

    // Parse ~/.aws/credentials
    let credentials_path = home.join(".aws").join("credentials");
    if credentials_path.exists() {
        if let Ok(contents) = std::fs::read_to_string(&credentials_path) {
            for line in contents.lines() {
                let trimmed = line.trim();
                if trimmed.starts_with('[') && trimmed.ends_with(']') {
                    let profile = trimmed[1..trimmed.len() - 1].to_string();
                    if !profiles.contains(&profile) {
                        profiles.push(profile);
                    }
                }
            }
        }
    }

    // Parse ~/.aws/config (profiles are prefixed with "profile ")
    let config_path = home.join(".aws").join("config");
    if config_path.exists() {
        if let Ok(contents) = std::fs::read_to_string(&config_path) {
            for line in contents.lines() {
                let trimmed = line.trim();
                if trimmed.starts_with('[') && trimmed.ends_with(']') {
                    let section = &trimmed[1..trimmed.len() - 1];
                    let profile = if let Some(name) = section.strip_prefix("profile ") {
                        name.to_string()
                    } else {
                        section.to_string()
                    };
                    if !profiles.contains(&profile) {
                        profiles.push(profile);
                    }
                }
            }
        }
    }

    Ok(profiles)
}
