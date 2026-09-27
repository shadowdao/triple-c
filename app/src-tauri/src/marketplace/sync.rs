//! Pushes a project's marketplace payload into its container and runs the
//! sync script there (spec §4).

use std::time::Duration;

use super::payload::Payload;
use crate::docker::exec::{exec_oneshot_as, exec_oneshot_streams_as, upload_bytes_to_container};
use crate::models::marketplace::{SkippedItem, SyncReport};

/// Where the payload and the script are uploaded. Owned by `claude`.
pub const INCOMING_DIR: &str = "/home/claude/.claude/triple-c/marketplace/incoming";

/// The sync script. Shipped with the app and uploaded on every sync, so a new
/// app version reaches existing containers without an image migration.
pub const SYNC_SCRIPT: &str = include_str!("sync.sh");

/// True once the entrypoint has finished: its last step execs this exact
/// command line. Before that it may still be merging `settings.json` or running
/// `claude update`, both of which the sync would race.
const READY_PROBE: &str = "pgrep -x -f 'su -s /bin/bash claude -c exec sleep infinity' >/dev/null";
const READY_TIMEOUT: Duration = Duration::from_secs(180);
const READY_POLL: Duration = Duration::from_secs(2);

/// Run as root: `~/.claude` is a volume and `triple-c/` may not exist yet, and
/// the uploads below are root-owned files in a directory `claude` must own so
/// the script can delete them.
const PREPARE_SCRIPT: &str = r#"set -e
d=/home/claude/.claude/triple-c/marketplace/incoming
mkdir -p "$d"
chown -R claude:claude /home/claude/.claude/triple-c
rm -f "$d/payload.tar" "$d/sync.sh""#;

fn sh(script: &str) -> Vec<String> {
    vec!["sh".to_string(), "-c".to_string(), script.to_string()]
}

/// The readiness probe, run as root.
fn ready_probe_cmd() -> Vec<String> {
    sh(READY_PROBE)
}

/// The sync script invocation, run as `claude`.
fn run_script_cmd() -> Vec<String> {
    vec!["sh".to_string(), format!("{INCOMING_DIR}/sync.sh")]
}

fn run_script_env() -> Vec<String> {
    vec!["HOME=/home/claude".to_string()]
}

async fn wait_until_ready(container_id: &str) -> Result<(), String> {
    let deadline = tokio::time::Instant::now() + READY_TIMEOUT;
    loop {
        let (_, code) = exec_oneshot_as(container_id, "root", ready_probe_cmd(), vec![]).await?;
        if code == 0 {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(format!(
                "The container did not finish starting within {} seconds, so marketplace items \
                 were not applied. They are applied on the next start, or with Apply now.",
                READY_TIMEOUT.as_secs()
            ));
        }
        tokio::time::sleep(READY_POLL).await;
    }
}

/// The last `max` bytes of `text`, trimmed, never splitting a character.
fn tail(text: &str, max: usize) -> &str {
    let text = text.trim();
    if text.len() <= max {
        return text;
    }
    let mut start = text.len() - max;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    &text[start..]
}

/// Wait for readiness, upload the payload and the script, run the script as
/// `claude`, and return its report.
pub async fn sync_container(container_id: &str, payload: &Payload) -> Result<SyncReport, String> {
    wait_until_ready(container_id).await?;

    let (out, code) = exec_oneshot_as(container_id, "root", sh(PREPARE_SCRIPT), vec![]).await?;
    if code != 0 {
        return Err(format!(
            "Could not prepare the container for the marketplace sync: {}",
            tail(&out, 500)
        ));
    }
    upload_bytes_to_container(
        container_id,
        INCOMING_DIR,
        "payload.tar",
        &payload.tar,
        0o644,
    )
    .await?;
    upload_bytes_to_container(
        container_id,
        INCOMING_DIR,
        "sync.sh",
        SYNC_SCRIPT.as_bytes(),
        0o755,
    )
    .await?;

    let (stdout, stderr, code) =
        exec_oneshot_streams_as(container_id, "claude", run_script_cmd(), run_script_env()).await?;
    parse_report(&stdout).map_err(|e| {
        format!(
            "The marketplace sync script failed (exit {code}): {e}. {}",
            tail(&stderr, 500)
        )
    })
}

/// The script's report is the last non-empty line of stdout.
pub fn parse_report(stdout: &str) -> Result<SyncReport, String> {
    let line = stdout
        .lines()
        .rev()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .ok_or_else(|| "the sync script printed no report".to_string())?;
    serde_json::from_str(line)
        .map_err(|e| format!("the sync script's report could not be read: {e}"))
}

