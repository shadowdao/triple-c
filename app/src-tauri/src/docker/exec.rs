use bollard::exec::{CreateExecOptions, ResizeExecOptions, StartExecResults};
use futures_util::StreamExt;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::io::AsyncWriteExt;
use tokio::sync::{mpsc, Mutex};

use super::client::get_docker;

pub struct ExecSession {
    pub exec_id: String,
    pub input_tx: mpsc::UnboundedSender<Vec<u8>>,
    shutdown_tx: mpsc::Sender<()>,
}

impl ExecSession {
    pub async fn send_input(&self, data: Vec<u8>) -> Result<(), String> {
        self.input_tx
            .send(data)
            .map_err(|e| format!("Failed to send input: {}", e))
    }

    pub async fn resize(&self, cols: u16, rows: u16) -> Result<(), String> {
        let docker = get_docker()?;
        docker
            .resize_exec(
                &self.exec_id,
                ResizeExecOptions {
                    width: cols,
                    height: rows,
                },
            )
            .await
            .map_err(|e| format!("Failed to resize exec: {}", e))
    }

    pub fn shutdown(&self) {
        let _ = self.shutdown_tx.try_send(());
    }
}

pub struct ExecSessionManager {
    sessions: Arc<Mutex<HashMap<String, ExecSession>>>,
}

impl ExecSessionManager {
    pub fn new() -> Self {
        Self {
            sessions: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub async fn create_session<F>(
        &self,
        container_id: &str,
        session_id: &str,
        cmd: Vec<String>,
        on_output: F,
        on_exit: Box<dyn FnOnce() + Send>,
    ) -> Result<(), String>
    where
        F: Fn(Vec<u8>) + Send + 'static,
    {
        let docker = get_docker()?;

        let exec = docker
            .create_exec(
                container_id,
                CreateExecOptions {
                    attach_stdin: Some(true),
                    attach_stdout: Some(true),
                    attach_stderr: Some(true),
                    tty: Some(true),
                    cmd: Some(cmd),
                    working_dir: Some("/workspace".to_string()),
                    ..Default::default()
                },
            )
            .await
            .map_err(|e| format!("Failed to create exec: {}", e))?;

        let exec_id = exec.id.clone();

        let result = docker
            .start_exec(&exec_id, None)
            .await
            .map_err(|e| format!("Failed to start exec: {}", e))?;

        let (input_tx, mut input_rx) = mpsc::unbounded_channel::<Vec<u8>>();
        let (shutdown_tx, mut shutdown_rx) = mpsc::channel::<()>(1);

        match result {
            StartExecResults::Attached { mut output, mut input } => {
                // Output reader task
                let session_id_clone = session_id.to_string();
                let shutdown_tx_clone = shutdown_tx.clone();
                tokio::spawn(async move {
                    loop {
                        tokio::select! {
                            msg = output.next() => {
                                match msg {
                                    Some(Ok(output)) => {
                                        on_output(output.into_bytes().to_vec());
                                    }
                                    Some(Err(e)) => {
                                        log::error!("Exec output error for {}: {}", session_id_clone, e);
                                        break;
                                    }
                                    None => {
                                        log::info!("Exec output stream ended for {}", session_id_clone);
                                        break;
                                    }
                                }
                            }
                            _ = shutdown_rx.recv() => {
                                log::info!("Exec session {} shutting down", session_id_clone);
                                break;
                            }
                        }
                    }
                    on_exit();
                    let _ = shutdown_tx_clone;
                });

                // Input writer task
                tokio::spawn(async move {
                    while let Some(data) = input_rx.recv().await {
                        if let Err(e) = input.write_all(&data).await {
                            log::error!("Failed to write to exec stdin: {}", e);
                            break;
                        }
                    }
                });
            }
            StartExecResults::Detached => {
                return Err("Exec started in detached mode".to_string());
            }
        }

        let session = ExecSession {
            exec_id,
            input_tx,
            shutdown_tx,
        };

        self.sessions
            .lock()
            .await
            .insert(session_id.to_string(), session);

        Ok(())
    }

    pub async fn send_input(&self, session_id: &str, data: Vec<u8>) -> Result<(), String> {
        let sessions = self.sessions.lock().await;
        let session = sessions
            .get(session_id)
            .ok_or_else(|| format!("Session {} not found", session_id))?;
        session.send_input(data).await
    }

    pub async fn resize(&self, session_id: &str, cols: u16, rows: u16) -> Result<(), String> {
        let sessions = self.sessions.lock().await;
        let session = sessions
            .get(session_id)
            .ok_or_else(|| format!("Session {} not found", session_id))?;
        session.resize(cols, rows).await
    }

    pub async fn close_session(&self, session_id: &str) {
        let mut sessions = self.sessions.lock().await;
        if let Some(session) = sessions.remove(session_id) {
            session.shutdown();
        }
    }

    pub async fn close_all_sessions(&self) {
        let mut sessions = self.sessions.lock().await;
        for (_, session) in sessions.drain() {
            session.shutdown();
        }
    }
}
