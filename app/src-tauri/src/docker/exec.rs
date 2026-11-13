use bollard::container::{LogOutput, UploadToContainerOptions};
use bollard::exec::{CreateExecOptions, ResizeExecOptions, StartExecResults};
use futures_util::StreamExt;

use super::client::get_docker;
use crate::host::{ExecChunk, ExecSpec};

// `AttachedExec` lives in `host` now (host-neutral output chunks); this
// module's job is to produce one from Docker. Its consumers are
// `host::sessions::ExecSessionManager` (terminals, audio bridge) and the auth
// bridge's per-connection `socat` tunnels, both through `host::ContainerTarget`.
// With `tty = false` Docker demultiplexes stdout from stderr, which the auth
// bridge relies on: `socat`'s diagnostics must not be spliced into the proxied
// byte stream.
pub use crate::host::AttachedExec;

/// An attached exec with the user and working directory spelled out.
///
/// Only base-image migration calls this directly: replaying `apt` and unpacking
/// a payload tar at `/` have to run as **root**. Everything else goes through
/// `host::ContainerTarget::attach`.
pub async fn create_attached_exec_as(
    container_id: &str,
    cmd: Vec<String>,
    tty: bool,
    user: &str,
    working_dir: &str,
) -> Result<AttachedExec, String> {
    let spec = ExecSpec::new(cmd)
        .tty(tty)
        .user(user)
        .working_dir(Some(working_dir));
    create_attached_exec_spec(container_id, &spec).await
}

/// The single place an attached exec is opened on Docker. Everything else —
/// [`create_attached_exec_as`] and `host::ContainerTarget::attach` — is a thin
/// wrapper over this.
pub async fn create_attached_exec_spec(
    container_id: &str,
    spec: &ExecSpec,
) -> Result<AttachedExec, String> {
    let docker = get_docker()?;

    let exec = docker
        .create_exec(
            container_id,
            CreateExecOptions {
                attach_stdin: Some(spec.attach_stdin),
                attach_stdout: Some(true),
                attach_stderr: Some(true),
                tty: Some(spec.tty),
                cmd: Some(spec.cmd.clone()),
                env: if spec.env.is_empty() { None } else { Some(spec.env.clone()) },
                user: Some(spec.user.clone()),
                working_dir: spec.working_dir.clone(),
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
            output: Box::pin(output.map(|r| {
                r.map(log_output_to_chunk)
                    .map_err(|e| e.to_string())
            })),
            input,
        }),
        StartExecResults::Detached => Err("Exec started in detached mode".to_string()),
    }
}

/// bollard's `LogOutput` → the host-neutral [`ExecChunk`]. `Console` is what
/// Docker calls a TTY stream; it has no stderr, so it is stdout.
pub fn log_output_to_chunk(o: LogOutput) -> ExecChunk {
    match o {
        LogOutput::StdOut { message } | LogOutput::Console { message } => ExecChunk::StdOut(message),
        LogOutput::StdErr { message } => ExecChunk::StdErr(message),
        LogOutput::StdIn { message } => ExecChunk::Other(message),
    }
}

/// Start a command with nothing attached and return its exec id without
/// waiting for it.
pub async fn exec_detached(container_id: &str, spec: &ExecSpec) -> Result<String, String> {
    let docker = get_docker()?;
    let exec = docker
        .create_exec(
            container_id,
            CreateExecOptions {
                attach_stdout: Some(false),
                attach_stderr: Some(false),
                tty: Some(spec.tty),
                cmd: Some(spec.cmd.clone()),
                env: if spec.env.is_empty() { None } else { Some(spec.env.clone()) },
                user: Some(spec.user.clone()),
                working_dir: spec.working_dir.clone(),
                ..Default::default()
            },
        )
        .await
        .map_err(|e| format!("Failed to create exec: {}", e))?;
    docker
        .start_exec(
            &exec.id,
            Some(bollard::exec::StartExecOptions {
                detach: true,
                ..Default::default()
            }),
        )
        .await
        .map_err(|e| format!("Failed to start exec: {}", e))?;
    Ok(exec.id)
}

