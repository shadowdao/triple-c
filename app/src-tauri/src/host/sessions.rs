//! Interactive exec sessions — terminals and the audio bridge — keyed by the
//! frontend's session id. Host-neutral: every session holds the
//! [`ContainerTarget`] it was opened on and does all its I/O through it.
//! (Moved here from `docker/exec.rs` in phase 1 of the AWS host work.)

use futures_util::StreamExt;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::io::AsyncWriteExt;
use tokio::sync::{mpsc, Mutex};

use super::{AttachedExec, ContainerTarget};

pub struct ExecSession {
    pub exec_id: String,
    pub target: ContainerTarget,
    pub input_tx: mpsc::UnboundedSender<Vec<u8>>,
    shutdown_tx: mpsc::Sender<()>,
}

impl ExecSession {
    pub async fn send_input(&self, data: Vec<u8>) -> Result<(), String> {
        self.input_tx
            .send(data)
            .map_err(|e| format!("Failed to send input: {}", e))
    }

    #[allow(dead_code)]
    pub async fn resize(&self, cols: u16, rows: u16) -> Result<(), String> {
        self.target.resize_exec(&self.exec_id, cols, rows).await
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
        target: &ContainerTarget,
        session_id: &str,
        cmd: Vec<String>,
        on_output: F,
        on_exit: Box<dyn FnOnce() + Send>,
    ) -> Result<(), String>
    where
        F: Fn(Vec<u8>) + Send + 'static,
    {
        self.create_session_with_tty(target, session_id, cmd, true, on_output, on_exit)
            .await
    }

    pub async fn create_session_with_tty<F>(
        &self,
        target: &ContainerTarget,
        session_id: &str,
        cmd: Vec<String>,
        tty: bool,
        on_output: F,
        on_exit: Box<dyn FnOnce() + Send>,
    ) -> Result<(), String>
    where
        F: Fn(Vec<u8>) + Send + 'static,
    {
        let AttachedExec {
            exec_id,
            mut output,
            mut input,
        } = target.attach_exec(cmd, tty).await?;

        let (input_tx, mut input_rx) = mpsc::unbounded_channel::<Vec<u8>>();
        let (shutdown_tx, mut shutdown_rx) = mpsc::channel::<()>(1);

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

        let session = ExecSession {
            exec_id,
            target: target.clone(),
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
        // async host call to avoid holding the mutex across await.
        let (exec_id, target) = {
            let sessions = self.sessions.lock().await;
            let session = sessions
                .get(session_id)
                .ok_or_else(|| format!("Session {} not found", session_id))?;
            (session.exec_id.clone(), session.target.clone())
        };
        target.resize_exec(&exec_id, cols, rows).await
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
            .filter(|(_, s)| s.target.id() == container_id)
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
        Ok(session.target.id().to_string())
    }

    pub async fn write_file_to_container(
        &self,
        target: &ContainerTarget,
        file_name: &str,
        data: &[u8],
    ) -> Result<String, String> {
        // Owned by the container user, stamped now: a default tar header would
        // land it as root:root/1970 and Claude Code could not rewrite it.
        let ids = target.user_ids().await;
        target
            .upload_bytes_with_ids("/tmp", file_name, data, 0o644, ids)
            .await
    }
}
