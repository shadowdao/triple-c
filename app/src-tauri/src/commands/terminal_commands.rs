use tauri::{AppHandle, Emitter, State};

use crate::commands::aws_commands;
use crate::host::persistent::{self, TerminalKind};
use crate::host::ContainerTarget;
use crate::models::{
    Backend, BedrockAuthMethod, OpenedTerminal, Project, ProjectStatus, ReattachOutcome,
    WorkspaceTerminal,
};
use crate::storage::terminal_store::{self, PersistedTerminal};
use crate::AppState;

/// Build the command to run in the container terminal.
///
/// Always a `bash -c` script, because every session runs [`UPDATE_PRELUDE`]
/// before `exec claude`. For Bedrock Profile projects the script additionally
/// validates the AWS session first, and runs `aws sso login` if it has expired
/// so the user can re-authenticate (the URL is clickable via xterm.js
/// WebLinksAddon).
fn build_terminal_cmd(
    project: &Project,
    state: &AppState,
    session_name: Option<&str>,
    resume_session_id: Option<&str>,
) -> Vec<String> {
    let settings = state.settings_store.get();
    build_claude_terminal_cmd(
        project,
        settings.global_aws.aws_profile.as_deref(),
        session_name,
        resume_session_id,
    )
}

/// Shell line run immediately before `exec claude` in every Claude terminal
/// session.
///
/// `container/entrypoint.sh` already runs `claude update` when the container
/// starts, but containers here use a stop/start (and often just keep running)
/// model, so a long-lived container's CLI goes stale between restarts. Running
/// it per session is what keeps a week-old container current.
///
/// Deliberately non-fatal and time-bounded: `|| echo` swallows a failure (no
/// network, npm registry down) so a session always opens, and `timeout 60`
/// bounds how long a user waits for a terminal.
///
/// **`flock` is load-bearing, not tidiness.** Nothing serialises this against
/// the entrypoint's own `claude update`, and the entrypoint prints "container
/// ready" only *after* its copy finishes — so "start the project, open a tab"
/// races two updaters against the same `~/.claude/bin` install, as does
/// opening two tabs at once. `|| echo` would then hide a half-written install
/// behind a friendly message and the very next line (`exec claude`) would run
/// it. `-w 90` gives the entrypoint's `timeout 120` copy room to finish rather
/// than failing the wait, and `-E 0` makes losing the race a success: the
/// other holder just updated, so there is nothing left to do.
pub(crate) const UPDATE_PRELUDE: &str = concat!(
    "flock -w 90 -E 0 /tmp/.triple-c-claude-update.lock ",
    r#"timeout 60 claude update 2>&1 || echo "(update skipped — continuing)""#,
);

/// Single-quote one argument for interpolation into a shell script string.
fn shell_quote_arg(arg: &str) -> String {
    format!(" '{}'", arg.replace('\'', "'\\''"))
}

