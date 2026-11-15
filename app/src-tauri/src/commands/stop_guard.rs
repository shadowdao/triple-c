//! The AWS stop guard: what `triple-c-wip-push` says about a workspace's
//! unsaved git work, and the push a Stop may run first.
//!
//! The decisions are made over [`WipRunner`] so a fake target exercises them;
//! [`ContainerTarget`] runs the script through the agent as `claude` (the
//! script needs the user's credential helper and `~/.ssh`).

use std::future::Future;
use std::time::Duration;

use crate::host::ContainerTarget;
use crate::models::{RepoState, UnsavedWork, WipChoice};

/// The script, baked into the image (`container/triple-c-wip-push`).
pub const WIP_PUSH: &str = "/usr/local/bin/triple-c-wip-push";
/// A dry run only reads `git status`; it should be quick.
pub const DRY_RUN_TIMEOUT: Duration = Duration::from_secs(30);
/// A push goes over the network, once per dirty repo.
pub const PUSH_TIMEOUT: Duration = Duration::from_secs(300);
/// Cap on the push output carried into an error.
const MAX_OUTPUT: usize = 4000;

/// Where the entrypoint's stop hook looks for the app's decision (Task 7's
/// file, which the idle watcher also writes `idle` to).
pub const STOP_REASON_FILE: &str = "/run/triple-c/stop-reason";
/// Recorded after the guard's push succeeded: the hook does not push again.
pub const MARKER_PUSHED: &str = "pushed";
/// Recorded for "Stop without pushing": the hook does not push.
pub const MARKER_DISCARD: &str = "discard";

/// The workspace, as the stop guard uses it.
pub trait WipRunner: Sync {
    /// Run the WIP script as `claude`: `(combined output, exit code)`.
    fn run(&self, args: Vec<String>) -> impl Future<Output = Result<(String, i64), String>> + Send;
    /// Record `marker` in [`STOP_REASON_FILE`] (as root: the directory is root's).
    fn write_stop_reason(&self, marker: &str) -> impl Future<Output = Result<(), String>> + Send;
}

impl WipRunner for ContainerTarget {
    async fn run(&self, args: Vec<String>) -> Result<(String, i64), String> {
        let mut cmd = vec![WIP_PUSH.to_string()];
        cmd.extend(args);
        self.exec_oneshot_as("claude", cmd, Vec::new()).await
    }

    async fn write_stop_reason(&self, marker: &str) -> Result<(), String> {
        let (out, code) = self
            .exec_oneshot_as("root", stop_reason_command(marker), Vec::new())
            .await?;
        if code == 0 {
            Ok(())
        } else {
            Err(format!("exit {code}: {}", cap(&out)))
        }
    }
}

/// The root command that records `marker`; the value travels as `$1`, never
/// inside the script.
pub fn stop_reason_command(marker: &str) -> Vec<String> {
    vec![
        "sh".into(),
        "-c".into(),
        format!(
            "umask 077 && mkdir -p /run/triple-c && chmod 700 /run/triple-c && printf '%s\\n' \"$1\" > {STOP_REASON_FILE}"
        ),
        "sh".into(),
        marker.into(),
    ]
}

/// The repository lines of a dry run. Lines that are not JSON objects (a
/// warning on stderr) are skipped; a JSON line of the wrong shape is an error.
pub fn parse_dry_run(output: &str) -> Result<Vec<RepoState>, String> {
    output
        .lines()
        .map(str::trim)
        .filter(|l| l.starts_with('{'))
        .map(|l| {
            serde_json::from_str::<RepoState>(l)
                .map_err(|e| format!("Unexpected output from the WIP check: {e}"))
        })
        .collect()
}

fn unchecked(reason: String) -> UnsavedWork {
    UnsavedWork { checked: false, repos: Vec::new(), error: Some(reason) }
}

