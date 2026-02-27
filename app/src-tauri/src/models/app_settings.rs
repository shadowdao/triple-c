use serde::{Deserialize, Serialize};

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
        }
    }
}
