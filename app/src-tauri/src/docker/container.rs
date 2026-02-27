use bollard::container::{
    Config, CreateContainerOptions, ListContainersOptions, RemoveContainerOptions,
    StartContainerOptions, StopContainerOptions,
};
use bollard::models::{ContainerSummary, HostConfig, Mount, MountTypeEnum};
use std::collections::HashMap;

use super::client::get_docker;
use crate::models::{AuthMode, BedrockAuthMethod, ContainerInfo, GlobalAwsSettings, Project};

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
) -> Result<String, String> {
    let docker = get_docker()?;
    let container_name = project.container_name();

    let mut env_vars: Vec<String> = Vec::new();

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

    let mut mounts = vec![
        // Project directory -> /workspace
        Mount {
            target: Some("/workspace".to_string()),
            source: Some(project.path.clone()),
            typ: Some(MountTypeEnum::BIND),
            read_only: Some(false),
            ..Default::default()
        },
        // Named volume for claude config persistence
        Mount {
            target: Some("/home/claude/.claude".to_string()),
            source: Some(format!("triple-c-claude-config-{}", project.id)),
            typ: Some(MountTypeEnum::VOLUME),
            read_only: Some(false),
            ..Default::default()
        },
    ];

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
        mounts.push(Mount {
            target: Some("/var/run/docker.sock".to_string()),
            source: Some(docker_socket_path.to_string()),
            typ: Some(MountTypeEnum::BIND),
            read_only: Some(false),
            ..Default::default()
        });
    }

    let mut labels = HashMap::new();
    labels.insert("triple-c.managed".to_string(), "true".to_string());
    labels.insert("triple-c.project-id".to_string(), project.id.clone());
    labels.insert("triple-c.project-name".to_string(), project.name.clone());

    let host_config = HostConfig {
        mounts: Some(mounts),
        ..Default::default()
    };

    let config = Config {
        image: Some(image_name.to_string()),
        hostname: Some("triple-c".to_string()),
        env: Some(env_vars),
        labels: Some(labels),
        working_dir: Some("/workspace".to_string()),
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
    docker
        .remove_container(
            container_id,
            Some(RemoveContainerOptions {
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
pub async fn container_needs_recreation(container_id: &str, project: &Project) -> Result<bool, String> {
    let docker = get_docker()?;
    let info = docker
        .inspect_container(container_id, None)
        .await
        .map_err(|e| format!("Failed to inspect container: {}", e))?;

    let mounts = info
        .host_config
        .as_ref()
        .and_then(|hc| hc.mounts.as_ref());

    // ── Docker socket mount ──────────────────────────────────────────────
    let has_socket = mounts
        .map(|m| {
            m.iter()
                .any(|mount| mount.target.as_deref() == Some("/var/run/docker.sock"))
        })
        .unwrap_or(false);
    if has_socket != project.allow_docker_access {
        log::info!("Docker socket mismatch (container={}, project={})", has_socket, project.allow_docker_access);
        return Ok(true);
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
