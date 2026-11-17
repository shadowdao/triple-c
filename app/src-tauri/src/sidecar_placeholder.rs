//! The stand-in `build.rs` writes for the bundled `session-manager-plugin` sidecar in a
//! **debug** build that has no real one, and how the app recognises it.
//!
//! `bundle.externalBin` makes tauri-build fail outright when the per-target file is missing,
//! for `cargo test`, `cargo check` and `tauri dev` as much as for a release. The real binary
//! comes from `scripts/fetch-ssm-plugin.sh` (`.ps1` on Windows), which downloads a pinned,
//! hash-checked AWS release; requiring that for every test run would put AWS's S3 on the path
//! of every `cargo test`. So debug builds get this placeholder instead, a release build refuses
//! it (`build.rs`), and `host::ssm::find_plugin` skips it and falls back to an installed plugin.
//!
//! Shared by `build.rs` (through `#[path]`) and the app, so the two cannot disagree.

/// The first bytes of a placeholder. Not an executable format on any platform.
pub const MARKER: &[u8] = b"TRIPLE-C SSM PLUGIN PLACEHOLDER";

/// The whole placeholder file.
#[allow(dead_code)] // build.rs only
pub fn contents() -> Vec<u8> {
    let mut v = MARKER.to_vec();
    v.extend_from_slice(
        b"\nNot the AWS Session Manager plugin. Debug builds get this when \
          app/src-tauri/binaries has no real one; run scripts/fetch-ssm-plugin.sh \
          (scripts/fetch-ssm-plugin.ps1 on Windows) to bundle the real plugin.\n",
    );
    v
}

/// Whether `bytes` (the start of a file) is a placeholder.
pub fn is_placeholder(bytes: &[u8]) -> bool {
    bytes.starts_with(MARKER)
}
