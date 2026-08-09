//! Read-only introspection of what lives inside a project's container.
//!
//! Three inventories are exposed to the GUI:
//!   1. Claude Code sessions (transcripts on the persistent config volume)
//!   2. Container capabilities (skills / agents / commands / hooks / plugins / MCP)
//!   3. Scheduled tasks managed by the in-container `triple-c-scheduler`
//!
//! Everything here is read-only except the explicitly-mutating scheduler
//! commands at the bottom of the file (enable/disable, run, remove, clear
//! notifications), which shell out to the scheduler's own subcommands rather
//! than editing its state files.
//!
//! ## Container access
//!
//! All work happens inside the container via the existing `docker exec`
//! plumbing in [`crate::docker::exec`] — no second mechanism is introduced.
//! The heavy lifting (walking dirs, grepping transcripts, parsing JSON with
//! `jq`) runs *in* the container and only a small JSON summary crosses the
//! wire, so multi-megabyte transcripts are never streamed back.
//!
//! `HOME` is passed explicitly on every exec: `docker exec` inherits the
//! container image's environment rather than the target user's, so `$HOME` is
//! not reliably `/home/claude` otherwise (see `download_container_backup`,
//! which does the same).
//!
//! ## Injection safety
//!
//! Two rules, applied together (defense in depth):
//!
//! * The `sh -c` scripts below are compile-time constants. No caller-supplied
//!   value is ever interpolated into them.
//! * Every command that takes a caller-supplied id runs as a plain **argv
//!   vector** with no shell in the process tree at all, so shell metacharacters
//!   are inert by construction. On top of that, ids are validated against a
//!   strict allowlist ([`validate_task_id`], [`validate_session_id`]) that
//!   admits no shell metacharacters, no `/`, no `.` (so no path traversal into
//!   the scheduler's task dir), and no leading `-` (so no option injection).
//!
//! ## Degradation
//!
//! A stopped or missing container is a normal state, not an error: the
//! read-only commands return empty/zero results. Only the mutating scheduler
//! commands fail loudly, since they cannot do anything useful without a
//! running container.

use bollard::exec::{CreateExecOptions, StartExecOptions};
use serde::{Deserialize, Serialize};
use tauri::State;

use crate::docker::client::get_docker;
use crate::docker::container::is_container_running;
use crate::docker::exec::{exec_oneshot_env, exec_oneshot_env_status};
use crate::AppState;

/// Newest N session transcripts to inspect. Caps the work done inside the
/// container regardless of how much history has accumulated on the volume.
const MAX_SESSIONS: usize = 50;

/// Newest N scheduler notifications to return.
const MAX_NOTIFICATIONS: usize = 50;

const CONTAINER_HOME: &str = "/home/claude";

// ─────────────────────────────────────────────────────────────────────────────
// Response models
//
// These live here (rather than in `models/`) so this feature is confined to a
// single file; they are IPC response shapes, not persisted state.
// ─────────────────────────────────────────────────────────────────────────────

/// One Claude Code session transcript found inside the container.
#[derive(Debug, Clone, Serialize)]
pub struct ClaudeSession {
    /// Session UUID (the transcript's filename stem, and what `--resume` takes).
    pub id: String,
    /// User-set display name (`claude -n <name>`), if the session has one.
    pub name: Option<String>,
    /// Best available one-line description: Claude's auto-generated title if it
    /// produced one, otherwise the last prompt sent in the session.
    pub summary: Option<String>,
    /// Transcript mtime as an ISO 8601 / RFC 3339 timestamp (UTC).
    pub last_modified: String,
    pub size_bytes: u64,
    /// Approximate user + assistant turn count (counted by line, cheap).
    pub message_count: u64,
    /// The directory the session was started in.
    pub cwd: Option<String>,
}

/// A single installed capability (skill, agent, command, hook event, …).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapabilityItem {
    pub name: String,
    pub description: Option<String>,
    /// `"user"` (from `~/.claude`) or `"project"` (from a mounted workspace).
    pub scope: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CapabilityGroup {
    pub count: u64,
    pub items: Vec<CapabilityItem>,
}

/// Inventory of everything Claude Code has available inside the container.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ContainerCapabilities {
    pub skills: CapabilityGroup,
    pub agents: CapabilityGroup,
    pub commands: CapabilityGroup,
    /// One item per configured hook event; `count` is the total number of
    /// individual hook handlers across all events.
    pub hooks: CapabilityGroup,
    pub plugins: CapabilityGroup,
    pub mcp_servers: CapabilityGroup,
}

