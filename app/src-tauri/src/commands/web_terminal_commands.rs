use serde::Serialize;
use tauri::State;

use crate::web_terminal::WebTerminalServer;
use crate::AppState;

#[derive(Serialize)]
pub struct WebTerminalInfo {
    pub running: bool,
    pub port: u16,
    pub access_token: String,
    pub local_ip: Option<String>,
    pub url: Option<String>,
}

fn generate_token() -> String {
    use rand::Rng;
    let mut rng = rand::rng();
    let bytes: Vec<u8> = (0..32).map(|_| rng.random::<u8>()).collect();
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&bytes)
}

fn get_local_ip() -> Option<String> {
    local_ip_address::local_ip().ok().map(|ip| ip.to_string())
}

fn build_info(running: bool, port: u16, token: &str) -> WebTerminalInfo {
    let local_ip = get_local_ip();
    let url = if running {
        local_ip
            .as_ref()
            .map(|ip| format!("http://{}:{}?token={}", ip, port, token))
    } else {
        None
    };
    WebTerminalInfo {
        running,
        port,
        access_token: token.to_string(),
        local_ip,
        url,
    }
}

#[tauri::command]
pub async fn start_web_terminal(state: State<'_, AppState>) -> Result<WebTerminalInfo, String> {
    let mut server_guard = state.web_terminal_server.lock().await;
    if server_guard.is_some() {
        return Err("Web terminal server is already running".to_string());
    }

    let mut settings = state.settings_store.get();

    // Auto-generate token if not set
    if settings.web_terminal.access_token.is_none() {
        settings.web_terminal.access_token = Some(generate_token());
        settings.web_terminal.enabled = true;
        state.settings_store.update(settings.clone()).map_err(|e| format!("Failed to save settings: {}", e))?;
    }

    let token = settings.web_terminal.access_token.clone().unwrap_or_default();
    let port = settings.web_terminal.port;

    let server = WebTerminalServer::start(
        port,
        token.clone(),
        state.exec_manager.clone(),
        state.projects_store.clone(),
        state.settings_store.clone(),
    )
    .await?;

    *server_guard = Some(server);

    // Mark as enabled in settings
    if !settings.web_terminal.enabled {
        settings.web_terminal.enabled = true;
        let _ = state.settings_store.update(settings);
    }

    Ok(build_info(true, port, &token))
}

#[tauri::command]
pub async fn stop_web_terminal(state: State<'_, AppState>) -> Result<(), String> {
    let mut server_guard = state.web_terminal_server.lock().await;
    if let Some(server) = server_guard.take() {
        server.stop();
    }

    // Mark as disabled in settings
    let mut settings = state.settings_store.get();
    if settings.web_terminal.enabled {
        settings.web_terminal.enabled = false;
        let _ = state.settings_store.update(settings);
    }

    Ok(())
}

#[tauri::command]
pub async fn get_web_terminal_status(state: State<'_, AppState>) -> Result<WebTerminalInfo, String> {
    let server_guard = state.web_terminal_server.lock().await;
    let settings = state.settings_store.get();
    let token = settings.web_terminal.access_token.clone().unwrap_or_default();
    let running = server_guard.is_some();
    Ok(build_info(running, settings.web_terminal.port, &token))
}

#[tauri::command]
pub async fn regenerate_web_terminal_token(state: State<'_, AppState>) -> Result<WebTerminalInfo, String> {
    // Stop current server if running
    {
        let mut server_guard = state.web_terminal_server.lock().await;
        if let Some(server) = server_guard.take() {
            server.stop();
        }
    }

    // Generate new token and save
    let new_token = generate_token();
    let mut settings = state.settings_store.get();
    settings.web_terminal.access_token = Some(new_token.clone());
    state.settings_store.update(settings.clone()).map_err(|e| format!("Failed to save settings: {}", e))?;

    // Restart if was enabled
    if settings.web_terminal.enabled {
        let server = WebTerminalServer::start(
            settings.web_terminal.port,
            new_token.clone(),
            state.exec_manager.clone(),
            state.projects_store.clone(),
            state.settings_store.clone(),
        )
        .await?;
        let mut server_guard = state.web_terminal_server.lock().await;
        *server_guard = Some(server);
        return Ok(build_info(true, settings.web_terminal.port, &new_token));
    }

    Ok(build_info(false, settings.web_terminal.port, &new_token))
}
