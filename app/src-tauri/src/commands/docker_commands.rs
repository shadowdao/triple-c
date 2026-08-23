use tauri::State;

use crate::docker;
use crate::models::{container_config, ContainerInfo};
use crate::AppState;

#[tauri::command]
pub async fn check_docker() -> Result<bool, String> {
    docker::check_docker_available().await
}

#[tauri::command]
pub async fn check_image_exists(state: State<'_, AppState>) -> Result<bool, String> {
    let settings = state.settings_store.get();
    let image_name = container_config::resolve_image_name(&settings.image_source, &settings.custom_image_name);
    docker::image_exists(&image_name).await
}

#[tauri::command]
pub async fn build_image(app_handle: tauri::AppHandle) -> Result<(), String> {
    use tauri::Emitter;
    docker::build_image(move |msg| {
        let _ = app_handle.emit("image-build-progress", msg);
    })
    .await
}

#[tauri::command]
pub async fn get_container_info(
    project_id: String,
    state: State<'_, AppState>,
) -> Result<Option<ContainerInfo>, String> {
    let project = state
        .projects_store
        .get(&project_id)
        .ok_or_else(|| format!("Project {} not found", project_id))?;
    docker::get_container_info(&project).await
}

#[tauri::command]
pub async fn list_sibling_containers() -> Result<Vec<serde_json::Value>, String> {
    let containers = docker::list_sibling_containers().await?;
    let result: Vec<serde_json::Value> = containers
        .into_iter()
        .map(|c| {
            serde_json::json!({
                "id": c.id,
                "names": c.names,
                "image": c.image,
                "state": c.state,
                "status": c.status,
            })
        })
        .collect();
    Ok(result)
}

// ---------------------------------------------------------------------------
// Disk
// ---------------------------------------------------------------------------
//
// The disk view's IPC surface. It lives here rather than in a module of its own
// for the same reason `check_image_exists` does: these are thin shims over
// `crate::docker`, and the logic they call is in `docker/disk.rs` where it can
// be unit-tested without a daemon.

/// Measure where the daemon's bytes have gone.
///
/// **Expensive on purpose.** This is `GET /system/df` plus an `image_history`
/// per distinct image, and `df()` walks every image, container and volume on
/// the daemon to compute shared-layer sizes. On a 100 GB store that is seconds.
/// The frontend must keep it behind an explicit Scan button — never on panel
/// open, never on a timer.
#[tauri::command]
pub async fn get_docker_disk_usage(
    state: State<'_, AppState>,
) -> Result<docker::disk::DiskUsageReport, String> {
    let projects = state.projects_store.list();
    docker::disk::scan(&projects).await
}

/// Everything that could be reclaimed, each with its measured cost.
///
/// Takes the report from [`get_docker_disk_usage`] rather than re-measuring, so
/// a user who re-plans after ticking a box does not pay for a second `df()`.
#[tauri::command]
pub async fn list_reclaimable(
    report: docker::disk::DiskUsageReport,
    state: State<'_, AppState>,
) -> Result<docker::disk::ReclaimPlan, String> {
    let projects = state.projects_store.list();
    docker::disk::list_reclaimable(&projects, &report).await
}

/// Run the ticked targets and report what each one actually freed.
///
/// `ReclaimTarget` cannot express a destructive action — that is a different
/// type, reached only through [`destroy_project_disk_object`] with a typed
/// confirmation — so there is no selection a user can build here that deletes a
/// live project's data.
#[tauri::command]
pub async fn reclaim(
    targets: Vec<docker::disk::ReclaimTarget>,
    state: State<'_, AppState>,
) -> Result<docker::disk::ReclaimOutcome, String> {
    let projects = state.projects_store.list();
    Ok(docker::disk::reclaim(&targets, &projects).await)
}

/// Delete one object that has no other copy, against a typed confirmation of
/// the project's name.
///
/// Deliberately one target per call: this is never part of a bulk action.
#[tauri::command]
pub async fn destroy_project_disk_object(
    target: docker::disk::DestructiveTarget,
    confirmation: String,
    state: State<'_, AppState>,
) -> Result<docker::disk::ReclaimResult, String> {
    let projects = state.projects_store.list();
    docker::disk::destroy(&target, &confirmation, &projects).await
}

/// Run the orphaned-snapshot sweep on demand and return its report.
///
/// The sweep already runs at startup, after every recreation and after a
/// migration settles, but every one of those callers throws the report away —
/// so a user has never been able to see that 11.9 GB of superseded images were
/// found and left because a stopped container still pinned them.
#[tauri::command]
pub async fn sweep_orphaned_snapshots() -> Result<docker::SnapshotSweepReport, String> {
    Ok(docker::sweep_orphaned_snapshots().await)
}
