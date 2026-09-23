//! One cheap exec per tick: the file's full hash and size, or "gone".
//!
//! This is what the 2 s poll asks, instead of re-downloading up to 1 MiB of archive per
//! window per tick. The hash is coreutils `sha256sum`, which equals `write::sha256_hex`
//! of the bytes whenever the read was not truncated — the only case in which the
//! editor uses a hash as its save base.

use serde::Serialize;

use crate::docker::exec::exec_oneshot_streams_as;

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct ViewerPoll {
    pub exists: bool,
    pub hash: Option<String>,
    pub size: Option<u64>,
}

/// Exit 4 = gone. A failure after `test -f` passed is re-checked: if the file vanished
/// in between (deleted while being hashed), that is "gone", not an error (M6).
pub const POLL_SCRIPT: &str = r#"test -f "$1" || exit 4
sha256sum -- "$1" && stat -c %s -- "$1" && exit 0
test -f "$1" || exit 4
exit 1"#;

pub fn parse_poll_output(code: i64, stdout: &str) -> ViewerPoll {
    if code == 4 {
        return ViewerPoll { exists: false, hash: None, size: None };
    }
    let mut lines = stdout.lines();
    let hash = lines
        .next()
        .and_then(|l| l.split_whitespace().next())
        // GNU `sha256sum` prefixes the line with `\` when the name contains a
        // backslash or a newline; strip it before validating the hex (P15).
        .map(|h| h.trim_start_matches('\\'))
        .filter(|h| super::write::is_sha256_hex(h))
        .map(str::to_string);
    let size = lines.next().and_then(|l| l.trim().parse::<u64>().ok());
    ViewerPoll { exists: true, hash, size }
}

pub async fn poll_file(container_id: &str, container_path: &str) -> Result<ViewerPoll, String> {
    let cmd = vec![
        "sh".to_string(),
        "-c".to_string(),
        POLL_SCRIPT.to_string(),
        "poll".to_string(),
        container_path.to_string(),
    ];
    let (stdout, stderr, code) =
        exec_oneshot_streams_as(container_id, "claude", cmd, Vec::new()).await?;
    if code != 0 && code != 4 {
        return Err(format!(
            "Could not check the file: {}",
            crate::commands::file_commands::clip_container_text(&stderr)
        ));
    }
    Ok(parse_poll_output(code, &stdout))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_present_file_yields_hash_and_size() {
        let out = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855  /workspace/x\n42\n";
        assert_eq!(
            parse_poll_output(0, out),
            ViewerPoll {
                exists: true,
                hash: Some("e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855".into()),
                size: Some(42)
            }
        );
    }

    #[test]
    fn exit_four_means_gone() {
        assert_eq!(parse_poll_output(4, ""), ViewerPoll { exists: false, hash: None, size: None });
    }

    #[test]
    fn garbage_is_not_a_hash() {
        let p = parse_poll_output(0, "not a hash  /x\nabc\n");
        assert_eq!(p, ViewerPoll { exists: true, hash: None, size: None });
    }

    #[test]
    fn the_script_tests_existence_before_hashing() {
        assert!(POLL_SCRIPT.contains("test -f \"$1\" || exit 4"));
        assert!(POLL_SCRIPT.contains("sha256sum -- \"$1\""));
        assert!(POLL_SCRIPT.contains("stat -c %s -- \"$1\""));
    }

    #[cfg(unix)]
    fn run_poll_script(path_env: Option<&str>, target: &std::path::Path) -> (i64, String, String) {
        let mut cmd = std::process::Command::new("sh");
        if let Some(p) = path_env {
            cmd.env("PATH", p);
        }
        let out = cmd.arg("-c").arg(POLL_SCRIPT).arg("poll").arg(target).output().unwrap();
        (
            out.status.code().unwrap_or(-1) as i64,
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }

    #[cfg(unix)]
    fn test_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("tc-poll-{}-{}", name, uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[cfg(unix)]
    #[test]
    fn on_the_host_the_poll_script_reports_hash_size_and_gone() {
        let dir = test_dir("plain");
        let target = dir.join("t.txt");
        std::fs::write(&target, b"hello\n").unwrap();
        let (code, stdout, stderr) = run_poll_script(None, &target);
        assert_eq!(code, 0, "stderr={stderr}");
        let p = parse_poll_output(code, &stdout);
        assert_eq!(p.hash.as_deref(), Some(super::super::write::sha256_hex(b"hello\n").as_str()));
        assert_eq!(p.size, Some(6));

        let (code, _, _) = run_poll_script(None, &dir.join("missing"));
        assert_eq!(code, 4);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// M6: the file is deleted after `test -f` passed but before `sha256sum` read it
    /// (a `sha256sum` shim on PATH deletes it and fails). That is "gone", not an error
    /// the viewer would have to explain.
    #[cfg(unix)]
    #[test]
    fn on_the_host_a_file_deleted_mid_poll_reads_as_gone() {
        use std::os::unix::fs::PermissionsExt;
        let dir = test_dir("race");
        let bin = dir.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let shim = bin.join("sha256sum");
        std::fs::write(&shim, "#!/bin/sh\nrm -f -- \"$2\"\necho 'sha256sum: No such file or directory' >&2\nexit 1\n").unwrap();
        std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755)).unwrap();
        let target = dir.join("t.txt");
        std::fs::write(&target, b"x").unwrap();
        let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap_or_default());

        let (code, stdout, stderr) = run_poll_script(Some(&path), &target);

        assert_eq!(code, 4, "stderr={stderr}");
        assert_eq!(parse_poll_output(code, &stdout), ViewerPoll { exists: false, hash: None, size: None });
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A failure with the file still present stays a real error (exit 1), which
    /// `poll_file` turns into "Could not check the file: …".
    #[cfg(unix)]
    #[test]
    fn on_the_host_a_hash_failure_on_a_present_file_is_an_error() {
        use std::os::unix::fs::PermissionsExt;
        let dir = test_dir("fail");
        let bin = dir.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let shim = bin.join("sha256sum");
        std::fs::write(&shim, "#!/bin/sh\necho 'sha256sum: Permission denied' >&2\nexit 1\n").unwrap();
        std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755)).unwrap();
        let target = dir.join("t.txt");
        std::fs::write(&target, b"x").unwrap();
        let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap_or_default());

        let (code, _stdout, stderr) = run_poll_script(Some(&path), &target);

        assert_eq!(code, 1, "stderr={stderr}");
        assert!(stderr.contains("Permission denied"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// P15: a path containing a backslash makes GNU `sha256sum` prefix the whole
    /// line with `\`; that must not blind change detection by yielding `hash: None`.
    #[test]
    fn a_backslash_prefixed_hash_is_still_recognised() {
        let out = "\\e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855  /workspace/x\\y\n7\n";
        let p = parse_poll_output(0, out);
        assert_eq!(
            p.hash.as_deref(),
            Some("e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855")
        );
        assert_eq!(p.size, Some(7));
    }
}
