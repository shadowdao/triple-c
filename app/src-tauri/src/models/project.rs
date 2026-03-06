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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Project {
    pub id: String,
    pub name: String,
    pub paths: Vec<ProjectPath>,
    pub container_id: Option<String>,
    pub status: ProjectStatus,
    pub auth_mode: AuthMode,
    pub bedrock_config: Option<BedrockConfig>,
    pub allow_docker_access: bool,
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
    pub enabled_mcp_servers: Vec<String>,
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

/// How the project authenticates with Claude.
/// - `Anthropic`: User runs `claude login` inside the container (OAuth via Anthropic Console,
///   persisted in the config volume)
/// - `Bedrock`: Uses AWS Bedrock with per-project AWS credentials
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum AuthMode {
    /// Backward compat: old projects stored as "login" or "api_key" map to Anthropic.
    #[serde(alias = "login", alias = "api_key")]
    Anthropic,
    Bedrock,
}

impl Default for AuthMode {
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
            auth_mode: AuthMode::default(),
            bedrock_config: None,
            allow_docker_access: false,
            ssh_key_path: None,
            git_token: None,
            git_user_name: None,
            git_user_email: None,
            custom_env_vars: Vec::new(),
            port_mappings: Vec::new(),
            claude_instructions: None,
            enabled_mcp_servers: Vec::new(),
            created_at: now.clone(),
            updated_at: now,
        }
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