/// Ask the workspace what a stop would lose. `runner` is `None` when the
/// workspace's agent is not reachable (no tunnel).
pub async fn unsaved_work<R: WipRunner>(runner: Option<&R>, timeout: Duration) -> UnsavedWork {
    let Some(runner) = runner else {
        return unchecked(
            "Triple-C is not connected to the workspace, so it can't check for unsaved work."
                .into(),
        );
    };
    let res = tokio::time::timeout(timeout, runner.run(vec!["--dry-run".into()])).await;
    match res {
        Err(_) => unchecked("The workspace did not answer the check for unsaved work in time.".into()),
        Ok(Err(e)) => unchecked(format!("The workspace could not be asked about unsaved work: {e}")),
        Ok(Ok((out, code))) if code != 0 => unchecked(format!(
            "The check for unsaved work failed (exit {code}): {}",
            cap(&out)
        )),
        Ok(Ok((out, _))) => match parse_dry_run(&out) {
            Ok(repos) => UnsavedWork { checked: true, repos, error: None },
            Err(e) => unchecked(e),
        },
    }
}

fn cap(s: &str) -> String {
    let t = s.trim();
    if t.len() <= MAX_OUTPUT {
        return t.to_string();
    }
    let mut end = t.len() - MAX_OUTPUT;
    while !t.is_char_boundary(end) {
        end += 1;
    }
    format!("…{}", &t[end..])
}

/// What Stop does before stopping, given the user's choice. `Push` runs the
/// push and fails (aborting the stop) with its output when it does not
/// succeed; `Discard` and no choice do nothing.
pub async fn before_stop<R: WipRunner>(
    runner: Option<&R>,
    choice: Option<WipChoice>,
    timeout: Duration,
) -> Result<(), String> {
    if choice != Some(WipChoice::Push) {
        return Ok(());
    }
    let runner = runner.ok_or_else(|| {
        "Triple-C is not connected to the workspace, so it can't push unsaved work. The \
         workspace was not stopped."
            .to_string()
    })?;
    let res = tokio::time::timeout(timeout, runner.run(vec!["--reason".into(), "stop".into()])).await;
    match res {
        Err(_) => Err("Pushing unsaved work did not finish in time. The workspace was not stopped.".into()),
        Ok(Err(e)) => Err(format!("Could not push unsaved work: {e}. The workspace was not stopped.")),
        Ok(Ok((out, 0))) => {
            log::info!("WIP push before stop: {}", cap(&out));
            Ok(())
        }
        Ok(Ok((out, code))) => Err(format!(
            "Pushing unsaved work failed (exit {code}), so the workspace was not stopped:\n{}",
            cap(&out)
        )),
    }
}

/// The note a stop returns when the hook's own push may still run.
pub const MARKER_NOT_RECORDED: &str = "Triple-C could not tell the workspace what you chose, so its own \
     push of work in progress on shutdown may still run (if it is turned on).";

