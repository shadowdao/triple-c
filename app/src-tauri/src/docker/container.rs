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
/// The label every container Triple-C creates carries — and, because
/// `docker commit` copies a container's labels onto the image, every snapshot it
/// commits. [`sweep_orphaned_snapshots`] treats it as the mark of provenance,
/// which is what keeps the sweep away from the user's own images.
pub(crate) const LABEL_MANAGED: &str = "triple-c.managed";
/// Marks the throwaway container [`rewrite_image_without_secrets`] commits
/// from. It exists so the Disk panel's reclaim bucket can distinguish a live
/// credential rewrite from a leftover by label rather than by age.
pub(crate) const LABEL_SCRUB: &str = "triple-c.scrub";

/// Marks the image built from `container/Dockerfile` itself, as opposed to a
/// project snapshot committed from a container. Only ever `"true"` on a base
/// image; `create_container` writes it explicitly empty so an inherited value
/// cannot travel onto a snapshot. See the `LABEL` block in the Dockerfile.
pub(crate) const LABEL_BASE: &str = "triple-c.base";

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
    "CLAUDE_CODE_SETTINGS_CLEAR",
    "MISSION_CONTROL_ENABLED",
    "VPN_SUPPORT_ENABLED",
    "TRIPLE_C_PERMISSION_MODE",
    // The four env vars the Claude Code settings editor drives. Reserved for
    // the `VPN_SUPPORT_ENABLED` reason: each is now written on every create,
    // including its off value, so a hand-set custom var of the same name would
    // either be overridden without explanation or override the setting behind
    // the UI's back, depending on which one Docker kept.
    "CLAUDE_CODE_NO_FLICKER",
    "CLAUDE_CODE_ENABLE_AWAY_SUMMARY",
    "CLAUDE_CODE_SUBPROCESS_ENV_SCRUB",
    "ENABLE_PROMPT_CACHING_1H",
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

