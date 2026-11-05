//! Microphone access for the main window on Linux (WebKitGTK).
//!
//! WebKitGTK denies `getUserMedia` unless the embedder (a) has the
//! `enable-media-stream` setting on and (b) answers the webview's
//! `permission-request` signal. wry does neither, so without this the speech-
//! to-text button fails with
//! `NotAllowedError: The request is not allowed by the user agent ...` even
//! when GStreamer can see the microphone.
//!
//! Scope is deliberately narrow:
//!   * only the `main` window — the file-viewer windows and any embedded
//!     browser view load different content and get no grant;
//!   * only audio capture — a request that also asks for a camera, and every
//!     other permission type (geolocation, notifications, ...), is denied.

use tauri::{AppHandle, Manager};
use webkit2gtk::glib::prelude::*;
use webkit2gtk::{
    PermissionRequestExt, SettingsExt, UserMediaPermissionRequest, UserMediaPermissionRequestExt,
    WebView, WebViewExt,
};

/// Whether a media permission request should be granted: microphone only.
pub(crate) fn allow_media_request(for_audio: bool, for_video: bool) -> bool {
    for_audio && !for_video
}

pub fn enable_microphone(app: &AppHandle) {
    let Some(window) = app.get_webview_window("main") else {
        log::warn!("Microphone: no main window, capture will stay disabled");
        return;
    };
    let result = window.with_webview(|platform| {
        let webview: WebView = platform.inner();
        if let Some(settings) = WebViewExt::settings(&webview) {
            settings.set_enable_media_stream(true);
        }
        webview.connect_permission_request(|_, request| {
            if let Some(media) = request.downcast_ref::<UserMediaPermissionRequest>() {
                let ok =
                    allow_media_request(media.is_for_audio_device(), media.is_for_video_device());
                if ok {
                    request.allow();
                } else {
                    request.deny();
                }
                log::info!(
                    "Microphone: media permission request {}",
                    if ok { "allowed" } else { "denied" }
                );
            } else {
                request.deny();
            }
            true // handled; do not fall through to WebKit's default
        });
    });
    if let Err(e) = result {
        log::warn!("Microphone: could not configure the webview: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::allow_media_request;

    #[test]
    fn grants_audio_only() {
        assert!(allow_media_request(true, false));
    }

    #[test]
    fn denies_camera_and_mixed_requests() {
        assert!(!allow_media_request(false, true));
        assert!(!allow_media_request(true, true));
        assert!(!allow_media_request(false, false));
    }
}
