use bollard::container::{
    Config, CreateContainerOptions, ListContainersOptions, RemoveContainerOptions,
    StartContainerOptions, StopContainerOptions,
};
use bollard::image::{CommitContainerOptions, RemoveImageOptions};
use bollard::models::{ContainerSummary, HostConfig, Mount, MountTypeEnum, PortBinding};
use std::collections::HashMap;
use sha2::{Sha256, Digest};

use super::ca_certs;
use super::client::get_docker;
use crate::models::{Backend, BedrockAuthMethod, ClaudeCodeSettings, ContainerInfo, EnvVar, GlobalAwsSettings, GlobalLlamaCppSettings, GlobalOllamaSettings, GlobalOpenAiCompatibleSettings, PortMapping, Project, ProjectPath};

const SCHEDULER_INSTRUCTIONS: &str = r#"## Scheduled Tasks

This container supports scheduled tasks via `triple-c-scheduler`. You can set up recurring or one-time tasks that run as separate Claude Code agents.

### Commands
- `triple-c-scheduler add --name "NAME" --schedule "CRON" --prompt "TASK"` — Add a recurring task
- `triple-c-scheduler add --name "NAME" --at "YYYY-MM-DD HH:MM" --prompt "TASK"` — Add a one-time task
- `triple-c-scheduler list` — List all scheduled tasks, with a running/idle status column
- `triple-c-scheduler remove --id ID` — Remove a task
- `triple-c-scheduler enable --id ID` / `triple-c-scheduler disable --id ID` — Toggle tasks
- `triple-c-scheduler status [--id ID] [--watch]` — Show what is running right now, and for how long
- `triple-c-scheduler logs [--id ID] [--tail N]` — View execution logs
- `triple-c-scheduler run --id ID` — Manually trigger a task immediately (streams its log)
- `triple-c-scheduler notifications [--clear]` — View or clear completion notifications

### Cron format
Standard 5-field cron: `minute hour day-of-month month day-of-week`
Examples: `*/30 * * * *` (every 30 min), `0 9 * * 1-5` (9am weekdays), `0 */2 * * *` (every 2 hours)

### One-time tasks
Use `--at "YYYY-MM-DD HH:MM"` instead of `--schedule`. The task automatically removes itself after execution.

### Working directory
Use `--working-dir /workspace/project` to set where the task runs (default: /workspace).

### Checking results
While a task is running, `triple-c-scheduler status` reports it with elapsed time — a log that has stopped growing is normal, because `claude -p` writes its answer only at the end, so use `status` rather than log silence to tell a slow run from a dead one. After tasks run, check notifications with `triple-c-scheduler notifications` and detailed output with `triple-c-scheduler logs`.

### Timezone
Scheduled times use the container's configured timezone (check with `date`). If no timezone is configured, UTC is used."#;

const MISSION_CONTROL_GLOBAL_INSTRUCTIONS: &str = r#"## Mission Control

The `/workspace/mission-control/` directory contains **Flight Control** — an AI-first development methodology for structured project management. Use it for all project work.

### How It Works

- **Mission Control is a tool, not a project.** It provides skills and methodology for managing other projects.
- All Flight Control skills are installed as personal skills in `~/.claude/skills/` and are automatically available as `/slash-commands`
- The methodology docs and project registry live in `/workspace/mission-control/`

### When to Use

When working on any project that has a `.flightops/` directory, follow the Flight Control methodology:
1. Read the project's `.flightops/ARTIFACTS.md` to understand artifact storage
2. Read `.flightops/FLIGHT_OPERATIONS.md` for the implementation workflow
3. Use Mission Control skills for planning and execution

### Available Skills

| Skill | When to Use |
|-------|-------------|
| `/init-project` | Setting up a new project for Flight Control |
| `/mission` | Defining new work outcomes (days-to-weeks scope) |
| `/flight` | Creating technical specs from missions (hours-to-days scope) |
| `/leg` | Generating implementation steps from flights (minutes-to-hours scope) |
| `/agentic-workflow` | Executing legs with multi-agent workflow (implement, review, commit) |
| `/flight-debrief` | Post-flight analysis after a flight lands |
| `/mission-debrief` | Post-mission retrospective after completion |
| `/daily-briefing` | Cross-project status report |

### Key Rules

- **Planning skills produce artifacts only** — never modify source code directly
- **Phase gates require human confirmation** — missions before flights, flights before legs
- **Legs are immutable once in-flight** — create new ones instead of modifying
- **`/agentic-workflow` orchestrates implementation** — it spawns separate Developer and Reviewer agents
- **Artifacts live in the target project** — not in mission-control"#;

const MISSION_CONTROL_PROJECT_INSTRUCTIONS: &str = r#"## Flight Operations

This project uses **Flight Control** (bundled with Triple-C) for structured development.

**Before any mission/flight/leg work, read these files in order:**
1. `.flightops/README.md` — What the flightops directory contains
2. `.flightops/FLIGHT_OPERATIONS.md` — **The workflow you MUST follow**
3. `.flightops/ARTIFACTS.md` — Where all artifacts are stored
4. `.flightops/agent-crews/` — Project crew definitions for each phase (read the relevant crew file)"#;

const SANDBOX_INSTRUCTIONS: &str = r#"## Sandbox Mode

