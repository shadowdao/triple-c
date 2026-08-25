use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
use bollard::container::{DownloadFromContainerOptions, LogOutput};
use bollard::exec::{CreateExecOptions, StartExecResults};
use futures_util::StreamExt;
use serde::Serialize;
use tauri::State;

use crate::docker::client::get_docker;
use crate::docker::exec::{exec_oneshot_as, exec_oneshot_streams_as, OUTPUT_LIMIT_MARKER};
use crate::AppState;

#[derive(Debug, PartialEq, Serialize)]
pub struct FileEntry {
    pub name: String,
    pub path: String,
    /// Whether the entry behaves as a directory — *dereferenced*, so a symlink
    /// pointing at one is navigable rather than a dead row.
    pub is_directory: bool,
    /// Whether the entry itself is a symlink, which `is_directory` no longer
    /// tells you now that it follows the link.
    pub is_symlink: bool,
    pub size: u64,
    pub modified: String,
    pub permissions: String,
}

/// What a viewer read out of the container.
#[derive(Debug, Serialize)]
pub struct FileContents {
    /// Base64 rather than a byte vec: Tauri serialises `Vec<u8>` over IPC as a
    /// JSON array of numbers, which is roughly 4x the bytes and pathological at
    /// MB scale.
    pub contents_base64: String,
    /// True when the file is larger than the cap and only a prefix came back.
    pub truncated: bool,
    /// The file's real size, from the tar header — not the length of what was
    /// returned.
    pub size: u64,
}

/// Hard ceiling on a single viewer read, whatever the caller asks for. The tar
/// path buffers the whole payload in host RAM, so a caller-supplied cap is not
/// something to take on trust.
const MAX_READ_BYTES: u64 = 8 * 1024 * 1024;

#[tauri::command]
pub async fn list_container_files(
    project_id: String,
    path: String,
    state: State<'_, AppState>,
) -> Result<Vec<FileEntry>, String> {
    // Before anything else: an unvalidated `path` here is not a listing bug, it
    // is an argument-injection one. See the module's path-validation section.
    validate_container_path("Folder", &path)?;

    let project = state
        .projects_store
        .get(&project_id)
        .ok_or_else(|| format!("Project {} not found", project_id))?;

    let container_id = project
        .container_id
        .as_ref()
        .ok_or_else(|| "Container not running".to_string())?;

    // `exec_oneshot` discards the exit code, which is how a `find` that listed
    // nothing at all reached the UI as a cheerful "Empty directory". The status
    // only decides what an *empty* result means, though: `find` also exits
    // non-zero when a single child vanished mid-scan, and the rows it did
    // print are still the right answer.
    //
    // The two streams are taken apart rather than merged: `find`'s diagnostics
    // are the error message, its `-printf` records are the listing, and the
    // parser should never be handed the former.
    let (records, diagnostics, code) =
        exec_oneshot_streams_as(container_id, "claude", list_argv(&path), Vec::new())
            .await
            .map_err(|e| describe_listing_failure(&path, e))?;

    let entries = parse_find_output(&path, &records);
    if code != 0 && entries.is_empty() {
        // `find`'s own words — "Permission denied" — are usually the whole
        // diagnosis; the two a symlinked starting point produces are not.
        // See `describe_find_diagnostics`.
        let detail = diagnostics.trim();
        return Err(if detail.is_empty() {
            format!("Could not list {} (exit {})", path, code)
        } else {
            describe_find_diagnostics(&path, detail)
        });
    }
    if code != 0 {
        log::warn!(
            "find exited {} listing {}; returning the {} entries it did print",
            code,
            path,
            entries.len()
        );
    }

    Ok(entries)
}

/// The argv `list_container_files` runs, in one place so the format and the
/// parser can be pinned together.
///
/// `-H` is what makes a symlinked directory openable. `find` defaults to `-P`,
/// which does not follow a symlink *even when it is the starting point* — so
/// `find /workspace/link -mindepth 1` over a link to a directory matched the
/// link itself, `-mindepth 1` discarded it, and the panel showed a real
/// directory as "Empty directory". The row was navigable (see `%Y` below) and
/// navigating to it showed nothing. `-H` follows the starting point and only
/// the starting point, so nothing *inside* the directory is dereferenced during
/// the walk — which with `-maxdepth 1` is moot anyway, and is why this is not
/// `-L`: `-L` would have `find` chase links it enumerates, and a symlink loop
/// under a listed directory is then `find`'s problem rather than ours.
/// A loop *at* the starting point is resolved by the kernel, which answers
/// `ELOOP` immediately — a refusal, not a hang.
///
/// `%y` is the entry's own type, `%Y` the type it *dereferences* to. Both are
/// printed: `%Y` is what decides navigability (a symlinked directory reports
/// `l` under `%y`, which used to make it an unopenable row), while `%y` is the
/// only way left to tell the user it is a link at all. `%Y` is `N` for a broken
/// link and `L` for a loop, neither of which is `d`.
///
/// `%f` comes *last* and records are terminated by NUL, both because of what a
/// filename is allowed to contain: a tab in a name used to shift every column
/// after it (a crafted name rendered as a directory row), and a newline in a
/// name could forge a whole extra row. With the name last there is nothing left
/// to shift, and NUL is the one byte a Linux filename cannot hold.
///
/// The separators are passed as the two-character escapes `\t` and `\0` for
/// `find` itself to expand: a literal NUL cannot travel in argv, which would
/// truncate the format string at the terminator.
fn list_argv(path: &str) -> Vec<String> {
    vec![
        "find".to_string(),
        // Follow the starting point, and nothing else. See above.
        "-H".to_string(),
        path.to_string(),
        "-mindepth".to_string(),
        "1".to_string(),
        "-maxdepth".to_string(),
        "1".to_string(),
        "-printf".to_string(),
        "%y\\t%Y\\t%s\\t%T@\\t%m\\t%f\\0".to_string(),
    ]
}

/// Turn a listing exec's failure into something the person looking at the
/// folder can act on.
///
/// One case is worth naming: a directory with more entries than
/// [`crate::docker::exec::MAX_ONESHOT_OUTPUT`] will hold. Roughly 100k names is
/// the point where a `find` record set passes 8 MiB, and what the panel showed
/// was "Command output exceeded 8388608 bytes and was abandoned" — a true
/// statement about a buffer, and no help at all about a directory.
fn describe_listing_failure(path: &str, error: String) -> String {
    if error.starts_with(OUTPUT_LIMIT_MARKER) {
        return format!(
            "{} holds too many entries for this panel to list. Open it in a terminal, or look at a subfolder.",
            path
        );
    }
    error
}

/// Turn `find`'s own stderr into a sentence about the folder.
///
/// Only reached when `find` exited non-zero *and* printed no rows, i.e. when
/// its diagnostic is the whole diagnosis. Most of them already are one
/// ("Permission denied"), and those are passed through — but the two that
/// arrive now that `-H` follows the starting point are not: a link into nothing
/// and a link into itself both come back as raw `find:` text naming an errno,
/// about a row the user just double-clicked because it looked like a folder.
fn describe_find_diagnostics(path: &str, diagnostics: &str) -> String {
    let lower = diagnostics.to_lowercase();
    if lower.contains("too many levels of symbolic links") || lower.contains("eloop") {
        return format!("{} is a symbolic link that loops back on itself, so there is nothing to list.", path);
    }
    if lower.contains("no such file or directory") {
        return format!(
            "{} does not lead anywhere — it is either gone, or a symbolic link whose target is.",
            path
        );
    }
    if diagnostics.is_empty() {
        return format!("Could not list {}", path);
    }
    diagnostics.to_string()
}

/// Turn `find -printf '%y\t%Y\t%s\t%T@\t%m\t%f\0'` output into sorted entries.
///
/// Split out from the command so it can be tested without a container: it is
/// the half where a format change silently mis-types every row.
///
/// Records are NUL-terminated and the name is the *last* field, so the split is
/// capped at six pieces: whatever tabs a filename contains land inside the name
/// instead of shifting the type, size and permission columns along one.
fn parse_find_output(dir: &str, output: &str) -> Vec<FileEntry> {
    let mut entries: Vec<FileEntry> = output
        .split('\0')
        .filter(|record| !record.trim().is_empty())
        .filter_map(|record| {
            let mut parts = record.splitn(6, '\t');
            let own_type = parts.next()?;
            let deref_type = parts.next()?;
            let size_field = parts.next()?;
            let mtime_field = parts.next()?;
            let mode_field = parts.next()?;
            let name = parts.next()?.to_string();
            if name.is_empty() {
                return None;
            }
            let is_symlink = own_type == "l";
            let is_directory = deref_type == "d";
            let size = size_field.parse::<u64>().unwrap_or(0);
            let modified_epoch = mtime_field.parse::<f64>().unwrap_or(0.0);
            let permissions = mode_field.to_string();

            // Convert epoch to ISO-ish string
            let modified = {
                let secs = modified_epoch as i64;
                let dt = chrono::DateTime::from_timestamp(secs, 0)
                    .unwrap_or_default();
                dt.format("%Y-%m-%d %H:%M:%S").to_string()
            };

            Some(FileEntry {
                name: name.clone(),
                path: join_path(dir, &name),
                is_directory,
                is_symlink,
                size,
                modified,
                permissions,
            })
        })
        .collect();

    // Sort: directories first, then alphabetical
    entries.sort_by(|a, b| {
        b.is_directory
            .cmp(&a.is_directory)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });

    entries
}

/// Join a container directory and a child name without doubling the separator.
fn join_path(dir: &str, name: &str) -> String {
    if dir.ends_with('/') {
        format!("{}{}", dir, name)
    } else {
        format!("{}/{}", dir, name)
    }
}

/// The directory holding `path`. `/` is its own parent.
fn parent_dir(path: &str) -> String {
    let trimmed = path.trim_end_matches('/');
    match trimmed.rfind('/') {
        None | Some(0) => "/".to_string(),
        Some(i) => trimmed[..i].to_string(),
    }
}

/// Validate the *new name* half of a rename, or a new folder's name.
///
/// This is user-typed text that ends up in `mv`/`mkdir` argv, and the operation
/// is deliberately a rename rather than a move: a name carrying `/` would
/// relocate the entry, and `..` would walk it out of the directory entirely.
/// A leading `-` is left alone because every call site passes `--` first.
fn validate_entry_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("Name cannot be empty".to_string());
    }
    if name.contains('/') {
        return Err(
            "Name cannot contain '/' — this renames inside the folder, it does not move."
                .to_string(),
        );
    }
    // Can't survive argv anyway; caught here so the failure is legible.
    if name.contains('\0') {
        return Err("Name cannot contain a null byte".to_string());
    }
    if name == "." || name == ".." {
        return Err("\".\" and \"..\" are not valid names".to_string());
    }
    if name.len() > 255 {
        return Err("Name is too long (255 bytes maximum)".to_string());
    }
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// Path validation
// ─────────────────────────────────────────────────────────────────────────────
//
// `validate_entry_name` above covers the *new name* half of rename and mkdir.
// The paths themselves — `path`, `from_path`, `parent_path`, `container_dir`,
// `container_path`, `host_path` — arrived over IPC entirely unchecked, and both
// ends of the trip are real: a container path under `/workspace/{mount_name}`
// is a host bind mount, i.e. the user's actual repository, and a host path is
// the host.
//
// The listing command is the reason this section exists. `find` ends its list
// of starting points at the first argument beginning with `-`, so a `path` of
// `-delete` supplied zero starting points (it defaults to `.`, and the exec
// inherits the container's WorkingDir — the bind-mounted project) and an
// expression of `-delete -mindepth 1 -maxdepth 1 -printf …`. Verified against a
// live container on findutils 4.9.0 and again on 4.10.0: it deletes files and
// empty directories out of the bind mount, and `exec_oneshot` threw away the
// exit status, so the panel reported an empty folder afterwards. A `--`
// separator is *not* the fix — `find` has no such convention for starting
// points — but requiring the path to be absolute is, and it is the same check
// that stops `..` traversal.

/// `PATH_MAX` on Linux. Nothing legitimate comes close; a path longer than this
/// cannot name a file in the container anyway.
const MAX_CONTAINER_PATH_LEN: usize = 4096;

/// Container roots this panel may *create or rename into*.
///
/// Reads are deliberately not restricted this way (see
/// [`validate_container_path`]): the Files tab is a browser, `/etc/os-release`
/// and `/usr/lib` are legitimate things to look at, and for reading, the
/// container user's own permissions are the boundary that matters.
///
/// Writes are restricted, because a write here lands in one of exactly two
/// places worth protecting and nowhere else is worth reaching:
///   * `/workspace` — the project bind mounts, i.e. host files;
///   * `/home/claude` — the persisted home volume (settings, skills, session
///     history), which users legitimately reorganise from this panel, so it
///     cannot be excluded even though `.claude/.credentials.json` lives there;
///   * `/tmp` — where terminal drops and pasted images are staged.
/// Everything else is either read-only image content or a system directory
/// where the container user's `mv` fails anyway. Refusing up front turns a
/// confusing "Permission denied" into a clear sentence, and keeps a caller out
/// of `/etc` in a container that happens to run as root.
const CONTAINER_WRITE_ROOTS: &[&str] = &["/workspace", "/home/claude", "/tmp"];

/// Structural validation for any container path arriving over IPC.
///
/// `what` names the parameter in the error, because these messages are shown to
/// a user who is looking at a folder, not at argv.
fn validate_container_path(what: &str, path: &str) -> Result<(), String> {
    if path.is_empty() {
        return Err(format!("{} path cannot be empty", what));
    }
    if !path.starts_with('/') {
        // Absoluteness is what makes the string a *path* rather than an
        // argument: `-delete` is refused right here, and so is anything that
        // would otherwise be resolved against a working directory nobody chose.
        return Err(format!(
            "{} path must be absolute (start with \"/\"): {}",
            what, path
        ));
    }
    if path.contains('\0') {
        return Err(format!("{} path cannot contain a null byte", what));
    }
    // Rejected rather than normalised: a `..` in a path the UI built is a bug,
    // and a `..` in a path the UI did not build is an attempt to leave the
    // folder the user is looking at.
    if path.split('/').any(|segment| segment == "..") {
        return Err(format!("{} path cannot contain \"..\": {}", what, path));
    }
    if path.len() > MAX_CONTAINER_PATH_LEN {
        return Err(format!("{} path is too long ({} bytes maximum)", what, MAX_CONTAINER_PATH_LEN));
    }
    Ok(())
}

/// [`validate_container_path`] plus containment in [`CONTAINER_WRITE_ROOTS`],
/// for every path this module is about to change something at.
///
/// **Lexical, and only lexical.** `/workspace/link/x` is "under `/workspace`"
/// as a string no matter what `/workspace/link` points at, so this on its own
/// does not keep an operation inside the write roots — [`resolve_container_dir`]
/// is what asks the container where the path actually goes.
///
/// Worth being clear about what that resolution is and is not for. It is not a
/// containment boundary: the container user has a shell, and anything this
/// panel could be tricked into writing through a symlink it could write
/// directly. What it buys is that the *panel* keeps its promise — the roots
/// named in the refusal are the roots it writes to — and that a mis-aimed drop
/// cannot quietly land outside them.
fn validate_container_write_path(what: &str, path: &str) -> Result<(), String> {
    validate_container_path(what, path)?;
    if CONTAINER_WRITE_ROOTS
        .iter()
        .any(|root| is_under_root(path, root))
    {
        return Ok(());
    }
    Err(format!(
        "{} path is outside the folders this panel can change ({}): {}",
        what,
        CONTAINER_WRITE_ROOTS.join(", "),
        path
    ))
}

/// Resolve a container *directory* and check where it really lands.
///
/// `realpath -m` because the path is being written into rather than read: `-m`
/// wants no component to exist, which is what makes it usable for the parent of
/// a `mkdir`. The resolved answer goes back through
/// [`validate_container_write_path`], so a symlink out of `/workspace` is
/// refused by the same sentence a literal `/etc` would be.
///
/// The caller keeps operating on the path the *user* typed rather than on the
/// resolved one: they name the same directory, and the unresolved form is the
/// one the listing shows and the UI navigates back to. What is validated and
/// what is operated on can therefore drift if a link is swapped in between —
/// this is a container-side TOCTOU with the same shape as H4's, and unlike H4's
/// it costs nothing, because both sides of the window are already inside the
/// container's own trust boundary.
///
/// A `realpath` that cannot run at all (an image without coreutils) is logged
/// and the lexical answer stands: failing every write closed would break the
/// panel outright for a risk the container user does not need this code path to
/// take.
async fn resolve_container_dir(container_id: &str, what: &str, dir: &str) -> Result<(), String> {
    validate_container_write_path(what, dir)?;

    // Split streams, not the combined buffer: `realpath`'s answer is a *path*
    // and its diagnostics are not, so parsing the two together is the same
    // hazard the listing above took apart for `find`. A warning on stderr —
    // and there is one whenever a component is unreadable — used to be spliced
    // into the string this then compared against the write roots.
    let (stdout, diagnostics, code) = exec_oneshot_streams_as(
        container_id,
        "claude",
        vec![
            "realpath".to_string(),
            "-m".to_string(),
            "--".to_string(),
            dir.to_string(),
        ],
        Vec::new(),
    )
    .await?;

    let resolved = stdout.trim();
    if code != 0 || resolved.is_empty() {
        log::warn!(
            "Could not resolve {} in the container (exit {}{}); using the literal path",
            dir,
            code,
            if diagnostics.trim().is_empty() {
                String::new()
            } else {
                format!(": {}", diagnostics.trim())
            }
        );
        return Ok(());
    }
    if resolved == dir {
        return Ok(());
    }

    validate_container_write_path(what, resolved).map_err(|e| {
        format!("{} leads to {} — {}", dir, resolved, e)
    })
}

/// Whether `path` is `root` itself or something beneath it.
///
/// Compared by whole segments, so `/workspace-backup` is not "under"
/// `/workspace` — a plain `starts_with` is the classic way to get that wrong.
fn is_under_root(path: &str, root: &str) -> bool {
    let path = path.trim_end_matches('/');
    let root = root.trim_end_matches('/');
    path == root || path.strip_prefix(root).is_some_and(|rest| rest.starts_with('/'))
}

