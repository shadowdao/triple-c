//! Opening a URL in the *host's* browser — the half of triple-c#34 where
//! "Open" appeared to do nothing on Linux.
//!
//! # Why this module exists rather than `openUrl` from `@tauri-apps/plugin-opener`
//!
//! The plugin's Linux path shells out to `xdg-open`, and the child inherits
//! this process's environment verbatim. Inside an AppImage that environment is
//! not the user's — it is the AppImage's, and it is actively hostile to any
//! program that is not the one the bundle was built for:
//!
//!  - linuxdeploy's `AppRun`/`AppRun.wrapped` prepends the bundle's own
//!    directories to `LD_LIBRARY_PATH`, `PATH`, `XDG_DATA_DIRS`, `PYTHONPATH`,
//!    `PERLLIB`, `QT_PLUGIN_PATH` and `GSETTINGS_SCHEMA_DIR`.
//!  - `linuxdeploy-plugin-gtk`'s hook adds `GTK_PATH`, `GTK_EXE_PREFIX`,
//!    `GTK_DATA_PREFIX`, `GTK_IM_MODULE_FILE`, `GIO_MODULE_DIR` and
//!    `GDK_PIXBUF_MODULE_FILE`.
//!  - `scripts/finalize-appimage.sh` installs one more hook of our own
//!    (`triple-c-wayland-fallback.sh`) that can prepend
//!    `$APPDIR/usr/lib/wayland-fallback` to `LD_LIBRARY_PATH`.
//!  - `main.rs` sets `WEBKIT_DISABLE_DMABUF_RENDERER` process-wide, and the
//!    comment there has flagged this leak for a while: it reaches whatever the
//!    app spawns afterwards.
//!
//! A browser that is *already running* is unaffected — `xdg-open` just hands
//! the URL to the existing instance over D-Bus/IPC and the new process exits.
//! A **cold-launched** browser loads our bundled GTK/glib/pixbuf stack against
//! the host's, aborts before it ever paints, and `xdg-open` has already
//! returned 0. From the app's point of view the click did nothing. That is the
//! reported symptom, and it is why the bug only reproduces for some people.
//!
//! # What this does instead
//!
//! `open_url_external` re-validates the URL (see below) and spawns the opener
//! with a **sanitized child environment**. Sanitizing is
//! [`sanitize_child_env`], a pure function over two maps so it can be tested
//! without touching process-wide state:
//!
//!  1. If the AppImage saved the pre-launch value under a `*_ORIG` /
//!     `APPIMAGE_ORIGINAL_*` name, restore that. Restoring a saved original is
//!     strictly better than unsetting, because the user may genuinely have had
//!     an `LD_LIBRARY_PATH` of their own.
//!  2. Otherwise, if the variable differs from the value this process started
//!     with, restore the start-up value. That is what undoes *our own*
//!     `std::env::set_var` — `main.rs` snapshots the environment via
//!     [`capture_pristine_environment`] before any mutation runs.
//!  3. Otherwise, drop only the entries that point inside `$APPDIR`, keeping
//!     the rest of the list intact. Blanket-unsetting would also discard
//!     whatever the user's session had set; this removes exactly the
//!     bundle's own contribution.
//!
//! Nothing is invented: a variable the pristine environment did not have and
//! that does not point into `$APPDIR` is left alone, so outside an AppImage
//! (`cargo tauri dev`, a distro build) this is very close to a no-op.
//!
//! # Portal vs. `xdg-open`
//!
//! `org.freedesktop.portal.OpenURI` would sidestep both the environment leak
//! *and* a missing `x-scheme-handler/https` association, but reaching it means
//! a D-Bus client — `zbus` and its async stack — as a new dependency for one
//! call, on the only platform where we ship a single self-contained binary.
//! It also only helps where a portal is running, which is precisely the
//! desktop-environment case in which `xdg-open` already works once the
//! environment is clean. The environment *is* the bug here, so the cheap fix
//! is the complete one. `gio open` is kept as a second candidate because it
//! goes through GIO's own handler lookup rather than `xdg-open`'s shell
//! heuristics, which covers most of what the portal would have covered.
//!
//! # Security
//!
//! The URL reaching this command originates in an **untrusted container** (see
//! `app/src/lib/urlRelay.ts`). The frontend validates with `sanitizeRelayUrl`,
//! but a compromised webview can call this command directly, so the rules are
//! mirrored here and enforced again: `http`/`https` only, a non-empty host, no
//! embedded credentials, no control characters or whitespace, and a length
//! cap. The URL is never passed through a shell — `std::process::Command` with
//! explicit arguments, so there is no word-splitting, no globbing and no
//! metacharacter to escape.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use url::Url;

