use std::sync::Mutex;

use super::*;
use crate::host::tunnels;

/// Returns its scripted reads in order, then repeats the last one.
struct Scripted {
    reads: Mutex<Vec<StatusRead>>,
    count: Mutex<usize>,
}

impl Scripted {
    fn new(reads: Vec<StatusRead>) -> Self {
        Self { reads: Mutex::new(reads), count: Mutex::new(0) }
    }
    fn count(&self) -> usize {
        *self.count.lock().unwrap()
    }
}

impl StatusSource for Scripted {
    async fn read(&self) -> StatusRead {
        *self.count.lock().unwrap() += 1;
        let mut r = self.reads.lock().unwrap();
        if r.len() > 1 {
            r.remove(0)
        } else {
            r[0].clone()
        }
    }
}

fn st(json: &str) -> StatusRead {
    StatusRead::Status(parse_status(json).unwrap())
}

async fn follow(src: &Scripted, total: usize) -> (FollowEnd, Vec<String>) {
    let lines = Mutex::new(Vec::new());
    let progress = |m: &str| lines.lock().unwrap().push(m.to_string());
    let cancel = CancellationToken::new();
    let end = follow_workspace_status(src, total, FollowTiming::default(), &cancel, &progress).await;
    (end, lines.into_inner().unwrap())
}

const NOT_STARTED: &str = r#"{"clone":{"done":false,"repos":[]},"setup":{"state":"running","code":null,"tail":[]}}"#;
const CLONING_1: &str = r#"{"clone":{"done":false,"repos":[{"folder":"a","state":"cloning"}]},"setup":{"state":"running","code":null,"tail":[]}}"#;
const CLONING_2: &str = r#"{"clone":{"done":false,"repos":[{"folder":"a","state":"cloned"},{"folder":"b","state":"cloning"}]},"setup":{"state":"running","code":null,"tail":[]}}"#;
const SETUP_1: &str = r#"{"clone":{"done":true,"repos":[{"folder":"a","state":"cloned"},{"folder":"b","state":"failed"}]},"setup":{"state":"running","code":null,"tail":["Reading package lists...","","  Setting up jq  "]}}"#;
const SETUP_2: &str = r#"{"clone":{"done":true,"repos":[{"folder":"a","state":"cloned"},{"folder":"b","state":"failed"}]},"setup":{"state":"running","code":null,"tail":["Setting up jq","done"]}}"#;
const SETUP_FAILED: &str = r#"{"clone":{"done":true,"repos":[{"folder":"a","state":"cloned"},{"folder":"b","state":"failed"}]},"setup":{"state":"failed","code":2,"tail":["Setting up jq","E: oops"]}}"#;
const ALL_OK: &str = r#"{"clone":{"done":true,"repos":[{"folder":"a","state":"cloned"}]},"setup":{"state":"none","code":null,"tail":[]}}"#;

#[tokio::test(start_paused = true)]
async fn follow_emits_each_change_once_and_stops_when_settled() {
    let src = Scripted::new(vec![
        st(NOT_STARTED),
        st(CLONING_1),
        st(CLONING_1),
        st(CLONING_2),
        st(SETUP_1),
        st(SETUP_1),
        st(SETUP_2),
        st(SETUP_FAILED),
    ]);
    let started = tokio::time::Instant::now();
    let (end, lines) = follow(&src, 2).await;
    assert_eq!(
        lines,
        [
            "Cloning repos… (1/2)",
            "Cloning repos… (2/2)",
            "Running setup script…",
            "Setting up jq",
            "done",
        ]
    );
    assert_eq!(src.count(), 8);
    // Seven waits of 2 s between eight reads.
    assert_eq!(started.elapsed(), FOLLOW_INTERVAL * 7);
    let FollowEnd::Settled { status: s, first_read: false } = &end else { panic!("{end:?}") };
    // Seen failing in this follow: reported even on an attach.
    let w = follow_warning(&end, FOLLOW_LIMIT, false).unwrap();
    assert!(w.contains("Cloning failed for a repo: b."), "{w}");
    assert!(w.contains("The setup script failed (exit 2)."), "{w}");
    assert!(w.ends_with("Last lines:\nSetting up jq\nE: oops"), "{w}");
    assert_eq!(s.failed_repos(), ["b"]);
}

