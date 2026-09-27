//! Marketplace accounts: where a fetch credential comes from, checking a
//! pasted token, and turning a failed fetch into advice a person can act on.
//!
//! Nothing here logs, returns or formats a token into an error string. A
//! `GhHost` account stores nothing at all: its token is asked of the host's
//! `gh` every time, so a later `gh auth refresh` or logout takes effect.

use std::time::Duration;

use crate::marketplace::git::{Credential, FetchError};
use crate::models::marketplace::{AccountMethod, MarketplaceAccount};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostKind {
    GitHub,
    Gitea,
    GitLab,
    Unknown,
}

/// Known by name only; Gitea (and self-hosted GitLab) are recognised by
/// probing their API in [`validate_token`].
pub fn host_kind(host: &str) -> HostKind {
    match host.to_ascii_lowercase().as_str() {
        "github.com" => HostKind::GitHub,
        "gitlab.com" => HostKind::GitLab,
        _ => HostKind::Unknown,
    }
}

/// `host[:port]` characters only — also what keeps a host safe as a `gh` argument.
///
/// This is a character-set check, not full `host:port` validation — it does
/// not bound a port to 0–65535 or otherwise parse the `:port` suffix. A
/// caller that needs that (e.g. a host validator layered on top of this one)
/// checks the port itself.
///
/// `pub(crate)` so other validators (the add-marketplace form, `gh_login`) use
/// this same rule instead of a divergent copy (pre-flight F13).
pub(crate) fn valid_host(host: &str) -> bool {
    !host.is_empty()
        && host.len() <= 253
        && !host.starts_with('-')
        && host
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b':'))
}

/// The host of an `https://` marketplace URL, lowercased, with a non-default port kept.
pub fn host_of(url: &str) -> Result<String, String> {
    // Pre-flight N17: never echo the raw URL back on a parse failure — a
    // malformed URL can carry `user:token@` and this is the one branch that
    // has not already stripped it.
    let parsed = url::Url::parse(url.trim()).map_err(|e| format!("Not a valid URL: {}", e))?;
    if parsed.scheme() != "https" {
        return Err("Only https:// marketplace URLs are supported.".to_string());
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err("Put credentials in a marketplace account, not in the URL.".to_string());
    }
    let host = parsed
        .host_str()
        .ok_or_else(|| "The URL has no host".to_string())?
        .to_ascii_lowercase();
    let host = match parsed.port() {
        Some(port) => format!("{}:{}", host, port),
        None => host,
    };
    if !valid_host(&host) {
        return Err(format!("{:?} is not a supported host name", host));
    }
    Ok(host)
}

/// The username sent with the token over HTTPS.
pub fn fetch_username(account: &MarketplaceAccount) -> String {
    if host_kind(&account.host) == HostKind::GitHub {
        return "x-access-token".to_string();
    }
    account
        .username
        .clone()
        .filter(|u| !u.trim().is_empty())
        .unwrap_or_else(|| "oauth2".to_string())
}

// ─────────────────────────────────────────────────────────────────────────────
// Host `gh`
// ─────────────────────────────────────────────────────────────────────────────

const GH_TIMEOUT: Duration = Duration::from_secs(15);

