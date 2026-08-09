//! IPC surface for the browser view pane. The mechanism lives in
//! [`crate::browser_view`]; this file only translates between it and the
//! frontend.

use tauri::{AppHandle, State};

use crate::browser_view::{manager, BrowserViewStatus};
use crate::AppState;

/// Turn the pane on or off for a project.
///
/// Enabling probes the container and brings the viewer up when it can; a
/// container that isn't running, or one without Playwright, comes back as a
/// non-`Running` status carrying an explanation rather than an error, so the
/// pane always has something specific to say. This is host-side only — no
/// container recreation is involved either way.
#[tauri::command]
pub async fn set_browser_view_enabled(
    project_id: String,
    enabled: bool,
    app_handle: AppHandle,
    state: State<'_, AppState>,
) -> Result<BrowserViewStatus, String> {
    if !enabled {
        // Awaits the supervisor, so the host port is released before we return.
        manager().stop(&project_id).await;
        return Ok(manager().status(&project_id).await);
    }

    let project = state
        .projects_store
        .get(&project_id)
        .ok_or_else(|| format!("Project {} not found", project_id))?;

    let Some(container_id) = project.container_id.clone() else {
        return Err("Start the container before opening the browser view.".to_string());
    };
    if !crate::docker::container::is_container_running(&container_id)
        .await
        .unwrap_or(false)
    {
        return Err("Start the container before opening the browser view.".to_string());
    }

    manager()
        .start(
            project_id,
            container_id,
            app_handle,
            state.projects_store.clone(),
        )
        .await
}

/// Current status. Cheap: reads in-process state only, never the container.
#[tauri::command]
pub async fn get_browser_view_status(project_id: String) -> Result<BrowserViewStatus, String> {
    Ok(manager().status(&project_id).await)
}

/// Probe the container for Playwright without starting anything.
///
/// Lets the pane say "install this" before the user asks for a view, and lets
/// them re-check after installing without toggling the feature.
#[tauri::command]
pub async fn check_browser_view_support(
    project_id: String,
    state: State<'_, AppState>,
) -> Result<crate::browser_view::detect::PlaywrightDetection, String> {
    let project = state
        .projects_store
        .get(&project_id)
        .ok_or_else(|| format!("Project {} not found", project_id))?;
    let container_id = project
        .container_id
        .ok_or_else(|| "Start the container to check for Playwright.".to_string())?;
    crate::browser_view::detect::detect(&container_id).await
}
