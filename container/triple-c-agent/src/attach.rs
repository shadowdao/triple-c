//! `GET /v1/attach`: an exec with its stdio carried over a WebSocket.

use std::sync::Arc;
use std::time::Duration;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::response::Response;
use bytes::Bytes;
use serde::Deserialize;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::{mpsc, oneshot};

use crate::pty;
use crate::spawn::{self, ExecSpec, Io, PrepareError};
use crate::{AppState, CountGuard, MAX_WS_MESSAGE};

/// How long the client has to send its spec after the upgrade.
const SPEC_TIMEOUT: Duration = Duration::from_secs(30);

/// After a tty exec's own process exits, how long to keep draining the pty
/// for output still in flight. Waiting for EOF alone is not enough: a
/// background job that kept the terminal open would hold the attach (and its
/// exit frame) forever, while Docker ends the stream when the exec exits.
const TTY_DRAIN_IDLE: Duration = Duration::from_millis(100);

#[derive(Deserialize)]
struct AttachRequest {
    #[serde(flatten)]
    spec: ExecSpec,
    #[serde(default)]
    attach_stdin: bool,
    #[serde(default)]
    cols: Option<u16>,
    #[serde(default)]
    rows: Option<u16>,
}

/// What the pump sends to the socket.
enum Event {
    Out(u8, Bytes),
    Exit(i32),
}

/// What the socket sends to the process.
enum Input {
    Data(Bytes),
}

pub async fn upgrade(State(st): State<Arc<AppState>>, ws: WebSocketUpgrade) -> Response {
    ws.max_message_size(MAX_WS_MESSAGE)
        .max_frame_size(MAX_WS_MESSAGE)
        .on_upgrade(move |socket| session(socket, st))
}

fn text(v: serde_json::Value) -> Message {
    Message::Text(v.to_string().into())
}

async fn fail(mut socket: WebSocket, msg: String) {
    let _ = socket.send(text(serde_json::json!({ "type": "error", "error": msg }))).await;
    let _ = socket.send(Message::Close(None)).await;
}

fn frame(channel: u8, data: &[u8]) -> Message {
    let mut v = Vec::with_capacity(data.len() + 1);
    v.push(channel);
    v.extend_from_slice(data);
    Message::Binary(v.into())
}