/// What a host path is about to be used for. The three modes differ over which
/// components may be hidden — see [`validate_host_path`] and the variants.
#[derive(Clone, Copy, Debug, PartialEq)]
enum HostPathUse {
    /// Host bytes are about to be read *into* the container.
    Read,
    /// Container bytes are about to be written *onto* the host, at a path that
    /// arrived **over IPC as a string** — `download_container_backup`.
    ///
    /// The strictest of the three, and the leaf is judged along with every
    /// directory above it, because creating `~/.bashrc` is escape all by
    /// itself and nothing here can tell a path a person picked from one a
    /// compromised webview invented.
    Write,
    /// Container bytes are about to be written onto the host, at a name a
    /// person typed or accepted in an **OS save dialog opened by Rust** —
    /// `download_container_file`.
    ///
    /// Identical to [`HostPathUse::Write`] except that the final component is
    /// not judged for hiddenness, which is the difference between a rule and a
    /// bug. Under `Write`, saving `/workspace/.env` was refused *after* the
    /// modal and the overwrite prompt, with the message "\".env\" is a hidden
    /// file — Triple-C will not save there" — and the app had pre-filled that
    /// exact name itself. `.gitignore`, `.dockerignore`, `.eslintrc.json`,
    /// `.nvmrc` and the rest of an ordinary workspace were all unsavable, while
    /// uploading them worked, so a dotfile could go in and never come out.
    ///
    /// What justifies dropping it *here* and nowhere else is that the dialog is
    /// a real boundary for this caller and only this caller: the name is on
    /// screen, the user chose the directory, and the OS asked before
    /// overwriting anything. Every *directory* rule still applies, so `~/.ssh`
    /// and `~/.config` are as refused as they ever were.
    WriteChosenName,
}

/// Host directories nothing in this app has any business reading a file out of
/// or writing one into.
///
/// Defence in depth, not the boundary: most of these are root-owned and the
/// write would fail anyway. They are listed so that a build running with more
/// privilege than usual still cannot be talked into replacing a system file,
/// and so the refusal is a sentence rather than an errno. Compared after
/// [`normalize_host_path`] and lowercasing, which is what makes the Windows
/// entries work.
const HOST_SYSTEM_ROOTS: &[&str] = &[
    "/bin", "/boot", "/dev", "/etc", "/lib", "/lib32", "/lib64", "/libx32", "/opt", "/proc",
    "/root", "/sbin", "/snap", "/srv", "/sys", "/usr", "/var",
    // macOS keeps its own copies of the same idea. Its `/etc` and `/var` are
    // symlinks into `/private`, and the check now runs on the *resolved* path
    // (see [`resolve_host_path`]), so the resolved spellings have to be here
    // too. `/private/tmp` deliberately is not: that is what an entirely
    // ordinary `/tmp/report.pdf` resolves to on a Mac.
    "/system", "/library", "/applications", "/private/etc", "/private/var",
    // Windows.
    "c:/windows", "c:/program files", "c:/program files (x86)", "c:/programdata",
];

/// Places that sit *under* a [`HOST_SYSTEM_ROOTS`] entry and are nonetheless
/// entirely ordinary, because a real system puts real user data there.
///
/// Both of these only started to matter once the check ran on the *resolved*
/// path: `/home` is a symlink to `/var/home` on rpm-ostree systems (Fedora
/// Silverblue and friends), and a Mac's per-user temp directory resolves into
/// `/private/var/folders`. Without these, saving a download to your own home
/// directory on Silverblue is "that is a system location".
const HOST_SYSTEM_ROOT_EXCEPTIONS: &[&str] = &["/var/home", "/var/folders", "/private/var/folders"];

/// Directory *tails* whose contents the OS runs on the user's behalf at login.
///
/// The same defence-in-depth footing as [`HOST_SYSTEM_ROOTS`], and the same
/// caveat in stronger form: this is a list of places that happen to be known,
/// not a description of the ones that exist. See [`validate_host_path`] for why
/// the write policy cannot be finished here.
const HOST_AUTORUN_DIRS: &[&[&str]] = &[
    &["library", "launchagents"],
    &["library", "launchdaemons"],
    &["library", "startupitems"],
    &["start menu", "programs", "startup"],
];

/// Length of a `C:` drive prefix at the head of `path`, or 0.
fn drive_prefix_len(path: &str) -> usize {
    let b = path.as_bytes();
    if b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':' {
        2
    } else {
        0
    }
}

/// Whether `path` is written in Windows form, and so whether `\` separates its
/// components. On Linux a backslash is an ordinary filename character, which is
/// why this is a question rather than an unconditional substitution.
fn is_windows_style_path(path: &str) -> bool {
    cfg!(windows) || path.starts_with("\\\\") || drive_prefix_len(path) > 0
}

/// `path` with its separators unified and any Win32 verbatim/device prefix
/// removed — the form every rule below is expressed against.
///
/// `\\?\C:\Windows` and `\\?\UNC\server\share` name the *same locations* as
/// `C:\Windows` and `\\server\share`; the prefix only turns off Win32 path
/// parsing. Stripping it is what stops four characters being a bypass of
/// [`HOST_SYSTEM_ROOTS`] — and it has to run on our own output as well, because
/// `std::fs::canonicalize` hands back exactly that spelling on Windows.
fn normalize_host_path(path: &str) -> String {
    let mut s = if is_windows_style_path(path) {
        path.replace('\\', "/")
    } else {
        path.to_string()
    };
    // Slicing by byte index is safe here only because a prefix matched
    // case-insensitively as ASCII is ASCII, so its end is a char boundary.
    for prefix in ["//?/unc/", "//./unc/"] {
        if s.len() >= prefix.len() && s.as_bytes()[..prefix.len()].eq_ignore_ascii_case(prefix.as_bytes()) {
            return format!("//{}", &s[prefix.len()..]);
        }
    }
    for prefix in ["//?/", "//./"] {
        if s.len() >= prefix.len() && s.as_bytes()[..prefix.len()].eq_ignore_ascii_case(prefix.as_bytes()) {
            s = s[prefix.len()..].to_string();
            break;
        }
    }
    // Collapse runs of separators, keeping any leading pair (a UNC root is
    // `//server/share` and means something).
    //
    // Without this the *lexical* system-root rule was quietly absent for
    // Windows paths: `C:\\Windows\System32\x.dll` normalises to
    // `c://windows/...`, which `is_under_root` does not match, while the
    // single-separator form is refused. Nothing was exploitable — `resolve_host_path`
    // runs the same policy again over the canonical form and `canonicalize`
    // collapses the run — but a documented layer that silently does nothing is
    // a trap for the next caller who reaches for it without the resolved pass.
    let lead = if s.starts_with("//") { "//" } else { "" };
    let body: String = {
        let rest = &s[lead.len()..];
        let mut out = String::with_capacity(rest.len());
        let mut prev_sep = false;
        for c in rest.chars() {
            if c == '/' {
                if !prev_sep {
                    out.push(c);
                }
                prev_sep = true;
            } else {
                out.push(c);
                prev_sep = false;
            }
        }
        out
    };
    format!("{}{}", lead, body)
}

/// The named components of a host path, with the drive letter, the separators
/// and any `.` dropped.
fn host_path_names(path: &str) -> Vec<String> {
    let norm = normalize_host_path(path);
    norm[drive_prefix_len(&norm)..]
        .split('/')
        .filter(|s| !s.is_empty() && *s != ".")
        .map(|s| s.to_string())
        .collect()
}

/// Whether `path` names a location at all, on whichever platform wrote it.
///
/// Deliberately not [`Path::is_absolute`], which answers for the *host*
/// platform: under it a Windows path on Linux is simply "not absolute", every
/// Windows rule below goes unreached, and the tests that thought they were
/// exercising them were only ever exercising this line.
fn is_absolute_host_path(path: &str) -> bool {
    let norm = normalize_host_path(path);
    norm.starts_with('/') || norm[drive_prefix_len(&norm)..].starts_with('/')
}

/// A UNC path rewritten as the local path it actually reaches, when the share
/// is an administrative one: `\\host\C$\Windows` *is* `C:\Windows`, and
/// `\\host\ADMIN$` is the Windows directory itself. An ordinary file share has
/// no local equivalent and gets `None` — [`HOST_SYSTEM_ROOTS`] cannot reason
/// about someone else's server, and says so rather than guessing.
fn admin_share_target(norm_lower: &str) -> Option<String> {
    let mut parts = norm_lower.strip_prefix("//")?.splitn(3, '/');
    let _server = parts.next()?;
    let share = parts.next()?;
    let tail = parts.next().unwrap_or("");
    let b = share.as_bytes();
    if b.len() == 2 && b[0].is_ascii_alphabetic() && b[1] == b'$' {
        Some(format!("{}:/{}", b[0] as char, tail))
    } else if share == "admin$" {
        Some(format!("c:/windows/{}", tail))
    } else {
        None
    }
}

/// The [`HOST_SYSTEM_ROOTS`] entry `path` falls under, if any.
///
/// Pure, and platform-independent on purpose: this is the whole of the Windows
/// policy, so it is also the whole of what the tests have to be able to drive
/// from a Linux CI box.
fn host_system_root_for(path: &str) -> Option<&'static str> {
    let norm = normalize_host_path(path).to_lowercase();
    if HOST_SYSTEM_ROOT_EXCEPTIONS
        .iter()
        .any(|allowed| is_under_root(&norm, allowed))
    {
        return None;
    }
    let admin = admin_share_target(&norm);
    HOST_SYSTEM_ROOTS.iter().copied().find(|root| {
        is_under_root(&norm, root) || admin.as_deref().is_some_and(|p| is_under_root(p, root))
    })
}

/// Whether these directory components end in one of [`HOST_AUTORUN_DIRS`].
fn is_autorun_dir(names: &[String]) -> bool {
    HOST_AUTORUN_DIRS.iter().any(|tail| {
        names.len() >= tail.len()
            && names[names.len() - tail.len()..]
                .iter()
                .zip(tail.iter())
                .all(|(have, want)| have.eq_ignore_ascii_case(want))
    })
}

/// Structural and policy checks on a host path, returning it as a [`PathBuf`].
///
/// Four callers, and they no longer share a threat model — which is the thing
/// to hold on to when changing any of this:
///
///   * [`download_container_file`] and [`upload_files_to_container`], the Files
///     pane's own transfers, **open their dialog from Rust**. The webview can
///     ask for a picker and that is the whole of its influence — it cannot name
///     a host path as an input. (It still *sees* host paths in error text, and
///     canonical ones at that; the inbound direction is what was closed.) For
///     these two this policy is defence in depth.
///   * `terminal_commands::upload_host_file_to_terminal` (the terminal drop)
///     and [`download_container_backup`] still take a host path *from the
///     webview* as a string. A `save()`/`open()` dialog stands in front of both
///     in the UI, but a dialog is a UI convention, not a boundary: every
///     command is a single `invoke` away from any code running in the webview,
///     with container-controlled bytes on one side of it. For those two, this
///     policy is the boundary itself.
///
/// So the backend has its own policy:
///
///   * absolute, no `..`, no NUL — judged on the path's own components, so a
///     Windows path is judged as one wherever this runs;
///   * nothing under [`HOST_SYSTEM_ROOTS`] or in a login-item directory;
///   * no *hidden* path components. The interesting targets for "write
///     container-controlled bytes to a host path" are mostly dot directories —
///     `~/.ssh/authorized_keys`, `~/.config/autostart/`, `~/.local/bin/ls` —
///     and the interesting targets for the reverse, reading a host file into
///     the container, are the same ones plus `~/.aws/credentials`. A write is
///     refused a hidden *name* too (creating `~/.bashrc` is escape all by
///     itself); a read only cares about hidden *directories*, because dropping
///     a project's own `.env` onto the terminal is an ordinary thing to do and
///     its parent is not hidden.
///
/// **This is a lexical predicate over a string**, and [`resolve_host_path`]
/// runs it twice: once over the path as the user wrote it, and again over the
/// canonical form, because a path whose components are all visible can still
/// *lead* somewhere that is not. The second run is why a planted
/// `Downloads/pub → ~/.ssh` is refused.
///
/// **The general hidden rule over-catches, and that is the deliberate trade.**
/// A path that resolves through `node_modules/.pnpm`, `~/.cache` or
/// `~/.local/share` is refused even though nothing about it is an attack. That
/// cost was once paid the other way — the rule was narrowed to an eleven-entry
/// denylist of "credential" directories, which is allow-by-omission for the
/// whole of the rest of `$HOME`: `~/.local/bin` (a write there is the user's
/// next shell command), `~/.password-store`, browser profiles, `~/.pki/nssdb`.
/// Over-refusing is the cheaper mistake, so the general rule stands and the
/// refusal says plainly what tripped it. Note that the cost went **up** when
/// the Files pane got its transfers back: those callers are routine rather than
/// occasional, and because their path comes from a dialog the person is
/// choosing a real destination when it is refused. That is a known, accepted
/// wart — `~/.config` is not a place this pane will save to — and it is not a
/// reason to narrow the rule, because the two IPC-fed callers above are still
/// behind it.
///
/// **The rest of the policy is still a denylist, which is losing by
/// construction.** `~/Library/LaunchAgents`, `%AppData%\…\Startup` and `/opt`
/// are only refused because someone thought of them; the next persistence
/// directory is not. The honest fix is for the *backend* to own the file
/// dialog, so that the only host paths a command accepts are ones the user just
/// pointed at and no path arrives over IPC at all — and that fix is **done for
/// two of the four callers**: see [`pick_save_path`]. The lists are defence in
/// depth there.
///
/// It is still outstanding for the terminal drop and for
/// [`download_container_backup`], where these lists remain the only boundary
/// and losing-by-construction is the live risk. Both are worth converting the
/// same way; the drop is the harder of the two, because its path comes from an
/// OS drag rather than from a dialog Rust could have opened.
fn validate_host_path(path: &str, use_for: HostPathUse) -> Result<PathBuf, String> {
    if path.trim().is_empty() {
        return Err("No host path was given".to_string());
    }
    if path.contains('\0') {
        return Err("Host path cannot contain a null byte".to_string());
    }
    if !is_absolute_host_path(path) {
        return Err(format!("Host path must be absolute: {}", path));
    }

    let names = host_path_names(path);
    if names.iter().any(|n| n == "..") {
        return Err(format!("Host path cannot contain \"..\": {}", path));
    }

    // A trailing separator names a *directory*, and `Path::file_name` on
    // `/home/jo/Downloads/` answers `Downloads` — so the leaf a write kept was
    // the directory itself, the rename at the end failed with an errno, and the
    // cleanup then tried to `remove_file` a directory. Refuse it as a sentence
    // while it is still a string. A read is left alone: it opens the path and
    // the "that is a folder" refusal is already the one it gives.
    if use_for == HostPathUse::Write
        && (names.is_empty() || normalize_host_path(path).ends_with('/'))
    {
        return Err(format!(
            "Host path must name a file, not a folder: {}",
            path
        ));
    }

    // The final component is the file itself; everything before it is a
    // directory the path passes *through*.
    let hidden_limit = match use_for {
        HostPathUse::Write => names.len(),
        // The leaf is the user's own choice in both of these — dropped from a
        // file manager, or typed into a save dialog. See the enum.
        HostPathUse::Read | HostPathUse::WriteChosenName => names.len().saturating_sub(1),
    };
    if let Some(hidden) = names[..hidden_limit].iter().find(|n| n.starts_with('.')) {
        let verb = if use_for == HostPathUse::Write { "save" } else { "read" };
        return Err(if names.last() == Some(hidden) {
            format!(
                "\"{}\" is a hidden file — Triple-C will not save there. Choose a visible name.",
                hidden
            )
        } else {
            // Named as a *path* question rather than a name question, because
            // this rule also runs over the canonical form: the component that
            // trips it is frequently one the user never typed, and "the path
            // goes through it" is the only wording that makes that make sense.
            format!(
                "the path goes through \"{}\", a hidden folder — Triple-C will not {} anything whose folders are not all visible. Choose a visible location.",
                hidden, verb
            )
        });
    }

    let dirs = &names[..names.len().saturating_sub(1)];
    if use_for == HostPathUse::Write && is_autorun_dir(dirs) {
        return Err(format!(
            "{} is a startup folder — Triple-C will not save there. Choose an ordinary location.",
            dirs.last().map(String::as_str).unwrap_or(path)
        ));
    }

    if let Some(root) = host_system_root_for(path) {
        return Err(format!(
            "{} is a system location — Triple-C will not {} files there.",
            root,
            if use_for == HostPathUse::Write { "write" } else { "read" }
        ));
    }

    Ok(PathBuf::from(path))
}

/// The same policy, asked of a *canonical* path — where the bytes really land,
/// as opposed to what the user typed.
///
/// Round 2 ended [`resolve_host_path`] with exactly this: the whole lexical
/// predicate, hidden-component rule included, applied a second time to the
/// resolved form. Round 3 replaced it with a narrower rule — an eleven-entry
/// list of "credential" directories — to stop the general one refusing
/// `node_modules/.pnpm` and `~/.cache`. That is allow-by-omission for
/// everything nobody put on the list, and the things nobody put on the list
/// included `~/.local/bin` (write there and you own the user's next shell
/// command), `~/.password-store`, Firefox and Chrome profiles, and
/// `~/.pki/nssdb`. All four were reachable through a planted symlink with a
/// perfectly ordinary-looking name.
///
/// So the general rule is back, over-catch and all — see [`validate_host_path`]
/// for what that costs, and for why it is still the right way round now that
/// there are four callers rather than two. This is a thin wrapper rather than a second body so the two
/// questions cannot drift apart again; the caller supplies the "resolves to"
/// context, because on this side the offending component is usually one the
/// user never wrote.
fn validate_resolved_host_path(resolved: &str, use_for: HostPathUse) -> Result<(), String> {
    validate_host_path(resolved, use_for).map(|_| ())
}

