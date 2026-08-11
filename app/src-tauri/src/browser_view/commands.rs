//! IPC surface for the browser view pane. The mechanism lives in
//! [`crate::browser_view`]; this file only translates between it and the
//! frontend.

use tauri::{AppHandle, State};

use crate::browser_view::install::{self, BrowserSetupOutcome};
use crate::browser_view::{manager, page, popout, BrowserViewState, BrowserViewStatus};
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

/// Detach the view into a window of its own, or raise the one already open.
///
/// Host-side and window-only: the viewer keeps running exactly as it was, and
/// this touches neither the container nor the proxy. Requires a *live* view,
/// because a window with nothing behind it is not worth opening — the pane
/// only offers the button in that state, and this enforces it.
#[tauri::command]
pub async fn open_browser_view_popout(
    project_id: String,
    always_on_top: bool,
    app_handle: AppHandle,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let status = manager().status(&project_id).await;
    let (BrowserViewState::Running, Some(url)) = (status.state, status.url.as_deref()) else {
        return Err(
            "The browser view isn't running. Start it before opening it in its own window."
                .to_string(),
        );
    };

    let name = state
        .projects_store
        .get(&project_id)
        .map(|p| p.name)
        .unwrap_or_else(|| "Triple-C".to_string());

    popout::open(&app_handle, &project_id, &name, url, always_on_top)
}

/// Close the pop-out, putting the view back in the tab. No-op if it is closed.
///
/// Propagates a failed close rather than reporting success: the pane restores
/// its iframe on success, and doing that with the window still up puts two
/// viewers on one browser.
#[tauri::command]
pub async fn close_browser_view_popout(
    project_id: String,
    app_handle: AppHandle,
) -> Result<(), String> {
    popout::close(&app_handle, &project_id)
}

/// Whether the pop-out is open, and whether it is pinned on top.
///
/// Read on every pane mount: the window outlives the pane — which is unmounted
/// whenever another Project Home sub-tab is selected — so neither fact can be
/// carried in component state.
#[tauri::command]
pub async fn get_browser_view_popout_state(
    project_id: String,
    app_handle: AppHandle,
) -> Result<popout::PopoutState, String> {
    Ok(popout::state(&app_handle, &project_id))
}

/// Pin the pop-out above other windows, so it can be watched while working in
/// the main one.
#[tauri::command]
pub async fn set_browser_view_popout_always_on_top(
    project_id: String,
    on_top: bool,
    app_handle: AppHandle,
) -> Result<(), String> {
    popout::set_always_on_top(&app_handle, &project_id, on_top)
}

/// Open a URL in a browser *inside* the container, published so the pane shows
/// it.
///
/// Two uses, one action: an auth URL — where the OAuth callback listener is in
/// the container too, so the loop closes without the host being involved at all
/// — and a dev server on container loopback, which is how you watch a UI Claude
/// is building.
///
/// The scheme allow-list mirrors the URL relay's: `http`/`https` only, so this
/// can never be talked into opening `file:` on the container's filesystem.
#[tauri::command]
pub async fn open_page_in_container_browser(
    project_id: String,
    url: String,
    width: u32,
    height: u32,
    show_window: bool,
    app_handle: AppHandle,
    state: State<'_, AppState>,
) -> Result<page::PageState, String> {
    let trimmed = url.trim();
    if !(trimmed.starts_with("http://") || trimmed.starts_with("https://")) {
        return Err("Only http:// and https:// URLs can be opened in the browser.".to_string());
    }
    let container_id = running_container(&state, &project_id, "opening a page").await?;
    let detection = crate::browser_view::detect::detect(&container_id).await?;
    let opened = page::open(
        &container_id,
        &detection,
        trimmed,
        page::Viewport::sane(width, height),
    )
    .await?;

    // A page nobody can see is not an opened page. Opening one used to leave
    // the user to go and press Start in the Browser tab themselves — and from
    // the terminal's URL prompt, with no indication that was even needed.
    // Asking for a page *is* asking to watch it, so the viewer comes up too.
    let status = manager().status(&project_id).await;
    if status.state != BrowserViewState::Running {
        manager()
            .start(
                project_id.clone(),
                container_id,
                app_handle.clone(),
                state.projects_store.clone(),
            )
            .await?;
    }

    // From the terminal there is no pane on screen to fill, so the page needs a
    // window of its own or it lands somewhere the user isn't looking.
    if show_window {
        let status = manager().status(&project_id).await;
        if let Some(url) = status.url.as_deref() {
            let name = state
                .projects_store
                .get(&project_id)
                .map(|p| p.name)
                .unwrap_or_else(|| "Triple-C".to_string());
            popout::open(&app_handle, &project_id, &name, url, false)?;
        }
    }

    Ok(opened)
}

/// Resize the page this opened. The pop-out's "match window" mode calls this on
/// every settled resize, so it is deliberately cheap: one control-file write.
#[tauri::command]
pub async fn set_container_page_viewport(
    project_id: String,
    width: u32,
    height: u32,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let container_id = running_container(&state, &project_id, "resizing the page").await?;
    page::set_viewport(&container_id, page::Viewport::sane(width, height)).await
}

/// State of the page this opened, if any. Never fails: "no page" is an answer.
#[tauri::command]
pub async fn get_container_page_state(
    project_id: String,
    state: State<'_, AppState>,
) -> Result<page::PageState, String> {
    let Ok(container_id) = running_container(&state, &project_id, "reading the page").await else {
        return Ok(page::PageState::default());
    };
    Ok(page::state(&container_id).await)
}

/// Close the page this opened, leaving the view itself running.
#[tauri::command]
pub async fn close_container_page(
    project_id: String,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let container_id = running_container(&state, &project_id, "closing the page").await?;
    page::close(&container_id).await;
    Ok(())
}

/// Make the page track the pop-out window's size as it is dragged.
///
/// Only affects a page **this app opened**: a bound browser admits no second
/// client, so one `@playwright/mcp` launched keeps the viewport it was given.
/// Turning it on applies the window's current size immediately, so the toggle
/// has a visible effect without waiting for a drag.
#[tauri::command]
pub async fn set_browser_view_match_window(
    project_id: String,
    enabled: bool,
    app_handle: AppHandle,
    state: State<'_, AppState>,
) -> Result<(), String> {
    popout::set_match_window(&project_id, enabled);
    if !enabled {
        return Ok(());
    }
    let Some((width, height)) = popout::inner_size(&app_handle, &project_id) else {
        return Ok(());
    };
    let container_id = running_container(&state, &project_id, "matching the window").await?;
    page::set_viewport(&container_id, page::Viewport::sane(width, height)).await
}

/// Whether match-window mode is on. Read on mount, like the rest of the
/// pop-out's state — the pane is unmounted whenever another sub-tab is shown.
#[tauri::command]
pub async fn get_browser_view_match_window(project_id: String) -> Result<bool, String> {
    Ok(popout::match_window(&project_id))
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
