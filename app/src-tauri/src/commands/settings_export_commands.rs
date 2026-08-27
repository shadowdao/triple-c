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
//!
//! **This is new attack surface**: a settings export is a file one person
//! can hand another and ask them to import, together with a password, and
//! `apply_settings_import` applies whatever `AppSettings` it decrypts to
//! wholesale — see the module doc on `models::settings_export` for the
//! `web_terminal.access_token` carve-out a review of this feature found,
//! and treat that as the standing example of the class of thing to keep
//! checking for here, not a one-off fixed bug.

use std::path::{Path, PathBuf};

use tauri::State;
use tauri_plugin_dialog::DialogExt;
use zeroize::Zeroizing;

use crate::models::{
    AppSettings, ExportedSecrets, SettingsExportPayload, SettingsImportPreview,
    SETTINGS_EXPORT_FORMAT_VERSION,
};
use crate::storage::{secure, settings_crypto};
use crate::AppState;

const FILE_EXTENSION: &str = "triplec";

/// Enforced here, not only in the export modal: the frontend's minimum is a
/// UX nudge, but `export_settings` is the actual boundary a weak password
/// has to cross, and Argon2id's memory-hardness buys little against an
/// attacker who can just try a three-character password directly.
const MIN_PASSWORD_LEN: usize = 8;

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