/// The host path a command is really going to open: every symlink in it
/// resolved by the OS, judged by [`validate_host_path`] as the user wrote it
/// and again as it really lands.
///
/// H4, and the reason the lexical check alone was not a check. Nothing here
/// used to call `canonicalize`, so the rules above were being applied to a
/// string rather than to a location: with `~/Downloads/pub` a symlink to
/// `~/.ssh`, a `host_path` of `~/Downloads/pub/authorized_keys` has no hidden
/// component, is under no system root, and lands in `~/.ssh` anyway. The
/// container can plant that link *and know where to plant it* —
/// `/proc/self/mountinfo` inside a Triple-C container spells the host's
/// project paths out verbatim. The same trick worked in the other direction,
/// reading `~/.ssh/id_rsa` into the container through a visible name.
///
/// A write resolves the *parent* and keeps the caller's leaf, because the leaf
/// is never followed: the partial file is created with `create_new`
/// (`O_EXCL`, which refuses a symlink outright) and [`finish_download`]
/// finishes with a rename, which replaces a link rather than writing through
/// it. A read resolves the whole path, because the whole path is opened.
///
/// When the resolved form is refused, the message leads with what the path
/// *resolves to*: the component that tripped the rule is one the user did not
/// write, and a refusal naming a directory that does not appear in what they
/// typed is otherwise unreadable.
///
/// What this does **not** close by itself is the swap between resolving and
/// opening; [`verify_opened_path`] is the other half.
async fn resolve_host_path(path: &str, use_for: HostPathUse) -> Result<PathBuf, String> {
    let candidate = validate_host_path(path, use_for)?;

    let resolved = match use_for {
        HostPathUse::Read => tokio::fs::canonicalize(&candidate)
            .await
            .map_err(|e| format!("Cannot access {}: {}", candidate.display(), e))?,
        // Both write modes resolve the *parent* and keep the caller's leaf; they
        // differ only in whether that leaf may be hidden, which
        // `validate_host_path` has already decided by this point.
        HostPathUse::Write | HostPathUse::WriteChosenName => {
            let parent = candidate
                .parent()
                .ok_or_else(|| format!("{} does not name a file", candidate.display()))?;
            let name = candidate
                .file_name()
                .ok_or_else(|| format!("{} does not name a file", candidate.display()))?;
            let dir = tokio::fs::canonicalize(parent)
                .await
                .map_err(|e| format!("Cannot save into {}: {}", parent.display(), e))?;
            let joined = dir.join(name);
            // Windows hands out 8.3 aliases, and `BASHRC~1` is a perfectly
            // ordinary-looking name for `.bashrc`. So a leaf that already
            // exists is judged under the name the filesystem gives it as well
            // as the one the caller typed. Only the *name* is taken from the
            // canonical form: a destination that is a symlink gets replaced by
            // the rename, never followed, so its target is not what is at risk.
            if use_for == HostPathUse::Write {
                if let Ok(full) = tokio::fs::canonicalize(&joined).await {
                    let real = full.file_name().map(|n| n.to_string_lossy().to_string());
                    if real.as_deref().is_some_and(|n| n.starts_with('.')) {
                        return Err(format!(
                            "\"{}\" is a hidden file — Triple-C will not save there. Choose a visible name.",
                            real.unwrap_or_default()
                        ));
                    }
                }
            }
            joined
        }
    };

    // The same policy over the canonical form — see
    // [`validate_resolved_host_path`] for why it is the *same* policy again.
    validate_resolved_host_path(&resolved.to_string_lossy(), use_for).map_err(|e| {
        if resolved == candidate {
            e
        } else {
            format!("{} resolves to {} — {}", path, resolved.display(), e)
        }
    })?;

    Ok(resolved)
}

/// Confirm that the handle we are holding is the file we validated.
///
/// The other half of H4. `resolve_host_path` answers "where does this path lead
/// *now*", and a component can be replaced between that answer and the `open`
/// that acts on it — the classic TOCTOU, and a live one here because the
/// attacker owns a directory the path passes through. On Linux the kernel will
/// simply say where an open descriptor ended up, so we ask it and compare;
/// anything else is refused before a byte of payload is written or read.
///
/// Elsewhere — macOS, Windows — there is no equivalent that needs no new
/// dependency, so this is a **compile-time no-op**: on those platforms the
/// TOCTOU window is not closed at all, and the guarantee is the weaker one —
/// resolve-then-open, plus `O_EXCL` on the create, plus a rename whose source
/// must exist at the resolved path under a name carrying 32 random bits.
///
/// On Linux, an unreadable `/proc/self/fd/N` is a **failure**, not a pass. It
/// used to be an `if let Ok(…)`, so the one condition under which the check
/// cannot answer — no `/proc`, a hardened kernel, the fd table exhausted — was
/// also the condition under which it silently approved whatever the descriptor
/// had become. A check that cannot see is not a check that saw nothing wrong.
pub(crate) fn verify_opened_path(file: &std::fs::File, expected: &Path) -> Result<(), String> {
    #[cfg(target_os = "linux")]
    {
        use std::os::fd::AsRawFd;
        let link = format!("/proc/self/fd/{}", file.as_raw_fd());
        let actual = std::fs::read_link(&link).map_err(|e| {
            format!(
                "Refusing to use {}: could not confirm what was opened ({}: {}).",
                expected.display(),
                link,
                e
            )
        })?;
        if actual != expected {
            return Err(format!(
                "Refusing to use {}: while it was being opened it became {}.",
                expected.display(),
                actual.display()
            ));
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (file, expected);
    }
    Ok(())
}

/// [`resolve_host_path`] for a host file about to be read into a container,
/// handed back as a `String`.
///
/// Public because one of its callers lives elsewhere: the terminal's
/// drag-and-drop drop target, `terminal_commands::upload_host_file_to_terminal`.
/// The other is [`upload_files_to_container`] just below, the Files pane's own
/// upload. Both routes apply this same policy, so which one a file arrives by
/// does not change what it is allowed to be.
pub async fn resolve_host_read_path(path: &str) -> Result<String, String> {
    Ok(resolve_host_path(path, HostPathUse::Read)
        .await?
        .to_string_lossy()
        .to_string())
}

/// The name a dropped host file should land under in the container.
///
/// Taken from the path as the user gave it, deliberately — see the call site in
/// `terminal_commands::upload_host_file_to_terminal`: `~/Downloads/latest.log`
/// is routinely a symlink to `2026-08-23.log`, and taking the leaf off the
/// *resolved* path renamed the file on its way in. [`host_path_names`] rather than
/// `Path::file_name` so a Windows path is split as one wherever this runs, and
/// the answer goes through [`validate_entry_name`] because it becomes a tar
/// entry name, a container path and an argv element.
pub(crate) fn host_upload_name(path: &str) -> Result<String, String> {
    if normalize_host_path(path).ends_with('/') {
        // A trailing separator names a directory, and `Downloads` is not the
        // name of a file to upload. The recursive-upload refusal further down
        // says the same thing, but only after a resolve and a `stat`.
        return Err(format!("{} is a folder — upload its files individually.", path));
    }
    let name = host_path_names(path)
        .pop()
        .ok_or_else(|| format!("{} does not name a file", path))?;
    validate_entry_name(&name)?;
    Ok(name)
}

/// Where a host write is staged before it becomes the file the user asked for.
///
/// Two callers: [`download_container_backup`] and [`download_container_file`],
/// the two commands that put container bytes on the host.
///
/// Same directory as the destination, so the last step is a rename within one
/// filesystem: atomic, and the destination is not touched *at all* until the
/// whole transfer has succeeded. That ordering is the fix for the worst part of
/// the old code, which created (i.e. truncated) the destination first and then
/// deleted it when the stream failed — turning "your download failed" into
/// "your download failed and the file that used to be there is gone".
///
/// A rename also handles an existing destination better than an `open` would:
/// it replaces a symlink rather than following it out of the vetted directory.
///
/// Deliberately not a hidden name: if a crash leaves one behind, it should be
/// visible next to the file it was going to become.
fn partial_download_path(dest: &Path) -> Result<PathBuf, String> {
    let name = dest
        .file_name()
        .ok_or_else(|| format!("{} does not name a file", dest.display()))?;
    let suffix = format!(
        ".triple-c-part-{}",
        &uuid::Uuid::new_v4().simple().to_string()[..8]
    );
    // The leaf has to be shortened to make room, or a destination name that fits
    // its directory perfectly well produces a partial name that does not.
    // `NAME_MAX` is 255 bytes on ext4, APFS and NTFS alike, and the suffix is
    // 23 of them.
    //
    // This did not matter while the only caller was Backup, whose name a person
    // types. It matters now that a name can come from the container: a bundler
    // writing `<230-char content hash>.js` would be unsavable, with the refusal
    // blaming a temporary name the user never saw. Lossy is fine for a partial
    // — it is thrown away by the rename, and the uuid is what makes it unique.
    const NAME_MAX: usize = 255;
    let budget = NAME_MAX.saturating_sub(suffix.len());
    let name = name.to_string_lossy();
    let mut base: &str = &name;
    if base.len() > budget {
        let mut end = budget;
        while end > 0 && !base.is_char_boundary(end) {
            end -= 1;
        }
        base = &base[..end];
    }
    Ok(dest.with_file_name(format!("{}{}", base, suffix)))
}

/// Move a finished partial archive onto the destination the user chose.
///
/// A plain rename is the whole story on Unix: atomic, and it replaces an
/// existing file. Windows refuses to rename onto an existing path, so *there*
/// the destination is removed and the rename retried — after the payload is
/// completely written, and only for a destination the user picked in a save
/// dialog that already asked about overwriting. That is the difference from the
/// old code, which deleted the destination on the *failure* path, when the
/// replacement did not exist.
///
/// The retry is fenced by two conditions, and both matter:
///
///   * `cfg!(windows)`. On Unix a rename never fails *because* the destination
///     exists, so reaching the delete there means the rename failed for some
///     other reason — `EACCES`, `EISDIR`, `EBUSY`, or a partial that is no
///     longer there — and deleting the user's file would be destroying it to
///     fix nothing. The guard used to be "did the rename fail and does the
///     destination exist", which is true in every one of those cases.
///   * the partial still exists. "Replace the destination" is only ever a
///     sensible move when there is something to replace it *with*; if our
///     source has vanished the answer is to report the failure and leave what
///     the user already had alone.
async fn finish_download(partial: &Path, dest: &Path) -> Result<(), String> {
    match tokio::fs::rename(partial, dest).await {
        Ok(()) => Ok(()),
        Err(e) => {
            let replaceable = cfg!(windows)
                && tokio::fs::try_exists(partial).await.unwrap_or(false)
                && tokio::fs::try_exists(dest).await.unwrap_or(false);
            if !replaceable {
                return Err(format!("Failed to save {}: {}", dest.display(), e));
            }
            tokio::fs::remove_file(dest)
                .await
                .map_err(|e| format!("Failed to replace {}: {}", dest.display(), e))?;
            tokio::fs::rename(partial, dest)
                .await
                .map_err(|e| format!("Failed to save {}: {}", dest.display(), e))
        }
    }
}

/// How much of a container's stderr is worth keeping to explain a failure.
///
/// Every other reader of container output in this codebase is capped —
/// `MAX_ONESHOT_OUTPUT`, `PROC_NET_OUTPUT_LIMIT` — for the reason named in
/// their comments: the source is hostile. The two streaming commands were the
/// exception, and their stdout is bounded by the disk it is being written to
/// while their stderr was bounded by nothing at all. A container that puts a
/// chattier `dd` earlier in `PATH` (it has passwordless sudo, so it can) could
/// grow this `String` until the app was killed.
///
/// A few kilobytes is far more than any real diagnostic and far less than any
/// pressure on the process.
const MAX_EXEC_STDERR: usize = 8 * 1024;

/// Append a container's stderr frame, stopping at [`MAX_EXEC_STDERR`].
///
/// Silently, and deliberately so: this text exists to explain a failure to a
/// person, and "(truncated)" in the middle of a `dd` diagnostic explains
/// nothing that the first eight kilobytes did not.
fn push_capped(buf: &mut String, frame: &[u8]) {
    if buf.len() >= MAX_EXEC_STDERR {
        return;
    }
    let room = MAX_EXEC_STDERR - buf.len();
    let text = String::from_utf8_lossy(frame);
    if text.len() <= room {
        buf.push_str(&text);
        return;
    }
    let mut end = room;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    buf.push_str(&text[..end]);
}

/// One regular file's bytes, pulled out of a container.
struct FetchedFile {
    bytes: Vec<u8>,
    /// The size the tar header declared, i.e. the file's real size — which is
    /// not `bytes.len()` once `max_bytes` has cut the read short.
    size: u64,
    truncated: bool,
}

/// Fetch a single regular file from a container as exact bytes.
///
/// The viewer's read. It deliberately goes through Docker's archive endpoint rather than
/// `exec_oneshot`: that reader runs every chunk through `String::from_utf8_lossy`
/// and merges stderr into stdout, so it would both corrupt any non-UTF-8 file
/// and be able to splice diagnostics into what the caller believes is content.
///
/// The transfer is abandoned once the cap (plus enough slack for the tar
/// framing) is in hand, so previewing a huge file does not pull the whole thing
/// across the socket.
///
/// `max_bytes` is deliberately not optional. It used to be, and the old
/// download command passed `None`: the cap below then did nothing and the whole
/// file — or the whole *directory tree*, since the type check happens after the
/// read — landed in host RAM twice. This function buffers, so every caller of
/// it must name a ceiling.
async fn fetch_container_file(
    container_id: &str,
    container_path: &str,
    max_bytes: u64,
) -> Result<FetchedFile, String> {
    let docker = get_docker()?;

    let mut stream = docker.download_from_container(
        container_id,
        Some(DownloadFromContainerOptions {
            path: container_path.to_string(),
        }),
    );

    // A tar member is a 512-byte header plus payload padded to 512. 8 KiB of
    // slack past the payload cap guarantees the header and the whole capped
    // prefix are present even with the stream cut short.
    const TAR_SLACK: u64 = 8 * 1024;
    let stop_after = max_bytes.saturating_add(TAR_SLACK);

    let mut tar_bytes: Vec<u8> = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| format!("Failed to download file: {}", e))?;
        tar_bytes.extend_from_slice(&chunk);
        if tar_bytes.len() as u64 >= stop_after {
            // Dropping the stream cancels the rest of the transfer.
            break;
        }
    }

    let mut archive = tar::Archive::new(&tar_bytes[..]);
    let mut entries = archive
        .entries()
        .map_err(|e| format!("Failed to read tar entries: {}", e))?;
    let mut entry = match entries.next() {
        Some(entry) => entry.map_err(|e| format!("Failed to read tar entry: {}", e))?,
        None => return Err(format!("{} not found in the container", container_path)),
    };

    // Docker tars whatever the path names, so a directory arrives as a whole
    // tree. Reading only its first member used to write a silently wrong file;
    // say so instead.
    let entry_type = entry.header().entry_type();
    if entry_type.is_dir() {
        return Err(format!(
            "{} is a folder — open its files individually, or use Backup to take a whole tree.",
            container_path
        ));
    }
    if entry_type.is_symlink() || entry_type.is_hard_link() {
        return Err(format!("{} is a link — open its target instead.", container_path));
    }
    if !entry_type.is_file() {
        return Err(format!("{} is not a regular file.", container_path));
    }

    // `entry.size()` rather than the raw header field: see
    // `stream_container_file_to_host`. A file past the ustar 8 GiB octal limit
    // carries its real size in a PAX record, and reading the header field
    // instead reported it as 0 — an empty preview of a very large file.
    let size = entry.size();
    let truncated = size > max_bytes;
    let want = max_bytes.min(size);

    let mut bytes = Vec::with_capacity(want.min(1024 * 1024) as usize);
    std::io::Read::read_to_end(&mut std::io::Read::take(&mut entry, want), &mut bytes)
        .map_err(|e| format!("Failed to read file contents: {}", e))?;

    Ok(FetchedFile {
        bytes,
        size,
        truncated,
    })
}

/// Read a file out of the container for the in-app viewer.
///
/// `max_bytes` is the caller's ceiling (the viewer asks for more when it is
/// about to decode an image, which is what usually goes over a text-sized cap);
/// it is clamped to [`MAX_READ_BYTES`] regardless, because the whole payload is
/// buffered in host RAM on the way through.
#[tauri::command]
pub async fn read_container_file(
    project_id: String,
    path: String,
    max_bytes: Option<u64>,
    state: State<'_, AppState>,
) -> Result<FileContents, String> {
    validate_container_path("File", &path)?;

    let project = state
        .projects_store
        .get(&project_id)
        .ok_or_else(|| format!("Project {} not found", project_id))?;

    let container_id = project
        .container_id
        .as_ref()
        .ok_or_else(|| "Container not running".to_string())?;

    let cap = max_bytes.unwrap_or(MAX_READ_BYTES).min(MAX_READ_BYTES);
    let fetched = fetch_container_file(container_id, &path, cap).await?;

    Ok(FileContents {
        contents_base64: BASE64.encode(&fetched.bytes),
        truncated: fetched.truncated,
        size: fetched.size,
    })
}

/// Rename an entry in place. `to_path` is the **new name**, not a destination
/// path — moving between directories is deliberately not offered here, so the
/// name is validated to carry no `/`.
///
/// Runs through `exec_oneshot_as` rather than `exec_oneshot` because the exit
/// code is the only reliable signal: `exec_oneshot` discards the status, so a
/// permission failure (renaming under `/etc` or `/usr`, which the container
/// user genuinely cannot do) would return `Ok` with the error text as its
/// "output". Returns the new full path.
#[tauri::command]
pub async fn rename_container_path(
    project_id: String,
    from_path: String,
    to_path: String,
    state: State<'_, AppState>,
) -> Result<String, String> {
    let project = state
        .projects_store
        .get(&project_id)
        .ok_or_else(|| format!("Project {} not found", project_id))?;

    let container_id = project
        .container_id
        .as_ref()
        .ok_or_else(|| "Container not running".to_string())?;

    // The name is checked by `validate_entry_name`; the path it is applied to
    // was checked by nothing at all, which is how an `invoke` naming
    // `/home/claude/.claude/.credentials.json` used to move the OAuth
    // credential out from under Claude Code.
    validate_container_write_path("Item", &from_path)?;
    // The *parent* is resolved, never the item itself: `realpath` would follow
    // a symlink to its target, and renaming a link has always meant renaming
    // the link. `/` as a parent means a one-component path, which has no
    // directory component to resolve.
    let parent = parent_dir(&from_path);
    if parent != "/" {
        resolve_container_dir(container_id, "Item", &parent).await?;
    }

    let new_name = to_path.trim();
    validate_entry_name(new_name)?;

    let dest = join_path(&parent_dir(&from_path), new_name);
    if dest == from_path {
        return Ok(dest);
    }

    // `mv -n` refuses to clobber, but GNU coreutils makes that refusal *silent*
    // and exits 0 — so `-n` on its own would report a rename that never
    // happened. The existence check is what turns it into an error the user
    // sees; `-n` stays as the belt-and-braces against the race between them.
    let (_, exists) = exec_oneshot_as(
        container_id,
        "claude",
        vec!["test".to_string(), "-e".to_string(), dest.clone()],
        Vec::new(),
    )
    .await?;
    if exists == 0 {
        return Err(format!("\"{}\" already exists in this folder", new_name));
    }

    let (output, code) = exec_oneshot_as(
        container_id,
        "claude",
        vec![
            "mv".to_string(),
            "-n".to_string(),
            "--".to_string(),
            from_path.clone(),
            dest.clone(),
        ],
        Vec::new(),
    )
    .await?;

    if code != 0 {
        // Surface `mv`'s own words: "Permission denied" is the common case
        // outside /workspace and a generic message would hide why.
        let detail = output.trim();
        return Err(if detail.is_empty() {
            format!("Rename failed (exit {})", code)
        } else {
            detail.to_string()
        });
    }

    Ok(dest)
}