/// A sync never fails its caller: an error becomes a report that says so.
pub fn report_from_result(r: Result<SyncReport, String>) -> SyncReport {
    let mut report = match r {
        Ok(report) => report,
        Err(e) => SyncReport {
            errors: vec![e],
            ..Default::default()
        },
    };
    report.finished_at = chrono::Utc::now().to_rfc3339();
    report
}

/// Items the host left out of the payload (invalid, missing from the cache, …)
/// never reach the script, so the stored report lists them ahead of its own.
pub fn with_payload_skips(mut report: SyncReport, payload_skipped: &[SkippedItem]) -> SyncReport {
    let mut skipped = payload_skipped.to_vec();
    skipped.append(&mut report.skipped);
    report.skipped = skipped;
    report
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_report_is_the_last_non_empty_stdout_line() {
        let out = "noise\n{\"installed\":[\"agent:a\"],\"errors\":[]}\n\n";
        let r = parse_report(out).unwrap();
        assert_eq!(r.installed, vec!["agent:a"]);
        assert!(r.skipped.is_empty());
    }

    #[test]
    fn missing_or_garbled_reports_are_errors() {
        assert!(parse_report("").unwrap_err().contains("no report"));
        assert!(parse_report("not json\n")
            .unwrap_err()
            .contains("could not be read"));
    }

    #[test]
    fn a_failed_sync_becomes_a_report() {
        // A failed sync becomes a report with the error in it — never an Err
        // that could propagate into container start.
        let r = report_from_result(Err("container went away".into()));
        assert_eq!(r.errors, vec!["container went away"]);
        assert!(!r.finished_at.is_empty());

        let ok = report_from_result(Ok(SyncReport {
            installed: vec!["hook:h".into()],
            ..Default::default()
        }));
        assert_eq!(ok.installed, vec!["hook:h"]);
        assert!(chrono::DateTime::parse_from_rfc3339(&ok.finished_at).is_ok());
    }

    #[test]
    fn the_embedded_script_is_the_sync_script() {
        assert!(SYNC_SCRIPT.starts_with("#!/bin/sh"));
        assert!(SYNC_SCRIPT.contains("MARKETPLACE_INCOMING"));
    }

    #[test]
    fn readiness_probes_the_entrypoints_final_exec() {
        assert_eq!(
            ready_probe_cmd(),
            vec![
                "sh",
                "-c",
                "pgrep -x -f 'su -s /bin/bash claude -c exec sleep infinity' >/dev/null"
            ]
        );
        assert_eq!(READY_POLL, Duration::from_secs(2));
        assert_eq!(READY_TIMEOUT, Duration::from_secs(180));
    }

    #[test]
    fn the_incoming_dir_is_prepared_for_claude() {
        // The uploads are root-owned, so the directory must exist and belong
        // to claude before they land (claude extracts and deletes them).
        assert!(PREPARE_SCRIPT.contains(INCOMING_DIR));
        assert!(PREPARE_SCRIPT.contains("mkdir -p"));
        assert!(PREPARE_SCRIPT.contains("chown -R claude:claude /home/claude/.claude/triple-c"));
    }

    #[test]
    fn the_script_runs_as_claude_with_home_set() {
        assert_eq!(
            run_script_cmd(),
            vec!["sh".to_string(), format!("{INCOMING_DIR}/sync.sh")]
        );
        assert_eq!(run_script_env(), vec!["HOME=/home/claude"]);
    }

    #[test]
    fn payload_skips_come_before_the_scripts_own() {
        use crate::models::marketplace::SkippedItem;
        let payload_skip = SkippedItem {
            item: "agent:a".into(),
            reason: "invalid".into(),
        };
        let script_skip = SkippedItem {
            item: "hook:h".into(),
            reason: "no jq".into(),
        };
        let report = SyncReport {
            skipped: vec![script_skip.clone()],
            ..Default::default()
        };
        let merged = with_payload_skips(report, std::slice::from_ref(&payload_skip));
        assert_eq!(merged.skipped, vec![payload_skip, script_skip]);
    }

    #[test]
    fn long_output_is_tailed_on_a_char_boundary() {
        assert_eq!(tail("  short \n", 10), "short");
        let s = format!("{}é", "x".repeat(20));
        let t = tail(&s, 1);
        assert!(s.ends_with(t));
        assert!(t.len() <= 2);
    }
}
