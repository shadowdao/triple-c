//! The stand-in `build.rs` writes for the bundled `session-manager-plugin` sidecar in a
//! **debug** build that has no real one, and how the app recognises it.
//!
//! `bundle.externalBin` makes tauri-build fail outright when the per-target file is missing,
//! for `cargo test`, `cargo check` and `tauri dev` as much as for a release. The real binary
//! is built from AWS's pinned source by `scripts/build-ssm-plugin.sh` (`.ps1` on Windows), with
//! a downloaded Go toolchain; requiring that for every test run would put a Go build and the
//! network on the path of every `cargo test`. So debug builds get this placeholder instead, a
//! release build refuses it (`build.rs`), and `host::ssm::find_plugin` skips it and falls back to
//! an installed plugin.
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
          app/src-tauri/binaries has no real one; run scripts/build-ssm-plugin.sh \
          (scripts/build-ssm-plugin.ps1 on Windows) to build the real plugin.\n",
    );
    v
}

/// Whether `bytes` (the start of a file) is a placeholder.
pub fn is_placeholder(bytes: &[u8]) -> bool {
    bytes.starts_with(MARKER)
}