async fn session(mut socket: WebSocket, st: Arc<AppState>) {
    // 1. The spec, as one text frame.
    let first = match tokio::time::timeout(SPEC_TIMEOUT, socket.recv()).await {
        Ok(Some(Ok(Message::Text(t)))) => t,
        Ok(Some(Ok(_))) => return fail(socket, "the first frame must be a text ExecSpec".into()).await,
        _ => return,
    };
    let req: AttachRequest = match serde_json::from_str(first.as_str()) {
        Ok(r) => r,
        Err(e) => return fail(socket, format!("invalid attach request: {e}")).await,
    };
    let tty = req.spec.tty;

    let prepared = match spawn::prepare(&st.config, &req.spec, tty) {
        Ok(p) => p,
        Err(PrepareError::Refused(_, m)) => return fail(socket, m).await,
        Err(PrepareError::CannotStart { code, message }) => {
            return not_started(socket, &st, code, message).await
        }
    };

    // 2. Start it. A tty exec gets the pty slave on all three fds; stdin
    // stays the pty even with `attach_stdin: false` (the client's bytes are
    // just ignored), because `stty`, `read -s` and friends need a terminal on
    // fd 0 — the same as a Docker tty exec without `-i`.
    let (child, master) = if tty {
        let (master, slave) = match pty::open(req.cols.unwrap_or(80), req.rows.unwrap_or(24)) {
            Ok(p) => p,
            Err(e) => return fail(socket, format!("cannot open a pty: {e}")).await,
        };
        let fds = (|| Ok::<_, std::io::Error>((slave.try_clone()?, slave.try_clone()?, slave)))();
        let (a, b, c) = match fds {
            Ok(f) => f,
            Err(e) => return fail(socket, format!("cannot open a pty: {e}")).await,
        };
        // The slave copies are moved into the Command and closed when this
        // returns, so the master sees EOF once the *child's* copies close.
        match prepared.spawn(Io::Fd(a), Io::Fd(b), Io::Fd(c), true) {
            Ok(child) => (child, Some(Arc::new(master))),
            Err(PrepareError::CannotStart { code, message }) => {
                return not_started(socket, &st, code, message).await
            }
            Err(PrepareError::Refused(_, m)) => return fail(socket, m).await,
        }
    } else {
        let stdin = if req.attach_stdin { Io::Piped } else { Io::Null };
        match prepared.spawn(stdin, Io::Piped, Io::Piped, false) {
            Ok(child) => (child, None),
            Err(PrepareError::CannotStart { code, message }) => {
                return not_started(socket, &st, code, message).await
            }
            Err(PrepareError::Refused(_, m)) => return fail(socket, m).await,
        }
    };
    let exec_id = st.execs.insert_running(master.clone());
    if socket
        .send(text(serde_json::json!({ "type": "started", "exec_id": exec_id })))
        .await
        .is_err()
    {
        // Gone before it began; still let the process run to an exit code.
    }

    // 3. The pump owns the child and its output for the process's whole life,
    // independent of the socket: if the client leaves, it keeps draining (and
    // discarding) output so the process is never wedged on a full pipe, and
    // it records the exit code either way.
    let (ev_tx, mut ev_rx) = mpsc::channel::<Event>(64);
    let (in_tx, in_rx) = mpsc::unbounded_channel::<Input>();
    let (hangup_tx, hangup_rx) = oneshot::channel::<()>();
    let guard = CountGuard::execs(&st);
    {
        let st = st.clone();
        let exec_id = exec_id.clone();
        let master = master.clone();
        tokio::spawn(async move {
            let _active = guard;
            let code = match master {
                Some(m) => pump_tty(child, m, in_rx, hangup_rx, req.attach_stdin, &ev_tx).await,
                None => pump_pipes(child, in_rx, &ev_tx).await,
            };
            st.execs.finish(&exec_id, code);
            let _ = ev_tx.send(Event::Exit(code)).await;
        });
    }

    // 4. Shuttle between socket and pump. Unbounded stdin on purpose: a child
    // that is not reading stdin must not stop us forwarding its output, or a
    // heavy two-way stream deadlocks. The client is authenticated; how much it
    // queues is its own business, as with Docker's hijacked connection.
    // The loop only `break`s when the client has gone; the normal end (exit
    // frame sent) `return`s from inside it.
    let mut in_tx = Some(in_tx);
    loop {
        tokio::select! {
            ev = ev_rx.recv() => match ev {
                Some(Event::Out(ch, data)) => {
                    st.activity.touch();
                    if socket.send(frame(ch, &data)).await.is_err() {
                        break;
                    }
                }
                Some(Event::Exit(code)) => {
                    let _ = socket.send(text(serde_json::json!({ "type": "exit", "code": code }))).await;
                    let _ = socket.send(Message::Close(None)).await;
                    return;
                }
                None => return,
            },
            msg = socket.recv() => match msg {
                Some(Ok(Message::Binary(data))) => {
                    st.activity.touch();
                    if req.attach_stdin {
                        if let Some(tx) = &in_tx {
                            let _ = tx.send(Input::Data(data));
                        }
                    }
                }
                Some(Ok(Message::Text(t))) => {
                    let is_close = serde_json::from_str::<serde_json::Value>(t.as_str())
                        .ok()
                        .is_some_and(|v| v["type"] == "close_stdin");
                    if is_close {
                        // Dropping the sender ends the writer, which drops the
                        // child's stdin: EOF, like Docker's CloseWrite.
                        in_tx = None;
                    }
                }
                Some(Ok(Message::Ping(_) | Message::Pong(_))) => {}
                Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
            },
        }
    }

    // 5. The client went away, as a dropped `docker exec` does. A tty exec
    // gets SIGHUP on its process group — what a terminal hangup delivers. A
    // non-tty exec only loses stdin and is NOT killed: Docker does not kill
    // it either, and `gh_login`/`setup-token` clean up after themselves with
    // their own `pkill`. The pump keeps running and records the exit code.
    //
    // The pump sends the SIGHUP, not this task: only the pump knows whether it
    // has already reaped the process, and once it has, the pid — which is the
    // process-group id — may belong to someone else. If the pump is already
    // gone the send just fails.
    drop(in_tx);
    if tty {
        let _ = hangup_tx.send(());
    }
}

/// The command could not start: register it as finished, and tell the client
/// the way a Docker attach would show it — started, the reason on stderr,
/// then the exit code.
async fn not_started(mut socket: WebSocket, st: &Arc<AppState>, code: i32, message: String) {
    let id = st.execs.insert_finished(code);
    let _ = socket.send(text(serde_json::json!({ "type": "started", "exec_id": id }))).await;
    let _ = socket.send(frame(2, format!("{message}\n").as_bytes())).await;
    let _ = socket.send(text(serde_json::json!({ "type": "exit", "code": code }))).await;
    let _ = socket.send(Message::Close(None)).await;
}

