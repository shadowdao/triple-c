use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Project {
    pub id: String,
    pub name: String,
    pub path: String,
    pub container_id: Option<String>,
    pub status: ProjectStatus,
    pub auth_mode: AuthMode,
    pub bedrock_config: Option<BedrockConfig>,
    pub allow_docker_access: bool,
    pub ssh_key_path: Option<String>,
    pub git_token: Option<String>,
    pub git_user_name: Option<String>,
    pub git_user_email: Option<String>,
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
/// - `Login`: User runs `claude login` inside the container (OAuth, persisted via config volume)
/// - `ApiKey`: Uses the API key stored in the OS keychain
/// - `Bedrock`: Uses AWS Bedrock with per-project AWS credentials
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum AuthMode {
    Login,
    ApiKey,
    Bedrock,
}

impl Default for AuthMode {
    fn default() -> Self {
        Self::Login
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
    pub aws_access_key_id: Option<String>,
    pub aws_secret_access_key: Option<String>,
    pub aws_session_token: Option<String>,
    pub aws_profile: Option<String>,
    pub aws_bearer_token: Option<String>,
    pub model_id: Option<String>,
    pub disable_prompt_caching: bool,
}

impl Project {
    pub fn new(name: String, path: String) -> Self {
        let now = chrono::Utc::now().to_rfc3339();
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            name,
            path,
            container_id: None,
            status: ProjectStatus::Stopped,
            auth_mode: AuthMode::default(),
            bedrock_config: None,
            allow_docker_access: false,
            ssh_key_path: None,
            git_token: None,
            git_user_name: None,
            git_user_email: None,
            created_at: now.clone(),
            updated_at: now,
        }
    }

    pub fn container_name(&self) -> String {
        format!("triple-c-{}", self.id)
    }
}