pub async fn resize_exec(exec_id: &str, cols: u16, rows: u16) -> Result<(), String> {
    let docker = get_docker()?;
    docker
        .resize_exec(
            exec_id,
            ResizeExecOptions {
                width: cols,
                height: rows,
            },
        )
        .await
        .map_err(|e| format!("Failed to resize exec: {}", e))
}

/// Docker's archive API: `path` as a tar stream. Errors are stringified so the
/// stream is host-neutral.
pub fn download_archive(container_id: &str, path: &str) -> crate::host::ArchiveStream {
    let docker = match get_docker() {
        Ok(d) => d,
        Err(e) => return Box::pin(futures_util::stream::once(async move { Err(e) })),
    };
    let stream = docker.download_from_container(
        container_id,
        Some(bollard::container::DownloadFromContainerOptions {
            path: path.to_string(),
        }),
    );
    Box::pin(stream.map(|r| r.map_err(|e| e.to_string())))
}

/// Ceiling on one host file packed into a container upload.
///
/// The file goes through host RAM twice — once as bytes, once inside the tar —
/// so this is a memory bound, and it is checked against the *descriptor* that
/// was opened rather than a `metadata` call that described whatever the path
/// meant a moment earlier.
pub const MAX_DROP_BYTES: u64 = 256 * 1024 * 1024;

/// Upload a host file into `dest_dir` under `dest_name`. The file is
/// read and packed into the tar inside a blocking task, so the synchronous IO
/// runs off the async worker. The tar's declared entry size is taken from the
/// bytes actually read (not a separate `stat`), so a file changing size between
/// a size check and the read can't desync the header and corrupt the archive.
/// Returns the in-container path (`<dest_dir>/<dest_name>`).
///
/// `dest_dir` must already exist and must already have been checked by the
/// caller — Docker's archive extractor writes wherever it is pointed. The two
/// callers both do that first, by different routes because they are answering
/// different questions: the terminal drop stages into a fixed `/tmp` path it
/// creates itself, and the Files pane passes the directory the user is looking
/// at, which `file_commands::resolve_container_dir` has already confirmed
/// resolves inside `CONTAINER_WRITE_ROOTS`.
pub async fn upload_host_file_to_container(
    container_id: &str,
    host_path: &str,
    dest_dir: &str,
    dest_name: &str,
) -> Result<String, String> {
    let ids = container_user_ids(container_id).await;
    upload_host_file_with_ids(container_id, host_path, dest_dir, dest_name, ids).await
}

/// [`upload_host_file_to_container`] for a caller that already knows the
/// container user's ids.
///
/// `container_user_ids` is a `docker exec`, and the Files pane's upload is a
/// *selection* — one dialog can hand back twenty files. Resolving the ids per
/// file made twenty extra round trips to answer the same `id -u` twenty times,
/// which is seconds of latency for a fact that cannot change inside one
/// container's lifetime. So the loop resolves once and passes the answer in.
/// The wrapper above keeps the single-file callers unchanged.
pub async fn upload_host_file_with_ids(
    container_id: &str,
    host_path: &str,
    dest_dir: &str,
    dest_name: &str,
    (uid, gid): (u64, u64),
) -> Result<String, String> {
    let host_path = host_path.to_string();
    let dest_name = dest_name.to_string();
    let dest_for_blk = dest_name.clone();
    let mtime = now_epoch_secs();

    let tar_buf = tokio::task::spawn_blocking(move || -> Result<Vec<u8>, String> {
        // The caller resolved this path (`resolve_host_read_path`); opening it
        // is a second trip through the same directories, so the descriptor is
        // checked against the path that was validated before its bytes are
        // packed into anything. Two paths reach here: the terminal's drop
        // target, and the Files pane's upload via `upload_host_file_with_ids`.
        // Between them they are how host bytes enter a container.
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
                path: dest_dir.to_string(),
                ..Default::default()
            }),
            tar_buf.into(),
        )
        .await
        .map_err(|e| format!("Failed to upload file to container: {}", e))?;

    Ok(container_join(dest_dir, &dest_name))
}

