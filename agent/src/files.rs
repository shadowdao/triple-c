//! `PUT /v1/files` and `GET /v1/archive`: the agent's side of the app's file
//! upload and Docker's archive download.

use std::io::{self, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::rejection::QueryRejection;
use axum::extract::{Query, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use bytes::Bytes;
use futures_util::StreamExt;
use serde::Deserialize;
use tokio::io::AsyncWriteExt;
use tokio::sync::mpsc;

use crate::{error, AppState};

fn query_rejection(r: QueryRejection) -> Response {
    error(StatusCode::BAD_REQUEST, format!("invalid query: {}", r.body_text()))
}

#[derive(Deserialize)]
pub struct PutQuery {
    dir: String,
    name: String,
    /// Octal, e.g. `644`. Defaults to 0644, the mode the app's uploads use.
    #[serde(default)]
    mode: Option<String>,
    #[serde(default)]
    uid: Option<u32>,
    #[serde(default)]
    gid: Option<u32>,
}

/// A name the spec allows: one path component, nothing that walks.
fn valid_name(name: &str) -> bool {
    !name.is_empty() && name != "." && name != ".." && !name.contains('/') && !name.contains('\0')
}

/// `PUT /v1/files?dir=&name=&mode=&uid=&gid=`.
///
/// Mirrors what Docker's archive extractor does with the app's single-file
/// tar (`build_single_file_tar`): an existing file is unlinked and a new one
/// created — never written through, so a hard link or a file another process
/// holds open keeps its old content — then given `mode` and `uid:gid`, mtime
/// now. `dir` is not created: the app has already checked it, and silently
/// making directories is how a typo becomes a stray tree.
pub async fn put_file(
    State(st): State<Arc<AppState>>,
    q: Result<Query<PutQuery>, QueryRejection>,
    body: Body,
) -> Response {
    let Query(q) = match q {
        Ok(q) => q,
        Err(r) => return query_rejection(r),
    };
    if !q.dir.starts_with('/') {
        return error(StatusCode::BAD_REQUEST, "dir must be an absolute path");
    }
    if !valid_name(&q.name) {
        return error(StatusCode::BAD_REQUEST, "name must be a single path component");
    }
    let mode = match q.mode.as_deref() {
        None => 0o644,
        Some(m) => match u32::from_str_radix(m, 8) {
            Ok(m) if m <= 0o7777 => m,
            _ => return error(StatusCode::BAD_REQUEST, format!("invalid octal mode: {m}")),
        },
    };
    let (euid, egid) = unsafe { (libc::geteuid(), libc::getegid()) };
    let uid = q.uid.unwrap_or(euid);
    let gid = q.gid.unwrap_or(egid);
    // Same rule as execs: a non-root agent (tests) can only act as itself.
    // Checked before touching the filesystem, so a refusal changes nothing.
    if euid != 0 && (uid != euid || gid != egid) {
        return error(StatusCode::FORBIDDEN, "the agent is not root and cannot chown to another user");
    }
    match std::fs::metadata(&q.dir) {
        Ok(m) if m.is_dir() => {}
        Ok(_) => return error(StatusCode::BAD_REQUEST, format!("{} is not a directory", q.dir)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            return error(StatusCode::NOT_FOUND, format!("{} does not exist", q.dir))
        }
        Err(e) => return error(StatusCode::INTERNAL_SERVER_ERROR, format!("{}: {e}", q.dir)),
    }
    let path = PathBuf::from(&q.dir).join(&q.name);
    let shown = format!("{}/{}", q.dir.trim_end_matches('/'), q.name);

    // Unlink, then create — tar extraction's replace. A directory in the way
    // is refused rather than removed recursively.
    match std::fs::symlink_metadata(&path) {
        Ok(m) if m.is_dir() => {
            return error(StatusCode::CONFLICT, format!("{shown} is a directory"));
        }
        Ok(_) => {
            if let Err(e) = std::fs::remove_file(&path) {
                return error(StatusCode::INTERNAL_SERVER_ERROR, format!("cannot replace {shown}: {e}"));
            }
        }
        Err(_) => {}
    }
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&path);
    let file = match file {
        Ok(f) => f,
        Err(e) => return error(StatusCode::INTERNAL_SERVER_ERROR, format!("cannot create {shown}: {e}")),
    };
    // Owner before mode: chown clears setuid/setgid bits, so the mode has to
    // land last to survive. fchmod rather than the open mode so the umask
    // does not get a say — tar extraction as root restores modes exactly.
    let fd = file.as_raw_fd();
    let set = unsafe {
        (euid != 0 || libc::fchown(fd, uid, gid) == 0) && libc::fchmod(fd, mode as libc::mode_t) == 0
    };
    if !set {
        let e = io::Error::last_os_error();
        let _ = std::fs::remove_file(&path);
        return error(StatusCode::INTERNAL_SERVER_ERROR, format!("cannot set owner/mode on {shown}: {e}"));
    }

    let mut file = tokio::fs::File::from_std(file);
    let mut stream = body.into_data_stream();
    let mut written: u64 = 0;
    let fail = |status, msg: String| {
        let _ = std::fs::remove_file(&path);
        error(status, msg)
    };
    while let Some(chunk) = stream.next().await {
        let chunk = match chunk {
            Ok(c) => c,
            Err(e) => return fail(StatusCode::BAD_REQUEST, format!("upload interrupted: {e}")),
        };
        written += chunk.len() as u64;
        if written > st.config.max_file_bytes {
            return fail(
                StatusCode::PAYLOAD_TOO_LARGE,
                format!("File too large to upload (limit {} MB)", st.config.max_file_bytes / (1024 * 1024)),
            );
        }
        if let Err(e) = file.write_all(&chunk).await {
            return fail(StatusCode::INTERNAL_SERVER_ERROR, format!("cannot write {shown}: {e}"));
        }
    }
    if let Err(e) = file.flush().await {
        return fail(StatusCode::INTERNAL_SERVER_ERROR, format!("cannot write {shown}: {e}"));
    }
    Json(serde_json::json!({ "path": shown })).into_response()
}

#[derive(Deserialize)]
pub struct ArchiveQuery {
    path: String,
}

/// `GET /v1/archive?path=`: a streamed tar whose root member is
/// `basename(path)`, recursing into directories — the shape of Docker's
/// `GET /containers/{id}/archive`, which is what the app's download parses.
///
/// Symlinks are archived as links, not followed, at the root as well as
/// inside: Docker does the same, and following them would let a link in the
/// tree pull in files from anywhere in the container.
pub async fn archive(q: Result<Query<ArchiveQuery>, QueryRejection>) -> Response {
    let Query(q) = match q {
        Ok(q) => q,
        Err(r) => return query_rejection(r),
    };
    if !q.path.starts_with('/') {
        return error(StatusCode::BAD_REQUEST, "path must be absolute");
    }
    let path = PathBuf::from(&q.path);
    let meta = match std::fs::symlink_metadata(&path) {
        Ok(m) => m,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            return error(StatusCode::NOT_FOUND, format!("{} does not exist", q.path))
        }
        Err(e) => return error(StatusCode::INTERNAL_SERVER_ERROR, format!("{}: {e}", q.path)),
    };
    let root_name = path
        .file_name()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));

    // tar is a synchronous writer, so it runs on a blocking thread feeding a
    // small channel. When the client disconnects, the body (and receiver) is
    // dropped, the next send fails, and the walk stops — "dropping the
    // connection cancels the read".
    let (tx, mut rx) = mpsc::channel::<io::Result<Bytes>>(8);
    tokio::task::spawn_blocking(move || {
        let mut w = ChannelWriter { tx: tx.clone(), buf: Vec::with_capacity(CHUNK) };
        let res = (|| {
            let mut b = tar::Builder::new(&mut w);
            b.follow_symlinks(false);
            b.mode(tar::HeaderMode::Complete);
            if meta.is_dir() {
                b.append_dir_all(&root_name, &path)?;
            } else {
                b.append_path_with_name(&path, &root_name)?;
            }
            b.finish()
        })();
        let res = res.and_then(|()| w.flush());
        if let Err(e) = res {
            // Surface as a body error: the client sees a truncated, failed
            // download rather than a short archive that looks complete.
            let _ = tx.blocking_send(Err(e));
        }
    });
    let stream = futures_util::stream::poll_fn(move |cx| rx.poll_recv(cx));
    Response::builder()
        .header(header::CONTENT_TYPE, "application/x-tar")
        .body(Body::from_stream(stream))
        .unwrap_or_else(|_| error(StatusCode::INTERNAL_SERVER_ERROR, "response build failed"))
}

const CHUNK: usize = 64 * 1024;

struct ChannelWriter {
    tx: mpsc::Sender<io::Result<Bytes>>,
    buf: Vec<u8>,
}

impl ChannelWriter {
    fn send(&mut self) -> io::Result<()> {
        if self.buf.is_empty() {
            return Ok(());
        }
        let data = Bytes::from(std::mem::replace(&mut self.buf, Vec::with_capacity(CHUNK)));
        self.tx
            .blocking_send(Ok(data))
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "client went away"))
    }
}

impl Write for ChannelWriter {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        self.buf.extend_from_slice(data);
        if self.buf.len() >= CHUNK {
            self.send()?;
        }
        Ok(data.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        self.send()
    }
}

