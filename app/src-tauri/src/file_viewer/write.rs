//! Saving: stage in `/tmp`, then swap in as the container user.
//!
//! The Docker archive API writes as root, so it is used for exactly one thing — landing
//! the payload at `/tmp/triple-c-viewer-<uuid>`, owned by the container user (the
//! existing `write_file_to_container`). Everything that touches the *target directory*
//! runs in an exec as `claude`, so a save can do nothing the user's own shell could not.
//! A non-root process cannot `chown`, so the saved file is owned by the container user,
//! as it would be after Claude Code edited it; mode is kept with `chmod --reference`.

use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::commands::file_commands::clip_container_text;
use crate::docker::exec::{exec_oneshot_streams_as, ExecSessionManager};

/// Spec §4/§5: only untruncated (≤ 1 MiB) text is editable, so nothing larger is saved.
pub const MAX_WRITE_BYTES: usize = 1024 * 1024;

pub fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|b| format!("{:02x}", b)).collect()
}

pub fn is_sha256_hex(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// `$1` target, `$2` staged payload in /tmp, `$3` the hash the editor loaded from.
/// Exit 1 = a step failed (unreadable target, a failed stage/replace, …), 3 = changed
/// on disk, 4 = gone, 5 = the target is not writable by the container user; stdout on
/// success is `sha256sum` of the target *after* the write. That is not necessarily the
/// hash of what we wrote: another writer (Claude Code, on the same file) can land
/// between `mv` and `sha256sum`. `saved_file` therefore takes the save's base from the
/// bytes and only reports this one as what the disk held afterwards (M2).
///
/// P15: `sha256sum -- "$target"` prefixes its whole line with `\` when the path
/// contains a backslash or a newline, so `$actual` has that prefix stripped before
/// it is compared with `$expect` (which never carries one) — otherwise such a path
/// would conflict forever.
///
/// I1: `$actual` is read from a plain `sha256sum` command substitution, not a
/// pipeline into `cut` — POSIX sh has no `pipefail`, so `cmd | cut … || exit 1` tests
/// only `cut`'s exit status and an unreadable file (EACCES, EIO) fell through as a
/// false "changed on disk" conflict (empty `$actual` never equals `$expect`) instead
/// of a real error, hiding the actual failure from the user and from `classify_write`.
///
/// I2/M3: `$staged` is created by `mktemp` (exclusive — never follows a planted
/// symlink or stale leftover at that name) and is part of the `EXIT` trap from the
/// moment it is assigned, so a failure at any later step (`cp`, `chmod`, `mv`) cannot
/// leave a partial `.<name>.triple-c-<suffix>` behind in the user's own directory —
/// including on a signal, for the steps after the trap covers it.
pub const WRITE_SCRIPT: &str = r#"target=$1; tmp=$2; expect=$3
staged=
trap 'rm -f -- "$tmp" ${staged:+"$staged"}' EXIT
test -f "$target" || exit 4
actual=$(sha256sum -- "$target") || exit 1
actual=${actual%% *}; actual=${actual#\\}
[ "$actual" = "$expect" ] || exit 3
# I3: the file's own mode is a boundary the user set from outside the container (0444,
# a different owning uid, a read-only bind mount, …). Replacing it via rename or
# truncating it in place would silently cross that boundary even though `claude` is
# allowed to — an editor such as vim, or a plain `echo > file` in the user's own shell,
# would refuse. This is stricter than spec §5 step 3's literal "if the directory is
# writable" branch, which never looks at the file's own permissions; the branch below
# only ever chooses *how* to write, never *whether*.
#
# The rename branch replaces whatever is at "$target" (a symlink planted there after
# the window opened is replaced, not followed). The in-place `cat >` fallback, taken
# only for a writable file in a read-only directory, DOES follow such a symlink and
# writes through it. That is accepted: the write runs as `claude`, so it can reach
# nothing Claude Code in the same container cannot already write.
[ -w "$target" ] || { echo "The file is read-only for the container user." >&2; exit 5; }
dir=$(dirname -- "$target"); name=$(basename -- "$target")
if [ -w "$dir" ]; then
  staged=$(mktemp -- "$dir/.$name.triple-c-XXXXXX") || exit 1
  cp -- "$tmp" "$staged" || exit 1
  chmod --reference="$target" "$staged" 2>/dev/null
  mv -f -- "$staged" "$target" || exit 1
else
  cat -- "$tmp" > "$target" || exit 1
fi
sha256sum -- "$target""#;

/// A save refused because the file changed since its base hash. The frontend matches
/// this prefix; its copy lives in `app/src/viewer/ipcMessages.ts` (pinned by a test).
pub const CONFLICT_PREFIX: &str = "conflict:";
/// A save refused because the file no longer exists; mirrored in `ipcMessages.ts`.
pub const GONE_PREFIX: &str = "gone:";
/// The read-only refusal. The script echoes the same sentence (pinned by a test), but
/// the caller always gets this constant, whatever the script printed; mirrored in
/// `ipcMessages.ts`.
pub const READ_ONLY_MESSAGE: &str = "The file is read-only for the container user.";

/// I3: distinct from the generic failure code so the caller can hand back a specific,
/// readable message instead of whatever the script's own diagnostic text says.
const EXIT_READ_ONLY: i64 = 5;

pub enum WriteOutcome {
    Saved(String),
    Conflict,
    Gone,
    Failed(String),
}

pub fn classify_write(code: i64, stdout: &str, stderr: &str) -> WriteOutcome {
    match code {
        3 => WriteOutcome::Conflict,
        4 => WriteOutcome::Gone,
        EXIT_READ_ONLY => WriteOutcome::Failed(READ_ONLY_MESSAGE.into()),
        0 => match stdout
            .split_whitespace()
            .next()
            .map(|h| h.trim_start_matches('\\'))
            .filter(|h| is_sha256_hex(h))
        {
            Some(h) => WriteOutcome::Saved(h.to_string()),
            None => WriteOutcome::Failed(
                "The container did not report the saved file's hash.".into(),
            ),
        },
        _ => WriteOutcome::Failed(clip_container_text(stderr)),
    }
}

/// The write script's argv beyond `sh -c SCRIPT`: `$0=save`, `$1=target`, `$2=tmp`,
/// `$3=base_hash` — pulled out pure so the argument shape has a unit test (P8).
fn write_command(target: &str, tmp: &str, base_hash: &str) -> Vec<String> {
    vec![
        "sh".to_string(),
        "-c".to_string(),
        WRITE_SCRIPT.to_string(),
        "save".to_string(),
        target.to_string(),
        tmp.to_string(),
        base_hash.to_string(),
    ]
}

/// Refuses a payload too large to be editable, or a malformed base hash, before
/// anything is staged in the container (P8).
fn check_write_input(len: usize, base_hash: &str) -> Result<(), String> {
    if len > MAX_WRITE_BYTES {
        return Err("Files over 1 MiB are read-only in the viewer.".into());
    }
    if !is_sha256_hex(base_hash) {
        return Err("The editor's base hash is malformed; reload the file.".into());
    }
    Ok(())
}

/// What a successful save reports: `hash` is the new base, `sha256_hex` of the bytes
/// we wrote; `disk_hash` is what the container hashed right after the swap. They differ
/// only when another writer landed in between, and then the editor must show "Changed
/// on disk" rather than adopt the other writer's hash as its base (M2).
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct SavedFile {
    pub hash: String,
    pub disk_hash: String,
}

/// `viewer_write_file`'s result, pure so the error-prefix contract has a unit test.
fn saved_file(outcome: WriteOutcome, bytes: &[u8]) -> Result<SavedFile, String> {
    match outcome {
        WriteOutcome::Saved(disk_hash) => Ok(SavedFile { hash: sha256_hex(bytes), disk_hash }),
        WriteOutcome::Conflict => Err(format!(
            "{} the file changed on disk since it was loaded.",
            CONFLICT_PREFIX
        )),
        WriteOutcome::Gone => Err(format!("{} the file no longer exists.", GONE_PREFIX)),
        WriteOutcome::Failed(msg) => Err(format!("Could not save the file: {}", msg)),
    }
}

pub async fn write_file(
    container_id: &str,
    exec_manager: &ExecSessionManager,
    target: &str,
    bytes: &[u8],
    base_hash: &str,
) -> Result<SavedFile, String> {
    check_write_input(bytes.len(), base_hash)?;
    let tmp_name = format!("triple-c-viewer-{}", uuid::Uuid::new_v4().simple());
    let tmp_path = exec_manager
        .write_file_to_container(container_id, &tmp_name, bytes)
        .await?;
    let cmd = write_command(target, &tmp_path, base_hash);
    let (stdout, stderr, code) =
        exec_oneshot_streams_as(container_id, "claude", cmd, Vec::new()).await?;
    saved_file(classify_write(code, &stdout, &stderr), bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_matches_coreutils() {
        // `printf 'hello\n' | sha256sum`
        assert_eq!(
            sha256_hex(b"hello\n"),
            "5891b5b522d5df086d0ff0b110fbd9d21bb4fc7163af34d08286a2e846f6be03"
        );
        assert!(is_sha256_hex(&sha256_hex(b"")));
        assert!(!is_sha256_hex("ABC"));
        assert!(!is_sha256_hex(&"g".repeat(64)));
    }

    #[test]
    fn exit_codes_map_to_outcomes() {
        let h = "5891b5b522d5df086d0ff0b110fbd9d21bb4fc7163af34d08286a2e846f6be03";
        assert!(matches!(classify_write(0, &format!("{}  /x\n", h), ""), WriteOutcome::Saved(s) if s == h));
        assert!(matches!(classify_write(3, "", ""), WriteOutcome::Conflict));
        assert!(matches!(classify_write(4, "", ""), WriteOutcome::Gone));
        assert!(matches!(classify_write(1, "", "cp: Permission denied"), WriteOutcome::Failed(m) if m.contains("Permission denied")));
        // Success without a parseable hash is still a failure: the editor's base would be wrong.
        assert!(matches!(classify_write(0, "junk", ""), WriteOutcome::Failed(_)));
    }

    /// I3: exit 5 is the script's read-only refusal, and it must not be swallowed by
    /// the generic `_ => Failed(stderr)` arm — the caller gets a fixed, readable
    /// message regardless of exactly what the script printed.
    #[test]
    fn exit_five_is_a_distinct_read_only_refusal() {
        assert!(matches!(
            classify_write(5, "", "The file is read-only for the container user."),
            WriteOutcome::Failed(m) if m.contains("read-only")
        ));
    }

    /// M2: the new base is the hash of the bytes we wrote, never the script's
    /// post-`mv` hash, which may belong to a writer that landed after us.
    #[test]
    fn a_save_takes_its_base_from_the_written_bytes() {
        let ours = sha256_hex(b"new\n");
        let same = saved_file(WriteOutcome::Saved(ours.clone()), b"new\n").unwrap();
        assert_eq!(same, SavedFile { hash: ours.clone(), disk_hash: ours.clone() });

        let foreign = sha256_hex(b"someone else's\n");
        let raced = saved_file(WriteOutcome::Saved(foreign.clone()), b"new\n").unwrap();
        assert_eq!(raced.hash, ours, "the base must be what we wrote");
        assert_eq!(raced.disk_hash, foreign, "the foreign hash is reported, not adopted");
    }

    /// Important #4: the frontend matches these exact strings
    /// (`app/src/viewer/ipcMessages.ts`), so pin them here too.
    #[test]
    fn save_errors_keep_the_prefix_contract() {
        let conflict = saved_file(WriteOutcome::Conflict, b"").unwrap_err();
        assert!(conflict.starts_with("conflict:"), "{conflict}");
        assert_eq!(conflict, "conflict: the file changed on disk since it was loaded.");

        let gone = saved_file(WriteOutcome::Gone, b"").unwrap_err();
        assert!(gone.starts_with("gone:"), "{gone}");
        assert_eq!(gone, "gone: the file no longer exists.");

        let read_only = saved_file(classify_write(5, "", "whatever the script said"), b"").unwrap_err();
        assert_eq!(read_only, "Could not save the file: The file is read-only for the container user.");
        assert!(!read_only.starts_with(CONFLICT_PREFIX) && !read_only.starts_with(GONE_PREFIX));

        let other = saved_file(classify_write(1, "", "No space left on device"), b"").unwrap_err();
        assert_eq!(other, "Could not save the file: No space left on device");

        // The script's own refusal text is the same sentence the caller is given.
        assert!(WRITE_SCRIPT.contains(&format!("echo \"{}\" >&2; exit 5", READ_ONLY_MESSAGE)));
    }

    /// The TypeScript side keeps one copy of each matched string; a change on either
    /// side without the other fails here.
    #[test]
    fn the_frontend_copies_of_the_ipc_messages_match() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../src/viewer/ipcMessages.ts");
        let ts = std::fs::read_to_string(&path).expect("app/src/viewer/ipcMessages.ts");
        for (name, value) in [
            ("CONFLICT_PREFIX", CONFLICT_PREFIX),
            ("GONE_PREFIX", GONE_PREFIX),
            ("READ_ONLY_MESSAGE", READ_ONLY_MESSAGE),
            ("NOT_RUNNING_PREFIX", crate::commands::file_commands::NOT_RUNNING_PREFIX),
        ] {
            let line = format!("export const {} = \"{}\";", name, value);
            assert!(ts.contains(&line), "ipcMessages.ts must contain `{line}`");
        }
    }

    /// P15: a target path with a backslash makes `sha256sum` prefix the line;
    /// the parsed hash must still be recognised as the saved hash.
    #[test]
    fn a_backslash_prefixed_saved_hash_is_still_recognised() {
        let h = "5891b5b522d5df086d0ff0b110fbd9d21bb4fc7163af34d08286a2e846f6be03";
        assert!(matches!(
            classify_write(0, &format!("\\{}  /x\\y\n", h), ""),
            WriteOutcome::Saved(s) if s == h
        ));
    }

    #[test]
    fn the_write_script_checks_then_swaps_and_always_cleans_up() {
        for needle in [
            "test -f \"$target\" || exit 4",
            "exit 3",
            "chmod --reference=\"$target\"",
            "mv -f --",
            "cat -- \"$tmp\" > \"$target\"",
            // I2/M3: the trap covers the staged file too, and it comes from `mktemp`.
            "trap 'rm -f -- \"$tmp\" ${staged:+\"$staged\"}' EXIT",
            "mktemp -- \"$dir/.$name.triple-c-XXXXXX\"",
            // I1: a plain command substitution, not a pipeline `cut` could mask.
            "actual=$(sha256sum -- \"$target\") || exit 1",
            // I3: a read-only target is refused before any write is attempted.
            "[ -w \"$target\" ] || { echo \"The file is read-only for the container user.\" >&2; exit 5; }",
        ] {
            assert!(WRITE_SCRIPT.contains(needle), "missing: {}", needle);
        }
        // The old pipeline form must be gone, not merely superseded.
        assert!(!WRITE_SCRIPT.contains("cut -d' ' -f1"));
    }

    /// P8: the write script's test list is binding, and the argument order is
    /// exactly what a later edit could silently break.
    #[test]
    fn write_command_has_the_expected_argv_shape() {
        let cmd = write_command("/w/t.txt", "/tmp/x", "abc123");
        assert_eq!(
            cmd,
            vec![
                "sh".to_string(),
                "-c".to_string(),
                WRITE_SCRIPT.to_string(),
                "save".to_string(),
                "/w/t.txt".to_string(),
                "/tmp/x".to_string(),
                "abc123".to_string(),
            ]
        );
    }

    /// P8: the size cap and base-hash checks are unit-testable in isolation from
    /// the async `write_file`.
    #[test]
    fn check_write_input_refuses_oversized_payload_and_malformed_hash() {
        let h = "5891b5b522d5df086d0ff0b110fbd9d21bb4fc7163af34d08286a2e846f6be03";
        assert!(check_write_input(MAX_WRITE_BYTES, h).is_ok());
        assert!(check_write_input(MAX_WRITE_BYTES + 1, h).is_err());
        assert!(check_write_input(0, "not-a-hash").is_err());
    }

    // ── M10: WRITE_SCRIPT run for real, against a temp dir on the host ──────────
    //
    // The needle test above only proves the script *contains* certain substrings; it
    // cannot catch the pipefail-shaped bug I1 was (the needle text was correct, the
    // shell semantics were not). These run the exact `sh -c SCRIPT save target tmp
    // hash` invocation `write_command` builds, so they pin the exit codes and cleanup
    // behaviour that `write_file`/`classify_write` actually depend on. `sh` and the
    // coreutils used here (`sha256sum`, `mktemp`, `dirname`, `basename`) are present
    // on dev machines and CI alike.

    #[cfg(unix)]
    fn run_write_script(
        target: &std::path::Path,
        tmp: &std::path::Path,
        base_hash: &str,
    ) -> (i32, String, String) {
        let out = std::process::Command::new("sh")
            .arg("-c")
            .arg(WRITE_SCRIPT)
            .arg("save")
            .arg(target)
            .arg(tmp)
            .arg(base_hash)
            .output()
            .expect("sh must be on PATH to run this test");
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }

    #[cfg(unix)]
    fn unique_test_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("tc-write-{}-{}", name, uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[cfg(unix)]
    #[test]
    fn on_the_host_a_clean_save_replaces_the_file_and_cleans_up() {
        let dir = unique_test_dir("clean");
        let target = dir.join("t.txt");
        let tmp = dir.join("payload");
        std::fs::write(&target, b"old\n").unwrap();
        std::fs::write(&tmp, b"new\n").unwrap();
        let base = sha256_hex(b"old\n");

        let (code, stdout, stderr) = run_write_script(&target, &tmp, &base);

        assert_eq!(code, 0, "stdout={stdout} stderr={stderr}");
        let new_hash = sha256_hex(b"new\n");
        assert!(stdout.contains(&new_hash), "stdout={stdout}");
        // With no other writer, the reported disk hash is ours, so no conflict is shown.
        let saved = saved_file(classify_write(code as i64, &stdout, &stderr), b"new\n").unwrap();
        assert_eq!(saved, SavedFile { hash: new_hash.clone(), disk_hash: new_hash.clone() });
        assert_eq!(std::fs::read(&target).unwrap(), b"new\n");
        assert!(!tmp.exists(), "the staged /tmp payload must be cleaned up");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// M2, for real: another writer lands between the script's `mv` and its final
    /// `sha256sum` (simulated by a `sha256sum` shim on PATH that rewrites the target on
    /// its second call). The save's base must still be the hash of our bytes, and the
    /// foreign hash must come back as `disk_hash`, so the editor shows "Changed on disk".
    #[cfg(unix)]
    #[test]
    fn on_the_host_a_write_that_lands_after_ours_is_reported_not_adopted() {
        use std::os::unix::fs::PermissionsExt;
        let real = std::process::Command::new("sh")
            .args(["-c", "command -v sha256sum"])
            .output()
            .expect("sh");
        let real = String::from_utf8_lossy(&real.stdout).trim().to_string();
        assert!(!real.is_empty(), "sha256sum must be on PATH");

        let dir = unique_test_dir("race");
        let bin = dir.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let mark = dir.join("called-once");
        let shim = bin.join("sha256sum");
        std::fs::write(
            &shim,
            format!(
                "#!/bin/sh\nif [ -e '{mark}' ]; then printf 'theirs\\n' > \"$2\"; fi\n: > '{mark}'\nexec '{real}' \"$@\"\n",
                mark = mark.display(),
                real = real
            ),
        )
        .unwrap();
        std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755)).unwrap();

        let target = dir.join("t.txt");
        let tmp = dir.join("payload");
        std::fs::write(&target, b"old\n").unwrap();
        std::fs::write(&tmp, b"new\n").unwrap();
        let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap_or_default());
        let out = std::process::Command::new("sh")
            .env("PATH", path)
            .arg("-c")
            .arg(WRITE_SCRIPT)
            .arg("save")
            .arg(&target)
            .arg(&tmp)
            .arg(sha256_hex(b"old\n"))
            .output()
            .unwrap();
        let (stdout, stderr) = (String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
        assert_eq!(out.status.code(), Some(0), "stdout={stdout} stderr={stderr}");
        assert_eq!(std::fs::read(&target).unwrap(), b"theirs\n", "the shim's write landed last");

        let saved = saved_file(classify_write(0, &stdout, &stderr), b"new\n").unwrap();
        assert_eq!(saved.hash, sha256_hex(b"new\n"));
        assert_eq!(saved.disk_hash, sha256_hex(b"theirs\n"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn on_the_host_a_stale_base_hash_conflicts_and_leaves_everything_untouched() {
        let dir = unique_test_dir("stale");
        let target = dir.join("t.txt");
        let tmp = dir.join("payload");
        std::fs::write(&target, b"old\n").unwrap();
        std::fs::write(&tmp, b"new\n").unwrap();
        let wrong_base = sha256_hex(b"not what is on disk\n");

        let (code, _stdout, stderr) = run_write_script(&target, &tmp, &wrong_base);

        assert_eq!(code, 3, "stderr={stderr}");
        assert_eq!(std::fs::read(&target).unwrap(), b"old\n", "must be untouched");
        assert!(!tmp.exists(), "the staged /tmp payload must still be cleaned up");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn on_the_host_a_missing_target_reports_gone() {
        let dir = unique_test_dir("gone");
        let target = dir.join("does-not-exist");
        let tmp = dir.join("payload");
        std::fs::write(&tmp, b"new\n").unwrap();

        let (code, _stdout, stderr) = run_write_script(&target, &tmp, &sha256_hex(b"whatever"));

        assert_eq!(code, 4, "stderr={stderr}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// I1: a real read failure must be a real error (exit 1), never the exit-3
    /// conflict a bare `sha256sum | cut` pipeline (no `pipefail` in POSIX sh) would
    /// silently produce.
    #[cfg(unix)]
    #[test]
    fn on_the_host_an_unreadable_target_is_an_error_not_a_conflict() {
        use std::os::unix::fs::PermissionsExt;
        let dir = unique_test_dir("unreadable");
        let target = dir.join("t.txt");
        let tmp = dir.join("payload");
        std::fs::write(&target, b"old\n").unwrap();
        std::fs::write(&tmp, b"new\n").unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o000)).unwrap();

        if std::fs::read(&target).is_ok() {
            // Running as root (or some other bypass): 0o000 does not block reads,
            // so this scenario cannot be reproduced here.
            eprintln!("skipping: still able to read a 0o000 file (root?)");
            let _ = std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o644));
            let _ = std::fs::remove_dir_all(&dir);
            return;
        }

        let (code, _stdout, stderr) = run_write_script(&target, &tmp, &sha256_hex(b"old\n"));

        assert_eq!(
            code, 1,
            "an unreadable target must be a real error, not exit 3; stderr={stderr}"
        );
        assert!(!tmp.exists(), "the staged /tmp payload must still be cleaned up");

        let _ = std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o644));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// I3: a target the container user cannot write is refused outright, never
    /// replaced via rename.
    #[cfg(unix)]
    #[test]
    fn on_the_host_a_read_only_target_is_refused_not_replaced() {
        use std::os::unix::fs::PermissionsExt;
        let dir = unique_test_dir("readonly");
        let target = dir.join("t.txt");
        let tmp = dir.join("payload");
        std::fs::write(&target, b"old\n").unwrap();
        std::fs::write(&tmp, b"new\n").unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o444)).unwrap();

        if std::fs::OpenOptions::new().write(true).open(&target).is_ok() {
            eprintln!("skipping: still able to write a 0o444 file (root?)");
            let _ = std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o644));
            let _ = std::fs::remove_dir_all(&dir);
            return;
        }

        let (code, _stdout, stderr) = run_write_script(&target, &tmp, &sha256_hex(b"old\n"));

        assert_eq!(code as i64, EXIT_READ_ONLY, "stderr={stderr}");
        assert!(stderr.contains("read-only"), "stderr={stderr}");
        assert_eq!(
            std::fs::read(&target).unwrap(),
            b"old\n",
            "a read-only file must not be replaced"
        );
        assert!(!tmp.exists(), "the staged /tmp payload must still be cleaned up");

        let _ = std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o644));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// I2: a failed stage (here: an unreadable source payload, so `cp` fails after
    /// `mktemp` has already created the destination) must not leave a partial
    /// `.<name>.triple-c-<suffix>` behind in the user's own directory.
    #[cfg(unix)]
    #[test]
    fn on_the_host_a_failed_stage_leaves_no_partial_file_behind() {
        use std::os::unix::fs::PermissionsExt;
        let dir = unique_test_dir("cpfail");
        let target = dir.join("t.txt");
        let tmp = dir.join("payload");
        std::fs::write(&target, b"old\n").unwrap();
        std::fs::write(&tmp, b"new\n").unwrap();
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o000)).unwrap();

        if std::fs::read(&tmp).is_ok() {
            eprintln!("skipping: still able to read a 0o000 file (root?)");
            let _ = std::fs::remove_dir_all(&dir);
            return;
        }

        let (code, _stdout, stderr) = run_write_script(&target, &tmp, &sha256_hex(b"old\n"));

        assert_eq!(code, 1, "stderr={stderr}");
        assert_eq!(std::fs::read(&target).unwrap(), b"old\n", "must be untouched");
        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with(".t.txt.triple-c-"))
            .collect();
        assert!(leftovers.is_empty(), "staged file(s) left behind: {leftovers:?}");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
