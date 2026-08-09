//! The host-side, token-gated front door for one project's Playwright viewer.
//!
//! ## Why this is not `PortForward` on its own
//!
//! [`crate::auth_bridge::tunnel::PortForward`] mirrors a container loopback port
//! onto the *same* host loopback port with **no authentication at all**. That is
//! the right trade for the auth bridge — the things it exposes are short-lived
//! OAuth callback listeners whose whole purpose is to receive one unauthenticated
//! request — but it is the wrong trade here. The Playwright viewer is full mouse
//! and keyboard control of a browser running inside a container that has
//! passwordless sudo and, very often, the host's Docker socket bind-mounted. A
//! bare loopback port is reachable by:
//!
//! * any other local user on a multi-user host, and
//! * **any web page the user happens to have open**, via localhost port scanning
//!   or DNS rebinding.
//!
//! So this module keeps the tunnel half of the auth bridge (a per-connection
//! `socat` exec through the Docker API — see
//! [`crate::auth_bridge::tunnel::tunnel_connection_with_prelude`]) and replaces
//! the listener half with one that authenticates before a single byte reaches
//! the container. There is therefore exactly **one** host-bound socket per
//! session, and it is gated.
//!
//! ## The gate
//!
//! Gating happens on the first HTTP request head of every accepted TCP
//! connection, before anything is forwarded. To get a connection through you
//! must satisfy all of:
//!
//! 1. `Host` is `127.0.0.1:<port>` or `localhost:<port>` — this is the
//!    anti-DNS-rebinding check. A page on `evil.com` that rebinds its name to
//!    127.0.0.1 still sends `Host: evil.com`.
//! 2. Either
//!    * the request carries the session token (in `?token=`, in a `Cookie`, or
//!      in the query of a same-origin `Referer`), **or**
//!    * `Origin` / `Referer` is exactly this proxy's own origin — i.e. the
//!      request was issued by a document that we already served, which itself
//!      had to present the token. This is what lets the viewer's own
//!      sub-resource and WebSocket requests through: a browser will not let a
//!      hostile page forge either header, and requests that carry neither (a
//!      cross-site `<script src>` or a top-level navigation) are rejected.
//!
//! Once the first head passes, the rest of the connection is spliced verbatim,
//! so HTTP/1.1 keep-alive, the WebSocket upgrade and the CDP screencast frames
//! all pass through untouched and protocol-agnostically. Riding an existing
//! connection is not an escalation: opening one required the token.
//!
//! ## Port allocation and the CSP
//!
//! Host ports come from the small fixed range [`PROXY_PORTS`]. That is
//! deliberate: `tauri.conf.json`'s `frame-src` has to name every origin the pane
//! may embed, and CSP has no port wildcards short of `http://127.0.0.1:*`.
//! Allocating from a bounded, known range keeps that directive an exact
//! enumeration instead of "any localhost port".

use std::net::{Ipv4Addr, SocketAddr};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::{JoinHandle, JoinSet};

use crate::auth_bridge::proc_net::PortFamily;
use crate::auth_bridge::tunnel::tunnel_connection_with_prelude;

/// Host loopback ports the pane may be served on, and therefore the exact set of
/// origins enumerated in the app's `frame-src`. Keep the two in sync: adding a
/// port here without adding it to `tauri.conf.json` produces a pane that is
/// silently blocked by CSP.
pub const PROXY_PORTS: std::ops::RangeInclusive<u16> = 47820..=47827;

/// Ceiling on the request head we will buffer before deciding. Real heads are
/// well under 8 KiB; anything larger is either broken or hostile.
const MAX_HEAD: usize = 32 * 1024;

/// How long a freshly accepted connection has to produce a complete request
/// head. Prevents a slowloris from pinning accept-loop tasks.
const HEAD_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

const REFUSAL_BODY: &str = concat!(
    "<!doctype html><meta charset=\"utf-8\">",
    "<title>Not available</title>",
    "<p>This Triple-C browser view is only reachable from the app that started it.</p>"
);

