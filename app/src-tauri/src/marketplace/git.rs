//! The marketplace cache: one bare `gix` repository per marketplace.
//!
//! Everything here is blocking — call it from `tokio::task::spawn_blocking`.
//! Credentials are handed to gix through its credential callback for the
//! duration of one fetch and are never written to disk or into the repo
//! config.

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

/// The ref the fetched branch tip is stored under.
pub const HEAD_REF: &str = "refs/triple-c/head";
/// Prefix of the refs that keep pinned commits alive.
pub const PIN_PREFIX: &str = "refs/triple-c/pins/";

#[derive(Clone)]
pub struct Credential {
    pub username: String,
    pub password: String,
}

impl std::fmt::Debug for Credential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Credential")
            .field("username", &self.username)
            .field("password", &"<redacted>")
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FetchError {
    /// 401 / 403, or gix's "credentials … were not accepted" / "no
    /// credentials were returned" (anonymous fetch of a private repo).
    Auth {
        status: u16,
    },
    /// 404 / "repository not found".
    NotFound,
    Network(String),
    Other(String),
}

impl std::fmt::Display for FetchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FetchError::Auth { status } => write!(f, "access denied (HTTP {})", status),
            FetchError::NotFound => write!(f, "repository not found"),
            FetchError::Network(m) => write!(f, "network error: {}", m),
            FetchError::Other(m) => write!(f, "{}", m),
        }
    }
}

/// Classify a gix error by its Debug-formatted chain. gix wraps transport
/// errors several layers deep and some layers are not `std::error::Error`,
/// so the text is the one stable thing to match on.
pub fn classify_fetch_error(chain: &str) -> FetchError {
    let lower = chain.to_ascii_lowercase();
    if lower.contains("http status 401")
        || lower.contains("not accepted by the remote")
        // GitHub and GitLab answer an anonymous fetch of a private (or
        // missing) repo with a credential challenge; with no credential
        // callback result gix reports this (pre-flight F2).
        || lower.contains("no credentials were returned")
    {
        return FetchError::Auth { status: 401 };
    }
    if lower.contains("http status 403") {
        return FetchError::Auth { status: 403 };
    }
    if lower.contains("http status 404") || lower.contains("repository not found") {
        return FetchError::NotFound;
    }
    const NETWORK: &[&str] = &[
        "dns error",
        "resolving dns",
        "failed to lookup address",
        "connection refused",
        "connection reset",
        "timed out",
        "timeout",
        "network is unreachable",
        "no route to host",
        "error sending request",
        "tcp connect error",
    ];
    if NETWORK.iter().any(|needle| lower.contains(needle)) {
        // The outermost line is a generic "Transport handshake failed"; the
        // innermost `└─` line names the actual cause.
        let cause = chain
            .lines()
            .filter_map(|l| l.trim_start().strip_prefix("└─"))
            .next_back()
            .unwrap_or(chain);
        return FetchError::Network(first_line(cause));
    }
    FetchError::Other(first_line(chain))
}

/// First line of `chain`, without gix's `", at <source path>:<line>"` suffix,
/// capped at 300 characters.
fn first_line(chain: &str) -> String {
    let line = chain.lines().next().unwrap_or("");
    let line = line.split(", at /").next().unwrap_or(line);
    line.trim().chars().take(300).collect()
}

fn classify<E: std::fmt::Debug>(e: E) -> FetchError {
    classify_fetch_error(&format!("{:?}", e))
}

pub fn cache_path(data_root: &Path, marketplace_id: &str) -> PathBuf {
    data_root
        .join("marketplaces")
        .join(format!("{}.git", marketplace_id))
}

/// Branch names that are safe inside a refspec. Stricter than git's own
/// rules on purpose: nothing that could change the refspec's meaning.
/// `pub(crate)` so the add-marketplace form validates with this same rule
/// (pre-flight F13).
pub(crate) fn valid_branch(branch: &str) -> bool {
    !branch.is_empty()
        && branch.len() <= 200
        && !branch.starts_with('-')
        && !branch.starts_with('/')
        && !branch.ends_with('/')
        && !branch.ends_with(".lock")
        && !branch.contains("..")
        && !branch.contains("//")
        && branch
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'/'))
}

