use serde::{Deserialize, Serialize};

use super::project::EnvVar;

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
}

impl Default for GlobalAwsSettings {
    fn default() -> Self {
        Self {
            aws_config_path: None,
            aws_profile: None,
            aws_region: None,
        }
    }
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
            global_claude_instructions: default_global_instructions(),
            global_custom_env_vars: Vec::new(),
            auto_check_updates: true,
            dismissed_update_version: None,
            timezone: None,
            default_microphone: None,
            dismissed_image_digest: None,
        }
    }
}
