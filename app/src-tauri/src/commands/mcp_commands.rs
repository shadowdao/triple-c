use tauri::State;

use crate::models::McpServer;
use crate::AppState;

#[tauri::command]
pub async fn list_mcp_servers(state: State<'_, AppState>) -> Result<Vec<McpServer>, String> {
    Ok(state.mcp_store.list())
}

#[tauri::command]
pub async fn add_mcp_server(
    name: String,
    state: State<'_, AppState>,
) -> Result<McpServer, String> {
    let name = name.trim().to_string();
    if name.is_empty() {
        return Err("MCP server name cannot be empty.".to_string());
    }
    let server = McpServer::new(name);
    state.mcp_store.add(server)
}

#[tauri::command]
pub async fn update_mcp_server(
    server: McpServer,
    state: State<'_, AppState>,
) -> Result<McpServer, String> {
    state.mcp_store.update(server)
}

#[tauri::command]
pub async fn remove_mcp_server(
    server_id: String,
    state: State<'_, AppState>,
) -> Result<(), String> {
    state.mcp_store.remove(&server_id)
}
