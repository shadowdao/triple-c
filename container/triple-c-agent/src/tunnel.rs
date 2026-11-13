//! `GET /v1/tunnel?port=`: a WebSocket piped to `127.0.0.1:<port>` inside the
//! container — what the auth bridge and browser view do with `socat` through
//! the Docker API today.

use std::sync::Arc;
use std::time::Duration;

use axum::extract::rejection::QueryRejection;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::Response;
use serde::Deserialize;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::{error, AppState, CountGuard, MAX_WS_MESSAGE};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Deserialize)]
pub struct TunnelQuery {
    port: u16,
}

pub async fn upgrade(
    State(st): State<Arc<AppState>>,
    q: Result<Query<TunnelQuery>, QueryRejection>,
    ws: WebSocketUpgrade,
) -> Response {
    let port = match q {
        Ok(Query(q)) if q.port != 0 => q.port,
        Ok(_) => return error(StatusCode::BAD_REQUEST, "port must be 1-65535"),
        Err(r) => return error(StatusCode::BAD_REQUEST, format!("invalid query: {}", r.body_text())),
    };
    ws.max_message_size(MAX_WS_MESSAGE)
        .max_frame_size(MAX_WS_MESSAGE)
        .on_upgrade(move |socket| session(socket, st, port))
}

async fn session(mut socket: WebSocket, st: Arc<AppState>, port: u16) {
    // Loopback only, by construction: the tunnel reaches in-container
    // listeners, never the network the container can see.
    let tcp = match tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(("127.0.0.1", port))).await {
        Ok(Ok(s)) => s,
        Ok(Err(e)) => return fail(socket, format!("cannot connect to 127.0.0.1:{port}: {e}")).await,
        Err(_) => return fail(socket, format!("timed out connecting to 127.0.0.1:{port}")).await,
    };
    let _ = tcp.set_nodelay(true);
    let _active = CountGuard::tunnels(&st);
    let (mut rd, mut wr) = tcp.into_split();
    let mut buf = vec![0u8; 32 * 1024];
    // When either side ends, the other is closed (spec): TCP EOF sends a
    // WebSocket close; a WebSocket close drops the TCP connection.
    loop {
        tokio::select! {
            r = rd.read(&mut buf) => match r {
                Ok(n) if n > 0 => {
                    st.activity.touch();
                    if socket.send(Message::Binary(buf[..n].to_vec().into())).await.is_err() {
                        return;
                    }
                }
                _ => {
                    let _ = socket.send(Message::Close(None)).await;
                    return;
                }
            },
            m = socket.recv() => match m {
                Some(Ok(Message::Binary(d))) => {
                    st.activity.touch();
                    if wr.write_all(&d).await.is_err() {
                        let _ = socket.send(Message::Close(None)).await;
                        return;
                    }
                }
                Some(Ok(Message::Text(_) | Message::Ping(_) | Message::Pong(_))) => {}
                Some(Ok(Message::Close(_))) | Some(Err(_)) | None => return,
            },
        }
    }
}

async fn fail(mut socket: WebSocket, msg: String) {
    let _ = socket
        .send(Message::Text(serde_json::json!({ "type": "error", "error": msg }).to_string().into()))
        .await;
    let _ = socket.send(Message::Close(None)).await;
}
