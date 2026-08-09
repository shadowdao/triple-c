mod auth_bridge;
mod commands;
mod docker;
mod install_helper;
mod logging;
mod models;
mod storage;
pub mod web_terminal;

use std::sync::Arc;

use auth_bridge::AuthBridgeManager;
use docker::exec::ExecSessionManager;
use storage::projects_store::ProjectsStore;
use storage::settings_store::SettingsStore;
use tauri::Manager;
use web_terminal::WebTerminalServer;

pub struct AppState {
    pub projects_store: Arc<ProjectsStore>,
    pub settings_store: Arc<SettingsStore>,
    pub exec_manager: Arc<ExecSessionManager>,
    pub auth_bridge: Arc<AuthBridgeManager>,
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
    let exec_manager = Arc::new(ExecSessionManager::new());
    let auth_bridge = Arc::new(AuthBridgeManager::new());

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
            exec_manager,
            auth_bridge,
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

            // Auto-start STT container if enabled in settings
            if settings.stt.enabled {
                let stt_settings = settings.stt.clone();
                tauri::async_runtime::spawn(async move {
                    match docker::stt::ensure_stt_running(&stt_settings).await {
                        Ok(status) => {
                            if status.running {
                                log::info!("STT container auto-started on port {}", stt_settings.port);
                            } else {
                                log::warn!("STT auto-start: container not running after ensure_stt_running");
                            }
                        }
                        Err(e) => {
                            log::error!("Failed to auto-start STT container: {}", e);
                        }
                    }
                });
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
                    // Stop STT container
                    let _ = docker::stt::stop_stt_container().await;
                    // Close all exec sessions
                    state.exec_manager.close_all_sessions().await;
                    // Release every host loopback port held by the auth bridge
                    state.auth_bridge.stop_all().await;
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
            // Auth bridge
            commands::auth_bridge_commands::set_auth_bridge_enabled,
            commands::auth_bridge_commands::get_auth_bridge_status,
            // Shared Claude Code auth token
            commands::auth_token_commands::acquire_claude_token,
            commands::auth_token_commands::submit_claude_token_code,
            commands::auth_token_commands::cancel_claude_token,
            commands::auth_token_commands::has_claude_token,
            commands::auth_token_commands::clear_claude_token,
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
            commands::terminal_commands::upload_host_file_to_terminal,
            commands::terminal_commands::start_audio_bridge,
            commands::terminal_commands::send_audio_data,
            commands::terminal_commands::stop_audio_bridge,
            // Files
            commands::file_commands::list_container_files,
            commands::file_commands::download_container_file,
            commands::file_commands::download_container_backup,
            commands::file_commands::upload_file_to_container,
            // AWS
            commands::aws_commands::aws_sso_refresh,
            // Updates
            commands::update_commands::get_app_version,
            commands::update_commands::check_for_updates,
            commands::update_commands::check_image_update,
            // Help
            commands::help_commands::get_help_content,
            // Install helper
            commands::install_helper_commands::detect_install_options,
            commands::install_helper_commands::run_docker_install,
            // Web Terminal
            commands::web_terminal_commands::start_web_terminal,
            commands::web_terminal_commands::stop_web_terminal,
            commands::web_terminal_commands::get_web_terminal_status,
            commands::web_terminal_commands::regenerate_web_terminal_token,
            // STT
            commands::stt_commands::get_stt_status,
            commands::stt_commands::start_stt,
            commands::stt_commands::stop_stt,
            commands::stt_commands::build_stt_image,
            commands::stt_commands::pull_stt_image,
            commands::stt_commands::transcribe_audio,
            // Container introspection (sessions / capabilities / scheduler)
            commands::inspect_commands::list_claude_sessions,
            commands::inspect_commands::resume_session_command,
            commands::inspect_commands::list_container_capabilities,
            commands::inspect_commands::list_scheduled_tasks,
            commands::inspect_commands::get_scheduled_task_log,
            commands::inspect_commands::set_scheduled_task_enabled,
            commands::inspect_commands::run_scheduled_task_now,
            commands::inspect_commands::remove_scheduled_task,
            commands::inspect_commands::get_scheduler_notifications,
            commands::inspect_commands::clear_scheduler_notifications,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
