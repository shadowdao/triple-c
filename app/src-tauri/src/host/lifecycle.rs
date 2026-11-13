//! Bringing a project's container into existence, starting, stopping and
//! removing it — the host-specific half of the project lifecycle commands.
//!
//! The commands in `commands/project_commands.rs` keep everything that is the
//! same on every host: validating the LLM backend config, the Bedrock SSO
//! check, project status transitions, the project lock, closing terminal
//! sessions, the auth bridge, marketplace and credential syncs. They ask a
//! [`ProjectHost`] for the rest. For Docker that is the
//! find → maybe-snapshot-and-recreate → create → start sequence, which used to
//! sit inline in `start_project_container_locked` and is moved here unchanged.
//!
//! Docker-only operations with no host-neutral meaning — base-image migration,
//! snapshot sweeps, legacy cleanup at startup — keep calling `crate::docker`
//! directly; see the boundary test in `host/mod.rs`.

use super::ContainerTarget;
use crate::docker;
use crate::models::{AppSettings, Project};
use crate::models::container_config;

/// The host a project runs on. One variant today; an AWS variant will carry
/// the connection, region and environment (see the spec's data model).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProjectHost {
    Docker,
}

/// An agent target is a way *into* a container, not a handle on its
/// lifecycle: whatever runs the container (ECS, the local harness) stops and
/// removes it. Reaching here means a caller paired the Docker host with an
/// agent target, which is a bug to surface, not a panic.
const AGENT_LIFECYCLE: &str =
    "{op} a container is not supported for an agent target; the host that runs it owns its lifecycle";

/// What a host could not clean up when asked to discard a project's state.
/// Docker: the snapshot image and the named volumes.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct DiscardLeftovers {
    pub image: Option<String>,
    pub volumes: Vec<String>,
}

impl ProjectHost {
    pub fn for_project(_project: &Project) -> Self {
        ProjectHost::Docker
    }

    /// Make sure the project's container exists, matches the project's
    /// current configuration, and is running. Returns where it runs.
    ///
    /// `progress` receives the same human-readable steps the start view has
    /// always shown. Failure leaves the caller to reset the project status.
    pub async fn ensure_started(
        &self,
        project: &Project,
        settings: &AppSettings,
        progress: &(dyn Fn(&str) + Send + Sync),
    ) -> Result<ContainerTarget, String> {
        match self {
            ProjectHost::Docker => docker_ensure_started(project, settings, progress)
                .await
                .map(ContainerTarget::docker),
        }
    }

    /// The container the host currently has for this project, found by asking
    /// the host rather than trusting `project.container_id` — that field can
    /// be stale (a crash between remove and the store update, a hand-edited
    /// `projects.json`).
    pub async fn find_current(&self, project: &Project) -> Result<Option<ContainerTarget>, String> {
        match self {
            ProjectHost::Docker => Ok(docker::find_existing_container(project)
                .await?
                .map(ContainerTarget::docker)),
        }
    }

    pub async fn stop(&self, target: &ContainerTarget) -> Result<(), String> {
        match target {
            ContainerTarget::Docker { container_id } => docker::stop_container(container_id).await,
            ContainerTarget::Agent(_) => Err(AGENT_LIFECYCLE.replace("{op}", "Stopping")),
        }
    }

    /// Stop (errors ignored — a stopped container is the goal) and remove.
    pub async fn remove(&self, target: &ContainerTarget) -> Result<(), String> {
        match target {
            ContainerTarget::Docker { container_id } => {
                let _ = docker::stop_container(container_id).await;
                docker::remove_container(container_id).await
            }
            ContainerTarget::Agent(_) => Err(AGENT_LIFECYCLE.replace("{op}", "Removing")),
        }
    }

    /// Delete what outlives the container — for Docker the snapshot image and
    /// both named volumes (`~/.claude`, the OAuth login and transcripts go with
    /// them). Best-effort; reports what it could not remove.
    pub async fn discard_project_state(&self, project: &Project) -> DiscardLeftovers {
        match self {
            ProjectHost::Docker => {
                let mut left = DiscardLeftovers::default();
                if let Err(e) = docker::remove_snapshot_image(project).await {
                    log::warn!("Failed to remove snapshot image for project {}: {}", project.id, e);
                    left.image = Some(docker::get_snapshot_image_name(project));
                }
                left.volumes = docker::remove_project_volumes(project).await;
                left
            }
        }
    }

    /// Remove leftovers of features this host no longer has (Docker: the
    /// deleted MCP feature's containers and network).
    pub async fn remove_legacy_leftovers(&self, project: &Project) {
        match self {
            ProjectHost::Docker => {
                docker::remove_legacy_mcp_containers(&project.id).await;
                docker::remove_legacy_project_network(&project.id).await;
            }
        }
    }
}