This container has Claude Code's bash sandbox enabled, managed by Triple-C
(toggle it from the project's "Sandbox mode" switch in the Triple-C UI).
Bash commands run inside `bubblewrap` with filesystem and network isolation
(`enableWeakerNestedSandbox` is on because we are inside Docker).

### When a command fails because of sandbox restrictions

Triple-C disables the `dangerouslyDisableSandbox` escape hatch
(`allowUnsandboxedCommands: false`), so failing commands cannot bypass the
sandbox at runtime. To make a blocked command work, edit
`~/.claude/settings.json` and restart Claude Code:

| Need | Setting |
|---|---|
| Write to a path outside the project (e.g. `~/.kube`) | Add to `sandbox.filesystem.allowWrite` |
| Reach a new domain | Will prompt; or add permanently to `sandbox.allowedDomains` |
| Run a specific tool entirely outside the sandbox | Add a glob (e.g. `"docker *"`) to `sandbox.excludedCommands` |

### Docker commands

The `docker` CLI does not work inside the sandbox. If this project has
"Allow container spawning" enabled in Triple-C and you need to run
`docker` commands, add `"docker *"` to `sandbox.excludedCommands` in
`~/.claude/settings.json`. Other tools known to be sandbox-incompatible
include `watchman` — pass `--no-watchman` to `jest`.

### Disabling sandbox mode

Do not change `sandbox.enabled` in `settings.json` — Triple-C overwrites it
on every container start. To turn sandbox off, stop the container in
Triple-C, flip the "Sandbox mode" switch off, then start the container."#;

/// Build the full CLAUDE_INSTRUCTIONS value by merging global + project
/// instructions, appending port mapping docs, and appending scheduler docs.
/// Used by both create_container() and container_needs_recreation() to ensure
/// the same value is produced in both paths.
fn build_claude_instructions(
    global_instructions: Option<&str>,
    project_instructions: Option<&str>,
    port_mappings: &[PortMapping],
    mission_control_enabled: bool,
    sandbox_enabled: bool,
) -> Option<String> {
    let mut combined = merge_claude_instructions(
        global_instructions,
        project_instructions,
        mission_control_enabled,
    );

    if !port_mappings.is_empty() {
        let mut port_lines: Vec<String> = Vec::new();
        port_lines.push("## Available Port Mappings".to_string());
        port_lines.push("The following ports are mapped from the host to this container. Use these container ports when starting services that need to be accessible from the host:".to_string());
        for pm in port_mappings {
            port_lines.push(format!(
                "- Host port {} -> Container port {} ({})",
                pm.host_port, pm.container_port, pm.protocol
            ));
        }
        let port_info = port_lines.join("\n");
        combined = Some(match combined {
            Some(existing) => format!("{}\n\n{}", existing, port_info),
            None => port_info,
        });
    }

    combined = Some(match combined {
        Some(existing) => format!("{}\n\n{}", existing, SCHEDULER_INSTRUCTIONS),
        None => SCHEDULER_INSTRUCTIONS.to_string(),
    });

    if sandbox_enabled {
        combined = Some(match combined {
            Some(existing) => format!("{}\n\n{}", existing, SANDBOX_INSTRUCTIONS),
            None => SANDBOX_INSTRUCTIONS.to_string(),
        });
    }

    combined
}

/// The env var Claude Code reads a long-lived `claude setup-token` credential
/// from. Named once so injection, the reserved-name blocklist, and the
/// stale-value neutralization pass can never disagree about the spelling.
pub const CLAUDE_OAUTH_TOKEN_ENV: &str = "CLAUDE_CODE_OAUTH_TOKEN";

/// Every managed env var whose *value* is a credential.
///
/// These are the names that must never survive into a snapshot image. A
/// container's env is visible to `docker inspect`, which is bad but bounded —
/// the container is recreated whenever the credential rotates, and removed
/// with the project. An **image**'s env is neither: `docker commit` copies the
/// container's full environment into `triple-c-snapshot-{id}:latest`, that tag
/// outlives every container built from it, and nothing about deleting a
/// keychain entry touches it. A ~1-year OAuth token baked in that way is
/// readable by `docker image inspect` for as long as the image exists, long
/// after the user has clicked Revoke.
///
/// [`commit_container_snapshot`] therefore blanks all of them at commit time,
/// and [`scrub_secrets_from_snapshots`] rewrites images committed before that
/// was true.
///
/// Blanked rather than omitted, because Docker's commit endpoint *merges* the
/// supplied config over the container's rather than replacing it: a key left
/// out of the list is inherited with its original value, so `KEY=` is the only
/// way to clear one. That matches how `MANAGED_AUTH_KEYS` already works at
/// create time, and Claude Code, the AWS SDK and git all treat an empty value
/// as unset.
pub const SECRET_ENV_KEYS: &[&str] = &[
    CLAUDE_OAUTH_TOKEN_ENV,
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_AUTH_TOKEN",
    "AWS_ACCESS_KEY_ID",
    "AWS_SECRET_ACCESS_KEY",
    "AWS_SESSION_TOKEN",
    "AWS_BEARER_TOKEN_BEDROCK",
    "GIT_TOKEN",
];

/// Env var name prefixes Triple-C manages itself; users cannot set these by hand.
const RESERVED_ENV_PREFIXES: &[&str] = &["ANTHROPIC_", "AWS_", "GIT_", "HOST_", "TRIPLE_C_"];

/// Exact env var names Triple-C manages itself. Not covered by
/// [`RESERVED_ENV_PREFIXES`] because they don't share those prefixes.
///
/// `MCP_SERVERS_JSON` is reserved for legacy reasons: the built-in MCP feature
/// was removed, but the name stays blocked so users cannot hand-set it.
/// `CLAUDE_CODE_OAUTH_TOKEN` is reserved because Triple-C owns it — a hand-set
/// value would silently outrank the keychain-held shared token and be invisible
/// to the auth UI.
const RESERVED_ENV_EXACT: &[&str] = &[
    "CLAUDE_INSTRUCTIONS",
    "MCP_SERVERS_JSON",
    "CLAUDE_CODE_SETTINGS_JSON",
    "MISSION_CONTROL_ENABLED",
    "TRIPLE_C_PERMISSION_MODE",
    CLAUDE_OAUTH_TOKEN_ENV,
    // The model-alias vars are already covered by the `ANTHROPIC_` prefix
    // above; they are listed explicitly so that a future narrowing of the
    // prefix list cannot silently unreserve them, and so `is_reserved_env_key`
    // reads as the single, complete statement of what Triple-C owns.
    ANTHROPIC_DEFAULT_OPUS_MODEL,
    ANTHROPIC_DEFAULT_SONNET_MODEL,
    ANTHROPIC_DEFAULT_HAIKU_MODEL,
    ANTHROPIC_DEFAULT_FABLE_MODEL,
];

/// Claude Code's model-alias env vars. Each names the concrete model id that
/// one of the `opus` / `sonnet` / `haiku` / `fable` aliases resolves to.
///
/// `ANTHROPIC_DEFAULT_HAIKU_MODEL` is the important one: it is documented as
/// *"Model ID that the `haiku` alias resolves to, also used for background
/// functionality"* — conversation titles, summarisation, and other out-of-band
/// calls. Left unset against a local server, Claude Code sends
/// Anthropic's own Haiku model id to a server that has never heard of it and
/// every background call fails, usually silently.
///
/// (`ANTHROPIC_SMALL_FAST_MODEL` is the deprecated predecessor of the Haiku
/// var and is deliberately *not* used.)
pub const ANTHROPIC_DEFAULT_OPUS_MODEL: &str = "ANTHROPIC_DEFAULT_OPUS_MODEL";
pub const ANTHROPIC_DEFAULT_SONNET_MODEL: &str = "ANTHROPIC_DEFAULT_SONNET_MODEL";
pub const ANTHROPIC_DEFAULT_HAIKU_MODEL: &str = "ANTHROPIC_DEFAULT_HAIKU_MODEL";
pub const ANTHROPIC_DEFAULT_FABLE_MODEL: &str = "ANTHROPIC_DEFAULT_FABLE_MODEL";

/// Resolve the four `ANTHROPIC_DEFAULT_*_MODEL` values for a backend that
/// points Claude Code at a custom endpoint.
///
/// All four aliases fall back to `effective_model` — the backend's configured
/// model id, already resolved per-project → global. That is the right default:
/// a local server almost always serves exactly one model, so every alias must
/// name it or the calls that use an alias (notably the background ones, which
/// use `haiku`) go to a model the server does not have.
///
/// `haiku_override` exists because that is the one alias someone might
/// legitimately want to point elsewhere — at a second, smaller server-side
/// model kept for cheap background work. A blank override falls back to
/// `effective_model` like the others.
///
/// Returns pairs in `(name, value)` form; a blank resolved value emits nothing
/// at all rather than an empty var, so an unconfigured backend is left exactly
/// as Claude Code found it.
pub fn compute_model_aliases(
    effective_model: Option<&str>,
    haiku_override: Option<&str>,
) -> Vec<(&'static str, String)> {
    let base = effective_model.map(str::trim).filter(|s| !s.is_empty());
    let haiku = haiku_override
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .or(base);

    let mut out: Vec<(&'static str, String)> = Vec::new();
    if let Some(m) = base {
        out.push((ANTHROPIC_DEFAULT_OPUS_MODEL, m.to_string()));
        out.push((ANTHROPIC_DEFAULT_SONNET_MODEL, m.to_string()));
    }
    if let Some(h) = haiku {
        out.push((ANTHROPIC_DEFAULT_HAIKU_MODEL, h.to_string()));
    }
    if let Some(m) = base {
        out.push((ANTHROPIC_DEFAULT_FABLE_MODEL, m.to_string()));
    }
    out
}

/// The fingerprint contribution of the model aliases, so that changing an
/// alias (or the model it falls back to) forces a container recreation.
/// `container_needs_recreation` is label-based and never diffs env, so an
/// env-only change is invisible without this.
fn model_alias_fingerprint_part(
    effective_model: Option<&str>,
    haiku_override: Option<&str>,
) -> String {
    compute_model_aliases(effective_model, haiku_override)
        .into_iter()
        .map(|(k, v)| format!("{}={}", k, v))
        .collect::<Vec<_>>()
        .join(",")
}

/// Whether `key` is an env var name Triple-C reserves for itself.
fn is_reserved_env_key(key: &str) -> bool {
    let upper = key.to_uppercase();
    RESERVED_ENV_PREFIXES.iter().any(|p| upper.starts_with(p))
        || RESERVED_ENV_EXACT.iter().any(|e| upper == *e)
}

/// Compute a fingerprint for the custom environment variables.
///
/// Sorted alphabetically so order changes do not cause spurious recreation, and
/// **hashed**, because this value is written as the
/// `triple-c.custom-env-fingerprint` label. Labels are readable by anything on
/// the host through `docker inspect`, `docker commit` copies them onto the
/// project's snapshot image, and `container_needs_recreation` logs both sides on
/// a mismatch — so a plaintext `KEY=VALUE` join published every custom
/// variable's *value*, API tokens included, to all three places. Same treatment
/// as `triple-c.git-token-hash`.
///
/// Empty stays empty rather than becoming the hash of the empty string: an empty
/// label is how every other `triple-c.*` key says "nothing configured".
fn compute_env_fingerprint(custom_env_vars: &[EnvVar]) -> String {
    let mut parts: Vec<String> = Vec::new();
    for env_var in custom_env_vars {
        let key = env_var.key.trim();
        if key.is_empty() || is_reserved_env_key(key) {
            continue;
        }
        parts.push(format!("{}={}", key, env_var.value));
    }
    parts.sort();
    if parts.is_empty() {
        return String::new();
    }
    sha256_hex(&parts.join(","))
}

/// The shared Claude Code OAuth token to inject for this project, paired with
/// its rotation id.
///
/// `None` unless *all* of: the backend is Anthropic (the token is meaningless
/// to Bedrock/Ollama/OpenAI-compatible), the project has not opted out, and a
/// non-blank token is actually in the keychain. Read here rather than passed in
/// because the token is global, not part of the per-project record.
///
/// The returned token is never logged and never leaves this module except as
/// the env var value handed to Docker.
fn shared_claude_auth(project: &Project) -> Option<(String, String)> {
    if project.backend != Backend::Anthropic || !project.use_shared_auth_token {
        return None;
    }
    let token = crate::storage::secure::get_claude_oauth_token()
        .unwrap_or_else(|e| {
            log::warn!("Could not read the shared Claude token from the keychain: {}", e);
            None
        })
        .filter(|t| !t.trim().is_empty())?;
    // A token with no rotation id predates versioning (or the id write failed).
    // A constant stand-in still differs from the empty "no token" label, so
    // presence changes are caught; only rotations could be missed.
    let version = crate::storage::secure::get_claude_oauth_token_version()
        .unwrap_or(None)
        .unwrap_or_else(|| "unversioned".to_string());
    Some((token, version))
}

/// Label value tracking which shared Claude token (if any) a container was
/// created with. Empty means "none injected". See
/// [`crate::storage::secure`] for why this is a random rotation id rather than
/// a hash of the token.
fn claude_token_label(project: &Project) -> String {
    shared_claude_auth(project)
        .map(|(_, version)| version)
        .unwrap_or_default()
}

/// Merge global and per-project custom environment variables.
/// Per-project variables override global variables with the same key.
fn merge_custom_env_vars(global: &[EnvVar], project: &[EnvVar]) -> Vec<EnvVar> {
    let mut merged: std::collections::HashMap<String, EnvVar> = std::collections::HashMap::new();
    for ev in global {
        let key = ev.key.trim().to_string();
        if !key.is_empty() {
            merged.insert(key, ev.clone());
        }
    }
    for ev in project {
        let key = ev.key.trim().to_string();
        if !key.is_empty() {
            merged.insert(key, ev.clone());
        }
    }
    merged.into_values().collect()
}

/// Merge global and per-project Claude instructions into a single string.
/// When mission_control_enabled is true, appends Mission Control global
/// instructions after global and project instructions after project.
fn merge_claude_instructions(
    global_instructions: Option<&str>,
    project_instructions: Option<&str>,
    mission_control_enabled: bool,
) -> Option<String> {
    // Build the global portion (user global + optional MC global)
    let global_part = if mission_control_enabled {
        match global_instructions {
            Some(g) => Some(format!("{}\n\n{}", g, MISSION_CONTROL_GLOBAL_INSTRUCTIONS)),
            None => Some(MISSION_CONTROL_GLOBAL_INSTRUCTIONS.to_string()),
        }
    } else {
        global_instructions.map(|g| g.to_string())
    };

    // Build the project portion (user project + optional MC project)
    let project_part = if mission_control_enabled {
        match project_instructions {
            Some(p) => Some(format!("{}\n\n{}", p, MISSION_CONTROL_PROJECT_INSTRUCTIONS)),
            None => Some(MISSION_CONTROL_PROJECT_INSTRUCTIONS.to_string()),
        }
    } else {
        project_instructions.map(|p| p.to_string())
    };

    match (global_part, project_part) {
        (Some(g), Some(p)) => Some(format!("{}\n\n{}", g, p)),
        (Some(g), None) => Some(g),
        (None, Some(p)) => Some(p),
        (None, None) => None,
    }
}

/// Hash a string with SHA-256 and return the hex digest.
fn sha256_hex(input: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(input.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// Resolve a per-project string value with a global fallback. Returns `None`
/// when both are blank, otherwise the per-project value if set, else the global.
fn resolve_with_global<'a>(per_project: Option<&'a str>, global: Option<&'a str>) -> Option<&'a str> {
    let project_val = per_project.map(str::trim).filter(|s| !s.is_empty());
    if project_val.is_some() {
        return project_val;
    }
    global.map(str::trim).filter(|s| !s.is_empty())
}

/// Compute a fingerprint for the Bedrock configuration so we can detect changes.
/// Includes the resolved model_id (per-project blank → global default) so that
/// changing the global default forces a container recreation.
fn compute_bedrock_fingerprint(project: &Project, global_aws: &GlobalAwsSettings) -> String {
    if let Some(ref bedrock) = project.bedrock_config {
        let effective_model = resolve_with_global(
            bedrock.model_id.as_deref(),
            global_aws.default_model_id.as_deref(),
        ).unwrap_or("").to_string();
        // NOTE: the static credential fields (access key / secret / session
        // token) are intentionally NOT part of the fingerprint. They are
        // written to ~/.aws/credentials on every start by
        // sync_bedrock_credentials(), so a key rotation should refresh
        // in place rather than force a full container recreation. Region,
        // profile, and bearer token remain env-based and so stay here.
        let parts = vec![
            format!("{:?}", bedrock.auth_method),
            bedrock.aws_region.clone(),
            bedrock.aws_profile.as_deref().unwrap_or("").to_string(),
            bedrock.aws_bearer_token.as_deref().unwrap_or("").to_string(),
            effective_model,
            format!("{}", bedrock.disable_prompt_caching),
            bedrock.service_tier.as_deref().unwrap_or("").to_string(),
        ];
        sha256_hex(&parts.join("|"))
    } else {
        String::new()
    }
}

/// Compute a fingerprint for the Ollama configuration so we can detect changes.
/// Includes the resolved base_url and model_id (per-project blank → global
/// default) and the resolved model aliases.
///
/// NOTE: adding the alias part changes this hash for every existing Ollama
/// container, so each will be recreated once on the next start. That is exactly
/// what is wanted — recreation is the only way to get the new
/// `ANTHROPIC_DEFAULT_*_MODEL` vars into the container's env.
fn compute_ollama_fingerprint(project: &Project, global_ollama: &GlobalOllamaSettings) -> String {
    if let Some(ref ollama) = project.ollama_config {
        let effective_url = resolve_with_global(
            Some(&ollama.base_url),
            global_ollama.base_url.as_deref(),
        ).unwrap_or("").to_string();
        let effective_model = resolve_with_global(
            ollama.model_id.as_deref(),
            global_ollama.default_model_id.as_deref(),
        ).unwrap_or("").to_string();
        let aliases = model_alias_fingerprint_part(
            Some(&effective_model),
            resolve_with_global(
                ollama.haiku_model_id.as_deref(),
                global_ollama.default_haiku_model_id.as_deref(),
            ),
        );
        let parts = vec![effective_url, effective_model, aliases];
        sha256_hex(&parts.join("|"))
    } else {
        String::new()
    }
}

/// Compute a fingerprint for the llama.cpp configuration so we can detect
/// changes. Mirrors [`compute_ollama_fingerprint`].
fn compute_llamacpp_fingerprint(
    project: &Project,
    global_llamacpp: &GlobalLlamaCppSettings,
) -> String {
    if let Some(ref cfg) = project.llamacpp_config {
        let effective_url = resolve_with_global(
            Some(&cfg.base_url),
            global_llamacpp.base_url.as_deref(),
        ).unwrap_or("").to_string();
        let effective_model = resolve_with_global(
            cfg.model_id.as_deref(),
            global_llamacpp.default_model_id.as_deref(),
        ).unwrap_or("").to_string();
        let aliases = model_alias_fingerprint_part(
            Some(&effective_model),
            resolve_with_global(
                cfg.haiku_model_id.as_deref(),
                global_llamacpp.default_haiku_model_id.as_deref(),
            ),
        );
        let parts = vec![effective_url, effective_model, aliases];
        sha256_hex(&parts.join("|"))
    } else {
        String::new()
    }
}

/// Compute a fingerprint for the OpenAI Compatible configuration so we can detect changes.
/// Includes the resolved base_url and model_id (per-project blank → global default).
fn compute_openai_compatible_fingerprint(
    project: &Project,
    global_openai_compatible: &GlobalOpenAiCompatibleSettings,
) -> String {
    if let Some(ref config) = project.openai_compatible_config {
        let effective_url = resolve_with_global(
            Some(&config.base_url),
            global_openai_compatible.base_url.as_deref(),
        ).unwrap_or("").to_string();
        let effective_model = resolve_with_global(
            config.model_id.as_deref(),
            global_openai_compatible.default_model_id.as_deref(),
        ).unwrap_or("").to_string();
        let aliases = model_alias_fingerprint_part(
            Some(&effective_model),
            resolve_with_global(
                config.haiku_model_id.as_deref(),
                global_openai_compatible.default_haiku_model_id.as_deref(),
            ),
        );
        let parts = vec![
            effective_url,
            config.api_key.as_deref().unwrap_or("").to_string(),
            effective_model,
            aliases,
        ];
        sha256_hex(&parts.join("|"))
    } else {
        String::new()
    }
}

/// Compute a fingerprint for the project paths so we can detect changes.
/// Sorted by mount_name so order changes don't cause spurious recreation.
fn compute_paths_fingerprint(paths: &[ProjectPath]) -> String {
    let mut parts: Vec<String> = paths
        .iter()
        .map(|p| format!("{}:{}", p.mount_name, p.host_path))
        .collect();
    parts.sort();
    let joined = parts.join(",");
    sha256_hex(&joined)
}

/// Compute a fingerprint for port mappings so we can detect changes.
/// Sorted so order changes don't cause spurious recreation.
fn compute_ports_fingerprint(port_mappings: &[PortMapping]) -> String {
    let mut parts: Vec<String> = port_mappings
        .iter()
        .map(|p| format!("{}:{}:{}", p.host_port, p.container_port, p.protocol))
        .collect();
    parts.sort();
    let joined = parts.join(",");
    sha256_hex(&joined)
}

/// Merge global and per-project ClaudeCodeSettings.
/// Per-project fields override global fields when set (non-default).
fn merge_claude_code_settings(
    global: Option<&ClaudeCodeSettings>,
    project: Option<&ClaudeCodeSettings>,
) -> Option<ClaudeCodeSettings> {
    match (global, project) {
        (None, None) => None,
        (Some(g), None) => Some(g.clone()),
        (None, Some(p)) => Some(p.clone()),
        (Some(g), Some(p)) => {
            // Project overrides global for each field when the project value is non-default
            Some(ClaudeCodeSettings {
                tui_mode: p.tui_mode.clone().or_else(|| g.tui_mode.clone()),
                effort: p.effort.clone().or_else(|| g.effort.clone()),
                auto_scroll_disabled: if p.auto_scroll_disabled { true } else { g.auto_scroll_disabled },
                focus_mode: if p.focus_mode { true } else { g.focus_mode },
                show_thinking_summaries: if p.show_thinking_summaries { true } else { g.show_thinking_summaries },
                enable_session_recap: if p.enable_session_recap { true } else { g.enable_session_recap },
                env_scrub: if p.env_scrub { true } else { g.env_scrub },
                prompt_caching_1h: if p.prompt_caching_1h { true } else { g.prompt_caching_1h },
            })
        }
    }
}

/// Compute a fingerprint for the Claude Code settings so we can detect changes.
/// The `sandbox_enabled` flag is included so that toggling sandbox mode forces
/// a container recreation (re-injecting the merged settings.json). When
/// sandbox is off the historical fingerprint is preserved unchanged so that
/// upgrading triple-c does not spuriously flag every existing container for
/// recreation.
fn compute_claude_code_settings_fingerprint(
    settings: Option<&ClaudeCodeSettings>,
    sandbox_enabled: bool,
) -> String {
    let base_fp = match settings {
        None => String::new(),
        Some(s) => {
            let parts = vec![
                s.tui_mode.as_deref().unwrap_or("").to_string(),
                s.effort.as_deref().unwrap_or("").to_string(),
                format!("{}", s.auto_scroll_disabled),
                format!("{}", s.focus_mode),
                format!("{}", s.show_thinking_summaries),
                format!("{}", s.enable_session_recap),
                format!("{}", s.env_scrub),
                format!("{}", s.prompt_caching_1h),
            ];
            sha256_hex(&parts.join("|"))
        }
    };
    if sandbox_enabled {
        sha256_hex(&format!("{}|sandbox=true", base_fp))
    } else {
        base_fp
    }
}

/// Build the settings.json content for Claude Code.
/// Returns a JSON string of the settings to be written to ~/.claude/settings.json.
/// Always emits a `sandbox.enabled` key reflecting the current per-project
/// toggle so that flipping it off in triple-c overrides any prior on-state
/// stored in the persisted settings.json (which lives in a named volume).
fn build_claude_code_settings_json(
    settings: Option<&ClaudeCodeSettings>,
    sandbox_enabled: bool,
) -> Option<String> {
    let mut map = serde_json::Map::new();

    if let Some(s) = settings {
        if let Some(ref tui) = s.tui_mode {
            map.insert("tui".to_string(), serde_json::json!(tui));
        }
        if let Some(ref effort) = s.effort {
            map.insert("effort".to_string(), serde_json::json!(effort));
        }
        if s.auto_scroll_disabled {
            map.insert("autoScrollEnabled".to_string(), serde_json::json!(false));
        }
        if s.focus_mode {
            map.insert("focusMode".to_string(), serde_json::json!(true));
        }
        if s.show_thinking_summaries {
            map.insert("showThinkingSummaries".to_string(), serde_json::json!(true));
        }
    }

    // Always emit `sandbox.enabled` so that toggling the per-project sandbox
    // off in triple-c clears any prior on-state in the persisted
    // settings.json (which lives in a named volume that survives recreation).
    // Inside a Docker container we can't rely on privileged user namespaces,
    // so `enableWeakerNestedSandbox` is required when sandbox is on.
    let sandbox_obj = if sandbox_enabled {
        serde_json::json!({
            "enabled": true,
            "enableWeakerNestedSandbox": true,
            "allowUnsandboxedCommands": false,
        })
    } else {
        serde_json::json!({ "enabled": false })
    };
    map.insert("sandbox".to_string(), sandbox_obj);

    if map.is_empty() {
        None
    } else {
        Some(serde_json::Value::Object(map).to_string())
    }
}

pub async fn find_existing_container(project: &Project) -> Result<Option<String>, String> {
    let docker = get_docker()?;
    let container_name = project.container_name();

    let filters: HashMap<String, Vec<String>> = HashMap::from([
        ("name".to_string(), vec![container_name.clone()]),
    ]);

    let containers: Vec<ContainerSummary> = docker
        .list_containers(Some(ListContainersOptions {
            all: true,
            filters,
            ..Default::default()
        }))
        .await
        .map_err(|e| format!("Failed to list containers: {}", e))?;

    // Match exact name (Docker prepends /)
    let expected = format!("/{}", container_name);
    for c in &containers {
        if let Some(names) = &c.names {
            if names.iter().any(|n| n == &expected) {
                return Ok(c.id.clone());
            }
        }
    }

    Ok(None)
}

/// Extra creation inputs that only base-image migration cares about, kept in
/// one struct so `create_container`'s already-long parameter list does not grow
/// two more positional arguments that every ordinary call site would have to
/// pass as `None`-ish placeholders.
#[derive(Debug, Clone, Copy, Default)]
pub struct CreateExtras<'a> {
    /// Extra labels merged in last, overriding anything computed here.
    /// Migration uses this to stamp `triple-c.migration-state=in-progress`.
    pub extra_labels: &'a [(&'a str, &'a str)],
}

/// Resolve the value for the `triple-c.base-image-id` label.
///
/// This is the **image ID**, not a `RepoDigests` entry: a locally built image
/// (`triple-c:latest`) and any custom image have no repo digest at all, so a
/// digest-based lineage would be blank for exactly the users most likely to
/// change their base.
///
/// Two cases:
/// * creating **from the base** — the base's own current `.Id`;
/// * creating **from the project's snapshot** — carry forward whatever lineage
///   the snapshot image already records, because a snapshot is a commit of a
///   container that itself descended from some base. Committing propagates
///   container labels onto the image (verified), which is what makes the
///   carry-forward chain hold across every recreation.
///
/// An empty string means "unknown" — a snapshot that predates this label. It is
/// deliberately *not* the same as "stale"; see [`crate::models::ContainerStaleness::known`].
async fn resolve_base_image_id(image_name: &str, base_image_name: &str) -> String {
    if image_name == base_image_name {
        return super::migration::image_id(base_image_name)
            .await
            .ok()
            .flatten()
            .unwrap_or_default();
    }
    super::migration::image_labels(image_name)
        .await
        .get(super::migration::LABEL_BASE_IMAGE_ID)
        .cloned()
        .unwrap_or_default()
}

pub async fn create_container(
    project: &Project,
    docker_socket_path: &str,
    image_name: &str,
    base_image_name: &str,
    extras: CreateExtras<'_>,
    aws_config_path: Option<&str>,
    global_aws: &GlobalAwsSettings,
    global_ollama: &GlobalOllamaSettings,
    global_llamacpp: &GlobalLlamaCppSettings,
    global_openai_compatible: &GlobalOpenAiCompatibleSettings,
    global_claude_instructions: Option<&str>,
    global_custom_env_vars: &[EnvVar],
    timezone: Option<&str>,
    global_claude_code_settings: Option<&ClaudeCodeSettings>,
    default_ssh_key_path: Option<&str>,
    default_ca_cert_path: Option<&str>,
    default_git_user_name: Option<&str>,
    default_git_user_email: Option<&str>,
) -> Result<String, String> {
    let docker = get_docker()?;
    let container_name = project.container_name();

    let mut env_vars: Vec<String> = Vec::new();

    // Tell CLI tools the terminal supports 24-bit RGB color
    env_vars.push("COLORTERM=truecolor".to_string());

    // Pass host UID/GID so the entrypoint can remap the container user
    #[cfg(unix)]
    {
        let uid = std::process::Command::new("id").arg("-u").output();
        let gid = std::process::Command::new("id").arg("-g").output();
        if let Ok(out) = uid {
            if out.status.success() {
                let val = String::from_utf8_lossy(&out.stdout).trim().to_string();
                if !val.is_empty() {
                    log::debug!("Host UID detected: {}", val);
                    env_vars.push(format!("HOST_UID={}", val));
                }
            } else {
                log::debug!("Failed to detect host UID (exit code {:?})", out.status.code());
            }
        }
        if let Ok(out) = gid {
            if out.status.success() {
                let val = String::from_utf8_lossy(&out.stdout).trim().to_string();
                if !val.is_empty() {
                    log::debug!("Host GID detected: {}", val);
                    env_vars.push(format!("HOST_GID={}", val));
                }
            } else {
                log::debug!("Failed to detect host GID (exit code {:?})", out.status.code());
            }
        }
    }
    #[cfg(windows)]
    {
        log::debug!("Skipping HOST_UID/HOST_GID on Windows — Docker Desktop's Linux VM handles user mapping");
    }

    if let Some(ref token) = project.git_token {
        env_vars.push(format!("GIT_TOKEN={}", token));
    }
    // Per-project git user overrides global defaults
    let effective_git_name = project.git_user_name.as_deref().or(default_git_user_name);
    let effective_git_email = project.git_user_email.as_deref().or(default_git_user_email);
    if let Some(name) = effective_git_name {
        env_vars.push(format!("GIT_USER_NAME={}", name));
    }
    if let Some(email) = effective_git_email {
        env_vars.push(format!("GIT_USER_EMAIL={}", email));
    }

    // Bedrock configuration
    if project.backend == Backend::Bedrock {
        if let Some(ref bedrock) = project.bedrock_config {
            env_vars.push("CLAUDE_CODE_USE_BEDROCK=1".to_string());

            // AWS region: per-project overrides global
            let region = if !bedrock.aws_region.is_empty() {
                Some(bedrock.aws_region.clone())
            } else {
                global_aws.aws_region.clone()
            };
            if let Some(ref r) = region {
                env_vars.push(format!("AWS_REGION={}", r));
            }

            match bedrock.auth_method {
                BedrockAuthMethod::StaticCredentials => {
                    // Static/session credentials are NOT injected as env vars.
                    // They are written to ~/.aws/credentials by
                    // sync_bedrock_credentials() on every container
                    // start, so rotated/updated keys are picked up without a
                    // full container recreation (and never get baked into the
                    // snapshot image). The empty values set by the
                    // MANAGED_AUTH_KEYS neutralization pass below are ignored by
                    // the AWS SDK, which falls through to the credentials file.
                }
                BedrockAuthMethod::Profile => {
                    // Per-project profile overrides global
                    let profile = bedrock.aws_profile.as_ref()
                        .or(global_aws.aws_profile.as_ref());
                    if let Some(p) = profile {
                        env_vars.push(format!("AWS_PROFILE={}", p));
                    }
                    env_vars.push("AWS_SSO_AUTH_REFRESH_CMD=triple-c-sso-refresh".to_string());
                }
                BedrockAuthMethod::BearerToken => {
                    if let Some(ref token) = bedrock.aws_bearer_token {
                        env_vars.push(format!("AWS_BEARER_TOKEN_BEDROCK={}", token));
                    }
                }
            }

            if let Some(model) = resolve_with_global(
                bedrock.model_id.as_deref(),
                global_aws.default_model_id.as_deref(),
            ) {
                env_vars.push(format!("ANTHROPIC_MODEL={}", model));
            }

            if bedrock.disable_prompt_caching {
                env_vars.push("DISABLE_PROMPT_CACHING=1".to_string());
            }

            if let Some(ref tier) = bedrock.service_tier {
                let trimmed = tier.trim();
                if !trimmed.is_empty() {
                    env_vars.push(format!("ANTHROPIC_BEDROCK_SERVICE_TIER={}", trimmed));
                }
            }
        }
    }

    // ── Custom-endpoint backends ─────────────────────────────────────────────
    // Ollama, llama.cpp and the OpenAI-Compatible gateway all point Claude Code
    // at a non-Anthropic server via ANTHROPIC_BASE_URL. Each resolves its model
    // id here; the model-alias vars are emitted once below, from
    // `alias_model` / `alias_haiku`, so the three backends cannot drift apart.
    let mut alias_model: Option<String> = None;
    let mut alias_haiku: Option<String> = None;

    // Ollama configuration
    if project.backend == Backend::Ollama {
        if let Some(ref ollama) = project.ollama_config {
            if let Some(url) = resolve_with_global(
                Some(&ollama.base_url),
                global_ollama.base_url.as_deref(),
            ) {
                env_vars.push(format!("ANTHROPIC_BASE_URL={}", url));
            }
            env_vars.push("ANTHROPIC_AUTH_TOKEN=ollama".to_string());
            if let Some(model) = resolve_with_global(
                ollama.model_id.as_deref(),
                global_ollama.default_model_id.as_deref(),
            ) {
                env_vars.push(format!("ANTHROPIC_MODEL={}", model));
                alias_model = Some(model.to_string());
            }
            alias_haiku = resolve_with_global(
                ollama.haiku_model_id.as_deref(),
                global_ollama.default_haiku_model_id.as_deref(),
            )
            .map(str::to_string);
        }
    }

    // llama.cpp (llama-server) configuration
    if project.backend == Backend::LlamaCpp {
        if let Some(ref cfg) = project.llamacpp_config {
            if let Some(url) = resolve_with_global(
                Some(&cfg.base_url),
                global_llamacpp.base_url.as_deref(),
            ) {
                env_vars.push(format!("ANTHROPIC_BASE_URL={}", url));
            }
            // llama-server only enforces an Authorization header when it was
            // started with `--api-key` (default: none), so the value here is
            // ignored in the common case. Claude Code still refuses to run
            // against a custom base URL with no credential at all, so a
            // placeholder is always sent — same trick as the Ollama branch
            // above, which sends the literal "ollama".
            env_vars.push("ANTHROPIC_AUTH_TOKEN=llama.cpp".to_string());
            if let Some(model) = resolve_with_global(
                cfg.model_id.as_deref(),
                global_llamacpp.default_model_id.as_deref(),
            ) {
                env_vars.push(format!("ANTHROPIC_MODEL={}", model));
                alias_model = Some(model.to_string());
            }
            alias_haiku = resolve_with_global(
                cfg.haiku_model_id.as_deref(),
                global_llamacpp.default_haiku_model_id.as_deref(),
            )
            .map(str::to_string);
        }
    }

    // OpenAI Compatible configuration
    if project.backend == Backend::OpenAiCompatible {
        if let Some(ref config) = project.openai_compatible_config {
            if let Some(url) = resolve_with_global(
                Some(&config.base_url),
                global_openai_compatible.base_url.as_deref(),
            ) {
                env_vars.push(format!("ANTHROPIC_BASE_URL={}", url));
            }
            if let Some(ref key) = config.api_key {
                env_vars.push(format!("ANTHROPIC_AUTH_TOKEN={}", key));
            }
            if let Some(model) = resolve_with_global(
                config.model_id.as_deref(),
                global_openai_compatible.default_model_id.as_deref(),
            ) {
                env_vars.push(format!("ANTHROPIC_MODEL={}", model));
                alias_model = Some(model.to_string());
            }
            alias_haiku = resolve_with_global(
                config.haiku_model_id.as_deref(),
                global_openai_compatible.default_haiku_model_id.as_deref(),
            )
            .map(str::to_string);
        }
    }

    // Model aliases — the fix for background Claude Code calls against a local
    // server. Only for backends that talk to a custom endpoint: Anthropic and
    // Bedrock reach servers that really do host the Anthropic model ids, so
    // they keep Claude Code's own defaults. Anything not emitted here is
    // blanked by the MANAGED_AUTH_KEYS pass below, so switching *away* from a
    // custom endpoint clears the aliases out of the snapshot image too.
    if project.backend.uses_custom_endpoint() {
        for (key, value) in
            compute_model_aliases(alias_model.as_deref(), alias_haiku.as_deref())
        {
            env_vars.push(format!("{}={}", key, value));
        }
    }

    // Shared Claude Code OAuth token (Anthropic backend only, opt-out per
    // project). Injected *before* the neutralization pass below so that pass
    // sees it as already-set; when it is absent the pass actively blanks the
    // variable instead of leaving a stale one baked into the snapshot image.
    let shared_claude = shared_claude_auth(project);
    if let Some((ref token, _)) = shared_claude {
        env_vars.push(format!("{}={}", CLAUDE_OAUTH_TOKEN_ENV, token));
        log::info!(
            "Injecting the shared Claude authentication token into the container for project {}",
            project.id
        );
    }

    // ── Corporate CA certificates ───────────────────────────────────────────
    // Resolved here (rather than down with the mounts) so the env vars land
    // *before* the neutralization pass below and are seen as already-set.
    //
    // A bad path is a hard error, not a warning: behind a TLS-terminating
    // proxy a container without the CA fails every HTTPS call — npm, pip, git,
    // and Claude Code's own API requests — each in its own confusing way. One
    // message naming the path is far kinder.
    //
    // The values are set here rather than exported by the entrypoint because a
    // terminal session is a `docker exec`, which sees the container's
    // configured env and nothing the entrypoint exported. Same lesson as
    // `$BROWSER` and the URL relay shim.
    let effective_ca_path =
        resolve_with_global(project.ca_cert_path.as_deref(), default_ca_cert_path);
    let resolved_ca = ca_certs::resolve(effective_ca_path)?;
    if let Some(ref ca) = resolved_ca {
        log::info!(
            "Mounting {} corporate CA certificate(s) from {} into project {}",
            ca.cert_files.len(),
            ca.host_path,
            project.id
        );
    }
    for (key, value) in ca_certs::ca_env_vars(resolved_ca.as_ref()) {
        env_vars.push(format!("{}={}", key, value));
    }

    // ── Neutralize stale backend auth env vars ──────────────────────────────
    // When a project switches backends (e.g. Bedrock → Anthropic) the container
    // is recreated *from a snapshot image* committed off the previous container,
    // and `docker commit` copies that container's full ENV into the image. So
    // any auth var set under the old backend (e.g. CLAUDE_CODE_USE_BEDROCK=1,
    // AWS_PROFILE, a model alias) survives in the image ENV and stays active
    // unless we explicitly override it at create time.
    //
    // This pass is about *staleness*, not secrecy. It fixes the container it is
    // building and does nothing to the image, so it is not — and never was —
    // a defence against a credential baked into a snapshot. That is
    // `commit_container_snapshot`'s job, via SECRET_ENV_KEYS.
    // Create-time env takes precedence over image ENV, so we set every managed
    // auth key the *current* backend did NOT set to an empty value, clearing the
    // stale baked-in one.
    const MANAGED_AUTH_KEYS: &[&str] = &[
        "CLAUDE_CODE_USE_BEDROCK",
        "AWS_REGION",
        "AWS_ACCESS_KEY_ID",
        "AWS_SECRET_ACCESS_KEY",
        "AWS_SESSION_TOKEN",
        "AWS_PROFILE",
        "AWS_BEARER_TOKEN_BEDROCK",
        "AWS_SSO_AUTH_REFRESH_CMD",
        "ANTHROPIC_BASE_URL",
        "ANTHROPIC_AUTH_TOKEN",
        "ANTHROPIC_MODEL",
        "DISABLE_PROMPT_CACHING",
        "ANTHROPIC_BEDROCK_SERVICE_TIER",
        // Switching from a custom-endpoint backend to Anthropic or Bedrock must
        // *clear* the aliases, not merely stop setting them: a stale
        // ANTHROPIC_DEFAULT_HAIKU_MODEL baked into the snapshot image would
        // keep pointing background calls at a model id the new backend has
        // never heard of.
        ANTHROPIC_DEFAULT_OPUS_MODEL,
        ANTHROPIC_DEFAULT_SONNET_MODEL,
        ANTHROPIC_DEFAULT_HAIKU_MODEL,
        ANTHROPIC_DEFAULT_FABLE_MODEL,
        // Revoking the shared token, opting a project out, or switching away
        // from the Anthropic backend must *clear* this, not merely stop setting
        // it — otherwise the value committed into the snapshot image keeps
        // authenticating the container with a credential the user removed.
        CLAUDE_OAUTH_TOKEN_ENV,
    ];
    // Same reasoning for the CA vars — `ca_env_vars` already emits them empty
    // when no CA is configured, so this list is belt-and-braces for a snapshot
    // committed by a build that predates the feature.
    let managed_keys: Vec<&str> = MANAGED_AUTH_KEYS
        .iter()
        .copied()
        .chain(ca_certs::CA_ENV_KEYS.iter().copied())
        .collect();
    let already_set: std::collections::HashSet<String> = env_vars
        .iter()
        .filter_map(|e| e.split('=').next().map(|k| k.to_string()))
        .collect();
    for key in &managed_keys {
        if !already_set.contains(*key) {
            env_vars.push(format!("{}=", key));
        }
    }

    // Custom environment variables (global + per-project, project overrides global for same key)
    let merged_env = merge_custom_env_vars(global_custom_env_vars, &project.custom_env_vars);
    for env_var in &merged_env {
        let key = env_var.key.trim();
        if key.is_empty() {
            continue;
        }
        if is_reserved_env_key(key) {
            log::warn!("Skipping reserved env var: {}", key);
            continue;
        }
        env_vars.push(format!("{}={}", key, env_var.value));
    }
    let custom_env_fingerprint = compute_env_fingerprint(&merged_env);
    env_vars.push(format!("TRIPLE_C_CUSTOM_ENV={}", custom_env_fingerprint));

    // Container timezone
    if let Some(tz) = timezone {
        if !tz.is_empty() {
            env_vars.push(format!("TZ={}", tz));
        }
    }

    // Mission Control env var
    if project.mission_control_enabled {
        env_vars.push("MISSION_CONTROL_ENABLED=1".to_string());
    }

    // Permission mode — read by triple-c-task-runner for scheduled (headless)
    // Claude Code runs. Interactive terminals get the flags directly instead.
    env_vars.push(format!(
        "TRIPLE_C_PERMISSION_MODE={}",
        project.effective_permission_mode().as_env_value()
    ));

    // Claude instructions (global + per-project, plus port mapping info + scheduler docs)
    let combined_instructions = build_claude_instructions(
        global_claude_instructions,
        project.claude_instructions.as_deref(),
        &project.port_mappings,
        project.mission_control_enabled,
        project.sandbox_mode_enabled,
    );

    if let Some(ref instructions) = combined_instructions {
        env_vars.push(format!("CLAUDE_INSTRUCTIONS={}", instructions));
    }

    // Claude Code settings (global + per-project merged)
    let merged_cc_settings = merge_claude_code_settings(
        global_claude_code_settings,
        project.claude_code_settings.as_ref(),
    );
    if let Some(ref cc) = merged_cc_settings {
        // Env-var-based settings (these are read directly by Claude Code)
        if cc.tui_mode.as_deref() == Some("fullscreen") {
            env_vars.push("CLAUDE_CODE_NO_FLICKER=1".to_string());
        }
        if cc.enable_session_recap {
            env_vars.push("CLAUDE_CODE_ENABLE_AWAY_SUMMARY=1".to_string());
        }
        if cc.env_scrub {
            env_vars.push("CLAUDE_CODE_SUBPROCESS_ENV_SCRUB=1".to_string());
        }
        if cc.prompt_caching_1h {
            env_vars.push("ENABLE_PROMPT_CACHING_1H=1".to_string());
        }
    }

    // settings.json-based settings (written by the entrypoint).
    // Always invoked so per-project sandbox state is injected even when no
    // ClaudeCodeSettings struct is present.
    if let Some(settings_json) = build_claude_code_settings_json(
        merged_cc_settings.as_ref(),
        project.sandbox_mode_enabled,
    ) {
        env_vars.push(format!("CLAUDE_CODE_SETTINGS_JSON={}", settings_json));
    }

    let mut mounts: Vec<Mount> = Vec::new();

    // Project directories -> /workspace/{mount_name}
    for pp in &project.paths {
        mounts.push(Mount {
            target: Some(format!("/workspace/{}", pp.mount_name)),
            source: Some(pp.host_path.clone()),
            typ: Some(MountTypeEnum::BIND),
            read_only: Some(false),
            ..Default::default()
        });
    }

    // Named volume for the entire home directory — preserves ~/.claude.json,
    // ~/.local (pip/npm globals), and any other user-level state across
    // container stop/start cycles.
    mounts.push(Mount {
        target: Some("/home/claude".to_string()),
        source: Some(format!("triple-c-home-{}", project.id)),
        typ: Some(MountTypeEnum::VOLUME),
        read_only: Some(false),
        ..Default::default()
    });

    // Named volume for claude config persistence — mounted as a nested volume
    // inside the home volume; Docker gives the more-specific mount precedence.
    mounts.push(Mount {
        target: Some("/home/claude/.claude".to_string()),
        source: Some(format!("triple-c-claude-config-{}", project.id)),
        typ: Some(MountTypeEnum::VOLUME),
        read_only: Some(false),
        ..Default::default()
    });

    // SSH keys mount (read-only staging; entrypoint copies to ~/.ssh with correct perms)
    // Per-project ssh_key_path overrides global default_ssh_key_path
    let effective_ssh_path = project.ssh_key_path.as_deref().or(default_ssh_key_path);
    if let Some(ssh_path) = effective_ssh_path {
        mounts.push(Mount {
            target: Some("/tmp/.host-ssh".to_string()),
            source: Some(ssh_path.to_string()),
            typ: Some(MountTypeEnum::BIND),
            read_only: Some(true),
            ..Default::default()
        });
    }

    // Corporate CA certificates mount (read-only staging; the entrypoint copies
    // them into /usr/local/share/ca-certificates with a `.crt` name and runs
    // update-ca-certificates). Mirrors /tmp/.host-ssh and /tmp/.host-aws.
    //
    // A directory mounts at /tmp/.host-ca; a single file mounts at
    // /tmp/.host-ca/<name>.crt so the entrypoint always sees a directory and
    // the certificate keeps a recognisable name. Docker creates the parent.
    if let Some(ref ca) = resolved_ca {
        mounts.push(Mount {
            target: Some(ca.mount_target.clone()),
            source: Some(ca.host_path.clone()),
            typ: Some(MountTypeEnum::BIND),
            read_only: Some(true),
            ..Default::default()
        });
    }

    // AWS config mount (read-only)
    // Mount if: Bedrock profile auth needs it, OR a global aws_config_path is set
    let should_mount_aws = if project.backend == Backend::Bedrock {
        if let Some(ref bedrock) = project.bedrock_config {
            bedrock.auth_method == BedrockAuthMethod::Profile
        } else {
            false
        }
    } else {
        false
    };

    // For static-credential Bedrock, sync_bedrock_credentials() is the sole
    // owner of ~/.aws/credentials (it rewrites it on every start). Mounting the
    // host AWS dir would make the entrypoint's `rm -rf ~/.aws; cp -a` race that
    // write at startup, so we never mount it in that case — the static keys
    // (+ AWS_REGION env) are self-sufficient and don't need the host config.
    let is_bedrock_static = project.backend == Backend::Bedrock
        && project
            .bedrock_config
            .as_ref()
            .map(|b| b.auth_method == BedrockAuthMethod::StaticCredentials)
            .unwrap_or(false);

    if (should_mount_aws || aws_config_path.is_some()) && !is_bedrock_static {
        let aws_dir = aws_config_path
            .map(|p| std::path::PathBuf::from(p))
            .or_else(|| dirs::home_dir().map(|h| h.join(".aws")));

        if let Some(ref aws_path) = aws_dir {
            if aws_path.exists() {
                mounts.push(Mount {
                    target: Some("/tmp/.host-aws".to_string()),
                    source: Some(aws_path.to_string_lossy().to_string()),
                    typ: Some(MountTypeEnum::BIND),
                    read_only: Some(true),
                    ..Default::default()
                });
            }
        }
    }

    // Docker socket (if allowed)
    if project.allow_docker_access {
        // On Windows, the named pipe (//./pipe/docker_engine) cannot be
        // bind-mounted into a Linux container. Docker Desktop exposes the
        // daemon socket as /var/run/docker.sock for container mounts.
        let mount_source = if docker_socket_path == "//./pipe/docker_engine" {
            "/var/run/docker.sock".to_string()
        } else {
            docker_socket_path.to_string()
        };
        mounts.push(Mount {
            target: Some("/var/run/docker.sock".to_string()),
            source: Some(mount_source),
            typ: Some(MountTypeEnum::BIND),
            read_only: Some(false),
            ..Default::default()
        });
    }

    // Port mappings
    let mut exposed_ports: HashMap<String, HashMap<(), ()>> = HashMap::new();
    let mut port_bindings: HashMap<String, Option<Vec<PortBinding>>> = HashMap::new();
    for pm in &project.port_mappings {
        let container_key = format!("{}/{}", pm.container_port, pm.protocol);
        exposed_ports.insert(container_key.clone(), HashMap::new());
        port_bindings.insert(
            container_key,
            Some(vec![PortBinding {
                host_ip: Some("0.0.0.0".to_string()),
                host_port: Some(pm.host_port.to_string()),
            }]),
        );
    }

    let mut labels = HashMap::new();
    labels.insert("triple-c.managed".to_string(), "true".to_string());
    labels.insert("triple-c.project-id".to_string(), project.id.clone());
    labels.insert("triple-c.project-name".to_string(), project.name.clone());
    labels.insert("triple-c.backend".to_string(), format!("{:?}", project.backend));
    labels.insert("triple-c.paths-fingerprint".to_string(), compute_paths_fingerprint(&project.paths));
    labels.insert("triple-c.bedrock-fingerprint".to_string(), compute_bedrock_fingerprint(project, global_aws));
    labels.insert("triple-c.ollama-fingerprint".to_string(), compute_ollama_fingerprint(project, global_ollama));
    labels.insert("triple-c.llamacpp-fingerprint".to_string(), compute_llamacpp_fingerprint(project, global_llamacpp));
    labels.insert("triple-c.openai-compatible-fingerprint".to_string(), compute_openai_compatible_fingerprint(project, global_openai_compatible));
    labels.insert("triple-c.ports-fingerprint".to_string(), compute_ports_fingerprint(&project.port_mappings));
    labels.insert("triple-c.image".to_string(), image_name.to_string());
    labels.insert("triple-c.timezone".to_string(), timezone.unwrap_or("").to_string());
    labels.insert("triple-c.mission-control".to_string(), project.mission_control_enabled.to_string());
    labels.insert("triple-c.permission-mode".to_string(),
        project.effective_permission_mode().as_env_value().to_string());
    labels.insert("triple-c.custom-env-fingerprint".to_string(), custom_env_fingerprint.clone());
    labels.insert("triple-c.claude-code-settings-fingerprint".to_string(),
        compute_claude_code_settings_fingerprint(merged_cc_settings.as_ref(), project.sandbox_mode_enabled));
    labels.insert("triple-c.instructions-fingerprint".to_string(),
        combined_instructions.as_ref().map(|s| sha256_hex(s)).unwrap_or_default());
    // Written unconditionally, even when empty — `container_needs_recreation`
    // is label-based and never diffs env or mounts, so without this a changed
    // CA path would silently do nothing until some unrelated setting forced a
    // rebuild. The fingerprint covers the certificate *bytes* as well as the
    // path, so swapping a rotated CA in at the same location is caught too.
    labels.insert("triple-c.ca-fingerprint".to_string(),
        ca_certs::compute_ca_fingerprint(effective_ca_path));
    labels.insert("triple-c.git-user-name".to_string(), effective_git_name.unwrap_or_default().to_string());
    labels.insert("triple-c.git-user-email".to_string(), effective_git_email.unwrap_or_default().to_string());
    labels.insert("triple-c.git-token-hash".to_string(),
        project.git_token.as_ref().map(|t| sha256_hex(t)).unwrap_or_default());
    // Rotation id, NOT the token and NOT a hash of it — labels are readable by
    // anything on the host via `docker inspect`.
    labels.insert("triple-c.claude-token-version".to_string(),
        shared_claude.as_ref().map(|(_, v)| v.clone()).unwrap_or_default());

    // ── Base-image lineage ───────────────────────────────────────────────────
    // `triple-c.create-image` is what this container was actually created
    // from — the snapshot when one exists, otherwise the configured base. It is
    // what `container_needs_recreation` compares against; the older
    // `triple-c.image` label recorded the same thing but was compared against
    // the container's *own* image, which is where it came from, so that check
    // was a tautology and never fired. `triple-c.image` is still written for
    // continuity with existing containers but is no longer compared.
    //
    // `triple-c.base-image-id` records the lineage — see `resolve_base_image_id`.
    //
    // All three (plus the migration marker) are written **unconditionally**,
    // even when empty. Docker merges an image's labels into a container's at
    // creation, and `docker commit` copies container labels onto the snapshot
    // image, so a value stamped once would otherwise ride the snapshot into
    // every future container forever. Writing the key explicitly overrides the
    // inherited one — the same defence MANAGED_AUTH_KEYS applies to env.
    labels.insert(
        super::migration::LABEL_CREATE_IMAGE.to_string(),
        image_name.to_string(),
    );
    labels.insert(
        super::migration::LABEL_BASE_IMAGE_ID.to_string(),
        resolve_base_image_id(image_name, base_image_name).await,
    );
    labels.insert(
        super::migration::LABEL_MIGRATION_STATE.to_string(),
        String::new(),
    );
    // Same defence, applied to the legacy MCP shim — and here it fixes a real,
    // observed bug rather than pre-empting one. `container_needs_recreation`
    // recreates any container carrying a non-empty `triple-c.mcp-fingerprint`,
    // but nothing has written that label since the MCP feature was removed. It
    // survives only by *inheritance* from a snapshot image committed by an
    // older build (one such image was found on this host with a non-empty
    // value), and every recreation re-commits it — so the shim can never
    // terminate and the project is recreated on every single start. Writing it
    // explicitly empty makes the shim fire exactly once, which is what it was
    // always meant to do.
    labels.insert("triple-c.mcp-fingerprint".to_string(), String::new());

    for (key, value) in extras.extra_labels {
        labels.insert((*key).to_string(), (*value).to_string());
    }

    let host_config = HostConfig {
        mounts: Some(mounts),
        port_bindings: if port_bindings.is_empty() { None } else { Some(port_bindings) },
        init: Some(true),
        ..Default::default()
    };

    let working_dir = if project.paths.len() == 1 {
        format!("/workspace/{}", project.paths[0].mount_name)
    } else {
        "/workspace".to_string()
    };

    let config = Config {
        image: Some(image_name.to_string()),
        hostname: Some("triple-c".to_string()),
        env: Some(env_vars),
        labels: Some(labels),
        working_dir: Some(working_dir),
        host_config: Some(host_config),
        exposed_ports: if exposed_ports.is_empty() { None } else { Some(exposed_ports) },
        tty: Some(true),
        ..Default::default()
    };

    let options = CreateContainerOptions {
        name: container_name,
        ..Default::default()
    };

    let response = docker
        .create_container(Some(options), config)
        .await
        .map_err(|e| format!("Failed to create container: {}", e))?;

    Ok(response.id)
}

pub async fn start_container(container_id: &str) -> Result<(), String> {
    let docker = get_docker()?;
    docker
        .start_container(container_id, None::<StartContainerOptions<String>>)
        .await
        .map_err(|e| format!("Failed to start container: {}", e))
}

pub async fn stop_container(container_id: &str) -> Result<(), String> {
    let docker = get_docker()?;
    docker
        .stop_container(
            container_id,
            Some(StopContainerOptions { t: 10 }),
        )
        .await
        .map_err(|e| format!("Failed to stop container: {}", e))
}

pub async fn remove_container(container_id: &str) -> Result<(), String> {
    let docker = get_docker()?;
    log::info!(
        "Removing container {} (v=false: named volumes such as claude config are preserved)",
        container_id
    );
    docker
        .remove_container(
            container_id,
            Some(RemoveContainerOptions {
                v: false, // preserve named volumes (claude config)
                force: true,
                ..Default::default()
            }),
        )
        .await
        .map_err(|e| format!("Failed to remove container: {}", e))
}

/// Return the snapshot image name for a project.
pub fn get_snapshot_image_name(project: &Project) -> String {
    format!("triple-c-snapshot-{}:latest", project.id)
}

/// Keep the container's `~/.aws/credentials` in sync with the project's Bedrock
/// auth on every container start:
///   - **Bedrock + static credentials**: (re)write `~/.aws/credentials` from the
///     latest keychain values and drop a stale `~/.aws/config` left by a prior
///     profile/SSO session, so rotated keys are picked up without recreating the
///     container.
///   - **Any other backend / auth method**: remove a stale `~/.aws/credentials`
///     written by a previous static-credential session, so the secrets don't
///     linger unused in the persistent home volume after switching away.
///
/// Both cleanups are skipped when `/tmp/.host-aws` is mounted (a global
/// `aws_config_path` is configured), since the entrypoint already refreshes
/// `~/.aws` from the host on every start in that case.
pub async fn sync_bedrock_credentials(
    container_id: &str,
    project: &Project,
) -> Result<(), String> {
    let static_bedrock = if project.backend == Backend::Bedrock {
        project
            .bedrock_config
            .as_ref()
            .filter(|b| b.auth_method == BedrockAuthMethod::StaticCredentials)
    } else {
        None
    };

    let bedrock = match static_bedrock {
        Some(b) if b.aws_access_key_id.as_deref().is_some_and(|k| !k.is_empty()) => b,
        _ => {
            // Not static-credential Bedrock (or static selected but no key set):
            // remove a stale credentials file from a previous static session.
            if matches!(static_bedrock, Some(_)) {
                log::warn!("Bedrock static auth selected but no AWS access key id is set");
            }
            let script = r#"if [ ! -d /tmp/.host-aws ]; then rm -f "$HOME/.aws/credentials"; fi"#;
            let cmd = vec!["sh".to_string(), "-c".to_string(), script.to_string()];
            let env = vec!["HOME=/home/claude".to_string()];
            if let Err(e) = crate::docker::exec::exec_oneshot_env(container_id, cmd, env).await {
                log::warn!(
                    "Failed to clear stale AWS credentials in container {}: {}",
                    container_id,
                    e
                );
            }
            return Ok(());
        }
    };

    let key_id = bedrock.aws_access_key_id.as_deref().unwrap_or("");
    let secret = bedrock.aws_secret_access_key.as_deref().unwrap_or("");

    // Pass secrets via the exec environment, then have the shell write them to
    // the file. This keeps them out of the process argv (visible via `ps`).
    let mut env = vec![
        "HOME=/home/claude".to_string(),
        format!("TC_AWS_KEY_ID={}", key_id),
        format!("TC_AWS_SECRET={}", secret),
    ];
    if let Some(token) = bedrock.aws_session_token.as_deref() {
        if !token.is_empty() {
            env.push(format!("TC_AWS_TOKEN={}", token));
        }
    }

    // umask 077 + explicit chmod guarantees 0600. The session-token line is only
    // emitted when the variable is non-empty.
    //
    // We also remove a stale ~/.aws/config left over from a previous
    // profile/SSO session on this project (the home volume persists across
    // backend switches), so its sso_session/profile settings don't shadow the
    // static [default] credentials. This is skipped when /tmp/.host-aws is
    // mounted (a global aws_config_path is configured) — in that case the
    // entrypoint already refreshes ~/.aws from the host on every start and the
    // config is intentional.
    let script = r#"set -e
umask 077
mkdir -p "$HOME/.aws"
if [ ! -d /tmp/.host-aws ] && [ -f "$HOME/.aws/config" ]; then
  rm -f "$HOME/.aws/config"
fi
{
  printf '[default]\n'
  printf 'aws_access_key_id=%s\n' "$TC_AWS_KEY_ID"
  printf 'aws_secret_access_key=%s\n' "$TC_AWS_SECRET"
  if [ -n "${TC_AWS_TOKEN:-}" ]; then
    printf 'aws_session_token=%s\n' "$TC_AWS_TOKEN"
  fi
} > "$HOME/.aws/credentials"
chmod 600 "$HOME/.aws/credentials""#;

    let cmd = vec!["sh".to_string(), "-c".to_string(), script.to_string()];
    let (output, exit_code) =
        crate::docker::exec::exec_oneshot_env_status(container_id, cmd, env)
            .await
            .map_err(|e| format!("Failed to write AWS credentials into container: {}", e))?;
    if exit_code != 0 {
        return Err(format!(
            "Writing AWS credentials into container failed (exit {}): {}",
            exit_code,
            output.trim()
        ));
    }

    log::info!("Wrote Bedrock static credentials into container {}", container_id);
    Ok(())
}

/// Commit the container's filesystem to a snapshot image so that system-level
/// changes (apt/pip/npm installs, ~/.claude.json, etc.) survive container
/// removal.
///
/// ## Why this passes a Config instead of `Default::default()`
///
/// `docker commit` bakes the *running container's* full ENV into the resulting
/// image. An earlier version of this function passed an empty `Config` and a
/// comment asserting that the API "gives no way to remove env vars", with
/// `MANAGED_AUTH_KEYS` cited as the defence. That was wrong on both counts.
///
/// `MANAGED_AUTH_KEYS` defends the *next container* — it overrides the image's
/// stale value at create time — and does nothing whatsoever about the value
/// sitting in the image. So `docker image inspect triple-c-snapshot-<id>:latest
/// --format '{{json .Config.Env}}'` returned the shared ~1-year OAuth token,
/// and kept returning it after the user revoked the token, because
/// `clear_claude_token` only deletes a keychain entry.
///
/// And the API does allow it. Verified against Engine 29.6: the config in the
/// commit body is **merged over** the container's config, key by key for `Env`,
/// with unmentioned fields (`Cmd`, `WorkingDir`, `Labels`, …) inherited
/// untouched. A key cannot be *dropped*, but it can be set — so every name in
/// [`SECRET_ENV_KEYS`] is committed as `KEY=`, which is exactly the "empty
/// means unset" convention the rest of the auth plumbing already uses.
///
/// Non-secret env (PATH, TZ, model aliases, instructions) is inherited as
/// before, so nothing about the snapshot's behaviour changes.
pub async fn commit_container_snapshot(container_id: &str, project: &Project) -> Result<(), String> {
    let docker = get_docker()?;
    let image_name = get_snapshot_image_name(project);

    // Parse repo:tag
    let (repo, tag) = match image_name.rsplit_once(':') {
        Some((r, t)) => (r.to_string(), t.to_string()),
        None => (image_name.clone(), "latest".to_string()),
    };

    let options = CommitContainerOptions {
        container: container_id.to_string(),
        repo: repo.clone(),
        tag: tag.clone(),
        pause: true,
        ..Default::default()
    };

    let config = Config::<String> {
        env: Some(blanked_secret_env()),
        ..Default::default()
    };

    docker
        .commit_container(options, config)
        .await
        .map_err(|e| format!("Failed to commit container snapshot: {}", e))?;

    log::info!("Committed container {} as snapshot {}:{}", container_id, repo, tag);
    Ok(())
}

/// `KEY=` for every name in [`SECRET_ENV_KEYS`] — the env override handed to
/// `docker commit` so no credential value reaches an image.
fn blanked_secret_env() -> Vec<String> {
    SECRET_ENV_KEYS
        .iter()
        .map(|key| format!("{}=", key))
        .collect()
}

/// Whether `env` (an image's `Config.Env`) holds a non-empty value for any
/// name in [`SECRET_ENV_KEYS`].
fn env_holds_a_secret(env: &[String]) -> bool {
    env.iter().any(|entry| {
        let Some((key, value)) = entry.split_once('=') else {
            return false;
        };
        !value.is_empty() && SECRET_ENV_KEYS.contains(&key)
    })
}

/// Outcome of [`scrub_secrets_from_snapshots`], so callers can tell the user
/// what actually happened rather than guessing.
#[derive(Debug, Default, Clone, serde::Serialize)]
pub struct SnapshotScrubReport {
    /// Snapshot images that were found to hold a credential and were rewritten.
    pub scrubbed: Vec<String>,
    /// Snapshot images that hold a credential and could **not** be rewritten,
    /// each with the reason. A non-empty list means the tag every future
    /// container is built from still carries the credential.
    pub failed: Vec<(String, String)>,
    /// Tags that *were* rewritten, but whose superseded image object could not
    /// be deleted — almost always because a container is still running off it.
    ///
    /// Much weaker than `failed`, and normal rather than exceptional. The tag
    /// is clean, so nothing new is built with the credential; what remains is
    /// an untagged image whose config is still readable by id, for exactly as
    /// long as the container using it survives — and that container already
    /// holds the same value in its own env, so it is not a new exposure. The
    /// rotation-id label mismatch recreates it on the next start, after which
    /// an image prune collects the leftover.
    pub superseded_retained: Vec<String>,
    /// Set when the image list itself could not be read (Docker not running,
    /// no permission). Nothing was scrubbed and nothing is known.
    pub unavailable: Option<String>,
}

impl SnapshotScrubReport {
    /// True when a credential is known to still be reachable through something
    /// that will keep being used — a tag, or an unknown state.
    pub fn left_something_behind(&self) -> bool {
        !self.failed.is_empty() || self.unavailable.is_some()
    }
}

/// Rewrite every `triple-c-snapshot-*` image whose ENV still carries a
/// credential, blanking the values in place.
///
/// Revoking a shared token has to mean something. Deleting the keychain entry
/// stops *new* containers getting it, but images committed before
/// [`commit_container_snapshot`] learned to strip secrets still have the token
/// in their config, and those images are the ones every future container of
/// that project is built from. This is the cleanup for them.
///
/// Mechanics: create (do not start) a throwaway container from the image, then
/// commit it straight back over the same tag with the secret keys blanked. The
/// new image shares every layer with the old one, so this costs no meaningful
/// disk and preserves the project's installed packages exactly. The superseded
/// image is then removed by id; if Docker refuses (some storage drivers will
/// not delete an image that is a parent of another), the report says so rather
/// than pretending the secret is gone.
///
/// Never fails the caller: an unreachable Docker engine is reported in the
/// return value, because the keychain deletion that precedes it must still
/// stand.
pub async fn scrub_secrets_from_snapshots() -> SnapshotScrubReport {
    use bollard::image::ListImagesOptions;

    let mut report = SnapshotScrubReport::default();

    let docker = match get_docker() {
        Ok(d) => d,
        Err(e) => {
            report.unavailable = Some(e);
            return report;
        }
    };

    let filters: HashMap<String, Vec<String>> = HashMap::from([(
        "reference".to_string(),
        vec!["triple-c-snapshot-*".to_string()],
    )]);
    let images = match docker
        .list_images(Some(ListImagesOptions {
            filters,
            ..Default::default()
        }))
        .await
    {
        Ok(images) => images,
        Err(e) => {
            report.unavailable = Some(format!("Could not list snapshot images: {}", e));
            return report;
        }
    };

    for summary in images {
        // `list_images` does not return Config, so inspect each candidate.
        let details = match docker.inspect_image(&summary.id).await {
            Ok(d) => d,
            Err(e) => {
                report
                    .failed
                    .push((summary.id.clone(), format!("could not inspect: {}", e)));
                continue;
            }
        };
        let env = details
            .config
            .as_ref()
            .and_then(|c| c.env.clone())
            .unwrap_or_default();
        if !env_holds_a_secret(&env) {
            continue;
        }

        if summary.repo_tags.is_empty() {
            // The reference filter should make this impossible; if it happens,
            // say so rather than silently leaving a credential in place.
            report.failed.push((
                summary.id.clone(),
                "an untagged snapshot image holds a credential and cannot be rewritten"
                    .to_string(),
            ));
            continue;
        }

        // Rewrite every tag this image answers to, so an old tag cannot keep
        // serving the un-scrubbed config.
        let mut all_tags_rewritten = true;
        for tag in summary.repo_tags.iter() {
            let (repo, tag_part) = match tag.rsplit_once(':') {
                Some((r, t)) => (r.to_string(), t.to_string()),
                None => (tag.clone(), "latest".to_string()),
            };
            if let Err(e) = rewrite_image_without_secrets(&docker, tag, &repo, &tag_part).await {
                all_tags_rewritten = false;
                report.failed.push((tag.clone(), e));
            } else {
                report.scrubbed.push(tag.clone());
            }
        }

        // Drop the superseded image so its config stops being inspectable.
        // Best effort by design — see the doc comment.
        if all_tags_rewritten {
            if let Err(e) = docker
                .remove_image(
                    &summary.id,
                    Some(RemoveImageOptions {
                        force: false,
                        noprune: false,
                    }),
                    None,
                )
                .await
            {
                // Expected whenever the project's container is still around:
                // Docker will not delete an image a container was created
                // from. Not a failure of the scrub — see `superseded_retained`.
                log::info!(
                    "Scrubbed snapshot {} but kept the superseded image {}: {}",
                    summary.repo_tags.join(", "),
                    summary.id,
                    e
                );
                report
                    .superseded_retained
                    .push(summary.repo_tags.join(", "));
            }
        }
    }

    report
}

/// Commit `source_image` back over `repo:tag` with [`SECRET_ENV_KEYS`] blanked.
async fn rewrite_image_without_secrets(
    docker: &bollard::Docker,
    source_image: &str,
    repo: &str,
    tag: &str,
) -> Result<(), String> {
    let scratch_name = format!("triple-c-scrub-{}", uuid::Uuid::new_v4().simple());

    let created = docker
        .create_container(
            Some(CreateContainerOptions {
                name: scratch_name.clone(),
                ..Default::default()
            }),
            Config::<String> {
                image: Some(source_image.to_string()),
                // Deliberately nothing else. The container is never started;
                // its only job is to be a config to commit from, and every
                // field left unset here is inherited from the image and
                // inherited back out by the commit. Setting `cmd` to a
                // placeholder — the obvious way to satisfy an image with no
                // CMD — would write that placeholder into the rewritten
                // snapshot. The base image has an ENTRYPOINT, so `create`
                // needs no command; an image with neither fails here and is
                // reported rather than silently mangled.
                ..Default::default()
            },
        )
        .await
        .map_err(|e| format!("could not create a scratch container: {}", e))?;

    let commit = docker
        .commit_container(
            CommitContainerOptions {
                container: created.id.clone(),
                repo: repo.to_string(),
                tag: tag.to_string(),
                // Nothing is running; pausing a created container is an error.
                pause: false,
                ..Default::default()
            },
            Config::<String> {
                env: Some(blanked_secret_env()),
                ..Default::default()
            },
        )
        .await
        .map_err(|e| format!("could not re-commit without the credential: {}", e));

    // Remove the scratch container whatever happened to the commit.
    if let Err(e) = docker
        .remove_container(
            &created.id,
            Some(RemoveContainerOptions {
                force: true,
                ..Default::default()
            }),
        )
        .await
    {
        log::warn!("Could not remove scratch container {}: {}", scratch_name, e);
    }

    commit.map(|_| ())
}

/// Remove the snapshot image for a project (used on Reset / project removal).
pub async fn remove_snapshot_image(project: &Project) -> Result<(), String> {
    let docker = get_docker()?;
    let image_name = get_snapshot_image_name(project);

    docker
        .remove_image(
            &image_name,
            Some(RemoveImageOptions {
                force: true,
                noprune: false,
            }),
            None,
        )
        .await
        .map_err(|e| format!("Failed to remove snapshot image {}: {}", image_name, e))?;

    log::info!("Removed snapshot image {}", image_name);
    Ok(())
}

/// Remove both named volumes for a project (used on Reset / project removal).
pub async fn remove_project_volumes(project: &Project) -> Result<(), String> {
    let docker = get_docker()?;
    for vol in [
        format!("triple-c-home-{}", project.id),
        format!("triple-c-claude-config-{}", project.id),
    ] {
        match docker.remove_volume(&vol, None).await {
            Ok(_) => log::info!("Removed volume {}", vol),
            Err(e) => log::warn!("Failed to remove volume {} (may not exist): {}", vol, e),
        }
    }
    Ok(())
}

/// Check whether the existing container's configuration still matches the
/// current project settings.  Returns `true` when the container must be
/// recreated (mounts or env vars differ).
pub async fn container_needs_recreation(
    container_id: &str,
    project: &Project,
    expected_create_image: &str,
    global_aws: &GlobalAwsSettings,
    global_ollama: &GlobalOllamaSettings,
    global_llamacpp: &GlobalLlamaCppSettings,
    global_openai_compatible: &GlobalOpenAiCompatibleSettings,
    global_claude_instructions: Option<&str>,
    global_custom_env_vars: &[EnvVar],
    timezone: Option<&str>,
    global_claude_code_settings: Option<&ClaudeCodeSettings>,
    default_ssh_key_path: Option<&str>,
    default_ca_cert_path: Option<&str>,
    default_git_user_name: Option<&str>,
    default_git_user_email: Option<&str>,
) -> Result<bool, String> {
    let docker = get_docker()?;
    let info = docker
        .inspect_container(container_id, None)
        .await
        .map_err(|e| format!("Failed to inspect container: {}", e))?;

    let labels = info
        .config
        .as_ref()
        .and_then(|c| c.labels.as_ref());

    let get_label = |name: &str| -> Option<String> {
        labels.and_then(|l| l.get(name).cloned())
    };

    let mounts = info
        .host_config
        .as_ref()
        .and_then(|hc| hc.mounts.as_ref());

    // ── Docker socket mount ──────────────────────────────────────────────
    // Intentionally NOT checked here. Toggling "Allow container spawning"
    // should not trigger a full container recreation (which loses Claude
    // Code settings stored in the named volume). The change takes effect
    // on the next explicit rebuild instead.

    // ── Backend ──────────────────────────────────────────────────────────
    let current_backend = format!("{:?}", project.backend);
    // Check new label name, falling back to old "triple-c.auth-mode" for pre-rename containers
    let container_backend = get_label("triple-c.backend").or_else(|| get_label("triple-c.auth-mode"));
    if let Some(container_backend) = container_backend {
        if container_backend != current_backend {
            log::info!("Backend mismatch (container={:?}, project={:?})", container_backend, current_backend);
            return Ok(true);
        }
    }

    // ── Project paths fingerprint ──────────────────────────────────────────
    let expected_paths_fp = compute_paths_fingerprint(&project.paths);
    match get_label("triple-c.paths-fingerprint") {
        Some(container_fp) => {
            if container_fp != expected_paths_fp {
                log::info!("Paths fingerprint mismatch (container={:?}, expected={:?})", container_fp, expected_paths_fp);
                return Ok(true);
            }
        }
        None => {
            // Old container without paths-fingerprint label -> force recreation for migration
            log::info!("Container missing paths-fingerprint label, triggering recreation for migration");
            return Ok(true);
        }
    }

    // ── Port mappings fingerprint ──────────────────────────────────────────
    let expected_ports_fp = compute_ports_fingerprint(&project.port_mappings);
    let container_ports_fp = get_label("triple-c.ports-fingerprint").unwrap_or_default();
    if container_ports_fp != expected_ports_fp {
        log::info!("Port mappings fingerprint mismatch (container={:?}, expected={:?})", container_ports_fp, expected_ports_fp);
        return Ok(true);
    }

    // ── Bedrock config fingerprint ───────────────────────────────────────
    let expected_bedrock_fp = compute_bedrock_fingerprint(project, global_aws);
    let container_bedrock_fp = get_label("triple-c.bedrock-fingerprint").unwrap_or_default();
    if container_bedrock_fp != expected_bedrock_fp {
        log::info!("Bedrock config mismatch");
        return Ok(true);
    }

    // ── Ollama config fingerprint ────────────────────────────────────────
    let expected_ollama_fp = compute_ollama_fingerprint(project, global_ollama);
    let container_ollama_fp = get_label("triple-c.ollama-fingerprint").unwrap_or_default();
    if container_ollama_fp != expected_ollama_fp {
        log::info!("Ollama config mismatch");
        return Ok(true);
    }

    // ── llama.cpp config fingerprint ─────────────────────────────────────
    // A missing label means the container predates the llama.cpp backend, in
    // which case the expected fingerprint is also "" (no llamacpp_config) and
    // nothing is recreated needlessly.
    let expected_llamacpp_fp = compute_llamacpp_fingerprint(project, global_llamacpp);
    let container_llamacpp_fp = get_label("triple-c.llamacpp-fingerprint").unwrap_or_default();
    if container_llamacpp_fp != expected_llamacpp_fp {
        log::info!("llama.cpp config mismatch");
        return Ok(true);
    }

    // ── OpenAI Compatible config fingerprint ────────────────────────────
    let expected_oai_fp = compute_openai_compatible_fingerprint(project, global_openai_compatible);
    let container_oai_fp = get_label("triple-c.openai-compatible-fingerprint").unwrap_or_default();
    if container_oai_fp != expected_oai_fp {
        log::info!("OpenAI Compatible config mismatch");
        return Ok(true);
    }

    // ── Create image ─────────────────────────────────────────────────────
    // What this container was created from, against what we would create it
    // from *now* — the caller resolves that (snapshot-if-it-exists, else the
    // configured base) and passes it in as `expected_create_image`, preserving
    // exactly today's semantics.
    //
    // This replaces a check that compared the container's actual image against
    // the `triple-c.image` label. `create_container` wrote that label from the
    // very image it created from, so the two could never differ: it was a
    // tautology that never once fired, and it is the reason a project stayed
    // pinned to its own snapshot lineage forever.
    //
    // A missing `triple-c.create-image` label means the container predates this
    // fix — unknown, so leave it alone rather than churn every existing
    // container on first launch after an update.
    if let Some(container_create_image) = get_label(crate::docker::migration::LABEL_CREATE_IMAGE) {
        if container_create_image != expected_create_image {
            log::info!(
                "Create-image mismatch (container={:?}, expected={:?})",
                container_create_image,
                expected_create_image
            );
            return Ok(true);
        }
    }

    // ── Base image id: deliberately NOT compared here ────────────────────
    // This departs from the CLAUDE.md rule that new container state gets a
    // label and a comparison, and the departure is the point.
    //
    // `triple-c.base-image-id` records which base a container's lineage
    // descends from. Comparing it here would mean that publishing a new base
    // image silently recreates every project on next start — and, because
    // `expected_create_image` is the snapshot whenever one exists, it would
    // recreate them *from their own snapshot*: pure churn, on the old base,
    // with no benefit. Worse, it would consume the very signal ("this project
    // is behind the base") that is supposed to prompt the user, without
    // actually migrating anything.
    //
    // Staleness is therefore a *surfaced* signal gating an explicit user
    // action — `get_container_staleness` / `migrate_project_to_base` — not an
    // automatic recreation trigger.

    // ── Timezone ─────────────────────────────────────────────────────────
    let expected_tz = timezone.unwrap_or("");
    let container_tz = get_label("triple-c.timezone").unwrap_or_default();
    if container_tz != expected_tz {
        log::info!("Timezone mismatch (container={:?}, expected={:?})", container_tz, expected_tz);
        return Ok(true);
    }

    // ── SSH key path mount ───────────────────────────────────────────────
    let ssh_mount_source = mounts
        .and_then(|m| {
            m.iter()
                .find(|mount| mount.target.as_deref() == Some("/tmp/.host-ssh"))
        })
        .and_then(|mount| mount.source.as_deref());
    let effective_ssh = project.ssh_key_path.as_deref().or(default_ssh_key_path);
    if ssh_mount_source != effective_ssh {
        log::info!(
            "SSH key path mismatch (container={:?}, expected={:?})",
            ssh_mount_source,
            effective_ssh
        );
        return Ok(true);
    }

    // ── Corporate CA certificates ────────────────────────────────────────
    // Both the resolved path and the certificate contents, so replacing a
    // rotated CA at the same path recreates the container — the copy inside
    // the container is made once, at start, and nothing else would notice.
    //
    // A container predating this feature has no label, i.e. "", which is also
    // what an unconfigured CA fingerprints as — so existing installs are not
    // churned until a CA is actually set.
    let expected_ca_fp = ca_certs::compute_ca_fingerprint(resolve_with_global(
        project.ca_cert_path.as_deref(),
        default_ca_cert_path,
    ));
    let container_ca_fp = get_label("triple-c.ca-fingerprint").unwrap_or_default();
    if container_ca_fp != expected_ca_fp {
        log::info!(
            "Corporate CA certificate mismatch (container={:?}, expected={:?})",
            container_ca_fp,
            expected_ca_fp
        );
        return Ok(true);
    }

    // ── Git settings (label-based to avoid stale snapshot env vars) ─────
    let expected_git_name = project.git_user_name.as_deref()
        .or(default_git_user_name)
        .unwrap_or_default()
        .to_string();
    let container_git_name = get_label("triple-c.git-user-name").unwrap_or_default();
    if container_git_name != expected_git_name {
        log::info!("GIT_USER_NAME mismatch (container={:?}, expected={:?})", container_git_name, expected_git_name);
        return Ok(true);
    }

    let expected_git_email = project.git_user_email.as_deref()
        .or(default_git_user_email)
        .unwrap_or_default()
        .to_string();
    let container_git_email = get_label("triple-c.git-user-email").unwrap_or_default();
    if container_git_email != expected_git_email {
        log::info!("GIT_USER_EMAIL mismatch (container={:?}, expected={:?})", container_git_email, expected_git_email);
        return Ok(true);
    }

    let expected_git_token_hash = project.git_token.as_ref().map(|t| sha256_hex(t)).unwrap_or_default();
    let container_git_token_hash = get_label("triple-c.git-token-hash").unwrap_or_default();
    if container_git_token_hash != expected_git_token_hash {
        log::info!("GIT_TOKEN mismatch");
        return Ok(true);
    }

    // ── Shared Claude Code OAuth token ───────────────────────────────────
    // Compares rotation ids, so this fires when the token is first acquired,
    // re-acquired (rotated), revoked, or opted out of. Both "" means no token
    // is in play, which is also what a container predating this feature reports
    // — so existing installs are not recreated until a token actually exists.
    // Recreation is the only way to change a container's env, so it is the only
    // way a revoked token stops being live in one. It does *not* clean the
    // snapshot image — `clear_claude_token` calls
    // `scrub_secrets_from_snapshots` for that.
    let expected_claude_token = claude_token_label(project);
    let container_claude_token = get_label("triple-c.claude-token-version").unwrap_or_default();
    if container_claude_token != expected_claude_token {
        log::info!("Shared Claude authentication token mismatch — recreating container");
        return Ok(true);
    }

    // ── Custom environment variables (label-based fingerprint) ──────────
    let merged_env = merge_custom_env_vars(global_custom_env_vars, &project.custom_env_vars);
    let expected_fingerprint = compute_env_fingerprint(&merged_env);
    let container_fingerprint = get_label("triple-c.custom-env-fingerprint").unwrap_or_default();
    if container_fingerprint != expected_fingerprint {
        log::info!("Custom env vars mismatch (container={:?}, expected={:?})", container_fingerprint, expected_fingerprint);
        return Ok(true);
    }

    // ── Mission Control ────────────────────────────────────────────────────
    let expected_mc = project.mission_control_enabled.to_string();
    let container_mc = get_label("triple-c.mission-control").unwrap_or_else(|| "false".to_string());
    if container_mc != expected_mc {
        log::info!("Mission Control mismatch (container={:?}, expected={:?})", container_mc, expected_mc);
        return Ok(true);
    }

    // ── Permission mode ────────────────────────────────────────────────────
    // The mode is injected as the TRIPLE_C_PERMISSION_MODE env var, and
    // container env can only change by recreating the container. A missing
    // label means the container predates this feature and therefore has no
    // such env var, so it must be recreated too (empty != any valid mode).
    let expected_permission_mode = project.effective_permission_mode().as_env_value();
    let container_permission_mode = get_label("triple-c.permission-mode").unwrap_or_default();
    if container_permission_mode != expected_permission_mode {
        log::info!(
            "Permission mode mismatch (container={:?}, expected={:?})",
            container_permission_mode,
            expected_permission_mode
        );
        return Ok(true);
    }

    // ── Claude instructions (label-based fingerprint) ─────────────────────
    let expected_instructions = build_claude_instructions(
        global_claude_instructions,
        project.claude_instructions.as_deref(),
        &project.port_mappings,
        project.mission_control_enabled,
        project.sandbox_mode_enabled,
    );
    let expected_instructions_fp = expected_instructions.as_ref().map(|s| sha256_hex(s)).unwrap_or_default();
    let container_instructions_fp = get_label("triple-c.instructions-fingerprint").unwrap_or_default();
    if container_instructions_fp != expected_instructions_fp {
        log::info!("CLAUDE_INSTRUCTIONS mismatch");
        return Ok(true);
    }

    // ── Claude Code settings fingerprint ───────────────────────────────
    let merged_cc = merge_claude_code_settings(
        global_claude_code_settings,
        project.claude_code_settings.as_ref(),
    );
    let expected_cc_fp = compute_claude_code_settings_fingerprint(merged_cc.as_ref(), project.sandbox_mode_enabled);
    let container_cc_fp = get_label("triple-c.claude-code-settings-fingerprint").unwrap_or_default();
    if container_cc_fp != expected_cc_fp {
        log::info!("Claude Code settings mismatch (container={:?}, expected={:?})", container_cc_fp, expected_cc_fp);
        return Ok(true);
    }

    // ── Legacy MCP migration shim ───────────────────────────────────────
    // One-release migration for containers created before the built-in MCP
    // feature was removed. Such containers carry a `triple-c.mcp-fingerprint`
    // label and/or are attached to the per-project `triple-c-net-<id>` network.
    // That user-defined network is deleted during cleanup, and a container
    // whose NetworkMode points at a missing network refuses to start — so force
    // a recreation to move them onto the default bridge. Containers created by
    // the current code never carry the label or the network, so this is a no-op
    // for them and can be dropped a release later.
    if let Some(fp) = get_label("triple-c.mcp-fingerprint") {
        if !fp.is_empty() {
            log::info!("Legacy container carries triple-c.mcp-fingerprint label — recreating without MCP");
            return Ok(true);
        }
    }
    let legacy_network = info
        .host_config
        .as_ref()
        .and_then(|hc| hc.network_mode.as_deref())
        .map(|nm| nm.starts_with("triple-c-net-"))
        .unwrap_or(false);
    if legacy_network {
        log::info!("Legacy container attached to a triple-c-net-* network — recreating without MCP");
        return Ok(true);
    }

    Ok(false)
}

pub async fn get_container_info(project: &Project) -> Result<Option<ContainerInfo>, String> {
    if let Some(ref container_id) = project.container_id {
        let docker = get_docker()?;
        match docker.inspect_container(container_id, None).await {
            Ok(info) => {
                let status = info
                    .state
                    .and_then(|s| s.status)
                    .map(|s| format!("{:?}", s))
                    .unwrap_or_else(|| "unknown".to_string());

                // Read actual image from Docker inspect
                let image = info
                    .config
                    .and_then(|c| c.image)
                    .unwrap_or_else(|| "unknown".to_string());

                Ok(Some(ContainerInfo {
                    container_id: container_id.clone(),
                    project_id: project.id.clone(),
                    status,
                    image,
                }))
            }
            Err(_) => Ok(None),
        }
    } else {
        Ok(None)
    }
}

/// Check whether a Docker container is currently running.
/// Returns false if the container doesn't exist or Docker is unavailable.
pub async fn is_container_running(container_id: &str) -> Result<bool, String> {
    let docker = get_docker()?;
    match docker.inspect_container(container_id, None).await {
        Ok(info) => Ok(info.state.and_then(|s| s.running).unwrap_or(false)),
        Err(_) => Ok(false),
    }
}

pub async fn list_sibling_containers() -> Result<Vec<ContainerSummary>, String> {
    let docker = get_docker()?;

    let all_containers: Vec<ContainerSummary> = docker
        .list_containers(Some(ListContainersOptions::<String> {
            all: true,
            ..Default::default()
        }))
        .await
        .map_err(|e| format!("Failed to list containers: {}", e))?;

    let siblings: Vec<ContainerSummary> = all_containers
        .into_iter()
        .filter(|c| {
            if let Some(labels) = &c.labels {
                !labels.contains_key("triple-c.managed")
            } else {
                true
            }
        })
        .collect();

    Ok(siblings)
}

#[cfg(test)]
mod tests {
    use super::*;

    const OPUS: &str = ANTHROPIC_DEFAULT_OPUS_MODEL;
    const SONNET: &str = ANTHROPIC_DEFAULT_SONNET_MODEL;
    const HAIKU: &str = ANTHROPIC_DEFAULT_HAIKU_MODEL;
    const FABLE: &str = ANTHROPIC_DEFAULT_FABLE_MODEL;

    fn aliases(model: Option<&str>, haiku: Option<&str>) -> Vec<(&'static str, String)> {
        compute_model_aliases(model, haiku)
    }

    #[test]
    fn all_four_aliases_fall_back_to_the_configured_model() {
        assert_eq!(
            aliases(Some("qwen3.5:27b"), None),
            vec![
                (OPUS, "qwen3.5:27b".to_string()),
                (SONNET, "qwen3.5:27b".to_string()),
                (HAIKU, "qwen3.5:27b".to_string()),
                (FABLE, "qwen3.5:27b".to_string()),
            ]
        );
    }

    #[test]
    fn the_haiku_override_replaces_only_the_haiku_alias() {
        let got = aliases(Some("big-model"), Some("small-model"));
        assert_eq!(
            got,
            vec![
                (OPUS, "big-model".to_string()),
                (SONNET, "big-model".to_string()),
                (HAIKU, "small-model".to_string()),
                (FABLE, "big-model".to_string()),
            ]
        );
    }

    #[test]
    fn a_blank_or_whitespace_haiku_override_falls_back_to_the_model() {
        for override_value in [Some(""), Some("   "), None] {
            let got = aliases(Some("m"), override_value);
            assert_eq!(
                got.iter().find(|(k, _)| *k == HAIKU).map(|(_, v)| v.as_str()),
                Some("m"),
                "override {:?} should fall back to the model id",
                override_value
            );
        }
    }

    #[test]
    fn values_are_trimmed() {
        assert_eq!(
            aliases(Some("  m  "), Some("  h  ")),
            vec![
                (OPUS, "m".to_string()),
                (SONNET, "m".to_string()),
                (HAIKU, "h".to_string()),
                (FABLE, "m".to_string()),
            ]
        );
    }

    #[test]
    fn no_model_and_no_override_emits_nothing() {
        // Nothing to point the aliases at — leave Claude Code's defaults alone
        // rather than injecting empty vars.
        assert!(aliases(None, None).is_empty());
        assert!(aliases(Some(""), Some("  ")).is_empty());
    }

    #[test]
    fn a_haiku_override_alone_still_fixes_background_calls() {
        // No model id configured, but the user pointed haiku somewhere: emit
        // just that one, because it is the alias background work uses.
        assert_eq!(
            aliases(None, Some("small-model")),
            vec![(HAIKU, "small-model".to_string())]
        );
    }

    #[test]
    fn only_custom_endpoint_backends_get_aliases() {
        assert!(!Backend::Anthropic.uses_custom_endpoint());
        assert!(!Backend::Bedrock.uses_custom_endpoint());
        assert!(Backend::Ollama.uses_custom_endpoint());
        assert!(Backend::LlamaCpp.uses_custom_endpoint());
        assert!(Backend::OpenAiCompatible.uses_custom_endpoint());
    }

    #[test]
    fn every_alias_var_is_reserved_and_managed() {
        for key in [OPUS, SONNET, HAIKU, FABLE] {
            assert!(is_reserved_env_key(key), "{} must be reserved", key);
            assert!(
                is_reserved_env_key(&key.to_lowercase()),
                "{} must be reserved case-insensitively",
                key
            );
        }
        // A user-set alias must never survive into the container env.
        let fp = compute_env_fingerprint(&[EnvVar {
            key: HAIKU.to_string(),
            value: "sneaky".to_string(),
        }]);
        assert_eq!(fp, "");
    }

    #[test]
    fn the_custom_env_fingerprint_never_carries_the_value() {
        // It goes into `triple-c.custom-env-fingerprint`, which `docker inspect`
        // hands to anything on the host, `docker commit` copies onto the
        // project's snapshot image, and the recreation check logs on a mismatch.
        let secret = "33da01c1b320644920c20d6b5e0a1c6b3c3451c2";
        let fp = compute_env_fingerprint(&[EnvVar {
            key: "TEA_TOKEN".to_string(),
            value: secret.to_string(),
        }]);
        assert!(!fp.contains(secret), "fingerprint leaked the value: {}", fp);
        assert!(!fp.contains("TEA_TOKEN"), "fingerprint leaked the key: {}", fp);
        assert_eq!(fp.len(), 64, "expected a sha256 hex digest, got {:?}", fp);

        // It still has to move when the value does, or a rotated token would
        // never reach the container.
        let rotated = compute_env_fingerprint(&[EnvVar {
            key: "TEA_TOKEN".to_string(),
            value: "rotated".to_string(),
        }]);
        assert_ne!(fp, rotated);
    }

    #[test]
    fn the_deprecated_small_fast_model_var_is_never_emitted() {
        let rendered: Vec<String> = aliases(Some("m"), Some("h"))
            .into_iter()
            .map(|(k, _)| k.to_string())
            .collect();
        assert!(!rendered.iter().any(|k| k == "ANTHROPIC_SMALL_FAST_MODEL"));
    }

    #[test]
    fn the_alias_fingerprint_tracks_both_the_model_and_the_override() {
        let base = model_alias_fingerprint_part(Some("m"), None);
        assert_eq!(base, model_alias_fingerprint_part(Some("m"), Some("")));
        assert_ne!(base, model_alias_fingerprint_part(Some("m2"), None));
        assert_ne!(base, model_alias_fingerprint_part(Some("m"), Some("h")));
        assert_eq!(model_alias_fingerprint_part(None, None), "");
    }

    fn project_with_llamacpp(model: Option<&str>, haiku: Option<&str>) -> Project {
        let mut p = Project::new("t".to_string(), Vec::new());
        p.backend = Backend::LlamaCpp;
        p.llamacpp_config = Some(crate::models::LlamaCppConfig {
            base_url: "http://host.docker.internal:8080".to_string(),
            model_id: model.map(str::to_string),
            haiku_model_id: haiku.map(str::to_string),
        });
        p
    }

    #[test]
    fn llamacpp_fingerprint_changes_when_the_haiku_override_changes() {
        let g = GlobalLlamaCppSettings::default();
        let a = compute_llamacpp_fingerprint(&project_with_llamacpp(Some("m"), None), &g);
        let b = compute_llamacpp_fingerprint(&project_with_llamacpp(Some("m"), Some("h")), &g);
        assert_ne!(a, b, "the haiku override must force a container recreation");

        // No config at all -> empty, so projects on other backends are not
        // flagged for recreation by this fingerprint.
        let plain = Project::new("t".to_string(), Vec::new());
        assert_eq!(compute_llamacpp_fingerprint(&plain, &g), "");
    }

    #[test]
    fn llamacpp_global_defaults_fill_in_for_blank_per_project_fields() {
        let g = GlobalLlamaCppSettings {
            base_url: Some("http://elsewhere:8080".to_string()),
            default_model_id: Some("global-model".to_string()),
            default_haiku_model_id: Some("global-haiku".to_string()),
        };
        // The per-project base URL is set in the fixture, so only the model and
        // haiku fields fall through to the globals. Filling them in from the
        // globals must be indistinguishable from setting them per-project.
        let with_global = compute_llamacpp_fingerprint(&project_with_llamacpp(None, None), &g);
        let explicit = compute_llamacpp_fingerprint(
            &project_with_llamacpp(Some("global-model"), Some("global-haiku")),
            &GlobalLlamaCppSettings::default(),
        );
        assert_eq!(with_global, explicit);
        // …and changing a global must change the fingerprint, so a global-only
        // edit still forces a recreation.
        assert_ne!(
            with_global,
            compute_llamacpp_fingerprint(
                &project_with_llamacpp(None, None),
                &GlobalLlamaCppSettings {
                    default_haiku_model_id: Some("other-haiku".to_string()),
                    ..g.clone()
                },
            )
        );
        assert_eq!(
            resolve_with_global(None, g.default_haiku_model_id.as_deref()),
            Some("global-haiku")
        );
        assert_eq!(
            resolve_with_global(Some("  "), g.default_model_id.as_deref()),
            Some("global-model")
        );
    }

    #[test]
    fn backend_serde_round_trips_llamacpp_and_accepts_legacy_spellings() {
        assert_eq!(
            serde_json::to_string(&Backend::LlamaCpp).unwrap(),
            "\"llama_cpp\""
        );
        for spelling in ["\"llama_cpp\"", "\"llamacpp\"", "\"llama-cpp\"", "\"llama.cpp\""] {
            let parsed: Backend = serde_json::from_str(spelling).unwrap();
            assert_eq!(parsed, Backend::LlamaCpp, "failed for {}", spelling);
        }
    }

    #[test]
    fn a_project_json_without_llamacpp_config_still_deserialises() {
        // `projects.json` written by an older build has no llamacpp_config key.
        let json = serde_json::json!({
            "id": "p1",
            "name": "old",
            "paths": [],
            "container_id": null,
            "status": "stopped",
            "backend": "ollama",
            "bedrock_config": null,
            "ollama_config": { "base_url": "http://x:11434", "model_id": "m" },
            "openai_compatible_config": null,
            "allow_docker_access": false,
            "created_at": "2026-01-01T00:00:00Z",
            "updated_at": "2026-01-01T00:00:00Z"
        });
        let p: Project = serde_json::from_value(json).unwrap();
        assert!(p.llamacpp_config.is_none());
        // The new per-backend haiku override also defaults cleanly.
        assert!(p.ollama_config.unwrap().haiku_model_id.is_none());
    }
    // ── Snapshot secret stripping ────────────────────────────────────────
    // The bug these cover: `docker commit` copies the container's whole
    // environment into `triple-c-snapshot-{id}:latest`, that image outlives
    // every container built from it, and revoking the shared token used to
    // touch only the keychain — so `docker image inspect` kept returning a
    // live ~1-year OAuth credential indefinitely.

    #[test]
    fn the_commit_override_blanks_every_credential_bearing_key() {
        let blanked = blanked_secret_env();
        assert_eq!(blanked.len(), SECRET_ENV_KEYS.len());
        for key in SECRET_ENV_KEYS {
            assert!(
                blanked.contains(&format!("{}=", key)),
                "{} is not blanked at commit time",
                key
            );
        }
        // Blanked, never omitted: the commit endpoint merges this over the
        // container's env key by key, so a name left out is inherited with its
        // original value.
        assert!(blanked.iter().all(|e| e.ends_with('=')));
    }

    #[test]
    fn the_shared_claude_token_is_one_of_the_stripped_keys() {
        assert!(SECRET_ENV_KEYS.contains(&CLAUDE_OAUTH_TOKEN_ENV));
    }

    #[test]
    fn an_image_holding_a_credential_is_detected() {
        let env = vec![
            "PATH=/usr/bin".to_string(),
            format!("{}=sk-ant-oat01-{}", CLAUDE_OAUTH_TOKEN_ENV, "x".repeat(90)),
        ];
        assert!(env_holds_a_secret(&env));
    }

    #[test]
    fn a_blanked_or_secret_free_image_is_left_alone() {
        // Already scrubbed.
        assert!(!env_holds_a_secret(&blanked_secret_env()));
        // Never had one.
        assert!(!env_holds_a_secret(&[
            "PATH=/usr/bin".to_string(),
            "TZ=UTC".to_string(),
            format!("{}=claude-sonnet-4-5", ANTHROPIC_DEFAULT_SONNET_MODEL),
        ]));
        // A non-secret var whose *name* merely contains a secret name.
        assert!(!env_holds_a_secret(&[
            format!("MY_{}=not-a-secret", CLAUDE_OAUTH_TOKEN_ENV)
        ]));
    }

    #[test]
    fn a_value_containing_an_equals_sign_is_still_recognised() {
        let env = vec!["ANTHROPIC_AUTH_TOKEN=abc=def==".to_string()];
        assert!(env_holds_a_secret(&env));
    }

    #[test]
    fn the_scrub_report_only_claims_success_when_nothing_is_left() {
        let clean = SnapshotScrubReport {
            scrubbed: vec!["triple-c-snapshot-a:latest".to_string()],
            ..Default::default()
        };
        assert!(!clean.left_something_behind());

        let partial = SnapshotScrubReport {
            failed: vec![("triple-c-snapshot-b:latest".to_string(), "nope".to_string())],
            ..Default::default()
        };
        assert!(partial.left_something_behind());

        let blind = SnapshotScrubReport {
            unavailable: Some("Docker is not running".to_string()),
            ..Default::default()
        };
        assert!(blind.left_something_behind());
    }
}
