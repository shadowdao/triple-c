//! Host-side loopback listener for one bridged port, and the per-connection
//! tunnel that carries its bytes into the container.
//!
//! ## Why not connect to the container's IP
//!
//! Container IPs are not routable from the host on Docker Desktop (macOS and
//! Windows run the engine in a VM), so a host→`172.17.x.x` dial cannot be the
//! transport. The Docker API is the only channel guaranteed to reach the
//! container from the host, so each accepted connection is carried by a
//! `docker exec` running `socat - TCP:127.0.0.1:<port>`, with the exec's stdin
//! and stdout wired to the TCP socket. `socat` ships in the container image.
//!
//! The exec plumbing itself is *not* reimplemented here: it comes from
//! [`crate::docker::exec::create_attached_exec`], the same helper the
//! interactive terminal sessions are built on.

use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};

use bollard::container::LogOutput;
use futures_util::StreamExt;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::{JoinHandle, JoinSet};

use crate::docker::exec::{create_attached_exec, AttachedExec};

use super::proc_net::PortFamily;

/// Buffer size for the host→container direction. OAuth callbacks are tiny; this
/// only needs to not be pathological.
const PUMP_BUF: usize = 16 * 1024;

/// Aborts a task when dropped, so a cancelled parent can never leave a detached
/// child running.
struct AbortOnDrop(JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// One host loopback port bound and proxied into the container.
///
/// The accept loop owns the [`TcpListener`](tokio::net::TcpListener)s and the
/// [`JoinSet`] of live connection tasks, so aborting the single task handle
/// releases the port *and* tears down every connection under it. [`Drop`] does
/// that as a backstop; [`PortForward::shutdown`] does it deterministically by
/// also awaiting the aborted task, which guarantees the socket is closed before
/// the caller proceeds (important when a port is rebound right after).
pub struct PortForward {
    pub port: u16,
    pub family: PortFamily,
    pub bridged_at: String,
    task: JoinHandle<()>,
}

impl Drop for PortForward {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl PortForward {
    /// Bind `port` on the host loopback and start proxying into `container_id`.
    ///
    /// The bind happens before the task is spawned, so an already-taken port is
    /// reported to the caller as an error rather than disappearing into a
    /// background task.
    pub async fn bind(
        container_id: String,
        port: u16,
        family: PortFamily,
    ) -> Result<Self, std::io::Error> {
        // SECURITY BOUNDARY: the host side binds loopback ONLY — 127.0.0.1 and
        // ::1, never 0.0.0.0 / ::. Everything reachable through this socket is
        // an unauthenticated service inside the container that deliberately
        // bound loopback because it expected to be reachable from nowhere else.
        // Binding a wildcard address here would publish container internals to
        // every host on the LAN. Do not "fix" a connectivity problem by
        // widening these addresses.
        let v4 = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, port))).await?;

        // Also take ::1 when it is available. Browsers and CLIs resolve
        // `localhost` to either family, and the IPv6 answer is often tried
        // first, so a v4-only host listener would miss those callbacks. This is
        // best-effort: if ::1 is unavailable (no IPv6, or that half is taken)
        // the v4 listener alone still works, so it is not treated as a conflict.
        let v6 = match TcpListener::bind(SocketAddr::from((Ipv6Addr::LOCALHOST, port))).await {
            Ok(l) => Some(l),
            Err(e) => {
                log::debug!(
                    "Auth bridge: bound 127.0.0.1:{} but not [::1]:{} ({}) — continuing with IPv4 only",
                    port,
                    port,
                    e
                );
                None
            }
        };

        let target = family.socat_target(port);
        let task = tokio::spawn(accept_loop(container_id, port, target, v4, v6));

        Ok(Self {
            port,
            family,
            bridged_at: chrono::Utc::now().to_rfc3339(),
            task,
        })
    }

    /// Stop accepting, drop the host socket, and abort every in-flight
    /// connection. Awaits the aborted task so the port is provably released
    /// when this returns.
    pub async fn shutdown(&mut self) {
        self.task.abort();
        let _ = (&mut self.task).await;
    }
}

