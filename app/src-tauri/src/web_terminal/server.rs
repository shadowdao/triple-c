use std::sync::Arc;

use axum::extract::{Query, State as AxumState, WebSocketUpgrade};
use axum::response::{Html, IntoResponse};
use axum::routing::get;
use axum::Router;
use serde::{Deserialize, Serialize};
use tokio::sync::watch;
use tower_http::cors::CorsLayer;

use crate::docker::exec::ExecSessionManager;
use crate::storage::projects_store::ProjectsStore;
use crate::storage::settings_store::SettingsStore;

use super::ws_handler;

/// Shared state passed to all axum handlers.
pub struct WebTerminalState {
    pub exec_manager: Arc<ExecSessionManager>,
    pub projects_store: Arc<ProjectsStore>,
    pub settings_store: Arc<SettingsStore>,
    pub access_token: String,
}

/// Manages the lifecycle of the axum HTTP+WS server.
pub struct WebTerminalServer {
    shutdown_tx: watch::Sender<()>,
    port: u16,
}

#[derive(Deserialize)]
pub struct TokenQuery {
    pub token: Option<String>,
}

#[derive(Serialize)]
struct ProjectInfo {
    id: String,
    name: String,
    status: String,
}

impl WebTerminalServer {
    /// Start the web terminal server on the given port.
    pub async fn start(
        port: u16,
        access_token: String,
        exec_manager: Arc<ExecSessionManager>,
        projects_store: Arc<ProjectsStore>,
        settings_store: Arc<SettingsStore>,
    ) -> Result<Self, String> {
        let (shutdown_tx, shutdown_rx) = watch::channel(());

        let shared_state = Arc::new(WebTerminalState {
            exec_manager,
            projects_store,
            settings_store,
            access_token,
        });

        let app = Router::new()
            .route("/", get(serve_html))
            .route("/ws", get(ws_upgrade))
            .route("/api/projects", get(list_projects))
            .layer(CorsLayer::permissive())
            .with_state(shared_state);

        let addr = format!("0.0.0.0:{}", port);
        let listener = tokio::net::TcpListener::bind(&addr)
            .await
            .map_err(|e| format!("Failed to bind web terminal to {}: {}", addr, e))?;

        log::info!("Web terminal server listening on {}", addr);

        let mut shutdown_rx_clone = shutdown_rx.clone();
        tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(async move {
                    let _ = shutdown_rx_clone.changed().await;
                })
                .await
                .unwrap_or_else(|e| {
                    log::error!("Web terminal server error: {}", e);
                });
            log::info!("Web terminal server shut down");
        });

        Ok(Self { shutdown_tx, port })
    }

    /// Stop the server gracefully.
    pub fn stop(&self) {
        log::info!("Stopping web terminal server on port {}", self.port);
        let _ = self.shutdown_tx.send(());
    }

    pub fn port(&self) -> u16 {
        self.port
    }
}

/// Serve the embedded HTML page.
async fn serve_html() -> Html<&'static str> {
    Html(include_str!("terminal.html"))
}

/// Validate token from query params.
fn validate_token(state: &WebTerminalState, token: &Option<String>) -> bool {
    match token {
        Some(t) => t == &state.access_token,
        None => false,
    }
}

/// WebSocket upgrade handler.
async fn ws_upgrade(
    ws: WebSocketUpgrade,
    AxumState(state): AxumState<Arc<WebTerminalState>>,
    Query(query): Query<TokenQuery>,
) -> impl IntoResponse {
    if !validate_token(&state, &query.token) {
        return (axum::http::StatusCode::UNAUTHORIZED, "Invalid token").into_response();
    }
    ws.on_upgrade(move |socket| ws_handler::handle_connection(socket, state))
        .into_response()
}

/// List running projects (REST endpoint).
async fn list_projects(
    AxumState(state): AxumState<Arc<WebTerminalState>>,
    Query(query): Query<TokenQuery>,
) -> impl IntoResponse {
    if !validate_token(&state, &query.token) {
        return (
            axum::http::StatusCode::UNAUTHORIZED,
            axum::Json(serde_json::json!({"error": "Invalid token"})),
        )
            .into_response();
    }

    let projects = state.projects_store.list();
    let infos: Vec<ProjectInfo> = projects
        .into_iter()
        .map(|p| ProjectInfo {
            id: p.id,
            name: p.name,
            status: serde_json::to_value(&p.status)
                .ok()
                .and_then(|v| v.as_str().map(|s| s.to_string()))
                .unwrap_or_else(|| "unknown".to_string()),
        })
        .collect();

    axum::Json(infos).into_response()
}