/// The Docker start sequence, moved here verbatim from
/// `start_project_container_locked`. Returns the running container's id.
async fn docker_ensure_started(
    project: &Project,
    settings: &AppSettings,
    progress: &(dyn Fn(&str) + Send + Sync),
) -> Result<String, String> {
    let image_name =
        container_config::resolve_image_name(&settings.image_source, &settings.custom_image_name);

    // Ensure image exists
    progress("Checking image...");
    if !docker::image_exists(&image_name).await? {
        return Err(format!("Docker image '{}' not found. Please pull or build the image first.", image_name));
    }

    // Determine docker socket path
    let docker_socket = settings.docker_socket_path
        .as_deref()
        .map(|s| s.to_string())
        .unwrap_or_else(|| docker::default_docker_socket());

    // AWS config path from global settings
    let aws_config_path = settings.global_aws.aws_config_path.clone();

    // What we would create this container from *right now*: the project's
    // snapshot when one exists, else the configured base. This is the value
    // `container_needs_recreation` compares against the container's
    // `triple-c.create-image` label — the check that replaced the old
    // tautological one. It is resolved *before* the commit below, so it
    // describes the pre-commit world the existing container was born into.
    let snapshot_image = docker::get_snapshot_image_name(project);
    let expected_create_image =
        if docker::image_exists(&snapshot_image).await.unwrap_or(false) {
            snapshot_image.clone()
        } else {
            image_name.clone()
        };

    let container_id = if let Some(existing_id) = docker::find_existing_container(project).await? {
        // Check if config changed — if so, snapshot + recreate
        let needs_recreate = docker::container_needs_recreation(
            &existing_id,
            project,
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
            settings.ca_cert_path.as_deref(),
            settings.default_git_user_name.as_deref(),
            settings.default_git_user_email.as_deref(),
        ).await.unwrap_or(false);

        if needs_recreate {
            log::info!("Container config changed for project {} — committing snapshot and recreating", project.id);
            // Snapshot the filesystem before destroying
            progress("Saving container state...");
            if let Err(e) = docker::commit_container_snapshot(&existing_id, project).await {
                log::warn!("Failed to snapshot container before recreation: {}", e);
            }
            progress("Recreating container...");
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

            let new_id = docker::create_container_for_project(
                project,
                settings,
                &docker_socket,
                aws_config_path.as_deref(),
                &create_image,
                &image_name,
                docker::CreateExtras::default(),
            ).await?;
            progress("Starting container...");
            docker::start_container(&new_id).await?;

            // The commit above moved `:latest` and orphaned the image it
            // used to point at; the container holding that image open was
            // removed a few lines up, so now is when Docker will actually
            // let it go. Detached because this is housekeeping and the
            // project is already running — and it sweeps every orphan, not
            // just this one, so recreations that happened before the sweep
            // existed are cleaned up too.
            tauri::async_runtime::spawn(async {
                docker::sweep_orphaned_snapshots_logged("after recreation").await;
            });

            new_id
        } else {
            progress("Starting container...");
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

        progress("Creating container...");
        let new_id = docker::create_container_for_project(
            project,
            settings,
            &docker_socket,
            aws_config_path.as_deref(),
            &create_image,
            &image_name,
            docker::CreateExtras::default(),
        ).await?;
        progress("Starting container...");
        docker::start_container(&new_id).await?;
        new_id
    };

    Ok(container_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// End to end against a real daemon and the registry sandbox image: a
    /// throwaway project (fresh UUID, so its container, volumes and snapshot
    /// name are its own) is started, reached, stopped, started again on the
    /// *same* container, then removed and its state discarded.
    #[tokio::test]
    #[ignore = "needs a Docker daemon and the triple-c-sandbox image; creates and removes a throwaway project container"]
    async fn a_project_container_round_trips_through_the_host() {
        let project = Project::new("lifecycle-live-test".to_string(), Vec::new());
        let settings = AppSettings::default();
        let host = ProjectHost::for_project(&project);
        let steps = std::sync::Mutex::new(Vec::<String>::new());
        let progress = |m: &str| steps.lock().unwrap().push(m.to_string());
        // Failures inside the round trip return instead of panicking, so the
        // cleanup below always runs.
        fn check(ok: bool, what: &str) -> Result<(), String> {
            if ok { Ok(()) } else { Err(format!("check failed: {what}")) }
        }

        let outcome = async {
            check(host.find_current(&project).await?.is_none(), "no container before the first start")?;

            let first = host.ensure_started(&project, &settings, &progress).await?;
            check(first.is_running().await?, "running after start")?;
            let (out, code) = first
                .exec_oneshot_env_status(vec!["id".into(), "-un".into()], vec![])
                .await?;
            check(out.trim() == "claude" && code == 0, "exec runs as claude")?;
            check(host.find_current(&project).await? == Some(first.clone()), "find_current names it")?;

            host.stop(&first).await?;
            check(first.has_exited().await, "exited after stop")?;

            // Same config, so the existing container is started, not recreated.
            let second = host.ensure_started(&project, &settings, &progress).await?;
            check(second == first, "restart reuses the container")?;
            check(second.is_running().await?, "running after restart")?;

            host.remove(&second).await?;
            check(host.find_current(&project).await?.is_none(), "gone after remove")?;
            Ok::<_, String>(())
        }
        .await;

        // Always clean up, even after a failed assertion above.
        if let Ok(Some(t)) = host.find_current(&project).await {
            let _ = host.remove(&t).await;
        }
        let left = host.discard_project_state(&project).await;
        outcome.expect("round trip");
        assert_eq!(left, DiscardLeftovers::default(), "nothing left behind");
        let steps = steps.into_inner().unwrap();
        assert_eq!(
            steps,
            [
                "Checking image...",
                "Creating container...",
                "Starting container...",
                "Checking image...",
                "Starting container...",
            ]
        );
    }
}
