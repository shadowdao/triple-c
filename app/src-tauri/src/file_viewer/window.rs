//! The viewer window itself. Mirrors `browser_view/popout.rs`, with two differences:
//! the URL is the app's own second entry (`WebviewUrl::App`), so the capability in
//! `capabilities/file-viewer.json` applies; and the registry entry is removed on
//! `Destroyed`, which fires for both the X button (after JS calls `destroy()`) and a
//! Rust-side `destroy()`.

use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindowBuilder, WindowEvent};

use super::registry::ViewerRegistry;

pub fn open_viewer_window(app: &AppHandle, label: &str, title: &str) -> Result<(), String> {
    let window = WebviewWindowBuilder::new(app, label, WebviewUrl::App("viewer.html".into()))
        .title(title)
        .inner_size(900.0, 700.0)
        .min_inner_size(480.0, 320.0)
        .build()
        .map_err(|e| format!("Could not open the file window: {}", e))?;

    let app_for_event = app.clone();
    let label_owned = label.to_string();
    window.on_window_event(move |event| {
        if let WindowEvent::Destroyed = event {
            app_for_event.state::<ViewerRegistry>().remove(&label_owned);
        }
    });
    Ok(())
}