/// Hard cap on a URL we will hand to the OS. Mirrors `MAX_RELAY_URL_LENGTH`
/// in `app/src/lib/urlRelay.ts`.
const MAX_URL_LEN: usize = 8192;

/// The environment this process was started with, captured before anything
/// mutates it. See [`capture_pristine_environment`].
// Only the Linux spawn path reads these; the macOS/Windows path delegates to
// the opener plugin. Kept unconditional (rather than `#[cfg(linux)]`) so the
// tests and the documentation stay in one piece on every platform.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
static PRISTINE_ENV: OnceLock<BTreeMap<String, String>> = OnceLock::new();

/// Record the environment as it was at process start.
///
/// Must be called from `main()` **before** any `std::env::set_var` — today
/// that means before `apply_webkit_wayland_workaround()`, which is the only
/// mutation in the tree. Calling it twice is harmless; the first call wins.
///
/// This is the only reliable source of truth for "what did the user actually
/// have?" for variables *we* set. It cannot recover what `AppRun` overwrote
/// before `main()` ran — that is what the `*_ORIG` and `$APPDIR` rules in
/// [`sanitize_child_env`] are for.
pub fn capture_pristine_environment() {
    let _ = PRISTINE_ENV.set(std::env::vars().collect());
}

/// Variables an AppImage launcher is known to override, and that break a
/// cold-launched child that is not this app.
///
/// `PATH` is in the list for the same reason as the rest: `AppRun` prepends
/// `$APPDIR/usr/bin`, and resolving `xdg-open` (or anything the browser's own
/// wrapper script calls) out of the bundle is its own failure mode.
// Only the Linux spawn path reads these; the macOS/Windows path delegates to
// the opener plugin. Kept unconditional (rather than `#[cfg(linux)]`) so the
// tests and the documentation stay in one piece on every platform.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
const SANITIZED_VARS: &[&str] = &[
    "GDK_PIXBUF_MODULEDIR",
    "GDK_PIXBUF_MODULE_FILE",
    "GIO_MODULE_DIR",
    "GSETTINGS_SCHEMA_DIR",
    "GTK_DATA_PREFIX",
    "GTK_EXE_PREFIX",
    "GTK_IM_MODULE_FILE",
    "GTK_PATH",
    "LD_LIBRARY_PATH",
    "PATH",
    "PERLLIB",
    "PYTHONPATH",
    "QT_PLUGIN_PATH",
    "XDG_DATA_DIRS",
    // Set by `main.rs`, not by AppRun — rule 2 (the pristine snapshot) is what
    // removes it, since the pristine environment almost never has it.
    "WEBKIT_DISABLE_DMABUF_RENDERER",
];

/// What to do to one variable in the child: `Some(value)` sets it, `None`
/// removes it.
// Only the Linux spawn path reads these; the macOS/Windows path delegates to
// the opener plugin. Kept unconditional (rather than `#[cfg(linux)]`) so the
// tests and the documentation stay in one piece on every platform.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
type EnvChange = (String, Option<String>);

