use tauri::{Emitter, State};

use crate::docker;
use crate::models::{container_config, AuthMode, Project, ProjectPath, ProjectStatus};
use crate::storage::secure;
use crate::AppState;

fn emit_progress(app_handle: &tauri::AppHandle, project_id: &str, message: &str) {
    let _ = app_handle.emit(
        "container-progress",
        serde_json::json!({
            "project_id": project_id,
            "message": message,
        }),
    );
}

/// Extract secret fields from a project and store them in the OS keychain.
fn store_secrets_for_project(project: &Project) -> Result<(), String> {
    if let Some(ref token) = project.git_token {
        secure::store_project_secret(&project.id, "git-token", token)?;
    }
    if let Some(ref bedrock) = project.bedrock_config {
        if let Some(ref v) = bedrock.aws_access_key_id {
            secure::store_project_secret(&project.id, "aws-access-key-id", v)?;
        }
        if let Some(ref v) = bedrock.aws_secret_access_key {
            secure::store_project_secret(&project.id, "aws-secret-access-key", v)?;
        }
        if let Some(ref v) = bedrock.aws_session_token {
            secure::store_project_secret(&project.id, "aws-session-token", v)?;
        }
        if let Some(ref v) = bedrock.aws_bearer_token {
            secure::store_project_secret(&project.id, "aws-bearer-token", v)?;
        }
    }
    Ok(())
}

/// Populate secret fields on a project struct from the OS keychain.
fn load_secrets_for_project(project: &mut Project) {
    project.git_token = secure::get_project_secret(&project.id, "git-token")
        .unwrap_or(None);
    if let Some(ref mut bedrock) = project.bedrock_config {
        bedrock.aws_access_key_id = secure::get_project_secret(&project.id, "aws-access-key-id")
            .unwrap_or(None);
        bedrock.aws_secret_access_key = secure::get_project_secret(&project.id, "aws-secret-access-key")
            .unwrap_or(None);
        bedrock.aws_session_token = secure::get_project_secret(&project.id, "aws-session-token")
            .unwrap_or(None);
        bedrock.aws_bearer_token = secure::get_project_secret(&project.id, "aws-bearer-token")
            .unwrap_or(None);
    }
}

#[tauri::command]
pub async fn list_projects(state: State<'_, AppState>) -> Result<Vec<Project>, String> {
    Ok(state.projects_store.list())
}

#[tauri::command]
pub async fn add_project(
    name: String,
    paths: Vec<ProjectPath>,
    state: State<'_, AppState>,
) -> Result<Project, String> {
    // Validate paths
    if paths.is_empty() {
        return Err("At least one folder path is required.".to_string());
    }
    let mut seen_names = std::collections::HashSet::new();
    for p in &paths {
        if p.mount_name.is_empty() {
            return Err("Mount name cannot be empty.".to_string());
        }
        if !p.mount_name.chars().all(|c| c.is_alphanumeric() || c == '-' || c == '_' || c == '.') {
            return Err(format!("Mount name '{}' contains invalid characters. Use alphanumeric, dash, underscore, or dot.", p.mount_name));
        }
        if !seen_names.insert(p.mount_name.clone()) {
            return Err(format!("Duplicate mount name '{}'.", p.mount_name));
        }
    }
    let project = Project::new(name, paths);
    store_secrets_for_project(&project)?;
    state.projects_store.add(project)
}

#[tauri::command]
pub async fn remove_project(
    project_id: String,
    state: State<'_, AppState>,
) -> Result<(), String> {
    // Stop and remove container if it exists
    if let Some(ref project) = state.projects_store.get(&project_id) {
        if let Some(ref container_id) = project.container_id {
            state.exec_manager.close_sessions_for_container(container_id).await;
            let _ = docker::stop_container(container_id).await;
            let _ = docker::remove_container(container_id).await;
        }
        // Clean up the snapshot image + volumes
        if let Err(e) = docker::remove_snapshot_image(project).await {
            log::warn!("Failed to remove snapshot image for project {}: {}", project_id, e);
        }
        if let Err(e) = docker::remove_project_volumes(project).await {
            log::warn!("Failed to remove project volumes for project {}: {}", project_id, e);
        }
    }

    // Clean up keychain secrets for this project
    if let Err(e) = secure::delete_project_secrets(&project_id) {
        log::warn!("Failed to delete keychain secrets for project {}: {}", project_id, e);
    }

    state.projects_store.remove(&project_id)
}