#[tokio::test(start_paused = true)]
async fn a_workspace_already_settled_ends_at_once_without_a_warning() {
    let src = Scripted::new(vec![st(ALL_OK)]);
    let started = tokio::time::Instant::now();
    let (end, lines) = follow(&src, 1).await;
    assert!(lines.is_empty());
    assert_eq!(started.elapsed(), std::time::Duration::ZERO);
    assert_eq!(follow_warning(&end, FOLLOW_LIMIT, true), None);
}

#[tokio::test(start_paused = true)]
async fn an_old_failure_warns_only_for_a_task_this_start_ran() {
    let src = Scripted::new(vec![st(SETUP_FAILED)]);
    let (end, lines) = follow(&src, 2).await;
    assert!(lines.is_empty());
    assert!(matches!(end, FollowEnd::Settled { first_read: true, .. }), "{end:?}");
    // Attached to a workspace that failed long ago: no toast.
    assert_eq!(follow_warning(&end, FOLLOW_LIMIT, false), None);
    // This start ran the task and it failed before the first read.
    let w = follow_warning(&end, FOLLOW_LIMIT, true).unwrap();
    assert!(w.contains("The setup script failed (exit 2)."), "{w}");
}

#[tokio::test(start_paused = true)]
async fn follow_times_out_after_35_minutes() {
    let src = Scripted::new(vec![st(CLONING_1)]);
    let started = tokio::time::Instant::now();
    let (end, lines) = follow(&src, 3).await;
    assert_eq!(lines, ["Cloning repos… (1/3)"]);
    assert!(matches!(end, FollowEnd::TimedOut(Some(_))), "{end:?}");
    assert!(started.elapsed() <= FOLLOW_LIMIT);
    assert!(started.elapsed() > FOLLOW_LIMIT - FOLLOW_INTERVAL * 2);
    let w = follow_warning(&end, FOLLOW_LIMIT, false).unwrap();
    assert!(w.contains("still cloning its repos after 35 minutes"), "{w}");
}

#[tokio::test(start_paused = true)]
async fn a_timed_out_setup_says_so_and_carries_any_clone_failure() {
    let src = Scripted::new(vec![st(SETUP_2)]);
    let (end, _) = follow(&src, 2).await;
    let w = follow_warning(&end, FOLLOW_LIMIT, false).unwrap();
    assert!(w.contains("still running its setup script"), "{w}");
    assert!(w.contains("Cloning failed for a repo: b."), "{w}");
}

#[tokio::test(start_paused = true)]
async fn a_lost_agent_ends_the_follow_quietly() {
    let src = Scripted::new(vec![st(CLONING_1), StatusRead::Gone]);
    let (end, lines) = follow(&src, 1).await;
    assert_eq!(end, FollowEnd::Lost);
    assert_eq!(lines, ["Cloning repos… (1/1)"]);
    assert_eq!(follow_warning(&end, FOLLOW_LIMIT, false), None);
}

#[tokio::test(start_paused = true)]
async fn failing_reads_are_retried_then_give_up_quietly() {
    let fail = || StatusRead::Failed("tunnel hiccup".into());
    // Four failures, then a good read: the count resets.
    let src = Scripted::new(vec![fail(), fail(), fail(), fail(), st(ALL_OK)]);
    assert!(matches!(follow(&src, 1).await.0, FollowEnd::Settled { .. }));
    let src = Scripted::new(vec![fail()]);
    let (end, _) = follow(&src, 1).await;
    assert_eq!(end, FollowEnd::Lost);
    assert_eq!(src.count(), MAX_READ_FAILURES as usize);
}

