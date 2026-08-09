//! Tauri commands for the model gateway container.
//!
//! Mirrors `stt_commands`. The one rule that is specific to this module: the
//! **provider API key never crosses back to the frontend**. It goes in through
//! `set_gateway_api_key`, lives in the OS keychain, and is only ever read
//! host-side when rendering the gateway config. `get_gateway_status` reports
//! its presence as a boolean.
//!
//! The gateway *master key* is different and is returned deliberately — it is
//! the value the user has to paste into a project's model config as its auth
//! token, so keeping it hidden would just make the feature unusable.

use tauri::{AppHandle, Emitter, State};

use crate::docker::gateway;
use crate::models::GatewayStatus;
use crate::storage::secure;
use crate::AppState;

#[tauri::command]
pub async fn get_gateway_status(state: State<'_, AppState>) -> Result<GatewayStatus, String> {
    let settings = state.settings_store.get();
    gateway::get_gateway_status(&settings.gateway).await
}

#[tauri::command]
pub async fn start_gateway(state: State<'_, AppState>) -> Result<GatewayStatus, String> {
    let settings = state.settings_store.get();
    gateway::ensure_gateway_running(&settings.gateway).await
}

#[tauri::command]
pub async fn stop_gateway() -> Result<(), String> {
    gateway::stop_gateway_container().await
}

/// Whether the gateway is actually answering yet. LiteLLM needs a few seconds
/// after the container starts before `/v1/messages` will serve anything.
#[tauri::command]
pub async fn check_gateway_health(state: State<'_, AppState>) -> Result<bool, String> {
    let settings = state.settings_store.get();
    gateway::check_gateway_health(settings.gateway.port).await
}

#[tauri::command]
pub async fn build_gateway_image(app_handle: AppHandle) -> Result<(), String> {
    gateway::build_gateway_image(move |msg| {
        let _ = app_handle.emit("gateway-build-progress", &msg);
    })
    .await
}

#[tauri::command]
pub async fn pull_gateway_image(app_handle: AppHandle) -> Result<(), String> {
    gateway::pull_gateway_image(move |msg| {
        let _ = app_handle.emit("gateway-pull-progress", &msg);
    })
    .await
}

/// Store the upstream provider API key. Write-only from the frontend's point
/// of view — there is no matching getter.
#[tauri::command]
pub async fn set_gateway_api_key(api_key: String) -> Result<(), String> {
    secure::store_gateway_api_key(&api_key)
}

/// Forget the provider API key. The gateway keeps serving until it is
/// restarted, at which point it will refuse to start without a key.
#[tauri::command]
pub async fn clear_gateway_api_key() -> Result<(), String> {
    secure::delete_gateway_api_key()
}

/// The token a project sends to the gateway (`ANTHROPIC_AUTH_TOKEN`), minting
/// one on first use.
#[tauri::command]
pub async fn get_gateway_auth_token() -> Result<String, String> {
    secure::get_or_create_gateway_master_key()
}

/// Mint a new gateway auth token, invalidating the old one. Projects still
/// holding the previous value stop working until they are updated, and the
/// gateway is recreated on its next start because the rotation id moved.
#[tauri::command]
pub async fn regenerate_gateway_auth_token() -> Result<String, String> {
    secure::regenerate_gateway_master_key()
}