/// Join a container directory to a name that may itself carry separators.
///
/// Only the *reported* path — the bytes have already landed by the time this is
/// called — but that path is what the terminal echoes and what the Files pane
/// puts in its toast, so `/tmp//x` reading back as a different file than `/tmp/x`
/// is worth the four lines. `"/"` trims to `""` and yields `/x`.
fn container_join(dir: &str, name: &str) -> String {
    format!("{}/{}", dir.trim_end_matches('/'), name.trim_start_matches('/'))
}

/// Write `data` into the container at `<dest_dir>/<file_name>` with `mode`.
///
/// For small, generated files — migration uses it for the `tar -T` include
/// list, which can be too long to pass as argv, and the marketplace sync for
/// its payload tar and script. Anything large should be
/// streamed through an attached exec's stdin instead, since this buffers the
/// whole payload in memory twice (once raw, once tarred).
pub async fn upload_bytes_to_container(
    container_id: &str,
    dest_dir: &str,
    file_name: &str,
    data: &[u8],
    mode: u32,
) -> Result<String, String> {
    upload_bytes_with_ids(container_id, dest_dir, file_name, data, mode, (0, 0)).await
}

/// [`upload_bytes_to_container`] with the tar header's owner set, stamped now.
pub async fn upload_bytes_with_ids(
    container_id: &str,
    dest_dir: &str,
    file_name: &str,
    data: &[u8],
    mode: u32,
    (uid, gid): (u64, u64),
) -> Result<String, String> {
    let docker = get_docker()?;

    let tar_buf = build_single_file_tar(file_name, data, mode, uid, gid, now_epoch_secs())?;

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
    let (out, _) = exec_oneshot_limited(
        container_id,
        "claude",
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
/// upstream bound, so this is where the bound goes. It is the default, not a
/// promise that every reader fits: the migration manifest of a running
/// container did pass it (8.6 MB from a busy `/workspace`), and now runs under
/// its own `migration::MANIFEST_OUTPUT_LIMIT` through [`exec_oneshot_limited`].
/// A reader that can legitimately grow with the user's files should do the
/// same rather than raise this.
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

/// A one-shot exec as `user` with a caller-chosen output ceiling, returning the
/// combined output and the exit code.
///
/// For readers whose budget is not [`MAX_ONESHOT_OUTPUT`]: smaller where the
/// input is container-controlled and the legitimate output tiny (the auth
/// bridge's procfs poll, the uid probe), larger where it can genuinely pass
/// 8 MiB (the migration manifest, as root).
pub async fn exec_oneshot_limited(
    container_id: &str,
    user: &str,
    cmd: Vec<String>,
    limit: usize,
) -> Result<(String, i64), String> {
    exec_oneshot_inner(container_id, user, cmd, Vec::new(), limit).await
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
                    // Finished. `exit_code` rather than `unwrap_or(0)`: an exec
                    // that has stopped without a reported code is a status
                    // nobody can vouch for, and flattening it to *success* is
                    // the wrong default when a caller is deciding whether to
                    // rename a downloaded file over the user's own.
                    // `download_container_file` treats `None` as a failure
                    // precisely because it cannot tell that silence from a
                    // clean exit; an `unwrap_or` here made that check
                    // unreachable. Callers that only care about "did it fail
                    // loudly" use `is_some_and`, which reads `None` as before.
                    return info.exit_code;
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

    /// The reported path, which is what the terminal echoes back to Claude and
    /// what the Files pane puts in its log line. `/tmp//x` and `/tmp/x` are the
    /// same file to the kernel and different strings to a person reading either
    /// of those.
    #[test]
    fn container_join_produces_one_separator() {
        assert_eq!(container_join("/tmp", "a.txt"), "/tmp/a.txt");
        // The terminal's drop passes a nested name; it must not gain a second
        // slash at the seam.
        assert_eq!(
            container_join("/tmp", "triple-c-drops/a.txt"),
            "/tmp/triple-c-drops/a.txt"
        );
        // A directory the user navigated to can carry a trailing slash, and the
        // container root is the case where trimming it must not eat the only
        // separator there is.
        assert_eq!(container_join("/workspace/", "a.txt"), "/workspace/a.txt");
        assert_eq!(container_join("/", "a.txt"), "/a.txt");
        assert_eq!(container_join("/", "/a.txt"), "/a.txt");
    }
}