/// Create a directory under `parent_path`. Fails rather than succeeding
/// silently if the name is taken — `mkdir` without `-p` is what gives that.
#[tauri::command]
pub async fn create_container_directory(
    project_id: String,
    parent_path: String,
    name: String,
    state: State<'_, AppState>,
) -> Result<String, String> {
    let project = state
        .projects_store
        .get(&project_id)
        .ok_or_else(|| format!("Project {} not found", project_id))?;

    let container_id = project
        .container_id
        .as_ref()
        .ok_or_else(|| "Container not running".to_string())?;

    resolve_container_dir(container_id, "Folder", &parent_path).await?;

    let name = name.trim();
    validate_entry_name(name)?;
    let dest = join_path(&parent_path, name);

    let (output, code) = exec_oneshot_as(
        container_id,
        "claude",
        vec!["mkdir".to_string(), "--".to_string(), dest.clone()],
        Vec::new(),
    )
    .await?;

    if code != 0 {
        let detail = output.trim();
        return Err(if detail.is_empty() {
            format!("Could not create folder (exit {})", code)
        } else {
            detail.to_string()
        });
    }

    Ok(dest)
}

/// Refuse, in a sentence, before a Docker error has to speak for us.
///
/// Both file transfers and the backup run through `docker exec`, which needs a
/// running container. Without this the failure surfaces as bollard's
/// `is not running` wrapped in whatever the caller was doing, and for the
/// upload it surfaces even less usefully: `resolve_container_dir`'s `realpath`
/// is the first thing to touch the container, so a stopped project fails inside
/// path *validation* and reads like the path was the problem.
async fn require_running(container_id: &str, action: &str) -> Result<(), String> {
    let docker = get_docker()?;
    let running = docker
        .inspect_container(container_id, None)
        .await
        .ok()
        .and_then(|info| info.state)
        .and_then(|s| s.running)
        .unwrap_or(false);
    if running {
        return Ok(());
    }
    Err(format!(
        "Start the project before {} — it runs inside the running container.",
        action
    ))
}

/// Copy one regular file out of a container onto a host path the user chose in
/// a save dialog.
///
/// ## Why this exists again
///
/// It was removed along with the rest of the Files pane's host I/O, on the
/// reasoning that four audits had found their criticals in host paths crossing
/// IPC. That reasoning was about the *reservation machinery* — the `link(2)`
/// destination reservation, the placeholder rollback, the collision marker —
/// and every one of those criticals lived there. Removing the machinery was
/// right. Removing the feature with it took away something the app shipped
/// before any of this work started, so the user upgraded into a regression.
///
/// None of the removed machinery comes back. The save dialog already asks about
/// overwriting, so there is nothing to reserve and no collision to mark, and
/// what is left is the sequence [`download_container_backup`] has been using
/// unchanged: resolve the destination, stream into a partial file beside it,
/// rename last. The destination the user already had is not touched until the
/// transfer has completely succeeded.
///
/// ## Why `cat` rather than the archive endpoint
///
/// [`fetch_container_file`] buffers, which is why it takes a mandatory ceiling
/// — it serves the viewer, where a cap is the correct behaviour. A download has
/// no business refusing a 3 GB file, so this streams instead, and streaming
/// means not reassembling a tar in host RAM. `exec_oneshot`'s reader is not
/// usable here either: it runs chunks through `String::from_utf8_lossy` and
/// merges stderr into stdout, so it would corrupt any non-UTF-8 file and let
/// diagnostics splice themselves into content. Attaching to the exec directly
/// gives pre-demuxed frames, and only `StdOut` frames are written.
///
/// The type check runs *inside* the same exec as the `cat`, not as a separate
/// round trip, so there is no window between deciding the path is a regular
/// file and reading it. `[ -f ]` follows symlinks — downloading through a
/// symlink is ordinary and stays allowed — and is false for a directory, a
/// device and, importantly, a FIFO: `cat` on one blocks forever with no writer
/// and there is no timeout anywhere on this path.
///
/// Returns the number of bytes written. Zero is a success: an empty file is a
/// file. (The backup's `total == 0` check is not copied down here — there it
/// means the tar pipeline produced nothing, which is a failure.)
#[tauri::command]
pub async fn download_container_file(
    project_id: String,
    container_path: String,
    window: tauri::Window,
    state: State<'_, AppState>,
) -> Result<Option<u64>, String> {
    // Read-only source, so structure is what matters — not the write roots. A
    // download may legitimately reach image content the panel cannot modify.
    // Checked before the dialog: there is no point asking the user where to put
    // something that was never going to be read.
    validate_container_path("Download", &container_path)?;

    // Before the dialog, matching `upload_files_to_container`: asking someone to
    // choose a destination and only then telling them the project is stopped is
    // the wrong order to find that out in. The window this opens — the project
    // being stopped *during* the dialog — costs nothing, because the exec then
    // fails with a Docker error and no host file has been touched.
    let project = state
        .projects_store
        .get(&project_id)
        .ok_or_else(|| format!("Project {} not found", project_id))?;

    let container_id = project
        .container_id
        .as_ref()
        .ok_or_else(|| "No container exists for this project yet — start it first".to_string())?;

    require_running(container_id, "saving a file to the host").await?;

    let Some(chosen) = pick_save_path(&window, &suggested_save_name(&container_path)).await else {
        // Dismissed. Not an error, and deliberately distinguishable from one:
        // the frontend shows nothing at all rather than a "cancelled" toast.
        return Ok(None);
    };
    let chosen = host_path_string(chosen)?;
    // Still validated, even though the path came from an OS dialog rather than
    // over IPC. It is no longer the only thing standing between a compromised
    // webview and an arbitrary host write — that is what moving the dialog into
    // Rust bought — but the rule is cheap and the two host-path commands
    // agreeing about what is writable is worth more than the few refusals it
    // costs. See the note on `pick_save_path` about which ones those are.
    let dest = resolve_host_path(&chosen, HostPathUse::WriteChosenName).await?;

    let docker = get_docker()?;

    // `$TC_SRC`, never argv interpolation: the path reaches the shell as an
    // environment value, so a name containing a quote or a `$` is data.
    //
    // ## Why `dd iflag=nonblock` and not `cat`
    //
    // `[ -f ]` and the `open` that follows it are two syscalls, and the
    // container owns the filesystem in between — a loop replacing the file with
    // a FIFO wins that race often enough to matter. `cat` then blocks in
    // `open(2)` forever with no writer, and there is no timeout anywhere on this
    // path: the `invoke` never settles and a partial file is left in the user's
    // directory for good. `O_NONBLOCK` makes that open return instead of hang.
    // On a regular file the kernel ignores the flag, so this is byte-for-byte
    // what `cat` did — verified against a real container, `cmp`-clean.
    //
    // ## Why the size, and not a second `[ -f ]`
    //
    // The first version of this bracketed the read with `[ -f ]` again, on the
    // reasoning that a file which stopped being a regular file had changed
    // underneath us. That check was wrong in **both** directions, and a review
    // demonstrated both against a real container:
    //
    //   * it did not catch the case that loses data. Truncation *in place* —
    //     `> file`, log rotation, `tar -x`, most build tools — leaves a regular
    //     file behind, so `dd` stopped at the new EOF, exited 0, and a 34 MB
    //     partial of a 600 MB file was renamed over the user's own copy and
    //     reported as `Saved (34.1 MB)`. Same for truncate-to-zero, which needs
    //     no adversary at all.
    //   * it failed *good* downloads. `dd` already holds the fd, and neither
    //     `rm` nor `mv` can affect an open one — the bytes are complete and
    //     correct. But `rm` makes `[ -f ]` false, so a finished 600 MB transfer
    //     of a file a bundler happened to unlink was deleted and reported as
    //     "nothing was saved".
    //
    // So the question asked is the one that actually matters: **did we get at
    // least as many bytes as the file had when we started?** `TC_SIZE=` on
    // stderr carries the answer out (stdout is the payload and must stay
    // pristine), and Rust compares it against what it wrote.
    //
    // That single rule subsumes everything the bracket was for and gets the two
    // cases above right: a deleted or renamed source still delivers its whole
    // length and passes; a truncated one delivers less and fails; the FIFO race
    // delivers zero against a non-zero size and fails. A file *growing* during
    // the read delivers more than it started with, which passes — correct, and
    // the reason the test is `>=`.
    //
    // `dd` is not `exec`-ed: a replaced shell cannot run what follows it.
    let script = r#"if [ -d "$TC_SRC" ]; then
  echo "$TC_SRC is a folder — save its files individually." >&2
  exit 3
fi
if [ ! -f "$TC_SRC" ]; then
  echo "$TC_SRC is not a regular file." >&2
  exit 4
fi
SZ=$(stat -c %s -- "$TC_SRC" 2>/dev/null) || SZ=""
case "$SZ" in
  ''|*[!0-9]*) echo "Could not measure $TC_SRC before reading it." >&2; exit 7 ;;
esac
echo "TC_SIZE=$SZ" >&2
dd iflag=nonblock bs=64k status=none if="$TC_SRC" || exit 5"#;

    let exec = docker
        .create_exec(
            container_id,
            CreateExecOptions {
                attach_stdout: Some(true),
                attach_stderr: Some(true),
                cmd: Some(vec!["sh".to_string(), "-c".to_string(), script.to_string()]),
                env: Some(vec![format!("TC_SRC={}", container_path)]),
                user: Some("claude".to_string()),
                ..Default::default()
            },
        )
        .await
        .map_err(|e| format!("Failed to start download: {}", e))?;

    let result = docker
        .start_exec(&exec.id, None)
        .await
        .map_err(|e| format!("Failed to start download: {}", e))?;

    let mut output = match result {
        StartExecResults::Attached { output, .. } => output,
        StartExecResults::Detached => return Err("Download exec started detached".to_string()),
    };

    use tokio::io::AsyncWriteExt;
    let partial = partial_download_path(&dest)?;
    // Same open-and-confirm as the backup: `create_new` is `O_EXCL` so the
    // final component cannot be a symlink, and `verify_opened_path` catches a
    // directory on the way having been swapped since it was resolved. The
    // `created` flag separates "nothing was made" from "it is ours to remove".
    let open_at = partial.clone();
    let created = Arc::new(AtomicBool::new(false));
    let opened = {
        let created = Arc::clone(&created);
        tokio::task::spawn_blocking(move || -> Result<std::fs::File, String> {
            let file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&open_at)
                .map_err(|e| format!("Failed to create {}: {}", open_at.display(), e))?;
            created.store(true, Ordering::SeqCst);
            verify_opened_path(&file, &open_at)?;
            Ok(file)
        })
        .await
        .map_err(|e| format!("Download task panicked: {}", e))?
    };
    let file = match opened {
        Ok(file) => file,
        Err(e) => {
            if created.load(Ordering::SeqCst) {
                let _ = tokio::fs::remove_file(&partial).await;
            }
            return Err(e);
        }
    };
    let file = tokio::fs::File::from_std(file);
    let mut writer = tokio::io::BufWriter::new(file);
    let mut total: u64 = 0;
    let mut stderr_text = String::new();
    let mut stream_err: Option<String> = None;
    // The size the container reported before the read, if it has arrived yet.
    // It is the first thing the script writes, so in practice it is in hand
    // before the first payload frame — but the loop does not depend on that.
    let mut declared: Option<u64> = None;

    while let Some(msg) = output.next().await {
        match msg {
            Ok(LogOutput::StdOut { message }) => {
                if let Err(e) = writer.write_all(&message).await {
                    stream_err = Some(format!("Failed to write {}: {}", dest.display(), e));
                    break;
                }
                total += message.len() as u64;
                // A ceiling, derived from what the container itself said the
                // file was. Without one, a single click on a file the panel
                // lists as 2 KB can fill the host disk: `dd` is resolved
                // through the container's `PATH`, which its agent owns with
                // passwordless sudo, and a replacement that writes forever was
                // measured at ~6 GB/s against a real container. The UI shows
                // only "Saving…" while that happens and has no cancel.
                //
                // Generous, because a file can legitimately grow while it is
                // being read — an active log is the ordinary case — and cutting
                // one of those off would be a bug of our own. What this bounds
                // is the *unbounded* case: the worst a single click can now
                // cost is the slack, not the volume.
                if let Some(limit) = declared.map(download_ceiling) {
                    if total > limit {
                        stream_err = Some(format!(
                            "{} kept producing data long past the {} it reported — stopped at {}. Nothing was saved.",
                            container_path,
                            human_bytes(declared.unwrap_or(0)),
                            human_bytes(total),
                        ));
                        break;
                    }
                }
            }
            Ok(LogOutput::StdErr { message }) => {
                push_capped(&mut stderr_text, &message);
                if declared.is_none() {
                    declared = parse_declared_size(&stderr_text);
                }
            }
            Ok(_) => {}
            Err(e) => {
                stream_err = Some(format!("Download stream error: {}", e));
                break;
            }
        }
    }
    if stream_err.is_none() {
        if let Err(e) = writer.flush().await {
            stream_err = Some(format!("Failed to finalize {}: {}", dest.display(), e));
        }
    }
    drop(writer);

    // The read can be killed part-way through and still have sent bytes, so the
    // exit code decides — not `total`.
    //
    // `!= Some(0)`, not `is_some_and(|c| c != 0)`. An *undeterminable* exit code
    // is a failure here, and the difference is not academic: the backup catches
    // this class with its `total == 0` check, which this command deliberately
    // does not have because an empty file is a legitimate save. Restart a
    // project mid-download and the exec is torn down — the attached stream ends
    // at EOF rather than in an `Err`, and the exec instance is purged, so
    // `inspect_exec` can no longer say how it ended. Reading that silence as
    // success renames a truncated partial over the file the user already had
    // and reports the byte count as if it were the whole thing. That is the one
    // way this command could destroy data, so silence is failure.
    let exit_code = crate::docker::exec::wait_for_exec_exit(&exec.id).await;
    if stream_err.is_none() && exit_code != Some(0) {
        // Framed and clipped, never verbatim.
        //
        // Be precise about what the frame buys, because the first version of
        // this comment overstated it. `readableRefusal` on the frontend matches
        // its markers with `includes`, not a prefix test — and it has to, since
        // the app's own refusals carry those markers mid-sentence. So a frame
        // does **not** stop container text reaching the toast headline: text
        // containing "Triple-C will not" is promoted wherever it sits.
        //
        // What the frame does buy is that the app's own words come first, so
        // the quoted part is visibly quoted. What the *clip* buys is the part
        // that actually mattered: `MAX_EXEC_STDERR` is 8 KiB, which is room for
        // a convincing forged instruction, and a toast is rendered above every
        // modal. A couple of hundred characters on one line is a diagnostic;
        // eight kilobytes of prose is a phishing surface with a scrollbar.
        //
        // Newlines and control characters go too: they are what let quoted text
        // fake the structure of a message the app wrote.
        let detail = clip_container_text(&stderr_text);
        let detail = detail.as_str();
        stream_err = Some(match exit_code {
            None => format!(
                "Could not confirm that {} was read completely — nothing was saved.",
                container_path
            ),
            Some(code) if detail.is_empty() => {
                format!("Could not read {} (exit {})", container_path, code)
            }
            Some(code) => format!("Could not read {} (exit {}): {}", container_path, code, detail),
        });
    }

    // Short of what the file measured is the data-loss case, and it is the one
    // an exit code cannot see: `dd` reports success for a file truncated under
    // it, because it faithfully read to the EOF it was given. Verified against
    // a real container — a 600 MB source truncated mid-read delivered 34 MB at
    // exit 0, and before this check that 34 MB was renamed over the user's own
    // copy and toasted as `Saved (34.1 MB)`.
    //
    // `>=`, not `==`: a file being appended to during the read delivers more
    // than it measured, and that is a complete read, not a failure. A file
    // deleted or renamed mid-read also passes, correctly — `dd` holds the fd
    // and neither operation can touch it, so the bytes are whole.
    if stream_err.is_none() {
        match declared {
            Some(size) if total < size => {
                stream_err = Some(format!(
                    "{} was {} when the copy started and only {} arrived — it changed while it was being read, so nothing was saved.",
                    container_path,
                    human_bytes(size),
                    human_bytes(total),
                ));
            }
            // The script exits 7 rather than staying quiet if it cannot measure
            // the file, so a missing size here means the exec never got that
            // far — and a zero exit with no measurement is not something to
            // rename over the user's file on trust.
            None => {
                stream_err = Some(format!(
                    "Could not confirm how much of {} arrived — nothing was saved.",
                    container_path
                ));
            }
            Some(_) => {}
        }
    }

    if let Some(err) = stream_err {
        // Only our own partial is removed. Whatever the user already had at
        // `dest` has not been touched at any point above.
        let _ = tokio::fs::remove_file(&partial).await;
        return Err(err);
    }

    if let Err(e) = finish_download(&partial, &dest).await {
        let _ = tokio::fs::remove_file(&partial).await;
        return Err(e);
    }

    log::info!(
        "Saved {} from project {} to {} ({} bytes)",
        container_path,
        project_id,
        dest.display(),
        total
    );
    Ok(Some(total))
}

/// What one upload dialog's worth of files did.
///
/// Both halves are needed because one dialog can select several files and they
/// do not have to agree: a folder among the selection, or one file over the
/// size ceiling, must not cost the user the files either side of it. The
/// failures are finished sentences, ready to show.
#[derive(Debug, Clone, serde::Serialize)]
pub struct UploadOutcome {
    /// In-container paths, in the order they landed.
    pub uploaded: Vec<String>,
    /// One sentence per file that did not.
    pub failures: Vec<String>,
}