/// A task managed by the in-container `triple-c-scheduler`.
///
/// Mirrors the scheduler's own on-disk JSON schema
/// (`~/.claude/scheduler/tasks/<id>.json`).
#[derive(Debug, Clone, Serialize)]
pub struct ScheduledTask {
    pub id: String,
    pub name: String,
    pub prompt: String,
    /// Cron expression. One-shot tasks are also stored as a cron expression;
    /// see `at` for the original wall-clock time.
    pub schedule: String,
    /// `"recurring"` or `"once"` (the scheduler's `type` field).
    pub task_type: String,
    /// Original `--at` value (`"YYYY-MM-DD HH:MM"`) for one-shot tasks.
    pub at: Option<String>,
    pub enabled: bool,
    pub working_dir: String,
    pub created_at: Option<String>,
    /// Derived from the newest file in `~/.claude/scheduler/logs/<id>/`; the
    /// scheduler does not record this in the task JSON itself.
    pub last_run: Option<String>,
    /// Only known for enabled one-shot tasks (their `at` time). Recurring cron
    /// expressions are not evaluated here.
    pub next_run: Option<String>,
}

/// A completion notice written by `triple-c-task-runner` after a task ran.
#[derive(Debug, Clone, Serialize)]
pub struct SchedulerNotification {
    pub task_id: String,
    pub task_name: Option<String>,
    /// `"SUCCESS"` or `"FAILED (exit code N)"`.
    pub status: Option<String>,
    /// The runner's own human-readable timestamp line.
    pub time: Option<String>,
    pub task_type: Option<String>,
    /// Tail of the run's log that the runner captured.
    pub summary: Option<String>,
    /// Full notification text, verbatim.
    pub body: String,
    /// Notification file mtime, ISO 8601 (UTC).
    pub created_at: String,
}

// ─────────────────────────────────────────────────────────────────────────────
// Shared helpers
// ─────────────────────────────────────────────────────────────────────────────

/// Resolve a project to a *running* container id.
///
/// `Ok(None)` means "there is nothing to inspect" — no container recorded, or
/// the container exists but is stopped. Callers that are read-only turn that
/// into an empty result; mutating callers turn it into an error.
/// `Err` is reserved for a genuinely unknown project id.
async fn running_container_for(
    project_id: &str,
    state: &State<'_, AppState>,
) -> Result<Option<String>, String> {
    let project = state
        .projects_store
        .get(project_id)
        .ok_or_else(|| format!("Project {} not found", project_id))?;

    let container_id = match project.container_id {
        Some(id) => id,
        None => return Ok(None),
    };

    if is_container_running(&container_id).await.unwrap_or(false) {
        Ok(Some(container_id))
    } else {
        Ok(None)
    }
}

/// Same as [`running_container_for`], but a stopped container is an error.
/// Used by the mutating scheduler commands.
async fn require_running_container(
    project_id: &str,
    state: &State<'_, AppState>,
) -> Result<String, String> {
    running_container_for(project_id, state).await?.ok_or_else(|| {
        "Container is not running — start the project first.".to_string()
    })
}

fn home_env() -> Vec<String> {
    vec![format!("HOME={}", CONTAINER_HOME)]
}

/// Run one of this module's constant scripts under `sh -c` and return stdout.
///
/// The scripts redirect their own stderr to `/dev/null` (`exec 2>/dev/null` on
/// the first line) so the combined stream `exec_oneshot_env` returns is pure
/// stdout and stays parseable as JSON. A non-zero exit therefore surfaces as
/// empty output, which the callers treat as "nothing to report".
async fn run_script(container_id: &str, script: impl Into<String>) -> Result<String, String> {
    exec_oneshot_env(
        container_id,
        vec!["sh".to_string(), "-c".to_string(), script.into()],
        home_env(),
    )
    .await
}

/// Parse script output as JSON, degrading to a default value (and a log line)
/// rather than failing the whole command if the container returned something
/// unexpected.
fn parse_or_default<T: Default + serde::de::DeserializeOwned>(raw: &str, what: &str) -> T {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return T::default();
    }
    match serde_json::from_str::<T>(trimmed) {
        Ok(v) => v,
        Err(e) => {
            log::warn!(
                "Failed to parse {} JSON from container ({}): {}",
                what,
                e,
                trimmed.chars().take(300).collect::<String>()
            );
            T::default()
        }
    }
}

