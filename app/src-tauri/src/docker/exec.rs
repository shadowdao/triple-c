use bollard::container::{LogOutput, UploadToContainerOptions};
use bollard::exec::{CreateExecOptions, ResizeExecOptions, StartExecResults};
use futures_util::{Stream, StreamExt};
use std::collections::HashMap;
use std::pin::Pin;
use std::sync::Arc;
use tokio::io::{AsyncWrite, AsyncWriteExt};
use tokio::sync::{mpsc, Mutex};

use super::client::get_docker;

/// A `docker exec` that has been created and started with stdin/stdout/stderr
/// attached — the raw duplex halves, before any policy about what to do with
/// them.
///
/// This is the single place in the codebase that knows how to open an attached
/// exec. Both consumers are built on it:
///   * [`ExecSessionManager`] — interactive terminals and the audio bridge,
///     which pump bytes through mpsc channels and a callback.
///   * `auth_bridge` — per-connection `socat` tunnels, which pump bytes
///     straight between a host TCP socket and these halves.
///
/// With `tty = false` the output stream is demultiplexed by Docker, so the
/// consumer can tell [`LogOutput::StdOut`] from [`LogOutput::StdErr`]. That
/// distinction matters for the auth bridge: `socat`'s diagnostics must not be
/// spliced into the proxied byte stream.
pub struct AttachedExec {
    pub exec_id: String,
    pub output: Pin<Box<dyn Stream<Item = Result<LogOutput, bollard::errors::Error>> + Send>>,
    pub input: Pin<Box<dyn AsyncWrite + Send>>,
}

/// Create and start an exec with stdin + stdout + stderr attached, returning the
/// raw duplex halves. Runs as `claude` in `/workspace`, like every other exec
/// this app opens.
pub async fn create_attached_exec(
    container_id: &str,
    cmd: Vec<String>,
    tty: bool,
) -> Result<AttachedExec, String> {
    create_attached_exec_as(container_id, cmd, tty, "claude", "/workspace").await
}

/// [`create_attached_exec`] with the user and working directory spelled out.
///
/// Only base-image migration needs this: replaying `apt` and unpacking a
/// payload tar at `/` have to run as **root**, and every other caller wants the
/// `claude` / `/workspace` defaults that [`create_attached_exec`] supplies. It
/// stays the single place an attached exec is opened.
pub async fn create_attached_exec_as(
    container_id: &str,
    cmd: Vec<String>,
    tty: bool,
    user: &str,
    working_dir: &str,
) -> Result<AttachedExec, String> {
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
                user: Some(user.to_string()),
                working_dir: Some(working_dir.to_string()),
                ..Default::default()
            },
        )
        .await
        .map_err(|e| format!("Failed to create exec: {}", e))?;

    let exec_id = exec.id.clone();

    match docker
        .start_exec(&exec_id, None)
        .await
        .map_err(|e| format!("Failed to start exec: {}", e))?
    {
        StartExecResults::Attached { output, input } => Ok(AttachedExec {
            exec_id,
            output,
            input,
        }),
        StartExecResults::Detached => Err("Exec started in detached mode".to_string()),
    }
}

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

    #[allow(dead_code)]
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
        let AttachedExec {
            exec_id,
            mut output,
            mut input,
        } = create_attached_exec(container_id, cmd, tty).await?;

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

/// Upload a host file into the container's `/tmp` under `dest_name`. The file is
/// read and packed into the tar inside a blocking task, so the synchronous IO
/// runs off the async worker. The tar's declared entry size is taken from the
/// bytes actually read (not a separate `stat`), so a file changing size between
/// a size check and the read can't desync the header and corrupt the archive.
/// Returns the in-container path (`/tmp/<dest_name>`).
pub async fn upload_host_file_to_container(
    container_id: &str,
    host_path: &str,
    dest_name: &str,
) -> Result<String, String> {
    let host_path = host_path.to_string();
    let dest_name = dest_name.to_string();
    let dest_for_blk = dest_name.clone();

    let tar_buf = tokio::task::spawn_blocking(move || -> Result<Vec<u8>, String> {
        let data = std::fs::read(&host_path)
            .map_err(|e| format!("Failed to read {}: {}", host_path, e))?;
        let mut tar_buf = Vec::with_capacity(data.len() + 1024);
        {
            let mut builder = tar::Builder::new(&mut tar_buf);
            let mut header = tar::Header::new_gnu();
            // Size comes from the bytes in hand, so header and payload can't disagree.
            header.set_size(data.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder
                .append_data(&mut header, &dest_for_blk, &data[..])
                .map_err(|e| format!("Failed to create tar entry: {}", e))?;
            builder
                .finish()
                .map_err(|e| format!("Failed to finalize tar: {}", e))?;
        }
        Ok(tar_buf)
    })
    .await
    .map_err(|e| format!("Upload task panicked: {}", e))??;

    let docker = get_docker()?;
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

    Ok(format!("/tmp/{}", dest_name))
}

/// Write `data` into the container at `<dest_dir>/<file_name>` with `mode`.
///
/// For small, generated files — migration uses it for the `tar -T` include
/// list, which can be too long to pass as argv. Anything large should be
/// streamed through an attached exec's stdin instead, since this buffers the
/// whole payload in memory twice (once raw, once tarred).
pub async fn upload_bytes_to_container(
    container_id: &str,
    dest_dir: &str,
    file_name: &str,
    data: &[u8],
    mode: u32,
) -> Result<String, String> {
    let docker = get_docker()?;

    let mut tar_buf = Vec::with_capacity(data.len() + 1024);
    {
        let mut builder = tar::Builder::new(&mut tar_buf);
        let mut header = tar::Header::new_gnu();
        header.set_size(data.len() as u64);
        header.set_mode(mode);
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
                path: dest_dir.to_string(),
                ..Default::default()
            }),
            tar_buf.into(),
        )
        .await
        .map_err(|e| format!("Failed to upload file to container: {}", e))?;

    Ok(format!("{}/{}", dest_dir.trim_end_matches('/'), file_name))
}

