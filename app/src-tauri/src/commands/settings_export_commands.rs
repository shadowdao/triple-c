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
//! The *decrypted payload* is not cached between preview and apply — the
//! password the frontend passes to each call is what it already held for
//! the first, not a fresh secret extracted from the user, but nothing here
//! keeps the plaintext itself — export/import secrets included — around for
//! longer than one command's execution; `apply_settings_import` re-decrypts
//! the file rather than reusing anything `preview_settings_import` computed.
//!
//! **This is new attack surface**: a settings export is a file one person
//! can hand another and ask them to import, together with a password, and
//! `apply_settings_import` applies whatever `AppSettings` it decrypts to
//! wholesale — see the module doc on `models::settings_export` for the
//! `web_terminal.access_token` carve-out a review of this feature found,
//! and treat that as the standing example of the class of thing to keep
//! checking for here, not a one-off fixed bug.

#[cfg(test)]
use std::path::Path;
use std::path::PathBuf;

use sha2::{Digest, Sha256};
use tauri::State;
use tauri_plugin_dialog::DialogExt;
use zeroize::Zeroizing;

use std::collections::BTreeMap;

use crate::models::host_connection::{AwsAuth, HostConnection, HostKind};
use crate::models::marketplace::{AccountMethod, MarketplaceAccount};
use crate::models::{
    AppSettings, ExportedSecrets, SettingsExportPayload, SettingsImportOutcome,
    SettingsImportPreview, SETTINGS_EXPORT_FORMAT_VERSION,
};
use crate::storage::{secure, settings_crypto};
use crate::AppState;

/// What `preview_settings_import` pins so `apply_settings_import` can tell
/// whether the file it's about to re-read is the same one the user actually
/// saw a preview of. Confirming a preview is only meaningful if it's binding
/// on what gets applied — without this, a file replaced on disk between the
/// two calls (this app's own stated threat model is a file shared between
/// people, which may sit in a synced or shared directory) would decrypt and
/// apply silently different content than what the confirmation dialog showed.
#[derive(Debug, Clone)]
pub struct PendingSettingsImport {
    path: PathBuf,
    ciphertext_hash: [u8; 32],
}

fn hash_ciphertext(data: &[u8]) -> [u8; 32] {
    Sha256::digest(data).into()
}

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
        marketplace_account_tokens: exported_marketplace_tokens(
            &settings.marketplace_accounts,
            secure::get_marketplace_token,
        ),
        aws_connection_keys: exported_aws_connection_keys(&settings.hosts, secure::get_aws_keys),
    };

    (settings, secrets)
}

/// The stored token of every marketplace account that has one, by account
/// id. A `GhHost` account stores none (its token is asked of the host's `gh`
/// each time), so it is not read. A missing or unreadable token is left out,
/// like the other keychain secrets above.
fn exported_marketplace_tokens(
    accounts: &[MarketplaceAccount],
    get: impl Fn(&str) -> Result<Option<String>, String>,
) -> BTreeMap<String, String> {
    accounts
        .iter()
        .filter(|a| a.method != AccountMethod::GhHost)
        .filter_map(|a| {
            let token = non_blank(get(&a.id).unwrap_or_default())?;
            Some((a.id.clone(), token))
        })
        .collect()
}

/// Write each imported marketplace token to the keychain, returning a
/// warning (never containing the token) for each one that could not be.
fn restore_marketplace_tokens(
    tokens: &BTreeMap<String, String>,
    mut store: impl FnMut(&str, &str) -> Result<(), String>,
) -> Vec<String> {
    let mut warnings = Vec::new();
    for (account_id, token) in tokens {
        if let Err(e) = store(account_id, token) {
            log::warn!(
                "Settings import: could not restore the token of marketplace account {}: {}",
                account_id,
                e
            );
            warnings.push(format!(
                "Could not restore a marketplace account's token ({}); sign that account in again.",
                e
            ));
        }
    }
    warnings
}

/// The stored keys of every `Keys` host connection, by connection id, as the
/// `AwsKeys` JSON. A `Profile` connection holds none. A missing or unreadable
/// entry is left out, like the other keychain secrets above.
fn exported_aws_connection_keys(
    hosts: &[HostConnection],
    get: impl Fn(&str) -> Result<Option<secure::AwsKeys>, String>,
) -> BTreeMap<String, String> {
    hosts
        .iter()
        .filter(|h| matches!(&h.kind, HostKind::Aws { auth: AwsAuth::Keys { .. }, .. }))
        .filter_map(|h| {
            let keys = get(&h.id).unwrap_or_default()?;
            Some((h.id.clone(), serde_json::to_string(&keys).ok()?))
        })
        .collect()
}

