// Prevents additional console window on Windows in release
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

/// WebKitGTK's DMA-BUF renderer (its default accelerated-compositing path
/// since 2.42) fails outright on some Mesa/driver/compositor combinations
/// under Wayland, printing `Could not create default EGL display:
/// EGL_BAD_PARAMETER. Aborting.` straight to stderr from WebKitGTK's own C
/// code and killing the webview before Triple-C's own logging even starts —
/// see triple-c#34, reported on CachyOS/Arch with Wayland. There is no
/// reliable way to detect the affected driver/compositor combination ahead
/// of time (it is not simply "Wayland vs. X11" — the same failure has been
/// reported under XWayland too), so this is set unconditionally on Linux
/// rather than gated on `WAYLAND_DISPLAY`. WebKitGTK falls back to a
/// software/shared-memory compositing path when this is set, which costs
/// some rendering performance but nothing this app's UI depends on.
///
/// Must be set before `triple_c_lib::run()` — GTK/WebKitGTK reads it at
/// their own init time, which happens inside the Tauri builder that
/// function calls into, not at binary load.
///
/// A user who has already set this themselves (including to `0`, to force
/// the DMA-BUF path back on for their own hardware) is left alone.
#[cfg(target_os = "linux")]
fn apply_webkit_wayland_workaround() {
    if std::env::var_os("WEBKIT_DISABLE_DMABUF_RENDERER").is_none() {
        std::env::set_var("WEBKIT_DISABLE_DMABUF_RENDERER", "1");
    }
}

fn main() {
    #[cfg(target_os = "linux")]
    apply_webkit_wayland_workaround();

    triple_c_lib::run()
}