/// Run a one-shot (non-interactive) exec command in a container and collect stdout.
pub async fn exec_oneshot(container_id: &str, cmd: Vec<String>) -> Result<String, String> {
    exec_oneshot_env(container_id, cmd, Vec::new()).await
}

/// Like `exec_oneshot`, but passes additional environment variables to the exec
/// process. Secrets passed this way live only in `/proc/<pid>/environ` (readable
/// by the same user / root) rather than in the process argv, so they are not
/// exposed via `ps`.
///
/// NOTE: the command's exit code is NOT checked — callers that need to know
/// whether the command succeeded should use `exec_oneshot_env_status`.
pub async fn exec_oneshot_env(
    container_id: &str,
    cmd: Vec<String>,
    env: Vec<String>,
) -> Result<String, String> {
    exec_oneshot_env_status(container_id, cmd, env)
        .await
        .map(|(output, _exit_code)| output)
}

/// Like `exec_oneshot_env`, but also returns the command's exit code (0 on
/// success). The returned string contains both stdout and stderr, interleaved
/// in arrival order, which is useful for surfacing failure detail.
pub async fn exec_oneshot_env_status(
    container_id: &str,
    cmd: Vec<String>,
    env: Vec<String>,
) -> Result<(String, i64), String> {
    exec_oneshot_as(container_id, "claude", cmd, env).await
}

/// [`exec_oneshot_env_status`] with the user spelled out.
///
/// Base-image migration is the only caller that needs anything but `claude`:
/// `apt-get`, `npm -g` and the payload unpack all run as **root**. Note that
/// the container does grant `claude` passwordless sudo, but going through
/// `sudo` would put the whole command in `ps` output and add a second failure
/// mode to interpret, so the exec is simply created as root.
pub async fn exec_oneshot_as(
    container_id: &str,
    user: &str,
    cmd: Vec<String>,
    env: Vec<String>,
) -> Result<(String, i64), String> {
    let docker = get_docker()?;

    let exec = docker
        .create_exec(
            container_id,
            CreateExecOptions {
                attach_stdout: Some(true),
                attach_stderr: Some(true),
                cmd: Some(cmd),
                env: if env.is_empty() { None } else { Some(env) },
                user: Some(user.to_string()),
                ..Default::default()
            },
        )
        .await
        .map_err(|e| format!("Failed to create exec: {}", e))?;

    let result = docker
        .start_exec(&exec.id, None)
        .await
        .map_err(|e| format!("Failed to start exec: {}", e))?;

    let mut combined = String::new();
    match result {
        StartExecResults::Attached { mut output, .. } => {
            while let Some(msg) = output.next().await {
                match msg {
                    Ok(data) => combined.push_str(&String::from_utf8_lossy(&data.into_bytes())),
                    Err(e) => return Err(format!("Exec output error: {}", e)),
                }
            }
        }
        StartExecResults::Detached => return Err("Exec started in detached mode".to_string()),
    }

    // The output stream draining doesn't strictly guarantee inspect_exec has the
    // final exit_code populated yet, so poll until the exec reports finished.
    let exit_code = wait_for_exec_exit(&exec.id).await.unwrap_or(0);

    Ok((combined, exit_code))
}

/// Poll `inspect_exec` until the exec reports finished and return its exit code.
/// Returns `None` if the code can't be determined (inspect error, or the exec
/// doesn't report finished within ~1s — which shouldn't happen once its output
/// stream has drained).
pub async fn wait_for_exec_exit(exec_id: &str) -> Option<i64> {
    let docker = get_docker().ok()?;
    for _ in 0..40 {
        match docker.inspect_exec(exec_id).await {
            Ok(info) => {
                if info.running != Some(true) {
                    // Finished: use the reported code (default 0 if somehow absent).
                    return Some(info.exit_code.unwrap_or(0));
                }
            }
            Err(_) => return None,
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    None
}
