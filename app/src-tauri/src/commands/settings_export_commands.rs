//! Settings export/import — see triple-c#35.
//!
//! Exports the *host* environment (global `AppSettings` plus the global
//! secrets kept in the OS keychain: the shared Claude Code OAuth login and
//! the model gateway's two keys), encrypted with a user-chosen password —
//! see `storage::settings_crypto` for the actual cryptography. Deliberately
//! out of scope: per-project settings, per-project secrets, and anything
//! living in a project's Docker volumes.
//!
//! **The save/open dialogs are opened from Rust**, the same pattern
//! `file_commands.rs`'s `pick_save_path`/`pick_files_to_upload` already
//! establish and document at length: a frontend-driven dialog handing Rust a
//! host path string is the exact shape of bug that produced this app's past
//! criticals, so the boundary here is drawn the same place. The frontend can
//! ask for a picker; it cannot name a host path as an *input*. `preview_
//! settings_import` resolves the chosen path itself and remembers it
//! (`AppState::pending_settings_import`) so `apply_settings_import` re-reads
//! the same file without the path ever crossing back over IPC.
//!
//! The password is re-entered (not cached) between preview and apply, so
//! that nothing here holds decrypted plaintext — export/import secrets
//! included — in memory for longer than one command's execution.

use std::path::PathBuf;

use tauri::State;
use tauri_plugin_dialog::DialogExt;

use crate::models::{
    ExportedSecrets, SettingsExportPayload, SettingsImportPreview, SETTINGS_EXPORT_FORMAT_VERSION,
};
use crate::storage::{secure, settings_crypto};
use crate::AppState;

const FILE_EXTENSION: &str = "triplec";

fn suggested_export_name() -> String {
    // Timestamped so exporting more than once doesn't silently overwrite an
    // earlier file just because the save dialog defaults to the same name.
    format!(
        "triple-c-settings-{}.{}",
        chrono::Utc::now().format("%Y%m%d-%H%M%S"),
        FILE_EXTENSION
    )
}

async fn pick_export_save_path(window: &tauri::Window, suggested: &str) -> Option<PathBuf> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    window
        .dialog()
        .file()
        .set_parent(window)
        .set_title("Export Triple-C settings")
        .set_file_name(suggested)
        .add_filter("Triple-C settings export", &[FILE_EXTENSION])
        .save_file(move |picked| {
            let _ = tx.send(picked);
        });
    rx.await.ok().flatten().and_then(|p| p.into_path().ok())
}

async fn pick_import_open_path(window: &tauri::Window) -> Option<PathBuf> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    window
        .dialog()
        .file()
        .set_parent(window)
        .set_title("Import Triple-C settings")
        .add_filter("Triple-C settings export", &[FILE_EXTENSION])
        .pick_file(move |picked| {
            let _ = tx.send(picked);
        });
    rx.await.ok().flatten().and_then(|p| p.into_path().ok())
}

/// Gather the current global secrets. A missing secret reads as `None` — a
/// keychain read failure is treated as "nothing to export" for that one
/// entry rather than aborting the whole export, matching how the rest of
/// this app degrades a keychain error to "absent" (`has_claude_oauth_token`,
/// `has_gateway_api_key`) rather than surfacing it as a hard failure.
fn gather_secrets() -> ExportedSecrets {
    ExportedSecrets {
        claude_oauth_token: secure::get_claude_oauth_token().unwrap_or_default(),
        gateway_api_key: secure::get_gateway_api_key().unwrap_or_default(),
        gateway_master_key: secure::get_gateway_master_key().unwrap_or_default(),
    }
}

/// Export the current global settings and secrets to a password-encrypted
/// file. `Ok(false)` means the save dialog was dismissed — not an error, and
/// deliberately distinguishable from one so the frontend shows nothing
/// rather than a "failed" toast for a plain cancel.
#[tauri::command]
pub async fn export_settings(
    password: String,
    window: tauri::Window,
    state: State<'_, AppState>,
) -> Result<bool, String> {
    if password.is_empty() {
        return Err("A password is required to export settings.".to_string());
    }

    let Some(dest) = pick_export_save_path(&window, &suggested_export_name()).await else {
        return Ok(false);
    };

    let secrets = gather_secrets();
    if secrets.is_empty() {
        log::info!("Exporting settings with no global secrets configured on this machine");
    }

    let payload = SettingsExportPayload {
        format_version: SETTINGS_EXPORT_FORMAT_VERSION,
        exported_at: chrono::Utc::now().to_rfc3339(),
        app_version: env!("CARGO_PKG_VERSION").to_string(),
        settings: state.settings_store.get(),
        secrets,
    };

    let plaintext = serde_json::to_vec(&payload)
        .map_err(|e| format!("Failed to prepare settings for export: {}", e))?;
    let encrypted = settings_crypto::encrypt(&plaintext, &password)?;

    std::fs::write(&dest, encrypted).map_err(|e| format!("Failed to write export file: {}", e))?;

    Ok(true)
}

