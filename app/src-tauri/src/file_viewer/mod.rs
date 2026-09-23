//! The terminal file viewer: one OS window per clicked path.
//!
//! Every window is a `file-viewer-<n>` label registered in [`registry::ViewerRegistry`];
//! the commands in `commands/file_viewer_commands.rs` gate on the label and act only on
//! the caller's own entry, which is why nothing here takes a path from a window.
//!
//! `file-viewer-*` is also the `windows` glob of `capabilities/file-viewer.json`, which grants
//! exactly the five `viewer_*` commands and nothing else. Labels are minted only here; a window
//! created anywhere else with a matching label would inherit those grants.

pub mod poll;
pub mod registry;
pub mod resolve;
pub mod window;
pub mod write;

/// Spec §3: the 21st click is refused with a toast.
pub const MAX_VIEWER_WINDOWS: usize = 20;
pub const VIEWER_LABEL_PREFIX: &str = "file-viewer-";

pub fn is_viewer_label(label: &str) -> bool {
    label
        .strip_prefix(VIEWER_LABEL_PREFIX)
        .is_some_and(|rest| !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_numbered_viewer_labels_pass() {
        assert!(is_viewer_label("file-viewer-1"));
        assert!(is_viewer_label("file-viewer-20"));
        assert!(!is_viewer_label("file-viewer-"));
        assert!(!is_viewer_label("file-viewer-x"));
        assert!(!is_viewer_label("main"));
        assert!(!is_viewer_label("browser-view-abc"));
    }

    /// Both Vite's dev server and Tauri's asset lookup fall back to `index.html`
    /// when `viewer.html` is missing, so a broken entry opens the *main app* in
    /// the viewer window with no error anywhere. Pin the two files the entry needs.
    #[test]
    fn the_viewer_entry_exists_and_is_a_vite_input() {
        let app_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let html = std::fs::read_to_string(app_dir.join("viewer.html")).expect("app/viewer.html");
        assert!(html.contains("/src/viewer/main.tsx"));
        assert!(!html.contains("<style"), "an inline <style> makes Tauri add a style nonce, which disables 'unsafe-inline' and breaks CodeMirror");
        let vite = std::fs::read_to_string(app_dir.join("vite.config.ts")).expect("vite.config.ts");
        assert!(vite.contains("viewer.html"), "vite.config.ts must list viewer.html in build.rollupOptions.input");
    }

    #[derive(serde::Deserialize)]
    struct Capability {
        windows: Vec<String>,
        permissions: Vec<String>,
    }

    /// Task 12: a substring check on the capability JSON (the form this test used to take)
    /// only proves a permission string appears *somewhere* in the file — it would not catch
    /// `windows` widened past `file-viewer-*`, nor an extra grant slipped in beside the ones
    /// this window actually needs. Parse both capability files and pin `windows`/`permissions`
    /// exactly, so a later widening of either file is a failing test, not a silent threat-model
    /// drift — this file *is* the reviewed threat model of record (see its own description).
    #[test]
    fn the_viewer_capability_grants_exactly_the_reviewed_windows_and_permissions() {
        let app_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let raw = std::fs::read_to_string(app_dir.join("src-tauri/capabilities/file-viewer.json"))
            .expect("capabilities/file-viewer.json");
        let cap: Capability = serde_json::from_str(&raw).expect("file-viewer.json must be valid JSON");

        assert_eq!(cap.windows, vec!["file-viewer-*"]);

        let mut permissions = cap.permissions;
        permissions.sort();
        assert_eq!(
            permissions,
            vec![
                // App commands (bare): the five viewer commands, and nothing else — build.rs
                // refuses any other bare grant in this file.
                "allow-viewer-choose-file",
                "allow-viewer-get-state",
                "allow-viewer-poll-file",
                "allow-viewer-read-file",
                "allow-viewer-write-file",
                // Plugin/core grants, unchanged.
                "core:event:allow-listen",
                "core:event:allow-unlisten",
                "core:webview:allow-internal-toggle-devtools",
                "core:window:allow-destroy",
            ]
        );
    }

    /// The main window's capability file must stay scoped to `main` — a `windows` list that
    /// grew to include `file-viewer-*` would hand every viewer window the dialog/store surface
    /// `default.json` grants `main`, which is a much larger IPC surface than the one
    /// `file-viewer.json` was deliberately kept small.
    #[test]
    fn the_default_capability_is_scoped_to_the_main_window_only() {
        let app_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let raw = std::fs::read_to_string(app_dir.join("src-tauri/capabilities/default.json"))
            .expect("capabilities/default.json");
        let cap: Capability = serde_json::from_str(&raw).expect("default.json must be valid JSON");

        assert_eq!(cap.windows, vec!["main"]);
    }
}
