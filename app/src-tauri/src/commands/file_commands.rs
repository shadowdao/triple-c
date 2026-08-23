use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, SystemTime};

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
use bollard::container::{DownloadFromContainerOptions, LogOutput, UploadToContainerOptions};
use bollard::exec::{CreateExecOptions, StartExecResults};
use futures_util::StreamExt;
use serde::Serialize;
use tauri::{AppHandle, Manager, State};

use crate::docker::client::get_docker;
use crate::docker::exec::{
    build_single_file_tar, container_user_ids, exec_oneshot_as, now_epoch_secs,
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
    let (output, code) =
        exec_oneshot_as(container_id, "claude", list_argv(&path), Vec::new()).await?;

    let entries = parse_find_output(&path, &output);
    if code != 0 && entries.is_empty() {
        // `find`'s own words — "Permission denied", "No such file or directory"
        // — are the whole diagnosis, and stderr is merged into `output`.
        let detail = output.trim();
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
/// lowercasing and mapping `\` to `/`, which is what makes the Windows entries
/// work.
const HOST_SYSTEM_ROOTS: &[&str] = &[
    "/bin", "/boot", "/dev", "/etc", "/lib", "/lib32", "/lib64", "/libx32", "/proc", "/root",
    "/sbin", "/sys", "/usr", "/var",
    // macOS keeps its own copies of the same idea.
    "/system", "/library",
    // Windows.
    "c:/windows", "c:/program files", "c:/program files (x86)", "c:/programdata",
];

/// Validate a host path that arrived over IPC, returning it as a [`PathBuf`].
///
/// The `save()`/`open()` dialog the Files pane puts in front of these commands
/// is a UI convention, not a boundary — every one of them is a single `invoke`
/// away from any code running in the webview, with a container-controlled
/// payload on one side. So the backend has its own policy, and it is deliberately
/// blunt:
///
///   * absolute, no `..`, no NUL — the same structural rules as a container
///     path, using [`Path::components`] so a Windows path is judged as one;
///   * nothing under [`HOST_SYSTEM_ROOTS`];
///   * no *hidden* path components. This is the rule that matters. The
///     interesting targets for "write a container-controlled file to an
///     arbitrary host path" are all dot directories — `~/.ssh/authorized_keys`,
///     `~/.config/autostart/`, `~/.claude/` — and the interesting targets for
///     the reverse, reading a host file into the container, are the same ones
///     plus `~/.aws/credentials`. A download is refused a hidden *name* too
///     (creating `~/.bashrc` is escape all by itself); an upload only cares
///     about hidden *directories*, because dragging a project's own `.env` into
///     the container is an ordinary thing to do and its parent is not hidden.
///
/// What it costs: saving a container file to a hidden host location now has to
/// go somewhere visible first. That is a small, explainable price for closing a
/// container→host write primitive.
fn validate_host_path(path: &str, use_for: HostPathUse) -> Result<PathBuf, String> {
    use std::path::Component;

    if path.trim().is_empty() {
        return Err("No host path was given".to_string());
    }
    if path.contains('\0') {
        return Err("Host path cannot contain a null byte".to_string());
    }

    let candidate = PathBuf::from(path);
    if !candidate.is_absolute() {
        return Err(format!("Host path must be absolute: {}", path));
    }

    let components: Vec<Component> = candidate.components().collect();
    if components.iter().any(|c| matches!(c, Component::ParentDir)) {
        return Err(format!("Host path cannot contain \"..\": {}", path));
    }

    // The final component is the file itself; everything before it is a
    // directory the path passes *through*.
    let names: Vec<String> = components
        .iter()
        .filter_map(|c| match c {
            Component::Normal(s) => Some(s.to_string_lossy().to_string()),
            _ => None,
        })
        .collect();
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

    let normalized = path.replace('\\', "/").to_lowercase();
    if let Some(root) = HOST_SYSTEM_ROOTS
        .iter()
        .find(|root| is_under_root(&normalized, root))
    {
        return Err(format!(
            "{} is a system location — Triple-C will not {} files there.",
            root,
            if use_for == HostPathUse::Write { "write" } else { "read" }
        ));
    }

    Ok(candidate)
}

/// [`validate_host_path`] for a host file about to be read into a container,
/// handed back as a `String`.
///
/// Public because the terminal's drag-and-drop drop target
/// (`terminal_commands::upload_host_file_to_terminal`) is the same primitive as
/// the Files pane's upload and must not have a different policy.
pub fn validate_host_read_path(path: &str) -> Result<String, String> {
    Ok(validate_host_path(path, HostPathUse::Read)?
        .to_string_lossy()
        .to_string())
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

#[tauri::command]
pub async fn download_container_file(
    project_id: String,
    container_path: String,
    host_path: String,
    state: State<'_, AppState>,
) -> Result<(), String> {
    validate_container_path("File", &container_path)?;
    let dest = validate_host_path(&host_path, HostPathUse::Write)?;

    let project = state
        .projects_store
        .get(&project_id)
        .ok_or_else(|| format!("Project {} not found", project_id))?;

    let container_id = project
        .container_id
        .as_ref()
        .ok_or_else(|| "Container not running".to_string())?;

    // Written beside the destination and renamed on success, so a failure
    // anywhere below leaves whatever was already at `dest` untouched.
    let partial = partial_download_path(&dest)?;

    let streamed = stream_container_file_to_host(container_id, &container_path, &partial).await;

    match streamed {
        Ok(written) => {
            if let Err(e) = finish_download(&partial, &dest).await {
                let _ = tokio::fs::remove_file(&partial).await;
                return Err(e);
            }
            log::info!(
                "Saved {} bytes from {} to {}",
                written,
                container_path,
                dest.display()
            );
            Ok(())
        }
        Err(e) => {
            // Only ever our own partial file — never the user's destination.
            let _ = tokio::fs::remove_file(&partial).await;
            Err(e)
        }
    }
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

// ─────────────────────────────────────────────────────────────────────────────
// Drag-out staging
// ─────────────────────────────────────────────────────────────────────────────
//
// Dragging a file onto the host desktop hands the OS a *host* path, and the
// files in this panel live inside a container, where nothing on the desktop can
// reach them. So a drag-out is really a copy-then-drag: materialise the file
// into a host temp directory first, then start the native drag on that copy.
//
// The copy is the reason this section carries a lifecycle. A staging directory
// nobody empties is a disk leak with a gesture attached to it, so there are two
// halves and both matter: `clear_drag_staging` on exit, and
// `reap_drag_staging` at startup for whatever a crash left behind.

/// Ceiling on one staged copy. Deliberately the same 256 MiB as
/// [`MAX_UPLOAD_BYTES`] — it is the same whole-file-through-host-RAM round trip,
/// only in the other direction.
const MAX_DRAG_STAGE_BYTES: u64 = 256 * 1024 * 1024;

/// Name of the app-owned directory inside the OS temp dir. Everything staged by
/// any Triple-C process lives under it, so housekeeping has exactly one place to
/// look and never walks the rest of the user's temp dir.
const DRAG_STAGE_DIR_NAME: &str = "triple-c-drag-out";

/// How long *another* process's leftover staging directory may sit before
/// startup housekeeping deletes it.
///
/// Only ever applied to directories this process does not own (see
/// [`drag_stage_session_dir`]), so it is not a limit on how long a staged file
/// survives in a live session — it is the crash-recovery threshold, and it is
/// generous because a second Triple-C running right now would also look like a
/// leftover.
const DRAG_STAGE_MAX_AGE: Duration = Duration::from_secs(24 * 60 * 60);

/// This process's own sub-directory name, stable for the life of the process.
///
/// Per-process rather than shared so exit cleanup can delete *ours* outright
/// without reaching into a directory another instance may be dragging out of.
fn drag_stage_session() -> &'static str {
    static SESSION: OnceLock<String> = OnceLock::new();
    SESSION.get_or_init(|| uuid::Uuid::new_v4().to_string())
}

/// The app-owned staging root inside `temp_dir`.
///
/// Takes the temp dir rather than reading it, because on Windows it is neither
/// `/tmp` nor a constant — Tauri's path API is the only thing that knows it —
/// and because a pure function is what the tests can drive.
pub fn drag_stage_root(temp_dir: &Path) -> PathBuf {
    temp_dir.join(DRAG_STAGE_DIR_NAME)
}

/// This process's staging directory: `<temp>/triple-c-drag-out/<session>`.
pub fn drag_stage_session_dir(temp_dir: &Path) -> PathBuf {
    drag_stage_root(temp_dir).join(drag_stage_session())
}

/// The per-file sub-directory a staged copy lives in, derived from the
/// container path.
///
/// Filenames are only unique within a directory, so `a/notes.txt` and
/// `b/notes.txt` would otherwise be the same host path — and the second drag
/// would silently rewrite the first one's contents under the first one's cached
/// path. A digest of the full container path separates them while staying
/// *deterministic*, so re-staging the same file reuses its slot instead of
/// growing a new one every drag.
fn drag_stage_slot(container_path: &str) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(container_path.as_bytes());
    digest[..8].iter().map(|b| format!("{:02x}", b)).collect()
}

/// The name the staged copy is given on the host.
///
/// The whole point is that what lands on the desktop is called `notes.txt` and
/// not `tmp1234`, so the container's basename is kept verbatim wherever it can
/// be. Only the characters Windows refuses outright are substituted — a Linux
/// file really can be called `a:b`, and the staged copy has to exist on NTFS.
/// A name that is not a filename at all (empty, `.`, `..`) is rejected rather
/// than invented: that means the caller passed something that never named a
/// file, and quietly inventing a name would stage the wrong thing.
fn stage_file_name(container_path: &str) -> Result<String, String> {
    let base = container_path
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or("");

    let cleaned: String = base
        .chars()
        .map(|c| match c {
            '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*' => '_',
            c if (c as u32) < 0x20 => '_',
            c => c,
        })
        .collect();
    // Windows also silently drops a trailing dot or space, which would make the
    // path we hand back not the path that exists.
    let cleaned = cleaned.trim_end_matches([' ', '.']);

    if cleaned.is_empty() || cleaned == "." || cleaned == ".." {
        return Err(format!("{} does not name a file", container_path));
    }
    Ok(cleaned.to_string())
}

/// Reject an oversize file *by its real size*, before anything is written.
///
/// Split out so the ceiling and its wording are testable without a container.
/// The message names the fallback, because "too large" with no way forward is
/// the one thing a size cap must not be.
fn check_stage_size(size: u64) -> Result<(), String> {
    if size > MAX_DRAG_STAGE_BYTES {
        return Err(format!(
            "{:.0} MB is too large to drag out (limit {} MB) — use \"Save to host…\" instead.",
            size as f64 / (1024.0 * 1024.0),
            MAX_DRAG_STAGE_BYTES / (1024 * 1024)
        ));
    }
    Ok(())
}

/// Whether a leftover staging directory is old enough to delete.
///
/// A modification time in the *future* (a clock step, a copied temp dir) makes
/// `duration_since` fail, and that answers "not stale" — housekeeping deleting
/// something it cannot date is worse than leaving it for the next startup.
fn drag_stage_is_stale(modified: SystemTime, now: SystemTime, max_age: Duration) -> bool {
    now.duration_since(modified)
        .map(|age| age >= max_age)
        .unwrap_or(false)
}

/// Delete every staging directory except this process's own, once it is older
/// than [`DRAG_STAGE_MAX_AGE`]. Called from startup housekeeping.
pub async fn reap_drag_staging(temp_dir: PathBuf) {
    let root = drag_stage_root(&temp_dir);
    let keep = drag_stage_session_dir(&temp_dir);
    let now = SystemTime::now();

    let mut dir = match tokio::fs::read_dir(&root).await {
        Ok(dir) => dir,
        // Nothing staged yet is the normal case, not a problem.
        Err(_) => return,
    };

    let mut reaped = 0usize;
    while let Ok(Some(entry)) = dir.next_entry().await {
        let path = entry.path();
        if path == keep {
            continue;
        }
        let stale = match entry.metadata().await.and_then(|m| m.modified()) {
            Ok(modified) => drag_stage_is_stale(modified, now, DRAG_STAGE_MAX_AGE),
            Err(_) => false,
        };
        if !stale {
            continue;
        }
        if tokio::fs::remove_dir_all(&path).await.is_ok() {
            reaped += 1;
        }
    }

    if reaped > 0 {
        log::info!("Startup housekeeping removed {} stale drag-out staging directory(ies)", reaped);
    }
}

/// Delete this process's staging directory. Called from the shutdown teardown.
pub async fn clear_drag_staging(temp_dir: PathBuf) {
    let dir = drag_stage_session_dir(&temp_dir);
    if let Err(e) = tokio::fs::remove_dir_all(&dir).await {
        if e.kind() != std::io::ErrorKind::NotFound {
            log::warn!("Failed to clear drag-out staging at {}: {}", dir.display(), e);
        }
    }
    // Best effort: leave no empty root behind either. Fails harmlessly while
    // another instance still has a directory in there.
    let _ = tokio::fs::remove_dir(drag_stage_root(&temp_dir)).await;
}

/// Copy a container file onto the host so it can be dragged to the desktop, and
/// return the absolute host path.
///
/// Reuses [`fetch_container_file`] rather than extracting a second way, so a
/// dragged file, a downloaded file and a previewed file are byte-identical and
/// refuse folders and links with the same words. The fetch is capped at
/// [`MAX_DRAG_STAGE_BYTES`], so an oversize file is recognised from the tar
/// header without being pulled across the socket in full.
#[tauri::command]
pub async fn stage_container_file_for_drag(
    app: AppHandle,
    project_id: String,
    path: String,
    state: State<'_, AppState>,
) -> Result<String, String> {
    validate_container_path("File", &path)?;

    let project = state
        .projects_store
        .get(&project_id)
        .ok_or_else(|| format!("Project {} not found", project_id))?;

    let container_id = project
        .container_id
        .as_ref()
        .ok_or_else(|| "Container not running".to_string())?;

    // Before the transfer: a path that cannot become a host filename is not
    // worth a round trip.
    let file_name = stage_file_name(&path)?;

    let fetched = fetch_container_file(container_id, &path, MAX_DRAG_STAGE_BYTES).await?;
    // `size` is the tar entry's, i.e. the file's real size, which is exactly
    // what a truncated fetch does not tell you from `bytes.len()`.
    check_stage_size(fetched.size)?;

    let temp_dir = app
        .path()
        .temp_dir()
        .map_err(|e| format!("No host temporary directory available: {}", e))?;
    let dir = drag_stage_session_dir(&temp_dir).join(drag_stage_slot(&path));
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|e| format!("Failed to create the drag staging directory: {}", e))?;

    let dest = dir.join(&file_name);
    tokio::fs::write(&dest, &fetched.bytes)
        .await
        .map_err(|e| format!("Failed to stage {} on the host: {}", file_name, e))?;

    Ok(dest.to_string_lossy().to_string())
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

    validate_container_write_path("Folder", &parent_path)?;

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
    let dest = validate_host_path(&host_path, HostPathUse::Write)?;

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
    let file = tokio::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&partial)
        .await
        .map_err(|e| format!("Failed to create backup file: {}", e))?;
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
    let host_path = validate_host_read_path(&host_path)?;

    let project = state
        .projects_store
        .get(&project_id)
        .ok_or_else(|| format!("Project {} not found", project_id))?;

    let container_id = project
        .container_id
        .as_ref()
        .ok_or_else(|| "Container not running".to_string())?;

    let docker = get_docker()?;

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

    let file_name = std::path::Path::new(&host_path)
        .file_name()
        .ok_or_else(|| "Invalid file path".to_string())?
        .to_string_lossy()
        .to_string();

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
    let dest = join_path(&container_dir, &file_name);
    if !overwrite.unwrap_or(false) {
        let (_, exists) = exec_oneshot_as(
            container_id,
            "claude",
            vec!["test".to_string(), "-e".to_string(), dest.clone()],
            Vec::new(),
        )
        .await?;
        if exists == 0 {
            return Err(upload_exists_error(&dest));
        }
    }

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

    // `std::fs::read` plus the tar build are synchronous and can be hundreds of
    // MB, so they run on a blocking thread rather than stalling an async worker
    // (the same discipline as `upload_host_file_to_container`).
    let read_path = host_path.clone();
    let tar_buf = tokio::task::spawn_blocking(move || -> Result<Vec<u8>, String> {
        let file_data = std::fs::read(&read_path)
            .map_err(|e| format!("Failed to read host file: {}", e))?;
        build_single_file_tar(&file_name, &file_data[..], 0o644, uid, gid, mtime)
    })
    .await
    .map_err(|e| format!("Upload task panicked: {}", e))??;

    docker
        .upload_to_container(
            container_id,
            Some(UploadToContainerOptions {
                path: container_dir,
                // Belt to the existence check's braces: this is the only thing
                // Docker itself will refuse, and it closes the race between the
                // `test -e` above and the extraction.
                no_overwrite_dir_non_dir: "true".to_string(),
            }),
            tar_buf.into(),
        )
        .await
        .map_err(|e| format!("Failed to upload file to container: {}", e))?;

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
        assert!(MAX_READ_BYTES <= MAX_DRAG_STAGE_BYTES);
        assert!(MAX_DRAG_STAGE_BYTES < MAX_DOWNLOAD_BYTES);
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

    // ── Drag-out staging ────────────────────────────────────────────────────

    #[test]
    fn the_staging_path_is_built_under_the_supplied_temp_dir() {
        // Never `/tmp`: on Windows the temp dir is per-user and nowhere near it,
        // so the whole path has to be derived from what Tauri hands us.
        let temp = Path::new("/somewhere/else");
        let root = drag_stage_root(temp);
        assert_eq!(root, Path::new("/somewhere/else/triple-c-drag-out"));

        let session = drag_stage_session_dir(temp);
        assert_eq!(session.parent(), Some(root.as_path()));
        assert!(session.starts_with(root));
    }

    #[test]
    fn every_call_in_a_process_stages_into_the_same_session_directory() {
        // Exit cleanup deletes this directory by name rather than tracking what
        // it wrote, which only works if the name does not move.
        let temp = Path::new("/tmp-ish");
        assert_eq!(drag_stage_session_dir(temp), drag_stage_session_dir(temp));
        assert_ne!(drag_stage_session_dir(temp), drag_stage_root(temp));
    }

    #[test]
    fn the_staged_copy_keeps_the_original_file_name() {
        // The reason the feature stages into a per-session directory at all: a
        // plain temp file would be dropped onto the desktop called `tmp1234`.
        assert_eq!(stage_file_name("/workspace/notes.txt").unwrap(), "notes.txt");
        assert_eq!(stage_file_name("/workspace/a b/.env").unwrap(), ".env");
        assert_eq!(stage_file_name("report.pdf").unwrap(), "report.pdf");
        assert_eq!(stage_file_name("/workspace/über.md").unwrap(), "über.md");
    }

    #[test]
    fn a_name_windows_cannot_hold_is_substituted_rather_than_dropped() {
        // These are all legal on Linux and all refused by NTFS, and the staged
        // copy has to exist on the host we are dragging onto.
        assert_eq!(stage_file_name("/workspace/a:b.txt").unwrap(), "a_b.txt");
        assert_eq!(stage_file_name("/workspace/q?.log").unwrap(), "q_.log");
        assert_eq!(stage_file_name("/workspace/a\\b").unwrap(), "a_b");
        // A trailing dot or space is not refused, it is silently dropped — so
        // the path we return would not be the path that exists.
        assert_eq!(stage_file_name("/workspace/trailing. ").unwrap(), "trailing");
    }

    #[test]
    fn a_path_that_does_not_name_a_file_is_refused_not_invented() {
        assert!(stage_file_name("/").is_err());
        assert!(stage_file_name("").is_err());
        assert!(stage_file_name("/workspace/..").is_err());
        assert!(stage_file_name("/workspace/.").is_err());
        // Trims down to nothing, which is the same problem one step later.
        assert!(stage_file_name("/workspace/...").is_err());
    }

    #[test]
    fn two_files_with_the_same_name_stage_to_different_places() {
        // Names are unique per directory, not per container — and the second
        // drag would otherwise rewrite the first one's bytes under the path the
        // first one is still cached at.
        assert_ne!(
            drag_stage_slot("/workspace/a/notes.txt"),
            drag_stage_slot("/workspace/b/notes.txt")
        );
    }

    #[test]
    fn re_staging_the_same_file_reuses_its_slot() {
        // Deterministic, so a file dragged repeatedly does not grow a new
        // directory in the host temp dir every time.
        assert_eq!(
            drag_stage_slot("/workspace/notes.txt"),
            drag_stage_slot("/workspace/notes.txt")
        );
        // Short enough to keep the path sane, long enough not to collide.
        assert_eq!(drag_stage_slot("/workspace/notes.txt").len(), 16);
    }

    #[test]
    fn the_drag_size_cap_matches_the_established_ceiling_and_names_the_fallback() {
        assert_eq!(MAX_DRAG_STAGE_BYTES, MAX_UPLOAD_BYTES);
        assert!(check_stage_size(MAX_DRAG_STAGE_BYTES).is_ok());

        let err = check_stage_size(MAX_DRAG_STAGE_BYTES + 1).unwrap_err();
        assert!(err.contains("256 MB"), "{}", err);
        // A size cap with no way forward is the one thing this must not be.
        assert!(err.contains("Save to host"), "{}", err);
    }

    #[test]
    fn the_reaper_only_takes_entries_past_the_age_threshold() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
        let age = Duration::from_secs(3_600);

        assert!(drag_stage_is_stale(now - Duration::from_secs(3_601), now, age));
        assert!(drag_stage_is_stale(now - age, now, age));
        assert!(!drag_stage_is_stale(now - Duration::from_secs(3_599), now, age));
        assert!(!drag_stage_is_stale(now, now, age));
    }

    #[test]
    fn a_future_timestamp_is_left_alone_rather_than_reaped() {
        // A clock step must not turn housekeeping into deletion of something it
        // cannot date.
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
        let age = Duration::from_secs(3_600);
        assert!(!drag_stage_is_stale(now + Duration::from_secs(60), now, age));
    }
}
