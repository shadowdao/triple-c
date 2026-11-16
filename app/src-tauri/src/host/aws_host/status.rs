//! An AWS workspace's clone and setup script, as the start follows them and
//! Project Home shows them (phase 6, Q4).
//!
//! The entrypoint clones in the background and then runs the setup script;
//! `triple-c-workspace-status` (container) prints both as one JSON line,
//! mirrored by [`WorkspaceStatus`]. The start reads it through the agent every
//! [`FOLLOW_INTERVAL`] as a **background** exec (never activity for idle
//! stop), emits a progress line only when it changes, and stops when the
//! clone is done and no setup is running, after [`FOLLOW_LIMIT`], or quietly
//! when the agent is gone (tunnel dropped, old image without the script).
//! A failed clone or setup does not fail the start: [`follow_warning`] is
//! what the user is told instead.

use std::future::Future;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::super::{tunnels, ContainerTarget};
use crate::models::Project;

/// The in-container status reader (Task 4).
pub const STATUS_SCRIPT: &str = "/usr/local/bin/triple-c-workspace-status";
pub const FOLLOW_INTERVAL: Duration = Duration::from_secs(2);
/// The setup script's own timeout is 30 minutes, after the clone.
pub const FOLLOW_LIMIT: Duration = Duration::from_secs(35 * 60);
/// One status read through the agent.
const READ_TIMEOUT: Duration = Duration::from_secs(20);
/// Reads failing in a row before the follow gives up (quietly).
const MAX_READ_FAILURES: u32 = 5;
/// The script prints at most ~20 KB (20 tail lines of ≤ 1000 characters).
const STATUS_OUTPUT_LIMIT: usize = 256 * 1024;
/// A setup log line shown as progress is cut to this many characters.
const MAX_PROGRESS_LINE: usize = 200;
/// Setup log lines a warning carries.
const WARNING_TAIL_LINES: usize = 10;

pub const PROGRESS_CLONING: &str = "Cloning repos…";
pub const PROGRESS_SETUP: &str = "Running setup script…";

// ──────────────────────────────────────────────────────────────── model

