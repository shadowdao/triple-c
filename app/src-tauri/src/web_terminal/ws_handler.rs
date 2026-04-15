use std::sync::Arc;

use axum::extract::ws::{Message, WebSocket};
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

use crate::commands::aws_commands;
use crate::models::{Backend, BedrockAuthMethod, Project, ProjectStatus};

use super::server::WebTerminalState;

// ── Wire protocol types ──────────────────────────────────────────────

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ClientMessage {
    ListProjects,
    Open {
        project_id: String,
        session_type: Option<String>,
    },
    Input {
        session_id: String,
        data: String, // base64
    },
    Resize {
        session_id: String,
        cols: u16,
        rows: u16,
    },
    Close {
        session_id: String,
    },
    Ping,
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ServerMessage {
    Projects {
        projects: Vec<ProjectEntry>,
    },
    Opened {
        session_id: String,
        project_name: String,
    },
    Output {
        session_id: String,
        data: String, // base64
    },
    Exit {
        session_id: String,
    },
    Error {
        message: String,
    },
    Pong,
}

#[derive(Serialize)]
struct ProjectEntry {
    id: String,
    name: String,
    status: String,
}

// ── Connection handler ───────────────────────────────────────────────

pub async fn handle_connection(socket: WebSocket, state: Arc<WebTerminalState>) {
    let (mut ws_tx, mut ws_rx) = socket.split();

    // Channel for sending messages from session output tasks → WS writer
    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<ServerMessage>();

    // Track session IDs owned by this connection for cleanup
    let owned_sessions: Arc<tokio::sync::Mutex<Vec<String>>> =
        Arc::new(tokio::sync::Mutex::new(Vec::new()));

    // Writer task: serializes ServerMessages and sends as WS text frames
    let writer_handle = tokio::spawn(async move {
        while let Some(msg) = out_rx.recv().await {
            if let Ok(json) = serde_json::to_string(&msg) {
                if ws_tx.send(Message::Text(json.into())).await.is_err() {
                    break;
                }
            }
        }
    });

    // Reader loop: parse incoming messages and dispatch
    while let Some(Ok(msg)) = ws_rx.next().await {
        let text = match &msg {
            Message::Text(t) => t.to_string(),
            Message::Close(_) => break,
            _ => continue,
        };

        let client_msg: ClientMessage = match serde_json::from_str(&text) {
            Ok(m) => m,
            Err(e) => {
                let _ = out_tx.send(ServerMessage::Error {
                    message: format!("Invalid message: {}", e),
                });
                continue;
            }
        };

        match client_msg {
            ClientMessage::Ping => {
                let _ = out_tx.send(ServerMessage::Pong);
            }

            ClientMessage::ListProjects => {
                let projects = state.projects_store.list();
                let entries: Vec<ProjectEntry> = projects
                    .into_iter()
                    .map(|p| ProjectEntry {
                        id: p.id,
                        name: p.name,
                        status: serde_json::to_value(&p.status)
                            .ok()
                            .and_then(|v| v.as_str().map(|s| s.to_string()))
                            .unwrap_or_else(|| "unknown".to_string()),
                    })
                    .collect();
                let _ = out_tx.send(ServerMessage::Projects { projects: entries });
            }

            ClientMessage::Open {
                project_id,
                session_type,
            } => {
                let result = handle_open(
                    &state,
                    &project_id,
                    session_type.as_deref(),
                    &out_tx,
                    &owned_sessions,
                )
                .await;
                if let Err(e) = result {
                    let _ = out_tx.send(ServerMessage::Error { message: e });
                }
            }

            ClientMessage::Input { session_id, data } => {
                match BASE64.decode(&data) {
                    Ok(bytes) => {
                        if let Err(e) = state.exec_manager.send_input(&session_id, bytes).await {
                            let _ = out_tx.send(ServerMessage::Error {
                                message: format!("Input error: {}", e),
                            });
                        }
                    }
                    Err(e) => {
                        let _ = out_tx.send(ServerMessage::Error {
                            message: format!("Base64 decode error: {}", e),
                        });
                    }
                }
            }

            ClientMessage::Resize {
                session_id,
                cols,
                rows,
            } => {
                if let Err(e) = state.exec_manager.resize(&session_id, cols, rows).await {
                    let _ = out_tx.send(ServerMessage::Error {
                        message: format!("Resize error: {}", e),
                    });
                }
            }

            ClientMessage::Close { session_id } => {
                state.exec_manager.close_session(&session_id).await;
                // Remove from owned list
                owned_sessions
                    .lock()
                    .await
                    .retain(|id| id != &session_id);
            }
        }
    }

    // Connection closed — clean up all owned sessions
    log::info!("Web terminal WebSocket disconnected, cleaning up sessions");
    let sessions = owned_sessions.lock().await.clone();
    for session_id in sessions {
        state.exec_manager.close_session(&session_id).await;
    }

    writer_handle.abort();
}

/// Build the command for a terminal session, mirroring terminal_commands.rs logic.
fn build_terminal_cmd(project: &Project, settings_store: &crate::storage::settings_store::SettingsStore) -> Vec<String> {
    let is_bedrock_profile = project.backend == Backend::Bedrock
        && project
            .bedrock_config
            .as_ref()
            .map(|b| b.auth_method == BedrockAuthMethod::Profile)
            .unwrap_or(false);

    if !is_bedrock_profile {
        let mut cmd = vec!["claude".to_string()];
        if project.full_permissions {
            cmd.push("--dangerously-skip-permissions".to_string());
        }
        return cmd;
    }

    let profile = aws_commands::resolve_profile_for_project(
        project,
        settings_store.get().global_aws.aws_profile.as_deref(),
    );

    let claude_cmd = if project.full_permissions {
        "exec claude --dangerously-skip-permissions"
    } else {
        "exec claude"
    };

    let script = format!(
        r#"
echo "Validating AWS session for profile '{profile}'..."
if aws sts get-caller-identity --profile '{profile}' >/dev/null 2>&1; then
    echo "AWS session valid."
else
    echo "AWS session expired or invalid."
    if aws configure get sso_start_url --profile '{profile}' >/dev/null 2>&1 || \
       aws configure get sso_session --profile '{profile}' >/dev/null 2>&1; then
        echo "Starting SSO login..."
        echo ""
        triple-c-sso-refresh
        if [ $? -ne 0 ]; then
            echo ""
            echo "SSO login failed or was cancelled. Starting Claude anyway..."
            echo "You may see authentication errors."
            echo ""
        fi
    else
        echo "Profile '{profile}' does not use SSO. Check your AWS credentials."
        echo "Starting Claude anyway..."
        echo ""
    fi
fi
{claude_cmd}
"#,
        profile = profile,
        claude_cmd = claude_cmd
    );

    vec!["bash".to_string(), "-c".to_string(), script]
}

/// Open a new terminal session for a project.
async fn handle_open(
    state: &WebTerminalState,
    project_id: &str,
    session_type: Option<&str>,
    out_tx: &mpsc::UnboundedSender<ServerMessage>,
    owned_sessions: &Arc<tokio::sync::Mutex<Vec<String>>>,
) -> Result<(), String> {
    let project = state
        .projects_store
        .get(project_id)
        .ok_or_else(|| format!("Project {} not found", project_id))?;

    if project.status != ProjectStatus::Running {
        return Err(format!("Project '{}' is not running", project.name));
    }

    let container_id = project
        .container_id
        .as_ref()
        .ok_or_else(|| "Container not running".to_string())?;

    let cmd = match session_type {
        Some("bash") => vec!["bash".to_string(), "-l".to_string()],
        _ => build_terminal_cmd(&project, &state.settings_store),
    };

    let session_id = uuid::Uuid::new_v4().to_string();
    let project_name = project.name.clone();

    // Set up output routing through the WS channel
    let out_tx_output = out_tx.clone();
    let session_id_output = session_id.clone();
    let on_output = move |data: Vec<u8>| {
        let encoded = BASE64.encode(&data);
        let _ = out_tx_output.send(ServerMessage::Output {
            session_id: session_id_output.clone(),
            data: encoded,
        });
    };

    let out_tx_exit = out_tx.clone();
    let session_id_exit = session_id.clone();
    let on_exit = Box::new(move || {
        let _ = out_tx_exit.send(ServerMessage::Exit {
            session_id: session_id_exit,
        });
    });

    state
        .exec_manager
        .create_session(container_id, &session_id, cmd, on_output, on_exit)
        .await?;

    // Track this session for cleanup on disconnect
    owned_sessions.lock().await.push(session_id.clone());

    let _ = out_tx.send(ServerMessage::Opened {
        session_id,
        project_name,
    });

    Ok(())
}