#[tauri::command]
pub async fn update_project(
    project: Project,
    state: State<'_, AppState>,
) -> Result<Project, String> {
    store_secrets_for_project(&project)?;
    state.projects_store.update(project)
}

#[tauri::command]
pub async fn start_project_container(
    project_id: String,
    app_handle: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<Project, String> {
    let mut project = state
        .projects_store
        .get(&project_id)
        .ok_or_else(|| format!("Project {} not found", project_id))?;

    // Populate secret fields from the OS keychain so they are available
    // in memory when building environment variables for the container.
    load_secrets_for_project(&mut project);

    // Load settings for image resolution and global AWS
    let settings = state.settings_store.get();
    let image_name = container_config::resolve_image_name(&settings.image_source, &settings.custom_image_name);

    // Validate auth mode requirements
    if project.auth_mode == AuthMode::Bedrock {
        let bedrock = project.bedrock_config.as_ref()
            .ok_or_else(|| "Bedrock auth mode selected but no Bedrock configuration found.".to_string())?;
        // Region can come from per-project or global
        if bedrock.aws_region.is_empty() && settings.global_aws.aws_region.is_none() {
            return Err("AWS region is required for Bedrock auth mode. Set it per-project or in global AWS settings.".to_string());
        }
    }

    // Update status to starting
    state.projects_store.update_status(&project_id, ProjectStatus::Starting)?;

    // Wrap container operations so that any failure resets status to Stopped.
    let result: Result<String, String> = async {
        // Ensure image exists
        emit_progress(&app_handle, &project_id, "Checking image...");
        if !docker::image_exists(&image_name).await? {
            return Err(format!("Docker image '{}' not found. Please pull or build the image first.", image_name));
        }

        // Determine docker socket path
        let docker_socket = settings.docker_socket_path
            .as_deref()
            .map(|s| s.to_string())
            .unwrap_or_else(|| default_docker_socket());

        // AWS config path from global settings
        let aws_config_path = settings.global_aws.aws_config_path.clone();

        let container_id = if let Some(existing_id) = docker::find_existing_container(&project).await? {
            // Check if config changed — if so, snapshot + recreate
            let needs_recreate = docker::container_needs_recreation(
                &existing_id,
                &project,
                settings.global_claude_instructions.as_deref(),
                &settings.global_custom_env_vars,
                settings.timezone.as_deref(),
            ).await.unwrap_or(false);

            if needs_recreate {
                log::info!("Container config changed for project {} — committing snapshot and recreating", project.id);
                // Snapshot the filesystem before destroying
                emit_progress(&app_handle, &project_id, "Saving container state...");
                if let Err(e) = docker::commit_container_snapshot(&existing_id, &project).await {
                    log::warn!("Failed to snapshot container before recreation: {}", e);
                }
                emit_progress(&app_handle, &project_id, "Recreating container...");
                let _ = docker::stop_container(&existing_id).await;
                docker::remove_container(&existing_id).await?;

                // Create from snapshot image (preserves system-level changes)
                let snapshot_image = docker::get_snapshot_image_name(&project);
                let create_image = if docker::image_exists(&snapshot_image).await.unwrap_or(false) {
                    snapshot_image
                } else {
                    image_name.clone()
                };

                let new_id = docker::create_container(
                    &project,
                    &docker_socket,
                    &create_image,
                    aws_config_path.as_deref(),
                    &settings.global_aws,
                    settings.global_claude_instructions.as_deref(),
                    &settings.global_custom_env_vars,
                    settings.timezone.as_deref(),
                ).await?;
                emit_progress(&app_handle, &project_id, "Starting container...");
                docker::start_container(&new_id).await?;
                new_id
            } else {
                emit_progress(&app_handle, &project_id, "Starting container...");
                docker::start_container(&existing_id).await?;
                existing_id
            }
        } else {
            // Container doesn't exist (first start, or Docker pruned it).
            // Check for a snapshot image first — it preserves system-level
            // changes (apt/pip/npm installs) from the previous session.
            let snapshot_image = docker::get_snapshot_image_name(&project);
            let create_image = if docker::image_exists(&snapshot_image).await.unwrap_or(false) {
                log::info!("Creating container from snapshot image for project {}", project.id);
                snapshot_image
            } else {
                image_name.clone()
            };

            emit_progress(&app_handle, &project_id, "Creating container...");
            let new_id = docker::create_container(
                &project,
                &docker_socket,
                &create_image,
                aws_config_path.as_deref(),
                &settings.global_aws,
                settings.global_claude_instructions.as_deref(),
                &settings.global_custom_env_vars,
                settings.timezone.as_deref(),
            ).await?;
            emit_progress(&app_handle, &project_id, "Starting container...");
            docker::start_container(&new_id).await?;
            new_id
        };

        Ok(container_id)
    }.await;

    // On failure, reset status to Stopped so the project doesn't get stuck.
    if let Err(ref e) = result {
        log::error!("Failed to start container for project {}: {}", project_id, e);
        let _ = state.projects_store.update_status(&project_id, ProjectStatus::Stopped);
    }

    let container_id = result?;

    // Update project with container info using granular methods (Issue 14: TOCTOU)
    state.projects_store.set_container_id(&project_id, Some(container_id.clone()))?;
    state.projects_store.update_status(&project_id, ProjectStatus::Running)?;

    project.container_id = Some(container_id);
    project.status = ProjectStatus::Running;
    Ok(project)
}

#[tauri::command]
pub async fn stop_project_container(
    project_id: String,
    app_handle: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let project = state
        .projects_store
        .get(&project_id)
        .ok_or_else(|| format!("Project {} not found", project_id))?;

    state.projects_store.update_status(&project_id, ProjectStatus::Stopping)?;

    if let Some(ref container_id) = project.container_id {
        // Close exec sessions for this project
        emit_progress(&app_handle, &project_id, "Stopping container...");
        state.exec_manager.close_sessions_for_container(container_id).await;

        if let Err(e) = docker::stop_container(container_id).await {
            log::warn!("Docker stop failed for container {} (project {}): {} — resetting to Stopped anyway", container_id, project_id, e);
        }
    }

    state.projects_store.update_status(&project_id, ProjectStatus::Stopped)?;
    Ok(())
}

#[tauri::command]
pub async fn rebuild_project_container(
    project_id: String,
    app_handle: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<Project, String> {
    let project = state
        .projects_store
        .get(&project_id)
        .ok_or_else(|| format!("Project {} not found", project_id))?;

    // Remove existing container
    if let Some(ref container_id) = project.container_id {
        state.exec_manager.close_sessions_for_container(container_id).await;
        let _ = docker::stop_container(container_id).await;
        docker::remove_container(container_id).await?;
        state.projects_store.set_container_id(&project_id, None)?;
    }

    // Remove snapshot image + volumes so Reset creates from the clean base image
    if let Err(e) = docker::remove_snapshot_image(&project).await {
        log::warn!("Failed to remove snapshot image for project {}: {}", project_id, e);
    }
    if let Err(e) = docker::remove_project_volumes(&project).await {
        log::warn!("Failed to remove project volumes for project {}: {}", project_id, e);
    }

    // Start fresh
    start_project_container(project_id, app_handle, state).await
}

fn default_docker_socket() -> String {
    if cfg!(target_os = "windows") {
        "//./pipe/docker_engine".to_string()
    } else {
        "/var/run/docker.sock".to_string()
    }
}