fn epoch_to_iso(epoch: i64) -> String {
    chrono::DateTime::from_timestamp(epoch, 0)
        .unwrap_or_default()
        .to_rfc3339()
}

/// Strict allowlist for scheduler task ids.
///
/// The scheduler generates ids as 8 lowercase hex chars (`head -c 4
/// /dev/urandom | od -An -tx1`). This accepts that plus a small tolerant
/// superset, while admitting **no** shell metacharacters, no `/` or `.` (so a
/// crafted id cannot escape `~/.claude/scheduler/tasks/`), and no leading `-`
/// (so it cannot be mistaken for an option). Combined with argv-only execution
/// this makes shell injection structurally impossible.
fn validate_task_id(id: &str) -> Result<(), String> {
    let valid = !id.is_empty()
        && id.len() <= 64
        && id.starts_with(|c: char| c.is_ascii_alphanumeric())
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if valid {
        Ok(())
    } else {
        Err(format!("Invalid scheduler task id: {:?}", id))
    }
}

/// Strict allowlist for session ids (Claude Code uses UUIDs).
fn validate_session_id(id: &str) -> Result<(), String> {
    let valid = !id.is_empty()
        && id.len() <= 64
        && id.starts_with(|c: char| c.is_ascii_alphanumeric())
        && id.chars().all(|c| c.is_ascii_hexdigit() || c == '-');
    if valid {
        Ok(())
    } else {
        Err(format!("Invalid session id: {:?}", id))
    }
}

/// Run `triple-c-scheduler <args…>` as a bare argv vector — no shell is
/// involved, so caller-supplied ids cannot be interpreted as shell syntax.
/// Returns the combined output, erroring with it on a non-zero exit.
async fn run_scheduler(container_id: &str, args: Vec<String>) -> Result<String, String> {
    let mut cmd = vec!["triple-c-scheduler".to_string()];
    cmd.extend(args);

    let (output, exit_code) = exec_oneshot_env_status(container_id, cmd, home_env()).await?;
    if exit_code != 0 {
        let detail = output.trim();
        return Err(if detail.is_empty() {
            format!("triple-c-scheduler failed (exit {})", exit_code)
        } else {
            detail.to_string()
        });
    }
    Ok(output)
}

// ─────────────────────────────────────────────────────────────────────────────
// 1. Sessions
// ─────────────────────────────────────────────────────────────────────────────