/// The testable core of [`build_terminal_cmd`], taking the resolved global AWS
/// profile rather than the whole [`AppState`].
///
/// `resume_session_id` adds `--resume <id>` (flag spelling checked against the
/// CLI in the container image) and drops `-n`: the resumed conversation keeps
/// the name it already has, and the tab label the caller passes as
/// `session_name` — often just "resume" — must not overwrite it.
fn build_claude_terminal_cmd(
    project: &Project,
    global_aws_profile: Option<&str>,
    session_name: Option<&str>,
    resume_session_id: Option<&str>,
) -> Vec<String> {
    let is_bedrock_profile = project.backend == Backend::Bedrock
        && project
            .bedrock_config
            .as_ref()
            .map(|b| b.auth_method == BedrockAuthMethod::Profile)
            .unwrap_or(false);

    let permission_args = project.effective_permission_mode().cli_args();

    // The args are interpolated into a shell script string, so single-quote
    // each one.
    let session_flags = match resume_session_id {
        Some(id) => format!(" --resume{}", shell_quote_arg(id)),
        None => session_name
            .filter(|n| !n.is_empty())
            .map(|n| format!(" -n{}", shell_quote_arg(n)))
            .unwrap_or_default(),
    };
    let permission_flags: String = permission_args.iter().map(|a| shell_quote_arg(a)).collect();
    let claude_cmd = format!("exec claude{}{}", permission_flags, session_flags);

    if !is_bedrock_profile {
        return vec![
            "bash".to_string(),
            "-c".to_string(),
            format!("{}\n{}\n", UPDATE_PRELUDE, claude_cmd),
        ];
    }

    let profile = aws_commands::resolve_profile_for_project(project, global_aws_profile);

    // Build a bash wrapper that validates credentials, re-auths if needed,
    // then exec's into claude.

    let script = format!(
        r#"
echo "Validating AWS session for profile '{profile}'..."
if aws sts get-caller-identity --profile '{profile}' >/dev/null 2>&1; then
    echo "AWS session valid."
else
    echo "AWS session expired or invalid."
    # Check if this profile uses SSO (has sso_start_url or sso_session configured)
    if aws configure get sso_start_url --profile '{profile}' >/dev/null 2>&1 || \
       aws configure get sso_session --profile '{profile}' >/dev/null 2>&1; then
        echo "Starting SSO login..."
        echo ""
        triple-c-sso-refresh
        if [ $? -ne 0 ]; then
            echo ""
            echo "SSO login failed or was cancelled. Starting Claude anyway..."
            echo "You may see authentication errors."
            echo ""
        fi
    else
        echo "Profile '{profile}' does not use SSO. Check your AWS credentials."
        echo "Starting Claude anyway..."
        echo ""
    fi
fi
{update_prelude}
{claude_cmd}
"#,
        profile = profile,
        update_prelude = UPDATE_PRELUDE,
        claude_cmd = claude_cmd
    );

    vec![
        "bash".to_string(),
        "-c".to_string(),
        script,
    ]
}

/// Open a terminal tab. On an AWS project whose image has tmux the command
/// runs inside its own tmux server keyed by `session_id`
/// (`host::persistent`), so it outlives the app; the tab is remembered so the
/// app reopens it on its next connect.
#[tauri::command]
pub async fn open_terminal_session(
    project_id: String,
    session_id: String,
    session_type: Option<String>,
    session_name: Option<String>,
    resume_session_id: Option<String>,
    app_handle: AppHandle,
    state: State<'_, AppState>,
) -> Result<OpenedTerminal, String> {
    let project = state
        .projects_store
        .get(&project_id)
        .ok_or_else(|| format!("Project {} not found", project_id))?;

    // Waits while an AWS project's tunnel is being re-opened after a restart.
    let target = ContainerTarget::for_project_waiting(&project)
        .await
        .ok_or_else(|| "Container not running".to_string())?;

    if let Some(id) = resume_session_id.as_deref() {
        crate::commands::inspect_commands::validate_session_id(id)?;
    }

    let kind = if session_type.as_deref() == Some("bash") { TerminalKind::Bash } else { TerminalKind::Claude };
    let cmd = match session_type.as_deref() {
        Some("bash") if resume_session_id.is_some() => {
            return Err("Only a Claude session can resume a conversation".to_string());
        }
        Some("bash") => vec!["bash".to_string(), "-l".to_string()],
        _ => build_terminal_cmd(
            &project,
            &state,
            session_name.as_deref(),
            resume_session_id.as_deref(),
        ),
    };

    // AWS copy-at-connect: the credentials go into this terminal's own
    // environment (never the agent's). Empty for everything else.
    let env = crate::host::aws_host::terminal_env(&project, &state.settings_store.get()).await;

    let plan = persistent::plan(
        project.is_aws(),
        &target,
        &session_id,
        kind,
        session_name.as_deref(),
        cmd,
        env,
    )
    .await?;

    let output_event = format!("terminal-output-{}", session_id);
    let exit_event = format!("terminal-exit-{}", session_id);
    let app_handle_output = app_handle.clone();
    let app_handle_exit = app_handle.clone();

    state
        .exec_manager
        .create_session(
            &target,
            &session_id,
            plan.cmd,
            plan.env,
            move |data| {
                let _ = app_handle_output.emit(&output_event, data);
            },
            Box::new(move || {
                let _ = app_handle_exit.emit(&exit_event, ());
            }),
        )
        .await?;

    if plan.persistent {
        let entry = PersistedTerminal {
            id: session_id.clone(),
            kind,
            name: persistent::clean_name(session_name.as_deref()),
        };
        if let Err(e) = terminal_store::add(&project_id, entry) {
            log::warn!("Could not remember terminal {session_id}: {e}");
        }
    }
    Ok(OpenedTerminal { persistent: plan.persistent, notice: plan.notice })
}

