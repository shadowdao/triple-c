use bollard::container::UploadToContainerOptions;
use bollard::exec::{CreateExecOptions, ResizeExecOptions, StartExecResults};
use futures_util::StreamExt;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::io::AsyncWriteExt;
use tokio::sync::{mpsc, Mutex};

use super::client::get_docker;

pub struct ExecSession {
    pub exec_id: String,
    pub container_id: String,
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
        self.create_session_with_tty(container_id, session_id, cmd, true, on_output, on_exit)
            .await
    }

    pub async fn create_session_with_tty<F>(
        &self,
        container_id: &str,
        session_id: &str,
        cmd: Vec<String>,
        tty: bool,
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
                    tty: Some(tty),
                    cmd: Some(cmd),
                    user: Some("claude".to_string()),
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
            container_id: container_id.to_string(),
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
        // Clone the exec_id under the lock, then drop the lock before the
        // async Docker API call to avoid holding the mutex across await.
        let exec_id = {
            let sessions = self.sessions.lock().await;
            let session = sessions
                .get(session_id)
                .ok_or_else(|| format!("Session {} not found", session_id))?;
            session.exec_id.clone()
        };
        let docker = get_docker()?;
        docker
            .resize_exec(
                &exec_id,
                ResizeExecOptions {
                    width: cols,
                    height: rows,
                },
            )
            .await
            .map_err(|e| format!("Failed to resize exec: {}", e))
    }

    pub async fn close_session(&self, session_id: &str) {
        let mut sessions = self.sessions.lock().await;
        if let Some(session) = sessions.remove(session_id) {
            session.shutdown();
        }
    }

    pub async fn close_sessions_for_container(&self, container_id: &str) {
        let mut sessions = self.sessions.lock().await;
        let ids_to_close: Vec<String> = sessions
            .iter()
            .filter(|(_, s)| s.container_id == container_id)
            .map(|(id, _)| id.clone())
            .collect();
        for id in ids_to_close {
            if let Some(session) = sessions.remove(&id) {
                session.shutdown();
            }
        }
    }

    pub async fn close_all_sessions(&self) {
        let mut sessions = self.sessions.lock().await;
        for (_, session) in sessions.drain() {
            session.shutdown();
        }
    }

    pub async fn get_container_id(&self, session_id: &str) -> Result<String, String> {
        let sessions = self.sessions.lock().await;
        let session = sessions
            .get(session_id)
            .ok_or_else(|| format!("Session {} not found", session_id))?;
        Ok(session.container_id.clone())
    }

    pub async fn write_file_to_container(
        &self,
        container_id: &str,
        file_name: &str,
        data: &[u8],
    ) -> Result<String, String> {
        let docker = get_docker()?;

        // Build a tar archive in memory containing the file
        let mut tar_buf = Vec::new();
        {
            let mut builder = tar::Builder::new(&mut tar_buf);
            let mut header = tar::Header::new_gnu();
            header.set_size(data.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder
                .append_data(&mut header, file_name, data)
                .map_err(|e| format!("Failed to create tar entry: {}", e))?;
            builder
                .finish()
                .map_err(|e| format!("Failed to finalize tar: {}", e))?;
        }

        docker
            .upload_to_container(
                container_id,
                Some(UploadToContainerOptions {
                    path: "/tmp".to_string(),
                    ..Default::default()
                }),
                tar_buf.into(),
            )
            .await
            .map_err(|e| format!("Failed to upload file to container: {}", e))?;

        Ok(format!("/tmp/{}", file_name))
    }
}

/// Run a one-shot (non-interactive) exec command in a container and collect stdout.
pub async fn exec_oneshot(container_id: &str, cmd: Vec<String>) -> Result<String, String> {
    let docker = get_docker()?;

    let exec = docker
        .create_exec(
            container_id,
            CreateExecOptions {
                attach_stdout: Some(true),
                attach_stderr: Some(true),
                cmd: Some(cmd),
                user: Some("claude".to_string()),
                ..Default::default()
            },
        )
        .await
        .map_err(|e| format!("Failed to create exec: {}", e))?;

    let result = docker
        .start_exec(&exec.id, None)
        .await
        .map_err(|e| format!("Failed to start exec: {}", e))?;

    match result {
        StartExecResults::Attached { mut output, .. } => {
            let mut stdout = String::new();
            while let Some(msg) = output.next().await {
                match msg {
                    Ok(data) => stdout.push_str(&String::from_utf8_lossy(&data.into_bytes())),
                    Err(e) => return Err(format!("Exec output error: {}", e)),
                }
            }
            Ok(stdout)
        }
        StartExecResults::Detached => Err("Exec started in detached mode".to_string()),
    }
}
