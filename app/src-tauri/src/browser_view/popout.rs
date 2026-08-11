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

use tauri::{AppHandle, Emitter, Manager, WebviewUrl, WebviewWindowBuilder, WindowEvent};

/// Emitted when a pop-out opens or closes. Payload: `{ project_id, open }`.
///
/// The window can close without the app asking it to — the user hits its X, or
/// a teardown takes it — so the pane learns about it the same way it learns
/// about everything else here, by listening.
const POPOUT_EVENT: &str = "browser-view-popout-changed";

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
        emit(app, project_id, true);
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
    // to take the view back into the tab.
    window.on_window_event(move |event| {
        if matches!(event, WindowEvent::Destroyed) {
            emit(&app_for_event, &project_id_owned, false);
        }
    });

    log::info!("Browser view: popped out for project {}", project_id);
    emit(app, project_id, true);
    Ok(())
}

/// Close the pop-out if there is one. Safe to call when there isn't.
///
/// `destroy`, not `close`: `close` raises `CloseRequested`, and the app's
/// window-event handler treats that as a request to quit for the main window.
/// Nothing here should ever be able to be mistaken for that.
pub fn close(app: &AppHandle, project_id: &str) {
    if let Some(window) = app.get_webview_window(&window_label(project_id)) {
        if let Err(e) = window.destroy() {
            log::warn!(
                "Browser view: could not close the pop-out for project {}: {}",
                project_id,
                e
            );
        }
    }
    // Unconditional: `Destroyed` covers the normal path, but a window that was
    // already gone still owes the pane an answer.
    emit(app, project_id, false);
}

pub fn is_open(app: &AppHandle, project_id: &str) -> bool {
    app.get_webview_window(&window_label(project_id)).is_some()
}

/// Pin the pop-out above other windows, or unpin it. No-op when it is closed.
pub fn set_always_on_top(app: &AppHandle, project_id: &str, on_top: bool) -> Result<(), String> {
    let Some(window) = app.get_webview_window(&window_label(project_id)) else {
        return Ok(());
    };
    window
        .set_always_on_top(on_top)
        .map_err(|e| format!("Could not change the window's stacking: {}", e))
}

fn emit(app: &AppHandle, project_id: &str, open: bool) {
    let _ = app.emit(
        POPOUT_EVENT,
        serde_json::json!({ "project_id": project_id, "open": open }),
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
}