/// Open a file picker, decrypt the chosen file with `password`, and return a
/// preview (counts and presence flags only — never a secret value) for a
/// confirmation UI. `Ok(None)` means the picker was dismissed.
///
/// Remembers the resolved path in `AppState::pending_settings_import` for
/// `apply_settings_import` to re-read; does **not** remember the decrypted
/// payload itself, so the password must be supplied again to actually apply
/// it — seeing the preview is not the same as committing to it.
#[tauri::command]
pub async fn preview_settings_import(
    password: String,
    window: tauri::Window,
    state: State<'_, AppState>,
) -> Result<Option<SettingsImportPreview>, String> {
    if password.is_empty() {
        return Err("A password is required to open a settings export.".to_string());
    }

    let Some(path) = pick_import_open_path(&window).await else {
        return Ok(None);
    };

    let payload = read_and_decrypt(&path, &password)?;
    let preview = SettingsImportPreview::from_payload(&payload);

    *state.pending_settings_import.lock().await = Some(path);

    Ok(Some(preview))
}

/// Apply the import a prior `preview_settings_import` call resolved a path
/// for. Fails if no preview is pending — this is not a general "decrypt and
/// apply this file" entry point, deliberately: seeing the preview first is
/// required, not just encouraged, since it is the only place a user is told
/// what an import is about to touch before it touches it.
///
/// Global settings are replaced wholesale — an import is "restore this
/// environment," not a field-by-field merge. Global secrets are handled
/// differently and on purpose: **only secrets actually present in the
/// import are written**; a secret the export doesn't have is left alone on
/// this machine rather than cleared, because an absent secret in the export
/// means "the source machine never had this configured," not "delete this
/// on import." A user who wants to clear a secret already has dedicated UI
/// for that (signing out of shared auth, clearing the gateway key).
#[tauri::command]
pub async fn apply_settings_import(
    password: String,
    state: State<'_, AppState>,
) -> Result<crate::models::AppSettings, String> {
    if password.is_empty() {
        return Err("A password is required to import settings.".to_string());
    }

    let path = state
        .pending_settings_import
        .lock()
        .await
        .take()
        .ok_or_else(|| "No import is pending — choose a file first.".to_string())?;

    let payload = read_and_decrypt(&path, &password)?;

    let saved = crate::commands::settings_commands::update_settings(payload.settings, state).await?;

    if let Some(token) = non_blank(payload.secrets.claude_oauth_token) {
        if let Err(e) = secure::store_claude_oauth_token(&token) {
            log::warn!("Settings import: could not restore the shared Claude login: {}", e);
        }
    }
    if let Some(key) = non_blank(payload.secrets.gateway_api_key) {
        if let Err(e) = secure::store_gateway_api_key(&key) {
            log::warn!("Settings import: could not restore the gateway provider API key: {}", e);
        }
    }
    if let Some(key) = non_blank(payload.secrets.gateway_master_key) {
        if let Err(e) = secure::store_gateway_master_key(&key) {
            log::warn!("Settings import: could not restore the gateway master key: {}", e);
        }
    }

    Ok(saved)
}

fn non_blank(value: Option<String>) -> Option<String> {
    value.filter(|v| !v.trim().is_empty())
}

fn read_and_decrypt(path: &std::path::Path, password: &str) -> Result<SettingsExportPayload, String> {
    let encrypted = std::fs::read(path).map_err(|e| format!("Failed to read export file: {}", e))?;
    let plaintext = settings_crypto::decrypt(&encrypted, password)?;

    let payload: SettingsExportPayload = serde_json::from_slice(&plaintext)
        .map_err(|e| format!("This file doesn't look like a valid settings export: {}", e))?;

    if payload.format_version > SETTINGS_EXPORT_FORMAT_VERSION {
        return Err(format!(
            "This export was made by a newer version of Triple-C (format {}, this app supports up to {}). \
             Update Triple-C before importing it.",
            payload.format_version, SETTINGS_EXPORT_FORMAT_VERSION
        ));
    }

    Ok(payload)
}
