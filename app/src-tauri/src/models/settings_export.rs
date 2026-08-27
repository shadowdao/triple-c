//! Settings export/import — see triple-c#35.
//!
//! `SettingsExportPayload` is the whole plaintext export before encryption
//! and after decryption (see `storage::settings_crypto`). It bundles
//! `AppSettings` (already the non-secret shape persisted to `settings.json`)
//! with the global secrets that live in the OS keychain instead — the shared
//! Claude Code OAuth login and the model gateway's two keys. Per-project
//! settings, per-project secrets, and anything living in a project's Docker
//! volumes are deliberately out of scope: this exports the *host*
//! environment, not any one project's.

use serde::{Deserialize, Serialize};

use super::AppSettings;

/// Bumped when the shape of [`SettingsExportPayload`] changes in a way that
/// isn't just an additive, `#[serde(default)]`-covered field — e.g. if a
/// field is ever removed or its meaning changes. `apply_settings_import`
/// checks this before touching anything.
pub const SETTINGS_EXPORT_FORMAT_VERSION: u32 = 1;

/// The global secrets bundled into an export. Deliberately a separate struct
/// from `AppSettings`: these live in the OS keychain, never in
/// `settings.json`, and — outside of this export/import flow — the values
/// themselves never cross into the frontend; see the doc comments on
/// `storage::secure::get_gateway_api_key` and
/// `commands::settings_export_commands` for why that boundary matters here
/// too.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ExportedSecrets {
    #[serde(default)]
    pub claude_oauth_token: Option<String>,
    #[serde(default)]
    pub gateway_api_key: Option<String>,
    #[serde(default)]
    pub gateway_master_key: Option<String>,
}

impl ExportedSecrets {
    pub fn is_empty(&self) -> bool {
        self.claude_oauth_token.is_none()
            && self.gateway_api_key.is_none()
            && self.gateway_master_key.is_none()
    }
}

/// The full plaintext payload — this is what gets encrypted on export and
/// what decryption recovers on import. Never written to disk unencrypted;
/// see `storage::settings_crypto`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SettingsExportPayload {
    pub format_version: u32,
    /// RFC3339. Purely informational — shown in the import preview so a user
    /// picking between a few old export files has something to go on.
    pub exported_at: String,
    /// The exporting app's `CARGO_PKG_VERSION`. Also informational: every
    /// field below already round-trips through `#[serde(default)]`-covered
    /// `AppSettings`, so an older or newer export still deserializes; this is
    /// for a human to notice "this is from a much older version" if an import
    /// ever looks wrong, not something the code branches on.
    pub app_version: String,
    pub settings: AppSettings,
    #[serde(default)]
    pub secrets: ExportedSecrets,
}

/// What `preview_settings_import` hands the frontend before anything is
/// applied — counts and presence flags only, **never** a secret value itself,
/// so this type is safe to return across the IPC boundary and render
/// directly. The confirmation UI is built from this.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SettingsImportPreview {
    pub exported_at: String,
    pub app_version: String,
    pub custom_env_var_count: usize,
    pub gateway_model_count: usize,
    pub has_claude_code_settings: bool,
    pub has_claude_oauth_token: bool,
    pub has_gateway_api_key: bool,
    pub has_gateway_master_key: bool,
}

impl SettingsImportPreview {
    pub fn from_payload(payload: &SettingsExportPayload) -> Self {
        Self {
            exported_at: payload.exported_at.clone(),
            app_version: payload.app_version.clone(),
            custom_env_var_count: payload.settings.global_custom_env_vars.len(),
            gateway_model_count: payload.settings.gateway.models.len(),
            has_claude_code_settings: payload.settings.global_claude_code_settings.is_some(),
            has_claude_oauth_token: payload
                .secrets
                .claude_oauth_token
                .as_deref()
                .is_some_and(|t| !t.trim().is_empty()),
            has_gateway_api_key: payload
                .secrets
                .gateway_api_key
                .as_deref()
                .is_some_and(|k| !k.trim().is_empty()),
            has_gateway_master_key: payload
                .secrets
                .gateway_master_key
                .as_deref()
                .is_some_and(|k| !k.trim().is_empty()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::AppSettings;

    fn payload_with(secrets: ExportedSecrets) -> SettingsExportPayload {
        let mut settings = AppSettings::default();
        settings.global_custom_env_vars = vec![
            crate::models::EnvVar { key: "A".to_string(), value: "1".to_string() },
            crate::models::EnvVar { key: "B".to_string(), value: "2".to_string() },
        ];
        SettingsExportPayload {
            format_version: SETTINGS_EXPORT_FORMAT_VERSION,
            exported_at: "2026-08-27T00:00:00Z".to_string(),
            app_version: "0.4.14".to_string(),
            settings,
            secrets,
        }
    }

    #[test]
    fn the_preview_never_carries_a_secret_value() {
        let payload = payload_with(ExportedSecrets {
            claude_oauth_token: Some("sk-super-secret-token".to_string()),
            gateway_api_key: Some("sk-another-secret".to_string()),
            gateway_master_key: Some("sk-triple-c-yet-another".to_string()),
        });
        let preview = SettingsImportPreview::from_payload(&payload);
        let serialized = serde_json::to_string(&preview).unwrap();

        assert!(!serialized.contains("sk-super-secret-token"));
        assert!(!serialized.contains("sk-another-secret"));
        assert!(!serialized.contains("sk-triple-c-yet-another"));
        assert!(preview.has_claude_oauth_token);
        assert!(preview.has_gateway_api_key);
        assert!(preview.has_gateway_master_key);
    }

    #[test]
    fn a_blank_secret_reads_as_absent_in_the_preview() {
        // A keychain entry that exists but holds only whitespace must not
        // read as "present" — same "blank counts as absent" rule the
        // keychain layer itself applies when storing these.
        let payload = payload_with(ExportedSecrets {
            claude_oauth_token: Some("   ".to_string()),
            gateway_api_key: None,
            gateway_master_key: None,
        });
        let preview = SettingsImportPreview::from_payload(&payload);
        assert!(!preview.has_claude_oauth_token);
        assert!(!preview.has_gateway_api_key);
        assert!(!preview.has_gateway_master_key);
    }

    #[test]
    fn counts_reflect_the_real_settings() {
        let payload = payload_with(ExportedSecrets::default());
        let preview = SettingsImportPreview::from_payload(&payload);
        assert_eq!(preview.custom_env_var_count, 2);
    }

    #[test]
    fn an_empty_secrets_bundle_reports_itself_as_empty() {
        assert!(ExportedSecrets::default().is_empty());
        assert!(!ExportedSecrets {
            claude_oauth_token: Some("x".to_string()),
            ..Default::default()
        }
        .is_empty());
    }
}