/// Run the host's `gh` with a plain argv (no shell) and return trimmed stdout.
async fn run_gh(args: &[&str]) -> Result<String, String> {
    let mut cmd = tokio::process::Command::new("gh");
    cmd.args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    let output = tokio::time::timeout(GH_TIMEOUT, cmd.output())
        .await
        .map_err(|_| "gh did not answer within 15 seconds".to_string())?
        .map_err(|e| format!("Could not run gh: {}", e))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(stderr
            .lines()
            .next()
            .unwrap_or("gh failed")
            .trim()
            .to_string());
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

pub async fn gh_host_available() -> bool {
    run_gh(&["--version"]).await.is_ok()
}

fn gh_login_instructions(host: &str) -> String {
    format!(
        "gh on this computer is not logged in to {host}. Run `gh auth login --hostname {host}` \
         in a terminal, then try again.",
        host = host
    )
}

/// The login name `gh` on the host is signed in as for `host`.
pub async fn gh_host_login(host: &str) -> Result<String, String> {
    if !valid_host(host) {
        return Err(format!("{:?} is not a supported host name", host));
    }
    run_gh(&["auth", "status", "--hostname", host])
        .await
        .map_err(|_| gh_login_instructions(host))?;
    let login = run_gh(&["api", "user", "--hostname", host, "--jq", ".login"]).await?;
    if login.is_empty() {
        return Err(gh_login_instructions(host));
    }
    Ok(login)
}

/// Resolve the credential for an account: `GhHost` → `gh auth token
/// --hostname <host>`; `GhContainer`/`Token` → the keychain.
pub async fn resolve_credential(account: &MarketplaceAccount) -> Result<Credential, String> {
    let password = match account.method {
        AccountMethod::GhHost => {
            if !valid_host(&account.host) {
                return Err(format!("{:?} is not a supported host name", account.host));
            }
            let token = run_gh(&["auth", "token", "--hostname", &account.host])
                .await
                .map_err(|_| gh_login_instructions(&account.host))?;
            if token.is_empty() {
                return Err(gh_login_instructions(&account.host));
            }
            token
        }
        AccountMethod::GhContainer | AccountMethod::Token => {
            crate::storage::secure::get_marketplace_token(&account.id)?.ok_or_else(|| {
                format!(
                    "No token is stored for the account \"{}\". Remove it and sign in again.",
                    account.label
                )
            })?
        }
    };
    Ok(Credential {
        username: fetch_username(account),
        password,
    })
}

// ─────────────────────────────────────────────────────────────────────────────
// Token validation
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, PartialEq, Eq)]
enum Probe {
    Login(String),
    Rejected(u16),
    NotThisKind,
}

fn http_client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .user_agent("Triple-C")
        .timeout(Duration::from_secs(15))
        // reqwest's default policy follows up to 10 redirects and only
        // strips Authorization/Cookie/Proxy-Authorization/WWW-Authenticate
        // on a cross-*host* hop — GitLab's PRIVATE-TOKEN header (and any
        // header on a same-host https→http downgrade) would otherwise
        // follow the token to wherever the response points. Never follow;
        // `who_am_i` treats the resulting 3xx like an unrecognised API.
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|e| format!("Could not create an HTTP client: {}", e))
}

/// One "who am I" call. `base` is the API root for GitHub
/// (`https://api.github.com`) and the site root for Gitea/GitLab.
async fn who_am_i(
    client: &reqwest::Client,
    kind: HostKind,
    base: &str,
    token: &str,
) -> Result<Probe, String> {
    let (url, header, value, field) = match kind {
        HostKind::GitHub => (
            format!("{}/user", base),
            "Authorization",
            format!("Bearer {}", token),
            "login",
        ),
        HostKind::Gitea => (
            format!("{}/api/v1/user", base),
            "Authorization",
            format!("token {}", token),
            "login",
        ),
        HostKind::GitLab => (
            format!("{}/api/v4/user", base),
            "PRIVATE-TOKEN",
            token.to_string(),
            "username",
        ),
        HostKind::Unknown => return Ok(Probe::NotThisKind),
    };
    let response = client
        .get(&url)
        .header(header, value)
        .header("Accept", "application/json")
        .send()
        .await
        // reqwest's error text carries the URL, never the header.
        .map_err(|e| format!("Could not reach {}: {}", base, e.without_url()))?;
    let status = response.status().as_u16();
    match status {
        200 => {
            let json: serde_json::Value = match response.json().await {
                Ok(json) => json,
                Err(_) => return Ok(Probe::NotThisKind),
            };
            match json.get(field).and_then(|v| v.as_str()) {
                Some(login) if !login.is_empty() => Ok(Probe::Login(login.to_string())),
                _ => Ok(Probe::NotThisKind),
            }
        }
        401 | 403 => Ok(Probe::Rejected(status)),
        404 => Ok(Probe::NotThisKind),
        // The client never follows redirects (see `http_client`); a 3xx here
        // means this API would have sent the token onward, so treat it the
        // same as a host that isn't this kind rather than as an error.
        300..=399 => Ok(Probe::NotThisKind),
        other => Err(format!(
            "{} answered HTTP {} when checking the token",
            base, other
        )),
    }
}

fn rejected(host: &str, status: u16) -> String {
    format!(
        "{} rejected the token (HTTP {}). Check that it has not expired and can read repositories.",
        host, status
    )
}

