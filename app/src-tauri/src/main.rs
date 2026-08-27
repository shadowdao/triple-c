// Prevents additional console window on Windows in release
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

/// WebKitGTK's DMA-BUF renderer (its default accelerated-compositing path
/// since 2.42) fails outright on some Mesa/driver/compositor combinations
/// under Wayland, printing `Could not create default EGL display:
/// EGL_BAD_PARAMETER. Aborting.` straight to stderr from WebKitGTK's own C
/// code and killing the webview before Triple-C's own logging even starts —
/// see triple-c#34, reported on CachyOS/Arch with Wayland.
///
/// Set unconditionally on Linux rather than gated on `WAYLAND_DISPLAY`: that
/// variable is exported into an XWayland client's environment too, so a
/// gate on it wouldn't even cleanly separate "Wayland" from "X11" — and
/// there is no reliable heuristic at all for the actual variable that
/// matters, which Mesa/driver/compositor combination is affected. This is
/// the blunt instrument, chosen deliberately because the fallback is a real
/// trade, not a free one: the terminal's `@xterm/addon-webgl` renderer
/// (`TerminalView.tsx`) is the one surface in this app actually asking for
/// GPU compositing, and it degrades to xterm's canvas renderer under this
/// setting — slower on very heavy output, but the addon's own construction
/// is already wrapped in a fallback (`WebGL not available` is a handled
/// case, not a crash), so this is a real but graceful downgrade, traded
/// against a startup abort that has no fallback at all.
///
/// Must be set before `triple_c_lib::run()` — GTK/WebKitGTK reads it at
/// their own init time, which happens inside the Tauri builder that
/// function calls into, not at binary load.
///
/// A user who has already set this themselves is left alone. That includes
/// setting it to `0`, on the assumption WebKitGTK treats it as a boolean
/// rather than presence-only — not verified against WebKitGTK's own source,
/// so if it turns out to be presence-only, `=0` still reads as "set" here
/// and disables DMA-BUF the same as any other value, which is at least the
/// safe direction to be wrong in.
///
/// This env var also leaks to whatever the app spawns afterwards — notably
/// a cold-launched default browser via the `opener` plugin's `xdg-open`
/// call. Narrow in practice (an already-running browser just receives the
/// URL; most non-WebKitGTK browsers ignore the variable entirely), but
/// worth knowing before chasing the "links don't open" half of triple-c#34
/// as a separate, unrelated cause.
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
