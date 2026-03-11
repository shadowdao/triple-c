use tauri::State;
use crate::AppState;

#[tauri::command]
pub async fn aws_sso_refresh(
    project_id: String,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let project = state.projects_store.get(&project_id)
        .ok_or_else(|| format!("Project {} not found", project_id))?;

    let profile = project.bedrock_config.as_ref()
        .and_then(|b| b.aws_profile.clone())
        .or_else(|| state.settings_store.get().global_aws.aws_profile.clone())
        .unwrap_or_else(|| "default".to_string());

    log::info!("Running host-side AWS SSO login for profile '{}'", profile);

    let status = tokio::process::Command::new("aws")
        .args(["sso", "login", "--profile", &profile])
        .status()
        .await
        .map_err(|e| format!("Failed to run aws sso login: {}", e))?;

    if !status.success() {
        return Err("SSO login failed or was cancelled".to_string());
    }

    Ok(())
}