/// Accept on both loopback listeners until aborted. Dropping this future drops
/// the listeners (freeing the port) and the `JoinSet` (aborting live tunnels).
async fn accept_loop(
    container_id: String,
    port: u16,
    target: String,
    v4: TcpListener,
    v6: Option<TcpListener>,
) {
    let mut conns: JoinSet<()> = JoinSet::new();

    loop {
        let accepted = tokio::select! {
            r = v4.accept() => r,
            r = accept_optional(v6.as_ref()) => r,
            // Reap finished tunnels so the JoinSet doesn't grow without bound.
            // When the set is empty `join_next()` yields None, the pattern fails
            // to match, and the branch simply drops out of the select.
            Some(_) = conns.join_next() => continue,
        };

        match accepted {
            Ok((stream, peer)) => {
                log::debug!("Auth bridge: connection from {} to bridged port {}", peer, port);
                let _ = stream.set_nodelay(true);
                conns.spawn(tunnel_connection(
                    container_id.clone(),
                    target.clone(),
                    stream,
                    port,
                ));
            }
            Err(e) => {
                log::warn!("Auth bridge: accept failed on port {}: {} — stopping listener", port, e);
                return;
            }
        }
    }
}

/// `accept()` on an optional listener; never completes when there is none, so it
/// can sit in a `select!` arm unconditionally.
async fn accept_optional(
    listener: Option<&TcpListener>,
) -> std::io::Result<(TcpStream, SocketAddr)> {
    match listener {
        Some(l) => l.accept().await,
        None => std::future::pending().await,
    }
}

/// Carry one accepted host connection into the container over `socat`.
async fn tunnel_connection(container_id: String, target: String, stream: TcpStream, port: u16) {
    let cmd = vec!["socat".to_string(), "-".to_string(), target.clone()];

    let AttachedExec {
        mut output,
        mut input,
        ..
    } = match create_attached_exec(&container_id, cmd, false).await {
        Ok(e) => e,
        Err(e) => {
            log::warn!(
                "Auth bridge: failed to open tunnel exec for port {} ({}): {}",
                port,
                target,
                e
            );
            return;
        }
    };

    let (mut host_rx, mut host_tx) = stream.into_split();

    // Host → container. Runs as its own task so the container→host direction is
    // never blocked behind a client that has stopped sending. Finishing this
    // direction drops `input`, which closes the exec's stdin and lets socat see
    // a clean EOF (a half-close, not a teardown of the whole connection).
    let upstream = AbortOnDrop(tokio::spawn(async move {
        let mut buf = vec![0u8; PUMP_BUF];
        loop {
            match host_rx.read(&mut buf).await {
                Ok(0) => break,
                Ok(n) => {
                    if input.write_all(&buf[..n]).await.is_err() || input.flush().await.is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    }));

    // Container → host. This direction is authoritative: when the exec's output
    // stream ends, socat has exited and the connection is over.
    while let Some(chunk) = output.next().await {
        match chunk {
            // Only stdout is payload. The exec is created with tty = false
            // precisely so Docker demultiplexes these, keeping socat's stderr
            // diagnostics out of the proxied byte stream.
            Ok(LogOutput::StdOut { message }) => {
                if host_tx.write_all(&message).await.is_err() {
                    break;
                }
            }
            Ok(LogOutput::StdErr { message }) => {
                log::debug!(
                    "Auth bridge: socat stderr for port {}: {}",
                    port,
                    String::from_utf8_lossy(&message).trim()
                );
            }
            Ok(_) => {}
            Err(e) => {
                log::debug!("Auth bridge: tunnel stream error on port {}: {}", port, e);
                break;
            }
        }
    }

    let _ = host_tx.shutdown().await;
    // Explicit: stop reading from the host now that the container side is gone.
    drop(upstream);
}
