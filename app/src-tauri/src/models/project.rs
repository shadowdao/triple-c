use std::collections::HashMap;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EnvVar {
    pub key: String,
    pub value: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProjectPath {
    pub host_path: String,
    pub mount_name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PortMapping {
    pub host_port: u16,
    pub container_port: u16,
    #[serde(default = "default_protocol")]
    pub protocol: String,
}

fn default_protocol() -> String {
    "tcp".to_string()
}

fn default_full_permissions() -> bool {
    true
}

/// `use_shared_auth_token` defaults to **on**: once the user has run
/// `claude setup-token` once, every existing Anthropic-backend project should
/// pick the token up without being edited one by one. Projects deliberately
/// pinned to their own `claude login` identity opt out.
fn default_use_shared_auth_token() -> bool {
    true
}

/// How much autonomy Claude Code is granted inside the container.
///
/// Maps onto Claude Code CLI flags — see [`PermissionMode::cli_args`], which is
/// the single definition of that mapping and must be used by every call site.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub enum PermissionMode {
    /// Read-only planning mode.
    Plan,
    /// Claude Code's own default behavior (prompts for permission).
    #[default]
    Default,
    /// Auto-accept file edits, prompt for everything else.
    AcceptEdits,
    /// Skip all permission prompts.
    Bypass,
}

impl PermissionMode {
    /// The CLI flags this mode adds to a `claude` invocation.
    /// Defined once here so every call site stays in sync.
    pub fn cli_args(&self) -> Vec<String> {
        match self {
            PermissionMode::Plan => vec!["--permission-mode".to_string(), "plan".to_string()],
            PermissionMode::Default => Vec::new(),
            PermissionMode::AcceptEdits => {
                vec!["--permission-mode".to_string(), "acceptEdits".to_string()]
            }
            PermissionMode::Bypass => vec!["--dangerously-skip-permissions".to_string()],
        }
    }

    /// The wire value used for the `TRIPLE_C_PERMISSION_MODE` container env var.
    /// Matches the serde `camelCase` representation.
    pub fn as_env_value(&self) -> &'static str {
        match self {
            PermissionMode::Plan => "plan",
            PermissionMode::Default => "default",
            PermissionMode::AcceptEdits => "acceptEdits",
            PermissionMode::Bypass => "bypass",
        }
    }
}

/// Settings for Claude Code CLI behavior inside the container.
/// These map to Claude Code env vars and ~/.claude/settings.json entries.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ClaudeCodeSettings {
    /// TUI rendering mode: None = default, Some("fullscreen") = flicker-free alt-screen
    #[serde(default)]
    pub tui_mode: Option<String>,
    /// Effort level: None = default, Some("low"|"medium"|"high")
    #[serde(default)]
    pub effort: Option<String>,
    /// Disable auto-scroll in fullscreen TUI mode
    #[serde(default)]
    pub auto_scroll_disabled: bool,
    /// Enable focus mode (collapsed tool output)
    #[serde(default)]
    pub focus_mode: bool,
    /// Show thinking summaries in responses
    #[serde(default)]
    pub show_thinking_summaries: bool,
    /// Enable session recap when returning to a session
    #[serde(default)]
    pub enable_session_recap: bool,
    /// Strip credentials from subprocess environments
    #[serde(default)]
    pub env_scrub: bool,
    /// Enable 1-hour prompt cache TTL (vs default 5-minute)
    #[serde(default)]
    pub prompt_caching_1h: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Project {
    pub id: String,
    pub name: String,
    pub paths: Vec<ProjectPath>,
    pub container_id: Option<String>,
    pub status: ProjectStatus,
    #[serde(alias = "auth_mode")]
    pub backend: Backend,
    pub bedrock_config: Option<BedrockConfig>,
    pub ollama_config: Option<OllamaConfig>,
    #[serde(alias = "litellm_config")]
    pub openai_compatible_config: Option<OpenAiCompatibleConfig>,
    pub allow_docker_access: bool,
    #[serde(default)]
    pub sandbox_mode_enabled: bool,
    #[serde(default)]
    pub mission_control_enabled: bool,
    /// Opt in to the auth bridge: while the container runs, its loopback
    /// listeners are mirrored onto the host's loopback so browser OAuth
    /// callbacks (`claude login`, `fly login`, `aws sso login`) can reach them.
    /// Purely host-side — it deliberately has no container-recreation label,
    /// because toggling it changes nothing about the container itself.
    #[serde(default)]
    pub auth_bridge_enabled: bool,
    /// Use the shared, long-lived Claude Code OAuth token (from
    /// `claude setup-token`, held in the OS keychain) for this project instead
    /// of requiring its own `claude login`. Only consulted when `backend` is
    /// [`Backend::Anthropic`] and a token has actually been stored.
    ///
    /// Defaults to **true** so a single `setup-token` run covers every project;
    /// turn it off to pin a project to the identity it logged in with inside
    /// its own container.
    #[serde(default = "default_use_shared_auth_token")]
    pub use_shared_auth_token: bool,
    /// Legacy binary permission flag. Superseded by `permission_mode`, but kept
    /// because it is the value already stored in users' `projects.json`; it is
    /// the fallback in `effective_permission_mode()` so old projects keep
    /// behaving identically without a data migration.
    #[serde(default = "default_full_permissions")]
    pub full_permissions: bool,
    /// Per-project permission mode. `None` means "not set yet" → fall back to
    /// the legacy `full_permissions` flag.
    #[serde(default)]
    pub permission_mode: Option<PermissionMode>,
    pub ssh_key_path: Option<String>,
    #[serde(skip_serializing, default)]
    pub git_token: Option<String>,
    pub git_user_name: Option<String>,
    pub git_user_email: Option<String>,
    #[serde(default)]
    pub custom_env_vars: Vec<EnvVar>,
    #[serde(default)]
    pub port_mappings: Vec<PortMapping>,
    #[serde(default)]
    pub claude_instructions: Option<String>,
    #[serde(default)]
    pub claude_code_settings: Option<ClaudeCodeSettings>,
    /// User-defined display names for terminal tabs, keyed by session id.
    #[serde(default)]
    pub renamed_session_names: HashMap<String, String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum ProjectStatus {
    Stopped,
    Starting,
    Running,
    Stopping,
    Error,
}

/// Which AI model backend/provider the project uses.
/// - `Anthropic`: Direct Anthropic API (user runs `claude login` inside the container)
/// - `Bedrock`: AWS Bedrock with per-project AWS credentials
/// - `Ollama`: Local or remote Ollama server
/// - `OpenAiCompatible`: Any OpenAI API-compatible endpoint (e.g., LiteLLM, vLLM, etc.)
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Backend {
    /// Backward compat: old projects stored as "login" or "api_key" map to Anthropic.
    #[serde(alias = "login", alias = "api_key")]
    Anthropic,
    Bedrock,
    Ollama,
    #[serde(alias = "lite_llm", alias = "litellm")]
    OpenAiCompatible,
}

impl Default for Backend {
    fn default() -> Self {
        Self::Anthropic
    }
}

/// How Bedrock authenticates with AWS.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum BedrockAuthMethod {
    StaticCredentials,
    Profile,
    BearerToken,
}

