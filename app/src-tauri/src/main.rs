// Prevents additional console window on Windows in release
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

/// WebKitGTK's DMA-BUF renderer (its default accelerated-compositing path
/// since 2.42) fails outright on some Mesa/driver/compositor combinations
/// under Wayland, killing the webview and leaving a blank window — see
/// triple-c#34, reported on CachyOS/Arch with Wayland.
///
/// **This is not the only cause of a blank window, and the error text alone
/// does not tell them apart.** An earlier version of this comment quoted
/// `Could not create default EGL display: EGL_BAD_PARAMETER. Aborting.` as
/// the error this fixes. The AppImage produces that same string for an
/// entirely unrelated reason: it bundled a `libwayland-client.so.0` that
/// shadowed the host's, and the host's `libEGL_mesa.so.0` has a hard
/// DT_NEEDED on that library, so the EGL driver failed to load before any
/// renderer choice was reachable. This flag was set, and correctly, and made no difference —
/// which cost a round of debugging that started from the comment rather than
/// from the evidence. See `scripts/unbundle-wayland-client.sh`.
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
/// A user who has already set this themselves is left alone — with one
/// correction. The earlier version of this function left *any* pre-set value
/// alone, including `0`, on the assumption WebKitGTK reads the variable as a
/// boolean. WebKitGTK reads it as presence-only, so `WEBKIT_DISABLE_DMABUF_
/// RENDERER=0` disabled DMA-BUF exactly like `=1` did, and there was no value
/// at all a user could set to get the accelerated path back: the escape hatch
/// the comment described did not exist. `0`, `false` and empty are now treated
/// as an explicit opt-out and the variable is *removed*, which is the only
/// thing WebKitGTK reads as "enabled". The default is unchanged — unset still
/// means disabled on Linux, so nobody who was not deliberately overriding this
/// sees any difference.
///
/// That matters more than it looks, because the trade described above is not
/// the trade actually being made. `@xterm/addon-webgl` does not fall back to
/// the canvas renderer here: its constructor throws only when WebGL is
/// *absent*, and with DMA-BUF disabled WebGL is still present — served by
/// software rasterisation. So the addon loads happily and every terminal frame
/// is rendered on the CPU and copied, which is slower than the canvas renderer
/// this comment assumed it would degrade to, not faster. See
/// `terminal_gpu_rendering` in `AppSettings` for the switch that decides
/// whether the addon is loaded at all.
///
/// This env var also leaks to whatever the app spawns afterwards — notably
/// a cold-launched default browser via the `opener` plugin's `xdg-open`
/// call. Narrow in practice (an already-running browser just receives the
/// URL; most non-WebKitGTK browsers ignore the variable entirely), but
/// worth knowing before chasing the "links don't open" half of triple-c#34
/// as a separate, unrelated cause.
#[cfg(target_os = "linux")]
const DMABUF_VAR: &str = "WEBKIT_DISABLE_DMABUF_RENDERER";

/// What to do with `WEBKIT_DISABLE_DMABUF_RENDERER`, given whatever it is
/// already set to. Split from the mutation so it can be tested without
/// touching process-wide environment state from a parallel test runner.
#[cfg(target_os = "linux")]
#[derive(Debug, PartialEq, Eq)]
enum DmabufAction {
    /// Not set by the user — apply the workaround.
    Disable,
    /// Explicitly opted out. WebKitGTK reads presence, not value, so the only
    /// way to express "enabled" is for the variable not to exist.
    Remove,
    /// Set to something meaning "disabled". Already what we want; leave it.
    LeaveAlone,
}

#[cfg(target_os = "linux")]
fn dmabuf_action(current: Option<&str>) -> DmabufAction {
    match current {
        None => DmabufAction::Disable,
        Some(value) => match value.trim().to_ascii_lowercase().as_str() {
            "" | "0" | "false" | "no" => DmabufAction::Remove,
            _ => DmabufAction::LeaveAlone,
        },
    }
}

#[cfg(target_os = "linux")]
fn apply_webkit_wayland_workaround() {
    let current = std::env::var(DMABUF_VAR).ok();
    match dmabuf_action(current.as_deref()) {
        DmabufAction::Disable => std::env::set_var(DMABUF_VAR, "1"),
        DmabufAction::Remove => std::env::remove_var(DMABUF_VAR),
        DmabufAction::LeaveAlone => {}
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::{dmabuf_action, DmabufAction};

    #[test]
    fn unset_gets_the_workaround() {
        assert_eq!(dmabuf_action(None), DmabufAction::Disable);
    }

    #[test]
    fn falsey_values_opt_out_by_removing_the_variable() {
        // The bug this replaces: these all previously read as "user set it,
        // leave it alone", and WebKitGTK then disabled DMA-BUF anyway because
        // it only checks presence. There was no way to ask for the GPU path.
        for value in ["0", "false", "no", "", "  0  ", "FALSE", "No"] {
            assert_eq!(
                dmabuf_action(Some(value)),
                DmabufAction::Remove,
                "{value:?} should opt out"
            );
        }
    }

    #[test]
    fn other_values_are_left_alone() {
        for value in ["1", "true", "yes", "anything"] {
            assert_eq!(
                dmabuf_action(Some(value)),
                DmabufAction::LeaveAlone,
                "{value:?} should be left alone"
            );
        }
    }
}

fn main() {
    #[cfg(target_os = "linux")]
    apply_webkit_wayland_workaround();

    triple_c_lib::run()
}
