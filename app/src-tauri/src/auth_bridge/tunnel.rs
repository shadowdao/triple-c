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
//!
//! ## What the host listener is, and is not
//!
//! The listener is **not authenticated**, and cannot be. The port number is
//! chosen by whatever CLI is logging in, the redirect URL is the provider's, and
//! nothing in that chain can be taught to present a token — so there is no path
//! token to add. Anything that can reach `127.0.0.1:<port>` on this host reaches
//! the container-side listener. That includes **any web page the user has open**,
//! which can port-scan loopback from script.
//!
//! Two things narrow that, and neither is a substitute for the other:
//!
//! * The whole feature is opt-in per project, off by default, and only mirrors
//!   ports while its container is running.
//! * [`web_request_verdict`] refuses the one case that is unambiguously a web
//!   page reaching in: a request whose fetch metadata says it is a cross-site
//!   **sub-resource** (`fetch`, `XMLHttpRequest`, `<img>`, `<script src>`,
//!   `<iframe>`). Cross-site *navigations* are allowed, because that is exactly
//!   what an OAuth redirect is.
//!
//! The residual risk, stated plainly rather than papered over: a client that
//! sends no `Sec-Fetch-Site` header at all is not filtered — that is every
//! non-browser client (which is the point; `curl`, a CLI, the container's own
//! probe must all still work) but also any browser predating fetch metadata
//! (Chrome < 76, Firefox < 90, Safari < 16.4). A page can also still reach the
//! port with a top-level navigation it opens itself (`window.open`), which
//! carries `Sec-Fetch-Mode: navigate` and is indistinguishable from the redirect
//! the bridge exists to deliver. And nothing here inspects *what* is behind the
//! port: if the container has something more interesting than a throwaway OAuth
//! listener on loopback, a same-machine caller reaches it.
//!
//! ## Bounds
//!
//! Every accepted connection costs a `docker exec`, and the number of
//! connections is decided by whoever can reach the port. So each forward caps
//! concurrent connections ([`MAX_CONNECTIONS`]), refuses a client that opens a
//! socket and then says nothing ([`FIRST_BYTE_TIMEOUT`], enforced *before* the
//! exec is created), and drops a connection the container has gone quiet on
//! ([`IDLE_TIMEOUT`]).

use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};
use std::time::Duration;

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

/// Concurrent connections one forwarded port will carry.
///
/// Each one is a `docker exec`, and the client side is anything on the host that
/// can dial loopback — including a web page in a loop. A login callback is one
/// connection, occasionally a handful; this is generous for that and still a
/// bound the engine will not notice.
const MAX_CONNECTIONS: usize = 16;

/// How long an accepted connection has to send its first byte before it is
/// dropped, *without* a `docker exec` ever being created for it.
///
/// This is a deliberate narrowing of what the bridge carries: a client that
/// connects and says nothing is not the HTTP OAuth callback this exists for, and
/// forwarding it costs a container exec for a socket that may never speak. A
/// server-speaks-first protocol behind a bridged port would be refused by this;
/// that is the trade, and it is the only protocol shape affected.
const FIRST_BYTE_TIMEOUT: Duration = Duration::from_secs(5);

/// How long a live connection may go with nothing coming back from the container
/// before it is torn down. Generous, because a bridged port is not always a
/// short OAuth callback — but finite, so an abandoned connection cannot pin an
/// exec forever.
const IDLE_TIMEOUT: Duration = Duration::from_secs(600);

/// Ceiling on the request head buffered for [`web_request_verdict`]. Real heads
/// are well under 8 KiB; past this we stop looking and forward what we have.
const MAX_HEAD: usize = 32 * 1024;

