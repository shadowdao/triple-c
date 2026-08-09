use serde::{Deserialize, Serialize};

use super::gateway_settings::GatewaySettings;
use super::project::{ClaudeCodeSettings, EnvVar};

fn default_true() -> bool {
    true
}

fn default_global_instructions() -> Option<String> {
    Some("If the project is not initialized with git, recommend to the user to initialize and use git to track changes. This makes it easier to revert should something break.\n\nUse subagents frequently. For long-running tasks, break the work into parallel subagents where possible. When handling multiple separate tasks, delegate each to its own subagent so they can run concurrently.".to_string())
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ImageSource {
    Registry,
    LocalBuild,
    Custom,
}

impl Default for ImageSource {
    fn default() -> Self {
        Self::Registry
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GlobalAwsSettings {
    #[serde(default)]
    pub aws_config_path: Option<String>,
    #[serde(default)]
    pub aws_profile: Option<String>,
    #[serde(default)]
    pub aws_region: Option<String>,
    #[serde(default)]
    pub default_model_id: Option<String>,
}

impl Default for GlobalAwsSettings {
    fn default() -> Self {
        Self {
            aws_config_path: None,
            aws_profile: None,
            aws_region: None,
            default_model_id: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct GlobalOllamaSettings {
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub default_model_id: Option<String>,
    /// Global fallback for the `haiku` alias override. Blank means "use the
    /// resolved model id", which is what makes background Claude Code calls
    /// work against a server that only serves one model.
    #[serde(default)]
    pub default_haiku_model_id: Option<String>,
}

/// Global defaults for the llama.cpp (`llama-server`) backend.
/// Mirrors [`GlobalOllamaSettings`]; used when the per-project field is blank.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct GlobalLlamaCppSettings {
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub default_model_id: Option<String>,
    #[serde(default)]
    pub default_haiku_model_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct GlobalOpenAiCompatibleSettings {
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub default_model_id: Option<String>,
    #[serde(default)]
    pub default_haiku_model_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppSettings {
    #[serde(default)]
    pub default_ssh_key_path: Option<String>,
    #[serde(default)]
    pub default_git_user_name: Option<String>,
    #[serde(default)]
    pub default_git_user_email: Option<String>,
    #[serde(default)]
    pub docker_socket_path: Option<String>,
    #[serde(default)]
    pub image_source: ImageSource,
    #[serde(default)]
    pub custom_image_name: Option<String>,
    #[serde(default)]
    pub global_aws: GlobalAwsSettings,
    #[serde(default)]
    pub global_ollama: GlobalOllamaSettings,
    #[serde(default)]
    pub global_llamacpp: GlobalLlamaCppSettings,
    #[serde(default)]
    pub global_openai_compatible: GlobalOpenAiCompatibleSettings,
    #[serde(default = "default_global_instructions")]
    pub global_claude_instructions: Option<String>,
    #[serde(default)]
    pub global_custom_env_vars: Vec<EnvVar>,
    #[serde(default = "default_true")]
    pub auto_check_updates: bool,
    #[serde(default)]
    pub dismissed_update_version: Option<String>,
    #[serde(default)]
    pub timezone: Option<String>,
    #[serde(default)]
    pub default_microphone: Option<String>,
    #[serde(default)]
    pub dismissed_image_digest: Option<String>,
    #[serde(default)]
    pub web_terminal: WebTerminalSettings,
    #[serde(default)]
    pub stt: SttSettings,
    #[serde(default)]
    pub gateway: GatewaySettings,
    #[serde(default)]
    pub global_claude_code_settings: Option<ClaudeCodeSettings>,
}

fn default_stt_model() -> String {
    "tiny".to_string()
}

fn default_stt_port() -> u16 {
    9876
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SttSettings {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_stt_model")]
    pub model: String,
    #[serde(default = "default_stt_port")]
    pub port: u16,
    #[serde(default)]
    pub language: Option<String>,
}

impl Default for SttSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            model: default_stt_model(),
            port: 9876,
            language: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SttStatus {
    pub container_exists: bool,
    pub running: bool,
    pub port: u16,
    pub model: String,
    pub image_exists: bool,
}

fn default_web_terminal_port() -> u16 {
    7681
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebTerminalSettings {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_web_terminal_port")]
    pub port: u16,
    #[serde(default)]
    pub access_token: Option<String>,
}

impl Default for WebTerminalSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            port: 7681,
            access_token: None,
        }
    }
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            default_ssh_key_path: None,
            default_git_user_name: None,
            default_git_user_email: None,
            docker_socket_path: None,
            image_source: ImageSource::default(),
            custom_image_name: None,
            global_aws: GlobalAwsSettings::default(),
            global_ollama: GlobalOllamaSettings::default(),
            global_llamacpp: GlobalLlamaCppSettings::default(),
            global_openai_compatible: GlobalOpenAiCompatibleSettings::default(),
            global_claude_instructions: default_global_instructions(),
            global_custom_env_vars: Vec::new(),
            auto_check_updates: true,
            dismissed_update_version: None,
            timezone: None,
            default_microphone: None,
            dismissed_image_digest: None,
            web_terminal: WebTerminalSettings::default(),
            stt: SttSettings::default(),
            gateway: GatewaySettings::default(),
            global_claude_code_settings: None,
        }
    }
}