/// `triple-c-workspace-status`'s JSON. Unknown fields are ignored and
/// missing ones default, so an older or newer image never breaks a read.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct WorkspaceStatus {
    pub clone: CloneStatus,
    pub setup: SetupStatus,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CloneStatus {
    /// Every repo has been tried.
    pub done: bool,
    /// The repos tried so far, in order.
    pub repos: Vec<RepoProgress>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RepoProgress {
    pub folder: String,
    /// `cloning`, `cloned`, `failed` or `exists`.
    pub state: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SetupStatus {
    pub state: SetupState,
    /// The script's exit code: 0 when `ok`, the code when `failed` (if known).
    pub code: Option<i64>,
    /// The setup log's last lines.
    pub tail: Vec<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SetupState {
    /// No setup script.
    #[default]
    None,
    Running,
    Ok,
    /// Also any state this app does not know.
    #[serde(other)]
    Failed,
}

impl RepoProgress {
    fn finished(&self) -> bool {
        matches!(self.state.as_str(), "cloned" | "failed" | "exists")
    }
}

impl WorkspaceStatus {
    /// Nothing more will change: the clone is done and no setup is running.
    pub fn settled(&self) -> bool {
        self.clone.done && self.setup.state != SetupState::Running
    }

    pub fn failed_repos(&self) -> Vec<&str> {
        self.clone
            .repos
            .iter()
            .filter(|r| r.state == "failed")
            .map(|r| r.folder.as_str())
            .collect()
    }

    /// The start's progress line for this state (`None` once settled).
    /// `total` is how many repos the project configures; the status only
    /// lists the ones started so far.
    pub fn progress_line(&self, total: usize) -> Option<String> {
        if !self.clone.done {
            let n = total.max(self.clone.repos.len());
            if n == 0 {
                return Some(PROGRESS_CLONING.into());
            }
            let finished = self.clone.repos.iter().filter(|r| r.finished()).count();
            return Some(format!("{PROGRESS_CLONING} ({}/{n})", (finished + 1).min(n)));
        }
        (self.setup.state == SetupState::Running).then(|| PROGRESS_SETUP.into())
    }

    /// The setup log's last non-blank line, cleaned for a progress line.
    fn last_log_line(&self) -> Option<String> {
        self.setup
            .tail
            .iter()
            .rev()
            .map(|l| clean_line(l))
            .find(|l| !l.is_empty())
    }

    /// What went wrong, for the user; `None` when nothing did.
    pub fn problems(&self) -> Option<String> {
        let mut parts = Vec::new();
        let failed = self.failed_repos();
        if !failed.is_empty() {
            parts.push(format!(
                "Cloning failed for {}: {}. The workspace started without {}.",
                if failed.len() == 1 { "a repo" } else { "some repos" },
                failed.join(", "),
                if failed.len() == 1 { "it" } else { "them" },
            ));
        }
        if self.setup.state == SetupState::Failed {
            let code = self.setup.code.map(|c| format!(" (exit {c})")).unwrap_or_default();
            let mut s = format!("The setup script failed{code}.");
            let tail: Vec<&String> = self.setup.tail.iter().rev().take(WARNING_TAIL_LINES).collect();
            if !tail.is_empty() {
                s.push_str("\nLast lines:\n");
                s.push_str(
                    &tail.into_iter().rev().map(String::as_str).collect::<Vec<_>>().join("\n"),
                );
            }
            parts.push(s);
        }
        (!parts.is_empty()).then(|| parts.join("\n\n"))
    }
}

/// One line of the setup log as a progress message: control characters
/// dropped, trimmed, at most [`MAX_PROGRESS_LINE`] characters.
fn clean_line(line: &str) -> String {
    let cleaned: String = line
        .chars()
        .map(|c| if c == '\t' { ' ' } else { c })
        .filter(|c| !c.is_control())
        .collect();
    let t = cleaned.trim();
    if t.chars().count() > MAX_PROGRESS_LINE {
        let mut s: String = t.chars().take(MAX_PROGRESS_LINE - 1).collect();
        s.push('…');
        s
    } else {
        t.to_string()
    }
}

/// Parse the script's output: its last non-blank line is the JSON (anything
/// before it — a stray warning — is ignored).
pub fn parse_status(output: &str) -> Result<WorkspaceStatus, String> {
    let line = output
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .ok_or("The workspace status was empty")?;
    serde_json::from_str(line.trim()).map_err(|e| format!("The workspace status could not be read: {e}"))
}

// ───────────────────────────────────────────────────────────── reading

/// One read of the status.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StatusRead {
    Status(WorkspaceStatus),
    /// No agent to ask (tunnel gone), or an image without the script.
    Gone,
    /// A read that failed; may be retried.
    Failed(String),
}

pub trait StatusSource: Send + Sync {
    fn read(&self) -> impl Future<Output = StatusRead> + Send;
}

/// The command that runs the script; exit 127 when the image predates it.
fn status_command() -> Vec<String> {
    vec![
        "sh".into(),
        "-c".into(),
        format!("[ -x {STATUS_SCRIPT} ] || exit 127; exec {STATUS_SCRIPT}"),
    ]
}

/// Read the status through `target`, as a background exec.
pub async fn read_status(target: &ContainerTarget) -> StatusRead {
    let res = tokio::time::timeout(
        READ_TIMEOUT,
        target.exec_oneshot_background("claude", status_command(), STATUS_OUTPUT_LIMIT),
    )
    .await;
    match res {
        Err(_) => StatusRead::Failed("The workspace status read timed out".into()),
        Ok(Err(e)) => StatusRead::Failed(e),
        Ok(Ok((out, 0))) => match parse_status(&out) {
            Ok(s) => StatusRead::Status(s),
            Err(e) => StatusRead::Failed(e),
        },
        Ok(Ok((_, 126 | 127))) => StatusRead::Gone,
        Ok(Ok((_, code))) => StatusRead::Failed(format!("The workspace status exited with {code}")),
    }
}

/// The project's registered tunnel, looked up on every read: a stop or a
/// dropped tunnel unregisters it, and the follow then ends.
struct TunnelStatusSource<'a> {
    project_id: &'a str,
}

impl StatusSource for TunnelStatusSource<'_> {
    async fn read(&self) -> StatusRead {
        match tunnels::get_endpoint(self.project_id) {
            Some(ep) => read_status(&ContainerTarget::Agent(ep)).await,
            None => StatusRead::Gone,
        }
    }
}

