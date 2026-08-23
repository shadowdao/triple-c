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

        // Owned by the container user, stamped now: a default tar header would
        // land it as root:root/1970 and Claude Code could not rewrite it.
        let (uid, gid) = container_user_ids(container_id).await;
        let tar_buf = build_single_file_tar(file_name, data, 0o644, uid, gid, now_epoch_secs())?;

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

/// Ceiling on one host file packed into a container upload.
///
/// The file goes through host RAM twice — once as bytes, once inside the tar —
/// so this is a memory bound, and it is checked against the *descriptor* that
/// was opened rather than a `metadata` call that described whatever the path
/// meant a moment earlier.
pub const MAX_DROP_BYTES: u64 = 256 * 1024 * 1024;

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
    let (uid, gid) = container_user_ids(container_id).await;
    let mtime = now_epoch_secs();

    let tar_buf = tokio::task::spawn_blocking(move || -> Result<Vec<u8>, String> {
        // The caller resolved this path (`resolve_host_read_path`); opening it
        // is a second trip through the same directories, so the descriptor is
        // checked against the path that was validated before its bytes are
        // packed into anything. Same policy as the Files pane's upload — this
        // is the terminal's drop target, and the two must not differ.
        let file = std::fs::File::open(&host_path)
            .map_err(|e| format!("Failed to read {}: {}", host_path, e))?;
        crate::commands::file_commands::verify_opened_path(
            &file,
            std::path::Path::new(&host_path),
        )?;
        let mut data = Vec::new();
        std::io::Read::read_to_end(
            &mut std::io::Read::take(file, MAX_DROP_BYTES.saturating_add(1)),
            &mut data,
        )
        .map_err(|e| format!("Failed to read {}: {}", host_path, e))?;
        if data.len() as u64 > MAX_DROP_BYTES {
            return Err(format!(
                "File too large to upload (limit {} MB)",
                MAX_DROP_BYTES / (1024 * 1024)
            ));
        }
        build_single_file_tar(&dest_for_blk, &data[..], 0o644, uid, gid, mtime)
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

    // Root-owned on purpose: the only caller is migration, whose `tar -T` list
    // is read back as root. The mtime still gets stamped so the file doesn't
    // read as 1970.
    let tar_buf = build_single_file_tar(file_name, data, mode, 0, 0, now_epoch_secs())?;

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

/// Build an in-memory tar archive holding a single regular file.
///
/// The uid/gid/mtime arguments exist because `tar::Header::new_gnu()` zeroes
/// them and Docker's archive extractor honours the header verbatim: a header
/// left at the defaults lands the file inside the container as `root:root`
/// with a 1970-01-01 mtime — not writable by `claude`, and confusing in any
/// listing. Callers that upload on a user's behalf should pass the container
/// user's ids from [`container_user_ids`].
pub fn build_single_file_tar(
    file_name: &str,
    data: &[u8],
    mode: u32,
    uid: u64,
    gid: u64,
    mtime: u64,
) -> Result<Vec<u8>, String> {
    let mut tar_buf = Vec::with_capacity(data.len() + 1024);
    {
        let mut builder = tar::Builder::new(&mut tar_buf);
        let mut header = tar::Header::new_gnu();
        // Size comes from the bytes in hand, so header and payload can't disagree.
        header.set_size(data.len() as u64);
        header.set_mode(mode);
        header.set_uid(uid);
        header.set_gid(gid);
        header.set_mtime(mtime);
        header.set_cksum();
        builder
            .append_data(&mut header, file_name, data)
            .map_err(|e| format!("Failed to create tar entry: {}", e))?;
        builder
            .finish()
            .map_err(|e| format!("Failed to finalize tar: {}", e))?;
    }
    Ok(tar_buf)
}

/// Seconds since the Unix epoch, for a tar header mtime.
pub fn now_epoch_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// The numeric uid/gid of the container's `claude` user.
///
/// It is not a constant: `entrypoint.sh` remaps `claude` to the *host* user's
/// ids on Unix so bind-mounted project files stay writable, and deliberately
/// does not on Windows. So the only reliable answer comes from asking the
/// container. Falls back to 1000:1000 (the image's build-time ids) if the exec
/// fails, which is strictly better than the 0:0 a default tar header carries.
pub async fn container_user_ids(container_id: &str) -> (u64, u64) {
    let out = exec_oneshot_limited(
        container_id,
        vec!["sh".to_string(), "-c".to_string(), "id -u; id -g".to_string()],
        256,
    )
    .await
    .unwrap_or_default();

    let mut ids = out.lines().filter_map(|l| l.trim().parse::<u64>().ok());
    match (ids.next(), ids.next()) {
        (Some(uid), Some(gid)) => (uid, gid),
        _ => (1000, 1000),
    }
}

/// Ceiling on how much container output a one-shot exec will buffer into the
/// host process.
///
/// Every `exec_oneshot*` call reads the whole stream into a `String` before any
/// caller sees a byte, and what it is reading is *container-controlled* — the
/// scheduler notifications reader `cat`s up to 50 files with no size cap, and
/// the auth bridge reads `/proc/net/tcp` every two seconds. Neither has an
/// upstream bound, so this is where the bound goes. Generous enough that no
/// legitimate reader (the largest is a package manifest of a full image) comes
/// close.
pub const MAX_ONESHOT_OUTPUT: usize = 8 * 1024 * 1024;

/// The auth bridge's per-tick budget. It reads two procfs files whose rows are
/// ~150 bytes; a real container has tens of listeners, and the parser only ever
/// yields at most one entry per port number. 1 MiB is thousands of rows — far
/// past anything genuine, far short of a problem.
pub const PROC_NET_OUTPUT_LIMIT: usize = 1024 * 1024;

/// Marker on the "that command printed more than this will buffer" refusal.
///
/// The byte count on its own is a fact about the transport, not about what the
/// user did — "Command output exceeded 8388608 bytes" is not a sentence anybody
/// can act on. A caller that knows what it was reading can recognise this and
/// say the useful thing instead; see `list_container_files`, where the real
/// cause is a directory with more entries than the panel can render.
pub const OUTPUT_LIMIT_MARKER: &str = "OUTPUT_LIMIT";

/// Append to `buf` while it stays inside `limit`, returning the range the chunk
/// now occupies. `None` once the limit is exceeded, at which point the caller
/// must stop reading — and nothing is appended, so a caller that ignored the
/// answer cannot parse a half-read document.
///
/// Bytes rather than `str` on purpose: Docker frames a stream wherever it
/// likes, so a chunk boundary can fall inside a UTF-8 sequence. Decoding each
/// chunk on its own turned that into two replacement characters in the middle
/// of a filename; the decode happens once, at the end, over the whole buffer.
fn push_capped(buf: &mut Vec<u8>, chunk: &[u8], limit: usize) -> Option<(usize, usize)> {
    if buf.len() + chunk.len() > limit {
        return None;
    }
    let start = buf.len();
    buf.extend_from_slice(chunk);
    Some((start, buf.len()))
}

/// Run a one-shot (non-interactive) exec command in a container and collect stdout.
pub async fn exec_oneshot(container_id: &str, cmd: Vec<String>) -> Result<String, String> {
    exec_oneshot_env(container_id, cmd, Vec::new()).await
}

/// [`exec_oneshot`] with a caller-chosen output ceiling, for readers whose
/// input is fully container-controlled and whose legitimate output is small.
pub async fn exec_oneshot_limited(
    container_id: &str,
    cmd: Vec<String>,
    limit: usize,
) -> Result<String, String> {
    exec_oneshot_inner(container_id, "claude", cmd, Vec::new(), limit)
        .await
        .map(|(output, _)| output)
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
    exec_oneshot_inner(container_id, user, cmd, env, MAX_ONESHOT_OUTPUT).await
}

/// What a one-shot exec printed, with the two streams still tellable apart.
///
/// `combined` is stdout and stderr interleaved in arrival order — the shape
/// every existing caller reads, and the right one for surfacing "why did that
/// fail". `stdout_ranges` indexes the parts of it that came from stdout, so a
/// caller that is *parsing* output can have just that without the buffer being
/// held twice.
struct OneshotOutput {
    combined: Vec<u8>,
    stdout_ranges: Vec<(usize, usize)>,
    exit_code: i64,
}

impl OneshotOutput {
    /// Everything the command printed, in the order it printed it.
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.combined).into_owned()
    }

    /// stdout alone — for callers that parse it, where a diagnostic spliced in
    /// mid-record is a parse error at best.
    fn stdout(&self) -> String {
        let mut out = Vec::with_capacity(self.combined.len());
        for (start, end) in &self.stdout_ranges {
            out.extend_from_slice(&self.combined[*start..*end]);
        }
        String::from_utf8_lossy(&out).into_owned()
    }

    /// stderr alone — the complement of [`Self::stdout`], i.e. the diagnostics.
    fn stderr(&self) -> String {
        let mut out = Vec::with_capacity(self.combined.len());
        let mut cursor = 0usize;
        for (start, end) in &self.stdout_ranges {
            out.extend_from_slice(&self.combined[cursor..*start]);
            cursor = *end;
        }
        out.extend_from_slice(&self.combined[cursor..]);
        String::from_utf8_lossy(&out).into_owned()
    }
}