/// Emits a JSON array describing the newest transcripts on the config volume.
///
/// Layout (verified empirically against Claude Code 2.1.226): transcripts are
/// JSON Lines at `~/.claude/projects/<cwd-with-slashes-turned-into-dashes>/<uuid>.jsonl`.
///
/// Metadata is pulled with a single `grep -o` pass per file that yields whole
/// JSON key/value fragments; each fragment is already a valid JSON object body,
/// so wrapping it in braces and letting `jq` merge them decodes escapes
/// correctly without ever parsing a full transcript line-by-line. Later records
/// win (so the newest title/prompt is used) except `cwd`, where the first
/// record wins (the directory the session actually started in). Malformed lines
/// simply fail to match and are skipped.
const SESSIONS_SCRIPT: &str = r#"exec 2>/dev/null
set -u
ROOT="$HOME/.claude/projects"
[ -d "$ROOT" ] || { echo '[]'; exit 0; }
TAB=$(printf '\t')
find "$ROOT" -mindepth 2 -maxdepth 2 -name '*.jsonl' -type f -printf '%T@\t%s\t%p\n' \
  | sort -rn | head -__MAX__ \
  | while IFS="$TAB" read -r mtime size path; do
      [ -n "${path:-}" ] || continue
      [ "${size:-0}" -gt 0 ] || continue
      id=$(basename "$path" .jsonl)
      meta=$(grep -aoE '"(cwd|aiTitle|customTitle|agentName|lastPrompt|summary)":"([^"\\]|\\.)*"' "$path" \
             | sed 's/^/{/; s/$/}/' \
             | jq -c -s '(reduce .[] as $o ({}; . + $o)) + (([.[] | select(has("cwd"))] | first) // {})') || meta=''
      [ -n "$meta" ] || meta='{}'
      count=$(grep -acE '"type":"(user|assistant)"' "$path") || count=0
      jq -c -n --arg id "$id" --arg mt "${mtime%%.*}" --arg sz "$size" --arg mc "$count" --argjson meta "$meta" \
        '{id: $id,
          modified_epoch: ($mt | tonumber),
          size_bytes: ($sz | tonumber),
          message_count: ($mc | tonumber),
          name: ($meta.customTitle // $meta.agentName // null),
          summary: ($meta.aiTitle // $meta.summary // $meta.lastPrompt // null),
          cwd: ($meta.cwd // null)}'
    done | jq -s '.'
"#;

#[derive(Debug, Deserialize)]
struct RawSession {
    id: String,
    modified_epoch: i64,
    size_bytes: u64,
    message_count: u64,
    name: Option<String>,
    summary: Option<String>,
    cwd: Option<String>,
}

/// List the Claude Code sessions stored inside a project's container, newest
/// first, capped at [`MAX_SESSIONS`].
///
/// Returns an empty vec (no error) when the container is stopped or has never
/// been started.
#[tauri::command]
pub async fn list_claude_sessions(
    project_id: String,
    state: State<'_, AppState>,
) -> Result<Vec<ClaudeSession>, String> {
    let container_id = match running_container_for(&project_id, &state).await? {
        Some(id) => id,
        None => return Ok(Vec::new()),
    };

    // `head -N` is the only piece of the script that varies, and it comes from
    // a const usize — never from the caller.
    let script = SESSIONS_SCRIPT.replace("__MAX__", &MAX_SESSIONS.to_string());

    let raw = run_script(&container_id, script).await?;
    let sessions: Vec<RawSession> = parse_or_default(&raw, "session list");

    Ok(sessions
        .into_iter()
        .map(|s| ClaudeSession {
            id: s.id,
            name: s.name.filter(|v| !v.is_empty()),
            summary: s.summary.filter(|v| !v.is_empty()),
            last_modified: epoch_to_iso(s.modified_epoch),
            size_bytes: s.size_bytes,
            message_count: s.message_count,
            cwd: s.cwd.filter(|v| !v.is_empty()),
        })
        .collect())
}

/// Build the shell command line that resumes a session, for the frontend to
/// drop into a terminal.
///
/// The flag spelling was checked against the CLI in the container image:
/// `claude --resume <session-id>` (short form `-r`).
///
/// The project's permission mode is folded in so the resumed session behaves
/// like a freshly opened one. The session id is validated first, and the
/// returned string contains only allowlisted characters.
#[tauri::command]
pub async fn resume_session_command(
    project_id: String,
    session_id: String,
    state: State<'_, AppState>,
) -> Result<String, String> {
    validate_session_id(&session_id)?;

    let project = state
        .projects_store
        .get(&project_id)
        .ok_or_else(|| format!("Project {} not found", project_id))?;

    let mut parts = vec!["claude".to_string()];
    parts.extend(project.effective_permission_mode().cli_args());
    parts.push("--resume".to_string());
    parts.push(session_id);

    Ok(parts.join(" "))
}

// ─────────────────────────────────────────────────────────────────────────────
// 2. Capabilities
// ─────────────────────────────────────────────────────────────────────────────

/// Emits a single JSON object with one group per capability category.
///
/// User scope is `~/.claude`. Project scope is `/workspace/.claude` *and*
/// `/workspace/<mount>/.claude` — Triple-C mounts each project path at
/// `/workspace/<mount_name>`, so a repo's own `.claude` dir lives one level
/// down, not at the workspace root.
///
/// Frontmatter `name`/`description` are pulled with a small `awk` reader
/// (first `---` block only, first matching key, surrounding quotes stripped);
/// no YAML crate is involved. Files without frontmatter fall back to their
/// path-derived name.
const CAPABILITIES_SCRIPT: &str = r#"exec 2>/dev/null
set -u
USER_BASE="$HOME/.claude"

# Project-scoped config roots: the workspace root plus each mounted project dir.
proj_bases() {
  [ -d /workspace/.claude ] && echo /workspace/.claude
  for d in /workspace/*/; do
    [ -d "$d/.claude" ] && echo "${d}.claude"
  done
}

# fm <file> <key> — value of a YAML frontmatter key, or nothing.
fm() {
  [ -f "$1" ] || return 0
  head -1 "$1" | grep -q '^---[[:space:]]*$' || return 0
  awk -v key="$2" '
    NR == 1 { next }
    /^---[[:space:]]*$/ { exit }
    {
      pfx = key ":"
      if (index($0, pfx) == 1) {
        v = substr($0, length(pfx) + 1)
        sub(/^[ \t]+/, "", v); sub(/[ \t\r]+$/, "", v)
        if (v ~ /^".*"$/) v = substr(v, 2, length(v) - 2)
        else if (v ~ /^\047.*\047$/) v = substr(v, 2, length(v) - 2)
        print v
        exit
      }
    }' "$1"
}

emit_item() {
  jq -c -n --arg n "$1" --arg d "$2" --arg s "$3" \
    '{name: $n, description: (if $d == "" then null else $d end), scope: $s}'
}

collect_skills() {
  base="$1"; scope="$2"
  [ -d "$base/skills" ] || return 0
  for d in "$base"/skills/*/; do
    [ -f "$d/SKILL.md" ] || continue
    n=$(fm "$d/SKILL.md" name)
    [ -n "$n" ] || n=$(basename "$d")
    emit_item "$n" "$(fm "$d/SKILL.md" description)" "$scope"
  done
}

collect_md() {
  base="$1"; scope="$2"; sub="$3"
  [ -d "$base/$sub" ] || return 0
  find "$base/$sub" -name '*.md' -type f | sort | while read -r f; do
    rel=${f#"$base/$sub/"}; rel=${rel%.md}
    n=$(fm "$f" name)
    [ -n "$n" ] || n="$rel"
    emit_item "$n" "$(fm "$f" description)" "$scope"
  done
}

# One item per hook event; `count` carries the number of individual handlers so
# the caller can sum them into the group total.
collect_hooks() {
  base="$1"; scope="$2"
  for sf in "$base/settings.json" "$base/settings.local.json"; do
    [ -f "$sf" ] || continue
    jq -c --arg s "$scope" --arg f "$(basename "$sf")" '
      (.hooks // {}) | to_entries[] |
      ([.value[]? | (.hooks // []) | length] | add // 0) as $n |
      {name: .key,
       description: ($f + ": " + ($n | tostring) + " handler(s)"),
       scope: $s,
       count: $n}' "$sf"
  done
}

collect_plugins() {
  ip="$USER_BASE/plugins/installed_plugins.json"
  [ -f "$ip" ] && jq -c '(.plugins // {}) | to_entries[] |
      {name: .key,
       description: ((.value[0].version // "") | if . == "" then null else "v" + . end),
       scope: (.value[0].scope // "user")}' "$ip"
  for cf in "$USER_BASE/settings.json" "$HOME/.claude.json"; do
    [ -f "$cf" ] || continue
    jq -c '(.enabledPlugins // {}) | to_entries[] | select(.value == true) |
      {name: .key, description: "enabled", scope: "user"}' "$cf"
  done
}

collect_mcp() {
  if [ -f "$HOME/.claude.json" ]; then
    jq -c '(.mcpServers // {}) | to_entries[] |
      {name: .key, description: ((.value.command // .value.url // .value.type) // null),
       scope: "user"}' "$HOME/.claude.json"
    jq -c '(.projects // {}) | to_entries[] | (.value.mcpServers // {}) | to_entries[] |
      {name: .key, description: ((.value.command // .value.url // .value.type) // null),
       scope: "project"}' "$HOME/.claude.json"
  fi
  for mf in /workspace/.mcp.json /workspace/*/.mcp.json; do
    [ -f "$mf" ] || continue
    jq -c '(.mcpServers // {}) | to_entries[] |
      {name: .key, description: ((.value.command // .value.url // .value.type) // null),
       scope: "project"}' "$mf"
  done
}

group() { jq -s 'unique_by([.scope, .name]) | {count: length, items: .}'; }

all_skills()   { collect_skills "$USER_BASE" user; proj_bases | while read -r b; do collect_skills "$b" project; done; }
all_agents()   { collect_md "$USER_BASE" user agents; proj_bases | while read -r b; do collect_md "$b" project agents; done; }
all_commands() { collect_md "$USER_BASE" user commands; proj_bases | while read -r b; do collect_md "$b" project commands; done; }
all_hooks()    { collect_hooks "$USER_BASE" user; proj_bases | while read -r b; do collect_hooks "$b" project; done; }

jq -c -n \
  --argjson skills   "$(all_skills | group)" \
  --argjson agents   "$(all_agents | group)" \
  --argjson commands "$(all_commands | group)" \
  --argjson hooks    "$(all_hooks | jq -s '{count: ([.[].count] | add // 0), items: map(del(.count))}')" \
  --argjson plugins  "$(collect_plugins | group)" \
  --argjson mcp      "$(collect_mcp | group)" \
  '{skills: $skills, agents: $agents, commands: $commands,
    hooks: $hooks, plugins: $plugins, mcp_servers: $mcp}'
"#;

/// Inventory the Claude Code capabilities installed inside a project's
/// container. A stopped container yields all-zero groups, not an error.
#[tauri::command]
pub async fn list_container_capabilities(
    project_id: String,
    state: State<'_, AppState>,
) -> Result<ContainerCapabilities, String> {
    let container_id = match running_container_for(&project_id, &state).await? {
        Some(id) => id,
        None => return Ok(ContainerCapabilities::default()),
    };

    let raw = run_script(&container_id, CAPABILITIES_SCRIPT).await?;
    Ok(parse_or_default(&raw, "container capabilities"))
}

// ─────────────────────────────────────────────────────────────────────────────
// 3. Scheduler
// ─────────────────────────────────────────────────────────────────────────────

/// Emits the scheduler's tasks as JSON, mirroring its on-disk schema:
/// `{id, name, prompt, schedule, type, at, created_at, enabled, working_dir}`.
///
/// `last_run` is not in that schema, so it is derived from the mtime of the
/// newest file in `~/.claude/scheduler/logs/<id>/`.
const SCHEDULER_LIST_SCRIPT: &str = r#"exec 2>/dev/null
set -u
TASKS="$HOME/.claude/scheduler/tasks"
LOGS="$HOME/.claude/scheduler/logs"
[ -d "$TASKS" ] || { echo '[]'; exit 0; }
for f in "$TASKS"/*.json; do
  [ -f "$f" ] || continue
  id=$(jq -r '.id // ""' "$f") || continue
  [ -n "$id" ] || id=$(basename "$f" .json)
  last=$(find "$LOGS/$id" -name '*.log' -type f -printf '%T@\n' | sort -rn | head -1)
  jq -c --arg fallback_id "$id" --arg lr "${last%%.*}" '{
      id: (if (.id // "") == "" then $fallback_id else .id end),
      name: (.name // ""),
      prompt: (.prompt // ""),
      schedule: (.schedule // ""),
      task_type: (.type // "recurring"),
      at: (if (.at // "") == "" then null else .at end),
      enabled: (.enabled == true),
      working_dir: (.working_dir // "/workspace"),
      created_at: (.created_at // null),
      last_run_epoch: (if $lr == "" then null else ($lr | tonumber) end)
    }' "$f"
done | jq -s 'sort_by(.name, .id)'
"#;

/// Emits the newest notification files as structured JSON. The runner writes
/// them as a fixed plain-text block (`Task:`/`Status:`/`Time:`/`Type:` then a
/// `Summary:` body), which is parsed here; the verbatim text is kept too.
const SCHEDULER_NOTIFICATIONS_SCRIPT: &str = r#"exec 2>/dev/null
set -u
NDIR="$HOME/.claude/scheduler/notifications"
[ -d "$NDIR" ] || { echo '[]'; exit 0; }
TAB=$(printf '\t')
find "$NDIR" -maxdepth 1 -name '*.notify' -type f -printf '%T@\t%p\n' \
  | sort -rn | head -__MAX__ \
  | while IFS="$TAB" read -r mtime path; do
      [ -f "$path" ] || continue
      base=$(basename "$path" .notify)
      jq -c -n --arg tid "${base%%_*}" --arg mt "${mtime%%.*}" --rawfile body "$path" '{
        task_id: $tid,
        created_epoch: ($mt | tonumber),
        task_name: (($body | capture("Task:[ \t]+(?<v>.*)") | .v | sub("[ \t]+$"; "")) // null),
        status: (($body | capture("Status:[ \t]+(?<v>.*)") | .v | sub("[ \t]+$"; "")) // null),
        time: (($body | capture("Time:[ \t]+(?<v>.*)") | .v | sub("[ \t]+$"; "")) // null),
        task_type: (($body | capture("Type:[ \t]+(?<v>.*)") | .v | sub("[ \t]+$"; "")) // null),
        summary: (($body | capture("Summary:\n(?<v>[\\s\\S]*)") | .v) // null),
        body: $body
      }'
    done | jq -s '.'
"#;

#[derive(Debug, Deserialize)]
struct RawScheduledTask {
    id: String,
    name: String,
    prompt: String,
    schedule: String,
    task_type: String,
    at: Option<String>,
    enabled: bool,
    working_dir: String,
    created_at: Option<String>,
    last_run_epoch: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct RawNotification {
    task_id: String,
    created_epoch: i64,
    task_name: Option<String>,
    status: Option<String>,
    time: Option<String>,
    task_type: Option<String>,
    summary: Option<String>,
    body: String,
}

/// List the container's scheduled tasks. Stopped container → empty vec.
#[tauri::command]
pub async fn list_scheduled_tasks(
    project_id: String,
    state: State<'_, AppState>,
) -> Result<Vec<ScheduledTask>, String> {
    let container_id = match running_container_for(&project_id, &state).await? {
        Some(id) => id,
        None => return Ok(Vec::new()),
    };

    let raw = run_script(&container_id, SCHEDULER_LIST_SCRIPT).await?;
    let tasks: Vec<RawScheduledTask> = parse_or_default(&raw, "scheduled tasks");

    Ok(tasks
        .into_iter()
        .map(|t| {
            // A one-shot task's `at` time is its next (and only) run. Recurring
            // cron expressions are left uncomputed rather than guessed at.
            let next_run = if t.task_type == "once" && t.enabled {
                t.at.clone()
            } else {
                None
            };
            ScheduledTask {
                id: t.id,
                name: t.name,
                prompt: t.prompt,
                schedule: t.schedule,
                task_type: t.task_type,
                at: t.at,
                enabled: t.enabled,
                working_dir: t.working_dir,
                created_at: t.created_at,
                last_run: t.last_run_epoch.map(epoch_to_iso),
                next_run,
            }
        })
        .collect())
}

/// Tail the most recent log for one task, via the scheduler's own `logs`
/// subcommand. Stopped container → empty string.
#[tauri::command]
pub async fn get_scheduled_task_log(
    project_id: String,
    task_id: String,
    tail_lines: Option<u32>,
    state: State<'_, AppState>,
) -> Result<String, String> {
    validate_task_id(&task_id)?;
    // Clamped, and an integer by type — cannot carry shell syntax.
    let tail = tail_lines.unwrap_or(200).clamp(1, 5000);

    let container_id = match running_container_for(&project_id, &state).await? {
        Some(id) => id,
        None => return Ok(String::new()),
    };

    run_scheduler(
        &container_id,
        vec![
            "logs".to_string(),
            "--id".to_string(),
            task_id,
            "--tail".to_string(),
            tail.to_string(),
        ],
    )
    .await
}

/// Read the scheduler's pending completion notifications, newest first.
/// Stopped container → empty vec.
#[tauri::command]
pub async fn get_scheduler_notifications(
    project_id: String,
    state: State<'_, AppState>,
) -> Result<Vec<SchedulerNotification>, String> {
    let container_id = match running_container_for(&project_id, &state).await? {
        Some(id) => id,
        None => return Ok(Vec::new()),
    };

    let script =
        SCHEDULER_NOTIFICATIONS_SCRIPT.replace("__MAX__", &MAX_NOTIFICATIONS.to_string());

    let raw = run_script(&container_id, script).await?;
    let notifications: Vec<RawNotification> = parse_or_default(&raw, "scheduler notifications");

    Ok(notifications
        .into_iter()
        .map(|n| SchedulerNotification {
            task_id: n.task_id,
            task_name: n.task_name,
            status: n.status,
            time: n.time,
            task_type: n.task_type,
            summary: n.summary.map(|s| s.trim_end().to_string()).filter(|s| !s.is_empty()),
            body: n.body,
            created_at: epoch_to_iso(n.created_epoch),
        })
        .collect())
}

// ── Mutating scheduler commands ──────────────────────────────────────────────
//
// These delegate to `triple-c-scheduler`'s own subcommands (which also rebuild
// the crontab) instead of editing its JSON, and each runs as a bare argv vector
// with a validated id.

/// Enable or disable a task via the scheduler's `enable` / `disable`.
#[tauri::command]
pub async fn set_scheduled_task_enabled(
    project_id: String,
    task_id: String,
    enabled: bool,
    state: State<'_, AppState>,
) -> Result<String, String> {
    validate_task_id(&task_id)?;
    let container_id = require_running_container(&project_id, &state).await?;

    let subcommand = if enabled { "enable" } else { "disable" };
    let output = run_scheduler(
        &container_id,
        vec![subcommand.to_string(), "--id".to_string(), task_id],
    )
    .await?;
    Ok(output.trim().to_string())
}

/// Trigger a task immediately via the scheduler's `run`.
///
/// The run itself invokes Claude Code and can take minutes, so the exec is
/// started **detached**: Docker keeps it alive after this call returns and the
/// UI is not blocked. Progress shows up through `get_scheduled_task_log` /
/// `get_scheduler_notifications`, exactly as for a cron-triggered run.
#[tauri::command]
pub async fn run_scheduled_task_now(
    project_id: String,
    task_id: String,
    state: State<'_, AppState>,
) -> Result<String, String> {
    validate_task_id(&task_id)?;
    let container_id = require_running_container(&project_id, &state).await?;

    let docker = get_docker()?;
    let exec = docker
        .create_exec(
            &container_id,
            CreateExecOptions {
                attach_stdout: Some(false),
                attach_stderr: Some(false),
                // Argv vector — no shell, so `task_id` is inert as data.
                cmd: Some(vec![
                    "triple-c-scheduler".to_string(),
                    "run".to_string(),
                    "--id".to_string(),
                    task_id.clone(),
                ]),
                env: Some(home_env()),
                user: Some("claude".to_string()),
                working_dir: Some("/workspace".to_string()),
                ..Default::default()
            },
        )
        .await
        .map_err(|e| format!("Failed to create exec: {}", e))?;

    docker
        .start_exec(
            &exec.id,
            Some(StartExecOptions {
                detach: true,
                ..Default::default()
            }),
        )
        .await
        .map_err(|e| format!("Failed to start task: {}", e))?;

    log::info!(
        "Triggered scheduler task {} in project {} (detached exec {})",
        task_id,
        project_id,
        exec.id
    );
    Ok(format!("Task {} started.", task_id))
}

/// Remove a task via the scheduler's `remove` (which also rebuilds the crontab).
#[tauri::command]
pub async fn remove_scheduled_task(
    project_id: String,
    task_id: String,
    state: State<'_, AppState>,
) -> Result<String, String> {
    validate_task_id(&task_id)?;
    let container_id = require_running_container(&project_id, &state).await?;

    let output = run_scheduler(
        &container_id,
        vec!["remove".to_string(), "--id".to_string(), task_id],
    )
    .await?;
    Ok(output.trim().to_string())
}

/// Clear all pending notifications via the scheduler's `notifications --clear`.
#[tauri::command]
pub async fn clear_scheduler_notifications(
    project_id: String,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let container_id = require_running_container(&project_id, &state).await?;
    run_scheduler(
        &container_id,
        vec!["notifications".to_string(), "--clear".to_string()],
    )
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_id_allowlist_accepts_scheduler_generated_ids() {
        assert!(validate_task_id("a1b2c3d4").is_ok());
        assert!(validate_task_id("00000000").is_ok());
        assert!(validate_task_id("task_1-a").is_ok());
    }

    #[test]
    fn task_id_allowlist_rejects_injection_and_traversal() {
        for bad in [
            "",
            "a b",
            "a;rm -rf /",
            "a$(id)",
            "a`id`",
            "a|b",
            "a&b",
            "a>b",
            "a'b",
            "a\"b",
            "a\nb",
            "../../etc/passwd",
            "a/b",
            "a.json",
            "-id",
            "--id",
            &"a".repeat(65),
        ] {
            assert!(
                validate_task_id(bad).is_err(),
                "should have rejected {:?}",
                bad
            );
        }
    }

    #[test]
    fn session_id_allowlist_accepts_uuids_only() {
        assert!(validate_session_id("e13d312d-2f38-4cf6-b0e0-1db60208a74c").is_ok());
        assert!(validate_session_id("zzzz").is_err());
        assert!(validate_session_id("abc; rm -rf /").is_err());
        assert!(validate_session_id("-abc").is_err());
        assert!(validate_session_id("").is_err());
    }

    #[test]
    fn parse_or_default_degrades_on_garbage() {
        let v: Vec<RawSession> = parse_or_default("not json", "test");
        assert!(v.is_empty());
        let v: Vec<RawSession> = parse_or_default("   ", "test");
        assert!(v.is_empty());
        let caps: ContainerCapabilities = parse_or_default("{}", "test");
        assert_eq!(caps.skills.count, 0);
    }

    #[test]
    fn epoch_to_iso_is_rfc3339() {
        assert!(epoch_to_iso(0).starts_with("1970-01-01T00:00:00"));
    }
}
