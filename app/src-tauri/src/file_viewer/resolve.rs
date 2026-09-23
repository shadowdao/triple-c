//! Turning what Claude printed into a container path that exists.
//!
//! Relative paths are the common case (Claude prints project-relative paths). The
//! terminal exec's cwd is `/workspace`, and each project path is mounted at
//! `/workspace/<mount_name>`, so those are the roots probed, in that order. The probe
//! is one exec as the container user and prints `realpath -e` of every candidate that
//! is a regular file: `fetch_container_file` refuses a symlink, so the registry must
//! hold the resolved path, not the one that was clicked.

use crate::commands::file_commands::validate_container_path;
use crate::docker::exec::exec_oneshot_streams_as;

pub const MAX_CANDIDATES: usize = 16;
const MAX_RAW_LEN: usize = 4096;

/// `$@` are the candidates. For each regular file, print its resolved path.
pub const PROBE_SCRIPT: &str = r#"for c in "$@"; do if test -f "$c"; then realpath -e -- "$c" 2>/dev/null; fi; done; exit 0"#;

pub fn candidate_paths(raw: &str, mount_names: &[String]) -> Result<Vec<String>, String> {
    if raw.is_empty() {
        return Err("The path is empty.".into());
    }
    if raw.len() > MAX_RAW_LEN {
        return Err("The path is too long.".into());
    }
    if raw.contains('\0') {
        return Err("The path contains a NUL byte.".into());
    }
    if raw.split('/').any(|seg| seg == "..") {
        return Err(format!("{} climbs out of its folder with `..`; refusing.", raw));
    }

    if raw.starts_with('/') {
        let normalised = collapse(raw);
        validate_container_path("File", &normalised)?;
        return Ok(vec![normalised]);
    }

    let rel = collapse(raw.strip_prefix("./").unwrap_or(raw));
    let rel = rel.trim_start_matches("./");
    if rel.is_empty() {
        return Err("The path is empty.".into());
    }

    let mut out: Vec<String> = Vec::new();
    let mut push = |candidate: String| {
        if out.len() < MAX_CANDIDATES && !out.contains(&candidate) {
            out.push(candidate);
        }
    };
    push(format!("/workspace/{}", rel));
    for mount in mount_names {
        if mount.is_empty() || mount.contains('/') || mount == "." || mount == ".." {
            continue;
        }
        push(format!("/workspace/{}/{}", mount, rel));
    }
    for c in &out {
        validate_container_path("File", c)?;
    }
    Ok(out)
}

/// `a//b/./c` → `a/b/c`. Never touches `..` (rejected before this runs).
fn collapse(path: &str) -> String {
    let absolute = path.starts_with('/');
    let joined = path
        .split('/')
        .filter(|seg| !seg.is_empty() && *seg != ".")
        .collect::<Vec<_>>()
        .join("/");
    if absolute { format!("/{}", joined) } else { joined }
}

/// One resolved path per line; anything that is not an absolute, valid container path is
/// dropped (the script's own diagnostics go to stderr, but a hostile `realpath` output is
/// still container-authored text).
pub fn parse_probe_output(stdout: &str) -> Vec<String> {
    let mut seen: Vec<String> = Vec::new();
    for line in stdout.lines() {
        let line = line.trim();
        if line.is_empty() || validate_container_path("File", line).is_err() {
            continue;
        }
        if !seen.iter().any(|s| s == line) {
            seen.push(line.to_string());
        }
    }
    seen
}

pub async fn probe_candidates(
    container_id: &str,
    candidates: &[String],
) -> Result<Vec<String>, String> {
    let mut cmd: Vec<String> = vec!["sh".into(), "-c".into(), PROBE_SCRIPT.into(), "probe".into()];
    cmd.extend(candidates.iter().cloned());
    let (stdout, _stderr, _code) =
        exec_oneshot_streams_as(container_id, "claude", cmd, Vec::new()).await?;
    Ok(parse_probe_output(&stdout))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mounts(names: &[&str]) -> Vec<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn an_absolute_path_is_its_own_only_candidate() {
        let c = candidate_paths("/workspace/api/src/main.rs", &mounts(&["api"])).unwrap();
        assert_eq!(c, vec!["/workspace/api/src/main.rs"]);
    }

    #[test]
    fn a_relative_path_probes_workspace_then_each_mount() {
        let c = candidate_paths("src/main.rs", &mounts(&["api", "web"])).unwrap();
        assert_eq!(
            c,
            vec!["/workspace/src/main.rs", "/workspace/api/src/main.rs", "/workspace/web/src/main.rs"]
        );
    }

    #[test]
    fn dot_prefix_and_duplicate_slashes_are_normalised_and_candidates_deduped() {
        let c = candidate_paths("./src//main.rs", &mounts(&["api", "api", ""])).unwrap();
        assert_eq!(c, vec!["/workspace/src/main.rs", "/workspace/api/src/main.rs"]);
    }

    #[test]
    fn traversal_nul_and_oversize_are_refused() {
        assert!(candidate_paths("../etc/passwd", &[]).is_err());
        assert!(candidate_paths("src/../../x", &[]).is_err());
        assert!(candidate_paths("/workspace/../etc/passwd", &[]).is_err());
        assert!(candidate_paths("a\0b", &[]).is_err());
        assert!(candidate_paths("", &[]).is_err());
        assert!(candidate_paths(&"a".repeat(5000), &[]).is_err());
    }

    #[test]
    fn candidate_list_is_capped() {
        let many: Vec<String> = (0..40).map(|i| format!("m{}", i)).collect();
        let c = candidate_paths("x.rs", &many).unwrap();
        assert_eq!(c.len(), MAX_CANDIDATES);
    }

    #[test]
    fn probe_output_keeps_valid_resolved_regular_files_only() {
        let out = "/workspace/api/src/main.rs\n/workspace/api/src/main.rs\n\nrelative/junk\n/etc/../x\n/workspace/web/src/main.rs\n";
        assert_eq!(
            parse_probe_output(out),
            vec!["/workspace/api/src/main.rs", "/workspace/web/src/main.rs"]
        );
    }

    #[test]
    fn the_probe_script_prints_resolved_paths_of_regular_files() {
        // Shape assertions: the script is data handed to `sh -c`, and these are the
        // three things a later edit must not lose.
        assert!(PROBE_SCRIPT.contains("test -f"));
        assert!(PROBE_SCRIPT.contains("realpath -e --"));
        assert!(PROBE_SCRIPT.contains("for c in \"$@\""));
    }
}
