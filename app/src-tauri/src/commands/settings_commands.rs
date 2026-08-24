use tauri::State;

use crate::docker;
use crate::models::gateway_settings::GatewaySettings;
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
    let before = state.settings_store.get();

    // The global half of the same rule the project half gets in
    // `update_project`: a global custom env var is merged into every project's
    // container environment, so an unchecked name here reaches all of them.
    crate::models::validate_env_vars_update(
        &before.global_custom_env_vars,
        &settings.global_custom_env_vars,
    )?;

    let saved = state.settings_store.update(settings)?;

    // Persisting a setting is not the same as applying it. The gateway is the
    // one settings block that owns a *container*, so a saved change that the
    // running container doesn't reflect is a live desync, not a preference.
    reconcile_gateway(&before.gateway, &saved.gateway).await;

    Ok(saved)
}

/// What a settings save has to do to the gateway container to stay honest.
///
/// Kept separate from the IPC command and expressed over plain settings so the
/// decision is testable without Docker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GatewayAction {
    /// Nothing to do.
    None,
    /// The gateway is off — a container left running must be stopped.
    StopIfRunning,
    /// The published shape moved. A *running* container is now serving on the
    /// old binding while status reports the new one, so it has to be recreated.
    RestartIfRunning,
}

/// Whether the container's published shape (as opposed to a purely cosmetic
/// field) changed. Provider, models and base URL all change the rendered
/// LiteLLM config, which is only read at boot.
fn gateway_shape_changed(before: &GatewaySettings, after: &GatewaySettings) -> bool {
    before.port != after.port
        || before.provider.trim() != after.provider.trim()
        || before.api_base.as_deref().unwrap_or("").trim()
            != after.api_base.as_deref().unwrap_or("").trim()
        || before.valid_models() != after.valid_models()
}

fn gateway_action(before: &GatewaySettings, after: &GatewaySettings) -> GatewayAction {
    if !after.enabled {
        // Includes the case where it was already disabled: a container found
        // running while the feature is off should not stay up.
        return GatewayAction::StopIfRunning;
    }
    if gateway_shape_changed(before, after) {
        return GatewayAction::RestartIfRunning;
    }
    GatewayAction::None
}

