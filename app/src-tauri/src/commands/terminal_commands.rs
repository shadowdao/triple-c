use tauri::{AppHandle, Emitter, State};

use crate::models::{Backend, BedrockAuthMethod, Project};
use crate::AppState;

/// Build the command to run in the container terminal.
///
/// For Bedrock Profile projects, wraps `claude` in a bash script that validates
/// the AWS session first. If the SSO session is expired, runs `aws sso login`
/// so the user can re-authenticate (the URL is clickable via xterm.js WebLinksAddon).
fn build_terminal_cmd(project: &Project, state: &AppState) -> Vec<String> {
    let is_bedrock_profile = project.backend == Backend::Bedrock
        && project
            .bedrock_config
            .as_ref()
            .map(|b| b.auth_method == BedrockAuthMethod::Profile)
            .unwrap_or(false);

    if !is_bedrock_profile {
        return vec![
            "claude".to_string(),
            "--dangerously-skip-permissions".to_string(),
        ];
    }

    // Resolve AWS profile: project-level → global settings → "default"
    let profile = project
        .bedrock_config
        .as_ref()
        .and_then(|b| b.aws_profile.clone())
        .or_else(|| state.settings_store.get().global_aws.aws_profile.clone())
        .unwrap_or_else(|| "default".to_string());

    // Build a bash wrapper that validates credentials, re-auths if needed,
    // then exec's into claude.
    let script = format!(
        r#"
echo "Validating AWS session for profile '{profile}'..."
if aws sts get-caller-identity --profile '{profile}' >/dev/null 2>&1; then
    echo "AWS session valid."
else
    echo "AWS session expired or invalid."
    # Check if this profile uses SSO (has sso_start_url or sso_session configured)
    if aws configure get sso_start_url --profile '{profile}' >/dev/null 2>&1 || \
       aws configure get sso_session --profile '{profile}' >/dev/null 2>&1; then
        echo "Starting SSO login..."
        echo ""
        triple-c-sso-refresh
        if [ $? -ne 0 ]; then
            echo ""
            echo "SSO login failed or was cancelled. Starting Claude anyway..."
            echo "You may see authentication errors."
            echo ""
        fi
    else
        echo "Profile '{profile}' does not use SSO. Check your AWS credentials."
        echo "Starting Claude anyway..."
        echo ""
    fi
fi
exec claude --dangerously-skip-permissions
"#,
        profile = profile
    );

    vec![
        "bash".to_string(),
        "-c".to_string(),
        script,
    ]
}

#[tauri::command]
pub async fn open_terminal_session(
    project_id: String,
    session_id: String,
    session_type: Option<String>,
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

    let cmd = match session_type.as_deref() {
        Some("bash") => vec!["bash".to_string(), "-l".to_string()],
        _ => build_terminal_cmd(&project, &state),
    };

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
    // Close audio bridge if it exists
    let audio_session_id = format!("audio-{}", session_id);
    state.exec_manager.close_session(&audio_session_id).await;
    // Close terminal session
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

#[tauri::command]
pub async fn start_audio_bridge(
    session_id: String,
    state: State<'_, AppState>,
) -> Result<(), String> {
    // Get container_id from the terminal session
    let container_id = state.exec_manager.get_container_id(&session_id).await?;

    // Create audio bridge exec session with ID "audio-{session_id}"
    // The loop handles reconnection when the FIFO reader (fake rec) is killed and restarted
    let audio_session_id = format!("audio-{}", session_id);
    let cmd = vec![
        "bash".to_string(),
        "-c".to_string(),
        "FIFO=/tmp/triple-c-audio-input; [ -p \"$FIFO\" ] || mkfifo \"$FIFO\"; trap '' PIPE; while true; do cat > \"$FIFO\" 2>/dev/null; sleep 0.1; done".to_string(),
    ];

    state
        .exec_manager
        .create_session_with_tty(
            &container_id,
            &audio_session_id,
            cmd,
            false,
            |_data| { /* ignore output from the audio bridge */ },
            Box::new(|| { /* no exit handler needed */ }),
        )
        .await
}

#[tauri::command]
pub async fn send_audio_data(
    session_id: String,
    data: Vec<u8>,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let audio_session_id = format!("audio-{}", session_id);
    state.exec_manager.send_input(&audio_session_id, data).await
}

#[tauri::command]
pub async fn stop_audio_bridge(
    session_id: String,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let audio_session_id = format!("audio-{}", session_id);
    state.exec_manager.close_session(&audio_session_id).await;
    Ok(())
}