fn open_or_init(repo_path: &Path) -> Result<gix::Repository, FetchError> {
    if repo_path.exists() {
        gix::open(repo_path)
            .map_err(|e| FetchError::Other(format!("Could not open the marketplace cache: {}", e)))
    } else {
        if let Some(parent) = repo_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                FetchError::Other(format!("Could not create {}: {}", parent.display(), e))
            })?;
        }
        gix::init_bare(repo_path).map_err(|e| {
            FetchError::Other(format!("Could not create the marketplace cache: {}", e))
        })
    }
}

/// `(scheme, host[:port])` of a credential request, lowercased, with a
/// default port dropped (gix's own normalisation). None if it names no host.
fn credential_origin(ctx: &gix::credentials::protocol::Context) -> Option<(String, String)> {
    let mut ctx = ctx.clone();
    ctx.destructure_url_in_place(false).ok()?;
    let protocol = ctx.protocol?.to_ascii_lowercase();
    let host = ctx.host?.to_ascii_lowercase();
    (!host.is_empty()).then_some((protocol, host))
}

/// True when a credential request is for the marketplace's own scheme, host
/// and port. gix follows redirects of the initial handshake, and the token
/// must never be offered to a host it was redirected to (final review M1).
pub(crate) fn credential_matches(ctx: &gix::credentials::protocol::Context, url: &str) -> bool {
    let wanted = gix::credentials::protocol::Context::from_url(url, Default::default());
    match (credential_origin(ctx), credential_origin(&wanted)) {
        (Some(asked), Some(wanted)) => asked == wanted,
        _ => false,
    }
}

/// Init the bare repo if missing, fetch `branch` (or the remote's default
/// branch) into [`HEAD_REF`], and return the head commit hex.
pub fn fetch(
    repo_path: &Path,
    url: &str,
    branch: Option<&str>,
    cred: Option<Credential>,
) -> Result<String, FetchError> {
    let refspec = match branch {
        Some(b) if !valid_branch(b) => {
            return Err(FetchError::Other(format!(
                "{:?} is not a valid branch name",
                b
            )));
        }
        Some(b) => format!("+refs/heads/{}:{}", b, HEAD_REF),
        None => format!("+HEAD:{}", HEAD_REF),
    };
    let repo = open_or_init(repo_path)?;
    let remote = repo
        .remote_at(url)
        .map_err(|e| FetchError::Other(format!("Invalid repository URL: {}", e)))?
        .with_refspecs([refspec.as_str()], gix::remote::Direction::Fetch)
        .map_err(|e| FetchError::Other(format!("Invalid refspec: {}", e)))?;
    let own_url = url.to_string();
    let connection = remote
        .connect(gix::remote::Direction::Fetch)
        .map_err(classify)?
        .with_credentials(move |action| match (action, &cred) {
            (gix::credentials::helper::Action::Get(ctx), Some(c))
                if credential_matches(&ctx, &own_url) =>
            {
                Ok(Some(gix::credentials::protocol::Outcome {
                    identity: gix::sec::identity::Account {
                        username: c.username.clone(),
                        password: c.password.clone(),
                        oauth_refresh_token: None,
                    },
                    next: gix::credentials::helper::NextAction::from(ctx),
                }))
            }
            _ => Ok(None),
        });
    connection
        .prepare_fetch(gix::progress::Discard, Default::default())
        .map_err(classify)?
        .receive(gix::progress::Discard, &AtomicBool::new(false))
        .map_err(classify)?;
    cached_head(repo_path)
        .map_err(FetchError::Other)?
        .ok_or_else(|| FetchError::Other("The remote did not return a branch to fetch".to_string()))
}

