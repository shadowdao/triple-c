use tauri::State;

use crate::docker;
use crate::models::{container_config, ContainerInfo};
use crate::AppState;

#[tauri::command]
pub async fn check_docker() -> Result<bool, String> {
    docker::check_docker_available().await
}

#[tauri::command]
pub async fn check_image_exists(state: State<'_, AppState>) -> Result<bool, String> {
    let settings = state.settings_store.get();
    let image_name = container_config::resolve_image_name(&settings.image_source, &settings.custom_image_name);
    docker::image_exists(&image_name).await
}

#[tauri::command]
pub async fn build_image(app_handle: tauri::AppHandle) -> Result<(), String> {
    use tauri::Emitter;
    docker::build_image(move |msg| {
        let _ = app_handle.emit("image-build-progress", msg);
    })
    .await
}

#[tauri::command]
pub async fn get_container_info(
    project_id: String,
    state: State<'_, AppState>,
) -> Result<Option<ContainerInfo>, String> {
    let project = state
        .projects_store
        .get(&project_id)
        .ok_or_else(|| format!("Project {} not found", project_id))?;
    docker::get_container_info(&project).await
}

#[tauri::command]
pub async fn list_sibling_containers() -> Result<Vec<serde_json::Value>, String> {
    let containers = docker::list_sibling_containers().await?;
    let result: Vec<serde_json::Value> = containers
        .into_iter()
        .map(|c| {
            serde_json::json!({
                "id": c.id,
                "names": c.names,
                "image": c.image,
                "state": c.state,
                "status": c.status,
            })
        })
        .collect();
    Ok(result)
}