/// Write each imported set of AWS keys to the keychain, returning a warning
/// (naming the connection id, never the value) for each that could not be.
fn restore_aws_connection_keys(
    keys: &BTreeMap<String, String>,
    mut store: impl FnMut(&str, &secure::AwsKeys) -> Result<(), String>,
) -> Vec<String> {
    let mut warnings = Vec::new();
    for (id, json) in keys {
        if json.trim().is_empty() {
            continue;
        }
        let Ok(parsed) = serde_json::from_str::<secure::AwsKeys>(json) else {
            log::warn!("Settings import: the AWS keys of connection {} are malformed", id);
            warnings.push(format!(
                "The AWS access keys of connection {} in the file are malformed; enter them again.",
                id
            ));
            continue;
        };
        if let Err(e) = store(id, &parsed) {
            log::warn!(
                "Settings import: could not restore the AWS keys of connection {}: {}",
                id,
                e
            );
            warnings.push(format!(
                "Could not restore the AWS access keys of connection {} ({}); enter them again.",
                id, e
            ));
        }
    }
    warnings
}

/// Checks the imported host connections with the rules the connection
/// commands use, before anything is written; refuses the import (like
/// `validate_imported_marketplace_state`) rather than applying a bad entry.
/// A `Keys` connection's `keyring_ref` must be its own (valid) id, and keys
/// may only be carried for a `Keys` connection in the file.
fn validate_imported_hosts(
    settings: &mut AppSettings,
    keys: &BTreeMap<String, String>,
) -> Result<(), String> {
    use crate::models::host_connection::{
        validate_connection_name, validate_profile_name, validate_supported_region,
    };
    let wrap = |e: String| format!("The file's host connections were refused: {e}");

    let mut seen = std::collections::HashSet::new();
    for h in &mut settings.hosts {
        if !secure::is_valid_connection_id(&h.id) {
            return Err(wrap("a connection has an invalid id.".into()));
        }
        if !seen.insert(h.id.clone()) {
            return Err(wrap("a connection appears twice.".into()));
        }
        h.name = validate_connection_name(&h.name).map_err(wrap)?;
        let HostKind::Aws { auth, default_region } = &h.kind;
        validate_supported_region(default_region).map_err(wrap)?;
        match auth {
            AwsAuth::Profile { name } => {
                validate_profile_name(name).map_err(wrap)?;
            }
            AwsAuth::Keys { keyring_ref } => {
                if !secure::is_valid_connection_id(keyring_ref) || *keyring_ref != h.id {
                    return Err(wrap(format!(
                        "the connection \"{}\" has an invalid key reference.",
                        h.name
                    )));
                }
            }
        }
    }
    for id in keys.keys() {
        let is_keys_connection = settings.hosts.iter().any(|h| {
            h.id == *id && matches!(&h.kind, HostKind::Aws { auth: AwsAuth::Keys { .. }, .. })
        });
        if !is_keys_connection {
            return Err(wrap("access keys are present for an unknown connection.".into()));
        }
    }
    Ok(())
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
/// Remembers the resolved path *and a hash of the file's ciphertext* in
/// `AppState::pending_settings_import` for `apply_settings_import` to check
/// against — does **not** remember the decrypted payload itself, so the
/// password must be supplied again to actually apply it — seeing the preview
/// is not the same as committing to it. The hash exists so it also can't be
/// swapped out from under that commitment: `apply_settings_import` refuses to
/// proceed if the file on disk no longer matches what was just previewed.
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

    let encrypted = std::fs::read(&path).map_err(|e| format!("Failed to read export file: {}", e))?;
    let payload = read_and_decrypt_bytes(&encrypted, &password)?;
    let local_hosts = state.settings_store.get().hosts;
    let preview = SettingsImportPreview::from_payload_against(&payload, &local_hosts, secure::aws_keys_entry_state);

    *state.pending_settings_import.lock().await = Some(PendingSettingsImport {
        path,
        ciphertext_hash: hash_ciphertext(&encrypted),
    });

    Ok(Some(preview))
}

