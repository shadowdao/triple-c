use tauri::State;

use crate::models::Project;
use crate::AppState;

/// Resolve AWS profile: project-level → global settings → "default".
pub fn resolve_profile_for_project(project: &Project, global_profile: Option<&str>) -> String {
    project
        .bedrock_config
        .as_ref()
        .and_then(|b| b.aws_profile.clone())
        .or_else(|| global_profile.map(|s| s.to_string()))
        .unwrap_or_else(|| "default".to_string())
}

/// Check if the AWS session is valid for the given profile on the host.
/// Returns `Ok(true)` if valid, `Ok(false)` if expired/invalid.
pub async fn check_sso_session(profile: &str) -> Result<bool, String> {
    let output = tokio::process::Command::new("aws")
        .args(["sts", "get-caller-identity", "--profile", profile])
        .output()
        .await
        .map_err(|e| format!("Failed to run aws sts get-caller-identity: {}", e))?;
    Ok(output.status.success())
}

/// Check if the given AWS profile uses SSO (has sso_start_url or sso_session configured).
pub async fn is_sso_profile(profile: &str) -> Result<bool, String> {
    let check_start_url = tokio::process::Command::new("aws")
        .args(["configure", "get", "sso_start_url", "--profile", profile])
        .output()
        .await;
    if let Ok(out) = check_start_url {
        if out.status.success() {
            return Ok(true);
        }
    }
    let check_session = tokio::process::Command::new("aws")
        .args(["configure", "get", "sso_session", "--profile", profile])
        .output()
        .await;
    if let Ok(out) = check_session {
        if out.status.success() {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Run `aws sso login --profile X` on the host. This is interactive (opens a browser).
pub async fn run_sso_login(profile: &str) -> Result<(), String> {
    log::info!("Running host-side AWS SSO login for profile '{}'", profile);

    let status = tokio::process::Command::new("aws")
        .args(["sso", "login", "--profile", profile])
        .status()
        .await
        .map_err(|e| format!("Failed to run aws sso login: {}", e))?;

    if !status.success() {
        return Err("SSO login failed or was cancelled".to_string());
    }

    Ok(())
}

#[tauri::command]
pub async fn aws_sso_refresh(
    project_id: String,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let project = state.projects_store.get(&project_id)
        .ok_or_else(|| format!("Project {} not found", project_id))?;

    let profile = resolve_profile_for_project(
        &project,
        state.settings_store.get().global_aws.aws_profile.as_deref(),
    );

    run_sso_login(&profile).await
}
