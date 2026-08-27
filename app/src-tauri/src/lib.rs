mod auth_bridge;
mod browser_view;
mod commands;
mod docker;
mod install_helper;
mod logging;
mod models;
mod project_lock;
mod storage;
pub mod web_terminal;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use auth_bridge::AuthBridgeManager;
use docker::exec::ExecSessionManager;
use storage::projects_store::ProjectsStore;
use storage::settings_store::SettingsStore;
use tauri::async_runtime::JoinHandle;
use tauri::{Emitter, Manager};
use tokio::sync::watch;
use web_terminal::WebTerminalServer;

pub struct AppState {
    pub projects_store: Arc<ProjectsStore>,
    pub settings_store: Arc<SettingsStore>,
    pub exec_manager: Arc<ExecSessionManager>,
    pub auth_bridge: Arc<AuthBridgeManager>,
    pub web_terminal_server: Arc<tokio::sync::Mutex<Option<WebTerminalServer>>>,
    pub lifecycle: Arc<Lifecycle>,
    /// The file `preview_settings_import` last decrypted successfully, held
    /// so `apply_settings_import` can re-read and re-decrypt the same file
    /// without the frontend ever passing a host path back to Rust as an
    /// argument — see the doc comment on `commands::settings_export_commands`
    /// for why that direction specifically is the one this app treats as
    /// dangerous. Deliberately re-decrypted rather than cached in plaintext:
    /// nothing here holds a decrypted secret in memory for longer than one
    /// command's execution.
    pub pending_settings_import: Arc<tokio::sync::Mutex<Option<std::path::PathBuf>>>,
}

// ─────────────────────────────────────────────────────────────────────────────
// Startup / shutdown coordination
// ─────────────────────────────────────────────────────────────────────────────

/// Total wall-clock budget for teardown before the process exits regardless.
///
/// Six teardown steps used to run *serially* inside a `block_on` on the
/// window-event thread with no timeout: two container stops at Docker's default
/// 10s grace, a `docker exec` per browser-view project, and every bollard call
/// inheriting a 120s client timeout. Quitting after Docker Desktop had already
/// gone away froze the window for minutes. Nothing here is worth more than a
/// few seconds of a user's exit.
const SHUTDOWN_BUDGET: Duration = Duration::from_secs(8);

/// How long the in-flight auto-start tasks get to notice cancellation before
/// they are aborted. They only have to reach their next await point.
const STARTUP_CANCEL_BUDGET: Duration = Duration::from_secs(3);

/// Backoff (seconds) between auto-start attempts. Docker Desktop routinely
/// takes 30-60s to accept API calls after login, which is exactly the window in
/// which Triple-C used to be launched, fail once, and stay broken for the whole
/// session.
const AUTOSTART_DELAYS: [u64; 8] = [0, 2, 4, 8, 15, 15, 30, 30];

/// Owns the "is the app going away?" signal and the handles of the background
/// tasks started during `setup`.
///
/// Both auto-starts are fire-and-forget, and quitting quickly used to race
/// them: `CloseRequested` stopped a gateway container that did not exist yet,
/// and the detached task then created and started it *after* the app was gone —
/// leaving an orphan proxy holding a provider key. The same shape orphaned the
/// web terminal, whose task wrote its server into the state slot that
/// `CloseRequested` had already `take()`-n. Shutdown therefore cancels and
/// waits for these tasks *before* running teardown, so teardown always sees the
/// final state of the world.
pub struct Lifecycle {
    cancel: watch::Sender<bool>,
    tasks: Mutex<Vec<JoinHandle<()>>>,
    shutting_down: AtomicBool,
}

impl Lifecycle {
    fn new() -> Self {
        let (cancel, _) = watch::channel(false);
        Self {
            cancel,
            tasks: Mutex::new(Vec::new()),
            shutting_down: AtomicBool::new(false),
        }
    }