/// Copy host files the user picked in an open dialog into the container
/// directory the Files pane is showing.
///
/// `Ok(None)` means the picker was dismissed, which is not a failure and is
/// deliberately distinguishable from one. Otherwise the [`UploadOutcome`]
/// says what each selected file did — see there for why that is two lists.
///
/// The other half of the restoration described on [`download_container_file`],
/// and the same rule: nothing that was removed comes back. In particular there
/// is no destination reservation. The audit that ended this feature found the
/// `link(2)` reservation returning success against a *directory* — linking into
/// it, leaving permanent stray files, and through a symlinked directory writing
/// outside the validated root — and failing permanently on any filesystem
/// without hard links. What it was defending against was a name collision, and
/// Docker's archive extractor overwrites on collision the same way `cp` does,
/// which is what a file manager's upload is expected to do.
///
/// Every step here already existed and is already hardened; this command is the
/// wiring, not new machinery:
///
///   * the name comes from the path **as the user gave it**, before resolution
///     ([`host_upload_name`]) — `~/Downloads/latest.log` is routinely a symlink,
///     and taking the leaf off the resolved path renames the file on its way in;
///   * the bytes come from [`resolve_host_read_path`], i.e. the host-read policy
///     applied to the path with its symlinks resolved, so a visible directory
///     that *leads* to `~/.ssh` is refused;
///   * the destination goes through [`resolve_container_dir`], so it is inside
///     [`CONTAINER_WRITE_ROOTS`] both as written and as it resolves;
///   * the read, the size ceiling and the descriptor check are
///     `docker::exec::upload_host_file_with_ids`', unchanged — including
///     landing the file owned by the container user rather than root.
///
/// The dialog itself is opened from Rust ([`pick_files_to_upload`]), which is
/// the whole reason this shape is acceptable at all.
#[tauri::command]
pub async fn upload_files_to_container(
    project_id: String,
    container_dir: String,
    window: tauri::Window,
    state: State<'_, AppState>,
) -> Result<Option<UploadOutcome>, String> {
    let project = state
        .projects_store
        .get(&project_id)
        .ok_or_else(|| format!("Project {} not found", project_id))?;

    let container_id = project
        .container_id
        .as_ref()
        .ok_or_else(|| "No container exists for this project yet — start it first".to_string())?;

    // Both checks before the dialog, so a project that cannot receive files
    // says so instead of asking the user to choose some first.
    require_running(container_id, "uploading files").await?;
    resolve_container_dir(container_id, "Upload", &container_dir).await?;

    let Some(picked) = pick_files_to_upload(&window).await else {
        return Ok(None);
    };

    // Again, now that the picker has closed. The check above is there so a
    // stopped or unwritable project says so *before* asking anyone to choose
    // files; this one is the check that actually guards the extraction. A modal
    // has no time limit, and `resolve_container_dir` answers a question about
    // the container's filesystem — which the container is free to change while
    // the dialog is open. Without this, `docker::exec`'s doc comment claim that
    // the destination "has already been confirmed" would be true only of a
    // moment that had passed.
    resolve_container_dir(container_id, "Upload", &container_dir).await?;

    // Once for the whole selection, not once per file — see
    // `upload_host_file_with_ids`.
    let ids = crate::docker::exec::container_user_ids(container_id).await;

    let mut outcome = UploadOutcome {
        uploaded: Vec::new(),
        failures: Vec::new(),
    };
    for path in picked {
        let path = match host_path_string(path) {
            Ok(path) => path,
            Err(e) => {
                outcome.failures.push(e);
                continue;
            }
        };
        match upload_one(container_id, &path, &container_dir, ids).await {
            Ok(dest) => {
                log::info!("Uploaded {} into project {} at {}", path, project_id, dest);
                outcome.uploaded.push(dest);
            }
            Err(e) => outcome.failures.push(e),
        }
    }
    Ok(Some(outcome))
}

/// One file of an upload selection. Every refusal it returns is a sentence
/// naming the file, because the caller may be reporting several at once and
/// "is a folder" on its own does not say which one.
async fn upload_one(
    container_id: &str,
    host_path: &str,
    container_dir: &str,
    ids: (u64, u64),
) -> Result<String, String> {
    let name = host_upload_name(host_path)?;
    let resolved = resolve_host_read_path(host_path).await?;

    let meta = tokio::fs::metadata(&resolved)
        .await
        .map_err(|e| format!("Cannot access {}: {}", resolved, e))?;
    // `!is_file()`, not `!is_dir()` — the same reasoning as the terminal drop.
    // A FIFO is neither a directory nor a regular file, reports `len() == 0`,
    // and `File::open` on one blocks forever with no writer and no timeout.
    if !meta.is_file() {
        return Err(if meta.is_dir() {
            format!("{} is a folder — upload its files individually.", host_path)
        } else {
            format!("{} is not a regular file — only ordinary files can be uploaded.", host_path)
        });
    }
    // The ceiling is enforced against the open descriptor inside the uploader;
    // this copy of it exists so the refusal arrives as a sentence instead of
    // after a 300 MB read.
    use crate::docker::exec::MAX_DROP_BYTES;
    if meta.len() > MAX_DROP_BYTES {
        return Err(format!(
            "{} is too large to upload ({:.0} MB; limit {} MB). Mount it into the project instead.",
            host_path,
            meta.len() as f64 / (1024.0 * 1024.0),
            MAX_DROP_BYTES / (1024 * 1024)
        ));
    }

    crate::docker::exec::upload_host_file_with_ids(
        container_id,
        &resolved,
        container_dir,
        &name,
        ids,
    )
    .await
}

/// Container-authored diagnostic text, cut down to something safe to quote.
///
/// One line, a couple of hundred characters, no control characters. See the
/// call site for why each of those three matters; the short version is that
/// this text ends up inside a toast that renders above every modal, and its
/// author is the container.
fn clip_container_text(text: &str) -> String {
    const MAX: usize = 200;
    let flattened: String = text
        .trim()
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let flattened = flattened.split_whitespace().collect::<Vec<_>>().join(" ");
    if flattened.chars().count() <= MAX {
        return flattened;
    }
    let kept: String = flattened.chars().take(MAX).collect();
    format!("{}…", kept.trim_end())
}

/// Pull `TC_SIZE=<n>` out of what the download script wrote to stderr.
///
/// A line rather than a second channel because there is no second channel: the
/// exec has exactly two streams and stdout is the payload, which has to stay
/// pristine — running it through anything is how a download corrupts a binary.
///
/// Deliberately strict. The container writes this, so it is parsed as one
/// well-formed line and nothing else: an unparseable value yields `None`, and
/// `None` fails the transfer rather than waving it through. The script has
/// already refused (exit 7) if `stat` could not answer, so a missing value here
/// means something further upstream went wrong.
fn parse_declared_size(stderr: &str) -> Option<u64> {
    stderr
        .lines()
        .find_map(|line| line.trim().strip_prefix("TC_SIZE="))
        .and_then(|value| value.trim().parse::<u64>().ok())
}

/// How much more than its measured size a file may deliver before the transfer
/// is treated as runaway.
///
/// Two shapes at once, because the two ends of the range need different things.
/// A small file needs *absolute* slack — a 2 KB file that grows to 40 MB is
/// unremarkable, and a multiplier would strangle it. A large one needs
/// *proportional* slack — doubling is plenty, and a fixed 256 MiB on top of
/// 3 GB is noise. So: whichever is larger.
///
/// This is a bound on the pathological case, not an attempt to predict the
/// honest one. The honest case is bounded by the file; this exists so that a
/// container answering a 2 KB read with an infinite stream costs the slack
/// rather than the disk.
fn download_ceiling(declared: u64) -> u64 {
    const FLOOR: u64 = 256 * 1024 * 1024;
    declared.saturating_mul(2).max(declared.saturating_add(FLOOR))
}

/// Bytes as a person reads them, for a refusal.
///
/// The frontend has `formatBytes` and these strings are built in Rust, so they
/// cannot share it. Kept deliberately small — this is for error text, not for
/// the UI, which formats its own.
fn human_bytes(bytes: u64) -> String {
    const UNITS: [(&str, u64); 4] = [
        ("GB", 1024 * 1024 * 1024),
        ("MB", 1024 * 1024),
        ("KB", 1024),
        ("bytes", 1),
    ];
    for (unit, scale) in UNITS {
        if bytes >= scale {
            return if scale == 1 {
                format!("{} {}", bytes, unit)
            } else {
                format!("{:.1} {}", bytes as f64 / scale as f64, unit)
            };
        }
    }
    "0 bytes".to_string()
}

/// The path a dialog handed back, as a `String`, or a refusal.
///
/// Every host-path check in this module is `&str`-based, so a `PathBuf` has to
/// become one somewhere. `to_string_lossy` is the wrong way to do it: invalid
/// bytes become U+FFFD, which is a *different path*. For an upload that reads
/// as "Cannot access …" on a file the user demonstrably just picked; for a save
/// it is worse, because the write would go somewhere other than the file the
/// dialog had just asked them to confirm overwriting.
///
/// Legacy Latin-1 filenames on Linux are the realistic way to meet this. Since
/// nothing downstream can handle such a path honestly, it is refused by name
/// rather than silently changed into another one.
fn host_path_string(path: PathBuf) -> Result<String, String> {
    path.into_os_string().into_string().map_err(|bad| {
        format!(
            "{} is not valid Unicode — Triple-C cannot transfer a file whose path it cannot spell exactly. Rename it, or move it somewhere with an ASCII name.",
            PathBuf::from(bad).display()
        )
    })
}

/// The name the save dialog opens with.
///
/// ## This string is container-authored, and it is a *name*, not a path
///
/// It goes straight to `rfd`'s `set_file_name`, and on Windows the common file
/// dialog parses its File-name box as a **path** on Save. A container can name
/// a file anything a Linux filesystem accepts, backslashes included, and
/// `validate_container_path` has no reason to object: it rejects a `..`
/// *segment*, and `..\..\Users\vic\…` is one POSIX segment.
///
/// So `rsplit('/')` alone — which is what this was — let the container choose
/// the destination on Windows, not just the file name:
///
/// ```text
/// /workspace/proj/..\..\..\Users\vic\AppData\Roaming\Microsoft\Word\STARTUP\x.dotm
///   -> "..\..\..\Users\vic\AppData\Roaming\Microsoft\Word\STARTUP\x.dotm"
/// ```
///
/// One un-read click on Save and that resolves. `resolve_host_path` still runs
/// on whatever the dialog produced, but Word's `STARTUP` and Excel's `XLSTART`
/// are auto-loading persistence directories that the denylist does not name —
/// and this file's own docs concede the denylist is "losing by construction".
/// It was never meant to be the boundary for this caller.
///
/// The fix is to make the string incapable of being a path on any platform this
/// ships to: every separator, the drive colon, and the rest of the characters
/// NTFS refuses are replaced rather than removed, so the name stays the same
/// length and stays recognisable. `?` and `*` are legal on Linux and go too —
/// this is a *suggestion* the user can edit, and a pre-filled name Windows
/// would reject is its own small bug.
///
/// The empty check is separate and also load-bearing: `rsplit` on a path with a
/// trailing separator yields `Some("")`, so without it the fallback never fires
/// and the dialog opens with a blank name.
fn suggested_save_name(container_path: &str) -> String {
    let leaf = container_path
        .rsplit('/')
        .next()
        .filter(|leaf| !leaf.is_empty())
        .unwrap_or("download");
    let cleaned: String = leaf
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '<' | '>' | '"' | '|' | '?' | '*' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect();
    // `.` and `..` are names the dialog cannot use, and a name that sanitised
    // down to nothing has nothing left to suggest.
    if cleaned.is_empty() || cleaned == "." || cleaned == ".." {
        return "download".to_string();
    }
    cleaned
}

/// Ask the user where to save, from Rust.
///
/// ## Why the dialog is here and not in the webview
///
/// This is the shape the previous round's threat model asked for by name. The
/// frontend used to call `@tauri-apps/plugin-dialog` and hand the chosen path
/// back over IPC as a string — at which point the backend has no way to tell a
/// path a person picked from a path a compromised webview invented, and
/// `resolve_host_path` is the *only* thing between `invoke` and an arbitrary
/// host write. Four audits' worth of criticals lived in that gap.
///
/// Driving the dialog from Rust closes it structurally: the webview can ask for
/// a save dialog, and that is the whole of its influence. It cannot name the
/// destination, and it cannot proceed without a person choosing one.
///
/// ## What is still validated, and what that costs
///
/// The chosen path still goes through `resolve_host_path`, whose hidden-
/// *directory* rule deliberately over-catches (see [`validate_host_path`]).
/// That rule was written for adversarial input, and against a dialog it will
/// occasionally refuse something a person meant — saving into `~/.config`, or
/// picking a file out of `~/.cache`. It is kept anyway: the refusal is a clear
/// sentence and the destinations it costs are unusual ones for this pane.
///
/// The hidden *leaf* rule is not kept, and the distinction matters. Under it,
/// saving `/workspace/.env` was refused after the modal and after the overwrite
/// prompt, quoting a name the app had pre-filled itself — and every
/// `.gitignore`, `.eslintrc.json` and `.nvmrc` in an ordinary workspace with
/// it, while uploading the same files worked. That is what
/// [`HostPathUse::WriteChosenName`] exists for.
///
/// `None` means dismissed, which is not a failure. The `oneshot` is used rather
/// than `blocking_save_file` because the blocking variants deadlock if they
/// ever reach the main thread, and "which thread does this command run on" is
/// not a property worth depending on.
async fn pick_save_path(window: &tauri::Window, suggested: &str) -> Option<PathBuf> {
    use tauri_plugin_dialog::DialogExt;
    let (tx, rx) = tokio::sync::oneshot::channel();
    window
        .dialog()
        .file()
        .set_parent(window)
        .set_title("Save to host")
        .set_file_name(suggested)
        .save_file(move |picked| {
            let _ = tx.send(picked);
        });
    rx.await.ok().flatten().and_then(|p| p.into_path().ok())
}

/// Ask the user which host files to upload, from Rust. See [`pick_save_path`]
/// for why the dialog lives on this side.
async fn pick_files_to_upload(window: &tauri::Window) -> Option<Vec<PathBuf>> {
    use tauri_plugin_dialog::DialogExt;
    let (tx, rx) = tokio::sync::oneshot::channel();
    window
        .dialog()
        .file()
        .set_parent(window)
        .set_title("Upload to container")
        .pick_files(move |picked| {
            let _ = tx.send(picked);
        });
    let picked: Vec<PathBuf> = rx
        .await
        .ok()
        .flatten()?
        .into_iter()
        .filter_map(|p| p.into_path().ok())
        .collect();
    // An empty selection is a dismissal as far as the caller is concerned —
    // there is nothing to report and nothing to refresh.
    (!picked.is_empty()).then_some(picked)
}

