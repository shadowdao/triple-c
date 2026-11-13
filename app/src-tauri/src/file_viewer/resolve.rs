//! Turning what Claude printed into a container path that exists.
//!
//! Relative paths are the common case (Claude prints project-relative paths). The
//! terminal exec's cwd is `/workspace`, and each project path is mounted at
//! `/workspace/<mount_name>`, so those are the roots probed, in that order.
//! A relative path none of them has is then tried under `/home/claude` and `/tmp`
//! ([`fallback_candidate_paths`]); `~/p` is `/home/claude/p`. The probe
//! is one exec as the container user and prints `realpath -e` of every candidate that
//! is a regular file: `fetch_container_file` refuses a symlink, so the registry must
//! hold the resolved path, not the one that was clicked.

use crate::commands::file_commands::validate_container_path;
use crate::docker::CONTAINER_HOME;
use crate::host::ContainerTarget;

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

    // Claude Code prints paths under $HOME as `~/...`. Only the bare `~/` form:
    // `~user/` names some other account's home and stays an ordinary name.
    if let Some(rest) = raw.strip_prefix("~/") {
        let rest = collapse(rest);
        if rest.is_empty() {
            return Err("The path is empty.".into());
        }
        let p = format!("{}/{}", CONTAINER_HOME, rest);
        validate_container_path("File", &p)?;
        return Ok(vec![p]);
    }

    let rel = relative_part(raw).ok_or_else(|| "The path is empty.".to_string())?;

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

/// Where a relative path is looked for when no project root has it, in order.
/// The container user's home, then `/tmp` (drops, pastes and scratch files).
/// Probed only after [`candidate_paths`] found nothing, so a project file never
/// turns into a "which one?" chooser because a same-named file sits in `/tmp`.
const FALLBACK_ROOTS: &[&str] = &[CONTAINER_HOME, "/tmp"];

/// The second tier for a relative path: [`FALLBACK_ROOTS`]`/<p>`. Empty for an
/// absolute or `~/` path (those name one place already) and for anything
/// [`candidate_paths`] refuses — it is always called after that succeeded, but
/// must not be a way around its checks.
pub fn fallback_candidate_paths(raw: &str) -> Vec<String> {
    if raw.starts_with('/') || raw.starts_with("~/") || candidate_paths(raw, &[]).is_err() {
        return Vec::new();
    }
    let Some(rel) = relative_part(raw) else { return Vec::new() };
    FALLBACK_ROOTS
        .iter()
        .map(|root| format!("{}/{}", root, rel))
        .filter(|c| validate_container_path("File", c).is_ok())
        .take(MAX_CANDIDATES)
        .collect()
}

/// `./a//b` → `a/b`; `None` when nothing is left.
fn relative_part(raw: &str) -> Option<String> {
    let rel = collapse(raw.strip_prefix("./").unwrap_or(raw));
    let rel = rel.trim_start_matches("./");
    (!rel.is_empty()).then(|| rel.to_string())
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
    container: &ContainerTarget,
    candidates: &[String],
) -> Result<Vec<String>, String> {
    let mut cmd: Vec<String> = vec!["sh".into(), "-c".into(), PROBE_SCRIPT.into(), "probe".into()];
    cmd.extend(candidates.iter().cloned());
    let (stdout, _stderr, _code) = container
        .exec_oneshot_streams_as("claude", cmd, Vec::new())
        .await?;
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
    fn an_absolute_tmp_path_is_accepted_as_is() {
        // Claude Code's scratchpad: long, and with a segment that starts with `-`.
        let p = "/tmp/claude-1000/-workspace/5e4ab145-915e-4902-9329-e039e2a07562/scratchpad/notes.md";
        assert_eq!(candidate_paths(p, &mounts(&["api"])).unwrap(), vec![p]);
    }

    #[test]
    fn a_home_relative_path_is_the_container_users_home() {
        // Claude Code prints paths under $HOME as `~/...`.
        let c = candidate_paths("~/.claude/plans/plan.md", &mounts(&["api"])).unwrap();
        assert_eq!(c, vec!["/home/claude/.claude/plans/plan.md"]);
        assert!(candidate_paths("~/", &[]).is_err());
        assert!(candidate_paths("~/../etc/passwd", &[]).is_err());
        // `~user/` is not expanded; it stays an ordinary relative name.
        assert_eq!(candidate_paths("~bob/x.md", &[]).unwrap(), vec!["/workspace/~bob/x.md"]);
    }

    #[test]
    fn a_relative_path_falls_back_to_home_and_tmp() {
        assert_eq!(
            fallback_candidate_paths("scratch/notes.md"),
            vec!["/home/claude/scratch/notes.md", "/tmp/scratch/notes.md"]
        );
        assert_eq!(fallback_candidate_paths("./notes.md"), vec!["/home/claude/notes.md", "/tmp/notes.md"]);
        // Absolute, home-relative and refused paths have no fallbacks.
        assert!(fallback_candidate_paths("/tmp/notes.md").is_empty());
        assert!(fallback_candidate_paths("~/notes.md").is_empty());
        assert!(fallback_candidate_paths("../notes.md").is_empty());
        assert!(fallback_candidate_paths("").is_empty());
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