/// True when `entry` is `appdir` itself or a path inside it.
// Only the Linux spawn path reads these; the macOS/Windows path delegates to
// the opener plugin. Kept unconditional (rather than `#[cfg(linux)]`) so the
// tests and the documentation stay in one piece on every platform.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn is_inside(entry: &str, appdir: &str) -> bool {
    let appdir = appdir.trim_end_matches('/');
    if appdir.is_empty() {
        return false;
    }
    entry == appdir || entry.strip_prefix(appdir).is_some_and(|r| r.starts_with('/'))
}

/// Drop the `$APPDIR` entries from a colon-separated list, keeping order and
/// keeping everything else.
///
/// Single-valued variables (`GDK_PIXBUF_MODULE_FILE`, say) are just lists of
/// one, so they need no separate case: a value inside `$APPDIR` filters down
/// to nothing and the variable is removed.
// Only the Linux spawn path reads these; the macOS/Windows path delegates to
// the opener plugin. Kept unconditional (rather than `#[cfg(linux)]`) so the
// tests and the documentation stay in one piece on every platform.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn strip_appdir_entries(value: &str, appdir: &str) -> Option<String> {
    let kept: Vec<&str> = value
        .split(':')
        .filter(|entry| !entry.is_empty() && !is_inside(entry, appdir))
        .collect();
    if kept.is_empty() {
        None
    } else {
        Some(kept.join(":"))
    }
}

/// Compute the changes that turn `current` into an environment safe to hand a
/// cold-launched host program.
///
/// Pure on purpose — `current` and `pristine` are passed in rather than read
/// from the process, so the rules can be tested without a global mutex around
/// the environment. Returns changes sorted by variable name so assertions are
/// deterministic.
// Only the Linux spawn path reads these; the macOS/Windows path delegates to
// the opener plugin. Kept unconditional (rather than `#[cfg(linux)]`) so the
// tests and the documentation stay in one piece on every platform.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn sanitize_child_env(
    current: &BTreeMap<String, String>,
    pristine: &BTreeMap<String, String>,
    appdir: Option<&str>,
) -> Vec<EnvChange> {
    let mut changes: Vec<EnvChange> = Vec::new();

    for var in SANITIZED_VARS {
        let now = current.get(*var);

        // 1. A saved original always wins. Both spellings are checked because
        //    which one exists depends on the launcher: linuxdeploy's AppRun
        //    and the various `AppRun.wrapped` generations have used each.
        //    An empty saved value means "it was unset", not "set it to empty".
        let saved = current
            .get(&format!("{var}_ORIG"))
            .or_else(|| current.get(&format!("APPIMAGE_ORIGINAL_{var}")));
        if let Some(saved) = saved {
            let restored = if saved.is_empty() {
                None
            } else {
                Some(saved.clone())
            };
            if restored.as_ref() != now {
                changes.push((var.to_string(), restored));
            }
            continue;
        }

        // 2. We changed it ourselves after start-up — put back what was there.
        let at_start = pristine.get(*var);
        if at_start != now {
            changes.push((var.to_string(), at_start.cloned()));
            continue;
        }

        // 3. Polluted before `main()` ran, with nothing saved. Remove the
        //    bundle's own entries and keep the user's.
        let (Some(now), Some(appdir)) = (now, appdir) else {
            continue;
        };
        let stripped = strip_appdir_entries(now, appdir);
        if stripped.as_deref() != Some(now.as_str()) {
            changes.push((var.to_string(), stripped));
        }
    }

    changes.sort_by(|a, b| a.0.cmp(&b.0));
    changes
}

/// Whether `candidate` holds a character that disqualifies it before parsing.
///
/// Mirrors `hasForbiddenChar` in `app/src/lib/urlRelay.ts`, and for the same
/// reasons: C0/C1 controls and whitespace are invisible in the UI and are
/// stripped rather than rejected by some URL parsers, and quote characters are
/// illegal in a URL per RFC 3986 while being exactly what an argument-splitting
/// opener downstream would act on. Written as a scan over code points rather
/// than a regex so the control ranges cannot be mangled by an editing tool.
fn has_forbidden_char(candidate: &str) -> bool {
    candidate.chars().any(|ch| {
        let code = ch as u32;
        code <= 0x20
            || code == 0x7f
            || (0x80..=0x9f).contains(&code)
            || ch == '"'
            || ch == '\''
            || ch == '`'
            || ch.is_whitespace()
    })
}

