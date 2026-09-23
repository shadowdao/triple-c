//! IPC for the terminal file viewer. Every command here is gated on the calling
//! window's label and reads its target from the registry — no path, no label, no
//! project id crosses IPC from a viewer window. See spec §6.

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State};

use crate::commands::file_commands::{
    fetch_container_file, not_running_message, require_running, validate_container_write_path, MAX_READ_BYTES,
};
use crate::file_viewer::is_viewer_label;
use crate::file_viewer::poll::{poll_file, ViewerPoll};
use crate::file_viewer::registry::{
    Choice, Location, Reservation, ViewerRegistry, ViewerTarget, ViewerTargetState,
};
use crate::file_viewer::resolve::{candidate_paths, probe_candidates};
use crate::file_viewer::window::open_viewer_window;
use crate::file_viewer::write::{sha256_hex, write_file, SavedFile, MAX_WRITE_BYTES};
use crate::models::Project;
use crate::AppState;

pub const GOTO_EVENT: &str = "file-viewer-goto";

#[derive(Clone, Debug, Serialize)]
pub struct ViewerState {
    pub project_id: String,
    pub project_name: String,
    pub raw_path: String,
    pub state: ViewerTargetState,
    pub initial: Location,
}

#[derive(Clone, Debug, Serialize)]
pub struct ViewerFile {
    pub contents_base64: String,
    pub truncated: bool,
    pub size: u64,
    pub hash: String,
    pub editable: bool,
    pub readonly_reason: Option<String>,
}

fn require_main(window_label: &str) -> Result<(), String> {
    if window_label == "main" {
        Ok(())
    } else {
        Err("Only the main window can open files.".into())
    }
}

fn require_viewer(window_label: &str) -> Result<String, String> {
    if is_viewer_label(window_label) {
        Ok(window_label.to_string())
    } else {
        Err("This command belongs to a file window.".into())
    }
}

fn viewer_state_of(_label: &str, target: ViewerTarget) -> ViewerState {
    ViewerState {
        project_id: target.project_id,
        project_name: target.project_name,
        raw_path: target.raw_path,
        state: target.state,
        initial: target.initial,
    }
}

fn window_title(raw_path: &str, project_name: &str) -> String {
    let base = raw_path.trim_end_matches('/').rsplit('/').next().unwrap_or(raw_path);
    format!("{} — {}", base, project_name)
}

/// Refuses a save payload before decoding it: base64 of at most
/// [`MAX_WRITE_BYTES`] is at most `4 * ceil(MAX_WRITE_BYTES / 3)` characters.
/// `write_file` enforces the cap on the decoded bytes too; this stops a
/// compromised viewer from making the app allocate and decode an arbitrarily
/// large string first.
fn check_encoded_len(encoded_len: usize) -> Result<(), String> {
    if encoded_len > MAX_WRITE_BYTES.div_ceil(3) * 4 {
        return Err("Files over 1 MiB are read-only in the viewer.".into());
    }
    Ok(())
}

/// The caller's registry entry, or a sentence.
fn own_target(
    window: &tauri::Window,
    registry: &ViewerRegistry,
) -> Result<(String, ViewerTarget), String> {
    let label = require_viewer(window.label())?;
    let target = registry
        .get(&label)
        .ok_or_else(|| "This file window is no longer registered.".to_string())?;
    Ok((label, target))
}

fn resolved_path(target: &ViewerTarget) -> Result<String, String> {
    match &target.state {
        ViewerTargetState::Resolved { container_path } => Ok(container_path.clone()),
        _ => Err("Choose a file first.".into()),
    }
}

/// The one place a viewer command looks up its project (P14).
fn project_of(state: &AppState, project_id: &str) -> Result<Project, String> {
    state
        .projects_store
        .get(project_id)
        .ok_or_else(|| "This project no longer exists.".to_string())
}

/// `action` completes "Start the project before …", e.g. "saving this file".
async fn running_container_of(project: &Project, action: &str) -> Result<String, String> {
    let container_id = project
        .container_id
        .clone()
        .ok_or_else(|| not_running_message(action, "files live in its container"))?;
    require_running(&container_id, action).await?;
    Ok(container_id)
}

/// The container of the project a viewer window belongs to, if it is running.
async fn running_container_for(
    state: &AppState,
    target: &ViewerTarget,
    action: &str,
) -> Result<String, String> {
    running_container_of(&project_of(state, &target.project_id)?, action).await
}

/// Raises an existing viewer window and moves it to `location`.
fn focus_viewer(app: &AppHandle, label: &str, location: Location) {
    if let Some(existing) = app.get_webview_window(label) {
        let _ = existing.unminimize();
        let _ = existing.set_focus();
        let _ = app.emit_to(label, GOTO_EVENT, location);
    }
}

