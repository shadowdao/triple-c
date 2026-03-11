mod commands;
mod docker;
mod logging;
mod models;
mod storage;

use docker::exec::ExecSessionManager;
use storage::projects_store::ProjectsStore;
use storage::settings_store::SettingsStore;
use storage::mcp_store::McpStore;
use tauri::Manager;

pub struct AppState {
    pub projects_store: ProjectsStore,
    pub settings_store: SettingsStore,
    pub mcp_store: McpStore,
    pub exec_manager: ExecSessionManager,
}

pub fn run() {
    logging::init();

    let projects_store = match ProjectsStore::new() {
        Ok(s) => s,
        Err(e) => {
            log::error!("Failed to initialize projects store: {}", e);
            panic!("Failed to initialize projects store: {}", e);
        }
    };
    let settings_store = match SettingsStore::new() {
        Ok(s) => s,
        Err(e) => {
            log::error!("Failed to initialize settings store: {}", e);
            panic!("Failed to initialize settings store: {}", e);
        }
    };
    let mcp_store = match McpStore::new() {
        Ok(s) => s,
        Err(e) => {
            log::error!("Failed to initialize MCP store: {}", e);
            panic!("Failed to initialize MCP store: {}", e);
        }
    };

    tauri::Builder::default()
        .plugin(tauri_plugin_store::Builder::default().build())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .manage(AppState {
            projects_store,
            settings_store,
            mcp_store,
            exec_manager: ExecSessionManager::new(),
        })
        .setup(|app| {
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
            Ok(())
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { .. } = event {
                let state = window.state::<AppState>();
                tauri::async_runtime::block_on(async {
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
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