/// A bound, token-gated host listener in front of one container-side viewer.
///
/// The accept loop owns the [`TcpListener`] and the [`JoinSet`] of live
/// connections, so aborting the one task handle releases the port *and* tears
/// down everything under it. [`Drop`] does that as a backstop;
/// [`BrowserViewProxy::shutdown`] does it deterministically by also awaiting the
/// aborted task, so the port is provably free before the caller continues.
pub struct BrowserViewProxy {
    pub port: u16,
    task: JoinHandle<()>,
}

impl Drop for BrowserViewProxy {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl BrowserViewProxy {
    /// Take the first free port in [`PROXY_PORTS`] on the host loopback and
    /// start gating connections into `container_id`'s `container_port`.
    pub async fn bind(
        container_id: String,
        container_port: u16,
        family: PortFamily,
        token: String,
    ) -> Result<Self, String> {
        let mut last_err = None;
        for port in PROXY_PORTS {
            // SECURITY BOUNDARY: 127.0.0.1 ONLY, never 0.0.0.0. Unlike
            // `web_terminal`, which binds a wildcard on purpose because remote
            // access *is* its feature, this pane is remote control of a browser
            // in a privileged container and must never leave the host. Do not
            // "fix" a connectivity problem by widening this address.
            match TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, port))).await {
                Ok(listener) => {
                    let task = tokio::spawn(accept_loop(
                        listener,
                        container_id,
                        family.socat_target(container_port),
                        container_port,
                        token,
                        self_origins(port),
                        host_authorities(port),
                    ));
                    log::info!("Browser view: proxy listening on 127.0.0.1:{}", port);
                    return Ok(Self { port, task });
                }
                Err(e) => last_err = Some(e),
            }
        }
        Err(format!(
            "No free host port in {}–{} for the browser view proxy ({}). \
             Close another project's browser view and try again.",
            PROXY_PORTS.start(),
            PROXY_PORTS.end(),
            last_err
                .map(|e| e.to_string())
                .unwrap_or_else(|| "range empty".to_string())
        ))
    }

    /// Stop accepting, release the host port and abort every live connection.
    pub async fn shutdown(&mut self) {
        self.task.abort();
        let _ = (&mut self.task).await;
        log::info!("Browser view: proxy on 127.0.0.1:{} released", self.port);
    }
}

/// The origins a request may legitimately claim to come from.
fn self_origins(port: u16) -> Vec<String> {
    vec![
        format!("http://127.0.0.1:{}", port),
        format!("http://localhost:{}", port),
    ]
}

/// The `Host` values we will answer to. Anything else is a rebinding attempt.
fn host_authorities(port: u16) -> Vec<String> {
    vec![
        format!("127.0.0.1:{}", port),
        format!("localhost:{}", port),
    ]
}