/// How long the rest of a request head has, once the first line has identified
/// the connection as HTTP. Only a stalled or hostile client reaches it.
const HEAD_TIMEOUT: Duration = Duration::from_secs(10);

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
    /// Why `[::1]` could not be taken alongside `127.0.0.1`, if it could not.
    ///
    /// A half-bound forward is the one failure mode that looks like a success:
    /// the status says the port is bridged, and a browser that resolves
    /// `localhost` to `::1` and does not fall back still gets a refused
    /// connection. It is not a conflict — the IPv4 half really is carrying
    /// traffic — so it rides along with the port it belongs to and the UI says
    /// so, rather than being logged at debug where nobody sees it.
    pub ipv6_warning: Option<String>,
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
        let (v6, ipv6_warning) =
            match TcpListener::bind(SocketAddr::from((Ipv6Addr::LOCALHOST, port))).await {
                Ok(l) => (Some(l), None),
                Err(e) => {
                    // Warn, not debug. Best-effort is about whether to *fail*,
                    // not about whether to say anything: on a host where
                    // `localhost` resolves to `::1` and the client does not
                    // fall back to IPv4, the callback is refused while the
                    // bridge reports itself healthy — a silent failure with no
                    // thread back to this line.
                    log::warn!(
                        "Auth bridge: bound 127.0.0.1:{} but not [::1]:{} ({}) — continuing with IPv4 only; \
                         a client that resolves localhost to ::1 without falling back will not reach it",
                        port,
                        port,
                        e
                    );
                    (
                        None,
                        Some(format!(
                            "IPv4 only — [::1]:{} could not be bound ({}). A browser that resolves \
                             localhost to ::1 without falling back will not reach this port.",
                            port, e
                        )),
                    )
                }
            };

        let target = family.socat_target(port);
        let task = tokio::spawn(accept_loop(container_id, port, target, v4, v6));

        Ok(Self {
            port,
            family,
            bridged_at: chrono::Utc::now().to_rfc3339(),
            ipv6_warning,
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
                // Reap first, so the cap counts *live* connections rather than
                // every one this listener has ever accepted.
                while conns.try_join_next().is_some() {}
                if conns.len() >= MAX_CONNECTIONS {
                    // Dropping the stream closes it. Better than queueing: the
                    // client side is whatever can dial loopback, so a queue is
                    // just a slower way to run out of execs.
                    log::warn!(
                        "Auth bridge: refusing connection from {} to bridged port {} — \
                         {} concurrent connections already open on it",
                        peer,
                        port,
                        MAX_CONNECTIONS
                    );
                    continue;
                }
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

/// Carry one accepted host connection into the container over `socat`, after
/// deciding it is not a web page reaching into loopback.
///
/// Nothing is forwarded until that decision is made, so a refused request never
/// reaches the container at all — not even a `docker exec`.
async fn tunnel_connection(container_id: String, target: String, mut stream: TcpStream, port: u16) {
    let head = match read_leading_bytes(&mut stream).await {
        Ok(head) => head,
        Err(e) => {
            log::debug!(
                "Auth bridge: dropping connection to bridged port {} before forwarding: {}",
                port,
                e
            );
            return;
        }
    };

    if let LeadingBytes::HttpRequest { buffer, head_len } = &head {
        // Authorize against the head slice only. Parsing past the blank line is
        // how a request *body* gets read as headers — a cross-site `fetch` with
        // a `text/plain` body is not preflighted, so it can put any line it
        // likes in there.
        let head_text = String::from_utf8_lossy(&buffer[..*head_len]);
        if web_request_verdict(&head_text) == Verdict::RefuseCrossSite {
            log::warn!(
                "Auth bridge: refused a cross-site sub-resource request to bridged port {} — \
                 a web page, not a login redirect",
                port
            );
            let _ = refuse(&mut stream).await;
            return;
        }
    }

    // The bytes already off the socket go back on the wire first, byte-exact.
    tunnel_connection_with_prelude(container_id, target, stream, port, head.into_buffer()).await
}

/// What the first bytes of an accepted connection turned out to be.
enum LeadingBytes {
    /// An HTTP request whose head we have in full. `head_len` is one past the
    /// blank line; `buffer` may hold pipelined body bytes beyond it.
    HttpRequest { buffer: Vec<u8>, head_len: usize },
    /// Not HTTP, or HTTP we gave up on reading. Forwarded verbatim, ungated.
    Opaque(Vec<u8>),
}

impl LeadingBytes {
    fn into_buffer(self) -> Vec<u8> {
        match self {
            LeadingBytes::HttpRequest { buffer, .. } => buffer,
            LeadingBytes::Opaque(buffer) => buffer,
        }
    }
}

/// Read just enough of the connection to classify it, without consuming
/// anything the caller cannot replay.
///
/// Bails out to [`LeadingBytes::Opaque`] the moment the first line proves this
/// is not HTTP, so a non-HTTP protocol pays one line of latency and no more.
/// The only hard failure is silence: a client that sends nothing within
/// [`FIRST_BYTE_TIMEOUT`] is dropped before an exec is spent on it.
async fn read_leading_bytes(stream: &mut TcpStream) -> Result<LeadingBytes, String> {
    let mut buf: Vec<u8> = Vec::with_capacity(1024);
    let mut chunk = [0u8; 1024];
    let mut deadline = tokio::time::Instant::now() + FIRST_BYTE_TIMEOUT;

    loop {
        let n = match tokio::time::timeout_at(deadline, stream.read(&mut chunk)).await {
            Ok(Ok(0)) if buf.is_empty() => {
                return Err("closed before sending anything".to_string())
            }
            // A half-close after some bytes is legitimate; forward what we have.
            Ok(Ok(0)) => return Ok(LeadingBytes::Opaque(buf)),
            Ok(Ok(n)) => n,
            Ok(Err(e)) => return Err(format!("read failed: {}", e)),
            Err(_) if buf.is_empty() => {
                return Err(format!(
                    "sent nothing within {}s",
                    FIRST_BYTE_TIMEOUT.as_secs()
                ))
            }
            // Bytes arrived but the head never finished. Fail open: this is a
            // gate on top of the bridge, not the bridge's reason to exist.
            Err(_) => return Ok(LeadingBytes::Opaque(buf)),
        };
        buf.extend_from_slice(&chunk[..n]);

        // Once the first line is complete we know whether to keep reading.
        if let Some(eol) = buf.iter().position(|b| *b == b'\n') {
            if !is_http_request_line(&buf[..eol]) {
                return Ok(LeadingBytes::Opaque(buf));
            }
            deadline = deadline.max(tokio::time::Instant::now() + HEAD_TIMEOUT);
        } else if buf.len() > MAX_HEAD {
            return Ok(LeadingBytes::Opaque(buf));
        }

        if let Some(head_len) = find_head_end(&buf) {
            return Ok(LeadingBytes::HttpRequest {
                buffer: buf,
                head_len,
            });
        }
        if buf.len() > MAX_HEAD {
            return Ok(LeadingBytes::Opaque(buf));
        }
    }
}

/// Whether a first line looks like `METHOD target HTTP/1.x`.
fn is_http_request_line(line: &[u8]) -> bool {
    let line = String::from_utf8_lossy(line);
    let line = line.trim_end_matches(['\r', '\n']);
    let mut parts = line.split(' ');
    let (Some(method), Some(target), Some(version), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return false;
    };
    !method.is_empty()
        && method.chars().all(|c| c.is_ascii_uppercase())
        && !target.is_empty()
        && (version == "HTTP/1.1" || version == "HTTP/1.0")
}

/// Index just past the blank line terminating an HTTP head, if it has arrived.
/// Tolerates a bare-LF terminator, which some minimal clients still emit.
fn find_head_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4)
        .position(|w| w == b"\r\n\r\n")
        .map(|i| i + 4)
        .or_else(|| buf.windows(2).position(|w| w == b"\n\n").map(|i| i + 2))
}