/// Validate a URL an untrusted source asked the host to open.
///
/// Returns the normalized URL, or a message safe to show the user. The message
/// never echoes the input: it is the input that is untrusted, and this error
/// is rendered in a toast.
fn validate_external_url(raw: &str) -> Result<String, String> {
    // Rust's `trim` strips slightly more than JavaScript's (NEL, U+0085, for
    // one), so a string the frontend would have rejected can reach the parser
    // here with its edges shaved. That only ever removes outer whitespace —
    // everything that survives still has to pass every check below — so the
    // divergence cannot widen what gets opened.
    let candidate = raw.trim();

    if candidate.is_empty() {
        return Err("Refused to open an empty URL.".to_string());
    }
    if candidate.len() > MAX_URL_LEN {
        return Err(format!(
            "Refused to open a URL longer than {MAX_URL_LEN} characters."
        ));
    }
    if has_forbidden_char(candidate) {
        return Err(
            "Refused to open a URL containing whitespace, quotes or control characters."
                .to_string(),
        );
    }

    let parsed = Url::parse(candidate).map_err(|_| "Refused to open a malformed URL.".to_string())?;

    // Scheme allowlist. Nothing else, ever — `file:`, `javascript:`, `data:`
    // and every registered protocol handler stay out of reach of the
    // container. The scheme is safe to interpolate: the parser restricts it to
    // ASCII alphanumerics, `+`, `-` and `.`.
    if parsed.scheme() != "http" && parsed.scheme() != "https" {
        return Err(format!(
            "Refused to open a {}: URL — only http and https are allowed.",
            parsed.scheme()
        ));
    }
    if parsed.host_str().is_none_or(str::is_empty) {
        return Err("Refused to open a URL with no host.".to_string());
    }
    // `https://claude.ai@evil.tld/x` reads as claude.ai anywhere the string is
    // truncated, and navigates to evil.tld.
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err("Refused to open a URL containing embedded credentials.".to_string());
    }

    let normalized = parsed.to_string();
    if normalized.len() > MAX_URL_LEN {
        return Err(format!(
            "Refused to open a URL longer than {MAX_URL_LEN} characters."
        ));
    }
    // A normalized http(s) URL is ASCII by construction — the host is
    // punycoded and everything after it is percent-encoded. Asserting it means
    // nothing non-ASCII can reach an `execvp` argument, whatever the parser
    // decides to do in a future version.
    if !normalized.is_ascii() {
        return Err("Refused to open a URL with non-ASCII characters.".to_string());
    }

    Ok(normalized)
}

/// Openers to try, in order, each as (program, leading arguments).
///
/// `xdg-open` first because it is what the desktop expects to be asked and
/// honours the user's `mimeapps.list`. `gio open` second: it is present
/// wherever glib is (which, for a GTK app's host, is everywhere) and resolves
/// the handler through GIO rather than `xdg-open`'s shell heuristics, so it
/// still works when the `x-scheme-handler/https` association `xdg-open` looks
/// for is missing or points at something broken.
#[cfg(target_os = "linux")]
const OPENERS: &[(&str, &[&str])] = &[("xdg-open", &[]), ("gio", &["open"])];

/// How long a candidate opener is given to fail before it is assumed to have
/// worked.
///
/// `xdg-open` usually returns immediately (it hands the URL to a running
/// browser and exits), but in its generic fallback mode it *is* the browser's
/// parent and stays alive for the session. So "still running" cannot be read
/// as failure, and "exited non-zero quickly" is the only reliable signal
/// there is.
#[cfg(target_os = "linux")]
const OPENER_GRACE: std::time::Duration = std::time::Duration::from_millis(400);