/// Merge global and per-project `ClaudeCodeSettings`.
///
/// A project field that is `Some` wins outright — **including `Some(false)`**.
/// That is the point of the widening: these used to be plain `bool`s ORed
/// together (`if p.x { true } else { g.x }`), so a project could only ever add
/// to the global set and never turn a globally-enabled setting off. `None` at
/// project level means "inherit", which is now a state the project can
/// actually be in rather than the only state an off switch could produce.
fn merge_claude_code_settings(
    global: Option<&ClaudeCodeSettings>,
    project: Option<&ClaudeCodeSettings>,
) -> Option<ClaudeCodeSettings> {
    match (global, project) {
        (None, None) => None,
        (Some(g), None) => Some(g.clone()),
        (None, Some(p)) => Some(p.clone()),
        (Some(g), Some(p)) => {
            Some(ClaudeCodeSettings {
                tui_mode: p.tui_mode.clone().or_else(|| g.tui_mode.clone()),
                effort: p.effort.clone().or_else(|| g.effort.clone()),
                auto_scroll_disabled: p.auto_scroll_disabled.or(g.auto_scroll_disabled),
                focus_mode: p.focus_mode.or(g.focus_mode),
                show_thinking_summaries: p.show_thinking_summaries.or(g.show_thinking_summaries),
                session_recap_disabled: p.session_recap_disabled.or(g.session_recap_disabled),
                env_scrub: p.env_scrub.or(g.env_scrub),
                prompt_caching_1h: p.prompt_caching_1h.or(g.prompt_caching_1h),
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
                // `{:?}` rather than `{}` so `None` and `Some(false)` produce
                // different text. They mean different things — inherit versus a
                // deliberate off — and a fingerprint that conflated them would
                // leave the container un-recreated on a real change.
                format!("{:?}", s.auto_scroll_disabled),
                format!("{:?}", s.focus_mode),
                format!("{:?}", s.show_thinking_summaries),
                format!("{:?}", s.session_recap_disabled),
                format!("{:?}", s.env_scrub),
                format!("{:?}", s.prompt_caching_1h),
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

/// The four Claude Code env vars the settings editor drives, as `KEY=VALUE`.
///
/// **All four are emitted on every create, including their off value.** This is
/// the `MANAGED_AUTH_KEYS` rule: `docker commit` bakes a container's env into
/// the snapshot image, and the next container inherits anything the create does
/// not override. A `=1` written once would ride that snapshot into every future
/// container and make the switch impossible to turn back off — the same
/// stickiness [`build_claude_code_settings_json`] fixes on the settings.json
/// side, in a place where it is even less visible.
///
/// Two of them use an **empty** value for "off", and the distinction matters:
///
/// * `CLAUDE_CODE_NO_FLICKER` documents `1` as fullscreen-on and `0` as
///   fullscreen-*off*, and it overrides the `tui` setting. `0` is therefore not
///   neutral — it would silently pin every project that has expressed no
///   preference to the classic renderer, when an unset `tui` is supposed to let
///   Claude Code choose. Empty is neither value, so it reads as unset while
///   still overriding a baked `1`.
/// * `CLAUDE_CODE_ENABLE_AWAY_SUMMARY` outranks both `awaySummaryEnabled` and
///   the in-container `/config` toggle. `0` is exactly right for "the user
///   turned the recap off in Triple-C", but a blanket `1` for the default state
///   would force the recap back on for someone who had turned it off with
///   `/config` inside their own container. Triple-C's default must not overrule
///   a choice it never asked about.
///
/// The other two are documented as "set to `1` to …" with no meaning attached
/// to `0`, so `0` is unambiguously neutral and is stated outright.
fn claude_code_env_vars(settings: Option<&ClaudeCodeSettings>) -> Vec<String> {
    let owned;
    let s = match settings {
        Some(s) => s,
        None => {
            owned = ClaudeCodeSettings::default();
            &owned
        }
    };

    vec![
        format!(
            "CLAUDE_CODE_NO_FLICKER={}",
            match s.tui_mode.as_deref() {
                Some("fullscreen") => "1",
                Some("default") => "0",
                _ => "",
            }
        ),
        format!(
            "CLAUDE_CODE_ENABLE_AWAY_SUMMARY={}",
            if s.session_recap_disabled.unwrap_or(false) { "0" } else { "" }
        ),
        format!(
            "CLAUDE_CODE_SUBPROCESS_ENV_SCRUB={}",
            if s.env_scrub.unwrap_or(false) { "1" } else { "0" }
        ),
        format!(
            "ENABLE_PROMPT_CACHING_1H={}",
            if s.prompt_caching_1h.unwrap_or(false) { "1" } else { "0" }
        ),
    ]
}

/// Build the settings.json payload for Claude Code, handed to the container as
/// `CLAUDE_CODE_SETTINGS_JSON` and applied by `entrypoint.sh`.
///
/// ## Every managed key is always present, and `null` means "delete"
///
/// The settings file lives on `triple-c-claude-config-{projectId}`, a named
/// volume that outlives the container, and the entrypoint *merges* into it. So
/// a key emitted only when it is non-default can be written once and never
/// taken back: turning the setting off simply omits the key, the merge
/// preserves whatever was there, and the setting stays on forever. Only a
/// destructive Reset — which also deletes the OAuth login, skills and
/// transcripts — ever cleared it. Four of the five keys here were sticky that
/// way; the `sandbox` block already carried the workaround and the comment
/// explaining it, and this is the same treatment applied to the rest.
///
/// Two shapes of "off" are needed, because Claude Code's own defaults differ:
///
/// * **A boolean with a documented default** (`autoScrollEnabled` is `true`,
///   `showThinkingSummaries` is `false`) is emitted with that neutral value.
/// * **A key whose neutral state is *unset*** (`tui`, `effortLevel`,
///   `viewMode`, `awaySummaryEnabled`) is emitted as JSON `null`, and the
///   entrypoint deletes rather than merges those. Writing a stand-in value
///   would not be neutral: an unset `tui` lets Claude Code choose the renderer
///   (`"default"` pins the classic one), and an unset `viewMode` lets the
///   user's own sticky `/focus` choice and `verbose` setting apply
///   (`"default"` overrides both).
///
/// Returns a `String` rather than an `Option<String>`: there is no longer any
/// input for which this produces nothing to say.
fn build_claude_code_settings_json(
    settings: Option<&ClaudeCodeSettings>,
    sandbox_enabled: bool,
) -> String {
    let owned;
    let s = match settings {
        Some(s) => s,
        // No struct at all is not "say nothing" — it is "every setting is at
        // its default", which still has to be asserted over a stale file.
        None => {
            owned = ClaudeCodeSettings::default();
            &owned
        }
    };

    let mut map = serde_json::Map::new();

    // `null` clears; see the module doc above.
    map.insert(
        "tui".to_string(),
        match s.tui_mode {
            Some(ref tui) => serde_json::json!(tui),
            None => serde_json::Value::Null,
        },
    );
    // `effortLevel`, not `effort`. Claude Code has never read a key called
    // `effort`, so the previous value was written and silently ignored.
    map.insert(
        "effortLevel".to_string(),
        match s.effort {
            Some(ref effort) => serde_json::json!(effort),
            None => serde_json::Value::Null,
        },
    );
    // Documented default `true`, so the neutral value is a value.
    map.insert(
        "autoScrollEnabled".to_string(),
        serde_json::json!(!s.auto_scroll_disabled.unwrap_or(false)),
    );
    // Documented default `false`.
    map.insert(
        "showThinkingSummaries".to_string(),
        serde_json::json!(s.show_thinking_summaries.unwrap_or(false)),
    );
    // `viewMode: "focus"` is the real setting behind what the UI calls focus
    // mode — "collapses tool output to one-line summaries" is that key's
    // documented behaviour. The `focusMode` key it replaces was invented and
    // did nothing.
    map.insert(
        "viewMode".to_string(),
        if s.focus_mode.unwrap_or(false) {
            serde_json::json!("focus")
        } else {
            serde_json::Value::Null
        },
    );
    // The recap is on by default, so only the *off* case has anything to write.
    // `CLAUDE_CODE_ENABLE_AWAY_SUMMARY` (set unconditionally at creation) takes
    // precedence over this key and is what actually enforces the choice; this
    // is here so the container's settings.json does not contradict it.
    map.insert(
        "awaySummaryEnabled".to_string(),
        if s.session_recap_disabled.unwrap_or(false) {
            serde_json::json!(false)
        } else {
            serde_json::Value::Null
        },
    );

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

    serde_json::Value::Object(map).to_string()
}

/// Split the managed payload into the half that is safe to merge anywhere and
/// the list of keys to delete.
///
/// The null-means-delete convention is only understood by the `entrypoint.sh`
/// shipped alongside this code — and an existing project recreates from *its
/// own snapshot image*, which carries whatever entrypoint it was built with. An
/// older one merges with a plain `.[0] * .[1]`, which would write the literal
/// `null`s straight into the user's `settings.json` rather than clearing the
/// keys. A settings file Claude Code then rejects would take the user's own
/// `model`, `statusLine` and everything else in it down with it.
///
/// So the nulls never leave Rust. `CLAUDE_CODE_SETTINGS_JSON` carries only real
/// values and stays safe under either merge; `CLAUDE_CODE_SETTINGS_CLEAR`
/// carries the key names to delete and is simply ignored by an entrypoint that
/// predates it. Such a project keeps the old sticky behaviour until it is
/// migrated or Reset — which is the pre-existing state, not a regression.
pub(crate) fn split_claude_code_settings_payload(payload: &str) -> (String, String) {
    let parsed: serde_json::Value = match serde_json::from_str(payload) {
        Ok(v) => v,
        // Not our business to fix; hand it through and let the merge fail loudly.
        Err(_) => return (payload.to_string(), "[]".to_string()),
    };
    let Some(obj) = parsed.as_object() else {
        return (payload.to_string(), "[]".to_string());
    };

    let mut set = serde_json::Map::new();
    let mut clear: Vec<serde_json::Value> = Vec::new();
    for (k, v) in obj {
        if v.is_null() {
            clear.push(serde_json::json!(k));
        } else {
            set.insert(k.clone(), v.clone());
        }
    }

    (
        serde_json::Value::Object(set).to_string(),
        serde_json::Value::Array(clear).to_string(),
    )
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

/// The `/dev/net/tun` character device, as it is named on both sides.
const TUN_DEVICE: &str = "/dev/net/tun";

/// The `HostConfig` fields "VPN support" contributes: `CapAdd`, `Devices`,
/// `Sysctls` — in that order.
type VpnHostConfigParts = (
    Option<Vec<String>>,
    Option<Vec<bollard::models::DeviceMapping>>,
    Option<HashMap<String, String>>,
);

/// The three host-config pieces a VPN client needs, or all-`None` when the
/// project has not opted in.
///
/// Returned as a triple rather than set inline so the exact shape is unit
/// testable — a container is created once, by a very long async function, and a
/// silently-dropped capability looks identical to a VPN server that is simply
/// unreachable.
///
/// All three are required together and each fails differently on its own:
/// * **`CAP_NET_ADMIN`** — without it the client cannot create an interface or
///   write a route. Docker's default bounding set grants `net_raw` but not
///   `net_admin`, which is why a client can ping but never connect.
/// * **`/dev/net/tun`** — the device is absent from a default container, so
///   there is nothing to open even with the capability. It is passed through
///   from the host rather than `mknod`-ed inside, so the kernel's `tun` module
///   backs it.
/// * **`net.ipv4.conf.all.src_valid_mark`** — WireGuard's own `wg-quick` sets
///   this, and cannot from inside a container (`/proc/sys` is read-only), so
///   its handshake packets are dropped by reverse-path filtering. Harmless for
///   OpenVPN-based clients, so it is set unconditionally with the rest.
///
/// What it costs, stated accurately: Docker does not enable user-namespace
/// remapping by default, so this is a real `CAP_NET_ADMIN` in the *initial*
/// user namespace and only the **network** namespace confines it. It cannot
/// touch the host's interfaces, but within its own namespace it can set
/// promiscuous mode and add arbitrary addresses, routes and NAT rules on the
/// shared `docker0` L2 segment — which puts sibling containers (the LiteLLM
/// gateway among them) within reach of ARP spoofing, and lets netlink trigger
/// host-kernel module auto-loading. It is also enough to flush netfilter rules
/// inside the container, so pair it with `sandbox_mode_enabled` advisedly.
/// Hence opt-in, per project, rather than on for everyone.
/// The env var `entrypoint.sh` installs and removes the `pia-vpn` skill from.
///
/// **Emitted either way, never omitted.** `~/.claude` is a persisted volume, so
/// turning the toggle off has to actively tell entrypoint to remove a skill an
/// earlier run left there, and an absent variable cannot say that. It is also
/// what stops a `=1` baked into a snapshot by `docker commit` from outliving
/// the setting — the explicit `=0` overwrites it.
///
/// Extracted for the same reason as [`vpn_host_config`]: the emitting code sits
/// in a very long function where a dropped or inverted value is invisible, and
/// `MISSION_CONTROL_ENABLED` twenty lines above shows the failure this avoids —
/// it is pushed only when true, so a snapshot's baked `=1` survives the toggle
/// going off.
fn vpn_env_var(enabled: bool) -> String {
    format!("VPN_SUPPORT_ENABLED={}", u8::from(enabled))
}

fn vpn_host_config(enabled: bool) -> VpnHostConfigParts {
    if !enabled {
        return (None, None, None);
    }

    let devices = vec![bollard::models::DeviceMapping {
        path_on_host: Some(TUN_DEVICE.to_string()),
        path_in_container: Some(TUN_DEVICE.to_string()),
        cgroup_permissions: Some("rwm".to_string()),
    }];

    let sysctls = HashMap::from([(
        "net.ipv4.conf.all.src_valid_mark".to_string(),
        "1".to_string(),
    )]);

    (
        Some(vec!["NET_ADMIN".to_string()]),
        Some(devices),
        Some(sysctls),
    )
}

/// Turn the daemon's device-passthrough failure into an explanation.
///
/// **This fires on `start`, not `create`.** Verified against Docker 29.7:
/// `docker create --device /dev/does-not-exist` succeeds and prints an id; the
/// device is only resolved when runc builds the container, so the failure lands
/// on the *next* call. Sysctls validate at the same point. Anything that
/// inspects only the create path will never see it — which is why both paths
/// route through here and the tests exercise the start-side string.
///
/// Unmapped, this reads as `Failed to start container: Docker responded with
/// status code 500: error gathering device information while adding custom
/// device "/dev/net/tun": no such file or directory` — a path the user will go
/// looking for on the wrong machine, since with Docker Desktop the relevant
/// host is the Linux VM rather than their own, and with nothing pointing back
/// at the switch that caused it.
///
/// Deliberately not gated on `vpn_support_enabled`: nothing else in Triple-C
/// ever asks for a device, so an error naming `/dev/net/tun` can only have come
/// from a container created with the switch on. That keeps the check usable
/// from [`start_container`], which has a container id and no project.
fn explain_container_failure(action: &str, err: &str) -> String {
    let device_missing = err.contains(TUN_DEVICE)
        && (err.contains("no such file or directory")
            || err.contains("No such file or directory")
            || err.contains("error gathering device information"));

    if device_missing {
        return format!(
            "Failed to {} container: the Docker host has no {} device, which \
             \"VPN support\" requires. The host kernel needs the `tun` module \
             loaded (on Docker Desktop that is the Linux VM, not your own \
             machine). Turn VPN support off in Config → Runtime to start this \
             project without it. Original error: {}",
            action, TUN_DEVICE, err
        );
    }

    format!("Failed to {} container: {}", action, err)
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

    env_vars.push(vpn_env_var(project.vpn_support_enabled));

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
    // Env-var-based settings, read directly by Claude Code. Extracted and unit
    // tested for the `vpn_host_config` reason: a container is created once, by
    // a very long function, and a variable emitted with the wrong value here is
    // invisible until someone wonders why a switch does nothing.
    env_vars.extend(claude_code_env_vars(merged_cc_settings.as_ref()));

    // settings.json-based settings (applied by the entrypoint). Always emitted,
    // even with no `ClaudeCodeSettings` struct present: the payload asserts the
    // *whole* managed key set, so "no settings" still has to be stated over a
    // settings.json left behind on the config volume by a previous config.
    // Split so the payload is safe under an older entrypoint that has never
    // heard of the null-means-delete convention — see
    // `split_claude_code_settings_payload`.
    let (cc_settings_set, cc_settings_clear) = split_claude_code_settings_payload(
        &build_claude_code_settings_json(merged_cc_settings.as_ref(), project.sandbox_mode_enabled),
    );
    env_vars.push(format!("CLAUDE_CODE_SETTINGS_JSON={}", cc_settings_set));
    env_vars.push(format!("CLAUDE_CODE_SETTINGS_CLEAR={}", cc_settings_clear));

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
        source: Some(home_volume_name(&project.id)),
        typ: Some(MountTypeEnum::VOLUME),
        read_only: Some(false),
        ..Default::default()
    });

    // Named volume for claude config persistence — mounted as a nested volume
    // inside the home volume; Docker gives the more-specific mount precedence.
    mounts.push(Mount {
        target: Some("/home/claude/.claude".to_string()),
        source: Some(config_volume_name(&project.id)),
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
    labels.insert(LABEL_MANAGED.to_string(), "true".to_string());
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
    // Capabilities, devices and sysctls are fixed at creation, so this is
    // container state and gets the label-and-compare treatment. Written
    // unconditionally (`false`, not omitted) because `docker commit` copies
    // container labels onto the snapshot image: a `true` stamped once would
    // otherwise ride that snapshot into every future container and make the
    // switch impossible to turn back off.
    labels.insert("triple-c.vpn-support".to_string(), project.vpn_support_enabled.to_string());
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

    // Same defence, for the label `container/Dockerfile` now stamps on the base
    // image. Docker merges an image's labels into the container it creates, and
    // `docker commit` copies the container's labels onto the snapshot — so
    // without this line every project snapshot would inherit
    // `triple-c.base=true` from the base it descends from and claim to *be* a
    // base image. Writing it explicitly empty overrides the inherited value.
    labels.insert(LABEL_BASE.to_string(), String::new());

    for (key, value) in extras.extra_labels {
        labels.insert((*key).to_string(), (*value).to_string());
    }

    let (cap_add, devices, sysctls) = vpn_host_config(project.vpn_support_enabled);

    let host_config = HostConfig {
        mounts: Some(mounts),
        port_bindings: if port_bindings.is_empty() { None } else { Some(port_bindings) },
        init: Some(true),
        log_config: Some(capped_log_config()),
        cap_add,
        devices,
        sysctls,
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
        .map_err(|e| explain_container_failure("create", &e.to_string()))?;

    Ok(response.id)
}

/// The rotation policy every Triple-C container is created with.
///
/// Without this a container inherits the daemon's `json-file` default, which
/// has **no size limit at all** — `docker logs` for a container that has been
/// up for weeks is a single file that grows until the disk does not have room
/// for it. Triple-C containers are long-lived by design (stop/start, not
/// create/destroy), and the entrypoint plus anything a session leaves running
/// on stdout all land in that one file.
///
/// 10 MiB × 3 keeps roughly the last 30 MiB, which is far more scrollback than
/// anything reads, and bounds the worst case at 30 MiB per project instead of
/// unbounded.
///
/// **Deliberately not part of `container_needs_recreation`.** That check is
/// label-based, so participating would mean a new `triple-c.*` label whose only
/// effect is to recreate every existing project once — and a recreation costs a
/// `docker commit`, i.e. a permanent multi-gigabyte layer, which is the very
/// thing this whole change set exists to avoid. Containers pick the policy up
/// on their next natural recreation instead; an existing container keeps its
/// unbounded log until then, which is exactly the status quo.
fn capped_log_config() -> bollard::models::HostConfigLogConfig {
    bollard::models::HostConfigLogConfig {
        typ: Some("json-file".to_string()),
        config: Some(HashMap::from([
            ("max-size".to_string(), "10m".to_string()),
            ("max-file".to_string(), "3".to_string()),
        ])),
    }
}

pub async fn start_container(container_id: &str) -> Result<(), String> {
    let docker = get_docker()?;
    docker
        .start_container(container_id, None::<StartContainerOptions<String>>)
        .await
        .map_err(|e| explain_container_failure("start", &e.to_string()))
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

/// Name of the named volume mounted at `/home/claude`.
///
/// Takes the id rather than the `Project` because the disk view runs this
/// mapping backwards: it reads volume names off the daemon and has to decide
/// which project — if any — each one belongs to. See [`HOME_VOLUME_PREFIX`].
pub fn home_volume_name(project_id: &str) -> String {
    format!("{}{}", HOME_VOLUME_PREFIX, project_id)
}

/// Name of the named volume mounted at `/home/claude/.claude`, nested inside
/// the home volume. This is the one holding the OAuth credential, the plugins
/// and every session transcript.
pub fn config_volume_name(project_id: &str) -> String {
    format!("{}{}", CONFIG_VOLUME_PREFIX, project_id)
}

/// Prefix of [`home_volume_name`]. Split out because orphan detection scans the
/// daemon's volume list for these prefixes and strips them back to a project id.
pub const HOME_VOLUME_PREFIX: &str = "triple-c-home-";

/// Prefix of [`config_volume_name`]. See [`HOME_VOLUME_PREFIX`].
pub const CONFIG_VOLUME_PREFIX: &str = "triple-c-claude-config-";

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

/// Paths deleted from a container's writable layer immediately before
/// [`commit_container_snapshot`] runs.
///
/// ## Why this exists
///
/// Every recreation commits the container, and a commit **stacks a new layer**
/// on top of the previous snapshot — it never rewrites one. Deleting a file
/// after it has been committed does not give the bytes back; it writes a
/// whiteout entry and the original bytes stay in the layer below, forever. One
/// project was measured carrying 14 stacked commit layers and ~5.1 GB above its
/// base image, and 24 different conditions trigger a recreation, so changing a
/// single settings field costs a multi-gigabyte layer that nothing can reclaim.
///
/// The only moment the bytes are still free to drop is *before* the commit that
/// captures them. Measured on one container's 4.48 GB pending writable layer:
/// 3.0 GB of agent scratchpad under `/tmp/claude-*`, the drag-and-drop staging
/// area (up to 256 MiB per dropped file, and nothing in the app ever removes
/// one), one PNG per pasted image, and the apt lists/cache/logs left by every
/// runtime `apt-get install` the browser-view installer and the Playwright
/// healer run — none of which has an `apt-get clean` behind it.
///
/// ## Why a hardcoded list and not a heuristic
///
/// A snapshot is the user's system layer: their packages, their `/opt`, their
/// `/var/lib/postgresql`. Nothing here may guess. Every entry is an absolute
/// path anchored to a directory Triple-C or a package manager owns:
///
/// * `/workspace/{mount_name}` subtrees are **host bind mounts** — the user's
///   real project directories. No entry may ever reach one, which is why no
///   pattern here starts with `/workspace`.
/// * The only bind mounts under `/tmp` are `/tmp/.host-ca` and `/tmp/.host-aws`
///   (both read-only). A leading-dot name is not matched by a shell glob, and
///   none of these patterns share their prefix.
/// * The apt entries keep their parent directory and remove only its contents
///   (`lists/*`, `archives/*.deb`, `apt/*`); `apt-get` is unhappy when the
///   directories themselves are missing.
///
/// ## Why a safe-looking *pattern* is not enough (C1)
///
/// The list used to be the whole of the defence, and that was wrong. These
/// patterns are expanded by `/bin/sh` inside the container running as **root**,
/// and for an entry ending `/*` the parent is a *path component*: both the glob
/// expansion and the `rm -rf` that follows resolve it. The agent in the
/// container has passwordless sudo, so anything able to run
/// `ln -s /workspace/myproject /var/log/apt` turns the next commit into a
/// recursive delete of the user's real files **on the host**. That was
/// reproduced end to end against a live container — a bind mount emptied by a
/// scrub whose path list named no `/workspace` anywhere.
///
/// (The entries whose glob is in the last component are the benign case: there
/// the *match* is the symlink, and `rm -rf -- link` unlinks the link and stops.
/// The distinction is not one a reviewer should have to make per entry, so the
/// script defends both alike.)
///
/// So these entries are only half the contract. The other half is
/// [`snapshot_scrub_script`], which validates the parent directory — not
/// reached through a symlink, not on another filesystem, inside
/// [`SCRUB_CONTAINMENT_PREFIXES`] — before it deletes anything inside it. That
/// is also why every entry here must keep its glob in the **final component**:
/// the parent has to be literal for the script to be able to check it at all. A
/// test enforces it.
///
/// A unit test also pins the list itself, because the blast radius of a wrong
/// entry here is a user's data and the code that consumes it is a shell string.
pub(crate) const SNAPSHOT_SCRUB_PATHS: &[&str] = &[
    // Agent scratchpads. The user's global CLAUDE.md instructs every agent to
    // put temporary files under a scratchpad directory in /tmp, so this is
    // where a long-running project's writable layer actually goes.
    //
    // Not age-limited, and worth being explicit about why that is only *just*
    // safe: the scrub runs as root against a container that is still running,
    // so a live Claude Code session or a `triple-c-scheduler` task writing here
    // has its scratchpad pulled out from under it mid-write. Both callers stop
    // the container within a line or two — `start_project_container` in
    // `project_commands.rs` stops and removes it, `migrate_project_to_base`
    // stops it — so the process that would notice is about to be killed anyway.
    // That is a property of those two call sites and not of this entry: a third
    // caller scrubbing a container it means to keep running would be corrupting
    // a live session, and would need to age-limit this the way the two below
    // are.
    "/tmp/claude-*",
    // Files drag-dropped into a terminal, staged by
    // `commands/terminal_commands.rs` at up to 256 MiB each, and one PNG per
    // pasted image from the same module. Nothing else in the repo deletes
    // either, so leaving them out of this list restores unbounded growth — but
    // they are the user's *own* files, and often the only copy inside the
    // container of something handed to the agent seconds ago by a path that is
    // still sitting in the conversation. Scrubbing them unconditionally meant
    // "drop a file, change any of the 24 settings that trigger a recreation,
    // lose it silently". Both are therefore age-limited rather than removed —
    // see [`SCRUB_MIN_AGE_DAYS`].
    "/tmp/triple-c-drops/*",
    "/tmp/clipboard_*.png",
    // Runtime apt debris. `browser_view/install.rs` and
    // `container/triple-c-playwright-heal` both run `apt-get install` inside a
    // live container without an `apt-get clean` after it.
    "/var/lib/apt/lists/*",
    "/var/cache/apt/archives/*.deb",
    "/var/log/apt/*",
    "/var/log/dpkg.log",
];

/// Entries of [`SNAPSHOT_SCRUB_PATHS`] that are only deleted once a match's own
/// mtime is older than the given number of days. Anything not named here is
/// deleted whenever it is present.
///
/// Two weeks is chosen against the thing that goes wrong: a recreation can
/// happen seconds after a drop, and no conversation is still quoting a path it
/// was given a fortnight ago. It is a compromise, not a proof — the alternative
/// of dropping these patterns entirely would be silent unbounded growth in a
/// directory nothing else ever cleans.
const SCRUB_MIN_AGE_DAYS: &[(&str, u32)] = &[
    ("/tmp/triple-c-drops/*", 14),
    ("/tmp/clipboard_*.png", 14),
];

/// The only directory trees [`snapshot_scrub_script`] will operate in, checked
/// against the *resolved* parent directory at run time inside the container.
///
/// Deliberately **not** derived from [`SNAPSHOT_SCRUB_PATHS`]: it is the
/// backstop for the case where that list is itself wrong. An entry added under
/// `/workspace`, `/home/claude` or `/etc` fails this check and deletes nothing,
/// however plausible it looked in review.
const SCRUB_CONTAINMENT_PREFIXES: &[&str] = &["/tmp", "/var/log", "/var/lib/apt", "/var/cache/apt"];

/// Marker the scrub script prints so the byte total can be read back out of the
/// exec's interleaved stdout/stderr.
const SCRUB_MARKER: &str = "###TRIPLE-C-SCRUBBED ";

/// Split a [`SNAPSHOT_SCRUB_PATHS`] entry into the literal directory it is
/// anchored to and the glob to expand inside it.
///
/// `None` for anything that is not an absolute path with a non-empty final
/// component — which a test rules out, but the script generator must not have
/// to trust that, since what it emits runs as root.
fn split_scrub_pattern(pattern: &str) -> Option<(&str, &str)> {
    let (dir, glob) = pattern.rsplit_once('/')?;
    if !pattern.starts_with('/') || glob.is_empty() {
        return None;
    }
    // A top-level entry such as `/dpkg.log` leaves an empty parent; anchor it.
    Some((if dir.is_empty() { "/" } else { dir }, glob))
}

/// The `/bin/sh` program run inside the container to perform the scrub.
pub(crate) fn snapshot_scrub_script() -> String {
    snapshot_scrub_script_under("")
}

/// [`snapshot_scrub_script`] with every absolute path re-anchored under `root`.
///
/// ## What the script does
///
/// One `scrub_in <pattern> <min-age-days, or `-`>` call per entry in
/// [`SNAPSHOT_SCRUB_PATHS`], with the pattern single-quoted so it reaches the
/// function unexpanded — unquoted, `/bin/sh` would expand the glob against the
/// script's own working directory before `scrub_in` ever ran. The function
/// splits it into parent and glob (`${1%/*}` / `${1##*/}`, which is why the
/// glob must live in the final component), and expands the glob only once the
/// working directory *is* the validated parent. Quoted everywhere it is used,
/// so a filename containing whitespace stays one argument; an unmatched glob
/// expands to itself and is skipped by the existence guard.
///
/// Passing the whole pattern rather than the two halves also keeps each entry
/// readable verbatim in the compaction `RUN` line — `disk.rs` asserts exactly
/// that, to catch a second forked copy of the list.
///
/// ## The containment guarantee (C1)
///
/// `scrub_in` is the only place in the script that deletes anything, and it
/// does so only after five checks. The first four are about the parent
/// directory, in order:
///
/// 0. `cd -P` into the parent **first**. Everything after that is relative to
///    the inode that gets validated, so re-pointing the *path* afterwards
///    cannot redirect the `rm`. A working directory is the only TOCTOU-free
///    handle `/bin/sh` offers.
/// 1. `pwd -P` — the fully resolved path — must equal what was asked for.
///    A symlinked component anywhere in the parent lands somewhere else, and
///    this is what catches `ln -s /workspace/myproject /var/log/apt`.
/// 2. The resolved path must be inside [`SCRUB_CONTAINMENT_PREFIXES`]. A
///    symlink is not the only way to name the wrong directory; this refuses to
///    operate outside the trees the scrub owns whatever the path list says.
/// 3. It must be on the same filesystem as the root. Check 1 does not see a
///    *bind mount*, which is not a symlink — but a bind mount and a named
///    volume each have a different `st_dev` from the overlay, and neither is
///    part of the writable layer the commit is about to capture. So anything on
///    another device is both dangerous to delete and pointless to.
///
/// The fifth is about each *match*, and it is the one the parent checks cannot
/// stand in for:
///
/// 4. Every expansion of the glob must itself be on the root's filesystem, or
///    it is skipped. Since the parent has already been proved to be, this is
///    exactly a "the match is not a mount point" test — the device of a
///    directory differs from its parent's precisely when something is mounted
///    on it.
///
///    Checks 0–3 validate the *parent*, and `rm --one-file-system` compares
///    against the device of **its own command-line argument** — so when the
///    argument *is* the mount root, `rm` is inside the mount already and
///    deletes everything under it. Verified in real containers against
///    `docker run -v vol:/tmp/claude-x`: mounted at the parent (`/var/log/apt`)
///    was refused, mounted one level below the match
///    (`/tmp/claude-x/inner`) was refused, and mounted *as* the match
///    (`/tmp/claude-x`) emptied the volume. In production that mount is the
///    user's own project directory, because a mount name of `../tmp/claude-x`
///    reaches it: the daemon normalises the `/workspace/{mount_name}` target
///    built at [`create_container`] straight back to `/tmp/claude-x`.
///
///    `stat` is not asked to dereference: a *symlinked* match reports the
///    link's own device, so it still passes and `rm -rf -- link` unlinks it and
///    stops — which is the behaviour the entries whose glob is in the last
///    component have always relied on.
///
/// Inside the loop, an expansion carrying a path separator means the list has
/// changed shape and is skipped, and `rm --one-file-system` (probed, because
/// busybox's `rm` would reject it) refuses to recurse across a mount planted
/// *below* a match — which is the only mount position it does cover.
///
/// ## Why the tools are named absolutely
///
/// Every external tool is called by absolute path, and `PATH` is reset before
/// anything runs. The image's `PATH` starts
/// `/home/claude/.claude/bin:/home/claude/.local/bin:/home/claude/.cargo/bin`,
/// all of which live in the container's *persisted home volume* and are
/// writable by the agent — so a three-line `stat` on the front of it decides
/// the answer to checks 3 and 4. That was reproduced: with such a shim planted,
/// a volume mounted at `/var/log/apt` — refused by an unshimmed scrub — had its
/// entire contents deleted. A *missing* `stat` does fail closed (an empty
/// device string matches nothing), but a shimmed one answers, so failing closed
/// was never the property that mattered here. `auth_bridge` calls
/// `/usr/bin/cat` for the same reason.
///
/// This does not defend against something that can write `/usr/bin` itself,
/// which the agent's passwordless sudo can. It closes the part of the gap that
/// survives a container restart and needs no privileges at all.
///
/// ## Why every line ends in `;`
///
/// `disk.rs` folds this script onto a single `RUN` line for the compaction
/// build, joining non-blank lines with a space. That is only a join and not a
/// rewrite if each line already terminates its own statement — the previous
/// version did not, and its folded form was a `"do" unexpected` syntax error,
/// so compaction had been running no scrub at all. It also means the script
/// carries **no `#` comments**: folded, one would swallow the rest of the
/// program. A test pins both the multi-line and the folded form.
///
/// ## Why `root` exists
///
/// `""` in production, which leaves the paths exactly as written. A non-empty
/// root lets the real generated script be run by a real `/bin/sh`, with real
/// symlinks planted in it, inside a throwaway directory tree — because
/// containment is a *runtime* property and the test this replaces
/// (`!dockerfile.contains("/workspace")`) was a substring check over the script
/// text that the exploitable version passed.
///
/// The tool paths are re-anchored with everything else, so a test root can put
/// its own `usr/bin/stat` where the script will look. That is how check 4 is
/// exercised without a mount: no unprivileged test can create a device
/// boundary (this container cannot even `unshare -Urm`), but a `stat` that
/// reports a foreign device for one directory is what the kernel would report
/// for a mount point, and the real boundary is covered by the container runs
/// recorded above.
fn snapshot_scrub_script_under(root: &str) -> String {
    let calls: String = SNAPSHOT_SCRUB_PATHS
        .iter()
        .filter_map(|pattern| {
            // Validate the shape here even though the shell re-derives it: an
            // entry the script could not split is one it must not be handed.
            split_scrub_pattern(pattern)?;
            let age = SCRUB_MIN_AGE_DAYS
                .iter()
                .find(|(p, _)| p == pattern)
                .map(|(_, days)| days.to_string())
                .unwrap_or_else(|| "-".to_string());
            Some(format!("scrub_in '{root}{pattern}' '{age}';\n"))
        })
        .collect();

    let allowed: String = SCRUB_CONTAINMENT_PREFIXES
        .iter()
        .map(|p| format!("{root}{p}|{root}{p}/*"))
        .collect::<Vec<_>>()
        .join("|");

    format!(
        r#"PATH={root}/usr/bin:{root}/bin; export PATH;
total=0;
rootdev=$({root}/usr/bin/stat -c %d '{root}/' 2>/dev/null);
rmopt=;
{root}/usr/bin/rm --one-file-system --help >/dev/null 2>&1 && rmopt=--one-file-system;
scrub_in() {{
n=$(
d=${{1%/*}}; [ -n "$d" ] || d=/;
g=${{1##*/}};
cd -P "$d" 2>/dev/null || exit 0;
[ "$(pwd -P)" = "$d" ] || exit 0;
case "$d" in {allowed}) ;; *) exit 0 ;; esac;
[ -n "$rootdev" ] || exit 0;
[ "$({root}/usr/bin/stat -c %d . 2>/dev/null)" = "$rootdev" ] || exit 0;
acc=0;
for p in $g; do
case "$p" in */*|.|..) continue ;; esac;
{{ [ -e "$p" ] || [ -L "$p" ]; }} || continue;
[ "$({root}/usr/bin/stat -c %d -- "$p" 2>/dev/null)" = "$rootdev" ] || continue;
[ "$2" = "-" ] || [ -n "$({root}/usr/bin/find "./$p" -maxdepth 0 -mtime +"$2" -print 2>/dev/null)" ] || continue;
sz=$({root}/usr/bin/du -sb -- "$p" 2>/dev/null | {root}/usr/bin/cut -f1);
case "$sz" in ''|*[!0-9]*) sz=0 ;; esac;
{root}/usr/bin/rm -rf $rmopt -- "$p" 2>/dev/null && acc=$((acc + sz));
done;
echo "$acc";
);
case "$n" in ''|*[!0-9]*) n=0 ;; esac;
total=$((total + n));
}};
{calls}echo "{marker}$total";
exit 0
"#,
        root = root,
        allowed = allowed,
        calls = calls,
        marker = SCRUB_MARKER,
    )
}

/// Parse the byte total the scrub script reports. Returns `None` when the
/// marker is absent, which is how a container that never ran the script (or a
/// `sh` that died early) is told apart from one that reclaimed nothing.
fn parse_scrub_total(output: &str) -> Option<u64> {
    output
        .lines()
        .rev()
        .find_map(|line| line.trim().strip_prefix(SCRUB_MARKER)?.trim().parse().ok())
}

/// How long [`scrub_writable_layer`] waits for its exec before the commit goes
/// ahead without it.
///
/// The scrub is a `du -sb` plus an `rm -rf` over a tree a running agent may
/// still be writing to, and it sits on the critical path of every recreate and
/// every migration with the UI parked on "Saving container state…" and no way
/// to cancel. Two minutes is an order of magnitude more than the measured cost
/// on a 4.48 GB layer, and far less than the point where a user force-quits.
const SCRUB_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

/// What [`scrub_writable_layer`] actually did.
///
/// The distinction that earns this type is [`ScrubOutcome::NotRunning`] versus
/// [`ScrubOutcome::Failed`]: "there was nothing to exec into" is routine, while
/// "the exec ran and broke" is the one case worth a warning in the log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScrubOutcome {
    /// The scrub ran to completion and freed this many bytes — possibly zero,
    /// which is a real answer.
    Reclaimed(u64),
    /// The container is not running, so there is no `docker exec` to run in.
    /// Not a failure: a stopped container's writable layer is not growing, and
    /// on the migration path it was already scrubbed while it was.
    NotRunning,
    /// The container was running, but the scrub did not finish within
    /// [`SCRUB_TIMEOUT`].
    TimedOut,
    /// The container was running and the scrub genuinely failed.
    Failed,
}

impl ScrubOutcome {
    /// What the commit's log line should say about the scrub — **empty when
    /// there is nothing to say**.
    ///
    /// Every migration used to log "0.00 MB dropped by the pre-commit scrub",
    /// because the migration path scrubs the container itself while it is
    /// still running and stops it before committing, so the call here finds
    /// nothing to exec into. A figure of zero reads as a scrub that ran and
    /// found nothing, which is a different and more alarming thing than a
    /// scrub that correctly had no work left.
    fn commit_log_suffix(&self) -> String {
        match self {
            Self::Reclaimed(bytes) => format!(
                " ({:.2} MB dropped by the pre-commit scrub)",
                *bytes as f64 / 1_048_576.0
            ),
            Self::NotRunning => String::new(),
            Self::TimedOut => " (the pre-commit scrub timed out, so the layer keeps whatever it had)".to_string(),
            Self::Failed => " (the pre-commit scrub did not run, so the layer keeps whatever it had)".to_string(),
        }
    }
}

/// Delete the throwaway files listed in [`SNAPSHOT_SCRUB_PATHS`] from a
/// container's writable layer so the commit that follows does not bake them in.
///
/// Runs as **root**: the apt debris is root-owned while the scratchpads belong
/// to `claude`, and root can remove both. That is also what makes
/// [`snapshot_scrub_script`]'s containment checks load-bearing rather than
/// decorative.
///
/// **Never fails the caller, by design.** A scrub is an optimisation; a commit
/// is the only copy of the user's system layer. Losing some disk is a strictly
/// better outcome than refusing to snapshot, so every failure here is a log
/// line and nothing more — including the timeout, which stops waiting and lets
/// the commit proceed rather than leaving the UI on "Saving container state…"
/// with no way out.
///
/// ## Why it checks whether the container is running (M11)
///
/// This is a `docker exec`, so it only works on a running container.
/// `migrate_project_to_base` knows that: it scrubs the container itself and
/// *then* stops it, because the pre-swap commit is the largest snapshot
/// Triple-C ever takes. The call inside [`commit_container_snapshot`] therefore
/// arrives at a stopped container and could only ever fail, which logged
/// "could not run … committing anyway" on every single migration — training
/// the reader to ignore the one line that would matter if the scrub had really
/// broken. Asking first turns that into a debug line and keeps the warning
/// meaningful.
pub async fn scrub_writable_layer(container_id: &str) -> ScrubOutcome {
    match is_container_running(container_id).await {
        Ok(true) => {}
        Ok(false) => {
            log::debug!(
                "Skipping pre-commit scrub of container {}: it is not running, so there is nothing to exec into and its writable layer is not growing",
                container_id
            );
            return ScrubOutcome::NotRunning;
        }
        Err(e) => {
            // Only Docker being unreachable reaches here, in which case the
            // commit this precedes is about to fail on its own — but say so
            // rather than reporting a skip that never happened.
            log::warn!(
                "Could not tell whether container {} is running ({}); skipping the pre-commit scrub and committing anyway",
                container_id,
                e
            );
            return ScrubOutcome::Failed;
        }
    }

    let script = snapshot_scrub_script();
    let cmd = vec!["/bin/sh".to_string(), "-c".to_string(), script];
    let exec = crate::docker::exec::exec_oneshot_as(container_id, "root", cmd, Vec::new());

    // M12: nothing under `docker/` has a timeout, and this is the one call on
    // the critical path of every recreate. Dropping the future stops us
    // *waiting*; the `sh` keeps running inside the container and its deletions
    // are still valid, they just stop counting towards the total. That is the
    // right trade — the commit that follows is merely a little larger.
    let exec_result = match tokio::time::timeout(SCRUB_TIMEOUT, exec).await {
        Ok(result) => result,
        Err(_) => {
            log::warn!(
                "Pre-commit scrub of container {} did not finish within {}s; committing anyway",
                container_id,
                SCRUB_TIMEOUT.as_secs()
            );
            return ScrubOutcome::TimedOut;
        }
    };

    match exec_result {
        Ok((output, _exit_code)) => match parse_scrub_total(&output) {
            Some(bytes) => {
                if bytes > 0 {
                    log::info!(
                        "Pre-commit scrub of container {} reclaimed {:.2} MB before it could be committed",
                        container_id,
                        bytes as f64 / 1_048_576.0
                    );
                }
                ScrubOutcome::Reclaimed(bytes)
            }
            None => {
                log::warn!(
                    "Pre-commit scrub of container {} did not report a total; committing anyway. Output: {}",
                    container_id,
                    output.trim()
                );
                ScrubOutcome::Failed
            }
        },
        Err(e) => {
            log::warn!(
                "Pre-commit scrub of container {} could not run ({}); committing anyway",
                container_id,
                e
            );
            ScrubOutcome::Failed
        }
    }
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
///
/// ## Why it scrubs first
///
/// See [`SNAPSHOT_SCRUB_PATHS`]. Every commit stacks a layer, so a file present
/// here is a file the project's image carries for the rest of its life.
///
/// The scrub is a no-op on a container that is already stopped — the migration
/// path scrubs it itself while it is still running and stops it before getting
/// here — and it can never fail this function; see [`scrub_writable_layer`].
pub async fn commit_container_snapshot(container_id: &str, project: &Project) -> Result<(), String> {
    let docker = get_docker()?;
    let image_name = get_snapshot_image_name(project);

    // Drop the throwaway files *before* the commit captures them. A commit
    // stacks a layer and never rewrites one, so anything present at this
    // instant is paid for permanently — see [`SNAPSHOT_SCRUB_PATHS`]. Failure
    // is swallowed inside; a scrub must never be able to block a snapshot.
    let scrub = scrub_writable_layer(container_id).await;

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

    log::info!(
        "Committed container {} as snapshot {}:{}{}",
        container_id,
        repo,
        tag,
        scrub.commit_log_suffix()
    );
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

/// Outcome of [`sweep_orphaned_snapshots`].
#[derive(Debug, Default, Clone, serde::Serialize)]
pub struct SnapshotSweepReport {
    /// Image ids that were removed.
    pub removed: Vec<String>,
    /// Bytes the removed images accounted for, as Docker reported them. A
    /// shared-layer estimate, not a disk-usage measurement.
    pub reclaimed_bytes: i64,
    /// Orphans Docker refused to delete because a container is still built
    /// from them. Normal, not a failure — the next sweep gets them.
    pub in_use: usize,
    /// Orphans that could not be removed for any other reason, with the error.
    pub failed: Vec<(String, String)>,
    /// Set when the engine could not be reached or listed at all.
    pub unavailable: Option<String>,
}

/// The filter every sweep runs under. Extracted so a test can hold the two
/// conditions in place: **dangling** and **labelled as ours**. Losing either
/// one turns a snapshot sweep into a prune of the user's whole image store.
fn orphan_sweep_filters() -> HashMap<String, Vec<String>> {
    HashMap::from([
        ("dangling".to_string(), vec!["true".to_string()]),
        (
            "label".to_string(),
            vec![format!("{}=true", LABEL_MANAGED)],
        ),
    ])
}

/// Remove the untagged snapshot commits left behind by recreation.
///
/// Every recreation commits the container to `triple-c-snapshot-{id}:latest`
/// and moves that tag; the image the tag pointed at before keeps its layers and
/// loses its name. Nothing else deletes those, so a project that has been
/// recreated a dozen times leaves a dozen multi-gigabyte orphans behind.
///
/// Two conditions, and the safety of this whole function rests on them:
///
/// * **Dangling** — untagged. Every image the app relies on carries a tag:
///   `triple-c-snapshot-{id}:latest` is what a project is rebuilt from, and a
///   migration's `pre-migration-*` pin is the only copy of a rollback target.
///   Neither can ever match this filter, so neither can be swept.
/// * **`triple-c.managed=true`** — only images Triple-C itself built or
///   committed. `docker commit` copies the container's labels onto the image,
///   and `container/Dockerfile` stamps the same label on the base, which is
///   what makes it a reliable mark of provenance. The user's own dangling
///   images are none of our business — the daemon this runs against is shared
///   with their unrelated work, so an unfiltered prune is never an option.
///
/// **Superseded base images are collected by exactly the same two conditions.**
/// They used to be unreachable: `container/Dockerfile` carried no `LABEL` at
/// all, so a base image left untagged when a newer build claimed
/// `triple-c-sandbox:latest` was dangling but not labelled and could never
/// match. ~11.9 GB was measured stranded that way. The Dockerfile now stamps
/// `triple-c.managed=true`, so no change is needed here beyond knowing that
/// this function is what reclaims them.
///
/// Removal is not forced, so Docker refuses (409) while any container is still
/// built from the image — including the stopped containers of projects that are
/// not running. That refusal is the third safety net and it is the daemon's,
/// not ours; those orphans are simply counted and left for a later sweep.
/// **This is why `force: false` stays.** Forcing would untag and delete an
/// image out from under a stopped project, and Docker would leave that
/// container unable to start. A superseded base image pinned by one stopped
/// container is therefore not reclaimed until that project is next recreated or
/// removed, which the startup sweep will notice on some later run.
///
/// Never fails the caller: this is housekeeping, and a full disk is a better
/// outcome than a project that will not start.
pub async fn sweep_orphaned_snapshots() -> SnapshotSweepReport {
    use bollard::image::ListImagesOptions;

    let mut report = SnapshotSweepReport::default();

    let docker = match get_docker() {
        Ok(d) => d,
        Err(e) => {
            report.unavailable = Some(e);
            return report;
        }
    };

    let images = match docker
        .list_images(Some(ListImagesOptions {
            all: false,
            filters: orphan_sweep_filters(),
            ..Default::default()
        }))
        .await
    {
        Ok(images) => images,
        Err(e) => {
            report.unavailable = Some(format!("Could not list orphaned snapshots: {}", e));
            return report;
        }
    };

    for summary in images {
        match docker
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
            Ok(_) => {
                report.reclaimed_bytes += summary.size;
                report.removed.push(summary.id);
            }
            Err(bollard::errors::Error::DockerResponseServerError {
                status_code: 409, ..
            }) => {
                report.in_use += 1;
            }
            Err(e) => {
                report.failed.push((summary.id, e.to_string()));
            }
        }
    }

    if !report.removed.is_empty() || report.in_use > 0 {
        log::info!(
            "Snapshot sweep: removed {} orphan(s) ({:.2} GB), {} still in use by a container",
            report.removed.len(),
            report.reclaimed_bytes as f64 / 1_073_741_824.0,
            report.in_use
        );
    }

    report
}

/// Run [`sweep_orphaned_snapshots`] and write the whole outcome to the log,
/// tagged with *why* the sweep ran.
///
/// Every caller of the sweep is housekeeping fired off in a detached task, and
/// every one of them dropped the `SnapshotSweepReport` on the floor — including
/// `reclaimed_bytes`, `failed` and `unavailable`, which are the only evidence
/// that a sweep ever happened or that it could not. When a user asks where
/// 116 GB went, "nothing was logged" is not an answer. There is no UI for this
/// yet by deliberate choice (prevention first), so the log is the whole
/// interface.
pub async fn sweep_orphaned_snapshots_logged(context: &str) {
    let report = sweep_orphaned_snapshots().await;

    if let Some(ref why) = report.unavailable {
        log::warn!("Snapshot sweep ({}) could not run: {}", context, why);
        return;
    }

    log::info!(
        "Snapshot sweep ({}): {} removed, {:.2} GB reclaimed, {} still pinned by a container, {} failed",
        context,
        report.removed.len(),
        report.reclaimed_bytes as f64 / 1_073_741_824.0,
        report.in_use,
        report.failed.len(),
    );

    for (id, error) in &report.failed {
        log::warn!("Snapshot sweep ({}) could not remove {}: {}", context, id, error);
    }
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

        // Claim the project before touching its snapshot.
        //
        // This is the third writer of `triple-c-snapshot-{id}:latest`, after a
        // recreate's commit and a compaction, and it has the same
        // read-modify-write shape: create a scratch container *from* the
        // snapshot, then commit back over the same tag. A `:latest` move
        // landing in between is silently overwritten by an image derived from
        // the pre-read state — which here would mean re-baking the very
        // credential this function exists to remove.
        //
        // A snapshot whose project is busy is left for the next call rather
        // than rewritten unsafely, and it is *reported*: silently skipping a
        // credential removal is the one outcome worse than failing it.
        let project_id = summary
            .repo_tags
            .iter()
            .find_map(|t| crate::docker::migration::parse_snapshot_reference(t))
            .map(|(id, _tag)| id);
        let _claim = match project_id.as_deref() {
            Some(id) => match crate::project_lock::try_acquire(id, crate::project_lock::ProjectOp::SecretScrub) {
                Ok(guard) => Some(guard),
                Err(reason) => {
                    report.failed.push((summary.repo_tags.join(", "), reason));
                    continue;
                }
            },
            // Not a `triple-c-snapshot-{uuid}` reference despite the filter.
            // Nothing owns it, so there is nothing to serialise against.
            None => None,
        };

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

    // `triple-c.scrub=true` so the Disk panel's scrub-container bucket can tell
    // a *live* rewrite from a leftover by label rather than by age. An age gate
    // alone was the previous discriminator, and a clock is a poor proxy for
    // "somebody is using this": killing this container between the create and
    // the commit below leaves the revoked credential baked into the snapshot's
    // `Config.Env`, which is precisely the state this function exists to end.
    let scratch_labels: HashMap<String, String> =
        HashMap::from([(LABEL_SCRUB.to_string(), "true".to_string())]);

    let created = docker
        .create_container(
            Some(CreateContainerOptions {
                name: scratch_name.clone(),
                ..Default::default()
            }),
            Config::<String> {
                image: Some(source_image.to_string()),
                labels: Some(scratch_labels),
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
        home_volume_name(&project.id),
        config_volume_name(&project.id),
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

    // ── VPN support (NET_ADMIN + /dev/net/tun + sysctl) ───────────────────
    // A container's capabilities, devices and sysctls are set at creation and
    // cannot be changed on a running or stopped container, so recreation is the
    // only way a toggle here takes effect. A missing label means the container
    // predates the feature, which is the same thing as having it off — so
    // existing projects are not churned until someone actually turns it on.
    let expected_vpn = project.vpn_support_enabled.to_string();
    let container_vpn = get_label("triple-c.vpn-support").unwrap_or_else(|| "false".to_string());
    if container_vpn != expected_vpn {
        log::info!("VPN support mismatch (container={:?}, expected={:?})", container_vpn, expected_vpn);
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
                !labels.contains_key(LABEL_MANAGED)
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
    fn vpn_support_off_touches_nothing_in_the_host_config() {
        // The default must stay byte-identical to a container created before the
        // feature existed, or every project recreates on the next start.
        let (cap_add, devices, sysctls) = vpn_host_config(false);
        assert_eq!(cap_add, None);
        assert_eq!(devices, None);
        assert_eq!(sysctls, None);
    }

    #[test]
    fn vpn_support_on_grants_all_three_pieces() {
        // Each is useless without the others — a client with the capability but
        // no device, or the device but no capability, still times out — so this
        // asserts the whole set rather than any one of them.
        let (cap_add, devices, sysctls) = vpn_host_config(true);

        assert_eq!(cap_add, Some(vec!["NET_ADMIN".to_string()]));

        let devices = devices.expect("the tun device must be passed through");
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].path_on_host.as_deref(), Some(TUN_DEVICE));
        assert_eq!(devices[0].path_in_container.as_deref(), Some(TUN_DEVICE));
        assert_eq!(devices[0].cgroup_permissions.as_deref(), Some("rwm"));

        assert_eq!(
            sysctls
                .expect("wireguard needs src_valid_mark")
                .get("net.ipv4.conf.all.src_valid_mark")
                .map(String::as_str),
            Some("1")
        );
    }

    #[test]
    fn vpn_support_never_grants_more_than_net_admin() {
        // NET_ADMIN is already a step out of the sandbox. Anything else added
        // here (SYS_ADMIN, or a blanket privileged flag) would be a much larger
        // one, so pin the set.
        let (cap_add, _, _) = vpn_host_config(true);
        assert_eq!(cap_add.unwrap(), vec!["NET_ADMIN"]);
    }

    #[test]
    fn the_vpn_skill_flag_is_emitted_either_way_never_omitted() {
        // The whole removal path depends on this. If `false` ever became "emit
        // nothing", a project that had the toggle on would keep the skill
        // forever: the container recreates from a snapshot whose baked
        // VPN_SUPPORT_ENABLED=1 would then go unchallenged, and entrypoint
        // would reinstall a skill for a capability the container no longer has.
        assert_eq!(vpn_env_var(true), "VPN_SUPPORT_ENABLED=1");
        assert_eq!(vpn_env_var(false), "VPN_SUPPORT_ENABLED=0");
    }

    #[test]
    fn the_vpn_skill_flag_is_reserved_from_custom_env() {
        // entrypoint.sh installs and removes the pia-vpn skill from this
        // variable. A custom env var of the same name would let a project claim
        // the skill without the capability behind it — or keep it after the
        // toggle is off — so it has to be unsettable like the others.
        assert!(is_reserved_env_key("VPN_SUPPORT_ENABLED"));
        assert!(is_reserved_env_key("vpn_support_enabled"));
        assert_eq!(
            compute_env_fingerprint(&[EnvVar {
                key: "VPN_SUPPORT_ENABLED".to_string(),
                value: "1".to_string(),
            }]),
            ""
        );
    }

    /// What bollard actually hands us when a tun-less host rejects the device.
    ///
    /// Captured verbatim from Docker 29.7: `docker create` with a missing
    /// device **succeeds**, and this arrives from the subsequent `start`.
    /// `DockerResponseServerError`'s Display is
    /// `"Docker responded with status code {code}: {message}"` with the
    /// daemon's message unaltered.
    const REAL_TUN_ERROR: &str = "Docker responded with status code 500: error \
        gathering device information while adding custom device \
        \"/dev/net/tun\": no such file or directory";

    #[test]
    fn a_missing_tun_device_is_explained_on_the_path_that_actually_fails() {
        // The start path is the one that matters: the daemon defers device
        // resolution to runc, so create returns an id on a host with no tun
        // module and only start fails. A version of this that checked create
        // alone would be dead code.
        let msg = explain_container_failure("start", REAL_TUN_ERROR);
        assert!(msg.starts_with("Failed to start container:"), "{}", msg);
        assert!(msg.contains("VPN support"), "should name the switch: {}", msg);
        assert!(msg.contains("tun` module"), "should name the cause: {}", msg);
        assert!(msg.contains("Config → Runtime"), "should say where to fix it: {}", msg);
        assert!(msg.contains(REAL_TUN_ERROR), "should keep the original: {}", msg);
    }

    #[test]
    fn the_same_explanation_covers_create_if_the_daemon_ever_checks_earlier() {
        // Belt and braces — older and future daemons may validate at create.
        let msg = explain_container_failure("create", REAL_TUN_ERROR);
        assert!(msg.starts_with("Failed to create container:"), "{}", msg);
        assert!(msg.contains("VPN support"), "{}", msg);
    }

    #[test]
    fn unrelated_failures_are_left_alone() {
        for (action, err) in [
            ("create", "Conflict. The container name \"/triple-c-x\" is already in use"),
            ("start", "Docker responded with status code 404: No such container"),
            ("start", "error gathering device information while adding custom device \"/dev/dri/card0\""),
        ] {
            assert_eq!(
                explain_container_failure(action, err),
                format!("Failed to {} container: {}", action, err),
                "{} should pass through untouched",
                err
            );
        }
    }

    #[test]
    fn the_orphan_sweep_only_ever_looks_at_our_own_untagged_images() {
        // Both conditions are load-bearing. Without `dangling` the sweep would
        // match `triple-c-snapshot-{id}:latest` — what every project is rebuilt
        // from — and a migration's `pre-migration-*` pin, which is the only copy
        // of a rollback target. Without the label it would match every dangling
        // image on the user's machine.
        let filters = orphan_sweep_filters();
        assert_eq!(filters.get("dangling"), Some(&vec!["true".to_string()]));
        assert_eq!(
            filters.get("label"),
            Some(&vec!["triple-c.managed=true".to_string()])
        );
        assert_eq!(filters.len(), 2, "an extra filter widens or narrows the sweep");
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

    // ── Pre-commit scrub (A1, C1) ────────────────────────────────────────────

    #[test]
    fn no_scrub_path_can_reach_a_host_bind_mount() {
        // `/workspace/{mount_name}` is the user's own project directory, bound
        // in from the host. Nothing in this list may ever name one — and the
        // two read-only host mounts under /tmp must be equally unreachable.
        //
        // Necessary, not sufficient: a pattern that looks like this can still
        // resolve into a bind mount through a symlinked parent, which is what
        // the script's own checks exist for. See
        // `the_scrub_script_refuses_a_symlinked_parent_...` below.
        for path in SNAPSHOT_SCRUB_PATHS {
            assert!(
                path.starts_with('/'),
                "{} is not absolute, so the shell would resolve it against an unknown cwd",
                path
            );
            assert!(
                !path.starts_with("/workspace"),
                "{} reaches into a host bind mount",
                path
            );
            assert!(
                !path.starts_with("/tmp/."),
                "{} could select /tmp/.host-ca or /tmp/.host-aws",
                path
            );
            assert!(
                !path.starts_with("/home"),
                "{} reaches into the persisted home volume",
                path
            );
        }
    }

    #[test]
    fn no_scrub_path_is_a_whole_system_directory() {
        // A trailing `/*` on a directory the system needs is fine; the
        // directory *itself* is not. Guards against an edit that shortens an
        // entry by one path component.
        const FORBIDDEN: &[&str] = &[
            "/", "/tmp", "/var", "/var/lib", "/var/log", "/var/cache", "/etc", "/usr", "/opt",
            "/workspace", "/home", "/home/claude", "/var/lib/apt", "/var/cache/apt",
        ];
        for path in SNAPSHOT_SCRUB_PATHS {
            let trimmed = path.trim_end_matches('/');
            assert!(
                !FORBIDDEN.contains(&trimmed),
                "{} would delete a directory the container needs",
                path
            );
        }
    }

    #[test]
    fn every_scrub_pattern_keeps_its_glob_in_the_final_component() {
        // The script can only validate a parent directory it can name, so the
        // parent has to be literal. An entry like `/var/*/apt/*` would put a
        // glob in a path *component*, which is the shape that made C1
        // exploitable in the first place.
        for pattern in SNAPSHOT_SCRUB_PATHS {
            let (dir, glob) = split_scrub_pattern(pattern)
                .unwrap_or_else(|| panic!("{} has no literal parent directory", pattern));
            assert!(
                !dir.contains(['*', '?', '[']),
                "{}: the parent {} is itself a glob, so the script cannot check it",
                pattern,
                dir
            );
            assert!(!glob.is_empty(), "{} has an empty final component", pattern);
            // Both halves are single-quoted into the script; a quote or a blank
            // would break out of that quoting and change what runs as root.
            assert!(
                !pattern.contains('\'') && !pattern.contains(char::is_whitespace),
                "{} cannot be safely single-quoted into the scrub script",
                pattern
            );
        }
    }

    #[test]
    fn every_scrub_parent_is_inside_the_scripts_containment_allowlist() {
        // The allowlist is hardcoded in the script and deliberately not derived
        // from this list, so the two can drift apart — in which case the entry
        // silently stops being scrubbed. Fail here instead.
        for pattern in SNAPSHOT_SCRUB_PATHS {
            let (dir, _) = split_scrub_pattern(pattern).expect("a literal parent");
            assert!(
                SCRUB_CONTAINMENT_PREFIXES
                    .iter()
                    .any(|p| dir == *p || dir.starts_with(&format!("{}/", p))),
                "{} is anchored at {}, which the script's containment check would refuse",
                pattern,
                dir
            );
        }
    }

    #[test]
    fn the_containment_allowlist_cannot_reach_anything_of_the_users() {
        for prefix in SCRUB_CONTAINMENT_PREFIXES {
            assert!(prefix.starts_with('/'), "{} is not absolute", prefix);
            assert!(!prefix.ends_with('/'), "{} would match the wrong things", prefix);
            for forbidden in [
                "/", "/workspace", "/home", "/etc", "/usr", "/opt", "/srv", "/root",
                "/var/lib/postgresql", "/var/lib/docker",
            ] {
                assert!(
                    !(forbidden == *prefix || forbidden.starts_with(&format!("{}/", prefix))),
                    "the allowlist entry {} contains {}",
                    prefix,
                    forbidden
                );
            }
        }
    }

    #[test]
    fn the_scrub_list_covers_every_measured_source_of_writable_layer_growth() {
        // Each of these was measured in a real container's pending commit.
        // Dropping one silently gives back multiple gigabytes per project.
        for expected in [
            "/tmp/claude-*",                 // agent scratchpads, 3.0 GB measured
            "/tmp/triple-c-drops/*",         // terminal drag-and-drop staging
            "/tmp/clipboard_*.png",          // pasted images
            "/var/lib/apt/lists/*",          // runtime apt, no `apt-get clean` behind it
            "/var/cache/apt/archives/*.deb",
            "/var/log/apt/*",
            "/var/log/dpkg.log",
        ] {
            assert!(
                SNAPSHOT_SCRUB_PATHS.contains(&expected),
                "{} is no longer scrubbed before commit",
                expected
            );
        }
    }

    #[test]
    fn the_users_own_files_are_only_scrubbed_once_they_are_stale() {
        // Regression: these two hold files the user handed the agent by hand,
        // and deleting them at commit time turned "change a setting" into
        // "silently lose the file you just dropped". They stay in the list —
        // nothing else ever reclaims them — but only with an age gate.
        for user_owned in ["/tmp/triple-c-drops/*", "/tmp/clipboard_*.png"] {
            assert!(
                SNAPSHOT_SCRUB_PATHS.contains(&user_owned),
                "{} was removed from the list instead of age-limited, so nothing reclaims it",
                user_owned
            );
            let age = SCRUB_MIN_AGE_DAYS.iter().find(|(p, _)| *p == user_owned);
            assert!(
                age.is_some_and(|(_, days)| *days >= 7),
                "{} is scrubbed without a meaningful age limit, which loses a just-dropped file",
                user_owned
            );
        }
        // The scratchpads are machine debris and are not age-limited; if that
        // ever changes, the comment explaining why must change with it.
        assert!(!SCRUB_MIN_AGE_DAYS.iter().any(|(p, _)| *p == "/tmp/claude-*"));
        // An age limit on a pattern that is not scrubbed at all does nothing
        // and reads as though it does.
        for (pattern, _) in SCRUB_MIN_AGE_DAYS {
            assert!(SNAPSHOT_SCRUB_PATHS.contains(pattern), "{} is not scrubbed", pattern);
        }
    }

    #[test]
    fn the_scrub_script_validates_every_directory_before_deleting_in_it() {
        // The structural half of the C1 fix, for the environments where the
        // `/bin/sh` test below cannot run. Each assertion here pins one of the
        // five checks; deleting any of them from the script fails this test.
        let script = snapshot_scrub_script();

        // Exactly one deletion in the whole script, inside `scrub_in` and
        // downstream of all five checks. A future edit that adds a bare `rm`
        // at the top level — which is what the vulnerable version was — fails
        // here.
        assert_eq!(
            script.matches("rm -rf").count(),
            1,
            "the scrub deletes somewhere other than inside the validated block:\n{}",
            script
        );
        // The handle: after this, paths are relative to a validated inode, so
        // re-pointing the directory cannot redirect the delete.
        assert!(script.contains(r#"cd -P "$d" 2>/dev/null || exit 0;"#));
        // 1. no symlinked component anywhere in the parent
        assert!(script.contains(r#"[ "$(pwd -P)" = "$d" ] || exit 0;"#));
        // 2. positive containment against a hardcoded allowlist
        assert!(script.contains("/tmp|/tmp/*|/var/log|/var/log/*"));
        assert!(!script.contains("/workspace"));
        // 3. the parent on the same filesystem as the root — a bind mount or
        //    volume is not part of the writable layer and must never be
        //    touched.
        assert!(script.contains(r#"[ -n "$rootdev" ] || exit 0;"#));
        assert!(script
            .contains(r#"[ "$(/usr/bin/stat -c %d . 2>/dev/null)" = "$rootdev" ] || exit 0;"#));
        // 4. and *each match* on it too. Check 3 says nothing about a mount
        //    planted at the match itself, and `rm --one-file-system` compares
        //    against its own argument's device, so without this line a volume
        //    mounted at `/tmp/claude-x` has its contents deleted — reproduced
        //    against a live daemon before this existed.
        assert!(script.contains(
            r#"[ "$(/usr/bin/stat -c %d -- "$p" 2>/dev/null)" = "$rootdev" ] || continue;"#
        ));
        // 5. an expansion carrying a separator means the list changed shape
        assert!(script.contains(r#"case "$p" in */*|.|..) continue ;; esac;"#));
    }

    #[test]
    fn the_scrub_script_cannot_be_steered_by_path() {
        // Checks 3 and 4 are `stat` answering a question, and the image's
        // `PATH` starts with three directories inside the container's own
        // persisted home volume. A three-line `stat` shim planted in the first
        // of them made an unshimmed-refused mount at `/var/log/apt` delete its
        // whole contents, so "no `stat` means no deletion" was never the
        // property that mattered. Every tool is therefore named absolutely and
        // `PATH` is reset before anything runs.
        let script = snapshot_scrub_script();
        assert!(
            script.starts_with("PATH=/usr/bin:/bin; export PATH;\n"),
            "the scrub inherits the container's PATH:\n{}",
            script
        );
        for tool in ["stat", "du", "find", "cut", "rm"] {
            for (at, _) in script.match_indices(tool) {
                // Skip the ones that are part of a longer word (`rmopt`,
                // `--one-file-system`) rather than a command being run.
                let before = &script[..at];
                let after = &script[at + tool.len()..];
                let is_word = !before.ends_with(|c: char| c.is_alphanumeric() || c == '-')
                    && !after.starts_with(|c: char| c.is_alphanumeric() || c == '-');
                if !is_word {
                    continue;
                }
                assert!(
                    before.ends_with("/usr/bin/"),
                    "`{}` is invoked without an absolute path, so the container decides which one runs:\n{}",
                    tool,
                    script
                );
            }
        }
    }

    #[test]
    fn the_scrub_script_hands_each_glob_over_unexpanded() {
        let script = snapshot_scrub_script();
        // Single-quoted at the call site: an unquoted `*` would be expanded
        // against the script's own working directory before `scrub_in` ran.
        assert!(script.contains("scrub_in '/tmp/claude-*' '-';"));
        assert!(script.contains("scrub_in '/var/log/apt/*' '-';"));
        assert!(script.contains("scrub_in '/tmp/triple-c-drops/*' '14';"));
        // The parent/glob split happens in the shell, so every entry stays
        // readable verbatim — `disk.rs` folds this onto one `RUN` line and
        // asserts each pattern appears there rather than a forked copy.
        for pattern in SNAPSHOT_SCRUB_PATHS {
            assert!(script.contains(pattern), "{} is not named in the script", pattern);
        }
        // Expanded inside the function, where the cwd is the validated
        // directory, and quoted everywhere it is *used* so a filename with a
        // space stays one argument.
        assert!(script.contains("for p in $g; do"));
        assert!(script.contains(r#"rm -rf $rmopt -- "$p""#));
        // `rm -rf /` would be catastrophic and is exactly what a botched
        // interpolation produces.
        assert!(!script.contains("rm -rf -- /\n"));
        assert!(!script.contains(" / "));
    }

    /// The behavioural half of the C1 fix.
    ///
    /// The test this replaces asserted `!script.contains("/workspace")`, which
    /// the exploited version passed: containment is a property of what the
    /// paths resolve to at run time. Docker is not available everywhere the
    /// suite runs, so a throwaway directory tree stands in for the container
    /// and `snapshot_scrub_script_under` re-anchors the real generated script
    /// onto it. Linux-only because the script uses GNU `stat -c` and `du -b`,
    /// which is what it will always run against inside the image.
    #[cfg(target_os = "linux")]
    #[test]
    fn the_scrub_script_refuses_a_symlinked_parent_and_still_reclaims_a_real_one() {
        use std::fs;
        use std::process::Command;

        // Canonicalised: a TMPDIR that is itself a symlink would trip check 1
        // and make every case pass by doing nothing at all.
        let base = fs::canonicalize(std::env::temp_dir()).expect("a real temp dir");
        let root = base.join(format!("triple-c-scrub-{}", uuid::Uuid::new_v4().simple()));
        let root_str = root.to_str().expect("a UTF-8 temp path").to_string();

        let mk = |rel: &str| fs::create_dir_all(root.join(rel)).expect("mkdir");
        // The script calls its tools by absolute path, so the re-anchored copy
        // needs them where it will look for them.
        plant_scrub_tools(&root);
        mk("tmp/triple-c-drops");
        mk("var/log");
        mk("var/lib/apt/lists");
        mk("var/cache/apt/archives");
        // Stands in for /workspace/{mount_name}: the user's own project,
        // bind-mounted from the host.
        mk("workspace/myproject/sub");
        fs::write(root.join("workspace/myproject/precious.txt"), "do not delete").unwrap();
        fs::write(root.join("workspace/myproject/sub/nested.txt"), "nor this").unwrap();

        // Debris the scrub is supposed to take, so this cannot pass by
        // refusing everything.
        mk("tmp/claude-scratch");
        fs::write(root.join("tmp/claude-scratch/blob"), vec![0u8; 64 * 1024]).unwrap();
        fs::write(root.join("var/lib/apt/lists/deb.list"), vec![0u8; 32 * 1024]).unwrap();

        // The attack, in the two shapes that were verified against a real
        // container: a symlinked *parent* (a path component, resolved by both
        // the glob and the `rm -rf`) and a symlinked *match* (safe already —
        // `rm -rf -- link` unlinks the link — and pinned here so it stays so).
        std::os::unix::fs::symlink(root.join("workspace/myproject"), root.join("var/log/apt"))
            .unwrap();
        std::os::unix::fs::symlink(
            root.join("workspace/myproject"),
            root.join("tmp/claude-link"),
        )
        .unwrap();

        // The age gate on the user's own files: the one dropped a moment ago
        // survives a recreation, the one from last month does not.
        fs::write(root.join("tmp/triple-c-drops/fresh.txt"), "just dropped").unwrap();
        fs::write(root.join("tmp/triple-c-drops/stale.txt"), vec![0u8; 8 * 1024]).unwrap();
        let touched = Command::new("touch")
            .arg("-d")
            .arg("30 days ago")
            .arg(root.join("tmp/triple-c-drops/stale.txt"))
            .status()
            .map(|s| s.success())
            .unwrap_or(false);

        let script = snapshot_scrub_script_under(&root_str);
        let output = Command::new("/bin/sh")
            .arg("-c")
            .arg(&script)
            .output()
            .expect("run the generated script");
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();

        let exists = |rel: &str| root.join(rel).exists();
        let survived = exists("workspace/myproject/precious.txt")
            && exists("workspace/myproject/sub/nested.txt");
        let debris_gone =
            !exists("tmp/claude-scratch") && !exists("var/lib/apt/lists/deb.list");
        let link_removed = fs::symlink_metadata(root.join("tmp/claude-link")).is_err();
        let fresh_kept = exists("tmp/triple-c-drops/fresh.txt");
        let stale_gone = !exists("tmp/triple-c-drops/stale.txt");
        let total = parse_scrub_total(&stdout);

        fs::remove_dir_all(&root).ok();

        assert!(
            survived,
            "the scrub followed a symlinked parent into a bind mount.\nstdout: {}\nstderr: {}\nscript:\n{}",
            stdout, stderr, script
        );
        assert!(
            debris_gone,
            "the scrub stopped reclaiming anything.\nstdout: {}\nstderr: {}",
            stdout, stderr
        );
        assert!(link_removed, "a symlinked match was left behind instead of unlinked");
        assert!(fresh_kept, "a file dropped moments ago was scrubbed anyway");
        if touched {
            assert!(stale_gone, "an age-limited file well past its limit was kept");
        }
        let total = total.expect("the marker line is missing");
        assert!(
            total >= 96 * 1024,
            "reported total {} is too small to have removed the planted debris",
            total
        );
    }

    /// Put the tools the re-anchored script calls where it will call them.
    ///
    /// Symlinks to the real ones, so a test can replace exactly one — which is
    /// how a mount point is simulated below without the privileges no test
    /// process has.
    #[cfg(target_os = "linux")]
    fn plant_scrub_tools(root: &std::path::Path) {
        std::fs::create_dir_all(root.join("usr/bin")).expect("mkdir usr/bin");
        for tool in ["stat", "du", "find", "cut", "rm"] {
            std::os::unix::fs::symlink(
                std::path::Path::new("/usr/bin").join(tool),
                root.join("usr/bin").join(tool),
            )
            .expect("link a tool into the test root");
        }
    }

    /// The other half of C1: a mount at the *match* rather than at its parent.
    ///
    /// Checks 0–3 validate the parent and `rm --one-file-system` compares
    /// against its own argument, so with the match itself a mount root there
    /// was nothing left between the scrub and the mounted filesystem's
    /// contents. Confirmed end to end before the fix — `docker run -v
    /// vol:/tmp/claude-x` came back with the volume empty, and the same run
    /// against a host directory bound at `/workspace/../tmp/claude-x` (the
    /// target the daemon builds from a mount name of `../tmp/claude-x`) emptied
    /// the host directory.
    ///
    /// A test process cannot mount anything — no `CAP_SYS_ADMIN`, and this
    /// container refuses `unshare -Urm` as well — so the boundary is simulated
    /// by the one thing that reports it: a `stat` that answers with a foreign
    /// device for one directory. Both negative controls are asserted, so a
    /// harness that had stopped being able to show the difference fails rather
    /// than passing quietly.
    #[cfg(target_os = "linux")]
    #[test]
    fn the_scrub_script_refuses_a_match_that_is_a_mount_point_and_ignores_a_shimmed_stat() {
        use std::fs;
        use std::process::Command;

        let base = fs::canonicalize(std::env::temp_dir()).expect("a real temp dir");
        let root = base.join(format!("triple-c-scrub-{}", uuid::Uuid::new_v4().simple()));
        let root_str = root.to_str().expect("a UTF-8 temp path").to_string();
        fs::create_dir_all(root.join("tmp")).expect("mkdir");
        plant_scrub_tools(&root);

        // `claude-mounted` stands in for the user's project directory bound
        // over a scratchpad path; `claude-real` is the debris the scrub is
        // still expected to take, so nothing here can pass by refusing
        // everything.
        let seed = || {
            fs::remove_dir_all(root.join("tmp/claude-mounted")).ok();
            fs::remove_dir_all(root.join("tmp/claude-real")).ok();
            fs::create_dir_all(root.join("tmp/claude-mounted/sub")).expect("mkdir");
            fs::create_dir_all(root.join("tmp/claude-real")).expect("mkdir");
            fs::write(root.join("tmp/claude-mounted/precious.txt"), "do not delete").unwrap();
            fs::write(root.join("tmp/claude-mounted/sub/nested.txt"), "nor this").unwrap();
            fs::write(root.join("tmp/claude-real/blob"), vec![0u8; 64 * 1024]).unwrap();
        };
        let mounted_survived = || {
            root.join("tmp/claude-mounted/precious.txt").exists()
                && root.join("tmp/claude-mounted/sub/nested.txt").exists()
        };

        // The kernel reports a mount point as a directory whose device differs
        // from its parent's. This says exactly that, for one name, and
        // otherwise answers honestly.
        fs::remove_file(root.join("usr/bin/stat")).expect("drop the symlink to the real stat");
        fs::write(
            root.join("usr/bin/stat"),
            "#!/bin/sh\nfor a in \"$@\"; do case \"$a\" in claude-mounted|*/claude-mounted) echo 999999; exit 0 ;; esac; done\nexec /usr/bin/stat \"$@\"\n",
        )
        .expect("overwrite the stat symlink with a stub");
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(root.join("usr/bin/stat"), fs::Permissions::from_mode(0o755))
                .unwrap();
        }

        // A shim of the shape an agent can plant in its own home volume, first
        // on `PATH`: one constant device makes every comparison agree.
        let hostile = root.join("hostile");
        fs::create_dir_all(&hostile).expect("mkdir");
        fs::write(hostile.join("stat"), "#!/bin/sh\necho 1\n").unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(hostile.join("stat"), fs::Permissions::from_mode(0o755)).unwrap();
        }
        let hostile_path = format!("{}:/usr/bin:/bin", hostile.display());

        let real = snapshot_scrub_script_under(&root_str);
        // Negative control 1: the same script with check 4 deleted.
        let without_match_check: String = real
            .lines()
            .filter(|l| !l.contains(r#"-- "$p" 2>/dev/null)" = "$rootdev" ] || continue;"#))
            .map(|l| format!("{}\n", l))
            .collect();
        // Negative control 2: the script as it was before the tools were named
        // absolutely — bare names, resolved through whatever `PATH` says.
        let with_bare_tools: String = real
            .replace(&format!("{}/usr/bin/", root_str), "")
            .lines()
            .filter(|l| !l.starts_with("PATH="))
            .map(|l| format!("{}\n", l))
            .collect();

        let run = |script: &str, path: &str| {
            Command::new("/bin/sh")
                .arg("-c")
                .arg(script)
                .env("PATH", path)
                .output()
                .expect("run the generated script")
        };

        seed();
        let real_out = run(&real, &hostile_path);
        let real_kept = mounted_survived();
        let real_reclaimed = !root.join("tmp/claude-real/blob").exists();

        seed();
        run(&without_match_check, &hostile_path);
        let no_check_kept = mounted_survived();

        seed();
        run(&with_bare_tools, &hostile_path);
        let bare_kept = mounted_survived();

        fs::remove_dir_all(&root).ok();

        assert!(
            !no_check_kept,
            "the harness cannot tell the difference: the mount-point check was removed and the \
             directory survived anyway, so the assertion below proves nothing"
        );
        assert!(
            !bare_kept,
            "the harness cannot tell the difference: the shimmed `stat` was reachable and did not \
             change the outcome, so PATH hardening cannot be shown either way"
        );
        assert!(
            real_kept,
            "the scrub emptied a mount planted at the match.\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&real_out.stdout),
            String::from_utf8_lossy(&real_out.stderr),
        );
        assert!(
            real_reclaimed,
            "the scrub stopped reclaiming a real scratchpad next to the mount"
        );
        assert_eq!(
            parse_scrub_total(&String::from_utf8_lossy(&real_out.stdout)).map(|t| t >= 64 * 1024),
            Some(true),
            "the reported total does not account for the scratchpad that was removed"
        );
    }

    #[test]
    fn the_scrub_total_is_read_back_from_the_marker_line() {
        assert_eq!(
            parse_scrub_total("some noise\n###TRIPLE-C-SCRUBBED 4812345\n"),
            Some(4812345)
        );
        // Nothing reclaimed is a real answer and must not read as a failure.
        assert_eq!(parse_scrub_total("###TRIPLE-C-SCRUBBED 0"), Some(0));
        // No marker means the script never got to the end — a different thing
        // from reclaiming nothing, and the caller logs it differently.
        assert_eq!(parse_scrub_total("sh: du: not found"), None);
        assert_eq!(parse_scrub_total(""), None);
    }

    #[test]
    fn only_a_completed_scrub_reports_bytes() {
        // M11: a stopped container is a skip, not a failure, and every outcome
        // that is not a completed run has no figure to contribute at all —
        // which is a stronger statement than the "contributes zero" this used
        // to make, and the reason the zero stopped being printed.
        assert_eq!(ScrubOutcome::NotRunning.commit_log_suffix(), "");
        assert!(!ScrubOutcome::TimedOut.commit_log_suffix().contains("MB"));
        assert!(!ScrubOutcome::Failed.commit_log_suffix().contains("MB"));
        assert!(ScrubOutcome::Reclaimed(4096)
            .commit_log_suffix()
            .contains("MB"));
        assert_ne!(ScrubOutcome::NotRunning, ScrubOutcome::Failed);
    }

    #[test]
    fn a_skipped_scrub_reports_no_reclaim_figure() {
        // Every migration logged "0.00 MB dropped by the pre-commit scrub":
        // the migration path scrubs the container while it is still running and
        // stops it before committing, so this second call has nothing to exec
        // into. A zero there reads as a scrub that ran and found nothing.
        assert_eq!(ScrubOutcome::NotRunning.commit_log_suffix(), "");
        assert!(!ScrubOutcome::TimedOut.commit_log_suffix().contains("MB"));
        assert!(!ScrubOutcome::Failed.commit_log_suffix().contains("MB"));
        // A scrub that really ran still reports, zero included — that zero is
        // an answer about a container that was there to be scrubbed.
        assert!(ScrubOutcome::Reclaimed(0)
            .commit_log_suffix()
            .contains("0.00 MB"));
        assert!(ScrubOutcome::Reclaimed(2 * 1_048_576)
            .commit_log_suffix()
            .contains("2.00 MB"));
    }

    #[test]
    fn the_scrub_cannot_hold_a_snapshot_up_indefinitely() {
        // M12: the scrub is a `du -sb` plus an `rm -rf` over a tree a running
        // agent may still be writing to, on the critical path of every
        // recreate with the UI on "Saving container state…" and no cancel.
        assert!(SCRUB_TIMEOUT.as_secs() >= 30, "too tight to survive a slow but healthy scrub");
        assert!(SCRUB_TIMEOUT.as_secs() <= 300, "long enough that a user would force-quit first");
    }

    /// `disk.rs` folds this script onto one `RUN` line for the compaction
    /// build by joining its non-blank lines with a space, so the script has to
    /// be a sequence of self-terminating statements and carry no `#` comments.
    /// The previous version was neither: its folded form was a `"do"
    /// unexpected` syntax error, which means compaction had been scrubbing
    /// nothing at all. The fold is reproduced here rather than imported
    /// because it is private to the other module — a divergence would show up
    /// as this test passing while the real Dockerfile broke, so it is pinned
    /// against the same wording in `fold_shell_script`.
    #[cfg(unix)]
    #[test]
    fn a_compaction_runs_this_module_s_scrub_script_byte_for_byte() {
        // The compaction build used to fold the script onto one `RUN` line by
        // joining its lines with a space, which turned `for p in …; do` into
        // `do` in statement position and made every compaction fail with
        // `syntax error: unexpected "do"`. That fold is gone — `disk.rs` now
        // emits the JSON exec form, whose string escapes carry newlines — so
        // the assertion worth pinning from this side is no longer "the folded
        // one-liner still parses" but the stronger one: whatever encoding
        // `disk.rs` chooses, the bytes that reach `sh` are *this* script.
        //
        // This is what stops the two files drifting. `container.rs` owns the
        // containment rules in `snapshot_scrub_script`; a compaction that ran a
        // mangled copy would be running a scrub with those rules altered, and
        // the mangling would be silent.
        let expected = snapshot_scrub_script();
        // Build the real Dockerfile the compaction would, then pull the script
        // back out of it — going through `compaction_dockerfile` rather than a
        // helper means a change to how the RUN line is emitted is caught here.
        let dockerfile = crate::docker::disk::compaction_dockerfile(
            "triple-c-snapshot-00000000-0000-0000-0000-000000000000:latest",
            &expected,
        );
        let run_line = dockerfile
            .lines()
            .find(|l| l.starts_with("RUN "))
            .expect("the compaction Dockerfile should carry a RUN line");
        let actual = crate::docker::disk::script_from_run_line(run_line)
            .expect("the compaction RUN line should be the JSON exec form");
        assert_eq!(
            actual, expected,
            "the compaction runs a different script than snapshot_scrub_script() produces"
        );
    }

    #[test]
    fn every_container_is_created_with_a_bounded_log() {
        let cfg = capped_log_config();
        assert_eq!(cfg.typ.as_deref(), Some("json-file"));
        let config = cfg.config.expect("a json-file driver with no config is unbounded");
        assert_eq!(config.get("max-size").map(String::as_str), Some("10m"));
        assert_eq!(config.get("max-file").map(String::as_str), Some("3"));
    }

    // ── Claude Code settings.json (Part B) ───────────────────────────────────

    fn settings_json(s: Option<&ClaudeCodeSettings>, sandbox: bool) -> serde_json::Value {
        serde_json::from_str(&build_claude_code_settings_json(s, sandbox))
            .expect("the payload must be valid JSON — the entrypoint pipes it into jq")
    }

    /// The five keys that used to be emitted only when non-default, plus the
    /// sandbox block that already knew better.
    const MANAGED_SETTINGS_KEYS: &[&str] = &[
        "tui",
        "effortLevel",
        "autoScrollEnabled",
        "showThinkingSummaries",
        "viewMode",
        "awaySummaryEnabled",
        "sandbox",
    ];

    #[test]
    fn a_project_can_turn_a_globally_enabled_setting_back_off() {
        // The whole reason the booleans are `Option<bool>`. Under the old
        // `if p.x { true } else { g.x }` merge there was no project value that
        // could produce `false` here.
        let global = ClaudeCodeSettings {
            focus_mode: Some(true),
            env_scrub: Some(true),
            prompt_caching_1h: Some(true),
            ..Default::default()
        };
        let project = ClaudeCodeSettings {
            focus_mode: Some(false),
            ..Default::default()
        };

        let merged = merge_claude_code_settings(Some(&global), Some(&project)).unwrap();

        assert_eq!(merged.focus_mode, Some(false), "project off must win");
        // Untouched project fields still inherit.
        assert_eq!(merged.env_scrub, Some(true));
        assert_eq!(merged.prompt_caching_1h, Some(true));

        // And it has to survive into what the container actually receives.
        let payload = build_claude_code_settings_json(Some(&merged), false);
        let v: serde_json::Value = serde_json::from_str(&payload).unwrap();
        assert!(
            v.get("viewMode").is_some_and(|m| m.is_null()),
            "viewMode should be cleared, got {:?}",
            v.get("viewMode")
        );
    }

    #[test]
    fn inherit_and_deliberate_off_fingerprint_differently() {
        // If these collided, switching a project from "inherit" to an explicit
        // "off" would not recreate the container and the change would silently
        // not apply.
        let inherit = ClaudeCodeSettings { focus_mode: None, ..Default::default() };
        let off = ClaudeCodeSettings { focus_mode: Some(false), ..Default::default() };
        assert_ne!(
            compute_claude_code_settings_fingerprint(Some(&inherit), false),
            compute_claude_code_settings_fingerprint(Some(&off), false),
        );
    }

    #[test]
    fn split_payload_never_emits_a_null_to_an_older_entrypoint() {
        // An existing project recreates from its own snapshot, which carries
        // whatever entrypoint it was built with. An older one merges with a
        // plain `.[0] * .[1]`, so a null reaching it would be written into the
        // user's settings.json verbatim instead of clearing the key.
        let payload = build_claude_code_settings_json(None, false);
        assert!(
            payload.contains("null"),
            "precondition: the unsplit payload uses null to mean delete"
        );

        let (set, clear) = split_claude_code_settings_payload(&payload);

        let set_val: serde_json::Value = serde_json::from_str(&set).unwrap();
        for (k, v) in set_val.as_object().unwrap() {
            assert!(!v.is_null(), "key {} reached the merge half as a null", k);
        }

        let clear_val: serde_json::Value = serde_json::from_str(&clear).unwrap();
        let cleared: Vec<&str> = clear_val
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        // The four whose neutral state is genuinely "unset".
        for k in ["tui", "effortLevel", "viewMode", "awaySummaryEnabled"] {
            assert!(cleared.contains(&k), "{} should be cleared, not pinned", k);
        }
    }

    #[test]
    fn split_payload_keeps_every_key_exactly_once() {
        // Nothing may be dropped or duplicated between the two halves, or a
        // managed key would silently stop being asserted.
        let settings = ClaudeCodeSettings {
            tui_mode: Some("fullscreen".to_string()),
            focus_mode: Some(true),
            ..Default::default()
        };
        let payload = build_claude_code_settings_json(Some(&settings), true);
        let original: serde_json::Value = serde_json::from_str(&payload).unwrap();

        let (set, clear) = split_claude_code_settings_payload(&payload);
        let set_val: serde_json::Value = serde_json::from_str(&set).unwrap();
        let clear_val: serde_json::Value = serde_json::from_str(&clear).unwrap();

        let mut seen: Vec<String> = set_val
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .chain(
                clear_val
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|v| v.as_str().unwrap().to_string()),
            )
            .collect();
        seen.sort();
        let mut expected: Vec<String> = original.as_object().unwrap().keys().cloned().collect();
        expected.sort();
        assert_eq!(seen, expected);
    }

    #[test]
    fn turning_a_setting_off_clears_it_rather_than_omitting_it() {
        // This is the whole bug. `~/.claude/settings.json` lives on a persisted
        // volume and the entrypoint merges into it, so a key that is merely
        // *absent* when the setting is off leaves the previous on-value in
        // place — the setting could never be turned back off short of a
        // destructive Reset.
        let on = ClaudeCodeSettings {
            tui_mode: Some("fullscreen".to_string()),
            effort: Some("xhigh".to_string()),
            auto_scroll_disabled: Some(true),
            focus_mode: Some(true),
            show_thinking_summaries: Some(true),
            session_recap_disabled: Some(true),
            ..Default::default()
        };
        let hot = settings_json(Some(&on), true);
        assert_eq!(hot["tui"], serde_json::json!("fullscreen"));
        assert_eq!(hot["effortLevel"], serde_json::json!("xhigh"));
        assert_eq!(hot["autoScrollEnabled"], serde_json::json!(false));
        assert_eq!(hot["showThinkingSummaries"], serde_json::json!(true));
        assert_eq!(hot["viewMode"], serde_json::json!("focus"));
        assert_eq!(hot["awaySummaryEnabled"], serde_json::json!(false));
        assert_eq!(hot["sandbox"]["enabled"], serde_json::json!(true));

        // Now everything back to default. Every key must still be *present*,
        // carrying either its neutral value or a null the entrypoint deletes.
        let cold = settings_json(Some(&ClaudeCodeSettings::default()), false);
        for key in MANAGED_SETTINGS_KEYS {
            assert!(
                cold.get(*key).is_some(),
                "{} is missing when the setting is off, so a stale on-value survives the merge",
                key
            );
        }
        assert_eq!(cold["tui"], serde_json::Value::Null);
        assert_eq!(cold["effortLevel"], serde_json::Value::Null);
        assert_eq!(cold["autoScrollEnabled"], serde_json::json!(true));
        assert_eq!(cold["showThinkingSummaries"], serde_json::json!(false));
        assert_eq!(cold["viewMode"], serde_json::Value::Null);
        assert_eq!(cold["awaySummaryEnabled"], serde_json::Value::Null);
        assert_eq!(cold["sandbox"]["enabled"], serde_json::json!(false));
    }

    #[test]
    fn no_settings_struct_at_all_still_asserts_every_key() {
        // "This project has no Claude Code settings" is not "say nothing" —
        // the file on the config volume may still hold a previous project
        // configuration's values.
        let cold = settings_json(None, false);
        for key in MANAGED_SETTINGS_KEYS {
            assert!(cold.get(*key).is_some(), "{} is missing with no settings struct", key);
        }
    }

    #[test]
    fn the_settings_payload_uses_the_key_names_claude_code_actually_reads() {
        let s = ClaudeCodeSettings {
            effort: Some("high".to_string()),
            focus_mode: Some(true),
            ..Default::default()
        };
        let json = settings_json(Some(&s), false);
        // `effort` and `focusMode` were both invented; Claude Code reads
        // `effortLevel` and `viewMode`.
        assert!(json.get("effort").is_none(), "`effort` is not a Claude Code setting");
        assert!(json.get("focusMode").is_none(), "`focusMode` is not a Claude Code setting");
        assert_eq!(json["effortLevel"], serde_json::json!("high"));
        assert_eq!(json["viewMode"], serde_json::json!("focus"));
    }

    #[test]
    fn tui_can_be_pinned_to_the_classic_renderer_as_well_as_left_automatic() {
        // Three distinct states; "automatic" is not "classic".
        let auto = settings_json(Some(&ClaudeCodeSettings::default()), false);
        assert_eq!(auto["tui"], serde_json::Value::Null);

        let classic = settings_json(
            Some(&ClaudeCodeSettings {
                tui_mode: Some("default".to_string()),
                ..Default::default()
            }),
            false,
        );
        assert_eq!(classic["tui"], serde_json::json!("default"));
    }

    #[test]
    fn a_project_that_never_touched_session_recap_leaves_it_alone() {
        // Claude Code's recap is on by default, so the zero value of the field
        // has to be "don't interfere". Getting this backwards would have
        // silently disabled recaps for every existing project.
        let untouched = ClaudeCodeSettings::default();
        assert_eq!(untouched.session_recap_disabled, None);
        assert_eq!(
            settings_json(Some(&untouched), false)["awaySummaryEnabled"],
            serde_json::Value::Null
        );
    }

    #[test]
    fn a_disabled_setting_changes_the_recreation_fingerprint() {
        // `container_needs_recreation` is label-based and never diffs env, so
        // the settings only reach a container if the fingerprint moves.
        let on = ClaudeCodeSettings {
            focus_mode: Some(true),
            ..Default::default()
        };
        let off = ClaudeCodeSettings::default();
        assert_ne!(
            compute_claude_code_settings_fingerprint(Some(&on), false),
            compute_claude_code_settings_fingerprint(Some(&off), false),
        );
        let recap_off = ClaudeCodeSettings {
            session_recap_disabled: Some(true),
            ..Default::default()
        };
        assert_ne!(
            compute_claude_code_settings_fingerprint(Some(&recap_off), false),
            compute_claude_code_settings_fingerprint(Some(&off), false),
        );
    }

    #[test]
    fn the_claude_code_env_vars_are_reserved_from_custom_env() {
        for key in [
            "CLAUDE_CODE_NO_FLICKER",
            "CLAUDE_CODE_ENABLE_AWAY_SUMMARY",
            "CLAUDE_CODE_SUBPROCESS_ENV_SCRUB",
            "ENABLE_PROMPT_CACHING_1H",
        ] {
            assert!(is_reserved_env_key(key), "{} must not be hand-settable", key);
            assert!(is_reserved_env_key(&key.to_lowercase()));
        }
    }

    #[test]
    fn every_claude_code_env_var_is_emitted_on_every_create() {
        // `docker commit` bakes env into the snapshot image, so a name that
        // goes missing when its setting is off keeps whatever the image
        // carries. All four must appear whatever the settings say.
        for settings in [None, Some(&ClaudeCodeSettings::default())] {
            let emitted = claude_code_env_vars(settings);
            let names: Vec<&str> = emitted
                .iter()
                .map(|entry| entry.split_once('=').expect("every entry is KEY=VALUE").0)
                .collect();
            for expected in [
                "CLAUDE_CODE_NO_FLICKER",
                "CLAUDE_CODE_ENABLE_AWAY_SUMMARY",
                "CLAUDE_CODE_SUBPROCESS_ENV_SCRUB",
                "ENABLE_PROMPT_CACHING_1H",
            ] {
                assert!(names.contains(&expected), "{} was not emitted", expected);
            }
        }
    }

    #[test]
    fn the_neutral_state_of_the_overriding_env_vars_is_empty_not_zero() {
        // Both of these outrank a setting the user can change from inside the
        // container, so their "off" has to be silence, not an instruction.
        let vars = claude_code_env_vars(Some(&ClaudeCodeSettings::default()));
        assert!(vars.contains(&"CLAUDE_CODE_NO_FLICKER=".to_string()));
        assert!(vars.contains(&"CLAUDE_CODE_ENABLE_AWAY_SUMMARY=".to_string()));
        // These two have no documented meaning for `0`, so it is safe to say.
        assert!(vars.contains(&"CLAUDE_CODE_SUBPROCESS_ENV_SCRUB=0".to_string()));
        assert!(vars.contains(&"ENABLE_PROMPT_CACHING_1H=0".to_string()));
    }

    #[test]
    fn turning_the_session_recap_off_actually_forces_it_off() {
        // The whole point of B3: `=1` when enabled was a no-op against a
        // feature that was already on, and there was no off path at all.
        let off = ClaudeCodeSettings {
            session_recap_disabled: Some(true),
            ..Default::default()
        };
        assert!(claude_code_env_vars(Some(&off))
            .contains(&"CLAUDE_CODE_ENABLE_AWAY_SUMMARY=0".to_string()));
    }

    #[test]
    fn the_tui_choice_reaches_the_env_var_that_outranks_the_setting() {
        let fullscreen = ClaudeCodeSettings {
            tui_mode: Some("fullscreen".to_string()),
            ..Default::default()
        };
        assert!(claude_code_env_vars(Some(&fullscreen))
            .contains(&"CLAUDE_CODE_NO_FLICKER=1".to_string()));

        let classic = ClaudeCodeSettings {
            tui_mode: Some("default".to_string()),
            ..Default::default()
        };
        assert!(claude_code_env_vars(Some(&classic))
            .contains(&"CLAUDE_CODE_NO_FLICKER=0".to_string()));
    }
}