// Nine parameters are fixed by the IPC contract (P10); four injected by Tauri.
#[allow(clippy::too_many_arguments)]
#[tauri::command]
pub async fn open_file_viewer(
    project_id: String,
    path: String,
    line: Option<u32>,
    col: Option<u32>,
    end_line: Option<u32>,
    window: tauri::Window,
    app: AppHandle,
    registry: State<'_, ViewerRegistry>,
    state: State<'_, AppState>,
) -> Result<(), String> {
    require_main(window.label())?;
    let project = project_of(&state, &project_id)?;
    let container_id = running_container_of(&project, "opening files").await?;

    let mounts: Vec<String> = project.paths.iter().map(|p| p.mount_name.clone()).collect();
    let candidates = candidate_paths(&path, &mounts)?;
    let matches = probe_candidates(&container_id, &candidates).await?;
    let initial = Location { line, col, end_line };

    let target_state = match matches.len() {
        0 => ViewerTargetState::NotFound { tried: candidates },
        1 => ViewerTargetState::Resolved { container_path: matches[0].clone() },
        _ => ViewerTargetState::Choose { candidates: matches },
    };

    let title = window_title(&path, &project.name);
    let target = ViewerTarget {
        project_id,
        project_name: project.name.clone(),
        raw_path: path,
        state: target_state,
        initial: initial.clone(),
    };
    // Dedupe, stale pruning and the cap are one registry call, so a second click
    // while the first window is still being built finds it rather than reading
    // its not-yet-existing window as stale.
    let label = match registry.reserve(target, |l| app.get_webview_window(l).is_some())? {
        Reservation::Reserved(label) => label,
        // Still being built: it opens at its own location in a moment.
        Reservation::Existing { built: false, .. } => return Ok(()),
        Reservation::Existing { label, built: true } => {
            focus_viewer(&app, &label, initial);
            return Ok(());
        }
    };
    if let Err(e) = open_viewer_window(&app, &label, &title) {
        registry.remove(&label);
        return Err(e);
    }
    registry.mark_built(&label);
    Ok(())
}

#[tauri::command]
pub async fn viewer_get_state(
    window: tauri::Window,
    registry: State<'_, ViewerRegistry>,
) -> Result<ViewerState, String> {
    let (label, target) = own_target(&window, &registry)?;
    Ok(viewer_state_of(&label, target))
}

#[tauri::command]
pub async fn viewer_choose_file(
    index: usize,
    window: tauri::Window,
    registry: State<'_, ViewerRegistry>,
) -> Result<ViewerState, String> {
    let (label, target) = own_target(&window, &registry)?;
    let chosen = match &target.state {
        ViewerTargetState::Choose { candidates } => candidates
            .get(index)
            .cloned()
            .ok_or_else(|| "That choice is no longer available.".to_string())?,
        _ => return Err("This window is not choosing a file.".into()),
    };
    let app = window.app_handle();
    match registry.choose(&label, chosen, |l| app.get_webview_window(l).is_some())? {
        Choice::Resolved(updated) => Ok(viewer_state_of(&label, updated)),
        // Another window already has this file. This window was only ever a
        // chooser, so hand over to that one and close this one, as a second
        // click on the same path would have. The error is what this window
        // shows if the destroy fails.
        Choice::AlreadyOpen { label: other, .. } => {
            focus_viewer(app, &other, target.initial);
            let _ = window.destroy();
            Err("This file is already open in another window.".into())
        }
    }
}

#[tauri::command]
pub async fn viewer_read_file(
    max_bytes: u64,
    window: tauri::Window,
    registry: State<'_, ViewerRegistry>,
    state: State<'_, AppState>,
) -> Result<ViewerFile, String> {
    let (_label, target) = own_target(&window, &registry)?;
    let path = resolved_path(&target)?;
    let container_id = running_container_for(&state, &target, "opening files").await?;
    let cap = max_bytes.clamp(1, MAX_READ_BYTES);
    let fetched = fetch_container_file(&container_id, &path, cap).await?;
    let (editable, readonly_reason) = match validate_container_write_path("File", &path) {
        Ok(()) => (true, None),
        Err(reason) => (false, Some(reason)),
    };
    Ok(ViewerFile {
        hash: sha256_hex(&fetched.bytes),
        contents_base64: BASE64.encode(&fetched.bytes),
        truncated: fetched.truncated,
        size: fetched.size,
        editable,
        readonly_reason,
    })
}