/// Apply the import a prior `preview_settings_import` call resolved a path
/// for. Fails if no preview is pending — this is not a general "decrypt and
/// apply this file" entry point, deliberately: seeing the preview first is
/// required, not just encouraged, since it is the only place a user is told
/// what an import is about to touch before it touches it. That requirement
/// is only real if the file can't change out from under it, so this also
/// refuses to proceed if the file's ciphertext no longer matches the hash
/// `preview_settings_import` pinned — a file replaced on disk between the
/// two calls (this feature's own threat model is a file shared between
/// people, which may sit in a synced or shared directory) must not be able
/// to apply silently different content than what the confirmation dialog
/// showed.
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
/// briefly disagreed. A gateway *secret* alone (same shape, new key) is
/// invisible to `reconcile_gateway`'s shape comparison, so this additionally
/// nudges a running gateway container to recreate itself whenever a secret
/// this import carried was actually written — otherwise the running
/// container keeps serving the old key material indefinitely while every
/// project container is handed the new one.
///
/// A keychain write failing is reported back rather than only logged: an
/// import that silently restores two of three secrets but not the third
/// must not read as unqualified success.
///
/// The pending import is only cleared on success. A failure here (rejected
/// by the validation above, a stale-file mismatch, or some other error)
/// leaves it pending so the frontend can let the user retry `apply` without
/// making them pick the file and re-enter the password again — the
/// preview's job was confirming *what* to import, not spending the one
/// attempt at applying it.
#[tauri::command]
pub async fn apply_settings_import(
    password: String,
    state: State<'_, AppState>,
) -> Result<SettingsImportOutcome, String> {
    if password.is_empty() {
        return Err("A password is required to import settings.".to_string());
    }

    let pending = state
        .pending_settings_import
        .lock()
        .await
        .clone()
        .ok_or_else(|| "No import is pending — choose a file first.".to_string())?;

    let encrypted = std::fs::read(&pending.path)
        .map_err(|e| format!("Failed to read export file: {}", e))?;
    if hash_ciphertext(&encrypted) != pending.ciphertext_hash {
        return Err(
            "This file changed since you reviewed it — choose it again to see an up-to-date preview."
                .to_string(),
        );
    }
    let payload = read_and_decrypt_bytes(&encrypted, &password)?;

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
    // The marketplace half, with the commands' own rules and normalisation,
    // also before anything is written (pre-flight F10).
    let marketplace_tokens = payload.secrets.marketplace_account_tokens;
    crate::commands::marketplace_commands::validate_imported_marketplace_state(
        &mut settings,
        &marketplace_tokens,
    )?;

    let aws_keys = payload.secrets.aws_connection_keys;
    validate_imported_hosts(&mut settings, &aws_keys)?;

    let mut secret_restore_warnings = Vec::new();
    let mut gateway_secret_changed = false;

    if let Some(token) = non_blank(payload.secrets.claude_oauth_token) {
        if let Err(e) = secure::store_claude_oauth_token(&token) {
            log::warn!(
                "Settings import: could not restore the shared Claude login: {}",
                e
            );
            secret_restore_warnings
                .push(format!("Could not restore your shared Claude login: {}", e));
        }
    }
    if let Some(key) = non_blank(payload.secrets.gateway_api_key) {
        match secure::store_gateway_api_key(&key) {
            Ok(()) => gateway_secret_changed = true,
            Err(e) => {
                log::warn!(
                    "Settings import: could not restore the gateway provider API key: {}",
                    e
                );
                secret_restore_warnings.push(format!(
                    "Could not restore the gateway provider API key: {}",
                    e
                ));
            }
        }
    }
    if let Some(key) = non_blank(payload.secrets.gateway_master_key) {
        match secure::store_gateway_master_key(&key) {
            Ok(()) => gateway_secret_changed = true,
            Err(e) => {
                log::warn!(
                    "Settings import: could not restore the gateway master key: {}",
                    e
                );
                secret_restore_warnings
                    .push(format!("Could not restore the gateway master key: {}", e));
            }
        }
    }

    secret_restore_warnings.extend(restore_marketplace_tokens(
        &marketplace_tokens,
        secure::store_marketplace_token,
    ));

    secret_restore_warnings.extend(restore_aws_connection_keys(
        &aws_keys,
        secure::store_aws_keys,
    ));

    let imported_owned = StoreOwned::of(&settings);
    let saved =
        crate::commands::settings_commands::update_settings(settings, state.clone()).await?;
    // `update_settings` keeps marketplace state and host connections
    // store-owned. An import is the one caller entitled to replace them
    // wholesale.
    let saved = state
        .settings_store
        .update(imported_owned.applied_to(saved))?;
    // Caches of marketplaces the import dropped are dead weight now, and
    // the pins must match the imported installs.
    use crate::commands::marketplace_commands as mc;
    for id in mc::dropped_marketplace_ids(&current, &saved) {
        mc::remove_cache(&state, &id).await;
    }
    mc::refresh_pins(&state).await;

    // `reconcile_gateway` (inside `update_settings`) only reacts to a changed
    // *shape* — port, provider, base URL, models — because that's what's
    // rendered into the container's config. A secret changing with the shape
    // held constant is invisible to it, so a running gateway container would
    // otherwise keep serving the old key material forever after an import
    // that restored a new one, while `docker::gateway`'s own fingerprint
    // (which does include the secret rotation id) means the *next* unrelated
    // settings save would suddenly and confusingly recreate it instead.
    if gateway_secret_changed && saved.gateway.enabled {
        match crate::docker::gateway::gateway_container_presence().await {
            Ok((true, true)) => {
                if let Err(e) = crate::docker::gateway::ensure_gateway_running(&saved.gateway).await
                {
                    log::error!(
                        "Settings import: could not apply the restored gateway credentials to the running gateway container: {}",
                        e
                    );
                }
            }
            Ok(_) => {}
            Err(e) => log::debug!("Settings import: gateway reconcile skipped ({})", e),
        }
    }

    state.pending_settings_import.lock().await.take();

    Ok(SettingsImportOutcome {
        settings: saved,
        secret_restore_warnings,
    })
}

