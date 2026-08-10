use tauri::{Emitter, State};

use crate::commands::aws_commands;
use crate::docker;
use crate::models::{container_config, AppSettings, Backend, BedrockAuthMethod, Project, ProjectPath, ProjectStatus};
use crate::storage::secure;
use crate::AppState;

pub(crate) fn emit_progress(app_handle: &tauri::AppHandle, project_id: &str, message: &str) {
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
    if let Some(ref oai_config) = project.openai_compatible_config {
        if let Some(ref v) = oai_config.api_key {
            secure::store_project_secret(&project.id, "openai-compatible-api-key", v)?;
        }
    }
    Ok(())
}

/// Create the project's container, threading every global setting through.
///
/// Exists so that the two ordinary create paths below and base-image migration
/// cannot drift apart — a container created by a migration must be
/// indistinguishable from one created by a normal start, or the next
/// `container_needs_recreation` would immediately throw it away.
///
/// `create_image` is what to create *from* (the snapshot or the base);
/// `base_image_name` is the configured base, which `create_container` needs in
/// order to tell those two apart when it stamps the lineage labels.
pub(crate) async fn create_container_for_project(
    project: &Project,
    settings: &AppSettings,
    docker_socket: &str,
    aws_config_path: Option<&str>,
    create_image: &str,
    base_image_name: &str,
    extras: docker::CreateExtras<'_>,
) -> Result<String, String> {
    docker::create_container(
        project,
        docker_socket,
        create_image,
        base_image_name,
        extras,
        aws_config_path,
        &settings.global_aws,
        &settings.global_ollama,
        &settings.global_llamacpp,
        &settings.global_openai_compatible,
        settings.global_claude_instructions.as_deref(),
        &settings.global_custom_env_vars,
        settings.timezone.as_deref(),
        settings.global_claude_code_settings.as_ref(),
        settings.default_ssh_key_path.as_deref(),
        settings.default_git_user_name.as_deref(),
        settings.default_git_user_email.as_deref(),
    )
    .await
}