#[tauri::command]
pub async fn viewer_poll_file(
    window: tauri::Window,
    registry: State<'_, ViewerRegistry>,
    state: State<'_, AppState>,
) -> Result<ViewerPoll, String> {
    let (_label, target) = own_target(&window, &registry)?;
    let path = resolved_path(&target)?;
    let container_id = running_container_for(&state, &target, "checking this file for changes").await?;
    poll_file(&container_id, &path).await
}

/// Errors from `write_file` pass through unchanged: the frontend matches the
/// `write::CONFLICT_PREFIX`/`GONE_PREFIX` prefixes and `READ_ONLY_MESSAGE` (TS copies in
/// `app/src/viewer/ipcMessages.ts`), and anything else (a full disk) is already a
/// sentence it shows as is. Success is a `SavedFile`: the new base hash and the hash
/// the disk held right after the swap.
#[tauri::command]
pub async fn viewer_write_file(
    contents_base64: String,
    base_hash: String,
    window: tauri::Window,
    registry: State<'_, ViewerRegistry>,
    state: State<'_, AppState>,
) -> Result<SavedFile, String> {
    let (_label, target) = own_target(&window, &registry)?;
    let path = resolved_path(&target)?;
    validate_container_write_path("File", &path)?;
    check_encoded_len(contents_base64.len())?;
    let bytes = BASE64
        .decode(contents_base64.as_bytes())
        .map_err(|_| "The editor sent malformed content.".to_string())?;
    let container_id = running_container_for(&state, &target, "saving this file").await?;
    write_file(&container_id, &state.exec_manager, &path, &bytes, &base_hash).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_is_main_only_and_viewer_commands_are_viewer_only() {
        assert!(require_main("main").is_ok());
        assert!(require_main("file-viewer-1").is_err());
        assert!(require_main("browser-view-x").is_err());
        assert_eq!(require_viewer("file-viewer-7").unwrap(), "file-viewer-7");
        assert!(require_viewer("main").is_err());
        assert!(require_viewer("file-viewer-").is_err());
    }

    /// Both "no container" refusals a viewer command can give start with the prefix
    /// the viewer reads as "Container not running" (`ipcMessages.ts`).
    #[test]
    fn not_running_refusals_carry_the_shared_prefix() {
        use crate::commands::file_commands::NOT_RUNNING_PREFIX;
        let m = not_running_message("checking this file for changes", "files live in its container");
        assert_eq!(m, "Start the project before checking this file for changes — files live in its container.");
        assert!(m.starts_with(NOT_RUNNING_PREFIX));
    }

    #[test]
    fn a_saved_file_serialises_both_hashes() {
        let json = serde_json::to_value(SavedFile { hash: "a".into(), disk_hash: "b".into() }).unwrap();
        assert_eq!(json, serde_json::json!({ "hash": "a", "disk_hash": "b" }));
    }

    #[test]
    fn the_title_is_basename_then_project() {
        assert_eq!(window_title("app/src/lib/urlRelay.ts", "Triple-C"), "urlRelay.ts — Triple-C");
        assert_eq!(window_title("/workspace/x/README.md", "x"), "README.md — x");
        assert_eq!(window_title("Makefile", "p"), "Makefile — p");
    }

    #[test]
    fn viewer_state_serialises_the_ipc_shape() {
        let target = ViewerTarget {
            project_id: "pid".into(),
            project_name: "P".into(),
            raw_path: "src/a.rs".into(),
            state: ViewerTargetState::Resolved { container_path: "/workspace/p/src/a.rs".into() },
            initial: Location { line: Some(3), col: Some(2), end_line: None },
        };
        let json = serde_json::to_value(viewer_state_of("file-viewer-1", target)).unwrap();
        assert_eq!(json["project_id"], "pid");
        assert_eq!(json["state"]["kind"], "resolved");
        assert_eq!(json["state"]["container_path"], "/workspace/p/src/a.rs");
        assert_eq!(json["initial"]["line"], 3);
        assert!(json["initial"]["end_line"].is_null());
    }

    #[test]
    fn the_encoded_length_is_capped_before_decoding() {
        let at_cap = BASE64.encode(vec![0u8; MAX_WRITE_BYTES]);
        assert!(check_encoded_len(at_cap.len()).is_ok());
        // MAX + 1 and MAX + 2 bytes pad to the same length as MAX; `write_file`'s
        // decoded check refuses those. The first size this bound itself refuses:
        let over_cap = BASE64.encode(vec![0u8; MAX_WRITE_BYTES + 3]);
        assert!(check_encoded_len(over_cap.len()).is_err());
        assert!(check_encoded_len(at_cap.len() + 1).is_err());
        assert!(check_encoded_len(0).is_ok());
    }
}
