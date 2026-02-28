use bollard::container::{
    Config, CreateContainerOptions, ListContainersOptions, RemoveContainerOptions,
    StartContainerOptions, StopContainerOptions,
};
use bollard::models::{ContainerSummary, HostConfig, Mount, MountTypeEnum};
use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use super::client::get_docker;
use crate::models::{AuthMode, BedrockAuthMethod, ContainerInfo, EnvVar, GlobalAwsSettings, Project, ProjectPath};

/// Compute a fingerprint string for the custom environment variables.
/// Sorted alphabetically so order changes do not cause spurious recreation.
fn compute_env_fingerprint(custom_env_vars: &[EnvVar]) -> String {
    let reserved_prefixes = ["ANTHROPIC_", "AWS_", "GIT_", "HOST_", "CLAUDE_", "TRIPLE_C_"];
    let mut parts: Vec<String> = Vec::new();
    for env_var in custom_env_vars {
        let key = env_var.key.trim();
        if key.is_empty() {
            continue;
        }
        let is_reserved = reserved_prefixes.iter().any(|p| key.to_uppercase().starts_with(p));
        if is_reserved {
            continue;
        }
        parts.push(format!("{}={}", key, env_var.value));
    }
    parts.sort();
    parts.join(",")
}

/// Merge global and per-project Claude instructions into a single string.
fn merge_claude_instructions(
    global_instructions: Option<&str>,
    project_instructions: Option<&str>,
) -> Option<String> {
    match (global_instructions, project_instructions) {
        (Some(g), Some(p)) => Some(format!("{}\n\n{}", g, p)),
        (Some(g), None) => Some(g.to_string()),
        (None, Some(p)) => Some(p.to_string()),
        (None, None) => None,
    }
}

