use std::fs;
use std::path::PathBuf;
use std::sync::Mutex;

use crate::models::McpServer;

pub struct McpStore {
    servers: Mutex<Vec<McpServer>>,
    file_path: PathBuf,
}

impl McpStore {
    pub fn new() -> Result<Self, String> {
        let data_dir = dirs::data_dir()
            .ok_or_else(|| "Could not determine data directory. Set XDG_DATA_HOME on Linux.".to_string())?
            .join("triple-c");

        fs::create_dir_all(&data_dir).ok();

        let file_path = data_dir.join("mcp_servers.json");

        let servers = if file_path.exists() {
            match fs::read_to_string(&file_path) {
                Ok(data) => {
                    match serde_json::from_str::<Vec<McpServer>>(&data) {
                        Ok(parsed) => parsed,
                        Err(e) => {
                            log::error!("Failed to parse mcp_servers.json: {}. Starting with empty list.", e);
                            let backup = file_path.with_extension("json.bak");
                            if let Err(be) = fs::copy(&file_path, &backup) {
                                log::error!("Failed to back up corrupted mcp_servers.json: {}", be);
                            }
                            Vec::new()
                        }
                    }
                }
                Err(e) => {
                    log::error!("Failed to read mcp_servers.json: {}", e);
                    Vec::new()
                }
            }
        } else {
            Vec::new()
        };

        Ok(Self {
            servers: Mutex::new(servers),
            file_path,
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<McpServer>> {
        self.servers.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn save(&self, servers: &[McpServer]) -> Result<(), String> {
        let data = serde_json::to_string_pretty(servers)
            .map_err(|e| format!("Failed to serialize MCP servers: {}", e))?;

        // Atomic write: write to temp file, then rename
        let tmp_path = self.file_path.with_extension("json.tmp");
        fs::write(&tmp_path, data)
            .map_err(|e| format!("Failed to write temp MCP servers file: {}", e))?;
        fs::rename(&tmp_path, &self.file_path)
            .map_err(|e| format!("Failed to rename MCP servers file: {}", e))?;
        Ok(())
    }

    pub fn list(&self) -> Vec<McpServer> {
        self.lock().clone()
    }

    pub fn get(&self, id: &str) -> Option<McpServer> {
        self.lock().iter().find(|s| s.id == id).cloned()
    }

    pub fn add(&self, server: McpServer) -> Result<McpServer, String> {
        let mut servers = self.lock();
        let cloned = server.clone();
        servers.push(server);
        self.save(&servers)?;
        Ok(cloned)
    }

    pub fn update(&self, updated: McpServer) -> Result<McpServer, String> {
        let mut servers = self.lock();
        if let Some(s) = servers.iter_mut().find(|s| s.id == updated.id) {
            *s = updated.clone();
            self.save(&servers)?;
            Ok(updated)
        } else {
            Err(format!("MCP server {} not found", updated.id))
        }
    }

    pub fn remove(&self, id: &str) -> Result<(), String> {
        let mut servers = self.lock();
        let initial_len = servers.len();
        servers.retain(|s| s.id != id);
        if servers.len() == initial_len {
            return Err(format!("MCP server {} not found", id));
        }
        self.save(&servers)?;
        Ok(())
    }
}
