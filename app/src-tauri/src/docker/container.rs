use bollard::container::{
    Config, CreateContainerOptions, ListContainersOptions, RemoveContainerOptions,
    StartContainerOptions, StopContainerOptions,
};
use bollard::image::{CommitContainerOptions, RemoveImageOptions};
use bollard::models::{ContainerSummary, HostConfig, Mount, MountTypeEnum, PortBinding};
use std::collections::HashMap;
use sha2::{Sha256, Digest};

use super::client::get_docker;
use crate::models::{Backend, BedrockAuthMethod, ClaudeCodeSettings, ContainerInfo, EnvVar, GlobalAwsSettings, GlobalOllamaSettings, GlobalOpenAiCompatibleSettings, McpServer, McpTransportType, PortMapping, Project, ProjectPath};

const SCHEDULER_INSTRUCTIONS: &str = r#"## Scheduled Tasks

This container supports scheduled tasks via `triple-c-scheduler`. You can set up recurring or one-time tasks that run as separate Claude Code agents.

### Commands
- `triple-c-scheduler add --name "NAME" --schedule "CRON" --prompt "TASK"` — Add a recurring task
- `triple-c-scheduler add --name "NAME" --at "YYYY-MM-DD HH:MM" --prompt "TASK"` — Add a one-time task
- `triple-c-scheduler list` — List all scheduled tasks
- `triple-c-scheduler remove --id ID` — Remove a task
- `triple-c-scheduler enable --id ID` / `triple-c-scheduler disable --id ID` — Toggle tasks
- `triple-c-scheduler logs [--id ID] [--tail N]` — View execution logs
- `triple-c-scheduler run --id ID` — Manually trigger a task immediately
- `triple-c-scheduler notifications [--clear]` — View or clear completion notifications

### Cron format
Standard 5-field cron: `minute hour day-of-month month day-of-week`
Examples: `*/30 * * * *` (every 30 min), `0 9 * * 1-5` (9am weekdays), `0 */2 * * *` (every 2 hours)

### One-time tasks
Use `--at "YYYY-MM-DD HH:MM"` instead of `--schedule`. The task automatically removes itself after execution.

### Working directory
Use `--working-dir /workspace/project` to set where the task runs (default: /workspace).

### Checking results
After tasks run, check notifications with `triple-c-scheduler notifications` and detailed output with `triple-c-scheduler logs`.

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