/// Compute a fingerprint for the Bedrock configuration so we can detect changes.
fn compute_bedrock_fingerprint(project: &Project) -> String {
    if let Some(ref bedrock) = project.bedrock_config {
        let mut hasher = DefaultHasher::new();
        format!("{:?}", bedrock.auth_method).hash(&mut hasher);
        bedrock.aws_region.hash(&mut hasher);
        bedrock.aws_access_key_id.hash(&mut hasher);
        bedrock.aws_secret_access_key.hash(&mut hasher);
        bedrock.aws_session_token.hash(&mut hasher);
        bedrock.aws_profile.hash(&mut hasher);
        bedrock.aws_bearer_token.hash(&mut hasher);
        bedrock.model_id.hash(&mut hasher);
        bedrock.disable_prompt_caching.hash(&mut hasher);
        format!("{:x}", hasher.finish())
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
    let mut hasher = DefaultHasher::new();
    joined.hash(&mut hasher);
    format!("{:x}", hasher.finish())
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
    api_key: Option<&str>,
    docker_socket_path: &str,
    image_name: &str,
    aws_config_path: Option<&str>,
    global_aws: &GlobalAwsSettings,
    global_claude_instructions: Option<&str>,
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

    if let Some(key) = api_key {
        env_vars.push(format!("ANTHROPIC_API_KEY={}", key));
    }

    if let Some(ref token) = project.git_token {
        env_vars.push(format!("GIT_TOKEN={}", token));
    }
    if let Some(ref name) = project.git_user_name {
        env_vars.push(format!("GIT_USER_NAME={}", name));
    }
    if let Some(ref email) = project.git_user_email {
        env_vars.push(format!("GIT_USER_EMAIL={}", email));
    }

    // Bedrock configuration
    if project.auth_mode == AuthMode::Bedrock {
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
                    if let Some(ref key_id) = bedrock.aws_access_key_id {
                        env_vars.push(format!("AWS_ACCESS_KEY_ID={}", key_id));
                    }
                    if let Some(ref secret) = bedrock.aws_secret_access_key {
                        env_vars.push(format!("AWS_SECRET_ACCESS_KEY={}", secret));
                    }
                    if let Some(ref token) = bedrock.aws_session_token {
                        env_vars.push(format!("AWS_SESSION_TOKEN={}", token));
                    }
                }
                BedrockAuthMethod::Profile => {
                    // Per-project profile overrides global
                    let profile = bedrock.aws_profile.as_ref()
                        .or(global_aws.aws_profile.as_ref());
                    if let Some(p) = profile {
                        env_vars.push(format!("AWS_PROFILE={}", p));
                    }
                }
                BedrockAuthMethod::BearerToken => {
                    if let Some(ref token) = bedrock.aws_bearer_token {
                        env_vars.push(format!("AWS_BEARER_TOKEN_BEDROCK={}", token));
                    }
                }
            }

            if let Some(ref model) = bedrock.model_id {
                env_vars.push(format!("ANTHROPIC_MODEL={}", model));
            }

            if bedrock.disable_prompt_caching {
                env_vars.push("DISABLE_PROMPT_CACHING=1".to_string());
            }
        }
    }

    // Custom environment variables
    let reserved_prefixes = ["ANTHROPIC_", "AWS_", "GIT_", "HOST_", "CLAUDE_", "TRIPLE_C_"];
    for env_var in &project.custom_env_vars {
        let key = env_var.key.trim();
        if key.is_empty() {
            continue;
        }
        let is_reserved = reserved_prefixes.iter().any(|p| key.to_uppercase().starts_with(p));
        if is_reserved {
            log::warn!("Skipping reserved env var: {}", key);
            continue;
        }
        env_vars.push(format!("{}={}", key, env_var.value));
    }
    let custom_env_fingerprint = compute_env_fingerprint(&project.custom_env_vars);
    env_vars.push(format!("TRIPLE_C_CUSTOM_ENV={}", custom_env_fingerprint));

    // Claude instructions (global + per-project)
    let combined_instructions = merge_claude_instructions(
        global_claude_instructions,
        project.claude_instructions.as_deref(),
    );
    if let Some(ref instructions) = combined_instructions {
        env_vars.push(format!("CLAUDE_INSTRUCTIONS={}", instructions));
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

    // Named volume for claude config persistence
    mounts.push(Mount {
        target: Some("/home/claude/.claude".to_string()),
        source: Some(format!("triple-c-claude-config-{}", project.id)),
        typ: Some(MountTypeEnum::VOLUME),
        read_only: Some(false),
        ..Default::default()
    });

    // SSH keys mount (read-only staging; entrypoint copies to ~/.ssh with correct perms)
    if let Some(ref ssh_path) = project.ssh_key_path {
        mounts.push(Mount {
            target: Some("/tmp/.host-ssh".to_string()),
            source: Some(ssh_path.clone()),
            typ: Some(MountTypeEnum::BIND),
            read_only: Some(true),
            ..Default::default()
        });
    }

    // AWS config mount (read-only)
    // Mount if: Bedrock profile auth needs it, OR a global aws_config_path is set
    let should_mount_aws = if project.auth_mode == AuthMode::Bedrock {
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
                    target: Some("/home/claude/.aws".to_string()),
                    source: Some(aws_path.to_string_lossy().to_string()),
                    typ: Some(MountTypeEnum::BIND),
                    read_only: Some(true),
                    ..Default::default()
                });
            }
        }
    }

    // Docker socket (only if allowed)
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

    let mut labels = HashMap::new();
    labels.insert("triple-c.managed".to_string(), "true".to_string());
    labels.insert("triple-c.project-id".to_string(), project.id.clone());
    labels.insert("triple-c.project-name".to_string(), project.name.clone());
    labels.insert("triple-c.auth-mode".to_string(), format!("{:?}", project.auth_mode));
    labels.insert("triple-c.paths-fingerprint".to_string(), compute_paths_fingerprint(&project.paths));
    labels.insert("triple-c.bedrock-fingerprint".to_string(), compute_bedrock_fingerprint(project));
    labels.insert("triple-c.image".to_string(), image_name.to_string());

    let host_config = HostConfig {
        mounts: Some(mounts),
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

/// Check whether the existing container's configuration still matches the
/// current project settings.  Returns `true` when the container must be
/// recreated (mounts or env vars differ).
pub async fn container_needs_recreation(
    container_id: &str,
    project: &Project,
    global_claude_instructions: Option<&str>,
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

    // ── Auth mode ────────────────────────────────────────────────────────
    let current_auth_mode = format!("{:?}", project.auth_mode);
    if let Some(container_auth_mode) = get_label("triple-c.auth-mode") {
        if container_auth_mode != current_auth_mode {
            log::info!("Auth mode mismatch (container={:?}, project={:?})", container_auth_mode, current_auth_mode);
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

    // ── Bedrock config fingerprint ───────────────────────────────────────
    let expected_bedrock_fp = compute_bedrock_fingerprint(project);
    let container_bedrock_fp = get_label("triple-c.bedrock-fingerprint").unwrap_or_default();
    if container_bedrock_fp != expected_bedrock_fp {
        log::info!("Bedrock config mismatch");
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

    // ── SSH key path mount ───────────────────────────────────────────────
    let ssh_mount_source = mounts
        .and_then(|m| {
            m.iter()
                .find(|mount| mount.target.as_deref() == Some("/tmp/.host-ssh"))
        })
        .and_then(|mount| mount.source.as_deref());
    let project_ssh = project.ssh_key_path.as_deref();
    if ssh_mount_source != project_ssh {
        log::info!(
            "SSH key path mismatch (container={:?}, project={:?})",
            ssh_mount_source,
            project_ssh
        );
        return Ok(true);
    }

    // ── Git environment variables ────────────────────────────────────────
    let env_vars = info
        .config
        .as_ref()
        .and_then(|c| c.env.as_ref());

    let get_env = |name: &str| -> Option<String> {
        env_vars.and_then(|vars| {
            vars.iter()
                .find(|v| v.starts_with(&format!("{}=", name)))
                .map(|v| v[name.len() + 1..].to_string())
        })
    };

    let container_git_name = get_env("GIT_USER_NAME");
    let container_git_email = get_env("GIT_USER_EMAIL");
    let container_git_token = get_env("GIT_TOKEN");

    if container_git_name.as_deref() != project.git_user_name.as_deref() {
        log::info!("GIT_USER_NAME mismatch (container={:?}, project={:?})", container_git_name, project.git_user_name);
        return Ok(true);
    }
    if container_git_email.as_deref() != project.git_user_email.as_deref() {
        log::info!("GIT_USER_EMAIL mismatch (container={:?}, project={:?})", container_git_email, project.git_user_email);
        return Ok(true);
    }
    if container_git_token.as_deref() != project.git_token.as_deref() {
        log::info!("GIT_TOKEN mismatch");
        return Ok(true);
    }

    // ── Custom environment variables ──────────────────────────────────────
    let expected_fingerprint = compute_env_fingerprint(&project.custom_env_vars);
    let container_fingerprint = get_env("TRIPLE_C_CUSTOM_ENV").unwrap_or_default();
    if container_fingerprint != expected_fingerprint {
        log::info!("Custom env vars mismatch (container={:?}, expected={:?})", container_fingerprint, expected_fingerprint);
        return Ok(true);
    }

    // ── Claude instructions ───────────────────────────────────────────────
    let expected_instructions = merge_claude_instructions(
        global_claude_instructions,
        project.claude_instructions.as_deref(),
    );
    let container_instructions = get_env("CLAUDE_INSTRUCTIONS");
    if container_instructions.as_deref() != expected_instructions.as_deref() {
        log::info!("CLAUDE_INSTRUCTIONS mismatch");
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