/// Gather the current global secrets, and hand back the `AppSettings` to
/// export with the web-terminal token blanked out of it — see the module
/// doc comment on `models::settings_export` for why that field cannot
/// travel through `settings` like the rest of this struct.
///
/// A missing keychain secret reads as `None` — a keychain read failure is
/// treated as "nothing to export" for that one entry rather than aborting
/// the whole export, matching how the rest of this app degrades a keychain
/// error to "absent" (`has_claude_oauth_token`, `has_gateway_api_key`)
/// rather than surfacing it as a hard failure.
fn split_settings_and_secrets(current: AppSettings) -> (AppSettings, ExportedSecrets) {
    let mut settings = current;
    let web_terminal_access_token = settings.web_terminal.access_token.take();

    let secrets = ExportedSecrets {
        claude_oauth_token: secure::get_claude_oauth_token().unwrap_or_default(),
        gateway_api_key: secure::get_gateway_api_key().unwrap_or_default(),
        gateway_master_key: secure::get_gateway_master_key().unwrap_or_default(),
        web_terminal_access_token,
    };

    (settings, secrets)
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
    // `.chars().count()` — Unicode scalar values, not bytes — to stay as
    // close as this pair of languages allows to the frontend's `.length`
    // check (UTF-16 code units); the two only diverge on astral-plane
    // characters, which no reasonable password touches.
    if password.chars().count() < MIN_PASSWORD_LEN {
        return Err(format!(
            "Use a password of at least {} characters.",
            MIN_PASSWORD_LEN
        ));
    }

    let Some(dest) = pick_export_save_path(&window, &suggested_export_name()).await else {
        return Ok(false);
    };

    let (settings, secrets) = split_settings_and_secrets(state.settings_store.get());
    if secrets.is_empty() {
        log::info!("Exporting settings with no global secrets configured on this machine");
    }

    let payload = SettingsExportPayload {
        format_version: SETTINGS_EXPORT_FORMAT_VERSION,
        exported_at: chrono::Utc::now().to_rfc3339(),
        app_version: env!("CARGO_PKG_VERSION").to_string(),
        settings,
        secrets,
    };

    let plaintext = Zeroizing::new(
        serde_json::to_vec(&payload)
            .map_err(|e| format!("Failed to prepare settings for export: {}", e))?,
    );
    let encrypted = settings_crypto::encrypt(&plaintext, &password)?;

    std::fs::write(&dest, &encrypted).map_err(|e| format!("Failed to write export file: {}", e))?;

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
///
/// Order matters here, twice over.
///
/// First: the imported settings are **validated before any secret is
/// written**, using the same checks `update_settings` itself runs
/// (`settings_commands::validate_settings_update`). Restoring a secret is
/// hard to undo unnoticed — a stale env-var-name rejection or a disallowed
/// host path used to be caught only when `update_settings` ran, by which
/// point the three keychain secrets below were already overwritten with the
/// file's, each with a fresh rotation id, silently flagging every project
/// container for recreation — while the error the user saw talked only
/// about the rejected setting and said nothing about the credentials that
/// had already moved. Failing this check first makes a rejected import
/// leave nothing touched, matching what "the import failed" is supposed to
/// mean.
///
/// Second, among the things that *do* get written: secrets are restored
/// **before** the settings replace runs (which is what triggers
/// `reconcile_gateway`), so a gateway recreation that replace provokes sees
/// the final key material rather than racing it — restoring the other way
/// round left a real window where the running gateway and the keychain
/// briefly disagreed.
///
/// The pending path is only cleared on success. A failure here (rejected by
/// the validation above, or some other error) leaves the import pending so
/// the frontend can let the user retry `apply` without making them pick the
/// file and re-enter the password again — the preview's job was confirming
/// *what* to import, not spending the one attempt at applying it.
#[tauri::command]
pub async fn apply_settings_import(
    password: String,
    state: State<'_, AppState>,
) -> Result<AppSettings, String> {
    if password.is_empty() {
        return Err("A password is required to import settings.".to_string());
    }

    let path = state
        .pending_settings_import
        .lock()
        .await
        .clone()
        .ok_or_else(|| "No import is pending — choose a file first.".to_string())?;

    let payload = read_and_decrypt(&path, &password)?;

    let current = state.settings_store.get();

    // The web-terminal token lives inside `AppSettings` itself rather than
    // the keychain, so "leave an absent secret alone" has to be done by
    // hand here: carry the destination's current token forward when the
    // import doesn't have one, instead of letting the wholesale replace
    // below blank it (every export writes `None` there — see
    // `split_settings_and_secrets`).
    let mut settings = payload.settings;
    settings.web_terminal.access_token = non_blank(payload.secrets.web_terminal_access_token)
        .or_else(|| current.web_terminal.access_token.clone());

    crate::commands::settings_commands::validate_settings_update(&current, &settings)?;

    if let Some(token) = non_blank(payload.secrets.claude_oauth_token) {
        if let Err(e) = secure::store_claude_oauth_token(&token) {
            log::warn!(
                "Settings import: could not restore the shared Claude login: {}",
                e
            );
        }
    }
    if let Some(key) = non_blank(payload.secrets.gateway_api_key) {
        if let Err(e) = secure::store_gateway_api_key(&key) {
            log::warn!(
                "Settings import: could not restore the gateway provider API key: {}",
                e
            );
        }
    }
    if let Some(key) = non_blank(payload.secrets.gateway_master_key) {
        if let Err(e) = secure::store_gateway_master_key(&key) {
            log::warn!(
                "Settings import: could not restore the gateway master key: {}",
                e
            );
        }
    }

    let saved =
        crate::commands::settings_commands::update_settings(settings, state.clone()).await?;

    state.pending_settings_import.lock().await.take();

    Ok(saved)
}

fn non_blank(value: Option<String>) -> Option<String> {
    value.filter(|v| !v.trim().is_empty())
}

/// Only the field `read_and_decrypt` needs before deciding whether the rest
/// of the payload is even worth attempting to parse.
#[derive(serde::Deserialize)]
struct FormatVersionProbe {
    format_version: u32,
}

/// Decrypt and parse an export file, checking the format version **before**
/// attempting to deserialize the full payload.
///
/// That ordering is not just tidiness: a version bump that isn't
/// deserialize-compatible (a field's type changes, not just a new
/// `#[serde(default)]`-covered one) is exactly the case this check exists
/// for, and parsing the full struct first would fail on the shape mismatch
/// before the version check ever ran, surfacing a raw parse error instead
/// of "update Triple-C" — and, more seriously, `serde_json`'s type-mismatch
/// errors quote the offending value inline. This file is not attacker
/// content in the usual sense (it must still decrypt under the right
/// password), but the plaintext it decrypts to can hold a live credential,
/// so neither error path below ever interpolates what `serde_json`
/// actually says — only a fixed, generic message.
fn read_and_decrypt(path: &Path, password: &str) -> Result<SettingsExportPayload, String> {
    let encrypted =
        std::fs::read(path).map_err(|e| format!("Failed to read export file: {}", e))?;
    let plaintext = settings_crypto::decrypt(&encrypted, password)?;

    let probe: FormatVersionProbe = serde_json::from_slice(&plaintext)
        .map_err(|_| "This file doesn't look like a valid settings export.".to_string())?;
    if probe.format_version > SETTINGS_EXPORT_FORMAT_VERSION {
        return Err(format!(
            "This export was made by a newer version of Triple-C (format {}, this app supports up to {}). \
             Update Triple-C before importing it.",
            probe.format_version, SETTINGS_EXPORT_FORMAT_VERSION
        ));
    }

    serde_json::from_slice(&plaintext).map_err(|_| {
        "This file doesn't look like a valid settings export (unexpected shape).".to_string()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_blank_treats_whitespace_only_as_absent() {
        assert_eq!(non_blank(Some("   ".to_string())), None);
        assert_eq!(non_blank(Some("".to_string())), None);
        assert_eq!(non_blank(None), None);
        assert_eq!(non_blank(Some(" a ".to_string())), Some(" a ".to_string()));
    }

    fn write_export(
        dir: &std::path::Path,
        name: &str,
        payload: &SettingsExportPayload,
        password: &str,
    ) -> PathBuf {
        write_raw_export(dir, name, &serde_json::to_value(payload).unwrap(), password)
    }

    /// Like `write_export`, but takes an arbitrary `serde_json::Value` rather
    /// than a real `SettingsExportPayload` — for fixtures that are
    /// deliberately not shape-compatible, which the typed helper above can't
    /// produce at all.
    fn write_raw_export(
        dir: &std::path::Path,
        name: &str,
        value: &serde_json::Value,
        password: &str,
    ) -> PathBuf {
        let plaintext = serde_json::to_vec(value).unwrap();
        let encrypted = settings_crypto::encrypt(&plaintext, password).unwrap();
        let path = dir.join(name);
        std::fs::write(&path, &encrypted).unwrap();
        path
    }

    #[test]
    fn splitting_settings_moves_the_web_terminal_token_out_rather_than_copying_it() {
        let mut settings = AppSettings::default();
        settings.web_terminal.access_token = Some("super-secret-token".to_string());

        let (settings, secrets) = split_settings_and_secrets(settings);

        assert_eq!(settings.web_terminal.access_token, None);
        assert_eq!(
            secrets.web_terminal_access_token,
            Some("super-secret-token".to_string())
        );
    }

    #[test]
    fn splitting_settings_with_no_token_leaves_it_absent_on_both_sides() {
        let (settings, secrets) = split_settings_and_secrets(AppSettings::default());

        assert_eq!(settings.web_terminal.access_token, None);
        assert_eq!(secrets.web_terminal_access_token, None);
    }

    fn sample_payload(format_version: u32) -> SettingsExportPayload {
        SettingsExportPayload {
            format_version,
            exported_at: "2026-08-27T00:00:00Z".to_string(),
            app_version: "0.4.14".to_string(),
            settings: AppSettings::default(),
            secrets: ExportedSecrets::default(),
        }
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "triple-c-settings-export-test-{}-{}",
            name,
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_file_from_a_newer_format_is_refused_before_the_full_shape_is_parsed() {
        // Shape-incompatible with the *current* `SettingsExportPayload` (a
        // future version could easily have changed `settings` from an object
        // to something else) as well as newer — so this only passes under
        // the probe-first ordering. Parsing the full struct first (the old
        // behavior) would fail on the shape mismatch and never reach the
        // version check, producing the "unexpected shape" message instead of
        // "newer version" / "Update Triple-C".
        let dir = temp_dir("newer-format");
        let path = write_raw_export(
            &dir,
            "export.triplec",
            &serde_json::json!({
                "format_version": SETTINGS_EXPORT_FORMAT_VERSION + 1,
                "exported_at": "2026-08-27T00:00:00Z",
                "app_version": "9.9.9",
                "settings": "this-app-version-stores-settings-differently",
                "secrets": {},
            }),
            "correct password",
        );

        let err = read_and_decrypt(&path, "correct password").unwrap_err();
        assert!(err.contains("newer version"), "unexpected message: {}", err);
        assert!(err.contains("Update Triple-C"));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_file_at_the_current_format_is_accepted() {
        let dir = temp_dir("current-format");
        let path = write_export(
            &dir,
            "export.triplec",
            &sample_payload(SETTINGS_EXPORT_FORMAT_VERSION),
            "correct password",
        );

        let payload = read_and_decrypt(&path, "correct password").unwrap();
        assert_eq!(payload.format_version, SETTINGS_EXPORT_FORMAT_VERSION);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_malformed_payload_produces_a_generic_error_not_a_raw_serde_message() {
        // A `format_version` the probe accepts, but a `settings` field of
        // the wrong *type* rather than just a missing field — this is what
        // makes `serde_json` produce an "invalid type: string `...`, expected
        // struct AppSettings" error that quotes the offending value
        // verbatim. That value here stands in for plaintext that, in a real
        // export, could be a live credential — the assertion below is only
        // meaningful against a fixture that actually exercises serde's
        // value-quoting behavior, which a merely-missing-field fixture does
        // not.
        let dir = temp_dir("malformed");
        let path = write_raw_export(
            &dir,
            "export.triplec",
            &serde_json::json!({
                "format_version": SETTINGS_EXPORT_FORMAT_VERSION,
                "exported_at": "2026-08-27T00:00:00Z",
                "app_version": "0.4.14",
                "settings": "NOT-A-REAL-CREDENTIAL-abc123",
                "secrets": {},
            }),
            "correct password",
        );

        let err = read_and_decrypt(&path, "correct password").unwrap_err();
        assert!(
            !err.contains("NOT-A-REAL-CREDENTIAL-abc123"),
            "leaked plaintext into the error: {}",
            err
        );
        assert!(err.contains("doesn't look like a valid settings export"));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_wrong_password_is_reported_without_a_version_check_ever_running() {
        let dir = temp_dir("wrong-password");
        let path = write_export(
            &dir,
            "export.triplec",
            &sample_payload(SETTINGS_EXPORT_FORMAT_VERSION),
            "correct password",
        );

        let err = read_and_decrypt(&path, "wrong password").unwrap_err();
        assert!(
            err.contains("Wrong password"),
            "unexpected message: {}",
            err
        );

        std::fs::remove_dir_all(&dir).ok();
    }
}