/// [`exec_oneshot_as`] with the two streams kept apart, for callers that parse
/// stdout.
///
/// `find`'s own diagnostics ("Permission denied") used to arrive inside the
/// records its `-printf` was emitting. GNU `find` escapes tabs and newlines in
/// those messages, so the listing parser held — but "the parser holds" is not
/// the same as "the input is trustworthy", and the fix costs one enum match.
pub async fn exec_oneshot_streams_as(
    container_id: &str,
    user: &str,
    cmd: Vec<String>,
    env: Vec<String>,
) -> Result<(String, String, i64), String> {
    let out = exec_oneshot_raw(container_id, user, cmd, env, MAX_ONESHOT_OUTPUT).await?;
    Ok((out.stdout(), out.stderr(), out.exit_code))
}

async fn exec_oneshot_inner(
    container_id: &str,
    user: &str,
    cmd: Vec<String>,
    env: Vec<String>,
    limit: usize,
) -> Result<(String, i64), String> {
    let out = exec_oneshot_raw(container_id, user, cmd, env, limit).await?;
    Ok((out.text(), out.exit_code))
}

async fn exec_oneshot_raw(
    container_id: &str,
    user: &str,
    cmd: Vec<String>,
    env: Vec<String>,
    limit: usize,
) -> Result<OneshotOutput, String> {
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

    let mut combined: Vec<u8> = Vec::new();
    let mut stdout_ranges: Vec<(usize, usize)> = Vec::new();
    match result {
        StartExecResults::Attached { mut output, .. } => {
            while let Some(msg) = output.next().await {
                match msg {
                    Ok(data) => {
                        let from_stdout = matches!(data, LogOutput::StdOut { .. });
                        let bytes = data.into_bytes();
                        match push_capped(&mut combined, &bytes, limit) {
                            Some(range) => {
                                if from_stdout {
                                    stdout_ranges.push(range);
                                }
                            }
                            // Stop reading rather than truncate silently: every
                            // caller parses this output, and a half-read
                            // manifest or JSON array is worse than an error.
                            // Dropping `output` kills the exec's stream.
                            None => {
                                return Err(format!(
                                    "{}: Command output exceeded {} bytes and was abandoned",
                                    OUTPUT_LIMIT_MARKER, limit
                                ))
                            }
                        }
                    }
                    Err(e) => return Err(format!("Exec output error: {}", e)),
                }
            }
        }
        StartExecResults::Detached => return Err("Exec started in detached mode".to_string()),
    }

    // The output stream draining doesn't strictly guarantee inspect_exec has the
    // final exit_code populated yet, so poll until the exec reports finished.
    let exit_code = require_exit_code(wait_for_exec_exit(&exec.id).await)?;

    Ok(OneshotOutput {
        combined,
        stdout_ranges,
        exit_code,
    })
}