    /// A receiver that flips to `true` when the app starts shutting down.
    pub fn cancellation(&self) -> watch::Receiver<bool> {
        self.cancel.subscribe()
    }

    pub fn is_shutting_down(&self) -> bool {
        *self.cancel.borrow()
    }

    /// Register a startup task so shutdown can wait for it.
    fn track(&self, handle: JoinHandle<()>) {
        self.tasks
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(handle);
    }

    /// `true` the first time only — the window can emit `CloseRequested` again
    /// once we ask the app to exit, and teardown must not restart.
    fn begin_shutdown(&self) -> bool {
        if self.shutting_down.swap(true, Ordering::SeqCst) {
            return false;
        }
        // `send_replace`, not `send`: `send` reports an error *and leaves the
        // value untouched* when nothing is subscribed, which is exactly the
        // case when neither auto-start is enabled — and `is_shutting_down` (the
        // web terminal's check) reads that stored value.
        self.cancel.send_replace(true);
        true
    }

    /// Let the tracked startup tasks unwind, then abort whatever is left.
    async fn settle_startup_tasks(&self) {
        let mut handles: Vec<JoinHandle<()>> = std::mem::take(
            &mut *self.tasks.lock().unwrap_or_else(|e| e.into_inner()),
        );
        if handles.is_empty() {
            return;
        }
        let settle = async {
            for handle in &mut handles {
                let _ = handle.await;
            }
        };
        if tokio::time::timeout(STARTUP_CANCEL_BUDGET, settle).await.is_err() {
            log::warn!("Startup tasks did not settle in time — aborting them");
            for handle in &handles {
                handle.abort();
            }
        }
    }
}

