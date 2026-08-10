//! IPC surface for the browser view pane. The mechanism lives in
//! [`crate::browser_view`]; this file only translates between it and the
//! frontend.

use tauri::{AppHandle, State};

use crate::browser_view::install::{self, BrowserSetupOutcome};
use crate::browser_view::{manager, BrowserViewStatus};
use crate::AppState;

/// Turn the pane on or off for a project.
///
/// Enabling probes the container and brings the viewer up when it can; a
/// container that isn't running, or one without Playwright, comes back as a
/// non-`Running` status carrying an explanation rather than an error, so the
/// pane always has something specific to say. This is host-side only — no
/// container recreation is involved either way.
#[tauri::command]
pub async fn set_browser_view_enabled(
    project_id: String,
    enabled: bool,
    app_handle: AppHandle,
    state: State<'_, AppState>,
) -> Result<BrowserViewStatus, String> {
    if !enabled {
        // Awaits the supervisor, so the host port is released before we return.
        manager().stop(&project_id).await;
        return Ok(manager().status(&project_id).await);
    }

    let container_id = running_container(&state, &project_id, "opening the browser view").await?;

    manager()
        .start(
            project_id,
            container_id,
            app_handle,
            state.projects_store.clone(),
        )
        .await
}

/// Current status. Cheap: reads in-process state only, never the container.
#[tauri::command]
pub async fn get_browser_view_status(project_id: String) -> Result<BrowserViewStatus, String> {
    Ok(manager().status(&project_id).await)
}

/// Probe the container for Playwright without starting anything.
///
/// Lets the pane say "install this" before the user asks for a view, and lets
/// them re-check after installing without toggling the feature. Read-only: it
/// runs one `node -e` and changes nothing.
#[tauri::command]
pub async fn check_browser_view_support(
    project_id: String,
    state: State<'_, AppState>,
) -> Result<crate::browser_view::detect::PlaywrightDetection, String> {
    let container_id = running_container(&state, &project_id, "checking for Playwright").await?;
    crate::browser_view::detect::detect(&container_id).await
}

/// Install `playwright` and `@playwright/cli` into the container.
///
/// **This mutates the container**, so it is a command of its own and is only
/// ever reached by the user pressing the button — nothing here runs on tab
/// open. Progress streams on `container-progress`; the outcome carries a fresh
/// probe so the pane updates itself.
///
/// Browsers are *not* fetched here. They are hundreds of megabytes and get
/// their own action, with the size stated before the click.
#[tauri::command]
pub async fn install_browser_view_support(
    project_id: String,
    app_handle: AppHandle,
    state: State<'_, AppState>,
) -> Result<BrowserSetupOutcome, String> {
    let container_id = running_container(&state, &project_id, "installing Playwright").await?;
    install::install_packages(&app_handle, &project_id, &container_id).await
}

/// Install a browser — `chromium` (Playwright's own build, for scripts that
/// call `chromium.launch()`) or `chrome` (the Google Chrome channel that
/// `@playwright/mcp` asks for) — along with the system libraries it needs, and
/// verify that it actually starts.
///
/// Also a mutation, also user-initiated only.
#[tauri::command]
pub async fn install_browser_view_browser(
    project_id: String,
    browser: String,
    app_handle: AppHandle,
    state: State<'_, AppState>,
) -> Result<BrowserSetupOutcome, String> {
    let target = install::BrowserTarget::parse(&browser)?;
    let container_id = running_container(&state, &project_id, "installing a browser").await?;
    install::install_browser(&app_handle, &project_id, &container_id, target).await
}

/// The project's container, or a sentence saying why there isn't one.
///
/// Every command here needs a *running* container, and every one of them used
/// to be able to fail somewhere further in with a Docker error instead. The
/// `action` is folded into the message so "start the container first" arrives
/// attached to what the user was trying to do.
async fn running_container(
    state: &State<'_, AppState>,
    project_id: &str,
    action: &str,
) -> Result<String, String> {
    let project = state
        .projects_store
        .get(project_id)
        .ok_or_else(|| format!("Project {} not found", project_id))?;

    let Some(container_id) = project.container_id.clone() else {
        return Err(format!(
            "This project has no container yet. Start it before {}.",
            action
        ));
    };
    if !crate::docker::container::is_container_running(&container_id)
        .await
        .unwrap_or(false)
    {
        return Err(format!(
            "The container for “{}” isn't running. Start it before {}.",
            project.name, action
        ));
    }
    Ok(container_id)
}
