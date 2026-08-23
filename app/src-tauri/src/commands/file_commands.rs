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
    build_single_file_tar, container_user_ids, exec_oneshot, exec_oneshot_as, now_epoch_secs,
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
    let project = state
        .projects_store
        .get(&project_id)
        .ok_or_else(|| format!("Project {} not found", project_id))?;

    let container_id = project
        .container_id
        .as_ref()
        .ok_or_else(|| "Container not running".to_string())?;

    // `%y` is the entry's own type, `%Y` the type it *dereferences* to. Both
    // are printed: `%Y` is what decides navigability (a symlinked directory
    // reports `l` under `%y`, which used to make it an unopenable row), while
    // `%y` is the only way left to tell the user it is a link at all. `%Y` is
    // `N` for a broken link and `L` for a loop, neither of which is `d`.
    let cmd = vec![
        "find".to_string(),
        path.clone(),
        "-mindepth".to_string(),
        "1".to_string(),
        "-maxdepth".to_string(),
        "1".to_string(),
        "-printf".to_string(),
        "%f\t%y\t%Y\t%s\t%T@\t%m\n".to_string(),
    ];

    let output = exec_oneshot(container_id, cmd).await?;

    Ok(parse_find_output(&path, &output))
}

/// Turn `find -printf '%f\t%y\t%Y\t%s\t%T@\t%m\n'` output into sorted entries.
///
/// Split out from the command so it can be tested without a container: it is
/// the half where a format change silently mis-types every row.
fn parse_find_output(dir: &str, output: &str) -> Vec<FileEntry> {
    let mut entries: Vec<FileEntry> = output
        .lines()
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| {
            let parts: Vec<&str> = line.split('\t').collect();
            if parts.len() < 6 {
                return None;
            }
            let name = parts[0].to_string();
            let is_symlink = parts[1] == "l";
            let is_directory = parts[2] == "d";
            let size = parts[3].parse::<u64>().unwrap_or(0);
            let modified_epoch = parts[4].parse::<f64>().unwrap_or(0.0);
            let permissions = parts[5].to_string();

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

#[tauri::command]
pub async fn download_container_file(
    project_id: String,
    container_path: String,
    host_path: String,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let project = state
        .projects_store
        .get(&project_id)
        .ok_or_else(|| format!("Project {} not found", project_id))?;

    let container_id = project
        .container_id
        .as_ref()
        .ok_or_else(|| "Container not running".to_string())?;

    let fetched = fetch_container_file(container_id, &container_path, None).await?;

    tokio::fs::write(&host_path, &fetched.bytes)
        .await
        .map_err(|e| format!("Failed to write file to host: {}", e))?;

    Ok(())
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
/// With `max_bytes` set the transfer is abandoned once the cap (plus enough
/// slack for the tar framing) is in hand, so previewing a huge file does not
/// pull the whole thing across the socket.
async fn fetch_container_file(
    container_id: &str,
    container_path: &str,
    max_bytes: Option<u64>,
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
    let stop_after = max_bytes.map(|m| m.saturating_add(TAR_SLACK));

    let mut tar_bytes: Vec<u8> = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| format!("Failed to download file: {}", e))?;
        tar_bytes.extend_from_slice(&chunk);
        if stop_after.is_some_and(|cap| tar_bytes.len() as u64 >= cap) {
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

    let size = entry.header().size().unwrap_or(0);
    let truncated = max_bytes.is_some_and(|cap| size > cap);
    let want = max_bytes.map(|cap| cap.min(size)).unwrap_or(size);

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
    let project = state
        .projects_store
        .get(&project_id)
        .ok_or_else(|| format!("Project {} not found", project_id))?;

    let container_id = project
        .container_id
        .as_ref()
        .ok_or_else(|| "Container not running".to_string())?;

    let cap = max_bytes.unwrap_or(MAX_READ_BYTES).min(MAX_READ_BYTES);
    let fetched = fetch_container_file(container_id, &path, Some(cap)).await?;

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

    let fetched = fetch_container_file(container_id, &path, Some(MAX_DRAG_STAGE_BYTES)).await?;
    // `size` is the tar header's, i.e. the file's real size, which is exactly
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
    let file = tokio::fs::File::create(&host_path)
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
        // Don't leave a partial/corrupt archive behind.
        let _ = tokio::fs::remove_file(&host_path).await;
        return Err(err);
    }

    log::info!(
        "Wrote {} byte backup for project {} to {}",
        total,
        project_id,
        host_path
    );
    Ok(total)
}

#[tauri::command]
pub async fn upload_file_to_container(
    project_id: String,
    host_path: String,
    container_dir: String,
    state: State<'_, AppState>,
) -> Result<(), String> {
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
                ..Default::default()
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

    /// A line as `find -printf '%f\t%y\t%Y\t%s\t%T@\t%m\n'` emits it.
    fn line(name: &str, own: &str, deref: &str, size: &str) -> String {
        format!("{}\t{}\t{}\t{}\t1700000000.0000000000\t644", name, own, deref, size)
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
        .join("\n");
        let entries = parse_find_output("/workspace", &output);
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["src", "alpha", "Zeta"]);
    }

    #[test]
    fn short_and_blank_rows_are_dropped_rather_than_mis_parsed() {
        let output = format!("\n  \nbroken\ttoo\tshort\n{}\n", line("ok", "f", "f", "1"));
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
        let output = "weird\tf\tf\t-\t-\t644";
        let entries = parse_find_output("/workspace", output);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].size, 0);
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