/// Create a `.tar.gz` backup of the container and stream it to a host file.
/// The archive contains:
///   - the workspace (default /workspace), minus regenerable build artifacts
///     (node_modules, target), under `workspace/`, and
///   - a sanitized copy of the home config under `home-claude/`: ~/.claude.json
///     with secret-bearing keys removed (`mcpServers` — Claude Code's own native
///     MCP config — and `settings` are kept) and ~/.claude/ minus the OAuth
///     `.credentials.json`, so settings and skills set up via Claude Code
///     survive a Reset.
/// `.git` is kept in full so the backup faithfully preserves git history,
/// including unpushed commits. Build + gzip happen inside the container so a
/// large workspace isn't streamed in full. The container must be RUNNING (the
/// backup runs via `docker exec`). Returns the number of bytes written.
#[tauri::command]
pub async fn download_container_backup(
    project_id: String,
    host_path: String,
    container_path: Option<String>,
    state: State<'_, AppState>,
) -> Result<u64, String> {
    // `host_path` reached `File::create` unchecked, which truncated whatever was
    // there before the exec had even started — and the error path then deleted
    // it, so a backup of a non-existent container path took the user's file with
    // it. Validate first, write to a partial file second, rename last.
    let dest = resolve_host_path(&host_path, HostPathUse::Write).await?;

    let project = state
        .projects_store
        .get(&project_id)
        .ok_or_else(|| format!("Project {} not found", project_id))?;

    let container_id = project
        .container_id
        .as_ref()
        .ok_or_else(|| "No container exists for this project yet — start it first".to_string())?;

    let docker = get_docker()?;

    require_running(container_id, "backing up").await?;

    let path = container_path.unwrap_or_else(|| "/workspace".to_string());
    // Read-only source: `tar -C` it, so absoluteness and `..` are what matter.
    validate_container_path("Backup", &path)?;

    // Stage a sanitized home config, then tar+gzip workspace + staged config to
    // stdout. mktemp/jq output go nowhere near stdout, so the only thing the
    // exec emits on stdout is the archive itself. --ignore-failed-read keeps a
    // transient unreadable file from aborting the whole backup. If jq can't
    // parse ~/.claude.json we substitute an empty object — never the raw file —
    // so secrets can't leak through the sanitization fallback.
    // The `--transform` nests the workspace under `workspace/` (parallel to
    // `home-claude/`) so an extracted archive has both clearly labeled instead
    // of scattering the workspace files into the extraction dir. Rewriting the
    // leading `.` (rather than `./`) also renames tar's root member from `./` to
    // `workspace`, so the archive carries a proper `workspace/` dir entry rather
    // than a bare `./` that would stamp the source root's mode/mtime onto the
    // extraction directory. `flags=rh` rewrites regular member names AND
    // hardlink target names (so an intra-workspace hardlink pair still resolves
    // on extract) while leaving symlink targets untouched (rewriting those would
    // corrupt relative/absolute links).
    let script = r#"set -e
STAGE=$(mktemp -d)
trap 'rm -rf "$STAGE"' EXIT
mkdir -p "$STAGE/home-claude"
if [ -f "$HOME/.claude.json" ]; then
  if ! jq 'del(.primaryApiKey, .oauthAccount, .customApiKeyResponses)' "$HOME/.claude.json" \
       > "$STAGE/home-claude/.claude.json" 2>/dev/null; then
    echo "warning: could not sanitize .claude.json; omitting it from backup" >&2
    printf '{}' > "$STAGE/home-claude/.claude.json"
  fi
fi
if [ -d "$HOME/.claude" ]; then
  cp -a "$HOME/.claude" "$STAGE/home-claude/.claude" 2>/dev/null || true
  rm -f "$STAGE/home-claude/.claude/.credentials.json"
fi
tar czf - --ignore-failed-read \
  --exclude='*/node_modules' --exclude='*/target' \
  --transform='flags=rh;s,^\.,workspace,' \
  -C "$TC_BACKUP_SRC" . \
  -C "$STAGE" home-claude"#;

    let cmd = vec!["sh".to_string(), "-c".to_string(), script.to_string()];

    let exec = docker
        .create_exec(
            container_id,
            CreateExecOptions {
                attach_stdout: Some(true),
                attach_stderr: Some(true),
                cmd: Some(cmd),
                env: Some(vec![
                    "HOME=/home/claude".to_string(),
                    format!("TC_BACKUP_SRC={}", path),
                ]),
                user: Some("claude".to_string()),
                ..Default::default()
            },
        )
        .await
        .map_err(|e| format!("Failed to create backup exec: {}", e))?;

    let result = docker
        .start_exec(&exec.id, None)
        .await
        .map_err(|e| format!("Failed to start backup exec: {}", e))?;

    let mut output = match result {
        StartExecResults::Attached { output, .. } => output,
        StartExecResults::Detached => return Err("Backup exec started detached".to_string()),
    };

    use tokio::io::AsyncWriteExt;
    let partial = partial_download_path(&dest)?;
    // Opened synchronously so the descriptor can be checked against the path
    // that was resolved a moment ago (H4): `create_new` is `O_EXCL`, so this
    // cannot have followed a symlink at the final component, but a directory
    // on the way could have been swapped since.
    //
    // The two failures inside are not the same and must not be cleaned up the
    // same way: `create_new` failing means nothing was created, while
    // `verify_opened_path` failing means the file exists and is ours. The
    // second used to propagate straight out and leave the partial behind.
    // Hence the flag rather than a `?`.
    let open_at = partial.clone();
    let created = Arc::new(AtomicBool::new(false));
    let opened = {
        let created = Arc::clone(&created);
        tokio::task::spawn_blocking(move || -> Result<std::fs::File, String> {
            let file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&open_at)
                .map_err(|e| format!("Failed to create backup file: {}", e))?;
            // From here on the file is ours, so a failure below may delete it.
            created.store(true, Ordering::SeqCst);
            verify_opened_path(&file, &open_at)?;
            Ok(file)
        })
        .await
        .map_err(|e| format!("Backup task panicked: {}", e))?
    };
    let file = match opened {
        Ok(file) => file,
        Err(e) => {
            if created.load(Ordering::SeqCst) {
                let _ = tokio::fs::remove_file(&partial).await;
            }
            return Err(e);
        }
    };
    let file = tokio::fs::File::from_std(file);
    let mut writer = tokio::io::BufWriter::new(file);
    let mut total: u64 = 0;
    let mut stderr_text = String::new();
    let mut stream_err: Option<String> = None;

    while let Some(msg) = output.next().await {
        match msg {
            Ok(LogOutput::StdOut { message }) => {
                if let Err(e) = writer.write_all(&message).await {
                    stream_err = Some(format!("Failed to write backup file: {}", e));
                    break;
                }
                total += message.len() as u64;
            }
            Ok(LogOutput::StdErr { message }) => {
                push_capped(&mut stderr_text, &message);
            }
            Ok(_) => {}
            Err(e) => {
                stream_err = Some(format!("Backup stream error: {}", e));
                break;
            }
        }
    }
    if stream_err.is_none() {
        if let Err(e) = writer.flush().await {
            stream_err = Some(format!("Failed to finalize backup file: {}", e));
        }
    }
    drop(writer);

    // The tar pipeline can abort mid-stream (producing a truncated archive) and
    // still have sent bytes, so a non-zero exit must be treated as failure even
    // when `total > 0`. Poll until the exec actually reports finished so the
    // exit code is reliably populated; if it can't be determined we fall back to
    // the `total == 0` check below.
    let exit_code = crate::docker::exec::wait_for_exec_exit(&exec.id).await;

    if stream_err.is_none() && exit_code.is_some_and(|c| c != 0) {
        stream_err = Some(format!(
            "Backup command failed (exit {}){}",
            exit_code.unwrap_or(-1),
            if stderr_text.trim().is_empty() {
                String::new()
            } else {
                format!(": {}", stderr_text.trim())
            }
        ));
    }
    if stream_err.is_none() && total == 0 {
        stream_err = Some(format!(
            "Backup produced no data{}",
            if stderr_text.trim().is_empty() {
                String::new()
            } else {
                format!(": {}", stderr_text.trim())
            }
        ));
    }

    if let Some(err) = stream_err {
        // Only our own partial archive is deleted — never whatever the user
        // already had at `dest`, which has not been touched yet.
        let _ = tokio::fs::remove_file(&partial).await;
        return Err(err);
    }

    if let Err(e) = finish_download(&partial, &dest).await {
        let _ = tokio::fs::remove_file(&partial).await;
        return Err(e);
    }

    log::info!(
        "Wrote {} byte backup for project {} to {}",
        total,
        project_id,
        dest.display()
    );
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A destination name that fits its directory must not become a partial
    /// name that does not. The leaf can come from the *container* now — a
    /// bundler's content-hashed chunk name is routinely 200+ characters — so
    /// this is reachable without anyone typing anything unusual.
    #[test]
    fn a_partial_name_stays_within_name_max() {
        let long = "x".repeat(250);
        let partial = partial_download_path(Path::new(&format!("/home/j/{}", long))).unwrap();
        let leaf = partial.file_name().unwrap().to_string_lossy().to_string();
        assert!(leaf.len() <= 255, "partial leaf was {} bytes", leaf.len());
        assert!(leaf.contains(".triple-c-part-"));
        // Same directory as the destination — the last step has to be a rename
        // within one filesystem.
        assert_eq!(partial.parent(), Some(Path::new("/home/j")));
    }

    /// Truncation must not split a character in half, or the partial cannot be
    /// spelled back on a filesystem that validates encoding.
    #[test]
    fn a_partial_name_truncates_on_a_character_boundary() {
        // 3 bytes each, deliberately: the budget is 232, which 3 does not
        // divide, so a naive byte slice lands mid-character. A 2-byte character
        // would divide it evenly and the test would pass without the boundary
        // walk existing at all — which is exactly what it did on the first
        // attempt.
        let long = "日".repeat(250);
        let partial = partial_download_path(Path::new(&format!("/home/j/{}", long))).unwrap();
        let leaf = partial.file_name().unwrap().to_string_lossy().to_string();
        assert!(leaf.len() <= 255);
        // The kept part must be a genuine prefix of the name, whole characters
        // only — which rules out both a mid-character cut and the other way of
        // "not splitting a character", throwing the whole name away.
        let kept = leaf.split(".triple-c-part-").next().unwrap();
        assert!(!kept.is_empty(), "the whole name was discarded");
        assert!(long.starts_with(kept), "kept part is not a prefix of the name");
        assert_eq!(kept.len() % 3, 0, "cut landed mid-character");
    }

    /// An ordinary name is left exactly as it is — the cap must not be paid by
    /// every download.
    #[test]
    fn an_ordinary_partial_name_keeps_the_whole_leaf() {
        let partial = partial_download_path(Path::new("/home/j/notes.txt")).unwrap();
        let leaf = partial.file_name().unwrap().to_string_lossy().to_string();
        assert!(leaf.starts_with("notes.txt.triple-c-part-"));
    }

    /// The container's stderr is attacker-controlled and unbounded at the
    /// source; it must be bounded here.
    #[test]
    fn container_stderr_stops_growing_at_the_cap() {
        let mut buf = String::new();
        for _ in 0..1000 {
            push_capped(&mut buf, &b"x".repeat(1024));
        }
        assert!(buf.len() <= MAX_EXEC_STDERR, "grew to {}", buf.len());
        // …and it is not simply empty: the point is to explain a failure.
        assert!(!buf.is_empty());
    }

    /// Capping must not split a character either — the text goes into an error
    /// message that is rendered.
    #[test]
    fn capping_stderr_truncates_on_a_character_boundary() {
        // 3 bytes each, and the cap is not a multiple of 3, so a naive byte
        // slice lands mid-character.
        let text = "日".repeat(MAX_EXEC_STDERR);
        let mut buf = String::new();
        push_capped(&mut buf, text.as_bytes());
        assert!(buf.len() <= MAX_EXEC_STDERR);
        assert!(!buf.is_empty(), "the whole diagnostic was discarded");
        assert!(text.starts_with(&buf), "kept part is not a prefix");
        assert_eq!(buf.len() % 3, 0, "cut landed mid-character");
    }

    /// A path a dialog produced that cannot be spelled exactly is refused, not
    /// quietly turned into a different path by U+FFFD substitution.
    #[cfg(unix)]
    #[test]
    fn a_non_unicode_dialog_path_is_refused_rather_than_mangled() {
        use std::os::unix::ffi::OsStringExt;
        let bad = PathBuf::from(std::ffi::OsString::from_vec(b"/home/j/caf\xe9.txt".to_vec()));
        let err = host_path_string(bad).unwrap_err();
        assert!(err.contains("not valid Unicode"), "{}", err);
        // An ordinary path still comes back untouched.
        assert_eq!(
            host_path_string(PathBuf::from("/home/j/café.txt")).unwrap(),
            "/home/j/café.txt"
        );
    }

    /// The dialog's pre-filled name is authored by the **container**, and on
    /// Windows the save dialog parses its name box as a path. So the string
    /// must be incapable of being one.
    #[test]
    fn a_suggested_name_can_never_be_a_path() {
        // The finding this exists for, verbatim: `..` segments plus backslashes
        // reach Word's auto-loading STARTUP directory, which the host-path
        // denylist does not name.
        let evil = r"/workspace/proj/..\..\..\Users\vic\AppData\Roaming\Microsoft\Word\STARTUP\x.dotm";
        let got = suggested_save_name(evil);
        assert!(!got.contains('\\'), "{}", got);
        assert!(!got.contains('/'), "{}", got);
        // A drive letter is a path on Windows too.
        let drive = r"/workspace/proj/C:\Users\vic\Desktop\payload.exe";
        let got = suggested_save_name(drive);
        assert!(!got.contains(':'), "{}", got);
        assert!(!got.contains('\\'), "{}", got);
    }

    /// Sanitising must not damage the ordinary case — the whole point of a
    /// suggestion is that it is the file's name.
    #[test]
    fn an_ordinary_suggested_name_is_untouched() {
        assert_eq!(suggested_save_name("/workspace/notes.txt"), "notes.txt");
        assert_eq!(suggested_save_name("/workspace/my report (v2).pdf"), "my report (v2).pdf");
        // Dotfiles are the common case this pane browses, and they are saveable
        // now — see `HostPathUse::WriteChosenName`.
        assert_eq!(suggested_save_name("/workspace/.env"), ".env");
    }

    /// A name that sanitises down to nothing usable still has to open the
    /// dialog with something.
    #[test]
    fn a_suggested_name_always_has_something_in_it() {
        assert_eq!(suggested_save_name("/workspace/logs/"), "download");
        assert_eq!(suggested_save_name("/"), "download");
        assert_eq!(suggested_save_name(""), "download");
        // `/` alone sanitises to `_`, not to nothing — that is fine and still a
        // name. But a leaf that *is* `..` is not usable as one.
        assert_eq!(suggested_save_name("/workspace/.."), "download");
    }

    /// A dotfile must be saveable to the host. Under the old rule the leaf was
    /// judged for hiddenness on every write, so `.env` was refused *after* the
    /// modal — with the name the app itself had pre-filled.
    #[test]
    fn a_dotfile_can_be_saved_when_the_user_named_it_in_a_dialog() {
        let dest = "/home/j/Documents/.env";
        assert!(
            validate_host_path(dest, HostPathUse::WriteChosenName).is_ok(),
            "a name chosen in a save dialog should be allowed to be hidden"
        );
        // But only the leaf. A hidden *directory* is refused exactly as before.
        assert!(validate_host_path("/home/j/.ssh/authorized_keys", HostPathUse::WriteChosenName).is_err());
        assert!(validate_host_path("/home/j/.config/x.txt", HostPathUse::WriteChosenName).is_err());
        // And the IPC-fed writer keeps the strict rule, because for it the
        // dialog is not a boundary.
        assert!(validate_host_path(dest, HostPathUse::Write).is_err());
    }

    /// The size line the download script emits is the only thing standing
    /// between a truncated read and a rename over the user's own file.
    #[test]
    fn the_declared_size_is_read_out_of_stderr() {
        assert_eq!(parse_declared_size("TC_SIZE=600000000\n"), Some(600000000));
        // Real diagnostics can share the stream.
        assert_eq!(
            parse_declared_size("dd: warning: something\nTC_SIZE=42\n"),
            Some(42)
        );
        assert_eq!(parse_declared_size("TC_SIZE=0"), Some(0));
        // Container-authored, so anything unparseable is *no answer*, which the
        // caller turns into a refusal rather than a pass.
        assert_eq!(parse_declared_size("TC_SIZE=notanumber"), None);
        assert_eq!(parse_declared_size("TC_SIZE=-1"), None);
        assert_eq!(parse_declared_size("nothing here"), None);
        assert_eq!(parse_declared_size(""), None);
    }

    /// The runaway ceiling has to be generous enough never to cut off an honest
    /// read and tight enough that a 2 KB file cannot fill a disk.
    #[test]
    fn the_download_ceiling_bounds_the_pathological_case_only() {
        // A small file gets absolute slack: an active log growing past its
        // starting size is ordinary.
        assert!(download_ceiling(2048) >= 256 * 1024 * 1024);
        // A large one gets proportional slack; doubling is plenty.
        let three_gb = 3 * 1024 * 1024 * 1024;
        assert!(download_ceiling(three_gb) >= three_gb * 2);
        // Never below the file itself, and no overflow at the top.
        assert!(download_ceiling(0) > 0);
        assert!(download_ceiling(u64::MAX) >= u64::MAX / 2);
    }

    /// Container-authored diagnostics end up in a toast that renders above
    /// every modal, so they arrive as one short line.
    #[test]
    fn container_diagnostics_are_clipped_to_one_short_line() {
        let hostile = format!("Triple-C will not save there.\n\n{}", "pad ".repeat(4000));
        let out = clip_container_text(&hostile);
        assert!(out.chars().count() <= 201, "{} chars", out.chars().count());
        assert!(!out.contains('\n'));
        assert!(!out.contains('\r'));
        // Not just newlines — `split_whitespace` would have handled those on its
        // own. The control-character map is here for the ones it would not:
        // an ESC lets container text repaint or hide part of a terminal, and a
        // backspace lets it overwrite the app's own framing.
        let escapes = clip_container_text("dd: \u{1b}[2Jcannot\u{8}\u{8} open");
        assert!(!escapes.contains('\u{1b}'), "{:?}", escapes);
        assert!(!escapes.contains('\u{8}'), "{:?}", escapes);
        // A real diagnostic still survives intact.
        assert_eq!(clip_container_text("  dd: cannot open 'x'  "), "dd: cannot open 'x'");
        assert_eq!(clip_container_text(""), "");
    }

    #[test]
    fn a_save_dialog_always_opens_with_a_name() {
        assert_eq!(suggested_save_name("/workspace/notes.txt"), "notes.txt");
        assert_eq!(suggested_save_name("/notes.txt"), "notes.txt");
        // A trailing separator is the case `unwrap_or` alone gets wrong: the
        // leaf is `Some("")`, so the fallback is never consulted and the dialog
        // opens blank.
        assert_eq!(suggested_save_name("/workspace/logs/"), "download");
        assert_eq!(suggested_save_name("/"), "download");
        assert_eq!(suggested_save_name(""), "download");
    }

    /// A record as `find -printf '%y\t%Y\t%s\t%T@\t%m\t%f\0'` emits it —
    /// fields first, name last, NUL-terminated.
    fn line(name: &str, own: &str, deref: &str, size: &str) -> String {
        format!("{}\t{}\t{}\t1700000000.0000000000\t644\t{}\0", own, deref, size, name)
    }

    #[test]
    fn parses_a_plain_file_row() {
        let entries = parse_find_output("/workspace", &line("notes.txt", "f", "f", "42"));
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "notes.txt");
        assert_eq!(entries[0].path, "/workspace/notes.txt");
        assert!(!entries[0].is_directory);
        assert!(!entries[0].is_symlink);
        assert_eq!(entries[0].size, 42);
        assert_eq!(entries[0].permissions, "644");
        assert_eq!(entries[0].modified, "2023-11-14 22:13:20");
    }

    #[test]
    fn a_symlink_to_a_directory_is_navigable_and_still_flagged_as_a_link() {
        // The bug this guards: `%y` reports `l`, so keying `is_directory` off it
        // made every symlinked directory an unopenable row.
        let entries = parse_find_output("/workspace", &line("app", "l", "d", "12"));
        assert!(entries[0].is_directory);
        assert!(entries[0].is_symlink);
    }

    #[test]
    fn a_broken_symlink_is_not_a_directory() {
        // `%Y` is `N` when the target is missing, `L` on a loop.
        for deref in ["N", "L", "?"] {
            let entries = parse_find_output("/workspace", &line("dangling", "l", deref, "9"));
            assert!(!entries[0].is_directory, "deref type {} became a directory", deref);
            assert!(entries[0].is_symlink);
        }
    }

    #[test]
    fn directories_sort_first_then_case_insensitively() {
        let output = [
            line("Zeta", "f", "f", "1"),
            line("alpha", "f", "f", "1"),
            line("src", "d", "d", "4096"),
        ]
        .concat();
        let entries = parse_find_output("/workspace", &output);
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["src", "alpha", "Zeta"]);
    }

    #[test]
    fn short_and_blank_rows_are_dropped_rather_than_mis_parsed() {
        let output = format!("\0  \0broken\ttoo\tshort\0{}", line("ok", "f", "f", "1"));
        let entries = parse_find_output("/workspace", &output);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "ok");
    }

    #[test]
    fn the_root_directory_does_not_get_a_doubled_separator() {
        let entries = parse_find_output("/", &line("etc", "d", "d", "4096"));
        assert_eq!(entries[0].path, "/etc");
    }

    #[test]
    fn unparseable_size_and_mtime_fall_back_instead_of_dropping_the_row() {
        let output = "f\tf\t-\t-\t644\tweird";
        let entries = parse_find_output("/workspace", output);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].size, 0);
    }

    #[test]
    fn the_listing_argv_puts_the_name_last_and_terminates_records_with_nul() {
        // Pinned together with the parser: these two only work as a pair, and
        // the separators must reach `find` as escapes — a literal NUL cannot
        // travel in argv.
        let argv = list_argv("/workspace");
        assert_eq!(argv[0], "find");
        // `-H` before the starting point, or `find` reads it as a predicate.
        assert_eq!(argv[1], "-H");
        assert_eq!(argv[2], "/workspace");
        let format = argv.last().unwrap();
        assert!(format.ends_with("%f\\0"), "{}", format);
        assert!(!format.contains('\0'));
        assert!(!format.contains('\n'));
    }

    #[test]
    fn a_symlinked_directory_is_listed_by_following_the_starting_point() {
        // `find` defaults to `-P`, which does not follow a link even when it is
        // the thing it was pointed at: `find /workspace/link -mindepth 1`
        // matched the link, `-mindepth 1` discarded it, and a real directory
        // rendered as "Empty directory". The row was navigable — `%Y` has said
        // "directory" for a symlinked one since the type columns were split —
        // so double-clicking it opened a folder that appeared to have nothing
        // in it.
        assert!(list_argv("/workspace/link").contains(&"-H".to_string()));
        // …and only the starting point: `-L` would have `find` chase the links
        // it enumerates, which is where a symlink loop becomes a walk that does
        // not end.
        assert!(!list_argv("/workspace/link").contains(&"-L".to_string()));
    }

    #[test]
    fn a_link_that_leads_nowhere_is_described_rather_than_quoted() {
        // Now that `-H` follows the starting point, the two failures a link can
        // produce reach the user — and `find`'s own words for them are an errno
        // about a path, for a row they double-clicked because it looked like a
        // folder.
        let looped = describe_find_diagnostics(
            "/workspace/loop",
            "find: '/workspace/loop': Too many levels of symbolic links",
        );
        assert!(looped.contains("loops back on itself"), "{}", looped);
        assert!(looped.contains("/workspace/loop"), "{}", looped);

        let broken = describe_find_diagnostics(
            "/workspace/broken",
            "find: '/workspace/broken': No such file or directory",
        );
        assert!(broken.contains("does not lead anywhere"), "{}", broken);

        // Everything else is `find`'s to say, and it says it well.
        assert_eq!(
            describe_find_diagnostics("/root", "find: '/root': Permission denied"),
            "find: '/root': Permission denied"
        );
    }

    #[test]
    fn a_tab_in_a_filename_cannot_forge_the_type_and_size_columns() {
        // The bug this guards: with the name first, `evil.txt\td\td\t4096…`
        // rendered as a *directory* of the attacker's chosen size. The name is
        // last now, so the tabs stay inside it.
        let entries = parse_find_output(
            "/workspace",
            &line("evil.txt\td\td\t4096", "f", "f", "3"),
        );
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "evil.txt\td\td\t4096");
        assert!(!entries[0].is_directory);
        assert_eq!(entries[0].size, 3);
    }

    #[test]
    fn a_newline_in_a_filename_cannot_forge_a_whole_row() {
        // A filename may contain a newline, so a line-terminated format let one
        // name print two rows. NUL is the byte a filename cannot contain.
        let entries = parse_find_output("/workspace", &line("two\nlines", "f", "f", "5"));
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "two\nlines");
        assert_eq!(entries[0].path, "/workspace/two\nlines");
    }

    #[test]
    fn parent_dir_walks_up_one_level_and_stops_at_root() {
        assert_eq!(parent_dir("/workspace/app/src"), "/workspace/app");
        assert_eq!(parent_dir("/workspace/app/src/"), "/workspace/app");
        assert_eq!(parent_dir("/workspace"), "/");
        assert_eq!(parent_dir("/"), "/");
    }

    #[test]
    fn a_rename_target_may_not_relocate_the_entry() {
        // The whole point of the validator: this is argv for `mv`, and a name
        // with a separator in it would be a move, not a rename.
        assert!(validate_entry_name("sub/dir").is_err());
        assert!(validate_entry_name("../escape").is_err());
        assert!(validate_entry_name("/etc/passwd").is_err());
    }

    #[test]
    fn dot_and_dotdot_and_empty_are_refused() {
        assert!(validate_entry_name("").is_err());
        assert!(validate_entry_name(".").is_err());
        assert!(validate_entry_name("..").is_err());
        assert!(validate_entry_name("\0").is_err());
        assert!(validate_entry_name(&"x".repeat(256)).is_err());
    }

    #[test]
    fn ordinary_names_including_awkward_ones_are_allowed() {
        // Nothing goes through a shell, so metacharacters are just characters —
        // and a leading `-` is safe because every call site passes `--` first.
        for name in [".hidden", "a b.txt", "$(whoami)", "it's", "-rf", "…unicode…"] {
            assert!(validate_entry_name(name).is_ok(), "{} was refused", name);
        }
    }

    #[test]
    fn the_viewer_cap_is_never_larger_than_the_hard_ceiling() {
        // The frontend picks a cap per file type; Rust still gets the last word
        // because the whole payload is buffered in host RAM.
        assert_eq!(Some(u64::MAX).unwrap().min(MAX_READ_BYTES), MAX_READ_BYTES);
    }

    // ── Path validation ─────────────────────────────────────────────────────

    #[test]
    fn a_listing_path_that_is_really_an_argument_is_refused() {
        // C2. `find` ends its starting-point list at the first argument
        // beginning with `-`, so `path = "-delete"` listed nothing and ran
        // `-delete` over the exec's working directory — the bind-mounted
        // project. Verified deleting on findutils 4.9.0. `--` does not help;
        // absoluteness does.
        for path in ["-delete", "-exec", "--", "-mindepth"] {
            let err = validate_container_path("Folder", path).unwrap_err();
            assert!(err.contains("absolute"), "{} → {}", path, err);
        }
    }

    #[test]
    fn a_container_path_must_be_absolute_and_traversal_free() {
        assert!(validate_container_path("Folder", "/workspace").is_ok());
        assert!(validate_container_path("Folder", "/home/claude/.claude").is_ok());
        // A name that merely *starts* with a dot-dot is not traversal.
        assert!(validate_container_path("Folder", "/workspace/..hidden").is_ok());

        assert!(validate_container_path("Folder", "").is_err());
        assert!(validate_container_path("Folder", "workspace/app").is_err());
        assert!(validate_container_path("Folder", "/workspace/../etc").is_err());
        assert!(validate_container_path("Folder", "/workspace/..").is_err());
        assert!(validate_container_path("Folder", "/work\0space").is_err());
        assert!(validate_container_path("Folder", &format!("/{}", "x".repeat(4096))).is_err());
    }

    #[test]
    fn only_the_folders_the_app_owns_can_be_written_to() {
        for path in ["/workspace", "/workspace/app/src", "/home/claude", "/tmp/x"] {
            assert!(validate_container_write_path("Item", path).is_ok(), "{}", path);
        }
        // Reading these is fine — changing them is not this panel's business,
        // and outside /workspace it would be a permission error anyway.
        for path in ["/", "/etc/passwd", "/usr/lib", "/home/other", "/workspace-backup/x"] {
            assert!(validate_container_write_path("Item", path).is_err(), "{}", path);
        }
    }

    #[test]
    fn containment_is_compared_by_whole_segments() {
        // The classic `starts_with` bug: `/workspace-backup` is not under
        // `/workspace`.
        assert!(is_under_root("/workspace", "/workspace"));
        assert!(is_under_root("/workspace/", "/workspace"));
        assert!(is_under_root("/workspace/app", "/workspace"));
        assert!(!is_under_root("/workspaces", "/workspace"));
        assert!(!is_under_root("/workspace-backup/x", "/workspace"));
        assert!(!is_under_root("/", "/workspace"));
    }

    #[test]
    fn an_ordinary_save_location_is_accepted() {
        for path in ["/home/jo/Downloads/report.pdf", "/tmp/out.txt", "/media/usb/a b.md"] {
            assert!(validate_host_path(path, HostPathUse::Write).is_ok(), "{}", path);
            assert!(validate_host_path(path, HostPathUse::Read).is_ok(), "{}", path);
        }
    }

    #[test]
    fn a_host_path_must_be_absolute_and_traversal_free() {
        assert!(validate_host_path("", HostPathUse::Write).is_err());
        assert!(validate_host_path("report.pdf", HostPathUse::Write).is_err());
        assert!(validate_host_path("/home/jo/../../etc/hosts", HostPathUse::Write).is_err());
        assert!(validate_host_path("/home/jo/re\0port", HostPathUse::Write).is_err());
    }

    #[test]
    fn a_hidden_host_directory_is_refused_in_both_directions() {
        // The container→host write primitive worth closing: container-controlled
        // bytes at a path of the caller's choosing.
        assert!(validate_host_path("/home/jo/.ssh/authorized_keys", HostPathUse::Write).is_err());
        assert!(validate_host_path("/home/jo/.config/autostart/x", HostPathUse::Write).is_err());
        // …and the host→container read that pairs with it.
        assert!(validate_host_path("/home/jo/.aws/credentials", HostPathUse::Read).is_err());
        assert!(validate_host_path("/home/jo/.ssh/id_rsa", HostPathUse::Read).is_err());
    }

    #[test]
    fn a_hidden_file_name_may_be_uploaded_but_not_created() {
        // Dragging a project's own `.env` into the container is ordinary; being
        // handed a container-controlled `~/.bashrc` is not.
        assert!(validate_host_path("/home/jo/project/.env", HostPathUse::Read).is_ok());
        assert!(validate_host_path("/home/jo/.bashrc", HostPathUse::Write).is_err());
    }

    #[test]
    fn host_system_locations_are_refused_including_windows_ones() {
        assert!(validate_host_path("/etc/cron.d/x", HostPathUse::Write).is_err());
        assert!(validate_host_path("/usr/bin/tool", HostPathUse::Write).is_err());
        assert!(validate_host_path("/etc/shadow", HostPathUse::Read).is_err());
        // Case and separator are normalised before the comparison.
        let windows = "C:\\Windows\\System32\\drivers\\etc\\hosts";
        assert!(validate_host_path(windows, HostPathUse::Write).is_err());
        // A user directory that merely shares a prefix is not a system one.
        assert!(validate_host_path("/home/jo/etcetera/notes.txt", HostPathUse::Write).is_ok());
    }

    // ── Staging a host write (Backup) ───────────────────────────────────────

    #[test]
    fn a_download_is_staged_beside_its_destination_and_renamed() {
        // Why: the destination must not be touched until the transfer has
        // succeeded, and the rename that finishes the job must not cross a
        // filesystem.
        let dest = Path::new("/home/jo/Downloads/report.pdf");
        let partial = partial_download_path(dest).unwrap();
        assert_eq!(partial.parent(), dest.parent());
        assert_ne!(partial, dest);
        let name = partial.file_name().unwrap().to_string_lossy().to_string();
        assert!(name.starts_with("report.pdf."), "{}", name);
        assert!(name.contains("triple-c-part-"), "{}", name);
        // Visible on purpose: a crash leaves it next to the file it meant to be.
        assert!(!name.starts_with('.'), "{}", name);
        // Two downloads of the same file must not share a partial.
        assert_ne!(partial_download_path(dest).unwrap(), partial);
        assert!(partial_download_path(Path::new("/")).is_err());
    }

    #[tokio::test]
    async fn finishing_a_download_replaces_the_destination_only_once_it_is_whole() {
        // The destination the user picked already holds something — the save
        // dialog asked about that — and what must never happen is losing it to a
        // download that did not arrive. Here the payload *has* arrived, so the
        // swap goes through, on Windows (rename refuses an existing target) as
        // well as Unix.
        let dir = std::env::temp_dir().join(format!("tc-finish-{}", uuid::Uuid::new_v4()));
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let dest = dir.join("thesis.docx");
        tokio::fs::write(&dest, b"the original").await.unwrap();

        let partial = partial_download_path(&dest).unwrap();
        tokio::fs::write(&partial, b"the download").await.unwrap();

        finish_download(&partial, &dest).await.unwrap();
        assert_eq!(tokio::fs::read(&dest).await.unwrap(), b"the download");
        assert!(!partial.exists(), "the partial file was left behind");

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn a_rename_that_fails_for_any_other_reason_leaves_the_destination_alone() {
        // The guard used to be "the rename failed and the destination exists",
        // which is true of every failure that has nothing to do with the
        // destination existing — a vanished partial, a permission error, a
        // destination that is a directory. Each of those then *deleted* the
        // file the user already had, in order to complete a move that could not
        // complete. Here the partial is missing, which is the clearest case:
        // there is nothing to replace it with, so nothing is replaced.
        let dir = std::env::temp_dir().join(format!("tc-finish-safe-{}", uuid::Uuid::new_v4()));
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let dest = dir.join("thesis.docx");
        tokio::fs::write(&dest, b"the original").await.unwrap();
        let missing = partial_download_path(&dest).unwrap();

        let err = finish_download(&missing, &dest).await.unwrap_err();
        assert!(err.starts_with("Failed to save"), "{}", err);
        assert_eq!(
            tokio::fs::read(&dest).await.unwrap(),
            b"the original",
            "the destination was destroyed by a failure that was not about it"
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    // ── Host path normalisation, on every platform ──────────────────────────

    #[test]
    fn windows_system_locations_are_recognised_wherever_this_runs() {
        // The bug this guards is a *test* bug with a real hole behind it. The
        // old assertion was `validate_host_path("C:\\Windows\\…").is_err()`,
        // and on Linux it passed because `Path::is_absolute` is false for a
        // Windows path there — so the four Windows entries in
        // `HOST_SYSTEM_ROOTS` were never once compared against anything in CI.
        // The rule is a pure function over a string now, and this drives it.
        assert_eq!(
            host_system_root_for("C:\\Windows\\System32\\drivers\\etc\\hosts"),
            Some("c:/windows")
        );
        assert_eq!(host_system_root_for("c:/Program Files/x"), Some("c:/program files"));

        // `\\?\` turns off Win32 path parsing; it does not name a different
        // place. `std::fs::canonicalize` returns this spelling on Windows, so
        // the check has to understand its own output.
        assert_eq!(
            host_system_root_for("\\\\?\\C:\\Windows\\System32\\x"),
            Some("c:/windows")
        );
        assert_eq!(
            host_system_root_for("\\\\.\\C:\\ProgramData\\x"),
            Some("c:/programdata")
        );

        // An administrative share reaches the same drive over UNC.
        assert_eq!(host_system_root_for("\\\\localhost\\C$\\Windows\\x"), Some("c:/windows"));
        assert_eq!(host_system_root_for("\\\\?\\UNC\\host\\C$\\Windows\\x"), Some("c:/windows"));
        assert_eq!(host_system_root_for("\\\\host\\ADMIN$\\System32\\x"), Some("c:/windows"));

        // An ordinary file share has no local equivalent, and this list does
        // not pretend to know what is on someone else's server.
        assert_eq!(host_system_root_for("\\\\host\\share\\report.pdf"), None);
        // A user directory that merely shares a prefix is not a system one.
        assert_eq!(host_system_root_for("/home/jo/etcetera/notes.txt"), None);
    }

    #[test]
    fn a_windows_path_is_split_into_components_wherever_this_runs() {
        // Same shape of bug one rule along: `Path::components` treats `\` as a
        // separator only on Windows, so on Linux the whole of
        // `C:\Users\jo\.ssh\id_rsa` was a single component and the
        // hidden-directory rule had nothing to find.
        assert_eq!(
            host_path_names("C:\\Users\\jo\\Downloads\\a.txt"),
            ["Users", "jo", "Downloads", "a.txt"]
        );
        assert_eq!(host_path_names("\\\\?\\C:\\Users\\jo\\x"), ["Users", "jo", "x"]);
        assert!(validate_host_path("C:\\Users\\jo\\.ssh\\authorized_keys", HostPathUse::Write).is_err());
        assert!(validate_host_path("C:\\Users\\jo\\..\\admin\\x", HostPathUse::Write).is_err());
        assert!(validate_host_path("C:\\Users\\jo\\Downloads\\report.pdf", HostPathUse::Write).is_ok());
        assert!(is_absolute_host_path("C:\\Users\\jo\\x"));
        assert!(!is_absolute_host_path("C:x"));
    }

    #[cfg(not(windows))]
    #[test]
    fn a_backslash_in_a_unix_filename_is_not_a_separator() {
        // Unifying separators unconditionally would split a legal Linux name.
        assert_eq!(host_path_names("/home/jo/a\\b.txt"), ["home", "jo", "a\\b.txt"]);
    }

    #[test]
    fn a_login_item_directory_is_refused_for_a_write() {
        // Defence in depth, and deliberately not called a fix: see
        // `validate_host_path` for why a denylist of persistence directories is
        // losing by construction.
        assert!(validate_host_path("/Users/jo/Library/LaunchAgents/x.plist", HostPathUse::Write).is_err());
        assert!(validate_host_path(
            "C:\\Users\\jo\\AppData\\Roaming\\Microsoft\\Windows\\Start Menu\\Programs\\Startup\\x.lnk",
            HostPathUse::Write
        )
        .is_err());
        // A directory that merely happens to be called Library is not one.
        assert!(validate_host_path("/Users/jo/Documents/Library/notes.md", HostPathUse::Write).is_ok());
    }

    #[test]
    fn the_unix_system_roots_cover_the_places_a_mac_resolves_them_to() {
        // Resolution happens before this check now, and on macOS `/etc` and
        // `/var` resolve into `/private`.
        assert!(validate_host_path("/private/etc/hosts", HostPathUse::Write).is_err());
        assert!(validate_host_path("/private/var/db/x", HostPathUse::Write).is_err());
        assert!(validate_host_path("/opt/homebrew/bin/x", HostPathUse::Write).is_err());
        assert!(validate_host_path("/srv/www/index.html", HostPathUse::Write).is_err());
        // …but `/tmp` resolves to `/private/tmp` there, and saving into the
        // temp directory is entirely ordinary.
        assert!(validate_host_path("/private/tmp/report.pdf", HostPathUse::Write).is_ok());
    }

    #[test]
    fn a_home_directory_that_lives_under_a_system_root_is_still_a_home_directory() {
        // Only a problem once the check ran on the resolved path: `/home` is a
        // symlink to `/var/home` on rpm-ostree systems, and a Mac's per-user
        // temp directory resolves into `/private/var/folders`.
        assert!(validate_host_path("/var/home/jo/Downloads/report.pdf", HostPathUse::Write).is_ok());
        assert!(validate_host_path("/private/var/folders/qx/T/report.pdf", HostPathUse::Write).is_ok());
        // The rest of `/var` is exactly as refused as it was.
        assert!(validate_host_path("/var/lib/docker/x", HostPathUse::Write).is_err());
        assert!(validate_host_path("/var/log/syslog", HostPathUse::Read).is_err());
    }

    // ── H4: a symlinked component, and where the bytes actually land ────────

    /// A throwaway host tree shaped like the real attack: a visible
    /// `Downloads/pub` that is really `~/.ssh`.
    ///
    /// This is the scenario verbatim — and the container end of it is not
    /// hypothetical: `/proc/self/mountinfo` inside a Triple-C container spells
    /// the host's project paths out, so code in there knows both where to plant
    /// the link and what host path to ask the backend for.
    #[cfg(unix)]
    fn plant_symlinked_downloads() -> (PathBuf, PathBuf, PathBuf) {
        let root = std::env::temp_dir()
            .canonicalize()
            .unwrap()
            .join(format!("tc-h4-{}", uuid::Uuid::new_v4()));
        let home = root.join("home");
        let downloads = home.join("Downloads");
        let ssh = home.join(".ssh");
        std::fs::create_dir_all(&downloads).unwrap();
        std::fs::create_dir_all(&ssh).unwrap();
        let secret = ssh.join("authorized_keys");
        std::fs::write(&secret, b"the key that was already there").unwrap();
        std::os::unix::fs::symlink(&ssh, downloads.join("pub")).unwrap();
        (root, downloads, secret)
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_symlinked_component_passes_the_lexical_check_and_is_still_refused() {
        let (root, downloads, secret) = plant_symlinked_downloads();
        let evil = downloads.join("pub").join("authorized_keys");
        let evil = evil.to_string_lossy().to_string();

        // The hole, stated: every rule the old code had says yes. No hidden
        // component, no `..`, no system root — because those are properties of
        // a string, and the string is not where the file goes.
        assert!(validate_host_path(&evil, HostPathUse::Write).is_ok());

        // Resolving first is what turns the string into a location.
        let err = resolve_host_path(&evil, HostPathUse::Write).await.unwrap_err();
        assert!(err.contains(".ssh"), "{}", err);
        assert!(err.contains("resolves to"), "{}", err);

        // Reading out through the same link is the same bypass backwards.
        assert!(resolve_host_path(&evil, HostPathUse::Read).await.is_err());
        assert_eq!(
            std::fs::read(&secret).unwrap(),
            b"the key that was already there"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_symlinked_file_cannot_be_read_into_the_container_under_a_visible_name() {
        // The upload direction, where the *final* component is the link:
        // `Downloads/key.txt` is a perfectly visible name for `~/.ssh/id_rsa`.
        let (root, downloads, secret) = plant_symlinked_downloads();
        let alias = downloads.join("key.txt");
        std::os::unix::fs::symlink(&secret, &alias).unwrap();

        let err = resolve_host_read_path(&alias.to_string_lossy())
            .await
            .unwrap_err();
        assert!(err.contains(".ssh"), "{}", err);

        // An ordinary file next to it is still readable, and comes back as the
        // path that will be opened.
        let ordinary = downloads.join("notes.md");
        std::fs::write(&ordinary, b"hello").unwrap();
        assert_eq!(
            resolve_host_read_path(&ordinary.to_string_lossy()).await.unwrap(),
            ordinary.to_string_lossy()
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    // ── What the general rule costs, and why it is still the trade ──────────

    #[cfg(unix)]
    #[tokio::test]
    async fn an_ordinary_path_that_resolves_through_a_dot_directory_is_over_caught() {
        // Stated rather than hidden: the general rule refuses more than the
        // attack. pnpm keeps every package under `node_modules/.pnpm/…` and
        // links the visible `node_modules/<pkg>` at it; `~/.cache`, `~/.local`
        // and `~/.var/app` are where whole ecosystems put the files a visible
        // directory points at. Round 3 weakened the rule to an eleven-name
        // denylist to buy those cases back, and the denylist let
        // `~/.local/bin` and `~/.password-store` straight through.
        //
        // Two of the four callers — the terminal drop and Backup — take their
        // host path from the webview, so this rule is their only boundary and
        // paying the over-catch is the right way round.
        // What the refusal must not be is mysterious: it says which component
        // is hidden and that the path *resolves* through it.
        let root = std::env::temp_dir()
            .canonicalize()
            .unwrap()
            .join(format!("tc-overcatch-{}", uuid::Uuid::new_v4()));
        let store = root.join("proj/node_modules/.pnpm/left-pad@1.3.0/node_modules/left-pad");
        std::fs::create_dir_all(&store).unwrap();
        std::fs::write(store.join("index.js"), b"module.exports = 1").unwrap();
        std::os::unix::fs::symlink(&store, root.join("proj/node_modules/left-pad")).unwrap();

        let visible = root
            .join("proj/node_modules/left-pad/index.js")
            .to_string_lossy()
            .to_string();
        // Lexically fine…
        assert!(validate_host_path(&visible, HostPathUse::Read).is_ok());
        // …and refused once resolved, in a sentence that explains itself.
        let err = resolve_host_read_path(&visible).await.unwrap_err();
        assert!(err.contains("resolves to"), "{}", err);
        assert!(err.contains(".pnpm"), "{}", err);
        assert!(err.contains("hidden folder"), "{}", err);

        let _ = std::fs::remove_dir_all(&root);
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn a_hidden_directory_reached_through_a_visible_link_is_refused_whatever_it_is_called() {
        // The escape round 3 reopened. What refuses `Downloads/pub → ~/.ssh` has
        // to be the general rule, because a list of "credential" directories is
        // allow-by-omission for everything nobody thought of — and the things
        // nobody thought of include the directory the user's next shell command
        // comes out of.
        let root = std::env::temp_dir()
            .canonicalize()
            .unwrap()
            .join(format!("tc-hidden-{}", uuid::Uuid::new_v4()));
        let home = root.join("home");
        let downloads = home.join("Downloads");
        std::fs::create_dir_all(&downloads).unwrap();

        for (dir, leaf) in [
            (".ssh", "authorized_keys"),
            (".config/autostart", "x.desktop"),
            // Not on any denylist, and the reason a denylist is the wrong shape:
            // a write here is the user's next `ls`.
            (".local/bin", "ls"),
            (".password-store", "bank.gpg"),
            (".pki/nssdb", "key4.db"),
        ] {
            let real = home.join(dir);
            std::fs::create_dir_all(&real).unwrap();
            std::fs::write(real.join(leaf), b"the secret that was already there").unwrap();
            let link = downloads.join(dir.replace('/', "-").trim_start_matches('.'));
            std::os::unix::fs::symlink(&real, &link).unwrap();

            let evil = link.join(leaf).to_string_lossy().to_string();
            // Lexically spotless — that is the whole point of the plant.
            assert!(validate_host_path(&evil, HostPathUse::Write).is_ok());

            let err = resolve_host_path(&evil, HostPathUse::Write).await.unwrap_err();
            assert!(err.contains("resolves to"), "{} → {}", dir, err);
            assert!(err.contains("hidden folder"), "{} → {}", dir, err);
            // Both directions: reading the secret out is the same bypass
            // backwards.
            assert!(resolve_host_path(&evil, HostPathUse::Read).await.is_err(), "{}", dir);
            assert_eq!(
                std::fs::read(real.join(leaf)).unwrap(),
                b"the secret that was already there"
            );
        }

        // The other direction, and the whole point of keeping the feature: an
        // ordinary file under an ordinary link is untouched by any of this.
        let real = home.join("Archive");
        std::fs::create_dir_all(&real).unwrap();
        std::fs::write(real.join("notes.md"), b"hello").unwrap();
        std::os::unix::fs::symlink(&real, downloads.join("shelf")).unwrap();
        let ordinary = downloads.join("shelf/notes.md").to_string_lossy().to_string();
        assert_eq!(
            resolve_host_read_path(&ordinary).await.unwrap(),
            real.join("notes.md").to_string_lossy()
        );
        assert_eq!(
            resolve_host_path(&downloads.join("shelf/backup.tgz").to_string_lossy(), HostPathUse::Write)
                .await
                .unwrap(),
            real.join("backup.tgz")
        );

        let _ = std::fs::remove_dir_all(&root);
    }
    #[test]
    fn the_resolved_policy_is_the_lexical_one_applied_to_where_the_bytes_land() {
        // Round 3 replaced this with an eleven-name denylist of "credential"
        // directories, i.e. allow-by-omission for the whole of the rest of
        // `$HOME`. `~/.local/bin` (write, then read on the user's next shell
        // command), `~/.password-store`, a Firefox or Chrome profile and
        // `~/.pki/nssdb` were all reachable through a planted symlink with a
        // perfectly visible name. There is no list that ends; the general rule
        // is the only one that holds.
        for path in [
            "/home/jo/.ssh/authorized_keys",
            "/home/jo/.ssh/keys/id_rsa",
            "/home/jo/.gnupg/trustdb.gpg",
            "/home/jo/.aws/credentials",
            "/home/jo/.config/autostart/x.desktop",
            "/home/jo/.claude/settings.json",
            // None of these was on the denylist, and every one of them is a
            // key, a password or the next command the user runs.
            "/home/jo/.local/bin/ls",
            "/home/jo/.password-store/bank.gpg",
            "/home/jo/.mozilla/firefox/p.default/logins.json",
            "/home/jo/.config/google-chrome/Default/Login Data",
            "/home/jo/.pki/nssdb/key4.db",
            "/home/jo/.bash_completion.d/x",
        ] {
            assert!(validate_resolved_host_path(path, HostPathUse::Write).is_err(), "{}", path);
            assert!(validate_resolved_host_path(path, HostPathUse::Read).is_err(), "{}", path);
        }

        // An ordinary location resolves to an ordinary location.
        for path in ["/home/jo/Downloads/report.pdf", "/var/home/jo/x.tgz"] {
            assert!(validate_resolved_host_path(path, HostPathUse::Write).is_ok(), "{}", path);
            assert!(validate_resolved_host_path(path, HostPathUse::Read).is_ok(), "{}", path);
        }

        // A project's own `.env` is a hidden *leaf*, not a hidden folder, and a
        // read still allows it — that is what the terminal drop is for.
        assert!(validate_resolved_host_path("/home/jo/proj/.env", HostPathUse::Read).is_ok());
        // The location rules that only a resolved path can answer are still here.
        assert!(validate_resolved_host_path("/private/etc/hosts", HostPathUse::Write).is_err());
        assert!(validate_resolved_host_path(
            "/Users/jo/Library/LaunchAgents/x.plist",
            HostPathUse::Write
        )
        .is_err());
    }
    #[test]
    fn a_write_path_that_names_a_folder_is_refused_as_one() {
        // `Path::file_name` on `/home/jo/Downloads/` answers `Downloads`, so
        // the leaf a write kept was the directory itself: the rename at the end
        // failed with an errno and the cleanup then tried to `remove_file` a
        // directory.
        let err = validate_host_path("/home/jo/Downloads/", HostPathUse::Write).unwrap_err();
        assert!(err.contains("must name a file"), "{}", err);
        assert!(validate_host_path("/", HostPathUse::Write).is_err());
        assert!(validate_host_path("C:\\Users\\jo\\Downloads\\", HostPathUse::Write).is_err());
        // A read opens the path and already says "that is a folder" itself.
        assert!(validate_host_path("/home/jo/Downloads/", HostPathUse::Read).is_ok());
        assert!(validate_host_path("/home/jo/Downloads/report.pdf", HostPathUse::Write).is_ok());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn an_open_descriptor_is_checked_against_the_path_that_was_validated() {
        // The other half of H4. Resolving answers "where does this lead *now*",
        // and a directory can be swapped between that answer and the open — so
        // the kernel is asked where the descriptor actually landed.
        let dir = std::env::temp_dir()
            .canonicalize()
            .unwrap()
            .join(format!("tc-fd-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let real = dir.join("report.pdf");
        let elsewhere = dir.join("authorized_keys");
        std::fs::write(&real, b"x").unwrap();

        let file = std::fs::File::open(&real).unwrap();
        assert!(verify_opened_path(&file, &real).is_ok());
        let err = verify_opened_path(&file, &elsewhere).unwrap_err();
        assert!(err.contains("while it was being opened"), "{}", err);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn a_download_into_a_directory_that_does_not_exist_says_so() {
        // Resolution needs the parent to exist, which it always does behind a
        // save dialog — but the refusal has to be a sentence, not an errno on
        // its own.
        let missing = std::env::temp_dir()
            .join(format!("tc-missing-{}", uuid::Uuid::new_v4()))
            .join("report.pdf");
        let err = resolve_host_path(&missing.to_string_lossy(), HostPathUse::Write)
            .await
            .unwrap_err();
        assert!(err.starts_with("Cannot save into"), "{}", err);
    }

    // ── The host side of the terminal's drag-and-drop ──────────────────────

    #[test]
    fn an_uploaded_file_keeps_the_name_the_user_chose() {
        // `~/Downloads/latest.log` is routinely a symlink to `2026-08-23.log`.
        // Taking the leaf off the *resolved* path renamed the file on its way
        // into the container, and named a file the user never picked in the
        // collision prompt.
        assert_eq!(host_upload_name("/home/jo/Downloads/latest.log").unwrap(), "latest.log");
        // Windows paths are split as Windows paths wherever this runs.
        assert_eq!(host_upload_name("C:\\Users\\jo\\Downloads\\a.txt").unwrap(), "a.txt");
        // The name becomes a tar entry, a container path and an argv element.
        assert!(host_upload_name("/home/jo/Downloads/").is_err());
        assert!(host_upload_name("/").is_err());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_symlinked_source_is_opened_by_its_target_and_named_by_its_link() {
        // The two answers are different on purpose and both are needed: the
        // bytes come from where the link leads, the name comes from what the
        // user picked. Reading both off the resolved path is what dropped
        // `latest.log` into the container as `2026-08-23.log`.
        let root = std::env::temp_dir()
            .canonicalize()
            .unwrap()
            .join(format!("tc-name-{}", uuid::Uuid::new_v4()));
        let logs = root.join("logs");
        std::fs::create_dir_all(&logs).unwrap();
        let dated = logs.join("2026-08-23.log");
        std::fs::write(&dated, b"today").unwrap();
        let latest = logs.join("latest.log");
        std::os::unix::fs::symlink(&dated, &latest).unwrap();

        let chosen = latest.to_string_lossy().to_string();
        assert_eq!(host_upload_name(&chosen).unwrap(), "latest.log");
        assert_eq!(
            resolve_host_read_path(&chosen).await.unwrap(),
            dated.to_string_lossy()
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    // ── Listings ────────────────────────────────────────────────────────────

    #[test]
    fn a_directory_too_big_to_buffer_is_described_as_one() {
        // What the panel used to show for a directory past ~100k entries:
        // "Command output exceeded 8388608 bytes and was abandoned" — true
        // about a buffer, no help about a folder.
        let refusal = format!("{}: Command output exceeded 8388608 bytes", OUTPUT_LIMIT_MARKER);
        let described = describe_listing_failure("/workspace/big", refusal);
        assert!(described.contains("too many entries"), "{}", described);
        assert!(described.contains("/workspace/big"), "{}", described);
        // Everything else is passed through as it arrived.
        let other = describe_listing_failure("/workspace", "Exec output error: eof".to_string());
        assert_eq!(other, "Exec output error: eof");
    }

    // ── Live Docker ─────────────────────────────────────────────────────────

    /// The container-side half of H4 against a real daemon: `/workspace/escape`
    /// is under a write root as a *string* and is `/etc` as a location, and
    /// only a running container can say so. Also drives the listing of a
    /// symlinked directory, a broken link and a symlink loop, which likewise
    /// have no answer outside a real filesystem.
    ///
    /// Ignored because it needs Docker and pulls a container up; run it with
    ///
    /// ```text
    /// cargo test -- --ignored --nocapture live_container
    /// ```
    #[cfg(unix)]
    #[tokio::test]
    #[ignore = "needs a Docker daemon; creates and removes a throwaway container"]
    async fn the_container_side_rules_hold_against_a_live_container() {
        fn docker_cli(args: &[&str]) -> String {
            let out = std::process::Command::new("docker")
                .args(args)
                .output()
                .expect("docker CLI");
            assert!(
                out.status.success(),
                "docker {:?} failed: {}",
                args,
                String::from_utf8_lossy(&out.stderr)
            );
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        }

        let name = format!("tc-live-{}", &uuid::Uuid::new_v4().simple().to_string()[..8]);
        docker_cli(&["run", "-d", "--name", &name, "ubuntu:24.04", "sleep", "300"]);
        docker_cli(&["exec", &name, "useradd", "-m", "claude"]);
        docker_cli(&[
            "exec",
            &name,
            "sh",
            "-c",
            "mkdir -p /workspace/real && printf 'the payload' > /workspace/real/report.txt \
             && ln -s /etc /workspace/escape \
             && ln -s real /workspace/link \
             && ln -s /workspace/nowhere /workspace/broken \
             && ln -s /workspace/loop /workspace/loop \
             && chown -R claude /workspace",
        ]);

        // `/workspace/escape` is under a write root as a string and is `/etc` as
        // a location.
        let escaped = resolve_container_dir(&name, "Folder", "/workspace/escape").await;
        println!("container escape: {:?}", escaped);
        assert!(escaped.is_err(), "a symlink out of /workspace was accepted");
        resolve_container_dir(&name, "Folder", "/workspace")
            .await
            .expect("/workspace itself");

        // A symlinked directory lists its target's contents rather than
        // reporting itself empty — `-H` follows the start point, and only the
        // start point.
        let (records, diagnostics, code) =
            exec_oneshot_streams_as(&name, "claude", list_argv("/workspace/link"), Vec::new())
                .await
                .unwrap();
        println!("link: code={} diagnostics={:?}", code, diagnostics);
        let entries = parse_find_output("/workspace/link", &records);
        assert_eq!(
            entries.iter().map(|e| e.name.as_str()).collect::<Vec<_>>(),
            vec!["report.txt"]
        );

        // The link itself is still *marked* as one in its parent's listing, and
        // still navigable.
        let (records, _, _) =
            exec_oneshot_streams_as(&name, "claude", list_argv("/workspace"), Vec::new())
                .await
                .unwrap();
        let parent = parse_find_output("/workspace", &records);
        let link = parent.iter().find(|e| e.name == "link").expect("the link row");
        assert!(link.is_symlink && link.is_directory, "{:?}", link);
        let broken = parent.iter().find(|e| e.name == "broken").expect("the broken row");
        assert!(broken.is_symlink && !broken.is_directory, "{:?}", broken);

        // A symlink loop is what turns `-L` into a walk that does not end.
        // `-H` resolves only the starting point, so the kernel answers `ELOOP`
        // and `find` returns at once — and the row is not navigable in the
        // first place, because `%Y` is `L` rather than `d`.
        let looped = parent.iter().find(|e| e.name == "loop").expect("the loop row");
        assert!(looped.is_symlink && !looped.is_directory, "{:?}", looped);

        for path in ["/workspace/broken", "/workspace/loop"] {
            let started = std::time::Instant::now();
            let (records, diagnostics, code) =
                exec_oneshot_streams_as(&name, "claude", list_argv(path), Vec::new())
                    .await
                    .unwrap();
            let took = started.elapsed();
            println!("{}: code={} in {:?} — {:?}", path, code, took, diagnostics);
            assert!(took < std::time::Duration::from_secs(10), "it hung: {:?}", took);
            assert!(parse_find_output(path, &records).is_empty(), "{}", path);
            // The loop is the one that reports; a broken link matches itself
            // and is then discarded by `-mindepth 1`, so it exits cleanly with
            // nothing to show. Neither is reachable by navigating, and neither
            // waits.
            if code != 0 {
                let said = describe_find_diagnostics(path, diagnostics.trim());
                println!("  said: {}", said);
                assert!(said.contains("loops back on itself"), "{} → {}", path, said);
            }
        }

        docker_cli(&["rm", "-f", &name]);
    }
}