/// Spawn `url` with an opener, under a sanitized environment.
#[cfg(target_os = "linux")]
fn spawn_with_clean_env(url: &str) -> Result<(), String> {
    let current: BTreeMap<String, String> = std::env::vars().collect();
    let pristine = PRISTINE_ENV.get().cloned().unwrap_or_else(|| current.clone());
    let appdir = current.get("APPDIR").cloned();
    let changes = sanitize_child_env(&current, &pristine, appdir.as_deref());

    let mut failures: Vec<String> = Vec::new();

    for (program, leading) in OPENERS {
        let mut command = std::process::Command::new(program);
        command.args(*leading).arg(url);
        // The bundle's own identity is not the child's business either, and a
        // browser that re-execs itself through a wrapper script can pick these
        // up.
        for var in ["APPDIR", "APPIMAGE", "ARGV0", "OWD"] {
            command.env_remove(var);
        }
        for (key, value) in &changes {
            match value {
                Some(value) => command.env(key, value),
                None => command.env_remove(key),
            };
        }
        // Detached: the opener must not inherit our stdio, or a browser
        // writing to stderr keeps a pipe to us open for the session.
        command
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());

        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(err) => {
                failures.push(format!("{program}: {err}"));
                continue;
            }
        };

        std::thread::sleep(OPENER_GRACE);
        match child.try_wait() {
            Ok(Some(status)) if !status.success() => {
                failures.push(format!("{program} exited with {status}"));
                continue;
            }
            Ok(_) => {}
            Err(err) => {
                failures.push(format!("{program}: could not be waited on: {err}"));
                continue;
            }
        }

        // Still running (it is the browser's parent) — reap it off-thread so it
        // does not become a zombie for the life of the app.
        std::thread::spawn(move || {
            let _ = child.wait();
        });
        return Ok(());
    }

    Err(format!(
        "Could not open the link. Tried: {}. Check that xdg-utils is installed and that a default browser is set.",
        failures.join("; ")
    ))
}