#[tauri::command]
pub async fn terminal_input(
    session_id: String,
    data: Vec<u8>,
    state: State<'_, AppState>,
) -> Result<(), String> {
    state.exec_manager.send_input(&session_id, data).await
}

#[tauri::command]
pub async fn terminal_resize(
    session_id: String,
    cols: u16,
    rows: u16,
    state: State<'_, AppState>,
) -> Result<(), String> {
    state.exec_manager.resize(&session_id, cols, rows).await
}

/// Close a tab. A persistent AWS terminal is *ended* (its tmux server
/// killed) and forgotten: closing a tab is the one gesture that ends one.
#[tauri::command]
pub async fn close_terminal_session(
    session_id: String,
    state: State<'_, AppState>,
) -> Result<(), String> {
    // Close audio bridge if it exists
    let audio_session_id = format!("audio-{}", session_id);
    state.exec_manager.close_session(&audio_session_id).await;
    let session_target = state.exec_manager.get_target(&session_id).await.ok();
    // Close terminal session
    state.exec_manager.close_session(&session_id).await;

    if let Some((project_id, _)) = terminal_store::find(&session_id) {
        // A reattach of this tab still in flight must not remember it again.
        mark_closed(&session_id);
        if let Err(e) = terminal_store::remove(&session_id) {
            log::warn!("Could not forget terminal {session_id}: {e}");
        }
        // The project's current tunnel first: the session's own endpoint is
        // stale after a re-attach on a new port.
        let target = state
            .projects_store
            .get(&project_id)
            .and_then(|p| ContainerTarget::for_project(&p))
            .or(session_target);
        let ended = match target {
            Some(t) => persistent::end(&t, &session_id).await,
            None => Err("not connected".to_string()),
        };
        if let Err(e) = ended {
            // Still running in the workspace (a leftover Claude would keep
            // it up): end it at the next connect.
            log::warn!("Terminal {session_id}: could not end it now ({e}); ending it at the next connect");
            if let Err(e) = terminal_store::add_pending_end(&project_id, &session_id) {
                log::warn!("Could not remember to end terminal {session_id}: {e}");
            }
        }
    }
    Ok(())
}

/// Session ids whose tab was closed in this run of the app.
fn closed_ids() -> &'static std::sync::Mutex<std::collections::HashSet<String>> {
    static C: std::sync::OnceLock<std::sync::Mutex<std::collections::HashSet<String>>> =
        std::sync::OnceLock::new();
    C.get_or_init(Default::default)
}

fn mark_closed(session_id: &str) {
    closed_ids().lock().unwrap_or_else(|e| e.into_inner()).insert(session_id.to_string());
}

fn was_closed(session_id: &str) -> bool {
    closed_ids().lock().unwrap_or_else(|e| e.into_inner()).contains(session_id)
}

/// End the project's sessions whose tabs were closed while it could not be
/// reached; returns the ones ended (or found gone).
async fn end_pending(project_id: &str, target: &ContainerTarget, running: &[persistent::RunningSession]) -> Vec<String> {
    let pending = terminal_store::pending_end(project_id);
    let mut done = Vec::new();
    for key in pending {
        if !running.iter().any(|r| r.key == key) {
            done.push(key);
            continue;
        }
        match persistent::end(target, &key).await {
            Ok(()) => done.push(key),
            Err(e) => log::warn!("Could not end closed terminal {key}: {e}"),
        }
    }
    if !done.is_empty() {
        if let Err(e) = terminal_store::done_pending_end(project_id, &done) {
            log::warn!("Could not update the terminals to end: {e}");
        }
    }
    done
}

/// A connected AWS project's target, waiting for a re-attach in progress.
async fn aws_target(project: &Project) -> Result<ContainerTarget, String> {
    if !project.is_aws() {
        return Err("Only AWS workspaces keep terminals".into());
    }
    if project.status != ProjectStatus::Running {
        return Err(format!("“{}” is not running", project.name));
    }
    ContainerTarget::for_project_waiting(project)
        .await
        .ok_or_else(|| "Triple-C is not connected to the workspace".to_string())
}

