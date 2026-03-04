use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum McpTransportType {
    Stdio,
    #[serde(alias = "sse")]
    Http,
}

impl Default for McpTransportType {
    fn default() -> Self {
        Self::Stdio
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServer {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub transport_type: McpTransportType,
    pub command: Option<String>,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: HashMap<String, String>,
    pub url: Option<String>,
    #[serde(default)]
    pub headers: HashMap<String, String>,
    #[serde(default)]
    pub docker_image: Option<String>,
    #[serde(default)]
    pub container_port: Option<u16>,
    pub created_at: String,
    pub updated_at: String,
}

impl McpServer {
    pub fn new(name: String) -> Self {
        let now = chrono::Utc::now().to_rfc3339();
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            name,
            transport_type: McpTransportType::default(),
            command: None,
            args: Vec::new(),
            env: HashMap::new(),
            url: None,
            headers: HashMap::new(),
            docker_image: None,
            container_port: None,
            created_at: now.clone(),
            updated_at: now,
        }
    }

    pub fn is_docker(&self) -> bool {
        self.docker_image.is_some()
    }

    pub fn mcp_container_name(&self) -> String {
        format!("triple-c-mcp-{}", self.id)
    }

    pub fn effective_container_port(&self) -> u16 {
        self.container_port.unwrap_or(3000)
    }
}
