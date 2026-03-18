mod commands;
mod docker;
mod logging;
mod models;
mod storage;
pub mod web_terminal;

use std::sync::Arc;

use docker::exec::ExecSessionManager;
use storage::projects_store::ProjectsStore;
use storage::settings_store::SettingsStore;
use storage::mcp_store::McpStore;
use tauri::Manager;
use web_terminal::WebTerminalServer;

pub struct AppState {
    pub projects_store: Arc<ProjectsStore>,
    pub settings_store: Arc<SettingsStore>,
    pub mcp_store: Arc<McpStore>,
    pub exec_manager: Arc<ExecSessionManager>,
    pub web_terminal_server: Arc<tokio::sync::Mutex<Option<WebTerminalServer>>>,
}

pub fn run() {
    logging::init();

    let projects_store = Arc::new(match ProjectsStore::new() {
        Ok(s) => s,
        Err(e) => {
            log::error!("Failed to initialize projects store: {}", e);
            panic!("Failed to initialize projects store: {}", e);
        }
    });
    let settings_store = Arc::new(match SettingsStore::new() {
        Ok(s) => s,
        Err(e) => {
            log::error!("Failed to initialize settings store: {}", e);
            panic!("Failed to initialize settings store: {}", e);
        }
    });
    let mcp_store = Arc::new(match McpStore::new() {
        Ok(s) => s,
        Err(e) => {
            log::error!("Failed to initialize MCP store: {}", e);
            panic!("Failed to initialize MCP store: {}", e);
        }
    });
    let exec_manager = Arc::new(ExecSessionManager::new());

    // Clone Arcs for the setup closure (web terminal auto-start)
    let projects_store_setup = projects_store.clone();
    let settings_store_setup = settings_store.clone();
    let exec_manager_setup = exec_manager.clone();

    tauri::Builder::default()
        .plugin(tauri_plugin_store::Builder::default().build())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .manage(AppState {
            projects_store,
            settings_store,
            mcp_store,
            exec_manager,
            web_terminal_server: Arc::new(tokio::sync::Mutex::new(None)),
        })
        .setup(move |app| {
            match tauri::image::Image::from_bytes(include_bytes!("../icons/icon.png")) {
                Ok(icon) => {
                    if let Some(window) = app.get_webview_window("main") {
                        let _ = window.set_icon(icon);
                    }
                }
                Err(e) => {
                    log::error!("Failed to load window icon: {}", e);
                }
            }

            // Auto-start web terminal server if enabled in settings
            let settings = settings_store_setup.get();
            if settings.web_terminal.enabled {
                if let Some(token) = &settings.web_terminal.access_token {
                    let token = token.clone();
                    let port = settings.web_terminal.port;
                    let exec_mgr = exec_manager_setup.clone();
                    let proj_store = projects_store_setup.clone();
                    let set_store = settings_store_setup.clone();
                    let state = app.state::<AppState>();
                    let web_server_mutex = state.web_terminal_server.clone();

                    tauri::async_runtime::spawn(async move {
                        match WebTerminalServer::start(
                            port,
                            token,
                            exec_mgr,
                            proj_store,
                            set_store,
                        )
                        .await
                        {
                            Ok(server) => {
                                let mut guard = web_server_mutex.lock().await;
                                *guard = Some(server);
                                log::info!("Web terminal auto-started on port {}", port);
                            }
                            Err(e) => {
                                log::error!("Failed to auto-start web terminal: {}", e);
                            }
                        }
                    });
                }
            }

            Ok(())
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { .. } = event {
                let state = window.state::<AppState>();
                tauri::async_runtime::block_on(async {
                    // Stop web terminal server
                    let mut server_guard = state.web_terminal_server.lock().await;
                    if let Some(server) = server_guard.take() {
                        server.stop();
                    }
                    // Close all exec sessions
                    state.exec_manager.close_all_sessions().await;
                });
            }
        })
        .invoke_handler(tauri::generate_handler![
            // Docker
            commands::docker_commands::check_docker,
            commands::docker_commands::check_image_exists,
            commands::docker_commands::build_image,
            commands::docker_commands::get_container_info,
            commands::docker_commands::list_sibling_containers,
            // Projects
            commands::project_commands::list_projects,
            commands::project_commands::add_project,
            commands::project_commands::remove_project,
            commands::project_commands::update_project,
            commands::project_commands::start_project_container,
            commands::project_commands::stop_project_container,
            commands::project_commands::rebuild_project_container,
            commands::project_commands::reconcile_project_statuses,
            // Settings
            commands::settings_commands::get_settings,
            commands::settings_commands::update_settings,
            commands::settings_commands::pull_image,
            commands::settings_commands::detect_aws_config,
            commands::settings_commands::list_aws_profiles,
            commands::settings_commands::detect_host_timezone,
            // Terminal
            commands::terminal_commands::open_terminal_session,
            commands::terminal_commands::terminal_input,
            commands::terminal_commands::terminal_resize,
            commands::terminal_commands::close_terminal_session,
            commands::terminal_commands::paste_image_to_terminal,
            commands::terminal_commands::start_audio_bridge,
            commands::terminal_commands::send_audio_data,
            commands::terminal_commands::stop_audio_bridge,
            // Files
            commands::file_commands::list_container_files,
            commands::file_commands::download_container_file,
            commands::file_commands::upload_file_to_container,
            // MCP
            commands::mcp_commands::list_mcp_servers,
            commands::mcp_commands::add_mcp_server,
            commands::mcp_commands::update_mcp_server,
            commands::mcp_commands::remove_mcp_server,
            // AWS
            commands::aws_commands::aws_sso_refresh,
            // Updates
            commands::update_commands::get_app_version,
            commands::update_commands::check_for_updates,
            commands::update_commands::check_image_update,
            // Help
            commands::help_commands::get_help_content,
            // Web Terminal
            commands::web_terminal_commands::start_web_terminal,
            commands::web_terminal_commands::stop_web_terminal,
            commands::web_terminal_commands::get_web_terminal_status,
            commands::web_terminal_commands::regenerate_web_terminal_token,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