/// The status of an AWS project's workspace for Project Home: `None` for a
/// local project, one with no tunnel, or an image without the script.
pub async fn workspace_status(project: &Project) -> Result<Option<WorkspaceStatus>, String> {
    if !project.is_aws() {
        return Ok(None);
    }
    match (TunnelStatusSource { project_id: &project.id }).read().await {
        StatusRead::Status(s) => Ok(Some(s)),
        StatusRead::Gone => Ok(None),
        StatusRead::Failed(e) => Err(e),
    }
}

// ──────────────────────────────────────────────────────────── following

#[derive(Debug, Clone, Copy)]
pub struct FollowTiming {
    pub interval: Duration,
    pub limit: Duration,
}

impl Default for FollowTiming {
    fn default() -> Self {
        Self { interval: FOLLOW_INTERVAL, limit: FOLLOW_LIMIT }
    }
}

/// How a follow ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FollowEnd {
    Settled(WorkspaceStatus),
    /// [`FollowTiming::limit`] passed; the last status read, if any.
    TimedOut(Option<WorkspaceStatus>),
    /// The agent went away or kept failing; nothing to say.
    Lost,
}

/// Follow `src` until it settles, the limit passes or it is lost, calling
/// `progress` with each new progress line and each new setup log line.
pub async fn follow_workspace_status<S: StatusSource>(
    src: &S,
    total_repos: usize,
    timing: FollowTiming,
    progress: &(dyn Fn(&str) + Send + Sync),
) -> FollowEnd {
    let deadline = tokio::time::Instant::now() + timing.limit;
    let mut last_line: Option<String> = None;
    let mut last_log: Option<String> = None;
    let mut last_status: Option<WorkspaceStatus> = None;
    let mut failures = 0u32;
    loop {
        match src.read().await {
            StatusRead::Status(s) => {
                failures = 0;
                if s.settled() {
                    return FollowEnd::Settled(s);
                }
                if let Some(line) = s.progress_line(total_repos) {
                    if last_line.as_deref() != Some(line.as_str()) {
                        progress(&line);
                        last_line = Some(line);
                    }
                }
                if s.setup.state == SetupState::Running {
                    if let Some(log) = s.last_log_line() {
                        if last_log.as_deref() != Some(log.as_str()) {
                            progress(&log);
                            last_log = Some(log);
                        }
                    }
                }
                last_status = Some(s);
            }
            StatusRead::Gone => return FollowEnd::Lost,
            StatusRead::Failed(e) => {
                failures += 1;
                log::debug!("Workspace status read failed ({failures}): {e}");
                if failures >= MAX_READ_FAILURES {
                    log::info!("Stopped following the workspace's start: {e}");
                    return FollowEnd::Lost;
                }
            }
        }
        if tokio::time::Instant::now() + timing.interval > deadline {
            return FollowEnd::TimedOut(last_status);
        }
        tokio::time::sleep(timing.interval).await;
    }
}

/// What the user is told after a follow; `None` when all went well (or the
/// follow was lost — Project Home still shows the status on demand).
pub fn follow_warning(end: &FollowEnd, limit: Duration) -> Option<String> {
    match end {
        FollowEnd::Settled(s) => s.problems(),
        FollowEnd::TimedOut(s) => {
            let what = match s {
                Some(s) if !s.clone.done => "still cloning its repos",
                _ => "still running its setup script",
            };
            let mut msg = format!(
                "The workspace is {what} after {} minutes. It is running; Project Home shows the \
                 progress.",
                limit.as_secs() / 60
            );
            if let Some(p) = s.as_ref().and_then(WorkspaceStatus::problems) {
                msg.push_str("\n\n");
                msg.push_str(&p);
            }
            Some(msg)
        }
        FollowEnd::Lost => None,
    }
}

/// The start's follow of a just-connected AWS project; the warning to show,
/// if any. A local project returns `None` at once.
pub async fn follow_start(
    project: &Project,
    progress: &(dyn Fn(&str) + Send + Sync),
) -> Option<String> {
    if !project.is_aws() {
        return None;
    }
    let total = project.aws.as_ref().map_or(0, |a| a.repos.len());
    let timing = FollowTiming::default();
    let end = follow_workspace_status(
        &TunnelStatusSource { project_id: &project.id },
        total,
        timing,
        progress,
    )
    .await;
    follow_warning(&end, timing.limit)
}

#[cfg(test)]
mod tests;