#[allow(clippy::too_many_arguments)]
async fn accept_loop(
    listener: TcpListener,
    container_id: String,
    target: String,
    container_port: u16,
    token: String,
    origins: Vec<String>,
    authorities: Vec<String>,
) {
    let mut conns: JoinSet<()> = JoinSet::new();

    loop {
        let accepted = tokio::select! {
            r = listener.accept() => r,
            // Reap finished connections so the set can't grow without bound.
            // An empty set yields `None`, the pattern fails, and the branch is
            // simply dropped from the select.
            Some(_) = conns.join_next() => continue,
        };

        match accepted {
            Ok((stream, _peer)) => {
                let _ = stream.set_nodelay(true);
                conns.spawn(serve_connection(
                    stream,
                    container_id.clone(),
                    target.clone(),
                    container_port,
                    token.clone(),
                    origins.clone(),
                    authorities.clone(),
                ));
            }
            Err(e) => {
                log::warn!("Browser view: accept failed: {} — stopping proxy listener", e);
                return;
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn serve_connection(
    mut stream: TcpStream,
    container_id: String,
    target: String,
    container_port: u16,
    token: String,
    origins: Vec<String>,
    authorities: Vec<String>,
) {
    let head = match tokio::time::timeout(HEAD_TIMEOUT, read_head(&mut stream)).await {
        Ok(Ok(head)) => head,
        Ok(Err(e)) => {
            log::debug!("Browser view: dropping connection: {}", e);
            let _ = reject(&mut stream, 400, "Bad Request").await;
            return;
        }
        Err(_) => {
            log::debug!("Browser view: dropping connection: no request head within timeout");
            return;
        }
    };

    let head_text = String::from_utf8_lossy(&head).into_owned();
    let verdict = authorize(&head_text, &token, &origins, &authorities);
    if verdict != Verdict::Allow {
        log::warn!(
            "Browser view: rejected a connection on the proxy for container port {} ({:?})",
            container_port,
            verdict
        );
        let _ = reject(&mut stream, 403, "Forbidden").await;
        return;
    }

    // Authorized: hand the socket to the same socat-over-Docker-exec tunnel the
    // auth bridge uses, replaying the head we had to buffer to make the call.
    tunnel_connection_with_prelude(container_id, target, stream, container_port, head).await;
}

/// Read bytes until the end of the HTTP request head (`\r\n\r\n`), or fail.
async fn read_head(stream: &mut TcpStream) -> Result<Vec<u8>, String> {
    let mut buf = Vec::with_capacity(1024);
    let mut chunk = [0u8; 1024];
    loop {
        let n = stream
            .read(&mut chunk)
            .await
            .map_err(|e| format!("read failed: {}", e))?;
        if n == 0 {
            return Err("connection closed before a request head arrived".to_string());
        }
        buf.extend_from_slice(&chunk[..n]);
        if find_head_end(&buf).is_some() {
            return Ok(buf);
        }
        if buf.len() > MAX_HEAD {
            return Err(format!("request head exceeded {} bytes", MAX_HEAD));
        }
    }
}

/// Index just past the blank line terminating the head, if it has arrived.
/// Tolerates a bare-LF terminator, which some minimal clients still emit.
fn find_head_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4)
        .position(|w| w == b"\r\n\r\n")
        .map(|i| i + 4)
        .or_else(|| buf.windows(2).position(|w| w == b"\n\n").map(|i| i + 2))
}

async fn reject(stream: &mut TcpStream, code: u16, reason: &str) -> std::io::Result<()> {
    let body = REFUSAL_BODY;
    let response = format!(
        "HTTP/1.1 {} {}\r\n\
         Content-Type: text/html; charset=utf-8\r\n\
         Content-Length: {}\r\n\
         Cache-Control: no-store\r\n\
         Connection: close\r\n\r\n{}",
        code,
        reason,
        body.len(),
        body
    );
    stream.write_all(response.as_bytes()).await?;
    stream.shutdown().await
}

// ─────────────────────────────────────────────────────────────────────────────
// The gate itself — pure, so it can be tested without sockets
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Verdict {
    Allow,
    /// No request line, or one we can't parse.
    Malformed,
    /// `Host` is not one of ours — a rebinding attempt, or a stray client.
    BadHost,
    /// Well-formed and addressed to us, but presented no token and no proof of
    /// having come from a document we served.
    Unauthenticated,
}

/// Decide whether the connection whose first request head this is may be
/// spliced into the container. See the module docs for the rules.
pub(crate) fn authorize(
    head: &str,
    token: &str,
    self_origins: &[String],
    host_authorities: &[String],
) -> Verdict {
    let mut lines = head.split(['\r', '\n']).filter(|l| !l.is_empty());

    let Some(request_line) = lines.next() else {
        return Verdict::Malformed;
    };
    // "GET /path?query HTTP/1.1"
    let mut parts = request_line.split(' ');
    let (Some(_method), Some(request_target)) = (parts.next(), parts.next()) else {
        return Verdict::Malformed;
    };
    if !request_target.starts_with('/') && !request_target.starts_with("http") {
        // CONNECT and origin-form-violating targets are not something the
        // viewer ever sends; refuse to be used as a forward proxy.
        return Verdict::Malformed;
    }

    let mut host = None;
    let mut origin = None;
    let mut referer = None;
    let mut cookie = None;
    let mut fetch_site = None;
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim();
        match name.trim().to_ascii_lowercase().as_str() {
            "host" => host = Some(value),
            "origin" => origin = Some(value),
            "referer" => referer = Some(value),
            "cookie" => cookie = Some(value),
            "sec-fetch-site" => fetch_site = Some(value),
            _ => {}
        }
    }

    // 1. Anti-rebinding. A hostile page that points its own name at 127.0.0.1
    //    still sends its own name here.
    match host {
        Some(h) if host_authorities.iter().any(|a| a.eq_ignore_ascii_case(h)) => {}
        _ => return Verdict::BadHost,
    }

    // 2a. An explicit token, from the request target, a cookie, or the query of
    //     the referring document's URL (same-origin requests send the full URL,
    //     query included, under the default referrer policy).
    if query_token(request_target).is_some_and(|t| tokens_match(t, token))
        || cookie_token(cookie.unwrap_or("")).is_some_and(|t| tokens_match(t, token))
        || referer.and_then(query_token).is_some_and(|t| tokens_match(t, token))
    {
        return Verdict::Allow;
    }

    // 2b. …or proof that a document we already served issued this request. The
    //     viewer's WebSocket upgrade carries `Origin` and no `Referer`, and
    //     nothing in it is under our control, so this is the clause that makes
    //     the pane work at all. A browser will not let a hostile page forge
    //     either header; a request with neither (cross-site `<script src>`,
    //     top-level navigation, `curl`) falls through and is refused.
    if origin.is_some_and(|o| origin_is_self(o, self_origins))
        || referer.is_some_and(|r| origin_is_self(r, self_origins))
        // Fetch metadata says the same thing as `Origin`, and keeps saying it
        // for the plain sub-resource loads that carry no `Origin` and whose
        // `Referer` a `no-referrer` policy could strip. `Sec-Fetch-Site` is a
        // forbidden header, so page script cannot set it either.
        || fetch_site.is_some_and(|s| s.eq_ignore_ascii_case("same-origin"))
    {
        return Verdict::Allow;
    }

    Verdict::Unauthenticated
}

/// The value of a `token` query parameter in a request target or absolute URL.
fn query_token(target: &str) -> Option<&str> {
    let query = target.split_once('?')?.1;
    // Fragments never reach the wire in a request target, but a `Referer` can
    // legally carry one on some clients.
    let query = query.split('#').next().unwrap_or(query);
    query.split('&').find_map(|pair| {
        let (k, v) = pair.split_once('=')?;
        (k == "token").then_some(v)
    })
}

/// The value of our session cookie in a `Cookie` header.
fn cookie_token(cookie_header: &str) -> Option<&str> {
    cookie_header.split(';').find_map(|pair| {
        let (k, v) = pair.split_once('=')?;
        (k.trim() == COOKIE_NAME).then_some(v.trim())
    })
}

/// Name of the cookie the gate will accept a token in. Nothing sets it today —
/// the pane relies on the query parameter for the document and on `Origin` /
/// `Referer` for everything under it, because a webview iframe pointed at
/// 127.0.0.1 is a third-party context and WKWebView and WebKitGTK both drop
/// third-party cookies by default. It is accepted so that a future first-party
/// entry point (opening the pane in the user's own browser, say) needs no
/// change here.
const COOKIE_NAME: &str = "triple_c_browser_view";

/// Whether a URL (or bare origin) has exactly one of our own origins.
fn origin_is_self(value: &str, self_origins: &[String]) -> bool {
    // Compare scheme://host:port only; a Referer carries a path as well.
    let origin = match value.split_once("://") {
        Some((scheme, rest)) => {
            let authority = rest.split(['/', '?', '#']).next().unwrap_or(rest);
            format!("{}://{}", scheme, authority)
        }
        None => value.to_string(),
    };
    self_origins.iter().any(|o| o.eq_ignore_ascii_case(&origin))
}

/// Length-independent-ish equality. A timing oracle over a loopback socket is
/// not a realistic attack, but comparing in constant time costs nothing and
/// keeps the primitive honest.
fn tokens_match(candidate: &str, expected: &str) -> bool {
    let a = candidate.as_bytes();
    let b = expected.as_bytes();
    let mut diff = (a.len() ^ b.len()) as u8;
    for i in 0..a.len().max(b.len()) {
        let x = a.get(i).copied().unwrap_or(0);
        let y = b.get(i).copied().unwrap_or(0);
        diff |= x ^ y;
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOKEN: &str = "s3cr3t-token-value";

    fn origins() -> Vec<String> {
        self_origins(47820)
    }
    fn authorities() -> Vec<String> {
        host_authorities(47820)
    }

    fn head(request_line: &str, headers: &[&str]) -> String {
        let mut s = String::from(request_line);
        s.push_str("\r\n");
        for h in headers {
            s.push_str(h);
            s.push_str("\r\n");
        }
        s.push_str("\r\n");
        s
    }

    fn verdict(request_line: &str, headers: &[&str]) -> Verdict {
        authorize(&head(request_line, headers), TOKEN, &origins(), &authorities())
    }

    #[test]
    fn the_initial_document_is_allowed_by_its_query_token() {
        assert_eq!(
            verdict(
                &format!("GET /?token={} HTTP/1.1", TOKEN),
                &["Host: 127.0.0.1:47820"]
            ),
            Verdict::Allow
        );
    }

    #[test]
    fn a_wrong_token_is_not_enough() {
        assert_eq!(
            verdict("GET /?token=nope HTTP/1.1", &["Host: 127.0.0.1:47820"]),
            Verdict::Unauthenticated
        );
    }

    #[test]
    fn a_subresource_is_allowed_by_the_token_in_its_referer() {
        assert_eq!(
            verdict(
                "GET /assets/app.js HTTP/1.1",
                &[
                    "Host: 127.0.0.1:47820",
                    &format!("Referer: http://127.0.0.1:47820/?token={}", TOKEN),
                ]
            ),
            Verdict::Allow
        );
    }

    #[test]
    fn the_websocket_upgrade_is_allowed_by_its_own_origin() {
        // The viewer's CDP screencast socket carries Origin and no Referer, and
        // its URL is not ours to add a token to.
        assert_eq!(
            verdict(
                "GET /ws HTTP/1.1",
                &[
                    "Host: 127.0.0.1:47820",
                    "Upgrade: websocket",
                    "Connection: Upgrade",
                    "Origin: http://127.0.0.1:47820",
                ]
            ),
            Verdict::Allow
        );
    }

    #[test]
    fn a_subresource_is_allowed_by_fetch_metadata_when_the_referer_is_stripped() {
        assert_eq!(
            verdict(
                "GET /assets/app.js HTTP/1.1",
                &[
                    "Host: 127.0.0.1:47820",
                    "Sec-Fetch-Site: same-origin",
                    "Sec-Fetch-Dest: script",
                ]
            ),
            Verdict::Allow
        );
    }

    #[test]
    fn cross_site_fetch_metadata_is_refused() {
        for site in ["cross-site", "same-site", "none"] {
            assert_eq!(
                verdict(
                    "GET /assets/app.js HTTP/1.1",
                    &["Host: 127.0.0.1:47820", &format!("Sec-Fetch-Site: {}", site)]
                ),
                Verdict::Unauthenticated,
                "Sec-Fetch-Site: {}",
                site
            );
        }
    }

    #[test]
    fn a_hostile_pages_fetch_is_refused_by_its_origin() {
        assert_eq!(
            verdict(
                "GET / HTTP/1.1",
                &["Host: 127.0.0.1:47820", "Origin: http://evil.example"]
            ),
            Verdict::Unauthenticated
        );
    }

    #[test]
    fn a_bare_port_scan_is_refused() {
        // No token, no Origin, no Referer — a cross-site <script src>, a
        // top-level navigation, or curl.
        assert_eq!(
            verdict("GET / HTTP/1.1", &["Host: 127.0.0.1:47820"]),
            Verdict::Unauthenticated
        );
    }

    #[test]
    fn dns_rebinding_is_refused_even_with_a_valid_token() {
        // The attacker's name resolves to 127.0.0.1, but the Host header still
        // says who the browser thinks it is talking to.
        assert_eq!(
            verdict(
                &format!("GET /?token={} HTTP/1.1", TOKEN),
                &["Host: evil.example:47820"]
            ),
            Verdict::BadHost
        );
    }

    #[test]
    fn localhost_is_an_acceptable_authority_and_origin() {
        assert_eq!(
            verdict(
                "GET /ws HTTP/1.1",
                &["Host: localhost:47820", "Origin: http://localhost:47820"]
            ),
            Verdict::Allow
        );
    }

    #[test]
    fn another_panes_origin_does_not_authorize_this_one() {
        // Ports are what separate one project's pane from another's, so the
        // neighbouring port must not be accepted as "self".
        assert_eq!(
            verdict(
                "GET /ws HTTP/1.1",
                &["Host: 127.0.0.1:47820", "Origin: http://127.0.0.1:47821"]
            ),
            Verdict::Unauthenticated
        );
    }

    #[test]
    fn a_cookie_borne_token_is_accepted() {
        assert_eq!(
            verdict(
                "GET /assets/app.js HTTP/1.1",
                &[
                    "Host: 127.0.0.1:47820",
                    &format!("Cookie: other=1; {}={}", COOKIE_NAME, TOKEN),
                ]
            ),
            Verdict::Allow
        );
    }

    #[test]
    fn a_missing_host_header_is_refused() {
        assert_eq!(
            verdict(&format!("GET /?token={} HTTP/1.1", TOKEN), &[]),
            Verdict::BadHost
        );
    }

    #[test]
    fn header_names_are_matched_case_insensitively() {
        assert_eq!(
            verdict(
                "GET /ws HTTP/1.1",
                &["HOST: 127.0.0.1:47820", "ORIGIN: http://127.0.0.1:47820"]
            ),
            Verdict::Allow
        );
    }

    #[test]
    fn a_connect_request_cannot_turn_this_into_a_forward_proxy() {
        assert_eq!(
            verdict("CONNECT evil.example:443 HTTP/1.1", &["Host: 127.0.0.1:47820"]),
            Verdict::Malformed
        );
    }

    #[test]
    fn an_empty_head_is_malformed() {
        assert_eq!(authorize("", TOKEN, &origins(), &authorities()), Verdict::Malformed);
    }

    #[test]
    fn the_head_terminator_is_found_for_both_crlf_and_lf() {
        assert_eq!(find_head_end(b"GET / HTTP/1.1\r\n\r\n"), Some(18));
        assert_eq!(find_head_end(b"GET / HTTP/1.1\n\n"), Some(16));
        assert_eq!(find_head_end(b"GET / HTTP/1.1\r\nHost: x\r\n"), None);
    }

    #[test]
    fn query_token_ignores_lookalike_parameters() {
        assert_eq!(query_token("/?mytoken=a&token=b"), Some("b"));
        assert_eq!(query_token("/?tokenish=a"), None);
        assert_eq!(query_token("/nothing"), None);
    }

    #[test]
    fn tokens_match_rejects_prefixes_and_suffixes() {
        assert!(tokens_match(TOKEN, TOKEN));
        assert!(!tokens_match(&TOKEN[..5], TOKEN));
        assert!(!tokens_match(&format!("{}x", TOKEN), TOKEN));
        assert!(!tokens_match("", TOKEN));
    }

    #[test]
    fn the_proxy_port_range_is_the_one_the_csp_enumerates() {
        // tauri.conf.json lists these origins in `frame-src`; a change here
        // without a change there yields a pane that is silently blocked.
        assert_eq!(PROXY_PORTS.clone().count(), 8);
        assert_eq!(*PROXY_PORTS.start(), 47820);
        assert_eq!(*PROXY_PORTS.end(), 47827);
    }
}