/// Tell a refused caller why, then close. Plain text and `Connection: close` —
/// there is no session here to keep alive.
async fn refuse(stream: &mut TcpStream) -> std::io::Result<()> {
    const BODY: &str = "This port is bridged from a container by Triple-C for a sign-in \
                        callback. It is not an API for web pages to call.\n";
    let response = format!(
        "HTTP/1.1 403 Forbidden\r\n\
         Content-Type: text/plain; charset=utf-8\r\n\
         Content-Length: {}\r\n\
         Cache-Control: no-store\r\n\
         Connection: close\r\n\r\n{}",
        BODY.len(),
        BODY
    );
    stream.write_all(response.as_bytes()).await?;
    stream.shutdown().await
}

// ─────────────────────────────────────────────────────────────────────────────
// The gate — pure, so it can be tested without sockets
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Verdict {
    /// Forward it. Either it is not a browser, or the browser says this is a
    /// navigation or a same-origin request.
    Allow,
    /// Fetch metadata says a document on another site pulled this in as a
    /// sub-resource. No login flow looks like that.
    RefuseCrossSite,
}

/// Decide whether an HTTP request head arriving on a bridged port may be
/// forwarded into the container.
///
/// Deliberately fail-open — see the module docs for exactly what that leaves
/// uncovered. The only refusal is the case with no innocent reading:
/// `Sec-Fetch-Site` says another site, and `Sec-Fetch-Mode` says this is not a
/// navigation. `Sec-Fetch-*` are forbidden header names, so page script cannot
/// set or clear them.
pub(crate) fn web_request_verdict(head: &str) -> Verdict {
    let mut lines = head.split(['\r', '\n']).filter(|l| !l.is_empty());
    // Skip the request line.
    if lines.next().is_none() {
        return Verdict::Allow;
    }

    let mut site: Option<&str> = None;
    let mut mode: Option<&str> = None;
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim();
        match name.trim().to_ascii_lowercase().as_str() {
            // A duplicate of either header is header smuggling, not a client.
            // Refuse rather than pick a winner: last-occurrence-wins is what
            // turns a smuggling primitive into a bypass.
            "sec-fetch-site" if site.is_some() => return Verdict::RefuseCrossSite,
            "sec-fetch-mode" if mode.is_some() => return Verdict::RefuseCrossSite,
            "sec-fetch-site" => site = Some(value),
            "sec-fetch-mode" => mode = Some(value),
            _ => {}
        }
    }

    let Some(site) = site else {
        // No fetch metadata: a CLI, `curl`, or a browser old enough not to send
        // it. Not something this gate can judge.
        return Verdict::Allow;
    };
    if site.eq_ignore_ascii_case("same-origin") || site.eq_ignore_ascii_case("none") {
        return Verdict::Allow;
    }
    // `navigate` is precisely the OAuth redirect: the provider sends the browser
    // to `http://localhost:<port>/callback`, cross-site, as a document load.
    // Refusing it would refuse the feature.
    if mode.is_none_or(|m| m.eq_ignore_ascii_case("navigate")) {
        return Verdict::Allow;
    }
    Verdict::RefuseCrossSite
}