/// The workspace's running terminals with what the app remembers of them,
/// remembered ones first in the order they were opened. Prunes remembered
/// entries the workspace no longer runs. Empty for a Docker project, a
/// stopped one and an image without tmux.
#[tauri::command]
pub async fn list_workspace_terminals(
    project_id: String,
    state: State<'_, AppState>,
) -> Result<Vec<WorkspaceTerminal>, String> {
    let project = state
        .projects_store
        .get(&project_id)
        .ok_or_else(|| format!("Project {} not found", project_id))?;
    if !project.is_aws() || project.status != ProjectStatus::Running {
        return Ok(Vec::new());
    }
    let target = aws_target(&project).await?;
    // An `Err` (an unanswered probe or listing) says nothing about what runs:
    // nothing is pruned or ended on it, and the caller asks again.
    let mut running = persistent::list(&target).await?;
    let ended = end_pending(&project_id, &target, &running).await;
    running.retain(|r| !ended.contains(&r.key));
    let keys = running.iter().map(|r| r.key.clone()).collect();
    if let Err(e) = terminal_store::retain(&project_id, &keys) {
        log::warn!("Could not prune remembered terminals: {e}");
    }
    Ok(merge_terminals(&terminal_store::list(&project_id), running))
}

/// Remembered sessions first (in their order), then the others oldest first.
pub(crate) fn merge_terminals(
    remembered: &[PersistedTerminal],
    mut running: Vec<persistent::RunningSession>,
) -> Vec<WorkspaceTerminal> {
    running.sort_by_key(|r| {
        (
            remembered.iter().position(|m| m.id == r.key).unwrap_or(usize::MAX),
            r.created_unix,
        )
    });
    running
        .into_iter()
        .map(|r| {
            let mine = remembered.iter().find(|m| m.id == r.key);
            WorkspaceTerminal {
                kind: mine.map(|m| m.kind).unwrap_or(r.kind),
                name: mine.map(|m| m.name.clone()).unwrap_or(r.name),
                known: mine.is_some(),
                session_id: r.key,
                created_unix: r.created_unix,
                attached: r.attached,
            }
        })
        .collect()
}

/// Attach a tab to a terminal still running in the workspace: a tab the app
/// reopens after a restart or a dropped connection, or one picked in Project
/// Home → Sessions. Never starts anything: a session that is gone is
/// `Ended` (and forgotten). With `rows` (a fresh xterm), the pane's history
/// is replayed into the tab's output first, so its scrollback comes back.
/// The tab's output listener must be registered before this is called.
#[tauri::command]
pub async fn reattach_terminal_session(
    project_id: String,
    session_id: String,
    rows: Option<u16>,
    app_handle: AppHandle,
    state: State<'_, AppState>,
) -> Result<ReattachOutcome, String> {
    persistent::validate_key(&session_id)?;
    let project = state
        .projects_store
        .get(&project_id)
        .ok_or_else(|| format!("Project {} not found", project_id))?;
    if was_closed(&session_id) {
        return Ok(ReattachOutcome::Ended);
    }
    let target = aws_target(&project).await?;
    // Unanswered → `Err` (the tab retries); only a real "no tmux" ends it.
    if !persistent::supported(&target).await? {
        terminal_store::remove(&session_id).ok();
        return Ok(ReattachOutcome::Ended);
    }
    if terminal_store::pending_end(&project_id).contains(&session_id) {
        return Ok(ReattachOutcome::Ended);
    }
    let running = persistent::list(&target).await?;
    let Some(found) = running.into_iter().find(|r| r.key == session_id) else {
        if let Err(e) = terminal_store::remove(&session_id) {
            log::warn!("Could not forget terminal {session_id}: {e}");
        }
        return Ok(ReattachOutcome::Ended);
    };
    let remembered = terminal_store::find(&session_id).filter(|(p, _)| *p == project_id);
    let (kind, name) = match remembered {
        Some((_, m)) => (m.kind, m.name),
        None => (found.kind, found.name),
    };

    let output_event = format!("terminal-output-{}", session_id);
    if let Some(rows) = rows {
        // Exactly one chunk, empty when there is no history: the tab treats
        // the first chunk after asking for a replay as the replay, so live
        // output after it gets its URL/OSC/SSO handling again.
        let bytes = persistent::history(&target, &session_id)
            .await
            .map(|h| persistent::replay_bytes(&h, rows))
            .unwrap_or_default();
        let _ = app_handle.emit(&output_event, bytes);
    }

    let exit_event = format!("terminal-exit-{}", session_id);
    let app_handle_output = app_handle.clone();
    let app_handle_exit = app_handle.clone();
    state
        .exec_manager
        .create_session(
            &target,
            &session_id,
            persistent::attach_cmd(&session_id)?,
            persistent::client_env(Vec::new()),
            move |data| {
                let _ = app_handle_output.emit(&output_event, data);
            },
            Box::new(move || {
                let _ = app_handle_exit.emit(&exit_event, ());
            }),
        )
        .await?;

    // Closed while this attach was under way: its close already ended (or
    // queued the end of) the session; don't remember it again.
    if was_closed(&session_id) {
        state.exec_manager.close_session(&session_id).await;
        return Ok(ReattachOutcome::Ended);
    }
    let entry = PersistedTerminal { id: session_id.clone(), kind, name: name.clone() };
    if let Err(e) = terminal_store::add(&project_id, entry) {
        log::warn!("Could not remember terminal {session_id}: {e}");
    }
    Ok(ReattachOutcome::Reattached { kind, name })
}

