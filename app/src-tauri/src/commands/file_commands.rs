use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
use bollard::container::{DownloadFromContainerOptions, LogOutput, UploadToContainerOptions};
use bollard::exec::{CreateExecOptions, StartExecResults};
use futures_util::StreamExt;
use serde::Serialize;
use tauri::State;

use crate::docker::client::get_docker;
use crate::docker::exec::{
    build_single_file_tar, container_user_ids, exec_oneshot_as, exec_oneshot_as_within,
    exec_oneshot_streams_as, now_epoch_secs, OUTPUT_LIMIT_MARKER,
};
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

/// Ceiling on a single upload, mirroring the terminal drop path's guard. The
/// file is packed into an in-memory tar before it goes anywhere.
const MAX_UPLOAD_BYTES: u64 = 256 * 1024 * 1024;

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
        // `find`'s own words — "Permission denied", "No such file or directory"
        // — are the whole diagnosis.
        let detail = diagnostics.trim();
        return Err(if detail.is_empty() {
            format!("Could not list {} (exit {})", path, code)
        } else {
            detail.to_string()
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

/// Container roots this panel may *create, rename or upload into*.
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

/// Directory *sequences* that hold credentials or authority, judged on the
/// path a symlink chain really leads to.
///
/// This is the part of the hidden-component rule that has to survive
/// resolution. The rule itself cannot: see [`validate_resolved_host_path`] for
/// why "the real location passes through a dot directory" describes ordinary
/// software far more often than it describes an attack — `node_modules/.pnpm`,
/// `~/.local/share`, `~/.cache`, `~/.var/app`, `~/.nvm`, `~/.cargo`. What
/// *is* worth refusing after resolution is the small set of directories whose
/// contents are keys, tokens and startup entries, and those can be named.
///
/// Matched as a contiguous run of components anywhere in the path, so
/// `~/.ssh/keys/id_rsa` is as refused as `~/.ssh/id_rsa`. Same defence-in-depth
/// footing as [`HOST_AUTORUN_DIRS`], and the same honest caveat: it is a list
/// of the places that are known, not of the ones that exist. The boundary is
/// the file dialog; this is what stops a *planted symlink* aiming an otherwise
/// ordinary-looking path at the one directory the attack wants.
const HOST_CREDENTIAL_DIRS: &[&[&str]] = &[
    &[".ssh"],
    &[".gnupg"],
    &[".aws"],
    &[".azure"],
    &[".kube"],
    &[".docker"],
    &[".claude"],
    &[".config", "gcloud"],
    &[".config", "autostart"],
    &[".config", "systemd", "user"],
    &[".local", "share", "keyrings"],
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

/// The [`HOST_CREDENTIAL_DIRS`] entry `names` passes through, spelled the way
/// the list spells it so the refusal can name it.
///
/// Anywhere in the path rather than at the end: what matters is that the path
/// goes *through* `~/.ssh`, not how much further it goes.
fn credential_dir_in(names: &[String]) -> Option<String> {
    HOST_CREDENTIAL_DIRS.iter().find_map(|seq| {
        (0..names.len().saturating_sub(seq.len() - 1)).find_map(|start| {
            names[start..start + seq.len()]
                .iter()
                .zip(seq.iter())
                .all(|(have, want)| have.eq_ignore_ascii_case(want))
                .then(|| seq.join("/"))
        })
    })
}

/// Structural and policy checks on a host path *as written*, returning it as a
/// [`PathBuf`].
///
/// The `save()`/`open()` dialog the Files pane puts in front of these commands
/// is a UI convention, not a boundary — every one of them is a single `invoke`
/// away from any code running in the webview, with a container-controlled
/// payload on one side. So the backend has its own policy:
///
///   * absolute, no `..`, no NUL — judged on the path's own components, so a
///     Windows path is judged as one wherever this runs;
///   * nothing under [`HOST_SYSTEM_ROOTS`] or in a login-item directory;
///   * no *hidden* path components. The interesting targets for "write a
///     container-controlled file to an arbitrary host path" are mostly dot
///     directories — `~/.ssh/authorized_keys`, `~/.config/autostart/`,
///     `~/.claude/` — and the interesting targets for the reverse, reading a
///     host file into the container, are the same ones plus `~/.aws/credentials`.
///     A download is refused a hidden *name* too (creating `~/.bashrc` is escape
///     all by itself); an upload only cares about hidden *directories*, because
///     dragging a project's own `.env` into the container is an ordinary thing
///     to do and its parent is not hidden.
///
/// **This is a lexical check on the string the user chose, and it judges only
/// that.** A path whose components are all visible can still *lead* somewhere
/// that deserves a second opinion, which is why [`resolve_host_path`] follows
/// this with [`validate_resolved_host_path`] over the canonical form. The two
/// ask different questions and must not be confused: this one is "did the user
/// point at a hidden place", that one is "where do these bytes actually land".
/// Re-running *this* function over a canonical path is the H6 regression — it
/// refuses `node_modules/pkg` under pnpm, and every visible directory that
/// symlinks into `~/.local/share`, `~/.cache`, `~/.var/app`, `~/.nvm` or
/// `~/.cargo`, none of which is anybody's attack.
///
/// **And the policy itself is a denylist, which is losing by construction.**
/// `~/Library/LaunchAgents`, `%AppData%\…\Startup`, `~/bin` and `/opt` are only
/// refused because someone thought of them; the next persistence directory is
/// not. The honest fix is not a longer list — it is for the *backend* to own the
/// file dialog (`tauri-plugin-dialog` can be driven from Rust) so that the only
/// host paths these commands accept are ones the user just pointed at, and no
/// path arrives over IPC at all. That is a frontend change as well as this one.
/// Until then: this list is defence in depth, and the dialog is the boundary.
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
        return Err(format!(
            "\"{}\" is a hidden {} — Triple-C will not {} there. Choose a visible location.",
            hidden,
            if names.last() == Some(hidden) { "file" } else { "folder" },
            if use_for == HostPathUse::Write { "save" } else { "read" }
        ));
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

/// The policy that applies to a *canonical* path — where the bytes really land,
/// as opposed to what the user typed.
///
/// H6, and the reason this is a separate function rather than a second call to
/// [`validate_host_path`]. Canonicalisation resolves *through* symlinks, so the
/// answer is a description of the filesystem's layout and not of the user's
/// intent. Judging it with the hidden-component rule made a great deal of
/// perfectly ordinary software unusable:
///
///   * pnpm stores every package under `node_modules/.pnpm/…` and links the
///     visible `node_modules/pkg` at it, so uploading a file out of a
///     dependency was "`.pnpm` is a hidden folder";
///   * `~/.local/share`, `~/.cache`, `~/.var/app` (Flatpak), `~/.nvm` and
///     `~/.cargo` are where whole ecosystems keep the files a visible directory
///     points at, and every download into one of those was refused.
///
/// None of that is an escape, and none of it was refused before resolution
/// started. So the canonical form is used for what only it can answer —
/// *containment*: the system roots (a Mac's `/etc` **is** `/private/etc`), the
/// login-item directories, and the credential directories in
/// [`HOST_CREDENTIAL_DIRS`]. That last list is what keeps H4's escape closed:
/// `Downloads/pub` → `~/.ssh` with a leaf of `authorized_keys` has no hidden
/// component *and no other tell*, and it is refused here because of where it
/// lands, not because of how the directory is spelled.
///
/// The leaf's own *identity* is judged by [`resolve_host_path`], which is the
/// other thing only a canonical path knows — Windows hands out `BASHRC~1` for
/// `.bashrc`.
fn validate_resolved_host_path(resolved: &str, use_for: HostPathUse) -> Result<(), String> {
    if let Some(root) = host_system_root_for(resolved) {
        return Err(format!(
            "{} is a system location — Triple-C will not {} files there.",
            root,
            if use_for == HostPathUse::Write { "write" } else { "read" }
        ));
    }

    let names = host_path_names(resolved);
    let dirs = &names[..names.len().saturating_sub(1)];
    if use_for == HostPathUse::Write && is_autorun_dir(dirs) {
        return Err(format!(
            "{} is a startup folder — Triple-C will not save there. Choose an ordinary location.",
            dirs.last().map(String::as_str).unwrap_or(resolved)
        ));
    }

    // The leaf is excluded on purpose. For a write it is never followed (the
    // partial file is created with `O_EXCL` and renamed into place), and for a
    // read a *file* called `.aws` is not the credential store — the directory
    // is. Uploading a project's own `.env` has to keep working.
    if let Some(dir) = credential_dir_in(dirs) {
        return Err(format!(
            "\"{}\" holds credentials — Triple-C will not {} files there.",
            dir,
            if use_for == HostPathUse::Write { "save" } else { "read" }
        ));
    }

    Ok(())
}

/// The host path a command is really going to open: every symlink in it
/// resolved by the OS, judged by [`validate_host_path`] as the user wrote it
/// and by [`validate_resolved_host_path`] as it really lands.
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
                        "\"{}\" is a hidden file — Triple-C will not save there. Choose a visible location.",
                        real.unwrap_or_default()
                    ));
                }
            }
            joined
        }
    };

    // The *resolved* policy, not the lexical one a second time: see
    // [`validate_resolved_host_path`]. Re-running the lexical rules here is H6.
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
/// Public because the terminal's drag-and-drop drop target
/// (`terminal_commands::upload_host_file_to_terminal`) is the same primitive as
/// the Files pane's upload and must not have a different policy.
pub async fn resolve_host_read_path(path: &str) -> Result<String, String> {
    Ok(resolve_host_path(path, HostPathUse::Read)
        .await?
        .to_string_lossy()
        .to_string())
}