/// Send output unless the socket side has gone; output after that is drained
/// and discarded so the process never blocks on a full pipe.
async fn emit(tx: &mpsc::Sender<Event>, ch: u8, data: &[u8]) {
    let _ = tx.send(Event::Out(ch, Bytes::copy_from_slice(data))).await;
}

async fn pump_pipes(
    mut child: tokio::process::Child,
    mut input: mpsc::UnboundedReceiver<Input>,
    tx: &mpsc::Sender<Event>,
) -> i32 {
    let mut stdout = child.stdout.take().expect("piped stdout");
    let mut stderr = child.stderr.take().expect("piped stderr");
    if let Some(mut stdin) = child.stdin.take() {
        tokio::spawn(async move {
            while let Some(Input::Data(d)) = input.recv().await {
                if stdin.write_all(&d).await.is_err() {
                    break;
                }
                let _ = stdin.flush().await;
            }
            // `stdin` drops here: EOF for the child.
        });
    }
    let (mut ob, mut eb) = (vec![0u8; 16 * 1024], vec![0u8; 16 * 1024]);
    let (mut out_open, mut err_open) = (true, true);
    let mut code: Option<i32> = None;
    // The exit code is taken as soon as the process exits (so `GET
    // /v1/exec/{id}` is not held up by a grandchild still holding a pipe), but
    // the exit *frame* waits for both streams to end: "all output has been
    // sent" first, as the spec requires.
    while out_open || err_open || code.is_none() {
        tokio::select! {
            r = stdout.read(&mut ob), if out_open => match r {
                Ok(n) if n > 0 => emit(tx, 1, &ob[..n]).await,
                _ => out_open = false,
            },
            r = stderr.read(&mut eb), if err_open => match r {
                Ok(n) if n > 0 => emit(tx, 2, &eb[..n]).await,
                _ => err_open = false,
            },
            s = child.wait(), if code.is_none() => {
                code = Some(s.map(spawn::exit_code).unwrap_or(255));
            }
        }
    }
    code.unwrap_or(255)
}

async fn pump_tty(
    mut child: tokio::process::Child,
    master: Arc<pty::PtyMaster>,
    mut input: mpsc::UnboundedReceiver<Input>,
    hangup: oneshot::Receiver<()>,
    attach_stdin: bool,
    tx: &mpsc::Sender<Event>,
) -> i32 {
    // `Some` until the client has gone (or the session ended without saying
    // so), so a completed receiver is never polled again.
    let mut hangup = Some(hangup);
    if attach_stdin {
        let master = master.clone();
        tokio::spawn(async move {
            while let Some(Input::Data(d)) = input.recv().await {
                if master.write_all(&d).await.is_err() {
                    break;
                }
            }
            // A tty has no EOF to give: closing stdin on a terminal is not
            // a thing, and the pty must stay open for output. Docker's
            // CloseWrite on a tty exec is likewise not an EOF to the process.
        });
    }
    let mut buf = vec![0u8; 16 * 1024];
    let mut code: Option<i32> = None;
    loop {
        if let Some(c) = code {
            match tokio::time::timeout(TTY_DRAIN_IDLE, master.read(&mut buf)).await {
                Ok(Ok(n)) if n > 0 => emit(tx, 1, &buf[..n]).await,
                _ => return c,
            }
            continue;
        }
        tokio::select! {
            r = master.read(&mut buf) => match r {
                // Everything is channel 1 on a tty: Docker's `Console` stream
                // has no stderr, and the app maps it to stdout.
                Ok(n) if n > 0 => emit(tx, 1, &buf[..n]).await,
                _ => {
                    let s = child.wait().await;
                    return s.map(spawn::exit_code).unwrap_or(255);
                }
            },
            s = child.wait() => {
                code = Some(s.map(spawn::exit_code).unwrap_or(255));
            }
            // Only reachable while `code` is `None`: `child.wait()` has not
            // returned, so the process is unreaped (alive or a zombie) and its
            // pid cannot have been reused for another process group.
            r = async { hangup.as_mut().expect("guarded by is_some").await }, if hangup.is_some() => {
                hangup = None;
                if r.is_ok() {
                    spawn::signal_group(child.id(), libc::SIGHUP);
                }
            }
        }
    }
}