/// Current [`HEAD_REF`], if fetched before.
pub fn cached_head(repo_path: &Path) -> Result<Option<String>, String> {
    if !repo_path.exists() {
        return Ok(None);
    }
    let repo =
        gix::open(repo_path).map_err(|e| format!("Could not open the marketplace cache: {}", e))?;
    let reference = repo
        .try_find_reference(HEAD_REF)
        .map_err(|e| format!("Could not read {}: {}", HEAD_REF, e))?;
    match reference {
        None => Ok(None),
        Some(mut r) => {
            let id = r
                .peel_to_id()
                .map_err(|e| format!("Could not resolve {}: {}", HEAD_REF, e))?;
            Ok(Some(id.to_string()))
        }
    }
}

pub fn has_commit(repo_path: &Path, commit: &str) -> bool {
    let Ok(repo) = gix::open(repo_path) else {
        return false;
    };
    let Ok(oid) = gix::ObjectId::from_hex(commit.as_bytes()) else {
        return false;
    };
    // Bound before returning: the `Result<Commit<'_>>` temporary borrows
    // `repo` and must drop first (pre-flight F1, E0597 as a tail expression).
    let found = repo.find_commit(oid).is_ok();
    found
}

/// Make `refs/triple-c/pins/*` exactly the given set (commits missing from
/// the cache are skipped), so pinned commits survive later fetches.
pub fn set_pins(repo_path: &Path, commits: &[String]) -> Result<(), String> {
    let repo =
        gix::open(repo_path).map_err(|e| format!("Could not open the marketplace cache: {}", e))?;
    let wanted: std::collections::BTreeSet<&str> = commits.iter().map(String::as_str).collect();

    let mut existing = Vec::new();
    let platform = repo
        .references()
        .map_err(|e| format!("Could not list refs: {}", e))?;
    for reference in platform
        .prefixed(PIN_PREFIX)
        .map_err(|e| format!("Could not list pins: {}", e))?
    {
        let reference = reference.map_err(|e| format!("Could not read a pin: {:?}", e))?;
        existing.push(reference.name().as_bstr().to_string());
    }

    for name in &existing {
        let commit = name.trim_start_matches(PIN_PREFIX);
        if !wanted.contains(commit) {
            if let Some(r) = repo
                .try_find_reference(name.as_str())
                .map_err(|e| format!("Could not read {}: {}", name, e))?
            {
                r.delete()
                    .map_err(|e| format!("Could not remove {}: {}", name, e))?;
            }
        }
    }
    for commit in wanted {
        let name = format!("{}{}", PIN_PREFIX, commit);
        if existing.contains(&name) {
            continue;
        }
        let Ok(oid) = gix::ObjectId::from_hex(commit.as_bytes()) else {
            continue;
        };
        if repo.find_commit(oid).is_err() {
            continue;
        }
        repo.reference(
            name.as_str(),
            oid,
            gix::refs::transaction::PreviousValue::Any,
            "triple-c pin",
        )
        .map_err(|e| format!("Could not pin {}: {}", commit, e))?;
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod test_support {
    //! Fixture repos built with the git CLI. Tests that need one call
    //! [`git_available`] first and return early without it.
    use std::path::Path;
    use std::process::Command;

    pub fn git_available() -> bool {
        Command::new("git")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    pub fn git(dir: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .args([
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@example.invalid",
                "-c",
                "init.defaultBranch=main",
            ])
            .args(args)
            .current_dir(dir)
            .output()
            .expect("git runs");
        assert!(
            out.status.success(),
            "git {:?}: {}",
            args,
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// Write `files` (path, contents, executable) into a new repo and commit.
    pub fn init_repo(dir: &Path, files: &[(&str, &str, bool)]) -> String {
        git(dir, &["init", "-q"]);
        commit_files(dir, files, "initial")
    }

    pub fn commit_files(dir: &Path, files: &[(&str, &str, bool)], message: &str) -> String {
        for (path, contents, exec) in files {
            let full = dir.join(path);
            std::fs::create_dir_all(full.parent().unwrap()).unwrap();
            std::fs::write(&full, contents).unwrap();
            #[cfg(unix)]
            if *exec {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&full, std::fs::Permissions::from_mode(0o755)).unwrap();
            }
            #[cfg(not(unix))]
            let _ = exec;
        }
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-q", "-m", message]);
        git(dir, &["rev-parse", "HEAD"])
    }

    pub fn file_url(dir: &Path) -> String {
        format!("file://{}", dir.display())
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::*;
    use super::*;
    use crate::marketplace::tree::{GitTree, TreeView};

    #[test]
    fn fetch_error_mapping() {
        let cases = [
            ("Credentials provided for \"https://x\" were not accepted by the remote\n└─ Received HTTP status 401", FetchError::Auth { status: 401 }),
            ("handshake\n└─ Received HTTP status 403", FetchError::Auth { status: 403 }),
            ("└─ Received HTTP status 404", FetchError::NotFound),
            ("remote: Repository not found.", FetchError::NotFound),
            // What gix actually reports for an anonymous fetch of a private
            // (or missing) GitHub/GitLab repo (pre-flight F2).
            (
                "No credentials were returned at all as if the credential helper isn't functioning unknowingly, at /home/u/.cargo/registry/src/index/gix-protocol-0.1/src/handshake/function.rs:70",
                FetchError::Auth { status: 401 },
            ),
        ];
        for (text, want) in cases {
            assert_eq!(classify_fetch_error(text), want, "{}", text);
        }
        assert!(matches!(
            classify_fetch_error("error sending request\n└─ dns error: failed to lookup address"),
            FetchError::Network(_)
        ));
        assert!(matches!(
            classify_fetch_error("operation timed out"),
            FetchError::Network(_)
        ));
        assert!(matches!(
            classify_fetch_error("something odd"),
            FetchError::Other(_)
        ));
    }

    #[test]
    fn fetch_error_text_drops_source_locations_and_names_the_network_cause() {
        // Pre-flight F2: gix appends ", at <cargo registry path>:<line>";
        // the innermost `└─` line is the useful network cause.
        let chain = "Transport handshake failed, at /home/u/.cargo/registry/src/x/handshake/function.rs:40\n\
                     ├─ An IO error occurred when talking to the server, at /home/u/.cargo/y.rs:12\n\
                     └─ error resolving DNS, at /home/u/.cargo/z.rs:9";
        assert_eq!(
            classify_fetch_error(chain),
            FetchError::Network("error resolving DNS".to_string())
        );
        let refused = "Transport handshake failed, at /home/u/.cargo/a.rs:1\n└─ Connection refused (os error 111)";
        assert_eq!(
            classify_fetch_error(refused),
            FetchError::Network("Connection refused (os error 111)".to_string())
        );
        assert_eq!(
            classify_fetch_error("Something odd, at /home/u/.cargo/b.rs:3\n└─ deeper"),
            FetchError::Other("Something odd".to_string())
        );
    }

    #[test]
    fn credential_debug_never_shows_the_password() {
        let c = Credential {
            username: "u".into(),
            password: "test-token-not-real".into(),
        };
        let shown = format!("{:?}", c);
        assert!(!shown.contains("test-token-not-real"));
        assert!(shown.contains("<redacted>"));
    }

    /// Final review M1: the token goes only to the marketplace's own scheme,
    /// host and port — never to a host the handshake was redirected to.
    #[test]
    fn credentials_are_offered_only_to_the_marketplace_host() {
        use gix::credentials::protocol::Context;
        let url = "https://git.example.com/org/repo.git";
        let ctx = |u: &str| Context::from_url(u, Default::default());

        assert!(credential_matches(&ctx(url), url));
        assert!(credential_matches(
            &ctx("https://git.example.com/other/path.git"),
            url
        ));
        assert!(credential_matches(
            &ctx("https://GIT.example.com/org/repo.git"),
            url
        ));
        assert!(credential_matches(
            &ctx("https://git.example.com:443/org/repo.git"),
            url
        ));
        for other in [
            "https://evil.example.net/org/repo.git",
            "https://git.example.com.evil.net/org/repo.git",
            "https://git.example.com:8443/org/repo.git",
            "http://git.example.com/org/repo.git",
        ] {
            assert!(!credential_matches(&ctx(other), url), "{other}");
        }
        let with_port = "https://git.example.com:8443/org/repo.git";
        assert!(credential_matches(&ctx(with_port), with_port));
        assert!(!credential_matches(&ctx(url), with_port));
        // A request that names no host gets nothing.
        assert!(!credential_matches(&Context::default(), url));
        let host_only = Context {
            protocol: Some("https".into()),
            host: Some("git.example.com".into()),
            ..Default::default()
        };
        assert!(credential_matches(&host_only, url));
    }

    #[test]
    fn refuses_unsafe_branch_names() {
        let dir = tempfile::tempdir().unwrap();
        for bad in ["-x", "a..b", "a b", "a:b", "x*", "a.lock", ""] {
            let err = fetch(
                &dir.path().join("c.git"),
                "file:///nowhere",
                Some(bad),
                None,
            )
            .unwrap_err();
            assert!(
                matches!(err, FetchError::Other(ref m) if m.contains("branch")),
                "{bad:?}: {err:?}"
            );
        }
    }

    #[test]
    fn valid_branch_accepts_ordinary_names() {
        // pub(crate) so the add-marketplace form validates with the same rule
        // the fetch applies (pre-flight F13).
        for good in ["main", "release/1.2", "feature_x", "v2.0-rc.1"] {
            assert!(valid_branch(good), "{good:?}");
        }
        for bad in [
            "/main", "main/", "a//b", "x.lock", "-x", "a..b", "a b", "a\\b",
        ] {
            assert!(!valid_branch(bad), "{bad:?}");
        }
    }

    #[test]
    fn fetches_default_branch_then_updates() {
        if !git_available() {
            return;
        }
        let src = tempfile::tempdir().unwrap();
        let first = init_repo(
            src.path(),
            &[
                ("agents/a.md", "one", false),
                ("hooks/h/run.sh", "#!/bin/sh", true),
            ],
        );
        let cache = tempfile::tempdir().unwrap();
        let repo = cache_path(cache.path(), "m1");

        assert_eq!(cached_head(&repo).unwrap(), None);
        let head = fetch(&repo, &file_url(src.path()), None, None).unwrap();
        assert_eq!(head, first);
        assert_eq!(cached_head(&repo).unwrap(), Some(first.clone()));
        assert!(has_commit(&repo, &first));

        let tree = GitTree::open(&repo, &first).unwrap();
        assert_eq!(
            tree.read_file("agents/a.md", 1024).unwrap().unwrap(),
            b"one"
        );
        let hook = tree.list_dir("hooks/h").unwrap().unwrap();
        assert!(hook[0].executable);
        assert!(tree.entry_id("agents/a.md").unwrap().is_some());
        assert_eq!(tree.list_dir("agents/a.md").unwrap(), None);

        let second = commit_files(src.path(), &[("agents/a.md", "two", false)], "second");
        assert_eq!(
            fetch(&repo, &file_url(src.path()), None, None).unwrap(),
            second
        );
        // The old commit is still readable after the update.
        assert_eq!(
            GitTree::open(&repo, &first)
                .unwrap()
                .read_file("agents/a.md", 1024)
                .unwrap()
                .unwrap(),
            b"one"
        );
    }

    #[test]
    fn fetches_a_named_branch() {
        if !git_available() {
            return;
        }
        let src = tempfile::tempdir().unwrap();
        init_repo(src.path(), &[("a.md", "main", false)]);
        git(src.path(), &["checkout", "-q", "-b", "next"]);
        let next = commit_files(src.path(), &[("a.md", "next", false)], "next");
        git(src.path(), &["checkout", "-q", "main"]);

        let cache = tempfile::tempdir().unwrap();
        let repo = cache_path(cache.path(), "m1");
        assert_eq!(
            fetch(&repo, &file_url(src.path()), Some("next"), None).unwrap(),
            next
        );
    }

    #[test]
    fn missing_repo_is_an_error_not_a_panic() {
        let cache = tempfile::tempdir().unwrap();
        let err = fetch(
            &cache_path(cache.path(), "m"),
            "file:///definitely/not/here",
            None,
            None,
        )
        .unwrap_err();
        assert!(!matches!(err, FetchError::Auth { .. }), "{err:?}");
    }

    #[test]
    fn refused_connection_is_a_network_error_without_source_paths() {
        // Port 1 on loopback: refused immediately, no real network involved.
        let cache = tempfile::tempdir().unwrap();
        let err = fetch(
            &cache_path(cache.path(), "m"),
            "https://127.0.0.1:1/x.git",
            None,
            None,
        )
        .unwrap_err();
        match err {
            FetchError::Network(m) => assert!(!m.contains(", at /"), "{m}"),
            other => panic!("expected a network error, got {other:?}"),
        }
    }

    #[test]
    fn has_commit_is_false_for_unknown_or_malformed_ids() {
        if !git_available() {
            return;
        }
        let src = tempfile::tempdir().unwrap();
        init_repo(src.path(), &[("x", "1", false)]);
        let cache = tempfile::tempdir().unwrap();
        let repo = cache_path(cache.path(), "m");
        fetch(&repo, &file_url(src.path()), None, None).unwrap();
        assert!(!has_commit(&repo, &"f".repeat(40)));
        assert!(!has_commit(&repo, "not-hex"));
        assert!(!has_commit(
            &cache.path().join("absent.git"),
            &"f".repeat(40)
        ));
    }

    #[test]
    fn pins_are_exactly_the_requested_set() {
        if !git_available() {
            return;
        }
        let src = tempfile::tempdir().unwrap();
        let a = init_repo(src.path(), &[("x", "1", false)]);
        let b = commit_files(src.path(), &[("x", "2", false)], "b");
        let cache = tempfile::tempdir().unwrap();
        let repo = cache_path(cache.path(), "m");
        fetch(&repo, &file_url(src.path()), None, None).unwrap();

        set_pins(&repo, &[a.clone(), b.clone(), "f".repeat(40)]).unwrap();
        let pins = |repo: &Path| -> Vec<String> {
            let r = gix::open(repo).unwrap();
            let mut names: Vec<String> = r
                .references()
                .unwrap()
                .prefixed(PIN_PREFIX)
                .unwrap()
                .map(|x| x.unwrap().name().as_bstr().to_string())
                .collect();
            names.sort();
            names
        };
        let mut want = vec![
            format!("{}{}", PIN_PREFIX, a),
            format!("{}{}", PIN_PREFIX, b),
        ];
        want.sort();
        assert_eq!(pins(&repo), want);

        set_pins(&repo, std::slice::from_ref(&b)).unwrap();
        assert_eq!(pins(&repo), vec![format!("{}{}", PIN_PREFIX, b)]);
    }

    #[test]
    fn git_tree_entry_with_a_backslash_marks_the_item_invalid() {
        // Task 3 review: a real git tree (not MemTree) whose entry name
        // contains `\` must make the catalog reject the item. git itself
        // refuses `/` in names, so `\` is the separator that can get through.
        if !git_available() {
            return;
        }
        let src = tempfile::tempdir().unwrap();
        init_repo(src.path(), &[("skills/ok/SKILL.md", "fine", false)]);
        let evil = commit_files(
            src.path(),
            &[
                ("skills/s/SKILL.md", "x", false),
                ("skills/s/..\\evil.sh", "boom", false),
            ],
            "evil",
        );
        let cache = tempfile::tempdir().unwrap();
        let repo = cache_path(cache.path(), "m");
        assert_eq!(
            fetch(&repo, &file_url(src.path()), None, None).unwrap(),
            evil
        );

        let tree = GitTree::open(&repo, &evil).unwrap();
        let names: Vec<String> = tree
            .list_dir("skills/s")
            .unwrap()
            .unwrap()
            .into_iter()
            .map(|e| e.name)
            .collect();
        assert!(names.contains(&"..\\evil.sh".to_string()), "{names:?}");

        let items = crate::marketplace::catalog::parse_catalog(&tree);
        let skill = items.iter().find(|i| i.key == "s").unwrap();
        assert!(skill.invalid.is_some(), "{skill:?}");
        let ok = items.iter().find(|i| i.key == "ok").unwrap();
        assert!(ok.invalid.is_none(), "{ok:?}");
    }
}