/// The name an uploaded host file should land under in the container.
///
/// Taken from the path as the user gave it, deliberately — see the call site in
/// [`upload_file_to_container`]. [`host_path_names`] rather than
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

/// Where a download is written before it becomes the file the user asked for.
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

/// Move a finished partial file onto the destination the user chose.
///
/// A plain rename is the whole story on Unix: atomic, and it replaces an
/// existing file. Windows refuses to rename onto an existing path, so the
/// destination is removed and the rename retried — deliberately *only here*,
/// after the payload is completely written and only for a destination the user
/// picked in a save dialog that already asked about overwriting. That is the
/// difference from the old code, which deleted the destination on the *failure*
/// path, when the replacement did not exist.
async fn finish_download(partial: &Path, dest: &Path) -> Result<(), String> {
    match tokio::fs::rename(partial, dest).await {
        Ok(()) => Ok(()),
        Err(_) if tokio::fs::try_exists(dest).await.unwrap_or(false) => {
            tokio::fs::remove_file(dest)
                .await
                .map_err(|e| format!("Failed to replace {}: {}", dest.display(), e))?;
            tokio::fs::rename(partial, dest)
                .await
                .map_err(|e| format!("Failed to save {}: {}", dest.display(), e))
        }
        Err(e) => Err(format!("Failed to save {}: {}", dest.display(), e)),
    }
}

/// Ceiling on one "Save to host…" download, checked against the size the tar
/// entry declares — i.e. before a byte of payload is read.
///
/// The transfer itself is streamed, so this is not a memory bound any more; it
/// is the bound on how much of the user's disk a single mis-aimed or hostile
/// download can consume before anyone notices. Comfortably past any file this
/// panel is used for, and the message names Backup as the way to take a whole
/// tree instead.
const MAX_DOWNLOAD_BYTES: u64 = 8 * 1024 * 1024 * 1024;

/// Refuse an oversize download by its declared size. Split out so the ceiling
/// and its wording are testable without a container.
fn check_download_size(size: u64) -> Result<(), String> {
    if size > MAX_DOWNLOAD_BYTES {
        return Err(format!(
            "{:.1} GB is too large to save ({} GB limit) — use Backup for a whole tree, or read it from the mounted project directly.",
            size as f64 / (1024.0 * 1024.0 * 1024.0),
            MAX_DOWNLOAD_BYTES / (1024 * 1024 * 1024)
        ));
    }
    Ok(())
}

/// The host half of "Save to host…": work out where the file is really going,
/// fill a partial file beside it, and rename that into place.
///
/// Split out from the command because it is the half that carries the security,
/// and because it is then something a test can drive. `fill` never sees the path
/// the caller asked for — only the resolved partial — and every failure path
/// deletes exactly what `fill` created and nothing else. Returns the resolved
/// destination alongside the byte count, because after [`resolve_host_path`]
/// that is not necessarily the path the caller named.
async fn save_to_host<F, Fut>(host_path: &str, fill: F) -> Result<(PathBuf, u64), String>
where
    F: FnOnce(PathBuf, Arc<AtomicBool>) -> Fut,
    Fut: std::future::Future<Output = Result<u64, String>>,
{
    // Resolved, not merely inspected: this is the path that will be opened,
    // with every symlink in its directories already followed. See H4 in
    // [`resolve_host_path`].
    let dest = resolve_host_path(host_path, HostPathUse::Write).await?;

    // Written beside the destination and renamed on success, so a failure
    // anywhere below leaves whatever was already at `dest` untouched.
    let partial = partial_download_path(&dest)?;

    // Set once `fill` has actually created the file, and read on every failure
    // path. Without it the cleanup deleted `partial` whichever way the transfer
    // failed — including the one failure that means "something was already
    // there": 32 bits of UUID make a collision vanishingly unlikely, but
    // "vanishingly unlikely" is not a reason to delete a file this app did not
    // create.
    let created = Arc::new(AtomicBool::new(false));

    match fill(partial.clone(), Arc::clone(&created)).await {
        Ok(written) => {
            if let Err(e) = finish_download(&partial, &dest).await {
                if created.load(Ordering::SeqCst) {
                    let _ = tokio::fs::remove_file(&partial).await;
                }
                return Err(e);
            }
            Ok((dest, written))
        }
        Err(e) => {
            // Only ever our own partial file — never the user's destination,
            // and never a file that was already sitting at the partial's name.
            if created.load(Ordering::SeqCst) {
                let _ = tokio::fs::remove_file(&partial).await;
            }
            Err(e)
        }
    }
}

#[tauri::command]
pub async fn download_container_file(
    project_id: String,
    container_path: String,
    host_path: String,
    state: State<'_, AppState>,
) -> Result<(), String> {
    validate_container_path("File", &container_path)?;

    let project = state
        .projects_store
        .get(&project_id)
        .ok_or_else(|| format!("Project {} not found", project_id))?;

    let container_id = project
        .container_id
        .clone()
        .ok_or_else(|| "Container not running".to_string())?;

    let source = container_path.clone();
    let (dest, written) = save_to_host(&host_path, move |partial, created| async move {
        stream_container_file_to_host(&container_id, &source, &partial, created).await
    })
    .await?;

    log::info!(
        "Saved {} bytes from {} to {}",
        written,
        container_path,
        dest.display()
    );
    Ok(())
}

