//! Settings and status for the **model gateway** — a LiteLLM proxy container
//! Triple-C runs as a sibling of the project containers.
//!
//! Claude Code speaks only the Anthropic Messages API (`POST
//! ${ANTHROPIC_BASE_URL}/v1/messages`). OpenAI has no such route, so an OpenAI
//! key cannot drive Claude Code directly. The gateway exposes `/v1/messages`
//! in Anthropic format and translates each call to the configured provider,
//! which is what turns "OpenAI Compatible" from *bring your own proxy* into
//! something Triple-C manages itself.
//!
//! Nothing secret lives in this module. The provider API key and the gateway's
//! own master key are held in the OS keychain (see `storage::secure`); what is
//! persisted to `settings.json` is only the non-secret shape of the config.

use serde::{Deserialize, Serialize};

/// LiteLLM's own default port, and the one the existing "OpenAI Compatible"
/// placeholder text already suggests.
pub fn default_gateway_port() -> u16 {
    4000
}

fn default_gateway_provider() -> String {
    "openai".to_string()
}

/// One entry of LiteLLM's `model_list`.
///
/// `name` is the friendly handle a project puts in its model field — it is what
/// Claude Code sends as the `model` of a `/v1/messages` request. `model_id` is
/// the provider-side id. The gateway config composes them as
/// `<provider>/<model_id>`, which is why the shape stays generic across
/// providers instead of hard-coding OpenAI.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct GatewayModel {
    /// Friendly name projects use (e.g. `gpt-5.1`).
    pub name: String,
    /// Provider-side model id (e.g. `gpt-5.1`).
    pub model_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GatewaySettings {
    /// Auto-start the gateway container with the app.
    #[serde(default)]
    pub enabled: bool,
    /// Host port the gateway is published on.
    #[serde(default = "default_gateway_port")]
    pub port: u16,
    /// LiteLLM provider prefix — `openai`, `azure`, `gemini`, `groq`, …
    #[serde(default = "default_gateway_provider")]
    pub provider: String,
    /// Optional provider base URL override (Azure endpoints, proxies, …).
    #[serde(default)]
    pub api_base: Option<String>,
    /// Models the gateway should serve.
    #[serde(default)]
    pub models: Vec<GatewayModel>,
}

impl Default for GatewaySettings {
    fn default() -> Self {
        Self {
            enabled: false,
            port: default_gateway_port(),
            provider: default_gateway_provider(),
            api_base: None,
            models: Vec::new(),
        }
    }
}

impl GatewaySettings {
    /// Models with both fields filled in. Half-typed rows in the UI must not
    /// reach the generated YAML.
    pub fn valid_models(&self) -> Vec<&GatewayModel> {
        self.models
            .iter()
            .filter(|m| !m.name.trim().is_empty() && !m.model_id.trim().is_empty())
            .collect()
    }
}

/// What the settings UI needs to know about the gateway. Deliberately carries
/// **no** secret: `has_api_key` is a boolean, not the key.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GatewayStatus {
    pub container_exists: bool,
    pub running: bool,
    pub port: u16,
    pub image_exists: bool,
    /// Number of fully-specified models in the current settings.
    pub model_count: usize,
    /// Whether a provider API key is present in the keychain.
    pub has_api_key: bool,
    /// The value a project should use for its base URL. See
    /// `docker::gateway::gateway_base_url`.
    pub base_url: String,
}
