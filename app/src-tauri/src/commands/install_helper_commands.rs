use crate::install_helper::{self, InstallOptions};

#[tauri::command]
pub async fn detect_install_options() -> Result<InstallOptions, String> {
    Ok(install_helper::detect_install_options())
}

#[tauri::command]
pub async fn run_docker_install(app_handle: tauri::AppHandle) -> Result<(), String> {
    install_helper::platform::run_install(&app_handle).await
}