/// Apply [`gateway_action`]. Never fails the settings save: the settings *are*
/// saved by this point, and a Docker hiccup must not make the UI think they
/// weren't. Both paths are no-ops when no container exists, so this stays cheap
/// on the overwhelmingly common "gateway not in use" save.
async fn reconcile_gateway(before: &GatewaySettings, after: &GatewaySettings) {
    let action = gateway_action(before, after);
    if action == GatewayAction::None {
        return;
    }

    let (exists, running) = match docker::gateway::gateway_container_presence().await {
        Ok(presence) => presence,
        // Docker down: there is nothing running to desync from.
        Err(e) => {
            log::debug!("Gateway reconcile skipped ({})", e);
            return;
        }
    };
    if !exists || !running {
        return;
    }

    match action {
        GatewayAction::StopIfRunning => {
            log::info!("Model gateway disabled in settings — stopping the container");
            if let Err(e) = docker::gateway::stop_gateway_container().await {
                log::error!("Failed to stop the model gateway after it was disabled: {}", e);
            }
        }
        GatewayAction::RestartIfRunning => {
            log::info!("Model gateway settings changed — recreating the container");
            // The fingerprint no longer matches, so this stops, removes and
            // recreates with the new port/config in one step.
            if let Err(e) = docker::gateway::ensure_gateway_running(after).await {
                log::error!("Failed to apply the new model gateway settings: {}", e);
            }
        }
        GatewayAction::None => unreachable!(),
    }
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
pub async fn detect_host_timezone() -> Result<String, String> {
    // Try the iana-time-zone crate first (cross-platform)
    match iana_time_zone::get_timezone() {
        Ok(tz) => return Ok(tz),
        Err(e) => log::debug!("iana_time_zone::get_timezone() failed: {}", e),
    }

    // Fallback: check TZ env var
    if let Ok(tz) = std::env::var("TZ") {
        if !tz.is_empty() {
            return Ok(tz);
        }
    }

    // Fallback: read /etc/timezone (Linux)
    if let Ok(tz) = std::fs::read_to_string("/etc/timezone") {
        let tz = tz.trim().to_string();
        if !tz.is_empty() {
            return Ok(tz);
        }
    }

    // Default to UTC if detection fails
    Ok("UTC".to_string())
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

/// What the UI shows next to a corporate CA certificate path.
///
/// Errors are returned *inside* the payload rather than as `Err` so the field
/// can render its own inline message while the user is still typing — a toast
/// per keystroke would be unusable. The same check runs again, as a hard error,
/// when the container is created.
#[derive(Debug, serde::Serialize)]
pub struct CaCertInfo {
    pub exists: bool,
    pub is_directory: bool,
    /// How many certificate files were found.
    pub cert_count: usize,
    /// The names they will be installed as inside the container. Surfacing
    /// these makes the silent `.pem` → `.crt` rename visible, which is the one
    /// step users most often do by hand and get wrong.
    pub installed_names: Vec<String>,
    /// Why the path is unusable, if it is.
    pub error: Option<String>,
}

#[tauri::command]
pub async fn inspect_ca_cert_path(path: String) -> Result<CaCertInfo, String> {
    use crate::docker::ca_certs;

    let trimmed = path.trim();
    if trimmed.is_empty() {
        return Ok(CaCertInfo {
            exists: false,
            is_directory: false,
            cert_count: 0,
            installed_names: Vec::new(),
            error: None,
        });
    }

    let p = std::path::Path::new(trimmed);
    let exists = p.exists();
    let is_directory = p.is_dir();

    match ca_certs::resolve(Some(trimmed)) {
        Ok(Some(resolved)) => Ok(CaCertInfo {
            exists,
            is_directory,
            cert_count: resolved.cert_files.len(),
            installed_names: resolved
                .cert_files
                .iter()
                .map(|f| {
                    ca_certs::container_cert_name(
                        &f.file_name().unwrap_or_default().to_string_lossy(),
                    )
                })
                .collect(),
            error: None,
        }),
        Ok(None) => Ok(CaCertInfo {
            exists,
            is_directory,
            cert_count: 0,
            installed_names: Vec::new(),
            error: None,
        }),
        Err(e) => Ok(CaCertInfo {
            exists,
            is_directory,
            cert_count: 0,
            installed_names: Vec::new(),
            error: Some(e),
        }),
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::gateway_settings::GatewayModel;

    fn enabled_gateway() -> GatewaySettings {
        GatewaySettings {
            enabled: true,
            port: 4000,
            provider: "openai".to_string(),
            api_base: None,
            models: vec![GatewayModel {
                name: "gpt-5.1".to_string(),
                model_id: "gpt-5.1".to_string(),
            }],
        }
    }

    #[test]
    fn disabling_the_gateway_stops_it() {
        // The bug: turning the toggle off only persisted `enabled: false` and
        // hid the Stop button, leaving a container serving with no way to stop
        // it.
        let before = enabled_gateway();
        let mut after = before.clone();
        after.enabled = false;
        assert_eq!(gateway_action(&before, &after), GatewayAction::StopIfRunning);
        // Still true when it was already off — a stray running container is
        // still a container that shouldn't be up.
        assert_eq!(gateway_action(&after, &after), GatewayAction::StopIfRunning);
    }

    #[test]
    fn changing_the_port_reconciles_the_container() {
        // Otherwise status reports the new port while the container keeps the
        // old binding, and every project gets a broken ANTHROPIC_BASE_URL.
        let before = enabled_gateway();
        let mut after = before.clone();
        after.port = 4100;
        assert_eq!(
            gateway_action(&before, &after),
            GatewayAction::RestartIfRunning
        );
    }

    #[test]
    fn config_changes_that_only_take_effect_at_boot_reconcile_too() {
        let before = enabled_gateway();

        let mut provider = before.clone();
        provider.provider = "groq".to_string();
        assert_eq!(
            gateway_action(&before, &provider),
            GatewayAction::RestartIfRunning
        );

        let mut api_base = before.clone();
        api_base.api_base = Some("https://example.test/v1".to_string());
        assert_eq!(
            gateway_action(&before, &api_base),
            GatewayAction::RestartIfRunning
        );

        let mut models = before.clone();
        models.models[0].model_id = "gpt-4.1".to_string();
        assert_eq!(
            gateway_action(&before, &models),
            GatewayAction::RestartIfRunning
        );
    }

    #[test]
    fn saving_an_unchanged_or_half_typed_gateway_touches_nothing() {
        let before = enabled_gateway();
        assert_eq!(gateway_action(&before, &before), GatewayAction::None);

        // Whitespace-only edits don't reach the rendered config.
        let mut trimmed = before.clone();
        trimmed.provider = "  openai  ".to_string();
        trimmed.api_base = Some("   ".to_string());
        assert_eq!(gateway_action(&before, &trimmed), GatewayAction::None);

        // A half-filled model row is skipped when rendering, so it must not
        // bounce a live container either.
        let mut half_typed = before.clone();
        half_typed.models.push(GatewayModel {
            name: "gpt".to_string(),
            model_id: String::new(),
        });
        assert_eq!(gateway_action(&before, &half_typed), GatewayAction::None);
    }
}