/// Copy one regular file out of a container straight onto a host path,
/// streaming, and return the number of bytes written.
///
/// The old download path called [`fetch_container_file`] with no cap, which
/// buffered the entire transfer in host RAM twice (the tar, then the extracted
/// bytes) and only refused a *directory* after that buffer had been filled — so
/// `container_path = "/"` pulled the whole container filesystem into memory
/// before erroring, and a 40 GB sparse file was an out-of-memory kill.
///
/// Nothing here holds more than a few chunks at a time: Docker's tar stream is
/// pumped through a small bounded channel into a blocking task, which is where
/// the `tar` crate (synchronous, and the only thing that correctly understands
/// PAX/GNU long-name and large-size members) reads the header, refuses anything
/// that is not a regular file *before creating the host file*, checks the
/// declared size against [`MAX_DOWNLOAD_BYTES`], and only then copies payload to
/// disk.
async fn stream_container_file_to_host(
    container_id: &str,
    container_path: &str,
    dest: &Path,
    created: Arc<AtomicBool>,
) -> Result<u64, String> {
    let docker = get_docker()?;

    let mut stream = docker.download_from_container(
        container_id,
        Some(DownloadFromContainerOptions {
            path: container_path.to_string(),
        }),
    );

    // Four chunks of backpressure: the feeder stops pulling from the socket as
    // soon as the writer stops consuming, which is what bounds memory here.
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<Vec<u8>, String>>(4);
    let feeder = tokio::spawn(async move {
        while let Some(chunk) = stream.next().await {
            let failed = chunk.is_err();
            let item = chunk
                .map(|bytes| bytes.to_vec())
                .map_err(|e| format!("Failed to download file: {}", e));
            // A closed receiver means the reader is done (or gave up) — dropping
            // the stream cancels the rest of the transfer.
            if tx.send(item).await.is_err() || failed {
                break;
            }
        }
    });

    let reader = ChannelReader::new(rx);
    let dest = dest.to_path_buf();
    let label = container_path.to_string();

    let result = tokio::task::spawn_blocking(move || -> Result<u64, String> {
        let mut archive = tar::Archive::new(reader);
        let mut entries = archive
            .entries()
            .map_err(|e| format!("Failed to read tar entries: {}", e))?;
        let mut entry = match entries.next() {
            Some(entry) => entry.map_err(|e| format!("Failed to read tar entry: {}", e))?,
            None => return Err(format!("{} not found in the container", label)),
        };

        // Type first, size second, host file third. That order is the fix.
        let entry_type = entry.header().entry_type();
        if entry_type.is_dir() {
            return Err(format!(
                "{} is a folder — download its files individually, or use Backup to archive a whole tree.",
                label
            ));
        }
        if entry_type.is_symlink() || entry_type.is_hard_link() {
            return Err(format!("{} is a link — save its target instead.", label));
        }
        if !entry_type.is_file() {
            return Err(format!("{} is not a regular file.", label));
        }

        // `entry.size()`, not `header().size()`: the ustar header's size field
        // is 12 octal digits, i.e. it tops out just under 8 GiB, and Docker's Go
        // tar writer puts anything larger in a preceding PAX record instead.
        // Reading the raw header field made a 9 GiB file look like an 8 GiB one
        // and a 40 GiB file look like nothing at all — verified against a real
        // container, where the ceiling below simply did not fire.
        let size = entry.size();
        check_download_size(size)?;

        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&dest)
            .map_err(|e| format!("Failed to create {}: {}", dest.display(), e))?;
        // From here on the file is ours, so the caller may delete it on failure.
        created.store(true, Ordering::SeqCst);
        // `create_new` is `O_EXCL`, so this open cannot have followed a symlink
        // at the final component — but a *directory* on the way could have been
        // swapped since the path was resolved, so ask the kernel where the
        // descriptor actually landed before writing a byte into it.
        verify_opened_path(&file, &dest)?;
        // `take` as well as the header check: the header is container-controlled
        // and a stream that keeps going past it must not keep filling the disk.
        let mut capped = std::io::Read::take(&mut entry, MAX_DOWNLOAD_BYTES);
        let written = std::io::copy(&mut capped, &mut file)
            .map_err(|e| format!("Failed to write {}: {}", dest.display(), e))?;
        Ok(written)
    })
    .await;

    // The blocking side is finished with the stream either way.
    feeder.abort();

    result.map_err(|e| format!("Download task panicked: {}", e))?
}

/// A blocking [`std::io::Read`] over an async channel of chunks.
///
/// The bridge between Docker's async byte stream and the `tar` crate, which is
/// synchronous. It holds one chunk at a time; the channel's capacity is the
/// whole memory budget of a download.
struct ChannelReader {
    rx: tokio::sync::mpsc::Receiver<Result<Vec<u8>, String>>,
    current: Vec<u8>,
    pos: usize,
}

impl ChannelReader {
    fn new(rx: tokio::sync::mpsc::Receiver<Result<Vec<u8>, String>>) -> Self {
        Self {
            rx,
            current: Vec::new(),
            pos: 0,
        }
    }
}

impl std::io::Read for ChannelReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        loop {
            if self.pos < self.current.len() {
                let n = (self.current.len() - self.pos).min(buf.len());
                buf[..n].copy_from_slice(&self.current[self.pos..self.pos + n]);
                self.pos += n;
                return Ok(n);
            }
            match self.rx.blocking_recv() {
                Some(Ok(chunk)) => {
                    self.current = chunk;
                    self.pos = 0;
                }
                Some(Err(e)) => return Err(std::io::Error::other(e)),
                // Stream finished: EOF, which is also how a tar with no trailing
                // zero blocks (a cancelled transfer) ends.
                None => return Ok(0),
            }
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
/// Shared by the "Save to host…" download and the viewer, so both get the same
/// answer. It deliberately goes through Docker's archive endpoint rather than
/// `exec_oneshot`: that reader runs every chunk through `String::from_utf8_lossy`
/// and merges stderr into stdout, so it would both corrupt any non-UTF-8 file
/// and be able to splice diagnostics into what the caller believes is content.
///
/// The transfer is abandoned once the cap (plus enough slack for the tar
/// framing) is in hand, so previewing a huge file does not pull the whole thing
/// across the socket.
///
/// `max_bytes` is deliberately not optional. It used to be, and the download
/// command passed `None`: the cap below then did nothing and the whole file —
/// or the whole *directory tree*, since the type check happens after the read —
/// landed in host RAM twice. Downloads now stream (see
/// [`stream_container_file_to_host`]); everything still using this function
/// buffers, so everything still using it must name a ceiling.
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
            "{} is a folder — download its files individually, or use Backup to archive a whole tree.",
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
    // second used to propagate straight out and leave the partial behind —
    // [`stream_container_file_to_host`] gets this right via `created`, and the
    // two sites disagreeing was the bug. Hence the flag rather than a `?`.
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

/// Marker on the "there is already a file called that" refusal, so the frontend
/// can tell it apart from every other upload failure and raise a
/// Replace/Skip prompt instead of reporting a dead end.
///
/// A marker in the string rather than a typed error because these commands
/// return `Result<_, String>` throughout; changing that shape is a bigger edit
/// than this bug is worth. The token and the "full container path" shape are a
/// contract with `app/src/lib/uploadErrors.ts` — `isFileExistsError` looks for
/// exactly this, and the prompt names the file.
pub const UPLOAD_EXISTS_MARKER: &str = "FILE_EXISTS";

/// The refusal itself. Split out so the marker and the sentence after it are
/// testable without a container.
fn upload_exists_error(dest: &str) -> String {
    format!("{}: {} already exists", UPLOAD_EXISTS_MARKER, dest)
}

/// The exit status [`UPLOAD_RESERVATION_SCRIPT`] uses for "the name is taken".
///
/// A dedicated code rather than "non-zero plus a second probe": the probe is
/// what made a *dangling symlink* a permanent dead end, since `set -C` refused
/// (it sees the link) and `test -e` followed it and said no. The script decides
/// while it is standing on the path, and says which of the two answers it got.
const UPLOAD_RESERVATION_TAKEN: i64 = 3;

/// How long the reservation may take before the upload gives up on it.
///
/// It is one `link(2)` against a directory the container has open; a second is
/// three orders of magnitude more than it needs, and the only thing that could
/// use the rest of it is a container that is not answering at all.
const UPLOAD_RESERVATION_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);

