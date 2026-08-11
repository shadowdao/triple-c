//! The browser view in a window of its own.
//!
//! Watching a browser and working in a terminal are the same task done at the
//! same time, and a tab can only be one of them. So the pane can be detached
//! into a second OS window — put on the other monitor, or pinned on top of
//! whatever else is in front.
//!
//! ## Why this is a native window and not a second iframe
//!
//! The window loads the *same* token-bearing loopback URL the pane's iframe
//! uses ([`crate::browser_view::BrowserViewStatus::url`]), as its top-level
//! document. That has two consequences worth stating:
//!
//! - It is a **remote-origin** webview. No capability lists this window, so it
//!   has no IPC surface at all — `invoke` is not reachable from it, which is
//!   exactly right for a page served out of a container. Do not add one.
//! - The app CSP does not apply, and does not need to: `frame-src` exists to
//!   constrain what the *app's* document may embed, and this is not embedded.
//!   The port is still confined to [`crate::browser_view::proxy`]'s range and
//!   still gated by the session token, which is what actually protects it.
//!
//! ## Lifetime
//!
//! The window is owned by the session, not by the user's patience: when a view
//! stops — the user pressed Stop, the container went away, the viewer died —
//! the supervisor's teardown calls [`close`], because a window left showing a
//! dead viewer is worse than no window. The reverse is not true; closing the
//! window leaves the view running, and the pane takes it back into the tab.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, WebviewUrl, WebviewWindowBuilder, WindowEvent};

/// Emitted when a pop-out opens or closes. Payload: [`PopoutState`] plus the
/// project id.
///
/// The window can close without the app asking it to — the user hits its X, or
/// a teardown takes it — so the pane learns about it the same way it learns
/// about everything else here, by listening.
const POPOUT_EVENT: &str = "browser-view-popout-changed";

/// What the pane needs to render its pop-out controls.
///
/// Both fields are read from the window itself rather than remembered on either
/// side: the pane is unmounted whenever another Project Home sub-tab is
/// selected, so anything it merely *remembers* about the window is gone by the
/// time the user comes back, while the window is still there.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct PopoutState {
    pub open: bool,
    pub always_on_top: bool,
}

impl PopoutState {
    const CLOSED: Self = Self {
        open: false,
        always_on_top: false,
    };
}

/// Tauri window labels admit `[a-zA-Z0-9-/:_]` only. Project ids are UUIDs, so
/// this never fires in practice; it exists so a hand-edited `projects.json`
/// cannot produce a label Tauri rejects at build time.
pub fn window_label(project_id: &str) -> String {
    let id: String = project_id
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect();
    format!("browser-view-{}", id)
}

/// Open the pop-out, or raise it if it is already open.
///
/// `url` is the live session's URL; the caller has already established that the
/// view is running, because there is nothing to show otherwise.
pub fn open(
    app: &AppHandle,
    project_id: &str,
    project_name: &str,
    url: &str,
    always_on_top: bool,
) -> Result<(), String> {
    let label = window_label(project_id);

    if let Some(window) = app.get_webview_window(&label) {
        // Asking twice means "I can't see it", not "open another".
        let _ = window.unminimize();
        let _ = window.set_focus();
        let _ = window.set_always_on_top(always_on_top);
        emit(app, project_id, state(app, project_id));
        return Ok(());
    }

    let parsed = url
        .parse()
        .map_err(|e| format!("The browser view's address is not a URL: {}", e))?;

    let project_id_owned = project_id.to_string();
    let app_for_event = app.clone();

    let window = WebviewWindowBuilder::new(app, &label, WebviewUrl::External(parsed))
        .title(format!("{} — browser", project_name))
        .inner_size(1100.0, 820.0)
        .min_inner_size(480.0, 360.0)
        .always_on_top(always_on_top)
        .build()
        .map_err(|e| format!("Could not open the browser window: {}", e))?;

    // Closed from its own titlebar, this is the only thing that tells the pane
    // to take the view back into the tab. `Resized` drives match-window mode —
    // see `set_match_window`.
    window.on_window_event(move |event| match event {
        WindowEvent::Destroyed => {
            set_match_window(&project_id_owned, false);
            emit(&app_for_event, &project_id_owned, PopoutState::CLOSED);
        }
        WindowEvent::Resized(size) => {
            on_resized(&app_for_event, &project_id_owned, size.width, size.height);
        }
        _ => {}
    });

    log::info!("Browser view: popped out for project {}", project_id);
    emit(app, project_id, state(app, project_id));
    Ok(())
}

/// Close the pop-out if there is one. Safe to call when there isn't.
///
/// `destroy`, not `close`: `close` raises `CloseRequested`, and the app's
/// window-event handler treats that as a request to quit for the main window.
/// Nothing here should ever be able to be mistaken for that.
///
/// A failure is **returned, not logged and forgotten**. The pane puts its
/// iframe back the moment it believes the window is gone, so reporting a close
/// that did not happen is how you end up with two viewers driving one browser —
/// the exact state the iframe is dropped to prevent.
pub fn close(app: &AppHandle, project_id: &str) -> Result<(), String> {
    if let Some(window) = app.get_webview_window(&window_label(project_id)) {
        window.destroy().map_err(|e| {
            log::warn!(
                "Browser view: could not close the pop-out for project {}: {}",
                project_id,
                e
            );
            format!("Could not close the browser window: {}", e)
        })?;
    }
    // `Destroyed` covers the normal path; a window that was already gone still
    // owes the pane an answer.
    emit(app, project_id, PopoutState::CLOSED);
    Ok(())
}

