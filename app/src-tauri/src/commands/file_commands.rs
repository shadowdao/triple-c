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

/// What a host path is about to be used for. The two directions differ over
/// hidden names — see [`validate_host_path`].
#[derive(Clone, Copy, Debug, PartialEq)]
enum HostPathUse {
    /// Host bytes are about to be read *into* the container.
    Read,
    /// Container bytes are about to be written *onto* the host.
    Write,
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
    s
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
/// Two callers are left, and both are occasional rather than routine: dropping
/// a host file onto the terminal, and "Back up container". Neither is reached
/// through a path the webview invented — a `save()`/`open()` dialog stands in
/// front of both — but a dialog is a UI convention, not a boundary: every
/// command is a single `invoke` away from any code running in the webview, with
/// container-controlled bytes on one side of it. So the backend has its own
/// policy:
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
/// For two occasional callers, over-refusing is the cheaper mistake, so the
/// general rule stands and the refusal says plainly what tripped it.
///
/// **The rest of the policy is still a denylist, which is losing by
/// construction.** `~/Library/LaunchAgents`, `%AppData%\…\Startup` and `/opt`
/// are only refused because someone thought of them; the next persistence
/// directory is not. The honest fix is for the *backend* to own the file dialog
/// (`tauri-plugin-dialog` can be driven from Rust) so that the only host paths
/// these commands accept are ones the user just pointed at, and no path arrives
/// over IPC at all. Until then: these lists are defence in depth, and the
/// dialog is the boundary.
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
        HostPathUse::Read => names.len().saturating_sub(1),
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
/// for what that costs and why it is the right way round for the two callers
/// that are left. This is a thin wrapper rather than a second body so the two
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
        HostPathUse::Write => {
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
            if let Ok(full) = tokio::fs::canonicalize(&joined).await {
                let real = full.file_name().map(|n| n.to_string_lossy().to_string());
                if real.as_deref().is_some_and(|n| n.starts_with('.')) {
                    return Err(format!(
                        "\"{}\" is a hidden file — Triple-C will not save there. Choose a visible name.",
                        real.unwrap_or_default()
                    ));
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
/// Public because its one caller lives elsewhere: the terminal's drag-and-drop
/// drop target, `terminal_commands::upload_host_file_to_terminal`. That is the
/// only way a host file gets into a container now — the Files pane is a
/// container-side browser and does no host I/O at all.
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
/// One caller left: [`download_container_backup`], which is the only command
/// that still puts container bytes on the host.
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
    let mut partial = name.to_os_string();
    partial.push(format!(
        ".triple-c-part-{}",
        &uuid::Uuid::new_v4().simple().to_string()[..8]
    ));
    Ok(dest.with_file_name(partial))
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

    // The backup runs inside the container via `docker exec`, which requires it
    // to be running. Fail with a clear message rather than a raw Docker error.
    let running = docker
        .inspect_container(container_id, None)
        .await
        .ok()
        .and_then(|info| info.state)
        .and_then(|s| s.running)
        .unwrap_or(false);
    if !running {
        return Err("Start the project before backing up — the backup runs inside the running container.".to_string());
    }

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
                stderr_text.push_str(&String::from_utf8_lossy(&message));
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
        // These two callers — the terminal drop and Backup — are occasional
        // rather than routine, so paying the over-catch is the right way round.
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