impl Default for BedrockAuthMethod {
    fn default() -> Self {
        Self::StaticCredentials
    }
}

/// AWS Bedrock configuration for a project.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BedrockConfig {
    pub auth_method: BedrockAuthMethod,
    pub aws_region: String,
    #[serde(skip_serializing, default)]
    pub aws_access_key_id: Option<String>,
    #[serde(skip_serializing, default)]
    pub aws_secret_access_key: Option<String>,
    #[serde(skip_serializing, default)]
    pub aws_session_token: Option<String>,
    pub aws_profile: Option<String>,
    #[serde(skip_serializing, default)]
    pub aws_bearer_token: Option<String>,
    pub model_id: Option<String>,
    pub disable_prompt_caching: bool,
    /// Optional value for the `ANTHROPIC_BEDROCK_SERVICE_TIER` env var
    /// (e.g. "priority"). Empty/None means leave unset.
    #[serde(default)]
    pub service_tier: Option<String>,
}

/// Ollama configuration for a project.
/// Ollama exposes an Anthropic-compatible API endpoint.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OllamaConfig {
    /// The base URL of the Ollama server (e.g., "http://host.docker.internal:11434" or "http://192.168.1.100:11434")
    pub base_url: String,
    /// Optional model override (e.g., "qwen3.5:27b")
    pub model_id: Option<String>,
}

/// OpenAI Compatible endpoint configuration for a project.
/// Routes Anthropic API calls through any OpenAI API-compatible endpoint
/// (e.g., LiteLLM, vLLM, or other compatible gateways).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenAiCompatibleConfig {
    /// The base URL of the OpenAI-compatible endpoint (e.g., "http://host.docker.internal:4000" or "https://api.example.com")
    pub base_url: String,
    /// API key for the OpenAI-compatible endpoint
    #[serde(skip_serializing, default)]
    pub api_key: Option<String>,
    /// Optional model override
    pub model_id: Option<String>,
}

impl Project {
    pub fn new(name: String, paths: Vec<ProjectPath>) -> Self {
        let now = chrono::Utc::now().to_rfc3339();
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            name,
            paths,
            container_id: None,
            status: ProjectStatus::Stopped,
            backend: Backend::default(),
            bedrock_config: None,
            ollama_config: None,
            openai_compatible_config: None,
            allow_docker_access: false,
            sandbox_mode_enabled: false,
            mission_control_enabled: false,
            auth_bridge_enabled: false,
            use_shared_auth_token: default_use_shared_auth_token(),
            full_permissions: false,
            permission_mode: None,
            ssh_key_path: None,
            git_token: None,
            git_user_name: None,
            git_user_email: None,
            custom_env_vars: Vec::new(),
            port_mappings: Vec::new(),
            claude_instructions: None,
            claude_code_settings: None,
            renamed_session_names: HashMap::new(),
            created_at: now.clone(),
            updated_at: now,
        }
    }

    /// The permission mode to actually use for this project.
    /// Falls back to the legacy `full_permissions` boolean when the newer
    /// `permission_mode` field has never been set.
    pub fn effective_permission_mode(&self) -> PermissionMode {
        self.permission_mode.unwrap_or(if self.full_permissions {
            PermissionMode::Bypass
        } else {
            PermissionMode::Default
        })
    }

    pub fn container_name(&self) -> String {
        format!("triple-c-{}", self.id)
    }

    /// Migrate a project JSON value from old single-`path` format to new `paths` format.
    /// If the value already has `paths`, it is returned unchanged.
    pub fn migrate_from_value(mut val: serde_json::Value) -> serde_json::Value {
        if let Some(obj) = val.as_object_mut() {
            if obj.contains_key("paths") {
                return val;
            }
            if let Some(path_val) = obj.remove("path") {
                let path_str = path_val.as_str().unwrap_or("").to_string();
                let mount_name = path_str
                    .trim_end_matches(['/', '\\'])
                    .rsplit(['/', '\\'])
                    .next()
                    .unwrap_or("workspace")
                    .to_string();
                let project_path = serde_json::json!([{
                    "host_path": path_str,
                    "mount_name": if mount_name.is_empty() { "workspace".to_string() } else { mount_name },
                }]);
                obj.insert("paths".to_string(), project_path);
            }
        }
        val
    }
}