/// Whether the window exists and how it is stacked, read from the window.
pub fn state(app: &AppHandle, project_id: &str) -> PopoutState {
    match app.get_webview_window(&window_label(project_id)) {
        Some(window) => PopoutState {
            open: true,
            // A window that cannot answer is not a reason to fail the call; the
            // pin is a preference, and "not pinned" is the safe reading.
            always_on_top: window.is_always_on_top().unwrap_or(false),
        },
        None => PopoutState::CLOSED,
    }
}

/// Pin the pop-out above other windows, or unpin it. No-op when it is closed.
pub fn set_always_on_top(app: &AppHandle, project_id: &str, on_top: bool) -> Result<(), String> {
    let Some(window) = app.get_webview_window(&window_label(project_id)) else {
        return Ok(());
    };
    window
        .set_always_on_top(on_top)
        .map_err(|e| format!("Could not change the window's stacking: {}", e))?;
    emit(app, project_id, state(app, project_id));
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// Match-window mode
// ─────────────────────────────────────────────────────────────────────────────

/// Projects whose pop-out is driving the page's viewport, and the generation of
/// the latest resize for each — the debounce is "did anything else arrive while
/// I slept?", which needs no timer to cancel.
static MATCH_WINDOW: OnceLock<Mutex<HashMap<String, (bool, u64)>>> = OnceLock::new();

/// How long the window has to stop moving before the page is resized.
///
/// A drag emits `Resized` continuously; each one costs a container exec, and
/// Chromium relayouts the page. Settling first turns a drag into one resize.
const RESIZE_SETTLE: Duration = Duration::from_millis(300);

fn match_window_map() -> &'static Mutex<HashMap<String, (bool, u64)>> {
    MATCH_WINDOW.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Turn match-window mode on or off for a project.
///
/// Only ever affects a page **Triple-C opened** — a bound browser cannot be
/// joined by a second client, so a page `@playwright/mcp` launched keeps
/// whatever viewport it was given. See [`super::page`].
pub fn set_match_window(project_id: &str, enabled: bool) {
    let mut map = match_window_map().lock().unwrap_or_else(|e| e.into_inner());
    let entry = map.entry(project_id.to_string()).or_insert((false, 0));
    entry.0 = enabled;
}

pub fn match_window(project_id: &str) -> bool {
    match_window_map()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(project_id)
        .map(|(on, _)| *on)
        .unwrap_or(false)
}

/// The pop-out's current inner size, for applying match-window immediately
/// rather than only on the next drag.
pub fn inner_size(app: &AppHandle, project_id: &str) -> Option<(u32, u32)> {
    let window = app.get_webview_window(&window_label(project_id))?;
    let size = window.inner_size().ok()?;
    Some((size.width, size.height))
}

/// Debounce a resize, then push the settled size into the page's viewport.
fn on_resized(app: &AppHandle, project_id: &str, width: u32, height: u32) {
    let generation = {
        let mut map = match_window_map().lock().unwrap_or_else(|e| e.into_inner());
        let Some(entry) = map.get_mut(project_id) else {
            return;
        };
        if !entry.0 {
            return;
        }
        entry.1 += 1;
        entry.1
    };

    let app = app.clone();
    let project_id = project_id.to_string();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(RESIZE_SETTLE).await;
        // Superseded by a later resize: that one will do the work.
        {
            let map = match_window_map().lock().unwrap_or_else(|e| e.into_inner());
            match map.get(&project_id) {
                Some((true, latest)) if *latest == generation => {}
                _ => return,
            }
        }

        let state = app.state::<crate::AppState>();
        let Some(container_id) = state
            .projects_store
            .get(&project_id)
            .and_then(|p| p.container_id)
        else {
            return;
        };
        if let Err(e) = super::page::set_viewport(
            &container_id,
            super::page::Viewport::sane(width, height),
        )
        .await
        {
            log::debug!("Browser view: could not match the page to the window: {}", e);
        }
    });
}

fn emit(app: &AppHandle, project_id: &str, state: PopoutState) {
    let _ = app.emit(
        POPOUT_EVENT,
        serde_json::json!({
            "project_id": project_id,
            "open": state.open,
            "always_on_top": state.always_on_top,
        }),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_are_derived_from_the_project_and_are_tauri_safe() {
        assert_eq!(
            window_label("6b1f4a2c-0d5e-4f9a-9c11-2f0b7d3e8a44"),
            "browser-view-6b1f4a2c-0d5e-4f9a-9c11-2f0b7d3e8a44"
        );
        assert_eq!(window_label("a b/c.d"), "browser-view-a_b_c_d");
    }

    #[test]
    fn distinct_projects_get_distinct_windows() {
        assert_ne!(window_label("alpha"), window_label("beta"));
    }

    #[test]
    fn match_window_is_off_until_asked_for_and_is_per_project() {
        assert!(!match_window("mw-a"));
        set_match_window("mw-a", true);
        assert!(match_window("mw-a"));
        // Another project's window must not start driving its page too.
        assert!(!match_window("mw-b"));
        set_match_window("mw-a", false);
        assert!(!match_window("mw-a"));
    }

    #[test]
    fn a_resize_supersedes_the_one_before_it() {
        // The debounce is a generation counter, not a cancellable timer: only
        // the newest resize of a drag survives to touch the container.
        set_match_window("mw-gen", true);
        let read = || {
            match_window_map()
                .lock()
                .unwrap()
                .get("mw-gen")
                .map(|(_, g)| *g)
                .unwrap()
        };
        let before = read();
        {
            let mut map = match_window_map().lock().unwrap();
            let entry = map.get_mut("mw-gen").unwrap();
            entry.1 += 1;
        }
        assert!(read() > before);
        set_match_window("mw-gen", false);
    }
}