/// GitHub is asked at `github_api`; anything else is probed as Gitea, then
/// GitLab, at `site`. `Ok(None)`: the host is neither, so the token could not
/// be checked here — the marketplace's test fetch checks it instead.
async fn validate_token_at(
    host: &str,
    github_api: Option<&str>,
    site: &str,
    token: &str,
) -> Result<Option<String>, String> {
    let client = http_client()?;
    if let Some(api) = github_api {
        return match who_am_i(&client, HostKind::GitHub, api, token).await? {
            Probe::Login(login) => Ok(Some(login)),
            Probe::Rejected(status) => Err(rejected(host, status)),
            Probe::NotThisKind => Err(format!("{} did not return a user for this token", host)),
        };
    }
    for kind in [HostKind::Gitea, HostKind::GitLab] {
        match who_am_i(&client, kind, site, token).await? {
            Probe::Login(login) => return Ok(Some(login)),
            Probe::Rejected(status) => return Err(rejected(host, status)),
            Probe::NotThisKind => {}
        }
    }
    Ok(None)
}

/// "Who am I" check for a pasted token. `Ok(Some(login))` when the host
/// confirmed it; `Ok(None)` when the host is not GitHub, Gitea or GitLab and
/// the token is left to the first fetch to prove.
pub async fn validate_token(host: &str, token: &str) -> Result<Option<String>, String> {
    if !valid_host(host) {
        return Err(format!("{:?} is not a supported host name", host));
    }
    if token.trim().is_empty() {
        return Err("Paste a token first.".to_string());
    }
    let site = format!("https://{}", host);
    match host_kind(host) {
        HostKind::GitHub => {
            validate_token_at(host, Some("https://api.github.com"), &site, token.trim()).await
        }
        _ => validate_token_at(host, None, &site, token.trim()).await,
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Fetch errors
// ─────────────────────────────────────────────────────────────────────────────

fn who(account: Option<&MarketplaceAccount>) -> String {
    match account {
        None => "anonymously (no account)".to_string(),
        Some(a) => match &a.username {
            Some(u) if !u.is_empty() => format!("with the account \"{}\" ({})", a.label, u),
            _ => format!("with the account \"{}\"", a.label),
        },
    }
}

/// User-facing message for a failed fetch, naming the account used and, for
/// access problems, the usual organisation causes with the page that fixes each.
///
/// `url` must be a marketplace URL already validated by [`host_of`] (as every
/// stored marketplace's URL is) — it is echoed into the message verbatim, so
/// passing unvalidated user input here would defeat the point of N17.
pub fn describe_fetch_error(
    err: &FetchError,
    account: Option<&MarketplaceAccount>,
    url: &str,
) -> String {
    let host = host_of(url).unwrap_or_else(|_| url.to_string());
    match err {
        FetchError::Auth { .. } | FetchError::NotFound => {
            let what = match err {
                FetchError::Auth { status } => format!("access was denied (HTTP {})", status),
                _ => "the repository was not found".to_string(),
            };
            let mut msg = format!("Could not read {} {}: {}.", url, who(account), what);
            if account.is_none() {
                msg.push_str(
                    "\n• The repository may be private — choose an account that can read it.",
                );
            }
            if host_kind(&host) == HostKind::GitHub {
                msg.push_str(
                    "\n• The organization may restrict third-party app access and not have approved \
                     the GitHub CLI or your token: \
                     https://docs.github.com/en/organizations/managing-oauth-access-to-your-organizations-data/about-oauth-app-access-restrictions\
                     \n• If the organization uses SAML single sign-on, the token must be authorized for it: \
                     https://github.com/settings/tokens\
                     \n• A fine-grained token only reaches repositories of the owner it was created for: \
                     https://github.com/settings/personal-access-tokens",
                );
            } else if account.is_some() {
                msg.push_str("\n• Check that the account's token has not expired and can read this repository.");
            }
            msg
        }
        FetchError::Network(m) => format!(
            "Could not reach {}: {}. The last fetched copy is still used.",
            host, m
        ),
        FetchError::Other(m) => format!("Fetching {} failed: {}", url, m),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn account(host: &str, username: Option<&str>) -> MarketplaceAccount {
        MarketplaceAccount {
            id: "acc-1".into(),
            label: "Work".into(),
            host: host.into(),
            method: AccountMethod::Token,
            username: username.map(str::to_string),
        }
    }

    #[test]
    fn host_of_accepts_https_only() {
        assert_eq!(host_of("https://GitHub.com/a/b.git").unwrap(), "github.com");
        assert_eq!(
            host_of("https://git.example.com:8443/a/b").unwrap(),
            "git.example.com:8443"
        );
        assert!(host_of("http://github.com/a/b").is_err());
        assert!(host_of("git@github.com:a/b.git").is_err());
        assert!(host_of("file:///tmp/x").is_err());
        let err = host_of("https://user:test-token-not-real@github.com/a/b").unwrap_err();
        assert!(!err.contains("test-token-not-real"));
    }

    /// Pre-flight N17, the parse-failure branch specifically: a URL that is
    /// both malformed (port out of `u16` range) *and* carries credentials
    /// must not have either the credentials or the raw URL echoed back.
    #[test]
    fn host_of_never_echoes_a_credential_bearing_url_that_fails_to_parse() {
        let err = host_of("https://user:test-token-not-real@github.com:99999/a").unwrap_err();
        assert!(!err.contains("test-token-not-real"), "{}", err);
        assert!(!err.contains("user:"), "{}", err);
    }

    #[test]
    fn fetch_username_per_host() {
        assert_eq!(
            fetch_username(&account("github.com", Some("me"))),
            "x-access-token"
        );
        assert_eq!(
            fetch_username(&account("repo.example.net", Some("jk"))),
            "jk"
        );
        assert_eq!(fetch_username(&account("repo.example.net", None)), "oauth2");
        assert_eq!(
            fetch_username(&account("repo.example.net", Some(" "))),
            "oauth2"
        );
    }

    #[test]
    fn host_kinds() {
        assert_eq!(host_kind("GITHUB.com"), HostKind::GitHub);
        assert_eq!(host_kind("gitlab.com"), HostKind::GitLab);
        assert_eq!(host_kind("repo.example.net"), HostKind::Unknown);
    }

    #[test]
    fn describe_access_errors_names_account_and_org_causes() {
        let url = "https://github.com/acme/private-market.git";
        let msg = describe_fetch_error(
            &FetchError::Auth { status: 403 },
            Some(&account("github.com", Some("me"))),
            url,
        );
        assert!(msg.contains("\"Work\" (me)"), "{}", msg);
        assert!(msg.contains("HTTP 403"));
        assert!(msg.contains("third-party app access"));
        assert!(msg.contains("single sign-on"));
        assert!(msg.contains("fine-grained"));

        let anon = describe_fetch_error(&FetchError::NotFound, None, url);
        assert!(anon.contains("anonymously"));
        assert!(anon.contains("may be private"));

        let gitea = describe_fetch_error(
            &FetchError::Auth { status: 401 },
            Some(&account("repo.example.net", None)),
            "https://repo.example.net/o/r.git",
        );
        assert!(!gitea.contains("single sign-on"));
        assert!(gitea.contains("expired"));
    }

    #[test]
    fn describe_network_and_other_errors() {
        let msg = describe_fetch_error(
            &FetchError::Network("dns error".into()),
            None,
            "https://github.com/a/b",
        );
        assert!(msg.contains("Could not reach github.com"));
        assert!(msg.contains("last fetched copy"));
        let msg = describe_fetch_error(
            &FetchError::Other("weird".into()),
            None,
            "https://github.com/a/b",
        );
        assert!(msg.contains("weird"));
    }

    // ── validate_token against a local mock API ──────────────────────────────

    const FAKE: &str = "test-token-not-real";

    async fn serve(app: axum::Router) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        format!("http://{}", addr)
    }

    fn authorised(headers: &axum::http::HeaderMap, name: &str, want: &str) -> bool {
        headers.get(name).and_then(|v| v.to_str().ok()) == Some(want)
    }

    #[tokio::test]
    async fn github_token_returns_login_or_is_rejected() {
        use axum::{http::HeaderMap, http::StatusCode, routing::get, Json, Router};
        let app = Router::new().route(
            "/user",
            get(|headers: HeaderMap| async move {
                if authorised(&headers, "authorization", &format!("Bearer {}", FAKE)) {
                    Ok(Json(serde_json::json!({ "login": "octo" })))
                } else {
                    Err(StatusCode::UNAUTHORIZED)
                }
            }),
        );
        let base = serve(app).await;
        assert_eq!(
            validate_token_at("github.com", Some(&base), "unused", FAKE)
                .await
                .unwrap(),
            Some("octo".to_string())
        );
        let err = validate_token_at("github.com", Some(&base), "unused", "wrong")
            .await
            .unwrap_err();
        assert!(err.contains("HTTP 401"), "{}", err);
        assert!(
            !err.contains("wrong"),
            "the token must not appear in the error"
        );
    }

    #[tokio::test]
    async fn gitea_is_detected_first() {
        use axum::{http::HeaderMap, http::StatusCode, routing::get, Json, Router};
        let app = Router::new().route(
            "/api/v1/user",
            get(|headers: HeaderMap| async move {
                if authorised(&headers, "authorization", &format!("token {}", FAKE)) {
                    Ok(Json(serde_json::json!({ "login": "jk" })))
                } else {
                    Err(StatusCode::UNAUTHORIZED)
                }
            }),
        );
        let site = serve(app).await;
        assert_eq!(
            validate_token_at("h", None, &site, FAKE).await.unwrap(),
            Some("jk".to_string())
        );
        assert!(validate_token_at("h", None, &site, "wrong").await.is_err());
    }

    #[tokio::test]
    async fn gitlab_is_tried_after_gitea_404() {
        use axum::{http::HeaderMap, http::StatusCode, routing::get, Json, Router};
        let app = Router::new().route(
            "/api/v4/user",
            get(|headers: HeaderMap| async move {
                if authorised(&headers, "private-token", FAKE) {
                    Ok(Json(serde_json::json!({ "username": "gl-user" })))
                } else {
                    Err(StatusCode::UNAUTHORIZED)
                }
            }),
        );
        let site = serve(app).await;
        assert_eq!(
            validate_token_at("h", None, &site, FAKE).await.unwrap(),
            Some("gl-user".to_string())
        );
    }

    #[tokio::test]
    async fn unknown_host_is_left_unchecked() {
        let site = serve(axum::Router::new()).await; // every path 404s
        assert_eq!(
            validate_token_at("h", None, &site, FAKE).await.unwrap(),
            None
        );
    }

    #[tokio::test]
    async fn validate_token_refuses_bad_input_without_network() {
        assert!(validate_token("-evil", FAKE).await.is_err());
        assert!(validate_token("github.com", "  ").await.is_err());
    }

    /// Fix-round-1 security finding: reqwest's default redirect policy
    /// follows up to 10 hops and only strips Authorization/Cookie/
    /// Proxy-Authorization/WWW-Authenticate on a cross-host hop — GitLab's
    /// PRIVATE-TOKEN header is none of those, so an unfollowed-by-default
    /// client is the only thing stopping a malicious/compromised "GitLab"
    /// host from redirecting the probe (with the token still attached) to
    /// an attacker-controlled target. Plain `std::net::TcpListener`s stand
    /// in for the origin and the redirect target so the test can assert the
    /// target is never even connected to, let alone handed the header.
    #[tokio::test]
    async fn redirect_is_never_followed_and_the_token_never_reaches_the_target() {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        use std::sync::mpsc;
        use std::time::Duration as StdDuration;

        // The redirect target. If the client ever followed the redirect,
        // this listener would receive the request — token header included.
        let target = TcpListener::bind("127.0.0.1:0").unwrap();
        let target_addr = target.local_addr().unwrap();
        let (tx, rx) = mpsc::channel::<String>();
        std::thread::spawn(move || {
            target.set_nonblocking(false).ok();
            if let Ok((mut stream, _)) = target.accept() {
                let mut buf = [0u8; 4096];
                let n = stream.read(&mut buf).unwrap_or(0);
                let request = String::from_utf8_lossy(&buf[..n]).to_string();
                let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n");
                let _ = tx.send(request);
            }
        });

        // The origin the probe actually asks, which answers with a 3xx
        // pointing at the target above.
        let origin = TcpListener::bind("127.0.0.1:0").unwrap();
        let origin_addr = origin.local_addr().unwrap();
        std::thread::spawn(move || {
            if let Ok((mut stream, _)) = origin.accept() {
                let mut buf = [0u8; 4096];
                let _ = stream.read(&mut buf);
                let body = format!(
                    "HTTP/1.1 302 Found\r\nLocation: http://{}/api/v4/user\r\nContent-Length: 0\r\n\r\n",
                    target_addr
                );
                let _ = stream.write_all(body.as_bytes());
            }
        });

        let base = format!("http://{}", origin_addr);
        let client = http_client().unwrap();
        let outcome = who_am_i(&client, HostKind::GitLab, &base, FAKE).await;

        // The redirect is reported as "not this kind of host", not an error
        // and not a login — it must not be silently trusted either way.
        assert_eq!(outcome.unwrap(), Probe::NotThisKind);

        // And the target must never see a connection carrying the token —
        // ideally no connection at all, since the client never follows.
        // no connection at all is also the expected outcome
        if let Ok(request) = rx.recv_timeout(StdDuration::from_millis(500)) {
            assert!(
                !request.contains(FAKE) && !request.to_ascii_lowercase().contains("private-token"),
                "the redirect target must never receive the token: {request}"
            );
        }
    }
}