/// Open `url` in the user's browser.
///
/// On Linux this goes through [`spawn_with_clean_env`] rather than
/// `@tauri-apps/plugin-opener`, for the AppImage reasons in this module's
/// documentation (triple-c#34). macOS and Windows keep the plugin's path —
/// neither has the environment problem, and `open`/`ShellExecute` are the
/// right calls there — but they are reached through this same command so the
/// frontend has one call site with one set of validation rules.
///
/// Errors are returned rather than logged-and-swallowed: "Open" silently doing
/// nothing is the bug being fixed, so the failure has to be something the UI
/// can show.
#[tauri::command]
pub async fn open_url_external(app: tauri::AppHandle, url: String) -> Result<(), String> {
    let validated = validate_external_url(&url)?;

    #[cfg(target_os = "linux")]
    {
        let _ = &app;
        tauri::async_runtime::spawn_blocking(move || spawn_with_clean_env(&validated))
            .await
            .map_err(|err| format!("Could not open the link: {err}"))?
    }

    #[cfg(not(target_os = "linux"))]
    {
        use tauri_plugin_opener::OpenerExt;
        app.opener()
            .open_url(validated, None::<&str>)
            .map_err(|err| format!("Could not open the link: {err}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    // ── URL re-validation ────────────────────────────────────────────────

    #[test]
    fn plain_http_and_https_urls_are_accepted() {
        for url in [
            "https://claude.ai/",
            "http://localhost:1420/callback?code=abc",
            "https://example.com/path#frag",
        ] {
            assert!(validate_external_url(url).is_ok(), "{url} should be allowed");
        }
    }

    #[test]
    fn urls_are_returned_normalized() {
        assert_eq!(
            validate_external_url("https://Example.COM").unwrap(),
            "https://example.com/"
        );
    }

    #[test]
    fn only_http_and_https_survive() {
        for url in [
            "file:///etc/passwd",
            "javascript:alert(1)",
            "data:text/html,<script>",
            "ftp://example.com/x",
            "vscode://foo/bar",
            "mailto:someone@example.com",
        ] {
            assert!(
                validate_external_url(url).is_err(),
                "{url} must not be openable"
            );
        }
    }

    #[test]
    fn embedded_credentials_are_refused() {
        for url in [
            "https://claude.ai@evil.tld/x",
            "https://user:pass@example.com/",
            "https://:pass@example.com/",
        ] {
            assert!(
                validate_external_url(url).is_err(),
                "{url} must not be openable"
            );
        }
    }

    #[test]
    fn control_characters_and_whitespace_are_refused() {
        // `\n` in particular: parsers that strip it would turn the first of
        // these into a `javascript:` URL.
        for url in [
            "java\nscript:alert(1)",
            "https://example.com/\u{7f}",
            "https://example.com/\u{85}x",
            "https://example.com/a b",
            "https://example.com/\u{00a0}x",
            "https://example.com/\"",
            "https://example.com/'",
            "https://example.com/`",
        ] {
            assert!(
                validate_external_url(url).is_err(),
                "{url:?} must not be openable"
            );
        }
    }

    #[test]
    fn empty_and_oversized_are_refused() {
        assert!(validate_external_url("").is_err());
        assert!(validate_external_url("   ").is_err());
        let long = format!("https://example.com/{}", "a".repeat(MAX_URL_LEN));
        assert!(validate_external_url(&long).is_err());
    }

    #[test]
    fn a_host_is_required() {
        assert!(validate_external_url("https://").is_err());
        assert!(validate_external_url("http://:8080/").is_err());
        // Not a missing host: WHATWG's "special authority ignore slashes"
        // state eats the third slash, so this is the host `path` in both
        // `new URL()` and here. Asserted so the parity is on the record.
        assert_eq!(
            validate_external_url("http:///path").unwrap(),
            "http://path/"
        );
    }

    #[test]
    fn error_messages_never_echo_the_input() {
        // The input is attacker-controlled and the message goes into a toast.
        let err = validate_external_url("file:///home/someone/.ssh/id_rsa").unwrap_err();
        assert!(!err.contains("id_rsa"), "message leaked the input: {err}");
    }

    // ── Environment sanitization ─────────────────────────────────────────

    #[test]
    fn appdir_entries_are_stripped_and_the_users_own_are_kept() {
        let current = map(&[
            ("APPDIR", "/tmp/.mount_abc"),
            ("LD_LIBRARY_PATH", "/tmp/.mount_abc/usr/lib:/opt/mine/lib"),
            ("XDG_DATA_DIRS", "/tmp/.mount_abc/usr/share:/usr/share"),
        ]);
        let changes = sanitize_child_env(&current, &current, Some("/tmp/.mount_abc"));
        assert_eq!(
            changes,
            vec![
                (
                    "LD_LIBRARY_PATH".to_string(),
                    Some("/opt/mine/lib".to_string())
                ),
                ("XDG_DATA_DIRS".to_string(), Some("/usr/share".to_string())),
            ]
        );
    }

    #[test]
    fn a_variable_that_is_entirely_appdir_is_removed() {
        let current = map(&[
            ("APPDIR", "/tmp/.mount_abc"),
            ("GTK_PATH", "/tmp/.mount_abc/usr/lib/gtk-3.0"),
            (
                "GDK_PIXBUF_MODULE_FILE",
                "/tmp/.mount_abc/usr/lib/gdk-pixbuf/loaders.cache",
            ),
        ]);
        let changes = sanitize_child_env(&current, &current, Some("/tmp/.mount_abc"));
        assert_eq!(
            changes,
            vec![
                ("GDK_PIXBUF_MODULE_FILE".to_string(), None),
                ("GTK_PATH".to_string(), None),
            ]
        );
    }

    #[test]
    fn a_saved_original_is_restored_rather_than_unset() {
        // Restoring beats unsetting: the user may have had one of their own.
        for saved_as in ["LD_LIBRARY_PATH_ORIG", "APPIMAGE_ORIGINAL_LD_LIBRARY_PATH"] {
            let current = map(&[
                ("APPDIR", "/tmp/.mount_abc"),
                ("LD_LIBRARY_PATH", "/tmp/.mount_abc/usr/lib"),
                (saved_as, "/home/someone/lib"),
            ]);
            let changes = sanitize_child_env(&current, &current, Some("/tmp/.mount_abc"));
            assert_eq!(
                changes,
                vec![(
                    "LD_LIBRARY_PATH".to_string(),
                    Some("/home/someone/lib".to_string())
                )],
                "{saved_as} should be restored"
            );
        }
    }

    #[test]
    fn an_empty_saved_original_means_it_was_unset() {
        let current = map(&[
            ("APPDIR", "/tmp/.mount_abc"),
            ("LD_LIBRARY_PATH", "/tmp/.mount_abc/usr/lib"),
            ("LD_LIBRARY_PATH_ORIG", ""),
        ]);
        let changes = sanitize_child_env(&current, &current, Some("/tmp/.mount_abc"));
        assert_eq!(changes, vec![("LD_LIBRARY_PATH".to_string(), None)]);
    }

    #[test]
    fn our_own_set_var_is_undone_from_the_pristine_snapshot() {
        // The leak `main.rs` documents: we set this after start-up, so the
        // start-up snapshot is what says it should not exist at all.
        let pristine = map(&[("HOME", "/home/someone")]);
        let current = map(&[
            ("HOME", "/home/someone"),
            ("WEBKIT_DISABLE_DMABUF_RENDERER", "1"),
        ]);
        let changes = sanitize_child_env(&current, &pristine, None);
        assert_eq!(
            changes,
            vec![("WEBKIT_DISABLE_DMABUF_RENDERER".to_string(), None)]
        );
    }

    #[test]
    fn a_value_the_user_set_themselves_is_left_alone() {
        let pristine = map(&[("WEBKIT_DISABLE_DMABUF_RENDERER", "1")]);
        let current = pristine.clone();
        assert!(sanitize_child_env(&current, &pristine, None).is_empty());
    }

    #[test]
    fn outside_an_appimage_nothing_is_touched() {
        let env = map(&[
            ("PATH", "/usr/bin:/bin"),
            ("LD_LIBRARY_PATH", "/opt/mine/lib"),
            ("XDG_DATA_DIRS", "/usr/share"),
        ]);
        assert!(
            sanitize_child_env(&env, &env, None).is_empty(),
            "a dev build or distro build must not have its environment rewritten"
        );
    }

    #[test]
    fn nothing_is_invented_for_variables_that_were_never_set() {
        let env = map(&[("APPDIR", "/tmp/.mount_abc")]);
        assert!(sanitize_child_env(&env, &env, Some("/tmp/.mount_abc")).is_empty());
    }

    #[test]
    fn a_prefix_that_merely_looks_like_appdir_is_not_stripped() {
        // `/tmp/.mount_abc-other` is not inside `/tmp/.mount_abc`.
        let env = map(&[
            ("APPDIR", "/tmp/.mount_abc"),
            ("LD_LIBRARY_PATH", "/tmp/.mount_abc-other/lib"),
        ]);
        assert!(sanitize_child_env(&env, &env, Some("/tmp/.mount_abc")).is_empty());
    }

    #[test]
    fn a_trailing_slash_on_appdir_still_matches() {
        let env = map(&[
            ("APPDIR", "/tmp/.mount_abc/"),
            ("GTK_PATH", "/tmp/.mount_abc/usr/lib/gtk-3.0"),
        ]);
        let changes = sanitize_child_env(&env, &env, Some("/tmp/.mount_abc/"));
        assert_eq!(changes, vec![("GTK_PATH".to_string(), None)]);
    }
}