/// The settings `update_settings` keeps store-owned (marketplace state and
/// host connections), which an import re-applies after that call.
struct StoreOwned {
    marketplace_accounts: Vec<crate::models::marketplace::MarketplaceAccount>,
    marketplaces: Vec<crate::models::marketplace::Marketplace>,
    global_marketplace_installs: Vec<crate::models::marketplace::MarketplaceInstall>,
    hosts: Vec<crate::models::HostConnection>,
}

impl StoreOwned {
    fn of(s: &AppSettings) -> Self {
        Self {
            marketplace_accounts: s.marketplace_accounts.clone(),
            marketplaces: s.marketplaces.clone(),
            global_marketplace_installs: s.global_marketplace_installs.clone(),
            hosts: s.hosts.clone(),
        }
    }

    fn applied_to(self, mut s: AppSettings) -> AppSettings {
        s.marketplace_accounts = self.marketplace_accounts;
        s.marketplaces = self.marketplaces;
        s.global_marketplace_installs = self.global_marketplace_installs;
        s.hosts = self.hosts;
        s
    }
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

/// Read and decrypt an export file at `path`, then parse it — see
/// `read_and_decrypt_bytes` for why the format-version check runs before the
/// full parse. Every real caller already has the file's bytes in hand by the
/// time it needs this (`preview_settings_import`/`apply_settings_import`
/// both hash the ciphertext first) and calls `read_and_decrypt_bytes`
/// directly to avoid reading the file twice; this path-based wrapper only
/// exists now for tests that don't need that.
#[cfg(test)]
fn read_and_decrypt(path: &Path, password: &str) -> Result<SettingsExportPayload, String> {
    let encrypted =
        std::fs::read(path).map_err(|e| format!("Failed to read export file: {}", e))?;
    read_and_decrypt_bytes(&encrypted, password)
}

/// Decrypt and parse an already-read export file's bytes, checking the
/// format version **before** attempting to deserialize the full payload.
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
fn read_and_decrypt_bytes(encrypted: &[u8], password: &str) -> Result<SettingsExportPayload, String> {
    let plaintext = settings_crypto::decrypt(encrypted, password)?;

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

    #[test]
    fn ciphertext_hashing_is_deterministic_and_tamper_sensitive() {
        // What `apply_settings_import` compares against the pinned hash from
        // `preview_settings_import` to detect a file swapped out from under a
        // pending import — this only defends anything if identical bytes
        // always hash identically and any change to those bytes changes the
        // hash.
        let bytes = b"pretend this is an encrypted export file";
        assert_eq!(hash_ciphertext(bytes), hash_ciphertext(bytes));

        let mut tampered = bytes.to_vec();
        tampered[0] ^= 0xFF;
        assert_ne!(hash_ciphertext(bytes), hash_ciphertext(&tampered));
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

    fn account(id: &str, method: AccountMethod) -> MarketplaceAccount {
        MarketplaceAccount {
            id: id.to_string(),
            label: format!("Account {id}"),
            host: "github.com".to_string(),
            method,
            username: None,
        }
    }

    #[test]
    fn export_carries_stored_tokens_of_token_and_container_accounts_only() {
        let accounts = vec![
            account("a-token", AccountMethod::Token),
            account("a-container", AccountMethod::GhContainer),
            account("a-host", AccountMethod::GhHost),
            account("a-missing", AccountMethod::Token),
            account("a-broken", AccountMethod::Token),
        ];
        let tokens = exported_marketplace_tokens(&accounts, |id| match id {
            "a-token" => Ok(Some("test-token-not-real-1".to_string())),
            "a-container" => Ok(Some("test-token-not-real-2".to_string())),
            "a-host" => panic!("a gh-host account stores no token, so none is read"),
            "a-missing" => Ok(None),
            _ => Err("keychain locked".to_string()),
        });
        assert_eq!(
            tokens,
            BTreeMap::from([
                ("a-container".to_string(), "test-token-not-real-2".to_string()),
                ("a-token".to_string(), "test-token-not-real-1".to_string()),
            ])
        );
    }

    #[test]
    fn marketplace_tokens_round_trip_through_an_export_and_validate_on_import() {
        use crate::models::marketplace::Marketplace;
        let id = "0f8fad5b-d9cb-469f-a165-70867728950e";
        let mut payload = sample_payload(SETTINGS_EXPORT_FORMAT_VERSION);
        payload
            .settings
            .marketplace_accounts
            .push(account(id, AccountMethod::Token));
        payload.settings.marketplaces.push(Marketplace {
            id: "7c9e6679-7425-40de-944b-e07fc1f90ae7".into(),
            name: "Team".into(),
            url: "https://github.com/org/repo.git".into(),
            branch: None,
            account_id: Some(id.into()),
        });
        payload.secrets.marketplace_account_tokens =
            BTreeMap::from([(id.to_string(), "test-token-not-real".to_string())]);

        let dir = temp_dir("marketplace-round-trip");
        let path = write_export(&dir, "x.triplec", &payload, "password123");
        let mut back = read_and_decrypt(&path, "password123").unwrap();

        assert_eq!(
            back.secrets.marketplace_account_tokens,
            payload.secrets.marketplace_account_tokens
        );
        assert_eq!(back.settings.marketplaces, payload.settings.marketplaces);
        crate::commands::marketplace_commands::validate_imported_marketplace_state(
            &mut back.settings,
            &back.secrets.marketplace_account_tokens,
        )
        .unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_marketplace_token_that_fails_to_restore_is_reported_without_its_value() {
        let tokens = BTreeMap::from([
            ("a1".to_string(), "test-token-not-real-1".to_string()),
            ("a2".to_string(), "test-token-not-real-2".to_string()),
        ]);
        let mut stored = Vec::new();
        let warnings = restore_marketplace_tokens(&tokens, |id, token| {
            if id == "a2" {
                return Err("keychain locked".to_string());
            }
            stored.push((id.to_string(), token.to_string()));
            Ok(())
        });
        assert_eq!(
            stored,
            vec![("a1".to_string(), "test-token-not-real-1".to_string())]
        );
        assert_eq!(warnings.len(), 1);
        assert!(!warnings[0].contains("test-token-not-real"));
    }

    fn keys_host(id: &str) -> HostConnection {
        HostConnection {
            id: id.into(),
            name: "Prod".into(),
            kind: HostKind::Aws {
                auth: AwsAuth::Keys { keyring_ref: id.into() },
                default_region: "us-west-2".into(),
            },
        }
    }

    fn profile_host(id: &str) -> HostConnection {
        HostConnection {
            id: id.into(),
            name: "Dev".into(),
            kind: HostKind::Aws {
                auth: AwsAuth::Profile { name: "default".into() },
                default_region: "us-west-2".into(),
            },
        }
    }

    fn test_keys() -> secure::AwsKeys {
        secure::AwsKeys {
            access_key_id: "AKIATESTNOTREAL".into(),
            secret_access_key: "test-secret-not-real".into(),
            session_token: None,
        }
    }

    #[test]
    fn aws_connection_keys_round_trip_through_export() {
        let hosts = vec![keys_host("k1"), profile_host("p1")];
        let exported = exported_aws_connection_keys(&hosts, |id| match id {
            "k1" => Ok(Some(test_keys())),
            other => panic!("only a Keys connection is read, got {other}"),
        });
        assert_eq!(exported.keys().collect::<Vec<_>>(), vec!["k1"]);
        assert_eq!(serde_json::from_str::<secure::AwsKeys>(&exported["k1"]).unwrap(), test_keys());

        let mut stored = Vec::new();
        let warnings = restore_aws_connection_keys(&exported, |id, keys| {
            stored.push((id.to_string(), keys.clone()));
            Ok(())
        });
        assert!(warnings.is_empty());
        assert_eq!(stored, vec![("k1".to_string(), test_keys())]);
    }

    #[test]
    fn aws_connection_keys_restore_failures_name_the_id_not_the_value() {
        let keys = BTreeMap::from([
            ("bad".to_string(), "not json test-secret-not-real".to_string()),
            ("locked".to_string(), serde_json::to_string(&test_keys()).unwrap()),
        ]);
        let warnings = restore_aws_connection_keys(&keys, |_, _| Err("keychain locked".into()));
        assert_eq!(warnings.len(), 2);
        assert!(warnings[0].contains("bad"));
        assert!(warnings[1].contains("locked"));
        assert!(warnings.iter().all(|w| !w.contains("test-secret-not-real")));
    }

    #[test]
    fn imported_hosts_are_validated() {
        let check = |hosts: Vec<HostConnection>, keys: BTreeMap<String, String>| {
            let mut s = AppSettings::default();
            s.hosts = hosts;
            validate_imported_hosts(&mut s, &keys)
        };
        let none = BTreeMap::new();
        assert!(check(vec![keys_host("k1"), profile_host("p1")], none.clone()).is_ok());
        assert!(check(vec![keys_host("k1")], BTreeMap::from([("k1".into(), "{}".into())])).is_ok());

        let mut bad_name = profile_host("p1");
        bad_name.name = "  ".into();
        assert!(check(vec![bad_name], none.clone()).is_err());

        let mut bad_region = profile_host("p1");
        bad_region.kind = HostKind::Aws {
            auth: AwsAuth::Profile { name: "default".into() },
            default_region: "nowhere".into(),
        };
        assert!(check(vec![bad_region], none.clone()).is_err());

        let mut cn = profile_host("p1");
        cn.kind = HostKind::Aws {
            auth: AwsAuth::Profile { name: "default".into() },
            default_region: "cn-north-1".into(),
        };
        let err = check(vec![cn], none.clone()).unwrap_err();
        assert!(err.contains("China and GovCloud regions aren't supported yet."), "{err}");

        let mut bad_profile = profile_host("p1");
        bad_profile.kind = HostKind::Aws {
            auth: AwsAuth::Profile { name: "a b".into() },
            default_region: "us-west-2".into(),
        };
        assert!(check(vec![bad_profile], none.clone()).is_err());

        let mut other_ref = keys_host("k1");
        other_ref.kind = HostKind::Aws {
            auth: AwsAuth::Keys { keyring_ref: "someone-else".into() },
            default_region: "us-west-2".into(),
        };
        assert!(check(vec![other_ref], none.clone()).is_err());

        assert!(check(vec![profile_host("../x")], none.clone()).is_err());
        assert!(check(vec![profile_host("p1"), profile_host("p1")], none.clone()).is_err());
        // Keys carried for a connection that is not a Keys connection.
        assert!(check(vec![profile_host("p1")], BTreeMap::from([("p1".into(), "{}".into())])).is_err());
        assert!(check(vec![], BTreeMap::from([("ghost".into(), "{}".into())])).is_err());
    }

    #[test]
    fn imported_hosts_survive_apply() {
        use crate::models::{AwsAuth, HostConnection, HostKind};
        let host = HostConnection {
            id: "h1".into(),
            name: "Imported".into(),
            kind: HostKind::Aws {
                auth: AwsAuth::Profile { name: "dev".into() },
                default_region: "us-west-2".into(),
            },
        };
        let stored = AppSettings::default();
        let mut imported = AppSettings::default();
        imported.hosts.push(host.clone());

        let owned = StoreOwned::of(&imported);
        // What `update_settings` does to the incoming settings.
        let mut after_update = imported.clone();
        crate::commands::settings_commands::restore_host_fields(&mut after_update, &stored);
        assert!(after_update.hosts.is_empty(), "update_settings drops hosts");

        assert_eq!(owned.applied_to(after_update).hosts, vec![host]);
    }
}