/// Compute a fingerprint string for the custom environment variables.
/// Sorted alphabetically so order changes do not cause spurious recreation.
fn compute_env_fingerprint(custom_env_vars: &[EnvVar]) -> String {
    let reserved_prefixes = ["ANTHROPIC_", "AWS_", "GIT_", "HOST_", "TRIPLE_C_"];
    let reserved_exact = ["CLAUDE_INSTRUCTIONS", "MCP_SERVERS_JSON", "CLAUDE_CODE_SETTINGS_JSON", "MISSION_CONTROL_ENABLED"];
    let mut parts: Vec<String> = Vec::new();
    for env_var in custom_env_vars {
        let key = env_var.key.trim();
        if key.is_empty() {
            continue;
        }
        let upper = key.to_uppercase();
        let is_reserved = reserved_prefixes.iter().any(|p| upper.starts_with(p))
            || reserved_exact.iter().any(|e| upper == *e);
        if is_reserved {
            continue;
        }
        parts.push(format!("{}={}", key, env_var.value));
    }
    parts.sort();
    parts.join(",")
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
        // write_bedrock_static_credentials(), so a key rotation should refresh
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
/// Includes the resolved base_url and model_id (per-project blank → global default).
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
        let parts = vec![effective_url, effective_model];
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
        let parts = vec![
            effective_url,
            config.api_key.as_deref().unwrap_or("").to_string(),
            effective_model,
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

/// Build the JSON value for MCP servers config to be injected into ~/.claude.json.
/// Produces `{"mcpServers": {"name": {"type": "stdio", ...}, ...}}`.
///
/// Handles 4 modes:
/// - Stdio+Docker: `docker exec -i <mcp-container-name> <command> ...args`
/// - Stdio+Manual: `<command> ...args` (existing behavior)
/// - HTTP+Docker: `streamableHttp` URL pointing to `http://<mcp-container-name>:<port>/mcp`
/// - HTTP+Manual: `streamableHttp` with user-provided URL + headers
fn build_mcp_servers_json(servers: &[McpServer]) -> String {
    let mut mcp_map = serde_json::Map::new();
    for server in servers {
        let mut entry = serde_json::Map::new();
        match server.transport_type {
            McpTransportType::Stdio => {
                entry.insert("type".to_string(), serde_json::json!("stdio"));
                if server.is_docker() {
                    // Stdio+Docker: use `docker exec` to communicate with MCP container
                    entry.insert("command".to_string(), serde_json::json!("docker"));
                    let mut args = vec![
                        "exec".to_string(),
                        "-i".to_string(),
                        server.mcp_container_name(),
                    ];
                    if let Some(ref cmd) = server.command {
                        args.push(cmd.clone());
                    }
                    args.extend(server.args.iter().cloned());
                    entry.insert("args".to_string(), serde_json::json!(args));
                } else {
                    // Stdio+Manual: existing behavior
                    if let Some(ref cmd) = server.command {
                        entry.insert("command".to_string(), serde_json::json!(cmd));
                    }
                    if !server.args.is_empty() {
                        entry.insert("args".to_string(), serde_json::json!(server.args));
                    }
                }
                if !server.env.is_empty() {
                    entry.insert("env".to_string(), serde_json::json!(server.env));
                }
            }
            McpTransportType::Http => {
                entry.insert("type".to_string(), serde_json::json!("streamableHttp"));
                if server.is_docker() {
                    // HTTP+Docker: point to MCP container by name on the shared network
                    let url = format!(
                        "http://{}:{}/mcp",
                        server.mcp_container_name(),
                        server.effective_container_port()
                    );
                    entry.insert("url".to_string(), serde_json::json!(url));
                } else {
                    // HTTP+Manual: user-provided URL + headers
                    if let Some(ref url) = server.url {
                        entry.insert("url".to_string(), serde_json::json!(url));
                    }
                    if !server.headers.is_empty() {
                        entry.insert("headers".to_string(), serde_json::json!(server.headers));
                    }
                }
            }
        }
        mcp_map.insert(server.name.clone(), serde_json::Value::Object(entry));
    }
    let wrapper = serde_json::json!({ "mcpServers": mcp_map });
    serde_json::to_string(&wrapper).unwrap_or_default()
}

/// Compute a fingerprint for MCP server configuration so we can detect changes.
fn compute_mcp_fingerprint(servers: &[McpServer]) -> String {
    if servers.is_empty() {
        return String::new();
    }
    let json = build_mcp_servers_json(servers);
    sha256_hex(&json)
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

pub async fn create_container(
    project: &Project,
    docker_socket_path: &str,
    image_name: &str,
    aws_config_path: Option<&str>,
    global_aws: &GlobalAwsSettings,
    global_ollama: &GlobalOllamaSettings,
    global_openai_compatible: &GlobalOpenAiCompatibleSettings,
    global_claude_instructions: Option<&str>,
    global_custom_env_vars: &[EnvVar],
    timezone: Option<&str>,
    mcp_servers: &[McpServer],
    network_name: Option<&str>,
    global_claude_code_settings: Option<&ClaudeCodeSettings>,
    default_ssh_key_path: Option<&str>,
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
                    // write_bedrock_static_credentials() on every container
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
            }
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
            }
        }
    }

    // ── Neutralize stale backend auth env vars ──────────────────────────────
    // When a project switches backends (e.g. Bedrock → Anthropic) the container
    // is recreated *from a snapshot image* committed off the previous container.
    // `docker commit` always bakes the previous container's full ENV into that
    // image, and the commit API cannot strip it. So any auth var set under the
    // old backend (e.g. CLAUDE_CODE_USE_BEDROCK=1, AWS_*) survives in the image
    // ENV and stays active unless we explicitly override it at create time.
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
    ];
    let already_set: std::collections::HashSet<String> = env_vars
        .iter()
        .filter_map(|e| e.split('=').next().map(|k| k.to_string()))
        .collect();
    for key in MANAGED_AUTH_KEYS {
        if !already_set.contains(*key) {
            env_vars.push(format!("{}=", key));
        }
    }

    // Custom environment variables (global + per-project, project overrides global for same key)
    let merged_env = merge_custom_env_vars(global_custom_env_vars, &project.custom_env_vars);
    let reserved_prefixes = ["ANTHROPIC_", "AWS_", "GIT_", "HOST_", "TRIPLE_C_"];
    let reserved_exact = ["CLAUDE_INSTRUCTIONS", "MCP_SERVERS_JSON", "CLAUDE_CODE_SETTINGS_JSON", "MISSION_CONTROL_ENABLED"];
    for env_var in &merged_env {
        let key = env_var.key.trim();
        if key.is_empty() {
            continue;
        }
        let upper = key.to_uppercase();
        let is_reserved = reserved_prefixes.iter().any(|p| upper.starts_with(p))
            || reserved_exact.iter().any(|e| upper == *e);
        if is_reserved {
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

    // MCP servers config
    if !mcp_servers.is_empty() {
        let mcp_json = build_mcp_servers_json(mcp_servers);
        env_vars.push(format!("MCP_SERVERS_JSON={}", mcp_json));
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

    if should_mount_aws || aws_config_path.is_some() {
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

    // Docker socket (if allowed, or auto-enabled for stdio+Docker MCP servers)
    let needs_docker_for_mcp = any_stdio_docker_mcp(mcp_servers);
    if project.allow_docker_access || needs_docker_for_mcp {
        if needs_docker_for_mcp && !project.allow_docker_access {
            log::info!("Auto-enabling Docker socket access for stdio+Docker MCP servers");
        }
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
    labels.insert("triple-c.openai-compatible-fingerprint".to_string(), compute_openai_compatible_fingerprint(project, global_openai_compatible));
    labels.insert("triple-c.ports-fingerprint".to_string(), compute_ports_fingerprint(&project.port_mappings));
    labels.insert("triple-c.image".to_string(), image_name.to_string());
    labels.insert("triple-c.timezone".to_string(), timezone.unwrap_or("").to_string());
    labels.insert("triple-c.mcp-fingerprint".to_string(), compute_mcp_fingerprint(mcp_servers));
    labels.insert("triple-c.mission-control".to_string(), project.mission_control_enabled.to_string());
    labels.insert("triple-c.custom-env-fingerprint".to_string(), custom_env_fingerprint.clone());
    labels.insert("triple-c.claude-code-settings-fingerprint".to_string(),
        compute_claude_code_settings_fingerprint(merged_cc_settings.as_ref(), project.sandbox_mode_enabled));
    labels.insert("triple-c.instructions-fingerprint".to_string(),
        combined_instructions.as_ref().map(|s| sha256_hex(s)).unwrap_or_default());
    labels.insert("triple-c.git-user-name".to_string(), effective_git_name.unwrap_or_default().to_string());
    labels.insert("triple-c.git-user-email".to_string(), effective_git_email.unwrap_or_default().to_string());
    labels.insert("triple-c.git-token-hash".to_string(),
        project.git_token.as_ref().map(|t| sha256_hex(t)).unwrap_or_default());

    let host_config = HostConfig {
        mounts: Some(mounts),
        port_bindings: if port_bindings.is_empty() { None } else { Some(port_bindings) },
        init: Some(true),
        // Connect to project network if specified (for MCP container communication)
        network_mode: network_name.map(|n| n.to_string()),
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
/// NOTE: `docker commit` always bakes the *running container's* full ENV into
/// the resulting image — passing an empty Config here does NOT strip it, and
/// the commit API gives no way to remove env vars. As a result auth vars (e.g.
/// CLAUDE_CODE_USE_BEDROCK, AWS_*) are present in this snapshot image's ENV.
/// `create_container` defends against that by explicitly overriding every
/// managed auth key for the active backend (see MANAGED_AUTH_KEYS), so a
/// backend switch does not inherit the previous backend's stale credentials.
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

    // Empty config — no env vars / cmd baked in
    let config = Config::<String> {
        ..Default::default()
    };

    docker
        .commit_container(options, config)
        .await
        .map_err(|e| format!("Failed to commit container snapshot: {}", e))?;

    log::info!("Committed container {} as snapshot {}:{}", container_id, repo, tag);
    Ok(())
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
    global_aws: &GlobalAwsSettings,
    global_ollama: &GlobalOllamaSettings,
    global_openai_compatible: &GlobalOpenAiCompatibleSettings,
    global_claude_instructions: Option<&str>,
    global_custom_env_vars: &[EnvVar],
    timezone: Option<&str>,
    mcp_servers: &[McpServer],
    global_claude_code_settings: Option<&ClaudeCodeSettings>,
    default_ssh_key_path: Option<&str>,
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

    // ── OpenAI Compatible config fingerprint ────────────────────────────
    let expected_oai_fp = compute_openai_compatible_fingerprint(project, global_openai_compatible);
    let container_oai_fp = get_label("triple-c.openai-compatible-fingerprint").unwrap_or_default();
    if container_oai_fp != expected_oai_fp {
        log::info!("OpenAI Compatible config mismatch");
        return Ok(true);
    }

    // ── Image ────────────────────────────────────────────────────────────
    // The image label is set at creation time; if the user changed the
    // configured image we need to recreate.  We only compare when the
    // label exists (containers created before this change won't have it).
    if let Some(container_image) = get_label("triple-c.image") {
        // The caller doesn't pass the image name, but we can read the
        // container's actual image from Docker inspect.
        let actual_image = info
            .config
            .as_ref()
            .and_then(|c| c.image.as_ref());
        if let Some(actual) = actual_image {
            if *actual != container_image {
                log::info!("Image mismatch (actual={:?}, label={:?})", actual, container_image);
                return Ok(true);
            }
        }
    }

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

    // ── MCP servers fingerprint ─────────────────────────────────────────
    let expected_mcp_fp = compute_mcp_fingerprint(mcp_servers);
    let container_mcp_fp = get_label("triple-c.mcp-fingerprint").unwrap_or_default();
    if container_mcp_fp != expected_mcp_fp {
        log::info!("MCP servers fingerprint mismatch (container={:?}, expected={:?})", container_mcp_fp, expected_mcp_fp);
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

// ── MCP Container Lifecycle ─────────────────────────────────────────────

/// Returns true if any MCP server uses stdio transport with Docker.
pub fn any_stdio_docker_mcp(servers: &[McpServer]) -> bool {
    servers.iter().any(|s| s.is_docker() && s.transport_type == McpTransportType::Stdio)
}

/// Find an existing MCP container by its expected name.
pub async fn find_mcp_container(server: &McpServer) -> Result<Option<String>, String> {
    let docker = get_docker()?;
    let container_name = server.mcp_container_name();

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
        .map_err(|e| format!("Failed to list MCP containers: {}", e))?;

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

/// Create a Docker container for an MCP server.
pub async fn create_mcp_container(
    server: &McpServer,
    network_name: &str,
) -> Result<String, String> {
    let docker = get_docker()?;
    let container_name = server.mcp_container_name();

    let image = server
        .docker_image
        .as_ref()
        .ok_or_else(|| format!("MCP server '{}' has no docker_image", server.name))?;

    let mut env_vars: Vec<String> = Vec::new();
    for (k, v) in &server.env {
        env_vars.push(format!("{}={}", k, v));
    }

    // Build command + args as Cmd
    let mut cmd: Vec<String> = Vec::new();
    if let Some(ref command) = server.command {
        cmd.push(command.clone());
    }
    cmd.extend(server.args.iter().cloned());

    let mut labels = HashMap::new();
    labels.insert("triple-c.managed".to_string(), "true".to_string());
    labels.insert("triple-c.mcp-server".to_string(), server.id.clone());

    let host_config = HostConfig {
        network_mode: Some(network_name.to_string()),
        ..Default::default()
    };

    let config = Config {
        image: Some(image.clone()),
        env: if env_vars.is_empty() { None } else { Some(env_vars) },
        cmd: if cmd.is_empty() { None } else { Some(cmd) },
        labels: Some(labels),
        host_config: Some(host_config),
        ..Default::default()
    };

    let options = CreateContainerOptions {
        name: container_name.clone(),
        ..Default::default()
    };

    let response = docker
        .create_container(Some(options), config)
        .await
        .map_err(|e| format!("Failed to create MCP container '{}': {}", container_name, e))?;

    log::info!(
        "Created MCP container {} (image: {}) on network {}",
        container_name,
        image,
        network_name
    );
    Ok(response.id)
}

/// Start all Docker-based MCP server containers. Finds or creates each one.
pub async fn start_mcp_containers(
    servers: &[McpServer],
    network_name: &str,
) -> Result<(), String> {
    for server in servers {
        if !server.is_docker() {
            continue;
        }

        let container_id = if let Some(existing_id) = find_mcp_container(server).await? {
            log::debug!("Found existing MCP container for '{}'", server.name);
            existing_id
        } else {
            create_mcp_container(server, network_name).await?
        };

        // Start the container (ignore already-started errors)
        if let Err(e) = start_container(&container_id).await {
            let err_str = e.to_string();
            if err_str.contains("already started") || err_str.contains("304") {
                log::debug!("MCP container '{}' already running", server.name);
            } else {
                return Err(format!(
                    "Failed to start MCP container '{}': {}",
                    server.name, e
                ));
            }
        }

        log::info!("MCP container '{}' started", server.name);
    }

    Ok(())
}

/// Stop all Docker-based MCP server containers (best-effort).
pub async fn stop_mcp_containers(servers: &[McpServer]) -> Result<(), String> {
    for server in servers {
        if !server.is_docker() {
            continue;
        }
        if let Ok(Some(container_id)) = find_mcp_container(server).await {
            if let Err(e) = stop_container(&container_id).await {
                log::warn!("Failed to stop MCP container '{}': {}", server.name, e);
            } else {
                log::info!("Stopped MCP container '{}'", server.name);
            }
        }
    }
    Ok(())
}

/// Stop and remove all Docker-based MCP server containers (best-effort).
pub async fn remove_mcp_containers(servers: &[McpServer]) -> Result<(), String> {
    for server in servers {
        if !server.is_docker() {
            continue;
        }
        if let Ok(Some(container_id)) = find_mcp_container(server).await {
            let _ = stop_container(&container_id).await;
            if let Err(e) = remove_container(&container_id).await {
                log::warn!("Failed to remove MCP container '{}': {}", server.name, e);
            } else {
                log::info!("Removed MCP container '{}'", server.name);
            }
        }
    }
    Ok(())
}