/// The script that claims an upload destination inside the container, but only
/// if nothing is there.
///
/// **`set -C` alone was not `O_EXCL`, and that hung the app.** noclobber makes
/// `>` refuse an existing *regular* file; against anything else the shell opens
/// it and carries on — so a destination that is a FIFO blocked in `open(2)`
/// waiting for a reader that never came. Verified on this host's dash and in a
/// fresh `ubuntu:24.04`: `sh -c 'set -C; : > "$0"' /tmp/apipe` never returns,
/// and the blocked `sh` stays in `ps`. With no ceiling on the exec, the upload
/// command never returned either and the Files pane sat on "Uploading…" for the
/// rest of the session.
///
/// So the reservation is a `link(2)`, which is the primitive the guard actually
/// wanted: it creates a name, it fails with `EEXIST` if the name is taken, it
/// never opens anything, and it cannot block. A staging file (created under
/// noclobber at an unguessable name in the same directory, so it can neither
/// collide nor be pre-planted) is linked to the destination and then unlinked,
/// leaving one ordinary empty file owned by the container user. `trap … EXIT`
/// removes the staging file on every path out, including a signal.
///
/// It also answers correctly for the cases the old probe could not:
///   * a FIFO, socket or device at the destination — `EEXIST`, immediately;
///   * a **dangling** symlink — `link(2)` does not follow the new-path link, so
///     that is `EEXIST` too, and `[ -L ]` confirms it. The user gets the
///     Replace prompt instead of raw shell text.
///
/// The paths travel as separate argv elements and are read back as `$0`/`$1`,
/// so no part of either is ever parsed as script — the same rule as the `find`
/// argv above, for the same reason.
const UPLOAD_RESERVATION_SCRIPT: &str = r#"set -C
trap 'rm -f -- "$1" 2>/dev/null' EXIT
: > "$1" || exit 1
if ln -- "$1" "$0" 2>/dev/null; then exit 0; fi
if [ -e "$0" ] || [ -L "$0" ]; then exit 3; fi
exit 1"#;

/// Where the reservation stages the file it is about to link into place.
///
/// Beside the destination, because `link(2)` cannot cross a filesystem, and
/// under a name carrying 32 random bits so the staging open cannot collide with
/// anything real or be pre-empted by something planted at a guessable name.
fn upload_reservation_staging_path(dest: &str) -> String {
    join_path(
        &parent_dir(dest),
        &format!(
            ".triple-c-reserve-{}",
            &uuid::Uuid::new_v4().simple().to_string()[..8]
        ),
    )
}

/// The argv that runs [`UPLOAD_RESERVATION_SCRIPT`].
fn upload_reservation_argv(dest: &str, staging: &str) -> Vec<String> {
    vec![
        "sh".to_string(),
        "-c".to_string(),
        UPLOAD_RESERVATION_SCRIPT.to_string(),
        dest.to_string(),
        staging.to_string(),
    ]
}

/// Create `dest` exclusively, or say why not.
///
/// The two failures that land here are very different and the frontend treats
/// them differently: "the name is taken" is the refusal it offers a Replace
/// for, everything else (a read-only mount, a missing directory) is a dead end.
/// The script distinguishes them itself — see [`UPLOAD_RESERVATION_TAKEN`] —
/// rather than leaving a second probe to guess, which is what used to strand a
/// dangling symlink with no Replace on offer.
async fn reserve_upload_destination(container_id: &str, dest: &str) -> Result<(), String> {
    let (output, code) = exec_oneshot_as_within(
        container_id,
        "claude",
        upload_reservation_argv(dest, &upload_reservation_staging_path(dest)),
        Vec::new(),
        UPLOAD_RESERVATION_TIMEOUT,
    )
    .await?;
    match code {
        0 => Ok(()),
        UPLOAD_RESERVATION_TAKEN => Err(upload_exists_error(dest)),
        _ => {
            let detail = output.trim();
            Err(if detail.is_empty() {
                format!("Could not create {} (exit {})", dest, code)
            } else {
                detail.to_string()
            })
        }
    }
}