/// Everything the guard does before StopTask: `Push` pushes (a failure is an
/// `Err` and the stop must not happen), then records `pushed`; `Discard`
/// records `discard`; no choice does nothing. A marker that can't be
/// written does not stop the stop: `Ok(Some(note))` says the hook may push.
pub async fn prepare_stop<R: WipRunner>(
    runner: Option<&R>,
    choice: Option<WipChoice>,
    push_timeout: Duration,
) -> Result<Option<String>, String> {
    let marker = match choice {
        None => return Ok(None),
        Some(WipChoice::Push) => {
            before_stop(runner, choice, push_timeout).await?;
            MARKER_PUSHED
        }
        Some(WipChoice::Discard) => MARKER_DISCARD,
    };
    let written = match runner {
        Some(r) => tokio::time::timeout(DRY_RUN_TIMEOUT, r.write_stop_reason(marker))
            .await
            .unwrap_or_else(|_| Err("timed out".into())),
        None => Err("not connected to the workspace".into()),
    };
    match written {
        Ok(()) => Ok(None),
        Err(e) => {
            log::warn!("Could not record the stop guard's choice ({marker}): {e}");
            Ok(Some(MARKER_NOT_RECORDED.to_string()))
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::sync::Mutex;

    pub(crate) struct Fake {
        pub reply: Result<(String, i64), String>,
        pub calls: Mutex<Vec<Vec<String>>>,
        pub hang: bool,
        pub markers: Mutex<Vec<String>>,
        pub marker_fails: bool,
    }

    impl Fake {
        pub fn new(reply: Result<(String, i64), String>) -> Self {
            Fake { reply, calls: Mutex::new(Vec::new()), hang: false, markers: Mutex::new(Vec::new()), marker_fails: false }
        }
        fn markers(&self) -> Vec<String> {
            self.markers.lock().unwrap().clone()
        }
        fn calls(&self) -> Vec<Vec<String>> {
            self.calls.lock().unwrap().clone()
        }
    }

    impl WipRunner for Fake {
        async fn run(&self, args: Vec<String>) -> Result<(String, i64), String> {
            self.calls.lock().unwrap().push(args);
            if self.hang {
                std::future::pending::<()>().await;
            }
            self.reply.clone()
        }
        async fn write_stop_reason(&self, marker: &str) -> Result<(), String> {
            if self.marker_fails {
                return Err("agent unreachable".into());
            }
            self.markers.lock().unwrap().push(marker.to_string());
            Ok(())
        }
    }

    const T: Duration = Duration::from_secs(5);

    #[test]
    fn a_dry_run_is_parsed_skipping_non_json_lines() {
        let out = "warning: something\n{\"folder\":\"api\",\"dirty\":true,\"unpushed\":0,\"branch\":\"main\"}\n\
                   {\"folder\":\"web\",\"dirty\":false,\"unpushed\":2,\"branch\":\"HEAD\"}\n";
        let repos = parse_dry_run(out).unwrap();
        assert_eq!(repos.len(), 2);
        assert_eq!(repos[0], RepoState { folder: "api".into(), dirty: true, unpushed: 0, branch: "main".into() });
        assert_eq!(repos[1].unpushed, 2);
        assert!(parse_dry_run("{\"folder\":1}").is_err());
        assert!(parse_dry_run("").unwrap().is_empty());
    }

    #[tokio::test]
    async fn the_check_runs_a_dry_run_and_reports_the_repos() {
        let fake = Fake::new(Ok(("{\"folder\":\"a\",\"dirty\":false,\"unpushed\":0,\"branch\":\"main\"}".into(), 0)));
        let w = unsaved_work(Some(&fake), T).await;
        assert!(w.checked && w.error.is_none());
        assert_eq!(w.repos.len(), 1);
        assert_eq!(fake.calls(), vec![vec!["--dry-run".to_string()]]);
    }

    #[tokio::test]
    async fn an_unreachable_workspace_is_unchecked_with_a_reason() {
        let w = unsaved_work::<Fake>(None, T).await;
        assert!(!w.checked);
        assert!(w.error.as_deref().unwrap().contains("not connected"));

        let failing = Fake::new(Err("connection refused".into()));
        let w = unsaved_work(Some(&failing), T).await;
        assert!(!w.checked && w.error.unwrap().contains("connection refused"));

        let nonzero = Fake::new(Ok(("boom".into(), 2)));
        let w = unsaved_work(Some(&nonzero), T).await;
        assert!(!w.checked && w.error.unwrap().contains("exit 2"));
    }

    #[tokio::test(start_paused = true)]
    async fn a_check_that_hangs_times_out_unchecked() {
        let mut fake = Fake::new(Ok((String::new(), 0)));
        fake.hang = true;
        let w = unsaved_work(Some(&fake), Duration::from_secs(1)).await;
        assert!(!w.checked && w.error.unwrap().contains("in time"));
    }

    #[tokio::test]
    async fn push_runs_the_script_with_reason_stop() {
        let fake = Fake::new(Ok(("pushed".into(), 0)));
        before_stop(Some(&fake), Some(WipChoice::Push), T).await.unwrap();
        assert_eq!(fake.calls(), vec![vec!["--reason".to_string(), "stop".to_string()]]);
    }

    #[tokio::test]
    async fn a_failed_push_aborts_with_its_output() {
        let fake = Fake::new(Ok(("triple-c-wip-push: api: push to origin failed: denied".into(), 1)));
        let e = before_stop(Some(&fake), Some(WipChoice::Push), T).await.unwrap_err();
        assert!(e.contains("push to origin failed: denied"), "{e}");
        assert!(e.contains("not stopped"));

        let gone = Fake::new(Err("tunnel closed".into()));
        assert!(before_stop(Some(&gone), Some(WipChoice::Push), T).await.unwrap_err().contains("tunnel closed"));
        assert!(before_stop::<Fake>(None, Some(WipChoice::Push), T).await.is_err());
    }

    #[tokio::test]
    async fn discard_or_no_choice_runs_nothing() {
        let fake = Fake::new(Err("must not run".into()));
        before_stop(Some(&fake), Some(WipChoice::Discard), T).await.unwrap();
        before_stop(Some(&fake), None, T).await.unwrap();
        before_stop::<Fake>(None, None, T).await.unwrap();
        assert!(fake.calls().is_empty());
    }

    #[test]
    fn long_output_keeps_its_end() {
        let s = format!("{}END", "x".repeat(10_000));
        let c = cap(&s);
        assert!(c.ends_with("END") && c.len() < 4100);
    }

    #[tokio::test]
    async fn a_successful_push_records_pushed_and_stop_without_pushing_records_discard() {
        let fake = Fake::new(Ok(("pushed".into(), 0)));
        assert_eq!(prepare_stop(Some(&fake), Some(WipChoice::Push), T).await.unwrap(), None);
        assert_eq!(fake.markers(), vec![MARKER_PUSHED]);

        let fake = Fake::new(Err("must not run".into()));
        assert_eq!(prepare_stop(Some(&fake), Some(WipChoice::Discard), T).await.unwrap(), None);
        assert!(fake.calls().is_empty());
        assert_eq!(fake.markers(), vec![MARKER_DISCARD]);

        assert_eq!(prepare_stop(Some(&fake), None, T).await.unwrap(), None);
        assert_eq!(fake.markers().len(), 1, "no choice records nothing");
    }

    #[tokio::test]
    async fn a_failed_push_records_nothing() {
        let fake = Fake::new(Ok(("denied".into(), 1)));
        assert!(prepare_stop(Some(&fake), Some(WipChoice::Push), T).await.is_err());
        assert!(fake.markers().is_empty());
    }

    #[tokio::test]
    async fn an_unwritable_marker_still_stops_and_says_the_hook_may_push() {
        let mut fake = Fake::new(Ok((String::new(), 0)));
        fake.marker_fails = true;
        let note = prepare_stop(Some(&fake), Some(WipChoice::Discard), T).await.unwrap();
        assert!(note.unwrap().contains("may still run"));
        let note = prepare_stop::<Fake>(None, Some(WipChoice::Discard), T).await.unwrap();
        assert!(note.is_some());
    }

    #[test]
    fn the_marker_is_written_as_an_argument_to_the_file_the_hook_reads() {
        let cmd = stop_reason_command("pushed");
        assert_eq!(&cmd[..2], &["sh".to_string(), "-c".to_string()]);
        assert!(cmd[2].contains("> /run/triple-c/stop-reason") && cmd[2].contains("\"$1\""));
        assert_eq!(cmd.last().unwrap(), "pushed");
        // The entrypoint matches exactly these values.
        let ep = include_str!("../../../../container/entrypoint.sh");
        assert!(ep.contains("pushed|discard)") && ep.contains(STOP_REASON_FILE));
    }
}