#[test]
fn status_parses_the_scripts_output_and_tolerates_drift() {
    let s = parse_status(&format!("a stray warning\n{SETUP_FAILED}\n\n")).unwrap();
    assert_eq!(s.setup.state, SetupState::Failed);
    assert_eq!(s.setup.code, Some(2));
    assert!(s.clone.done);
    // Unknown fields and states, missing fields.
    let s = parse_status(r#"{"clone":{"done":true,"extra":1},"setup":{"state":"paused"},"new":{}}"#)
        .unwrap();
    assert!(s.clone.repos.is_empty());
    assert_eq!(s.setup.state, SetupState::Failed);
    assert_eq!(parse_status("{}").unwrap(), WorkspaceStatus::default());
    assert!(parse_status("").is_err());
    assert!(parse_status("not json").is_err());
}

#[test]
fn progress_lines_count_repos_and_clean_log_lines() {
    let s = parse_status(NOT_STARTED).unwrap();
    assert_eq!(s.progress_line(0).as_deref(), Some("Cloning repos…"));
    assert_eq!(s.progress_line(3).as_deref(), Some("Cloning repos… (1/3)"));
    let s = parse_status(ALL_OK).unwrap();
    assert!(s.settled());
    assert_eq!(s.progress_line(1), None);
    let mut s = parse_status(SETUP_2).unwrap();
    s.setup.tail = vec![format!("\x1b[31m{}\x1b[0m", "x".repeat(500))];
    let l = s.last_log_line().unwrap();
    assert_eq!(l.chars().count(), MAX_PROGRESS_LINE);
    assert!(l.starts_with("[31mxxx") && l.ends_with('…'), "{l}");
    assert!(!l.contains('\x1b'));
}

#[test]
fn status_serialises_in_the_scripts_shape() {
    let s = parse_status(SETUP_FAILED).unwrap();
    let v = serde_json::to_value(&s).unwrap();
    assert_eq!(v, serde_json::from_str::<serde_json::Value>(SETUP_FAILED).unwrap());
}

#[test]
fn the_status_command_reports_a_missing_script_as_127() {
    let cmd = status_command();
    assert_eq!(cmd[..2], ["sh", "-c"]);
    assert!(cmd[2].contains("|| exit 127") && cmd[2].contains(STATUS_SCRIPT));
}

#[tokio::test]
async fn a_local_project_has_no_workspace_status_and_no_follower() {
    let p = Project::new("p".into(), vec![]);
    assert_eq!(workspace_status(&p).await, Ok(None));
    assert!(!spawn_follow(&p, |_| panic!("no warning for Docker")));
    assert!(!tunnels::is_following(&p.id));
}

/// Never answers: a follow in progress until something ends it.
struct Hang;
impl StatusSource for Hang {
    async fn read(&self) -> StatusRead {
        std::future::pending().await
    }
}

#[tokio::test(start_paused = true)]
async fn stop_ends_a_follow_in_progress_without_a_warning() {
    let id = "follow-stop-test";
    let task = tokio::spawn(async move {
        let warned = std::sync::Arc::new(Mutex::new(false));
        let w = warned.clone();
        run_follow(&Hang, id, 1, true, FollowTiming::default(), move |_| *w.lock().unwrap() = true)
            .await;
        let v = *warned.lock().unwrap();
        v
    });
    tokio::time::sleep(FOLLOW_INTERVAL * 3).await;
    assert!(tunnels::is_following(id));
    // What Stop and Remove call (`stop_project_container`, `stop_project`,
    // `remove_resources`); closing the tunnel does the same.
    tunnels::cancel_follow(id);
    let warned = tokio::time::timeout(std::time::Duration::from_secs(1), task)
        .await
        .expect("the follow ends at once")
        .unwrap();
    assert!(!warned);
    assert!(!tunnels::is_following(id));
}

#[tokio::test(start_paused = true)]
async fn a_new_follow_replaces_the_old_one() {
    let id = "follow-replace-test";
    let first = tokio::spawn(run_follow(&Hang, id, 1, true, FollowTiming::default(), |_| {}));
    tokio::time::sleep(FOLLOW_INTERVAL).await;
    let (_, second) = tunnels::begin_follow(id);
    tokio::time::timeout(std::time::Duration::from_secs(1), first).await.unwrap().unwrap();
    // The old follower's end does not unregister the new one.
    assert!(tunnels::is_following(id));
    assert!(!second.is_cancelled());
    tunnels::cancel_follow(id);
    assert!(second.is_cancelled());
}

#[test]
fn the_follower_takes_no_project_lock() {
    // Stop must never wait for it (ruling R5).
    let src = include_str!("../status.rs");
    assert!(!src.contains("project_lock"));
}