/// Turn "the exit code could not be determined" into an error rather than a 0.
///
/// `unwrap_or(0)` is how a rename that never happened reported success: callers
/// branch on `code != 0`, so an unreadable status silently became "it worked",
/// the UI closed its rename box and the file had not moved. An exec whose
/// outcome cannot be established has not been established to have succeeded —
/// fail closed and let the caller surface it.
///
/// The `test -e` probe in `rename_container_path` also fails closed under this:
/// it propagates the error instead of reading an undeterminable status as
/// "the destination does not exist".
fn require_exit_code(code: Option<i64>) -> Result<i64, String> {
    code.ok_or_else(|| {
        "Could not determine whether the command finished (Docker did not report an exit status)"
            .to_string()
    })
}

/// Poll `inspect_exec` until the exec reports finished and return its exit code.
/// Returns `None` if the code can't be determined (inspect error, or the exec
/// doesn't report finished within ~5s — which shouldn't happen once its output
/// stream has drained).
///
/// The window is generous because `None` is no longer a shrug: since
/// [`require_exit_code`], it fails the whole call. Waiting a few seconds longer
/// for a busy daemon to settle costs nothing in the normal case — the loop exits
/// on the first poll that reports finished — and it is the difference between a
/// spurious "the rename failed" and a real one.
pub async fn wait_for_exec_exit(exec_id: &str) -> Option<i64> {
    let docker = get_docker().ok()?;
    for _ in 0..200 {
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The frames a demultiplexed exec hands back, as `(is_stdout, bytes)`.
    fn collect(frames: &[(bool, &[u8])]) -> OneshotOutput {
        let mut combined = Vec::new();
        let mut stdout_ranges = Vec::new();
        for (from_stdout, bytes) in frames {
            let range = push_capped(&mut combined, bytes, usize::MAX).unwrap();
            if *from_stdout {
                stdout_ranges.push(range);
            }
        }
        OneshotOutput {
            combined,
            stdout_ranges,
            exit_code: 0,
        }
    }

    #[test]
    fn output_under_the_limit_is_buffered_whole() {
        let mut buf = Vec::new();
        assert_eq!(push_capped(&mut buf, b"hello ", 16), Some((0, 6)));
        assert_eq!(push_capped(&mut buf, b"world", 16), Some((6, 11)));
        assert_eq!(buf, b"hello world");
    }

    #[test]
    fn output_over_the_limit_is_refused_rather_than_truncated() {
        // The abandoned chunk must not land in the buffer either: a caller that
        // ignored the error would otherwise parse a half-read document.
        let mut buf = Vec::new();
        assert!(push_capped(&mut buf, b"0123456789", 12).is_some());
        assert!(push_capped(&mut buf, b"0123456789", 12).is_none());
        assert_eq!(buf, b"0123456789");
    }

    #[test]
    fn a_single_oversized_chunk_is_refused() {
        let mut buf = Vec::new();
        assert!(push_capped(&mut buf, b"0123456789", 4).is_none());
        assert!(buf.is_empty());
    }

    #[test]
    fn a_character_split_across_two_frames_survives_the_decode() {
        // Docker frames a stream wherever it likes, and a filename is where
        // that shows: decoding each chunk on its own turned the two halves of
        // `ü` into two replacement characters in the middle of a name.
        let out = collect(&[(true, &[0xc3]), (true, &[0xbc, b'.', b't', b'x', b't'])]);
        assert_eq!(out.stdout(), "ü.txt");
        assert_eq!(out.text(), "ü.txt");
    }

    #[test]
    fn a_diagnostic_never_lands_in_the_stream_a_caller_parses() {
        // `find`'s "Permission denied" used to arrive inside the records its
        // `-printf` was emitting. Arrival order is still available for the
        // error message; the parser gets stdout alone.
        let out = collect(&[
            (true, b"first"),
            (false, b"find: /x: Permission denied\n"),
            (true, b"second"),
        ]);
        assert_eq!(out.stdout(), "firstsecond");
        assert_eq!(out.stderr(), "find: /x: Permission denied\n");
        assert_eq!(out.text(), "firstfind: /x: Permission denied\nsecond");
    }

    #[test]
    fn an_output_limit_refusal_is_marked_so_a_caller_can_reword_it() {
        // "Command output exceeded 8388608 bytes" is a fact about a buffer.
        // The marker is what lets `list_container_files` say "too many entries"
        // instead, which is the thing that actually happened.
        assert!(!OUTPUT_LIMIT_MARKER.is_empty());
        let refusal = format!(
            "{}: Command output exceeded {} bytes and was abandoned",
            OUTPUT_LIMIT_MARKER, MAX_ONESHOT_OUTPUT
        );
        assert!(refusal.starts_with(OUTPUT_LIMIT_MARKER));
    }

    #[test]
    fn an_undeterminable_exit_status_is_an_error_not_a_zero() {
        // The bug this guards: `unwrap_or(0)` made every caller that branches on
        // `code != 0` — rename, mkdir — report success for an exec whose outcome
        // nobody could read.
        assert_eq!(require_exit_code(Some(0)).unwrap(), 0);
        assert_eq!(require_exit_code(Some(1)).unwrap(), 1);
        assert!(require_exit_code(None).is_err());
    }

    #[test]
    fn the_bridge_budget_is_far_smaller_than_the_general_one() {
        // The auth bridge re-reads container-controlled procfs every 2s, so it
        // gets a tighter ceiling than one-shot readers that run on demand.
        assert!(PROC_NET_OUTPUT_LIMIT < MAX_ONESHOT_OUTPUT);
        // …but still comfortably above a genuine /proc/net/tcp{,6} pair.
        assert!(PROC_NET_OUTPUT_LIMIT > 100 * 150);
    }
}