#[tauri::command]
pub async fn upload_file_to_container(
    project_id: String,
    host_path: String,
    container_dir: String,
    // Absent or false means refuse a collision; the frontend re-invokes with
    // `true` once the user has answered Replace. Defaulting to refusal is the
    // point — the safe behaviour is what you get by not asking.
    overwrite: Option<bool>,
    state: State<'_, AppState>,
) -> Result<(), String> {
    // An upload writes into `/workspace/{mount_name}`, i.e. the user's real
    // project directory, so the destination gets the write-root check; the
    // source is a host file being read *into* the container, so it gets the
    // host-read policy.
    validate_container_write_path("Folder", &container_dir)?;
    // The name the file arrives under comes from the path the *user* chose, not
    // from the one the symlinks lead to. `~/Downloads/latest.log` is very often
    // a link to `2026-08-23.log`, and taking the leaf off the resolved path
    // landed it in the container under a name the user had never seen — and
    // named that name in the collision prompt, about a file they did not pick.
    // The resolved path is still what gets opened; only the label differs.
    let file_name = host_upload_name(&host_path)?;
    let host_path = resolve_host_read_path(&host_path).await?;

    let project = state
        .projects_store
        .get(&project_id)
        .ok_or_else(|| format!("Project {} not found", project_id))?;

    let container_id = project
        .container_id
        .as_ref()
        .ok_or_else(|| "Container not running".to_string())?;

    let docker = get_docker()?;

    // Deferred to here rather than sitting with the lexical check above,
    // because resolving the destination needs the container it lives in.
    resolve_container_dir(container_id, "Folder", &container_dir).await?;

    let meta = tokio::fs::metadata(&host_path)
        .await
        .map_err(|e| format!("Cannot access {}: {}", host_path, e))?;

    // A directory here used to reach `std::fs::read`, whose "Is a directory"
    // error says nothing about what to do. Recursive upload is a bigger feature
    // than this panel needs; refuse clearly instead.
    if meta.is_dir() {
        return Err(format!(
            "{} is a folder — drop or upload its files individually.",
            host_path
        ));
    }
    if meta.len() > MAX_UPLOAD_BYTES {
        return Err(format!(
            "File too large to upload ({:.0} MB; limit {} MB). Mount it into the project instead.",
            meta.len() as f64 / (1024.0 * 1024.0),
            MAX_UPLOAD_BYTES / (1024 * 1024)
        ));
    }

    let dest = join_path(&container_dir, &file_name);

    // Own the file as the container user and keep the host's mtime. A default
    // tar header would land it root:root with a 1970-01-01 timestamp — i.e.
    // not editable by Claude Code, and misleading in the listing.
    let (uid, gid) = container_user_ids(container_id).await;
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or_else(now_epoch_secs);

    // Reading is a second open of a path that was resolved a moment ago, so the
    // descriptor is checked before its bytes are trusted (H4) and the size
    // ceiling is applied to *it* rather than to the `metadata` call above,
    // which described whatever the path meant at the time. `std::fs::read` plus
    // the tar build are synchronous and can be hundreds of MB, so they run on a
    // blocking thread rather than stalling an async worker (the same discipline
    // as `upload_host_file_to_container`).
    let read_path = host_path.clone();
    let tar_name = file_name.clone();
    let tar_buf = tokio::task::spawn_blocking(move || -> Result<Vec<u8>, String> {
        let file = std::fs::File::open(&read_path)
            .map_err(|e| format!("Failed to read host file: {}", e))?;
        verify_opened_path(&file, Path::new(&read_path))?;
        let mut file_data = Vec::new();
        std::io::Read::read_to_end(
            &mut std::io::Read::take(file, MAX_UPLOAD_BYTES.saturating_add(1)),
            &mut file_data,
        )
        .map_err(|e| format!("Failed to read host file: {}", e))?;
        if file_data.len() as u64 > MAX_UPLOAD_BYTES {
            return Err(format!(
                "File too large to upload (limit {} MB). Mount it into the project instead.",
                MAX_UPLOAD_BYTES / (1024 * 1024)
            ));
        }
        build_single_file_tar(&tar_name, &file_data[..], 0o644, uid, gid, mtime)
    })
    .await
    .map_err(|e| format!("Upload task panicked: {}", e))??;

    // Nothing in this stack checked whether the destination already existed:
    // there was no probe, and `noOverwriteDirNonDir` only stops a directory
    // being replaced by a non-directory (and vice versa) — Docker's extractor
    // overwrites a file with a file quite happily. So dragging a host
    // `.credentials.json` onto the folder holding the container's one destroyed
    // it with no prompt and no undo, while `create_container_directory`
    // deliberately omits `-p` and `rename_container_path` refuses an existing
    // destination. Silence here was an inconsistency, not a policy: refuse by
    // default, and say so in the words the frontend turns into a Replace/Skip
    // prompt.
    //
    // The refusal has to be the *creation*, not a probe before it. A `test -e`
    // and then an upload is two operations with a gap in between, and the file
    // the gap is about is `.credentials.json` — written by Claude Code, inside
    // the container, at a moment nobody controls.
    // [`UPLOAD_RESERVATION_SCRIPT`] closes that gap with a single `link(2)`,
    // which claims the name atomically and — unlike the `set -C` redirect it
    // replaced — cannot be made to block on what is already there.
    let reserved = if overwrite.unwrap_or(false) {
        false
    } else {
        reserve_upload_destination(container_id, &dest).await?;
        true
    };

    let uploaded = docker
        .upload_to_container(
            container_id,
            Some(UploadToContainerOptions {
                path: container_dir,
                // Not the race-closer this comment used to claim it was: all
                // Docker refuses here is a directory being replaced by a file
                // and vice versa. It stays because that is worth refusing —
                // the reservation above is what makes a file-over-file upload
                // wait for an answer.
                no_overwrite_dir_non_dir: "true".to_string(),
            }),
            tar_buf.into(),
        )
        .await;

    if let Err(e) = uploaded {
        if reserved {
            // Leaving a 0-byte placeholder where the user had nothing would be
            // a worse outcome than the failed upload, and it would make the
            // next attempt look like a collision. But an unconditional `rm -f`
            // deletes more than that: the reservation succeeding means the
            // destination did **not** exist, so anything at that path now was
            // written in the window since — by Docker's extractor, or by
            // something inside the container — and under `/workspace/…` that
            // is a file on the host. So only the placeholder as we left it, an
            // empty regular file, is removed. Anything with bytes in it is
            // somebody's, and the failed upload is reported without also
            // destroying it.
            let _ = exec_oneshot_as(
                container_id,
                "claude",
                vec![
                    "sh".to_string(),
                    "-c".to_string(),
                    "[ -f \"$0\" ] && [ ! -s \"$0\" ] && [ ! -L \"$0\" ] && rm -f -- \"$0\""
                        .to_string(),
                    dest.clone(),
                ],
                Vec::new(),
            )
            .await;
        }
        return Err(format!("Failed to upload file to container: {}", e));
    }

    Ok(())
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
        assert_eq!(argv[1], "/workspace");
        let format = argv.last().unwrap();
        assert!(format.ends_with("%f\\0"), "{}", format);
        assert!(!format.contains('\0'));
        assert!(!format.contains('\n'));
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
        assert!(MAX_READ_BYTES < MAX_UPLOAD_BYTES);
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

    // ── Downloads ───────────────────────────────────────────────────────────

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

    #[test]
    fn the_download_ceiling_is_checked_against_the_declared_size() {
        // The bug this guards: the download path passed `None` for the cap, so
        // a 40 GB (sparse, near-free in the container) file was buffered whole
        // in host RAM — twice.
        assert!(check_download_size(MAX_DOWNLOAD_BYTES).is_ok());
        let err = check_download_size(40 * 1024 * 1024 * 1024).unwrap_err();
        assert!(err.contains("40.0 GB"), "{}", err);
        // A ceiling with no way forward is the one thing a ceiling must not be.
        assert!(err.contains("Backup"), "{}", err);
    }

    #[test]
    fn every_buffering_read_has_to_name_a_ceiling() {
        // `fetch_container_file` takes a plain `u64` now, so the `None` that
        // made the cap inert cannot be written again. These are the two callers
        // left, and both buffer.
        assert!(MAX_READ_BYTES < MAX_DOWNLOAD_BYTES);
    }

    #[test]
    fn an_upload_collision_is_reported_so_the_ui_can_offer_to_overwrite() {
        // H5: Docker's extractor overwrites a file with a file silently, and
        // dropping a `.credentials.json` onto the folder holding one was
        // irrecoverable. The prefix is what lets the frontend tell this refusal
        // apart from a real failure.
        let err = upload_exists_error("/home/claude/.claude/.credentials.json");
        // The token and the full path are a contract with
        // `app/src/lib/uploadErrors.ts`, which turns this into the prompt.
        assert!(err.contains(UPLOAD_EXISTS_MARKER), "{}", err);
        assert_eq!(
            err,
            "FILE_EXISTS: /home/claude/.claude/.credentials.json already exists"
        );
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
    async fn a_download_through_a_symlinked_component_writes_nothing_anywhere() {
        // The whole pipeline the command runs, with the container half stubbed
        // out: if the destination is refused, `fill` is never called, so there
        // is no payload to land anywhere.
        let (root, downloads, secret) = plant_symlinked_downloads();
        let evil = downloads
            .join("pub")
            .join("authorized_keys")
            .to_string_lossy()
            .to_string();

        let called = Arc::new(AtomicBool::new(false));
        let saw = Arc::clone(&called);
        let result = save_to_host(&evil, move |partial, created| async move {
            saw.store(true, Ordering::SeqCst);
            std::fs::write(&partial, b"container-controlled bytes").unwrap();
            created.store(true, Ordering::SeqCst);
            Ok(26)
        })
        .await;

        assert!(result.is_err(), "the write was allowed through");
        assert!(!called.load(Ordering::SeqCst), "the transfer started anyway");
        assert_eq!(
            std::fs::read(&secret).unwrap(),
            b"the key that was already there"
        );
        // Nothing new in the protected directory either — no partial, no key.
        let names: Vec<String> = std::fs::read_dir(secret.parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
            .collect();
        assert_eq!(names, vec!["authorized_keys".to_string()]);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_download_to_an_ordinary_location_still_arrives() {
        // The other half of the proof: the check has to refuse the escape
        // without refusing the feature.
        let (root, downloads, _secret) = plant_symlinked_downloads();
        let good = downloads.join("report.txt").to_string_lossy().to_string();

        let (dest, written) = save_to_host(&good, |partial, created| async move {
            std::fs::write(&partial, b"a perfectly ordinary file").unwrap();
            created.store(true, Ordering::SeqCst);
            Ok(25)
        })
        .await
        .unwrap();

        assert_eq!(written, 25);
        assert_eq!(dest, downloads.join("report.txt"));
        assert_eq!(std::fs::read(&dest).unwrap(), b"a perfectly ordinary file");
        // And the staging file is gone, not left beside it.
        let names: Vec<String> = std::fs::read_dir(&downloads)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
            .filter(|n| n.contains("triple-c-part"))
            .collect();
        assert!(names.is_empty(), "{:?}", names);

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

    // ── H6: what a path resolves *through* is not the policy question ───────

    #[cfg(unix)]
    #[tokio::test]
    async fn a_dependency_reached_through_pnpms_store_can_be_uploaded() {
        // The regression, verbatim. pnpm keeps every package under
        // `node_modules/.pnpm/<pkg>@<ver>/node_modules/<pkg>` and links the
        // visible `node_modules/<pkg>` at it, so canonicalising an ordinary
        // `node_modules/left-pad/index.js` produces a path with `.pnpm` in it —
        // and re-running the hidden-component rule over that answer refused the
        // upload. Nothing about this is anybody's attack, and nothing about it
        // was refused before resolution started.
        let root = std::env::temp_dir()
            .canonicalize()
            .unwrap()
            .join(format!("tc-h6-pnpm-{}", uuid::Uuid::new_v4()));
        let modules = root.join("proj/node_modules");
        let store = modules.join(".pnpm/left-pad@1.3.0/node_modules/left-pad");
        std::fs::create_dir_all(&store).unwrap();
        std::fs::write(store.join("index.js"), b"module.exports = 1").unwrap();
        std::os::unix::fs::symlink(&store, modules.join("left-pad")).unwrap();

        let visible = modules.join("left-pad/index.js").to_string_lossy().to_string();
        let resolved = resolve_host_read_path(&visible)
            .await
            .unwrap_or_else(|e| panic!("pnpm dependency refused: {}", e));
        assert_eq!(resolved, store.join("index.js").to_string_lossy());
        // …and the name it lands under in the container is the one the user
        // chose, not the store's.
        assert_eq!(host_upload_name(&visible).unwrap(), "index.js");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_visible_directory_that_lives_under_a_dot_directory_still_works() {
        // The other half of the same regression: `~/.local/share`, `~/.cache`,
        // `~/.var/app` (Flatpak), `~/.nvm` and `~/.cargo` are where whole
        // ecosystems put the files a visible directory points at. Every
        // download into one of these was refused, and every upload out of one.
        let root = std::env::temp_dir()
            .canonicalize()
            .unwrap()
            .join(format!("tc-h6-dot-{}", uuid::Uuid::new_v4()));
        for tail in [".local/share/notes", ".cache/exports", ".var/app/org.x/data", ".cargo/registry"] {
            let real = root.join(tail);
            std::fs::create_dir_all(&real).unwrap();
            let visible = root.join(tail.replace('/', "-").trim_start_matches('.'));
            std::os::unix::fs::symlink(&real, &visible).unwrap();

            // A download into it.
            let dest = visible.join("report.pdf").to_string_lossy().to_string();
            let saved = resolve_host_path(&dest, HostPathUse::Write)
                .await
                .unwrap_or_else(|e| panic!("save into {} refused: {}", tail, e));
            assert_eq!(saved, real.join("report.pdf"));

            // …and an upload out of it.
            std::fs::write(real.join("notes.md"), b"hello").unwrap();
            let source = visible.join("notes.md").to_string_lossy().to_string();
            resolve_host_read_path(&source)
                .await
                .unwrap_or_else(|e| panic!("upload from {} refused: {}", tail, e));
        }

        let _ = std::fs::remove_dir_all(&root);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn the_credential_directories_are_still_refused_through_a_visible_link() {
        // The escape H4 was added for, and the thing H6's fix must not reopen:
        // the hidden-component rule stops judging where a path *resolves*, so
        // what refuses `Downloads/pub → ~/.ssh` is now the credential list,
        // which is about the destination rather than about the spelling.
        let root = std::env::temp_dir()
            .canonicalize()
            .unwrap()
            .join(format!("tc-h6-cred-{}", uuid::Uuid::new_v4()));
        let home = root.join("home");
        let downloads = home.join("Downloads");
        std::fs::create_dir_all(&downloads).unwrap();

        for (dir, leaf) in [
            (".ssh", "authorized_keys"),
            (".gnupg", "trustdb.gpg"),
            (".aws", "credentials"),
            (".config/autostart", "x.desktop"),
            (".config/gcloud", "credentials.db"),
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
            assert!(err.contains(dir), "{} → {}", dir, err);
            // Both directions: reading the secret out is the same bypass
            // backwards.
            assert!(resolve_host_path(&evil, HostPathUse::Read).await.is_err(), "{}", dir);
            assert_eq!(
                std::fs::read(real.join(leaf)).unwrap(),
                b"the secret that was already there"
            );
        }

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_resolved_policy_asks_where_the_bytes_land_not_how_it_is_spelled() {
        // The pure half, so the two questions can be seen apart. An ordinary
        // dot directory is not a policy event once resolution has happened…
        for path in [
            "/home/jo/proj/node_modules/.pnpm/left-pad@1.3.0/node_modules/left-pad/index.js",
            "/home/jo/.local/share/notes/report.pdf",
            "/home/jo/.cache/exports/report.pdf",
            "/home/jo/.var/app/org.x/data/report.pdf",
            "/home/jo/.nvm/versions/node/v22.0.0/bin/x",
            "/home/jo/.cargo/registry/src/a/b.rs",
        ] {
            assert!(validate_resolved_host_path(path, HostPathUse::Write).is_ok(), "{}", path);
            assert!(validate_resolved_host_path(path, HostPathUse::Read).is_ok(), "{}", path);
        }

        // …while the directories that hold authority are, in both directions.
        for path in [
            "/home/jo/.ssh/authorized_keys",
            "/home/jo/.ssh/keys/id_rsa",
            "/home/jo/.gnupg/trustdb.gpg",
            "/home/jo/.aws/credentials",
            "/home/jo/.config/autostart/x.desktop",
            "/home/jo/.config/gcloud/credentials.db",
            "/home/jo/.claude/.credentials.json",
            "/home/jo/.local/share/keyrings/login.keyring",
        ] {
            assert!(validate_resolved_host_path(path, HostPathUse::Write).is_err(), "{}", path);
            assert!(validate_resolved_host_path(path, HostPathUse::Read).is_err(), "{}", path);
        }

        // A *file* called `.aws` is not the credential store — the directory
        // is — and a project's own `.env` has to keep being uploadable.
        assert!(validate_resolved_host_path("/home/jo/proj/.env", HostPathUse::Read).is_ok());
        // The location rules that only a resolved path can answer are still here.
        assert!(validate_resolved_host_path("/private/etc/hosts", HostPathUse::Write).is_err());
        assert!(validate_resolved_host_path("/var/home/jo/x.pdf", HostPathUse::Write).is_ok());
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
    async fn a_failed_download_never_deletes_a_file_it_did_not_create() {
        // The partial's name carries 32 bits of UUID, so colliding with an
        // existing file is vanishingly unlikely — and "vanishingly unlikely" is
        // not a reason to delete somebody's file. The cleanup runs only when
        // the transfer actually created one.
        let dir = std::env::temp_dir().join(format!("tc-part-{}", uuid::Uuid::new_v4()));
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let dest = dir.join("report.pdf");

        let err = save_to_host(&dest.to_string_lossy(), |partial, _created| async move {
            // Stands in for the collision: something is already at the partial's
            // name, so `create_new` fails and nothing here is ours.
            std::fs::write(&partial, b"someone else's file").unwrap();
            Err("Failed to create the partial file".to_string())
        })
        .await
        .unwrap_err();
        assert!(err.contains("Failed to create"), "{}", err);

        let survivors: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
            .collect();
        assert_eq!(survivors.len(), 1, "{:?}", survivors);
        assert!(survivors[0].contains("triple-c-part"), "{:?}", survivors);

        let _ = tokio::fs::remove_dir_all(&dir).await;
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

    // ── Uploads ─────────────────────────────────────────────────────────────

    #[test]
    fn the_upload_reservation_is_one_exclusive_create_not_a_check_and_then_a_write() {
        // What the old comment claimed: `no_overwrite_dir_non_dir` "closes the
        // race" between the `test -e` probe and the extraction. It does not —
        // Docker refuses only a directory replaced by a non-directory and the
        // reverse, and file-over-file extraction proceeds, which is exactly the
        // `.credentials.json` case the guard exists for. There was no test of
        // any of it: the only occurrence of that name in the repository was the
        // bollard field itself. This is that test, against the thing that
        // actually closes the window.
        let staging = upload_reservation_staging_path("/workspace/notes.txt");
        let argv = upload_reservation_argv("/workspace/notes.txt", &staging);
        assert_eq!(argv[0], "sh");
        assert_eq!(argv[1], "-c");
        // Both paths are arguments read back as `$0`/`$1`, never part of the
        // script — so nothing in either is ever parsed as shell.
        assert_eq!(argv[3], "/workspace/notes.txt");
        assert_eq!(argv[4], staging);
        assert!(!argv[2].contains("/workspace"), "{}", argv[2]);

        // So a path that would be an injection anywhere else changes nothing
        // about what the shell is asked to run.
        let hostile_dest = "/workspace/$(touch pwned)`id`; rm -rf ~";
        let hostile = upload_reservation_argv(hostile_dest, &staging);
        assert_eq!(hostile[2], argv[2]);
        assert_eq!(hostile[3], hostile_dest);
        assert_eq!(hostile.len(), 5);
    }

    #[test]
    fn the_reservation_claims_a_name_with_link_rather_than_a_redirect() {
        // H8. `set -C; : > "$0"` is not `O_EXCL` for a destination that is not
        // a regular file: noclobber refuses an existing *file*, and against a
        // FIFO the shell simply opens it and blocks in `open(2)` until a reader
        // appears. With no ceiling on the exec, the upload command never
        // returned. `link(2)` is the primitive that was actually wanted — it
        // creates a name, it never opens anything, and `EEXIST` is immediate
        // whatever kind of thing is in the way.
        let script = UPLOAD_RESERVATION_SCRIPT;
        assert!(script.contains("ln -- \"$1\" \"$0\""), "{}", script);
        // The redirect that is left is onto the *staging* name, which carries
        // 32 random bits and therefore cannot be a planted FIFO.
        assert!(!script.contains("> \"$0\""), "{}", script);
        assert!(script.contains(": > \"$1\""), "{}", script);
        // The staging file is cleaned up on every path out, signals included.
        assert!(script.contains("trap 'rm -f -- \"$1\" 2>/dev/null' EXIT"), "{}", script);
        // A dangling symlink is a collision, not a dead end: `link(2)` does not
        // follow the new-path link, so `[ -e ]` alone would say "not there".
        assert!(script.contains("[ -L \"$0\" ]"), "{}", script);
        // One dedicated status for "taken", so no second probe has to guess.
        assert!(script.contains(&format!("exit {}", UPLOAD_RESERVATION_TAKEN)), "{}", script);
    }

    #[test]
    fn the_reservation_stages_beside_its_destination_under_an_unguessable_name() {
        // `link(2)` cannot cross a filesystem, so the staging file has to be in
        // the destination's own directory — and it has to be a name nothing can
        // have pre-planted, because the one open left in the script is on it.
        let staging = upload_reservation_staging_path("/workspace/app/notes.txt");
        assert_eq!(parent_dir(&staging), "/workspace/app");
        assert!(staging.contains("triple-c-reserve-"), "{}", staging);
        assert_ne!(
            upload_reservation_staging_path("/workspace/app/notes.txt"),
            staging
        );
        // A one-component destination still stages somewhere valid.
        assert_eq!(parent_dir(&upload_reservation_staging_path("/notes.txt")), "/");
    }

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

    #[test]
    fn the_collision_refusal_matches_the_shape_the_frontend_parses() {
        // `app/src/lib/uploadErrors.ts` matches the marker as a standalone
        // upper-case token followed by `:` — so that an unrelated failure
        // quoting a file called `FILE_EXISTS.txt` is not read as a collision
        // and answered with an overwrite.
        let err = upload_exists_error("/home/claude/.claude/.credentials.json");
        assert!(err.starts_with("FILE_EXISTS:"), "{}", err);
        assert_eq!(UPLOAD_EXISTS_MARKER, "FILE_EXISTS");
        assert_eq!(
            err,
            "FILE_EXISTS: /home/claude/.claude/.credentials.json already exists"
        );
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

    /// The whole of H4 against a real daemon: plant the symlink, ask for the
    /// download, show it is refused, then show an ordinary download still
    /// arrives byte for byte. Also drives the container-side write-root
    /// resolution, which cannot be tested any other way.
    ///
    /// Ignored because it needs Docker and pulls a container up; run it with
    ///
    /// ```text
    /// cargo test -- --ignored --nocapture symlink_escape
    /// ```
    #[cfg(unix)]
    #[tokio::test]
    #[ignore = "needs a Docker daemon; creates and removes a throwaway container"]
    async fn the_symlink_escape_is_closed_against_a_live_container() {
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

        let name = format!("tc-h4-live-{}", &uuid::Uuid::new_v4().simple().to_string()[..8]);
        docker_cli(&["run", "-d", "--name", &name, "ubuntu:24.04", "sleep", "300"]);
        docker_cli(&["exec", &name, "useradd", "-m", "claude"]);
        docker_cli(&[
            "exec",
            &name,
            "sh",
            "-c",
            "mkdir -p /workspace && printf 'the payload' > /workspace/report.txt && ln -s /etc /workspace/escape && chown -R claude /workspace",
        ]);

        let (root, downloads, secret) = plant_symlinked_downloads();
        let evil = downloads
            .join("pub")
            .join("authorized_keys")
            .to_string_lossy()
            .to_string();
        let good = downloads.join("report.txt").to_string_lossy().to_string();

        let cid = name.clone();
        let refused = save_to_host(&evil, move |partial, created| async move {
            stream_container_file_to_host(&cid, "/workspace/report.txt", &partial, created).await
        })
        .await;
        println!("refused: {:?}", refused);
        assert!(refused.is_err());
        assert_eq!(
            std::fs::read(&secret).unwrap(),
            b"the key that was already there"
        );

        let cid = name.clone();
        let (dest, written) = save_to_host(&good, move |partial, created| async move {
            stream_container_file_to_host(&cid, "/workspace/report.txt", &partial, created).await
        })
        .await
        .expect("an ordinary download");
        println!("saved {} bytes to {}", written, dest.display());
        assert_eq!(std::fs::read(&dest).unwrap(), b"the payload");

        // Container side: `/workspace/escape` is under a write root as a
        // string and is `/etc` as a location.
        let escaped = resolve_container_dir(&name, "Folder", "/workspace/escape").await;
        println!("container escape: {:?}", escaped);
        assert!(escaped.is_err(), "a symlink out of /workspace was accepted");
        resolve_container_dir(&name, "Folder", "/workspace")
            .await
            .expect("/workspace itself");

        let _ = std::fs::remove_dir_all(&root);
        docker_cli(&["rm", "-f", &name]);
    }

    /// The claim the old comment made, checked against a real daemon: Docker's
    /// `noOverwriteDirNonDir` does **not** stop a file replacing a file, and the
    /// reservation does.
    ///
    /// Ignored for the same reason as the test above; run it with
    ///
    /// ```text
    /// cargo test -- --ignored --nocapture overwrites_a_file
    /// ```
    #[tokio::test]
    #[ignore = "needs a Docker daemon; creates and removes a throwaway container"]
    async fn docker_overwrites_a_file_with_a_file_and_the_reservation_is_what_refuses() {
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

        let name = format!("tc-h5-live-{}", &uuid::Uuid::new_v4().simple().to_string()[..8]);
        docker_cli(&["run", "-d", "--name", &name, "ubuntu:24.04", "sleep", "300"]);
        docker_cli(&["exec", &name, "useradd", "-m", "claude"]);
        docker_cli(&[
            "exec",
            &name,
            "sh",
            "-c",
            "mkdir -p /workspace && printf 'the original' > /workspace/keep.txt && chown -R claude /workspace",
        ]);

        // 1. The flag the comment leaned on, exercised exactly as the upload
        //    command sets it.
        let tar = build_single_file_tar("keep.txt", b"replaced", 0o644, 0, 0, now_epoch_secs()).unwrap();
        get_docker()
            .unwrap()
            .upload_to_container(
                &name,
                Some(UploadToContainerOptions {
                    path: "/workspace".to_string(),
                    no_overwrite_dir_non_dir: "true".to_string(),
                }),
                tar.into(),
            )
            .await
            .expect("docker accepted the upload");
        let after = docker_cli(&["exec", &name, "cat", "/workspace/keep.txt"]);
        println!("after a file-over-file upload with noOverwriteDirNonDir: {:?}", after);
        assert_eq!(after, "replaced", "Docker refused it after all — check the comment");

        // 2. What actually refuses: one exclusive create.
        let taken = reserve_upload_destination(&name, "/workspace/keep.txt").await;
        println!("reservation over an existing file: {:?}", taken);
        let err = taken.unwrap_err();
        assert!(err.starts_with("FILE_EXISTS:"), "{}", err);
        assert!(err.contains("/workspace/keep.txt"), "{}", err);
        // …and the file it refused to touch is untouched.
        assert_eq!(docker_cli(&["exec", &name, "cat", "/workspace/keep.txt"]), "replaced");

        // 3. A free name is claimed, once.
        reserve_upload_destination(&name, "/workspace/fresh.txt")
            .await
            .expect("a free name");
        assert_eq!(docker_cli(&["exec", &name, "stat", "-c", "%s", "/workspace/fresh.txt"]), "0");
        let again = reserve_upload_destination(&name, "/workspace/fresh.txt").await;
        println!("reservation of the same name again: {:?}", again);
        assert!(again.unwrap_err().starts_with("FILE_EXISTS:"));

        // 4. A destination the container user cannot create is not reported as
        //    a collision — the frontend would offer a Replace that cannot work.
        let denied = reserve_upload_destination(&name, "/root/nope.txt").await;
        println!("reservation somewhere unwritable: {:?}", denied);
        let err = denied.unwrap_err();
        assert!(!err.starts_with("FILE_EXISTS:"), "{}", err);

        // 5. H8. A FIFO at the destination is what hung the app: `set -C` is
        //    not `O_EXCL` for a non-regular file, so the redirect blocked in
        //    `open(2)` waiting for a reader and never came back. `link(2)`
        //    answers `EEXIST` immediately, so this must finish in well under
        //    the reservation's own ceiling.
        docker_cli(&[
            "exec",
            &name,
            "sh",
            "-c",
            "mkfifo /workspace/apipe && ln -s /workspace/nowhere /workspace/dangle && chown -h claude /workspace/apipe /workspace/dangle",
        ]);
        let started = std::time::Instant::now();
        let fifo = reserve_upload_destination(&name, "/workspace/apipe").await;
        let took = started.elapsed();
        println!("reservation over a FIFO: {:?} in {:?}", fifo, took);
        assert!(took < std::time::Duration::from_secs(5), "it blocked: {:?}", took);
        assert!(fifo.unwrap_err().starts_with("FILE_EXISTS:"));
        // …and the FIFO is still a FIFO, not something we opened.
        assert_eq!(docker_cli(&["exec", &name, "stat", "-c", "%F", "/workspace/apipe"]), "fifo");

        // 6. A **dangling** symlink used to be a permanent dead end: the
        //    reservation refused (it sees the link), the confirming `test -e`
        //    followed it and said no, so raw shell text came back and the
        //    frontend never offered Replace.
        let dangling = reserve_upload_destination(&name, "/workspace/dangle").await;
        println!("reservation over a dangling symlink: {:?}", dangling);
        assert!(dangling.unwrap_err().starts_with("FILE_EXISTS:"));

        // 7. Nothing is left in the directory but what belongs there — the
        //    staging file is removed on every path out.
        let listing = docker_cli(&["exec", &name, "ls", "-a", "/workspace"]);
        println!("/workspace afterwards:\n{}", listing);
        assert!(!listing.contains("triple-c-reserve"), "{}", listing);

        docker_cli(&["rm", "-f", &name]);
    }
}