/// Populate secret fields on a project struct from the OS keychain.
pub(crate) fn load_secrets_for_project(project: &mut Project) {
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
    if let Some(ref mut oai_config) = project.openai_compatible_config {
        oai_config.api_key = secure::get_project_secret(&project.id, "openai-compatible-api-key")
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
    // Release any host loopback ports the auth bridge holds for this project
    // before the container (and the project record) go away.
    state.auth_bridge.stop(&project_id).await;

    // A migration record outliving its project leaks a state file, a staged
    // payload tar that can run to several GB, and a `:pre-migration-<ts>` tag
    // holding an entire snapshot image that nothing will ever reference again.
    crate::commands::migration_commands::purge_migration_artifacts(&project_id).await;

    // Stop and remove container if it exists
    if let Some(ref project) = state.projects_store.get(&project_id) {
        if let Some(ref container_id) = project.container_id {
            state.exec_manager.close_sessions_for_container(container_id).await;
            let _ = docker::stop_container(container_id).await;
            let _ = docker::remove_container(container_id).await;
        }

        // Legacy MCP cleanup (pre-MCP-removal installs): drop any leftover MCP
        // containers first, then the per-project network they were attached to.
        docker::remove_legacy_mcp_containers(&project.id).await;
        docker::remove_legacy_project_network(&project.id).await;

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
    app_handle: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<Project, String> {
    store_secrets_for_project(&project)?;
    let updated = state.projects_store.update(project)?;

    // `auth_bridge_enabled` can arrive through this generic save as well as
    // through `set_auth_bridge_enabled`, so reconcile the running bridge with
    // whatever was just persisted. `start` is idempotent and `stop` is a no-op
    // when nothing is running, so this is safe on every project save.
    if updated.auth_bridge_enabled {
        if let Some(ref container_id) = updated.container_id {
            if docker::is_container_running(container_id).await.unwrap_or(false) {
                state
                    .auth_bridge
                    .start(
                        updated.id.clone(),
                        container_id.clone(),
                        app_handle,
                        state.projects_store.clone(),
                    )
                    .await;
            }
        }
    } else {
        state.auth_bridge.stop(&updated.id).await;
    }

    Ok(updated)
}

#[tauri::command]
pub async fn start_project_container(
    project_id: String,
    app_handle: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<Project, String> {
    // A migration removes the container and creates its replacement moments
    // later. Starting in that window finds no container, creates a second one
    // under the same name, and the migration's own create then fails on the
    // name conflict — which sends it into an auto-rollback that also cannot
    // create. The UI already refuses (`canMigrate` gates on the container being
    // stopped and no run being in flight); this is the same gate on the side
    // that actually owns the invariant.
    if crate::commands::migration_commands::is_migrating(&project_id) {
        return Err(
            "A container base update is running for this project. Wait for it to finish, then start the project."
                .to_string(),
        );
    }

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

    // Validate backend requirements
    if project.backend == Backend::Bedrock {
        let bedrock = project.bedrock_config.as_ref()
            .ok_or_else(|| "Bedrock backend selected but no Bedrock configuration found.".to_string())?;
        // Region can come from per-project or global
        if bedrock.aws_region.is_empty() && settings.global_aws.aws_region.is_none() {
            return Err("AWS region is required for Bedrock backend. Set it per-project or in global AWS settings.".to_string());
        }
    }

    if project.backend == Backend::Ollama {
        let ollama = project.ollama_config.as_ref()
            .ok_or_else(|| "Ollama backend selected but no Ollama configuration found.".to_string())?;
        if ollama.base_url.is_empty()
            && settings.global_ollama.base_url.as_deref().map(str::trim).unwrap_or("").is_empty()
        {
            return Err("Ollama base URL is required. Set it per-project or in global Ollama settings.".to_string());
        }
    }

    if project.backend == Backend::LlamaCpp {
        let cfg = project.llamacpp_config.as_ref()
            .ok_or_else(|| "llama.cpp backend selected but no llama.cpp configuration found.".to_string())?;
        if cfg.base_url.trim().is_empty()
            && settings.global_llamacpp.base_url.as_deref().map(str::trim).unwrap_or("").is_empty()
        {
            return Err("llama.cpp base URL is required. Set it per-project or in global llama.cpp settings.".to_string());
        }
    }

    if project.backend == Backend::OpenAiCompatible {
        let oai_config = project.openai_compatible_config.as_ref()
            .ok_or_else(|| "OpenAI Compatible backend selected but no configuration found.".to_string())?;
        if oai_config.base_url.is_empty()
            && settings.global_openai_compatible.base_url.as_deref().map(str::trim).unwrap_or("").is_empty()
        {
            return Err("OpenAI Compatible base URL is required. Set it per-project or in global settings.".to_string());
        }
    }

    // Update status to starting
    state.projects_store.update_status(&project_id, ProjectStatus::Starting)?;

    // Pre-validate AWS SSO session on the host for Bedrock Profile projects.
    // If the session is expired, trigger `aws sso login` before starting the container
    // so the entrypoint copies already-fresh credentials from the host mount.
    if project.backend == Backend::Bedrock {
        if let Some(ref bedrock) = project.bedrock_config {
            if bedrock.auth_method == BedrockAuthMethod::Profile {
                let profile = aws_commands::resolve_profile_for_project(
                    &project,
                    settings.global_aws.aws_profile.as_deref(),
                );

                emit_progress(&app_handle, &project_id, "Validating AWS session...");

                let session_valid = tokio::time::timeout(
                    std::time::Duration::from_secs(10),
                    aws_commands::check_sso_session(&profile),
                )
                .await;

                match session_valid {
                    Ok(Ok(true)) => {
                        emit_progress(&app_handle, &project_id, "AWS session valid.");
                    }
                    Ok(Ok(false)) => {
                        // Session expired — check if this is an SSO profile
                        if aws_commands::is_sso_profile(&profile).await.unwrap_or(false) {
                            emit_progress(
                                &app_handle,
                                &project_id,
                                "AWS session expired. Starting SSO login (check your browser)...",
                            );
                            match aws_commands::run_sso_login(&profile).await {
                                Ok(()) => {
                                    emit_progress(
                                        &app_handle,
                                        &project_id,
                                        "SSO login successful.",
                                    );
                                }
                                Err(e) => {
                                    log::warn!(
                                        "SSO login failed for profile '{}': {} — continuing anyway",
                                        profile,
                                        e
                                    );
                                    emit_progress(
                                        &app_handle,
                                        &project_id,
                                        "SSO login failed or cancelled. Continuing...",
                                    );
                                }
                            }
                        } else {
                            log::warn!(
                                "AWS session invalid for profile '{}' (not SSO). Continuing...",
                                profile
                            );
                        }
                    }
                    Ok(Err(e)) => {
                        log::warn!("Failed to check AWS session: {} — continuing anyway", e);
                    }
                    Err(_) => {
                        log::warn!("AWS session check timed out — continuing anyway");
                    }
                }
            }
        }
    }

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

        // What we would create this container from *right now*: the project's
        // snapshot when one exists, else the configured base. This is the value
        // `container_needs_recreation` compares against the container's
        // `triple-c.create-image` label — the check that replaced the old
        // tautological one. It is resolved *before* the commit below, so it
        // describes the pre-commit world the existing container was born into.
        let snapshot_image = docker::get_snapshot_image_name(&project);
        let expected_create_image =
            if docker::image_exists(&snapshot_image).await.unwrap_or(false) {
                snapshot_image.clone()
            } else {
                image_name.clone()
            };

        let container_id = if let Some(existing_id) = docker::find_existing_container(&project).await? {
            // Check if config changed — if so, snapshot + recreate
            let needs_recreate = docker::container_needs_recreation(
                &existing_id,
                &project,
                &expected_create_image,
                &settings.global_aws,
                &settings.global_ollama,
                &settings.global_llamacpp,
                &settings.global_openai_compatible,
                settings.global_claude_instructions.as_deref(),
                &settings.global_custom_env_vars,
                settings.timezone.as_deref(),
                settings.global_claude_code_settings.as_ref(),
                settings.default_ssh_key_path.as_deref(),
                settings.default_git_user_name.as_deref(),
                settings.default_git_user_email.as_deref(),
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

                // Legacy MCP cleanup: the old container may have been attached to
                // `triple-c-net-<projectId>`. Tear down leftover MCP containers and
                // that network now, before the replacement is created without it.
                docker::remove_legacy_mcp_containers(&project.id).await;
                docker::remove_legacy_project_network(&project.id).await;

                // Create from snapshot image (preserves system-level changes).
                // Re-resolved after the commit above: when no snapshot existed
                // before, one does now, and creating from the base instead
                // would throw away the state that was just saved.
                let create_image = if docker::image_exists(&snapshot_image).await.unwrap_or(false) {
                    snapshot_image.clone()
                } else {
                    image_name.clone()
                };

                let new_id = create_container_for_project(
                    &project,
                    &settings,
                    &docker_socket,
                    aws_config_path.as_deref(),
                    &create_image,
                    &image_name,
                    docker::CreateExtras::default(),
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
            if expected_create_image == snapshot_image {
                log::info!("Creating container from snapshot image for project {}", project.id);
            }
            let create_image = expected_create_image.clone();

            emit_progress(&app_handle, &project_id, "Creating container...");
            let new_id = create_container_for_project(
                &project,
                &settings,
                &docker_socket,
                aws_config_path.as_deref(),
                &create_image,
                &image_name,
                docker::CreateExtras::default(),
            ).await?;
            emit_progress(&app_handle, &project_id, "Starting container...");
            docker::start_container(&new_id).await?;
            new_id
        };

        // Sync Bedrock credentials on every start: refresh static/session creds
        // so rotated keys are picked up without a full container recreation, and
        // clear stale creds when the project no longer uses static-cred Bedrock.
        if let Err(e) = docker::sync_bedrock_credentials(&container_id, &project).await {
            log::warn!("Failed to sync AWS credentials for project {}: {}", project.id, e);
        }

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

    // Arm the auth bridge if this project opted in. Purely host-side, so it
    // happens after the container is up and never affects the start itself.
    if project.auth_bridge_enabled {
        state
            .auth_bridge
            .start(
                project_id.clone(),
                container_id.clone(),
                app_handle.clone(),
                state.projects_store.clone(),
            )
            .await;
    }

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

    // Drop host listeners first: they only make sense while the container runs.
    state.auth_bridge.stop(&project_id).await;

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
    // Reset deletes both volumes and the snapshot image. Doing that while a
    // migration is mid-flight pulls the ground out from under it and leaves an
    // orphan migration record pointing at images that no longer exist.
    if crate::commands::migration_commands::is_migrating(&project_id) {
        return Err(
            "A container base update is running for this project. Wait for it to finish before resetting."
                .to_string(),
        );
    }

    let project = state
        .projects_store
        .get(&project_id)
        .ok_or_else(|| format!("Project {} not found", project_id))?;

    // Reset supersedes any migration decision that was still pending: the
    // snapshot image and both volumes are about to go, so a surviving record
    // could only describe things that no longer exist — while its
    // `:pre-migration-<ts>` tag held a whole snapshot image (multiple GB) alive
    // with nothing left that could ever use it.
    crate::commands::migration_commands::purge_migration_artifacts(&project_id).await;

    // The bridge is bound to the container that is about to be destroyed;
    // `start_project_container` below re-arms it against the new one.
    state.auth_bridge.stop(&project_id).await;

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

/// Reconcile project statuses against actual Docker container state.
/// Called by the frontend after Docker is confirmed available. Projects
/// marked as Running whose containers are no longer running get reset
/// to Stopped.
///
/// This is also where an interrupted **base-image migration** is picked up.
/// It runs at startup, which is exactly when a migration that died with the app
/// needs to be noticed — see
/// [`crate::commands::migration_commands::reconcile_migration`]. The migration
/// pass runs over *every* project, not just the Running ones, because a project
/// whose container was removed mid-migration reports Stopped.
#[tauri::command]
pub async fn reconcile_project_statuses(
    app_handle: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<Vec<Project>, String> {
    let projects = state.projects_store.list();

    for project in &projects {
        crate::commands::migration_commands::reconcile_migration(project, &app_handle).await;
    }

    for project in &projects {
        // `Starting` and `Stopping` are in here as a backstop, not because
        // anything is expected to leave a project in one. They are transitional
        // states owned by an in-flight command, so a project still wearing one
        // is a project whose command died — a crash mid-start, or a migration
        // that bailed out between the stop and the swap. Skipping them, as this
        // loop used to, meant nothing in the app ever put such a project right:
        // it sat at "Stopping" with the Start button disabled, permanently.
        // Docker is the authority either way, so the check below is correct for
        // all four.
        if !matches!(
            project.status,
            ProjectStatus::Running
                | ProjectStatus::Error
                | ProjectStatus::Starting
                | ProjectStatus::Stopping
        ) {
            continue;
        }
        // ...but never for a project this process is actively migrating: the
        // container is legitimately absent for part of that run.
        if crate::commands::migration_commands::is_migrating(&project.id) {
            continue;
        }

        let is_running = if let Some(ref container_id) = project.container_id {
            docker::is_container_running(container_id).await.unwrap_or(false)
        } else {
            false
        };

        if is_running {
            log::info!(
                "Project '{}' ({}) container is still running — keeping Running status",
                project.name,
                project.id
            );
            // The app may have restarted while the container kept running; the
            // bridge lives in this process, so re-arm it here. `start` is
            // idempotent, so a bridge that is already polling is untouched.
            if project.auth_bridge_enabled {
                if let Some(ref container_id) = project.container_id {
                    state
                        .auth_bridge
                        .start(
                            project.id.clone(),
                            container_id.clone(),
                            app_handle.clone(),
                            state.projects_store.clone(),
                        )
                        .await;
                }
            }
        } else {
            log::info!(
                "Project '{}' ({}) container is not running — setting to Stopped",
                project.name,
                project.id
            );
            let _ = state.projects_store.update_status(&project.id, ProjectStatus::Stopped);
        }
    }

    Ok(state.projects_store.list())
}

fn default_docker_socket() -> String {
    if cfg!(target_os = "windows") {
        "//./pipe/docker_engine".to_string()
    } else {
        "/var/run/docker.sock".to_string()
    }
}
