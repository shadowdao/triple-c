use tauri::{AppHandle, Emitter, State};

use crate::AppState;

#[tauri::command]
pub async fn open_terminal_session(
    project_id: String,
    session_id: String,
    app_handle: AppHandle,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let project = state
        .projects_store
        .get(&project_id)
        .ok_or_else(|| format!("Project {} not found", project_id))?;

    let container_id = project
        .container_id
        .as_ref()
        .ok_or_else(|| "Container not running".to_string())?;

    let cmd = vec![
        "claude".to_string(),
        "--dangerously-skip-permissions".to_string(),
    ];

    let output_event = format!("terminal-output-{}", session_id);
    let exit_event = format!("terminal-exit-{}", session_id);
    let app_handle_output = app_handle.clone();
    let app_handle_exit = app_handle.clone();

    state
        .exec_manager
        .create_session(
            container_id,
            &session_id,
            cmd,
            move |data| {
                let _ = app_handle_output.emit(&output_event, data);
            },
            Box::new(move || {
                let _ = app_handle_exit.emit(&exit_event, ());
            }),
        )
        .await
}

#[tauri::command]
pub async fn terminal_input(
    session_id: String,
    data: Vec<u8>,
    state: State<'_, AppState>,
) -> Result<(), String> {
    state.exec_manager.send_input(&session_id, data).await
}

#[tauri::command]
pub async fn terminal_resize(
    session_id: String,
    cols: u16,
    rows: u16,
    state: State<'_, AppState>,
) -> Result<(), String> {
    state.exec_manager.resize(&session_id, cols, rows).await
}

#[tauri::command]
pub async fn close_terminal_session(
    session_id: String,
    state: State<'_, AppState>,
) -> Result<(), String> {
    state.exec_manager.close_session(&session_id).await;
    Ok(())
}

#[tauri::command]
pub async fn paste_image_to_terminal(
    session_id: String,
    image_data: Vec<u8>,
    state: State<'_, AppState>,
) -> Result<String, String> {
    let container_id = state.exec_manager.get_container_id(&session_id).await?;

    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let file_name = format!("clipboard_{}.png", timestamp);

    state
        .exec_manager
        .write_file_to_container(&container_id, &file_name, &image_data)
        .await
}
