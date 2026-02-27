use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppSettings {
    pub default_ssh_key_path: Option<String>,
    pub default_git_user_name: Option<String>,
    pub default_git_user_email: Option<String>,
    pub docker_socket_path: Option<String>,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            default_ssh_key_path: None,
            default_git_user_name: None,
            default_git_user_email: None,
            docker_socket_path: None,
        }
    }
}
