//! IPC surface for the auth bridge. The mechanism lives in
//! [`crate::auth_bridge`]; this file only translates between it and the
//! frontend, and keeps the persisted per-project flag in step.

use tauri::{AppHandle, State};

use crate::auth_bridge::AuthBridgeStatus;
use crate::AppState;

/// Turn the bridge on or off for a project and return the resulting status.
///
/// Enabling starts polling immediately when the container is already running;
/// otherwise the flag is simply persisted and `start_project_container` arms the
/// bridge on the next start. This is a host-side feature, so no container
/// recreation is involved either way.
#[tauri::command]
pub async fn set_auth_bridge_enabled(
    project_id: String,
    enabled: bool,
    app_handle: AppHandle,
    state: State<'_, AppState>,
) -> Result<AuthBridgeStatus, String> {
    state
        .projects_store
        .set_auth_bridge_enabled(&project_id, enabled)?;

    if enabled {
        let project = state
            .projects_store
            .get(&project_id)
            .ok_or_else(|| format!("Project {} not found", project_id))?;
        if let Some(container_id) = project.container_id {
            if crate::docker::container::is_container_running(&container_id)
                .await
                .unwrap_or(false)
            {
                state
                    .auth_bridge
                    .start(
                        project_id.clone(),
                        container_id,
                        app_handle,
                        state.projects_store.clone(),
                    )
                    .await;
            }
        }
    } else {
        // Awaits the poller, so every host port is released before we return.
        state.auth_bridge.stop(&project_id).await;
    }

    Ok(state.auth_bridge.status(&project_id, enabled).await)
}

#[tauri::command]
pub async fn get_auth_bridge_status(
    project_id: String,
    state: State<'_, AppState>,
) -> Result<AuthBridgeStatus, String> {
    let enabled = state
        .projects_store
        .get(&project_id)
        .map(|p| p.auth_bridge_enabled)
        .unwrap_or(false);
    Ok(state.auth_bridge.status(&project_id, enabled).await)
}