/// As [`tunnel_connection`], but `prelude` is written into the container first,
/// ahead of anything further read from `stream`.
///
/// This exists for callers that must *inspect* the beginning of a connection
/// before deciding to forward it — the browser-view proxy reads the HTTP request
/// head off the socket to check a token, and then has to put those same bytes
/// back on the wire. Passing them here keeps the byte stream exact, rather than
/// re-serialising a parsed request.
pub async fn tunnel_connection_with_prelude(
    container_id: String,
    target: String,
    stream: TcpStream,
    port: u16,
    prelude: Vec<u8>,
) {
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
        // Bytes the caller already consumed from the socket go first, so the
        // container sees the connection exactly as the client sent it.
        if !prelude.is_empty()
            && (input.write_all(&prelude).await.is_err() || input.flush().await.is_err())
        {
            return;
        }
        let mut buf = vec![0u8; PUMP_BUF];
        loop {
            // Idle-bounded. Without this a client that connects, sends a
            // request and then never speaks or closes holds the exec open for
            // as long as the container runs.
            match tokio::time::timeout(IDLE_TIMEOUT, host_rx.read(&mut buf)).await {
                Ok(Ok(0)) | Err(_) => break,
                Ok(Ok(n)) => {
                    if input.write_all(&buf[..n]).await.is_err() || input.flush().await.is_err() {
                        break;
                    }
                }
                Ok(Err(_)) => break,
            }
        }
    }));

    // Container → host. This direction is authoritative: when the exec's output
    // stream ends, socat has exited and the connection is over. It is also the
    // one that decides the connection is dead: nothing back from the container
    // for `IDLE_TIMEOUT` tears the whole thing down, exec included.
    while let Some(chunk) = match tokio::time::timeout(IDLE_TIMEOUT, output.next()).await {
        Ok(chunk) => chunk,
        Err(_) => {
            log::debug!(
                "Auth bridge: bridged port {} idle for {}s — closing the tunnel",
                port,
                IDLE_TIMEOUT.as_secs()
            );
            None
        }
    } {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn head(lines: &[&str]) -> String {
        format!("{}\r\n\r\n", lines.join("\r\n"))
    }

    #[test]
    fn a_cli_callback_with_no_fetch_metadata_is_forwarded() {
        // The overwhelmingly common case, and the reason the gate fails open:
        // `curl`, a CLI's own probe, and anything not a browser send none of
        // these headers, and none of them can be judged from the wire.
        let verdict = web_request_verdict(&head(&[
            "GET /callback?code=abc HTTP/1.1",
            "Host: localhost:41733",
            "User-Agent: curl/8.5.0",
        ]));
        assert_eq!(verdict, Verdict::Allow);
    }

    #[test]
    fn the_oauth_redirect_is_forwarded_even_though_it_is_cross_site() {
        // This is the feature. The provider bounces the browser to
        // `http://localhost:<port>/callback`, which is cross-site and a
        // navigation. Refusing it would refuse every login the bridge exists
        // for.
        for site in ["cross-site", "same-site"] {
            let verdict = web_request_verdict(&head(&[
                "GET /callback?code=abc&state=xyz HTTP/1.1",
                "Host: localhost:41733",
                &format!("Sec-Fetch-Site: {}", site),
                "Sec-Fetch-Mode: navigate",
                "Sec-Fetch-Dest: document",
            ]));
            assert_eq!(verdict, Verdict::Allow, "site={}", site);
        }
    }

    #[test]
    fn a_form_post_callback_is_forwarded() {
        // `response_mode=form_post` providers POST the callback as a
        // navigation. Still a navigation, still allowed.
        let verdict = web_request_verdict(&head(&[
            "POST /callback HTTP/1.1",
            "Host: localhost:41733",
            "Origin: https://login.microsoftonline.com",
            "Sec-Fetch-Site: cross-site",
            "Sec-Fetch-Mode: navigate",
        ]));
        assert_eq!(verdict, Verdict::Allow);
    }

    #[test]
    fn a_cross_site_subresource_from_a_web_page_is_refused() {
        // The case the gate exists for: a page the user happens to have open
        // scanning loopback and poking whatever answers.
        for mode in ["cors", "no-cors", "same-origin", "websocket"] {
            let verdict = web_request_verdict(&head(&[
                "GET /admin HTTP/1.1",
                "Host: 127.0.0.1:41733",
                "Origin: https://evil.example",
                "Sec-Fetch-Site: cross-site",
                &format!("Sec-Fetch-Mode: {}", mode),
            ]));
            assert_eq!(verdict, Verdict::RefuseCrossSite, "mode={}", mode);
        }
    }

    #[test]
    fn the_containers_own_same_origin_requests_are_forwarded() {
        let verdict = web_request_verdict(&head(&[
            "GET /style.css HTTP/1.1",
            "Host: localhost:41733",
            "Sec-Fetch-Site: same-origin",
            "Sec-Fetch-Mode: no-cors",
        ]));
        assert_eq!(verdict, Verdict::Allow);
        // `none` is a user-initiated load — typed URL, bookmark.
        let verdict = web_request_verdict(&head(&[
            "GET / HTTP/1.1",
            "Host: localhost:41733",
            "Sec-Fetch-Site: none",
            "Sec-Fetch-Mode: navigate",
        ]));
        assert_eq!(verdict, Verdict::Allow);
    }

    #[test]
    fn duplicated_fetch_metadata_is_refused_rather_than_resolved() {
        // Last-occurrence-wins is what turns any header-smuggling primitive
        // into a bypass, and no real client sends two.
        let verdict = web_request_verdict(&head(&[
            "GET /x HTTP/1.1",
            "Sec-Fetch-Site: cross-site",
            "Sec-Fetch-Mode: cors",
            "Sec-Fetch-Site: same-origin",
        ]));
        assert_eq!(verdict, Verdict::RefuseCrossSite);
    }

    #[test]
    fn only_the_head_is_ever_judged() {
        // A cross-site `text/plain` POST is not preflighted, so its *body* is
        // fully attacker-chosen. `tunnel_connection` slices at the blank line
        // before calling in; this pins that the slice is what gets judged.
        let raw = "POST /x HTTP/1.1\r\n\
                   Sec-Fetch-Site: cross-site\r\n\
                   Sec-Fetch-Mode: cors\r\n\
                   Content-Type: text/plain\r\n\r\n\
                   Sec-Fetch-Site: same-origin\r\n";
        let head_len = find_head_end(raw.as_bytes()).expect("head terminator");
        let head = &raw[..head_len];
        assert!(!head.contains("same-origin"), "the forged line must be past the slice");
        assert_eq!(web_request_verdict(head), Verdict::RefuseCrossSite);

        // And if the slice were ever got wrong, the duplicate rule is the
        // backstop: a forged `Sec-Fetch-*` line is by construction a second
        // copy of one the browser already sent, which is refused outright
        // rather than resolved in the forgery's favour.
        assert_eq!(web_request_verdict(raw), Verdict::RefuseCrossSite);
    }

    #[test]
    fn a_non_http_first_line_is_never_treated_as_a_request() {
        // Bridged ports are not all HTTP. Anything whose first line is not a
        // request line is forwarded verbatim rather than parsed.
        assert!(!is_http_request_line(b"\x16\x03\x01\x02\x00\x01"));
        assert!(!is_http_request_line(b"*1\r"));
        assert!(!is_http_request_line(b"SSH-2.0-OpenSSH_9.6"));
        assert!(!is_http_request_line(b"GET /x HTTP/2.0"));
        assert!(!is_http_request_line(b"get /x HTTP/1.1"));
        assert!(is_http_request_line(b"GET /x HTTP/1.1\r"));
        assert!(is_http_request_line(b"POST /callback?code=a%20b HTTP/1.0"));
    }

    #[test]
    fn head_end_is_found_for_both_terminators() {
        assert_eq!(find_head_end(b"GET / HTTP/1.1\r\n\r\nBODY"), Some(18));
        assert_eq!(find_head_end(b"GET / HTTP/1.1\n\nBODY"), Some(16));
        assert_eq!(find_head_end(b"GET / HTTP/1.1\r\nHost: x\r\n"), None);
    }

    #[tokio::test]
    async fn a_client_that_says_nothing_never_costs_a_container_exec() {
        // Every accepted connection would otherwise spawn a `docker exec`
        // immediately, so silence was free for the caller and expensive here.
        let listener = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        let accept = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept");
            read_leading_bytes(&mut stream).await
        });

        let _client = TcpStream::connect(addr).await.expect("connect");
        let started = tokio::time::Instant::now();
        let result = accept.await.expect("join");

        assert!(result.is_err(), "silence should not be forwarded");
        assert!(
            started.elapsed() >= FIRST_BYTE_TIMEOUT,
            "should have waited out the first-byte grace period"
        );
    }

    #[tokio::test]
    async fn a_non_http_client_is_classified_from_its_first_line_alone() {
        let listener = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        let accept = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept");
            read_leading_bytes(&mut stream).await
        });

        let mut client = TcpStream::connect(addr).await.expect("connect");
        client.write_all(b"SSH-2.0-OpenSSH_9.6\r\n").await.expect("write");

        let result = accept.await.expect("join").expect("classified");
        // Verbatim, and without waiting for a head terminator that will never
        // come — the whole buffer is replayed into the tunnel.
        assert!(matches!(result, LeadingBytes::Opaque(_)));
        assert_eq!(result.into_buffer(), b"SSH-2.0-OpenSSH_9.6\r\n");
    }

    #[tokio::test]
    async fn an_http_head_is_read_whole_and_replayed_whole() {
        let listener = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        let accept = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept");
            read_leading_bytes(&mut stream).await
        });

        let raw = b"POST /callback HTTP/1.1\r\nHost: localhost\r\nContent-Length: 4\r\n\r\ncode";
        let mut client = TcpStream::connect(addr).await.expect("connect");
        client.write_all(raw).await.expect("write");

        let result = accept.await.expect("join").expect("classified");
        match &result {
            LeadingBytes::HttpRequest { buffer, head_len } => {
                assert_eq!(&buffer[*head_len..], b"code", "body must survive the peek");
                assert!(!buffer[..*head_len].ends_with(b"code"));
            }
            LeadingBytes::Opaque(_) => panic!("should have been recognised as HTTP"),
        }
        assert_eq!(result.into_buffer(), raw.to_vec());
    }
}
