use tauri::{AppHandle, Emitter, State};

use crate::commands::aws_commands;
use crate::models::{Backend, BedrockAuthMethod, Project};
use crate::AppState;

/// Build the command to run in the container terminal.
///
/// For Bedrock Profile projects, wraps `claude` in a bash script that validates
/// the AWS session first. If the SSO session is expired, runs `aws sso login`
/// so the user can re-authenticate (the URL is clickable via xterm.js WebLinksAddon).
fn build_terminal_cmd(project: &Project, state: &AppState, session_name: Option<&str>) -> Vec<String> {
    let is_bedrock_profile = project.backend == Backend::Bedrock
        && project
            .bedrock_config
            .as_ref()
            .map(|b| b.auth_method == BedrockAuthMethod::Profile)
            .unwrap_or(false);

    let permission_args = project.effective_permission_mode().cli_args();

    if !is_bedrock_profile {
        let mut cmd = vec!["claude".to_string()];
        cmd.extend(permission_args);
        if let Some(name) = session_name {
            if !name.is_empty() {
                cmd.push("-n".to_string());
                cmd.push(name.to_string());
            }
        }
        return cmd;
    }

    let profile = aws_commands::resolve_profile_for_project(
        project,
        state.settings_store.get().global_aws.aws_profile.as_deref(),
    );

    // Build a bash wrapper that validates credentials, re-auths if needed,
    // then exec's into claude.
    let name_flag = session_name
        .filter(|n| !n.is_empty())
        .map(|n| format!(" -n '{}'", n.replace('\'', "'\\''")))
        .unwrap_or_default();
    // The args are interpolated into a shell script string, so single-quote
    // each one (same escaping style as name_flag above).
    let permission_flags: String = permission_args
        .iter()
        .map(|a| format!(" '{}'", a.replace('\'', "'\\''")))
        .collect();
    let claude_cmd = format!("exec claude{}{}", permission_flags, name_flag);

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
{claude_cmd}
"#,
        profile = profile,
        claude_cmd = claude_cmd
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
    session_name: Option<String>,
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
        _ => build_terminal_cmd(&project, &state, session_name.as_deref()),
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

/// Copy a host file (e.g. dragged onto the terminal) into the container so
/// Claude Code can read it, and return the in-container path. Mirrors the
/// image-paste flow: the file is placed under /tmp/triple-c-drops/ keeping its
/// original name. Returns an error for paths that aren't readable regular files
/// (e.g. a dropped directory).
#[tauri::command]
pub async fn upload_host_file_to_terminal(
    session_id: String,
    host_path: String,
    state: State<'_, AppState>,
) -> Result<String, String> {
    // The drop target is a host path chosen by the webview, not by the OS drag
    // itself, so it gets the same host-read policy as the Files pane's upload:
    // absolute, no traversal, and nothing out of a hidden directory
    // (`~/.ssh`, `~/.aws`) or a system location — applied to the path with its
    // symlinks already resolved, so a visible directory that *leads* to `~/.ssh`
    // is refused too. What comes back is that resolved path, and it is what
    // gets opened.
    // The name is taken from the path the user actually dropped, *before*
    // resolution. Deriving it from the resolved path renames the file behind
    // the user's back: dropping `~/Downloads/latest.log`, where `latest.log` is
    // a symlink, would land it in the container as `2026-08-23.log`. The Files
    // pane's upload had the same bug and fixes it the same way — one helper, so
    // the two drop targets cannot drift.
    let base = crate::commands::file_commands::host_upload_name(&host_path)?;
    let host_path = crate::commands::file_commands::resolve_host_read_path(&host_path).await?;

    let container_id = state.exec_manager.get_container_id(&session_id).await?;

    let meta = tokio::fs::metadata(&host_path)
        .await
        .map_err(|e| format!("Cannot access {}: {}", host_path, e))?;
    if meta.is_dir() {
        return Err(format!("{} is a directory — drop individual files", host_path));
    }

    // Guard against ballooning host RAM: the file is packed into an in-memory
    // tar before upload, so cap the size of a dropped file. The ceiling lives
    // with the code that does the reading, which re-applies it to the open
    // descriptor — this check is here only so the refusal reads like a sentence
    // instead of arriving after a 300 MB read.
    use crate::docker::exec::MAX_DROP_BYTES;
    if meta.len() > MAX_DROP_BYTES {
        return Err(format!(
            "File too large to drop into the terminal ({:.0} MB; limit {} MB). Mount it into the project or use the Files panel instead.",
            meta.len() as f64 / (1024.0 * 1024.0),
            MAX_DROP_BYTES / (1024 * 1024)
        ));
    }



    // Ensure the destination directory exists rather than relying on Docker's
    // archive extractor to create the parent for the uploaded tar entry.
    crate::docker::exec::exec_oneshot(
        &container_id,
        vec!["mkdir".to_string(), "-p".to_string(), "/tmp/triple-c-drops".to_string()],
    )
    .await?;

    let file_name = format!("triple-c-drops/{}", base);
    crate::docker::exec::upload_host_file_to_container(&container_id, &host_path, &file_name).await
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

#[cfg(test)]
mod tests {
    /// Both drop targets must name a dropped file the way the *user* named it.
    ///
    /// The bug this pins: `upload_host_file_to_terminal` derived the tar entry
    /// name from the path *after* symlink resolution, so dropping
    /// `~/Downloads/latest.log` — where `latest.log` is a symlink to
    /// `2026-08-23.log` — silently landed the file in the container under the
    /// target's name. Nothing errored; the user just got a name they never
    /// typed. The Files pane had the identical bug.
    ///
    /// What actually keeps the two from drifting is that they now call one
    /// helper, so this asserts that helper's contract from the terminal side:
    /// the answer comes from the spelling, and a path that does not name a file
    /// is refused rather than silently substituted (it used to fall back to
    /// `"dropped-file"`).
    #[test]
    fn a_dropped_file_keeps_the_name_the_user_dropped() {
        use crate::commands::file_commands::host_upload_name;

        assert_eq!(
            host_upload_name("/home/u/Downloads/latest.log").unwrap(),
            "latest.log"
        );
        assert!(
            host_upload_name("/home/u/Downloads/").is_err(),
            "a directory is not a file to drop"
        );
        assert!(
            host_upload_name("/home/u/..").is_err(),
            "the name becomes a tar entry, a container path and an argv element"
        );
    }
}