/// End a terminal running in the workspace that has no tab here (Project
/// Home → Sessions → End). A tab's own close goes through
/// `close_terminal_session`.
#[tauri::command]
pub async fn end_workspace_terminal(
    project_id: String,
    session_id: String,
    state: State<'_, AppState>,
) -> Result<(), String> {
    persistent::validate_key(&session_id)?;
    let project = state
        .projects_store
        .get(&project_id)
        .ok_or_else(|| format!("Project {} not found", project_id))?;
    let target = aws_target(&project).await?;
    state.exec_manager.close_session(&session_id).await;
    persistent::end(&target, &session_id).await?;
    if let Err(e) = terminal_store::remove(&session_id) {
        log::warn!("Could not forget terminal {session_id}: {e}");
    }
    Ok(())
}

#[tauri::command]
pub async fn paste_image_to_terminal(
    session_id: String,
    image_data: Vec<u8>,
    state: State<'_, AppState>,
) -> Result<String, String> {
    let target = state.exec_manager.get_target(&session_id).await?;

    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let file_name = format!("clipboard_{}.png", timestamp);

    state
        .exec_manager
        .write_file_to_container(&target, &file_name, &image_data)
        .await
}

/// Copy a host file (e.g. dragged onto the terminal) into the container so
/// Claude Code can read it, and return the in-container path. Mirrors the
/// image-paste flow: the file is placed under /tmp/triple-c-drops/ keeping its
/// original name. Returns an error for paths that aren't readable regular files
/// (e.g. a dropped directory).
#[tauri::command]
pub async fn upload_host_file_to_terminal(
    session_id: String,
    host_path: String,
    state: State<'_, AppState>,
) -> Result<String, String> {
    // The drop target is a host path chosen by the webview, not by the OS drag
    // itself, so it goes through `file_commands`' host-read policy: absolute,
    // no traversal, and nothing whose path passes through a hidden directory
    // (`~/.ssh`, `~/.aws`, `~/.local/bin`) or a system location — applied to
    // the path with its symlinks already resolved, so a visible directory that
    // *leads* to one of those is refused too. What comes back is that resolved
    // path, and it is what gets opened. Four commands touch a host path now,
    // but only two take it *over IPC*: this one and `download_container_backup`.
    // The Files pane's `download_container_file` and `upload_files_to_container`
    // open their dialog from Rust instead, so for them the policy above is
    // defence in depth and for these two it is the boundary itself.
    // The name is taken from the path the user actually dropped, *before*
    // resolution. Deriving it from the resolved path renames the file behind
    // the user's back: dropping `~/Downloads/latest.log`, where `latest.log` is
    // a symlink, would land it in the container as `2026-08-23.log`.
    let base = crate::commands::file_commands::host_upload_name(&host_path)?;
    let host_path = crate::commands::file_commands::resolve_host_read_path(&host_path).await?;

    let target = state.exec_manager.get_target(&session_id).await?;

    let meta = tokio::fs::metadata(&host_path)
        .await
        .map_err(|e| format!("Cannot access {}: {}", host_path, e))?;
    // `!is_file()`, not `!is_dir()`. A FIFO is neither a directory nor a
    // regular file, reports `len() == 0`, and passes both the directory check
    // and the size cap below — and `std::fs::File::open` on one blocks forever
    // with no writer, with no timeout anywhere on this path. The upload then
    // never returns, the toast sticks on "Adding N files…" for the session and
    // the rest of the batch is abandoned. Sockets and device nodes are the same
    // shape. This is one of two routes for getting a host file into a
    // container (the Files pane's upload is the other), so it is the wrong
    // place to be clever.
    if !meta.is_file() {
        return Err(if meta.is_dir() {
            format!("{} is a directory — drop individual files", host_path)
        } else {
            format!(
                "{} is not a regular file — only ordinary files can be dropped into a terminal",
                host_path
            )
        });
    }

    // Guard against ballooning host RAM: the file is packed into an in-memory
    // tar before upload, so cap the size of a dropped file. The ceiling lives
    // with the code that does the reading, which re-applies it to the open
    // descriptor — this check is here only so the refusal reads like a sentence
    // instead of arriving after a 300 MB read.
    use crate::host::MAX_DROP_BYTES;
    if meta.len() > MAX_DROP_BYTES {
        return Err(format!(
            "File too large to drop into the terminal ({:.0} MB; limit {} MB). Mount it into the project instead.",
            meta.len() as f64 / (1024.0 * 1024.0),
            MAX_DROP_BYTES / (1024 * 1024)
        ));
    }



    // Ensure the destination directory exists rather than relying on Docker's
    // archive extractor to create the parent for the uploaded tar entry.
    target
        .exec_oneshot(vec![
            "mkdir".to_string(),
            "-p".to_string(),
            "/tmp/triple-c-drops".to_string(),
        ])
        .await?;

    let file_name = format!("triple-c-drops/{}", base);
    target
        .upload_host_file(&host_path, "/tmp", &file_name)
        .await
}