/// Run an auto-start until it succeeds, the app quits, or the retries run out.
///
/// Without this a launch that beats the Docker daemon (or Docker Desktop) to
/// readiness left the gateway and STT down for the entire session, with no
/// path back: nothing re-attempts them.
async fn autostart_with_retry<F, Fut>(label: &str, mut cancel: watch::Receiver<bool>, mut attempt: F)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<(), String>>,
{
    for (index, delay) in AUTOSTART_DELAYS.iter().enumerate() {
        if *delay > 0 {
            tokio::select! {
                _ = cancel.changed() => return,
                _ = tokio::time::sleep(Duration::from_secs(*delay)) => {}
            }
        }
        if *cancel.borrow() {
            return;
        }

        // Cancellation races the attempt itself, not just the backoff, so a
        // quick quit isn't held up by an in-flight Docker call — and, more
        // importantly, so the attempt cannot complete after teardown has run.
        let result = tokio::select! {
            _ = cancel.changed() => return,
            r = attempt() => r,
        };

        match result {
            Ok(()) => {
                if index > 0 {
                    log::info!("{} auto-start succeeded on attempt {}", label, index + 1);
                }
                return;
            }
            Err(e) => {
                let last = index + 1 == AUTOSTART_DELAYS.len();
                if index == 0 {
                    log::warn!("{} auto-start failed ({}) — will retry", label, e);
                } else if last {
                    log::error!("{} auto-start gave up after {} attempts: {}", label, index + 1, e);
                } else {
                    log::debug!("{} auto-start attempt {} failed: {}", label, index + 1, e);
                }
            }
        }
    }
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
    let lifecycle = Arc::new(Lifecycle::new());

    // Clone Arcs for the setup closure (web terminal auto-start)
    let projects_store_setup = projects_store.clone();
    let settings_store_setup = settings_store.clone();
    let exec_manager_setup = exec_manager.clone();
    let lifecycle_setup = lifecycle.clone();

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .manage(AppState {
            projects_store,
            settings_store,
            exec_manager,
            auth_bridge,
            web_terminal_server: Arc::new(tokio::sync::Mutex::new(None)),
            lifecycle,
            pending_settings_import: Arc::new(tokio::sync::Mutex::new(None)),
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

            // ── Startup disk housekeeping ────────────────────────────────
            // Until now the only sweep ran *after* a recreation, so a user who
            // simply stopped launching a project kept its orphaned snapshot
            // layers forever, and anything a crash left behind (a probe
            // container pinning a base image, a rollback pin whose migration
            // record is gone) had no path back at all. All of it is
            // read-mostly and finishes in well under a second on an idle daemon,
            // but they are detached anyway: housekeeping must never delay the
            // window appearing, and a daemon that is not running yet is a
            // logged warning rather than a failed start.
            //
            // Ordering matters. Probes are removed first because a probe holds
            // an image open and the sweep will not force; pins are untagged
            // second so the images they were holding are dangling by the time
            // the sweep lists them; the sweep runs last and collects both.
            let projects_store_for_cleanup = projects_store_setup.clone();
            tauri::async_runtime::spawn(async move {
                crate::docker::reap_probe_containers().await;
                let reaped = crate::docker::reap_stale_migration_pins().await;
                if reaped > 0 {
                    log::info!("Startup housekeeping dropped {} stale rollback pin(s)", reaped);
                }
                crate::docker::sweep_orphaned_snapshots_logged("startup").await;
                // A container/image/volume `remove_project` could not delete
                // is recorded rather than lost — see triple-c#31 — and this is
                // the only place anything ever retries it. Takes the store so
                // it can refuse to touch a project that turns out to still be
                // live — see the long comment on the function itself.
                crate::commands::project_commands::retry_pending_cleanup_logged(
                    &projects_store_for_cleanup,
                )
                .await;
            });

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
                    let lifecycle = lifecycle_setup.clone();

                    let handle = tauri::async_runtime::spawn(async move {
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
                                // The app may have been asked to quit while the
                                // server was coming up, in which case teardown
                                // has already emptied this slot and would never
                                // look at it again. Stop it here instead of
                                // storing an orphan.
                                if lifecycle.is_shutting_down() {
                                    server.stop();
                                    log::info!("Web terminal stopped immediately: app is exiting");
                                    return;
                                }
                                let mut guard = web_server_mutex.lock().await;
                                *guard = Some(server);
                                log::info!("Web terminal auto-started on port {}", port);
                            }
                            Err(e) => {
                                log::error!("Failed to auto-start web terminal: {}", e);
                            }
                        }
                    });
                    lifecycle_setup.track(handle);
                }
            }

            // Auto-start STT container if enabled in settings
            if settings.stt.enabled {
                let stt_settings = settings.stt.clone();
                let cancel = lifecycle_setup.cancellation();
                let handle = tauri::async_runtime::spawn(async move {
                    autostart_with_retry("STT container", cancel, || async {
                        let status = docker::stt::ensure_stt_running(&stt_settings).await?;
                        if status.running {
                            log::info!("STT container auto-started on port {}", stt_settings.port);
                            Ok(())
                        } else {
                            Err("container not running after ensure_stt_running".to_string())
                        }
                    })
                    .await;
                });
                lifecycle_setup.track(handle);
            }

            // Auto-start model gateway container if enabled in settings
            if settings.gateway.enabled {
                let gateway_settings = settings.gateway.clone();
                let cancel = lifecycle_setup.cancellation();
                let handle = tauri::async_runtime::spawn(async move {
                    autostart_with_retry("Model gateway", cancel, || async {
                        let status =
                            docker::gateway::ensure_gateway_running(&gateway_settings).await?;
                        if status.running {
                            log::info!(
                                "Model gateway auto-started on port {}",
                                gateway_settings.port
                            );
                            Ok(())
                        } else {
                            Err("container not running after ensure_gateway_running".to_string())
                        }
                    })
                    .await;
                });
                lifecycle_setup.track(handle);
            }

            Ok(())
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                // This handler fires for *every* window, and what follows stops
                // containers and exits the process. Only the main window means
                // that. Secondary windows — the browser view's pop-out — are
                // closed and reopened freely and must just close.
                if window.label() != "main" {
                    return;
                }

                let state = window.state::<AppState>();
                let lifecycle = state.lifecycle.clone();

                // Already shutting down: let the window close. That covers our
                // own `exit` unwinding it, and it deliberately leaves a second
                // click on the X as a force-quit — teardown is a courtesy, not
                // a hostage situation.
                if !lifecycle.begin_shutdown() {
                    return;
                }

                let exec_manager = state.exec_manager.clone();
                let auth_bridge = state.auth_bridge.clone();
                let web_terminal_server = state.web_terminal_server.clone();
                drop(state);

                // Teardown talks to Docker, so it cannot be instant. Keep the
                // window alive and tell the UI what is happening rather than
                // blocking the event thread on it and looking hung.
                api.prevent_close();
                let _ = window.emit("app-shutting-down", ());

                let app_handle = window.app_handle().clone();
                tauri::async_runtime::spawn(async move {
                    let teardown = async {
                        // First: let the auto-starts unwind. Anything they are
                        // midway through creating has to exist before the stops
                        // below run, or it outlives the app.
                        lifecycle.settle_startup_tasks().await;

                        // Then everything else, concurrently — these touch
                        // different subsystems and nothing here depends on
                        // another's result. Serially, the two container stops
                        // alone were 20s of Docker's default grace period.
                        let web_terminal = async {
                            if let Some(server) = web_terminal_server.lock().await.take() {
                                server.stop();
                            }
                        };
                        let stop_stt = async {
                            if let Err(e) = docker::stt::stop_stt_container().await {
                                log::warn!("Failed to stop the STT container on exit: {}", e);
                            }
                        };
                        let stop_gateway = async {
                            if let Err(e) = docker::gateway::stop_gateway_container().await {
                                log::warn!("Failed to stop the model gateway on exit: {}", e);
                            }
                        };
                        tokio::join!(
                            web_terminal,
                            stop_stt,
                            stop_gateway,
                            exec_manager.close_all_sessions(),
                            auth_bridge.stop_all(),
                            browser_view::manager().stop_all(),
                        );
                    };

                    if tokio::time::timeout(SHUTDOWN_BUDGET, teardown).await.is_err() {
                        log::warn!(
                            "Shutdown exceeded {}s — exiting with teardown incomplete",
                            SHUTDOWN_BUDGET.as_secs()
                        );
                    }
                    app_handle.exit(0);
                });
            }
        })
        .invoke_handler(tauri::generate_handler![
            // Docker
            commands::docker_commands::check_docker,
            commands::docker_commands::check_image_exists,
            commands::docker_commands::build_image,
            commands::docker_commands::get_container_info,
            // Projects
            commands::project_commands::list_projects,
            commands::project_commands::add_project,
            commands::project_commands::remove_project,
            commands::project_commands::update_project,
            commands::project_commands::start_project_container,
            commands::project_commands::stop_project_container,
            commands::project_commands::rebuild_project_container,
            commands::project_commands::reconcile_project_statuses,
            // Container base-image migration
            commands::migration_commands::get_container_staleness,
            commands::migration_commands::migrate_project_to_base,
            commands::migration_commands::confirm_migration,
            commands::migration_commands::rollback_migration,
            commands::migration_commands::get_migration_state,
            // Auth bridge
            commands::auth_bridge_commands::set_auth_bridge_enabled,
            commands::auth_bridge_commands::get_auth_bridge_status,
            // Browser view (Playwright dashboard pane)
            browser_view::commands::set_browser_view_enabled,
            browser_view::commands::get_browser_view_status,
            browser_view::commands::check_browser_view_support,
            browser_view::commands::install_browser_view_support,
            browser_view::commands::install_browser_view_browser,
            browser_view::commands::open_browser_view_popout,
            browser_view::commands::close_browser_view_popout,
            browser_view::commands::get_browser_view_popout_state,
            browser_view::commands::set_browser_view_popout_always_on_top,
            browser_view::commands::open_page_in_container_browser,
            browser_view::commands::set_container_page_viewport,
            browser_view::commands::get_container_page_state,
            browser_view::commands::close_container_page,
            browser_view::commands::set_browser_view_match_window,
            browser_view::commands::get_browser_view_match_window,
            // Shared Claude Code auth token
            commands::auth_token_commands::acquire_claude_token,
            commands::auth_token_commands::submit_claude_token_code,
            commands::auth_token_commands::cancel_claude_token,
            commands::auth_token_commands::has_claude_token,
            commands::auth_token_commands::clear_claude_token,
            commands::auth_token_commands::sweep_claude_token_snapshots,
            // Settings
            commands::settings_commands::get_settings,
            commands::settings_commands::update_settings,
            commands::settings_commands::pull_image,
            commands::settings_commands::detect_aws_config,
            commands::settings_commands::inspect_ca_cert_path,
            commands::settings_commands::list_aws_profiles,
            commands::settings_commands::detect_host_timezone,
            // Settings export/import
            commands::settings_export_commands::export_settings,
            commands::settings_export_commands::preview_settings_import,
            commands::settings_export_commands::apply_settings_import,
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
            commands::file_commands::download_container_backup,
            commands::file_commands::download_container_file,
            commands::file_commands::upload_files_to_container,
            commands::file_commands::read_container_file,
            commands::file_commands::rename_container_path,
            commands::file_commands::create_container_directory,
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
            // Model gateway (LiteLLM)
            commands::gateway_commands::get_gateway_status,
            commands::gateway_commands::start_gateway,
            commands::gateway_commands::stop_gateway,
            commands::gateway_commands::check_gateway_health,
            commands::gateway_commands::build_gateway_image,
            commands::gateway_commands::pull_gateway_image,
            commands::gateway_commands::set_gateway_api_key,
            commands::gateway_commands::clear_gateway_api_key,
            commands::gateway_commands::get_gateway_auth_token,
            commands::gateway_commands::regenerate_gateway_auth_token,
            // Container introspection (sessions / capabilities / scheduler)
            commands::inspect_commands::list_claude_sessions,
            commands::inspect_commands::resume_session_command,
            commands::inspect_commands::list_container_capabilities,
            commands::inspect_commands::list_scheduled_tasks,
            commands::inspect_commands::add_scheduled_task,
            commands::inspect_commands::update_scheduled_task,
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    /// Drives the retry loop under a paused clock, so the real backoff schedule
    /// is exercised without waiting for it.
    async fn run_autostart(
        cancel: watch::Receiver<bool>,
        outcomes: Vec<Result<(), String>>,
    ) -> usize {
        let calls = Arc::new(AtomicUsize::new(0));
        let counter = calls.clone();
        let outcomes = Arc::new(Mutex::new(outcomes.into_iter()));
        autostart_with_retry("test", cancel, move || {
            let counter = counter.clone();
            let outcomes = outcomes.clone();
            async move {
                counter.fetch_add(1, Ordering::SeqCst);
                outcomes
                    .lock()
                    .unwrap()
                    .next()
                    .unwrap_or(Err("still down".to_string()))
            }
        })
        .await;
        calls.load(Ordering::SeqCst)
    }

    #[tokio::test(start_paused = true)]
    async fn a_working_autostart_runs_exactly_once() {
        let (_tx, rx) = watch::channel(false);
        assert_eq!(run_autostart(rx, vec![Ok(())]).await, 1);
    }

    #[tokio::test(start_paused = true)]
    async fn an_autostart_that_beat_docker_to_readiness_recovers() {
        // The regression: Docker not being up yet used to cost the whole
        // session — gateway down, STT down, and nothing ever retried.
        let (_tx, rx) = watch::channel(false);
        let calls = run_autostart(
            rx,
            vec![
                Err("daemon not running".to_string()),
                Err("daemon not running".to_string()),
                Ok(()),
            ],
        )
        .await;
        assert_eq!(calls, 3);
    }

    #[tokio::test(start_paused = true)]
    async fn a_permanently_failing_autostart_gives_up_rather_than_looping_forever() {
        let (_tx, rx) = watch::channel(false);
        assert_eq!(
            run_autostart(rx, vec![]).await,
            AUTOSTART_DELAYS.len(),
            "should attempt once per backoff step and then stop"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_quick_quit_stops_the_retries_before_they_start() {
        // Quitting before the first attempt must not leave a task that creates
        // and starts a container after teardown has already run.
        let (tx, rx) = watch::channel(false);
        tx.send(true).unwrap();
        assert_eq!(run_autostart(rx, vec![Ok(())]).await, 0);
    }

    #[tokio::test(start_paused = true)]
    async fn cancelling_between_attempts_stops_the_retries() {
        let (tx, rx) = watch::channel(false);
        let calls = Arc::new(AtomicUsize::new(0));
        let counter = calls.clone();
        autostart_with_retry("test", rx, move || {
            let counter = counter.clone();
            let tx = tx.clone();
            async move {
                counter.fetch_add(1, Ordering::SeqCst);
                // The app starts quitting while this attempt is in flight.
                let _ = tx.send(true);
                Err("daemon not running".to_string())
            }
        })
        .await;
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn shutdown_begins_exactly_once() {
        // `CloseRequested` fires again when our own `exit(0)` unwinds the
        // window; teardown must not start a second time.
        let lifecycle = Lifecycle::new();
        assert!(!lifecycle.is_shutting_down());
        assert!(lifecycle.begin_shutdown());
        assert!(lifecycle.is_shutting_down());
        assert!(!lifecycle.begin_shutdown());
    }

    #[tokio::test]
    async fn beginning_shutdown_notifies_already_running_startup_tasks() {
        let lifecycle = Lifecycle::new();
        let mut cancel = lifecycle.cancellation();
        assert!(!*cancel.borrow());
        lifecycle.begin_shutdown();
        assert!(cancel.changed().await.is_ok());
        assert!(*cancel.borrow());
    }

    #[tokio::test(start_paused = true)]
    async fn a_startup_task_that_ignores_cancellation_is_abandoned_not_awaited() {
        // The budget is what keeps a wedged auto-start from turning quit into a
        // multi-minute freeze.
        let lifecycle = Lifecycle::new();
        lifecycle.track(tauri::async_runtime::spawn(async {
            tokio::time::sleep(Duration::from_secs(600)).await;
        }));
        lifecycle.begin_shutdown();
        let started = tokio::time::Instant::now();
        lifecycle.settle_startup_tasks().await;
        assert!(started.elapsed() <= STARTUP_CANCEL_BUDGET + Duration::from_secs(1));
    }

    /// The capability file is the app's entire IPC attack surface, and it is
    /// data — nothing in `cargo test` reads it, so a widened grant lands with a
    /// green suite. This is what noticing looks like.
    ///
    /// It exists because `core:default` was granted for months. That alias
    /// pulls in `core:image:default` → `allow-from-path`, which is an
    /// unconditional `std::fs::read` of any host path with no scope check, and
    /// nothing in the frontend has ever imported `@tauri-apps/api/image`.
    /// Every `#[tauri::command]` is registered, and every registration names a
    /// command that exists.
    ///
    /// This is the shape of the bug that caused the original OAuth-callback
    /// complaint: `set_auth_bridge_enabled` existed, worked, and had a typed
    /// frontend wrapper — with **zero call sites**. The switch the docs told
    /// users to flip was never wired to anything, so the bridge stayed off and
    /// every login callback was refused. Nothing failed; the feature was simply
    /// absent, and no test noticed because both halves compiled.
    ///
    /// The reverse direction matters too, and for a sharper reason: a command
    /// that is registered but reachable from nowhere is still IPC surface a
    /// compromised webview can call. `list_sibling_containers` — which returned
    /// every container on the daemon, including the user's unrelated work —
    /// sat in exactly that state, and this test is what found it. It has since
    /// been removed at all four levels: registration, command, docker helper,
    /// and the frontend wrapper and type.
    ///
    /// So this asserts the two lists agree, and leaves *deciding* what belongs
    /// on them to a human. It cannot see frontend call sites; `tsc` and the
    /// vitest suite cover that side.
    #[test]
    fn every_command_is_registered_exactly_once() {
        use std::collections::BTreeSet;

        let mut defined: BTreeSet<String> = BTreeSet::new();

        // Walk the source tree for the command attribute and take the `fn` name
        // that follows.
        //
        // The first version of this matched `line.trim() == "#[tauri::command]"`
        // exactly and broke on the first non-`#` line. An audit got five real,
        // compiling, unregistered commands past it — `#[tauri::command(async)]`,
        // `#[tauri::command(rename_all = "snake_case")]`, a trailing comment,
        // spaces in the path, and a bare `#[command]` after `use tauri::command`
        // — plus `pub(crate) fn` and a `///` line between attribute and `fn`.
        // Every one of those is a command the frontend could not call, which is
        // the bug this test exists for, and the test stayed green.
        //
        // The asymmetry matters: confusion on the *definition* side is a silent
        // pass, while on the *registration* side it fails loudly against
        // legitimate code — and rustc already covers that direction. So this
        // errs toward over-matching definitions.
        fn collect(dir: &std::path::Path, out: &mut BTreeSet<String>) {
            let Ok(entries) = std::fs::read_dir(dir) else { return };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    collect(&path, out);
                } else if path.extension().is_some_and(|e| e == "rs") {
                    let Ok(text) = std::fs::read_to_string(&path) else { continue };
                    let lines: Vec<&str> = text.lines().collect();
                    for (i, line) in lines.iter().enumerate() {
                        let t = line.trim();
                        // `#[tauri::command]`, `#[tauri::command(async)]`,
                        // `#[tauri :: command]`, a bare `#[command]` under
                        // `use tauri::command`, and any of those with a
                        // trailing comment.
                        let attr = t.strip_prefix("#[").map(|a| {
                            a.split(']').next().unwrap_or("").replace(' ', "")
                        });
                        let is_command_attr = attr.is_some_and(|a| {
                            a == "command" || a == "tauri::command"
                                || a.starts_with("command(")
                                || a.starts_with("tauri::command(")
                        });
                        if !is_command_attr {
                            continue;
                        }
                        // Skip further attributes and doc comments rather than
                        // giving up at the first line that is not an attribute.
                        for next in lines.iter().skip(i + 1) {
                            let t = next.trim();
                            if t.starts_with('#') || t.starts_with("//") || t.is_empty() {
                                continue;
                            }
                            // Any visibility, then `fn` or `async fn`.
                            let after_vis = t
                                .strip_prefix("pub(crate) ")
                                .or_else(|| t.strip_prefix("pub(super) "))
                                .or_else(|| t.strip_prefix("pub(in crate) "))
                                .or_else(|| t.strip_prefix("pub "))
                                .unwrap_or(t);
                            let after_async =
                                after_vis.strip_prefix("async ").unwrap_or(after_vis);
                            if let Some(rest) = after_async.strip_prefix("fn ") {
                                if let Some(name) = rest.split(['(', '<']).next() {
                                    out.insert(name.trim().to_string());
                                }
                            }
                            break;
                        }
                    }
                }
            }
        }
        collect(
            std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/src")),
            &mut defined,
        );

        // The registration list, read from this file rather than from a macro
        // expansion so the test does not depend on `generate_handler!`'s shape.
        let this = include_str!("lib.rs");
        let handler = this
            .split_once("generate_handler![")
            .and_then(|(_, rest)| rest.split_once("])"))
            .map(|(inside, _)| inside)
            .expect("lib.rs should contain a generate_handler! list");
        // Line-based, not `split(',')`: the list is grouped under `// Docker`
        // style comments, and splitting on commas glues each comment to the
        // command that follows it. A `starts_with("//")` filter then drops that
        // command — silently, and once per group.
        let registered: BTreeSet<String> = handler
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with("//"))
            .filter_map(|l| {
                l.trim_end_matches(',')
                    .rsplit("::")
                    .next()
                    .map(|n| n.trim().to_string())
            })
            .filter(|n| !n.is_empty())
            .collect();

        assert!(
            !defined.is_empty() && !registered.is_empty(),
            "the scan found nothing — it has stopped testing anything (defined={}, registered={})",
            defined.len(),
            registered.len()
        );

        let unregistered: Vec<&String> = defined.difference(&registered).collect();
        assert!(
            unregistered.is_empty(),
            "these commands exist but are not registered, so the frontend cannot call them: {:?}",
            unregistered
        );

        let undefined: Vec<&String> = registered.difference(&defined).collect();
        assert!(
            undefined.is_empty(),
            "these are registered but no `#[tauri::command]` defines them: {:?}",
            undefined
        );

        // "exactly once" was in this test's name and not in its body: both
        // sides were sets, so registering the same command twice in a
        // hand-maintained 118-line list compiled, warned about nothing, and
        // passed here.
        let mut seen: Vec<&str> = Vec::new();
        let mut duplicated: Vec<&str> = Vec::new();
        for line in handler
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with("//"))
        {
            if let Some(name) = line.trim_end_matches(',').rsplit("::").next() {
                let name = name.trim();
                if name.is_empty() {
                    continue;
                }
                if seen.contains(&name) {
                    duplicated.push(name);
                } else {
                    seen.push(name);
                }
            }
        }
        assert!(
            duplicated.is_empty(),
            "these are registered more than once: {:?}",
            duplicated
        );
    }

    #[test]
    fn the_capability_grants_are_the_ones_that_were_reviewed() {
        let raw = include_str!("../capabilities/default.json");
        let parsed: serde_json::Value =
            serde_json::from_str(raw).expect("capabilities/default.json must parse");
        let listed: Vec<String> = parsed["permissions"]
            .as_array()
            .expect("a `permissions` array")
            .iter()
            .map(|p| match p {
                // A scoped grant is an object; its identifier is what matters here.
                serde_json::Value::Object(o) => o["identifier"]
                    .as_str()
                    .expect("a scoped grant needs an identifier")
                    .to_string(),
                other => other.as_str().expect("a grant is a string or an object").to_string(),
            })
            .collect();

        let mut sorted = listed.clone();
        sorted.sort();
        let mut expected = vec![
            "core:event:allow-listen",
            "core:event:allow-unlisten",
            "core:webview:allow-internal-toggle-devtools",
            "dialog:allow-open",
            "dialog:allow-save",
            "opener:allow-open-url",
        ];
        expected.sort();
        assert_eq!(
            sorted, expected,
            "the capability set changed. That is allowed — but it is the IPC \
             surface a compromised webview can call, so update this list \
             deliberately rather than to make the test pass."
        );

        // Belt and braces: the `*:default` aliases are the specific trap here,
        // because they expand to a set the file never spells out. `store:*` in
        // particular was an arbitrary host-file read/write primitive.
        for grant in &listed {
            assert!(
                !grant.ends_with(":default"),
                "{} is an alias — it expands to permissions this file does not \
                 name. Enumerate them instead.",
                grant
            );
            assert!(
                !grant.starts_with("store:"),
                "store:* is `PathBuf::push` against AppData, which an absolute \
                 path discards: an arbitrary host-file read/write."
            );
        }
    }
}