#[tauri::command]
pub async fn start_audio_bridge(
    session_id: String,
    state: State<'_, AppState>,
) -> Result<(), String> {
    // Get the container from the terminal session
    let target = state.exec_manager.get_target(&session_id).await?;

    // Create audio bridge exec session with ID "audio-{session_id}"
    // The loop handles reconnection when the FIFO reader (fake rec) is killed and restarted
    let audio_session_id = format!("audio-{}", session_id);
    let cmd = vec![
        "bash".to_string(),
        "-c".to_string(),
        "FIFO=/tmp/triple-c-audio-input; [ -p \"$FIFO\" ] || mkfifo \"$FIFO\"; trap '' PIPE; while true; do cat > \"$FIFO\" 2>/dev/null; sleep 0.1; done".to_string(),
    ];

    state
        .exec_manager
        .create_session_with_tty(
            &target,
            &audio_session_id,
            cmd,
            false,
            |_data| { /* ignore output from the audio bridge */ },
            Box::new(|| { /* no exit handler needed */ }),
        )
        .await
}

#[tauri::command]
pub async fn send_audio_data(
    session_id: String,
    data: Vec<u8>,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let audio_session_id = format!("audio-{}", session_id);
    state.exec_manager.send_input(&audio_session_id, data).await
}

#[tauri::command]
pub async fn stop_audio_bridge(
    session_id: String,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let audio_session_id = format!("audio-{}", session_id);
    state.exec_manager.close_session(&audio_session_id).await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{build_claude_terminal_cmd, UPDATE_PRELUDE};
    use crate::models::Project;

    /// A dropped file must be named the way the *user* named it.
    ///
    /// The bug this pins: `upload_host_file_to_terminal` derived the tar entry
    /// name from the path *after* symlink resolution, so dropping
    /// `~/Downloads/latest.log` — where `latest.log` is a symlink to
    /// `2026-08-23.log` — silently landed the file in the container under the
    /// target's name. Nothing errored; the user just got a name they never
    /// typed.
    ///
    /// This asserts the shared helper's contract from the terminal side: the
    /// answer comes from the spelling, and a path that does not name a file is
    /// refused rather than silently substituted (it used to fall back to
    /// `"dropped-file"`).
    /// A `Project` with only the fields these tests care about set; the rest
    /// come through serde so the test does not have to track every field.
    fn project(backend: &str, bedrock_config: serde_json::Value) -> Project {
        serde_json::from_value(serde_json::json!({
            "id": "p1",
            "name": "Test",
            "paths": [],
            "container_id": null,
            "status": "running",
            "backend": backend,
            "bedrock_config": bedrock_config,
            "ollama_config": null,
            "openai_compatible_config": null,
            "allow_docker_access": false,
            "full_permissions": false,
            "ssh_key_path": null,
            "git_user_name": null,
            "git_user_email": null,
            "created_at": "now",
            "updated_at": "now"
        }))
        .expect("test project deserializes")
    }

    /// Every Claude session updates the CLI before launching it.
    ///
    /// `container/entrypoint.sh` only updates at container *start*, and these
    /// containers are long-lived, so a stale CLI is the normal case without
    /// this. The plain (non-Bedrock) path therefore has to be a `bash -c`
    /// wrapper rather than a bare `claude` argv.
    #[test]
    fn build_terminal_cmd_updates_before_launching_claude() {
        let cmd = build_claude_terminal_cmd(&project("anthropic", serde_json::Value::Null), None, None, None);

        assert_eq!(cmd[0], "bash");
        assert_eq!(cmd[1], "-c");
        assert!(
            cmd[2].contains(UPDATE_PRELUDE),
            "plain path must run the update prelude: {}",
            cmd[2]
        );
        assert!(cmd[2].contains("exec claude"), "got: {}", cmd[2]);
        // The update has to happen *before* the exec, which never returns.
        assert!(
            cmd[2].find(UPDATE_PRELUDE).unwrap() < cmd[2].find("exec claude").unwrap(),
            "prelude must precede the exec: {}",
            cmd[2]
        );
        assert!(
            UPDATE_PRELUDE.contains("timeout 60") && UPDATE_PRELUDE.contains("||"),
            "the update must stay time-bounded and non-fatal"
        );
    }

    /// The session name is interpolated into a shell script, so a quote in it
    /// must not break out of its single-quoted argument.
    #[test]
    fn build_terminal_cmd_escapes_a_quoted_session_name() {
        let cmd = build_claude_terminal_cmd(
            &project("anthropic", serde_json::Value::Null),
            None,
            Some("Bob's tab; rm -rf /"),
            None,
        );

        assert!(
            cmd[2].contains(r#"exec claude -n 'Bob'\''s tab; rm -rf /'"#),
            "session name must be single-quote escaped: {}",
            cmd[2]
        );
    }

    /// Permission flags travel the same escaped path, and an empty name adds
    /// no `-n` at all.
    #[test]
    fn build_terminal_cmd_quotes_permission_flags_and_omits_an_empty_name() {
        let mut p = project("anthropic", serde_json::Value::Null);
        p.full_permissions = true;
        let cmd = build_claude_terminal_cmd(&p, None, Some(""), None);

        assert!(
            cmd[2].contains("exec claude '--dangerously-skip-permissions'\n"),
            "got: {}",
            cmd[2]
        );
        assert!(!cmd[2].contains(" -n "), "empty name must add no flag: {}", cmd[2]);
    }

    /// Auto mode is passed as a `--permission-mode` value, not its own flag.
    #[test]
    fn build_terminal_cmd_passes_auto_permission_mode() {
        let mut p = project("anthropic", serde_json::Value::Null);
        p.permission_mode = Some(crate::models::project::PermissionMode::Auto);
        let cmd = build_claude_terminal_cmd(&p, None, None, None);

        assert!(
            cmd[2].contains("exec claude '--permission-mode' 'auto'"),
            "got: {}",
            cmd[2]
        );
    }

    /// The Bedrock-profile path keeps its AWS validation *and* gains the
    /// prelude, immediately before the exec.
    #[test]
    fn build_terminal_cmd_bedrock_validates_aws_and_updates() {
        let cmd = build_claude_terminal_cmd(
            &project("bedrock", serde_json::json!({
                "auth_method": "profile",
                "aws_region": "us-east-1",
                "aws_profile": "acme",
                "model_id": null,
                "disable_prompt_caching": false
            })),
            None,
            Some("it's fine"),
            None,
        );

        assert_eq!(cmd[0], "bash");
        let script = &cmd[2];
        assert!(script.contains("aws sts get-caller-identity --profile 'acme'"), "got: {}", script);
        assert!(script.contains("triple-c-sso-refresh"), "got: {}", script);
        assert!(script.contains(UPDATE_PRELUDE), "got: {}", script);
        assert!(script.contains(r#"exec claude -n 'it'\''s fine'"#), "got: {}", script);
        assert!(
            script.find(UPDATE_PRELUDE).unwrap() < script.find("exec claude").unwrap(),
            "prelude must precede the exec: {}",
            script
        );
    }

    /// Resume goes through the same launch as a new session — prelude,
    /// permission flags — and passes the id instead of the tab label, so a
    /// label of "resume" does not rename the conversation.
    #[test]
    fn build_terminal_cmd_resumes_without_renaming() {
        let mut p = project("anthropic", serde_json::Value::Null);
        p.full_permissions = true;
        let cmd = build_claude_terminal_cmd(
            &p,
            None,
            Some("resume"),
            Some("e13d312d-2f38-4cf6-b0e0-1db60208a74c"),
        );

        assert!(cmd[2].contains(UPDATE_PRELUDE), "got: {}", cmd[2]);
        assert!(
            cmd[2].contains(
                "exec claude '--dangerously-skip-permissions' --resume 'e13d312d-2f38-4cf6-b0e0-1db60208a74c'\n"
            ),
            "got: {}",
            cmd[2]
        );
        assert!(!cmd[2].contains(" -n "), "resume must not rename: {}", cmd[2]);
    }

    /// The app's own tabs come first in the order they were opened, with
    /// the app's labels; sessions from elsewhere follow, oldest first, and
    /// are not "known" (so they are listed, never auto-opened).
    #[test]
    fn workspace_terminals_merge_what_the_app_remembers_with_what_runs() {
        use super::merge_terminals;
        use crate::host::persistent::{RunningSession, TerminalKind};
        use crate::storage::terminal_store::PersistedTerminal;
        let run = |key: &str, kind, created, name: Option<&str>| RunningSession {
            key: key.into(),
            kind,
            created_unix: created,
            attached: false,
            name: name.map(str::to_string),
        };
        let remembered = vec![
            PersistedTerminal { id: "b".into(), kind: TerminalKind::Claude, name: Some("second".into()) },
            PersistedTerminal { id: "a".into(), kind: TerminalKind::Bash, name: None },
            PersistedTerminal { id: "gone".into(), kind: TerminalKind::Bash, name: None },
        ];
        let running = vec![
            run("x", TerminalKind::Bash, 50, Some("web terminal")),
            run("a", TerminalKind::Bash, 10, None),
            run("w", TerminalKind::Claude, 5, None),
            run("b", TerminalKind::Claude, 20, Some("tmux label")),
        ];
        let got = merge_terminals(&remembered, running);
        let ids: Vec<_> = got.iter().map(|t| t.session_id.as_str()).collect();
        assert_eq!(ids, ["b", "a", "w", "x"]);
        assert!(got[0].known && got[1].known && !got[2].known && !got[3].known);
        assert_eq!(got[0].name.as_deref(), Some("second"), "the app's label wins");
        assert_eq!(got[3].name.as_deref(), Some("web terminal"));
    }

    #[test]
    fn a_dropped_file_keeps_the_name_the_user_dropped() {
        use crate::commands::file_commands::host_upload_name;

        assert_eq!(
            host_upload_name("/home/u/Downloads/latest.log").unwrap(),
            "latest.log"
        );
        assert!(
            host_upload_name("/home/u/Downloads/").is_err(),
            "a directory is not a file to drop"
        );
        assert!(
            host_upload_name("/home/u/..").is_err(),
            "the name becomes a tar entry, a container path and an argv element"
        );
    }
}
