//! Container **base-image migration** — the IPC surface and the sequence.
//!
//! Moves a project off its own snapshot lineage and onto the current base image
//! **without touching either named volume**, which is the whole point: Reset
//! already gets you onto a clean base, and it takes `~/.claude`, the OAuth
//! credential, installed skills and every session transcript with it.
//!
//! # The sequence, and why it is crash-safe by construction
//!
//! ```text
//!  pre-flight (nothing destructive)
//!    resolve base ─ manifest(container|snapshot) ─ manifest(base) ─ deltas
//!    network check (apt-get update in a throwaway base container)
//!    disk check    (df inside that same container — NOT a host statvfs)
//!
//!  migrate
//!    persist migration_state ─ stop auth bridge / browser view / exec sessions
//!    stage verbatim payload to a host tar         ← container still running
//!    stop container
//!    commit_container_snapshot                    → :latest, still OLD lineage
//!    tag :latest → :pre-migration-<ts>            ← the rollback pin, free
//!    remove container
//!    create from BASE (labelled migration-state=in-progress) ─ start
//!    replay apt ─ replay npm -g ─ restore payload ─ probe
//!    commit_container_snapshot                    → :latest, NEW lineage
//!    phase = awaiting-confirmation
//! ```
//!
//! `triple-c-snapshot-<id>:latest` keeps pointing at the **old lineage** until
//! that final commit. Everything before it therefore self-heals: whatever the
//! app was doing when it died, `start_project_container` finds no container (or
//! the old one) and recreates from the old snapshot exactly as it always has.
//!
//! For the window *after* the container swap, the new container carries
//! `triple-c.migration-state=in-progress`. `reconcile_project_statuses` —
//! which already exists and already runs at startup — pairs that label with the
//! persisted state file and offers resume or rollback. See
//! [`crate::docker::migration::decide_recovery`] for the two-signal truth table.
//!
//! # One deliberate reordering
//!
//! The design memo puts payload staging after the container stop. `docker exec`
//! requires a running container, so staging happens immediately *before* the
//! stop instead. It is a read-only `tar` to a host file, so the crash-safety
//! argument is unchanged — and it now provably runs before the commit, which
//! means the pre-migration image contains everything that was staged.
//!
//! # What rollback does and does not restore
//!
//! Rollback puts the **system layer** back: the container is recreated from the
//! `:pre-migration-<ts>` image. The named volumes were never touched by the
//! migration at all, so anything written to `$HOME` during the migrated
//! session — a new login, new skills, new transcripts — survives the rollback.
//! Every report says so in words.

use tauri::State;

use crate::commands::project_commands::{create_container_for_project, emit_progress};
use crate::docker;
use crate::docker::migration::{self as mig, Recovery};
use crate::models::{
    ContainerStaleness, MigrationOptions, MigrationPhase, MigrationPlan, MigrationReport,
    MigrationState, PackageFailure, Project, ProjectStatus, UnpreservedData,
    MIGRATION_PHASE_AWAITING, MIGRATION_PHASE_INTERRUPTED,
};
use crate::storage::migration_store;
use crate::AppState;

// ─────────────────────────────────────────────────────────────────────────────
// Staleness
// ─────────────────────────────────────────────────────────────────────────────

/// Report how far behind the current base image a project's container is, and
/// what migrating it would actually carry across.
///
/// Read-only. Runs two filesystem probes (~3 s each) and is therefore meant to
/// be called on demand, not polled.
#[tauri::command]
pub async fn get_container_staleness(
    project_id: String,
    state: State<'_, AppState>,
) -> Result<ContainerStaleness, String> {
    let project = state
        .projects_store
        .get(&project_id)
        .ok_or_else(|| format!("Project {} not found", project_id))?;
    let settings = state.settings_store.get();
    let base_image = crate::models::container_config::resolve_image_name(
        &settings.image_source,
        &settings.custom_image_name,
    );
    let snapshot_image = docker::get_snapshot_image_name(&project);

    let mut out = ContainerStaleness::default();
    out.current_base_image_id = mig::image_id(&base_image).await.unwrap_or(None);
    out.snapshot_created_at = mig::image_created(&snapshot_image).await;

    // Lineage, most authoritative source first: the live container's label,
    // then the snapshot image's. Both are written by `create_container` and
    // propagated onto the snapshot by `docker commit`.
    let container_id = docker::find_existing_container(&project).await.unwrap_or(None);
    let recorded = match &container_id {
        Some(id) => container_label(id, mig::LABEL_BASE_IMAGE_ID).await,
        None => None,
    }
    .or_else(|| None);
    let recorded = match recorded {
        Some(v) => Some(v),
        None => mig::image_labels(&snapshot_image)
            .await
            .get(mig::LABEL_BASE_IMAGE_ID)
            .cloned(),
    }
    .filter(|v| !v.is_empty());

    out.base_image_id = recorded.clone();
    out.known = recorded.is_some();
    // An unknown lineage is "probe instead", never a claim of staleness.
    out.stale = match (&recorded, &out.current_base_image_id) {
        (Some(a), Some(b)) => a != b,
        _ => false,
    };

    // ── Probes ───────────────────────────────────────────────────────────
    let running = match &container_id {
        Some(id) => docker::is_container_running(id).await.unwrap_or(false),
        None => false,
    };
    let from_manifest = if running {
        mig::manifest_from_container(container_id.as_ref().unwrap()).await
    } else if docker::image_exists(&snapshot_image).await.unwrap_or(false) {
        mig::manifest_from_image(&snapshot_image).await
    } else {
        Err("This project has no container or snapshot image yet, so there is nothing to compare against the base image.".to_string())
    };

    let (from_manifest, base_manifest) = match from_manifest {
        Ok(f) => match mig::manifest_from_image(&base_image).await {
            Ok(b) => (f, b),
            Err(e) => {
                out.probe_error = Some(e);
                return Ok(out);
            }
        },
        Err(e) => {
            out.probe_error = Some(e);
            return Ok(out);
        }
    };

    let (missing_paths, missing_features) = mig::missing_features(&from_manifest, &base_manifest);
    out.missing_paths = missing_paths;
    out.missing_features = missing_features;
    out.apt_delta = mig::set_delta(&from_manifest.apt_manual, &base_manifest.apt_manual);
    out.npm_global_delta = mig::set_delta(&from_manifest.npm_global, &base_manifest.npm_global);
    out.verbatim_paths = mig::compute_verbatim_paths(
        &from_manifest,
        &base_manifest,
        &mig::bind_mount_exclusions(&project.paths),
    );
    out.outdated_package_count = mig::outdated_package_count(&from_manifest, &base_manifest);
    // Reported, never copied, and never silent — see `unpreserved_data`.
    out.unpreserved_data = mig::unpreserved_data(&from_manifest, &base_manifest);
    if !out.unpreserved_data.is_empty() {
        log::warn!(
            "Project {}: {} data-bearing subtree(s) under /var would not survive a migration: {}",
            project_id,
            out.unpreserved_data.len(),
            out.unpreserved_data
                .iter()
                .map(|d| d.path.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }

    let (etc_only_container, etc_only_base) = mig::etc_deltas(&from_manifest, &base_manifest);
    if !etc_only_container.is_empty() || !etc_only_base.is_empty() {
        // Reported, never copied — /etc is the base's to own. Copying the
        // snapshot's `nodesource.sources` onto a base that ships
        // `nodesource.list` would break every apt-get update on a duplicate
        // source, which is exactly the failure this logging exists to explain.
        log::info!(
            "Project {}: /etc differs from the base ({} only in the container, {} only in the base) — not copied by design",
            project_id,
            etc_only_container.len(),
            etc_only_base.len()
        );
    }

    Ok(out)
}

async fn container_label(container_id: &str, label: &str) -> Option<String> {
    let docker = docker::get_docker().ok()?;
    let info = docker.inspect_container(container_id, None).await.ok()?;
    info.config
        .and_then(|c| c.labels)
        .and_then(|l| l.get(label).cloned())
        .filter(|v| !v.is_empty())
}

// ─────────────────────────────────────────────────────────────────────────────
// Migrate
// ─────────────────────────────────────────────────────────────────────────────

/// Project ids with a migration running **in this process right now**.
///
/// Two things need it. `reconcile_project_statuses` is callable from the
/// frontend at any time, not only at startup, and a live migration looks
/// exactly like a crashed one from the outside (state file says `in-progress`,
/// container carries the label) — without this guard a reconcile mid-run would
/// rewrite the phase to `interrupted` underneath a migration that is fine.
/// It also makes a second concurrent `migrate_project_to_base` for the same
/// project impossible.
static ACTIVE_MIGRATIONS: std::sync::OnceLock<
    std::sync::Mutex<std::collections::HashSet<String>>,
> = std::sync::OnceLock::new();

fn active_migrations() -> &'static std::sync::Mutex<std::collections::HashSet<String>> {
    ACTIVE_MIGRATIONS.get_or_init(|| std::sync::Mutex::new(std::collections::HashSet::new()))
}

/// Whether a migration for this project is running **in this process right
/// now**. Every command that stops, removes or recreates the project's
/// container has to consult it: the window between `remove_container` and the
/// create that follows looks exactly like "no container", and an ordinary
/// Start landing in it creates a second container under the same name.
pub(crate) fn is_migrating(project_id: &str) -> bool {
    active_migrations()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .contains(project_id)
}

/// RAII marker: removes the project from [`ACTIVE_MIGRATIONS`] however the
/// migration ends, including an early `?`.
struct ActiveGuard(String);

impl ActiveGuard {
    /// `None` when a migration is already running for this project.
    fn acquire(project_id: &str) -> Option<Self> {
        let mut set = active_migrations()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if !set.insert(project_id.to_string()) {
            return None;
        }
        Some(Self(project_id.to_string()))
    }
}

impl Drop for ActiveGuard {
    fn drop(&mut self) {
        active_migrations()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&self.0);
    }
}

/// Move a project's container onto the current base image.
///
/// Volumes are never touched. Returns a [`MigrationReport`]; the project is
/// left in `awaiting-confirmation` so the user can try the container out and
/// then either [`confirm_migration`] or [`rollback_migration`].
#[tauri::command]
pub async fn migrate_project_to_base(
    project_id: String,
    options: MigrationOptions,
    app_handle: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<MigrationReport, String> {
    let Some(_guard) = ActiveGuard::acquire(&project_id) else {
        return Ok(MigrationReport::failed_preflight(
            "A migration is already running for this project.",
        ));
    };

    let existing = migration_store::load(&project_id)?;
    match existing.as_ref().map(|s| s.phase.as_str()) {
        Some(crate::models::MIGRATION_PHASE_IN_PROGRESS) => {
            // Only reachable if the app died mid-migration and reconcile has
            // not run yet; treat it exactly like `interrupted`.
            resume_migration(project_id, existing.unwrap(), app_handle, state).await
        }
        Some(MIGRATION_PHASE_INTERRUPTED) => {
            resume_migration(project_id, existing.unwrap(), app_handle, state).await
        }
        Some(MIGRATION_PHASE_AWAITING) => Ok(MigrationReport::failed_preflight(
            "This project already has a finished migration waiting for a decision. Confirm it or roll it back first.",
        )),
        Some(_) | None => fresh_migration(project_id, options, app_handle, state).await,
    }
}

async fn fresh_migration(
    project_id: String,
    options: MigrationOptions,
    app_handle: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<MigrationReport, String> {
    let mut project = state
        .projects_store
        .get(&project_id)
        .ok_or_else(|| format!("Project {} not found", project_id))?;
    crate::commands::project_commands::load_secrets_for_project(&mut project);
    let settings = state.settings_store.get();
    let base_image = crate::models::container_config::resolve_image_name(
        &settings.image_source,
        &settings.custom_image_name,
    );
    let snapshot_image = docker::get_snapshot_image_name(&project);

    // ── Pre-flight: nothing below here is destructive ────────────────────
    emit_progress(&app_handle, &project_id, "Checking the base image...");
    let base_id = match mig::image_id(&base_image).await? {
        Some(id) => id,
        None => {
            return Ok(MigrationReport::failed_preflight(format!(
                "The base image '{}' is not present. Pull or build it before migrating.",
                base_image
            )))
        }
    };

    let container_id = match docker::find_existing_container(&project).await? {
        Some(id) => id,
        None => {
            return Ok(MigrationReport::failed_preflight(
                "This project has no container yet. Start it once, then migrate.",
            ))
        }
    };

    // Both the manifest and the payload come from the *running* container, not
    // from the snapshot image: the image can lag by everything installed since
    // the last commit, and a verbatim set computed from a stale manifest would
    // silently fail to carry that work across.
    let was_running = docker::is_container_running(&container_id).await.unwrap_or(false);
    if !was_running {
        emit_progress(&app_handle, &project_id, "Starting the container to read its state...");
        docker::start_container(&container_id).await?;
    }

    emit_progress(&app_handle, &project_id, "Inspecting the current container...");
    let from_manifest = mig::manifest_from_container(&container_id).await?;
    emit_progress(&app_handle, &project_id, "Inspecting the base image...");
    let base_manifest = mig::manifest_from_image(&base_image).await?;

    let bind_targets = mig::bind_mount_exclusions(&project.paths);
    let verbatim = mig::compute_verbatim_paths(&from_manifest, &base_manifest, &bind_targets);
    let apt_delta = mig::set_delta(&from_manifest.apt_manual, &base_manifest.apt_manual);
    let npm_delta = mig::set_delta(&from_manifest.npm_global, &base_manifest.npm_global);
    let (missing_paths, _) = mig::missing_features(&from_manifest, &base_manifest);
    let payload_bytes = mig::verbatim_payload_bytes(&from_manifest, &verbatim);
    // Measured here as well as in the pre-flight probe, because this is the one
    // reading taken against the container that is about to be destroyed. It is
    // frozen into the plan so the finished report can still name it.
    let unpreserved = mig::unpreserved_data(&from_manifest, &base_manifest);
    if !unpreserved.is_empty() {
        log::warn!(
            "Project {}: migrating destroys {} data-bearing subtree(s) under /var: {}",
            project_id,
            unpreserved.len(),
            unpreserved
                .iter()
                .map(|d| format!("{} ({} files)", d.path, d.file_count))
                .collect::<Vec<_>>()
                .join(", ")
        );
    }

    emit_progress(&app_handle, &project_id, "Checking network and disk...");
    let env = mig::preflight_environment(&base_image).await?;
    if options.replay_packages && !apt_delta.is_empty() && !env.network_ok {
        return Ok(MigrationReport::failed_preflight(format!(
            "Cannot reach the package mirrors from a container ({}), so the {} package(s) this project added could not be reinstalled. Nothing was changed. Retry when the network is back, or migrate with package replay turned off.",
            env.network_detail,
            apt_delta.len()
        )));
    }
    let required = payload_bytes
        .saturating_mul(2)
        .saturating_add(mig::DISK_HEADROOM_BYTES);
    if env.available_bytes > 0 && env.available_bytes < required {
        return Ok(MigrationReport::failed_preflight(format!(
            "Not enough room on Docker's storage: {} available, about {} needed. Nothing was changed.",
            human_bytes(env.available_bytes),
            human_bytes(required)
        )));
    }

    // ── Everything from here is recorded before it happens ───────────────
    let mut mstate = MigrationState::new(
        mig::image_id(&snapshot_image).await.unwrap_or(None),
        Some(base_id),
        options,
    );
    mstate.plan = Some(MigrationPlan {
        apt_packages: apt_delta.clone(),
        npm_packages: npm_delta.clone(),
        verbatim_paths: verbatim.clone(),
        missing_paths,
        unpreserved_data: unpreserved,
    });
    migration_store::save(&project_id, &mstate)?;

    // Quiesce: every host-side attachment points at a container that is about
    // to stop existing.
    emit_progress(&app_handle, &project_id, "Closing sessions...");
    state.auth_bridge.stop(&project_id).await;
    crate::browser_view::manager().stop(&project_id).await;
    state.exec_manager.close_sessions_for_container(&container_id).await;

    // Stage the payload while the container is still up (docker exec needs it).
    if options.copy_paths && !verbatim.is_empty() {
        emit_progress(
            &app_handle,
            &project_id,
            &format!("Saving {} item(s) from /usr/local, /opt, /srv and /workspace...", verbatim.len()),
        );
        match stage_payload(&container_id, &project_id, &verbatim).await {
            Ok(path) => {
                mstate.staging_path = Some(path);
                migration_store::save(&project_id, &mstate)?;
            }
            Err(e) => {
                // Nothing destructive has happened yet, so bail cleanly.
                let _ = migration_store::clear(&project_id);
                let _ = migration_store::clear_staging(&project_id);
                return Ok(MigrationReport::failed_preflight(format!(
                    "Could not save the files that would be carried across: {}. Nothing was changed.",
                    e
                )));
            }
        }
    }

    emit_progress(&app_handle, &project_id, "Stopping the container...");
    let _ = state
        .projects_store
        .update_status(&project_id, ProjectStatus::Stopping);
    let _ = docker::stop_container(&container_id).await;

    emit_progress(&app_handle, &project_id, "Saving the current image...");
    if let Err(e) = docker::commit_container_snapshot(&container_id, &project).await {
        // The container is stopped but intact and `:latest` is untouched, so
        // the only repair needed is to stop claiming a migration is in flight.
        abandon_before_swap(&project_id, &state, None).await;
        return Ok(MigrationReport::failed_preflight(format!(
            "Could not save the container's current image: {}. Nothing was changed — the container is still on its previous image and can be started as usual.",
            e
        )));
    }

    // The rollback pin. `docker tag` of a 5.49 GB image was measured at 0.036 s
    // and 0 bytes, so this is free to take and only costs disk if it is kept.
    //
    // It is *not* best-effort. The commit above is now the only copy of the old
    // system layer, the next step removes the container, and `finish_migration`
    // repoints `:latest` at the new lineage — so a migration that carries on
    // without a usable pin has quietly made itself irreversible while the UI
    // goes on offering "Roll back". Tag, read it back, and abort while aborting
    // still costs nothing.
    emit_progress(&app_handle, &project_id, "Pinning a rollback image...");
    let (repo, _) = mig::split_image_ref(&snapshot_image);
    let tag = mig::rollback_tag(&chrono::Utc::now());
    let rollback_ref = format!("{}:{}", repo, tag);
    let tag_result = mig::tag_image(&snapshot_image, &repo, &tag).await;
    let resolved = if tag_result.is_ok() {
        mig::image_id(&rollback_ref).await.unwrap_or(None)
    } else {
        None
    };
    if let Err(why) = rollback_pin_verdict(
        tag_result.as_ref().err().map(|e| e.as_str()),
        resolved.as_deref(),
    ) {
        abandon_before_swap(&project_id, &state, Some(&rollback_ref)).await;
        return Ok(MigrationReport::failed_preflight(why));
    }
    mstate.rollback_image = Some(rollback_ref.clone());
    if let Err(e) = migration_store::save(&project_id, &mstate) {
        // A pin nothing has recorded is a pin no recovery path can find, which
        // is the same hole as not having one. Still pre-swap, so stop here.
        abandon_before_swap(&project_id, &state, Some(&rollback_ref)).await;
        return Ok(MigrationReport::failed_preflight(format!(
            "Could not record the rollback image before replacing the container: {}. Nothing was changed — the container is still on its previous image.",
            e
        )));
    }

    emit_progress(&app_handle, &project_id, "Recreating on the new base image...");
    if let Err(e) = docker::remove_container(&container_id).await {
        // Still pre-swap: the old container is the one in place and `:latest`
        // still holds its lineage.
        abandon_before_swap(&project_id, &state, Some(&rollback_ref)).await;
        return Ok(MigrationReport::failed_preflight(format!(
            "Could not remove the old container: {}. Nothing was changed — the container is still on its previous image and can be started as usual.",
            e
        )));
    }

    let docker_socket = settings
        .docker_socket_path
        .clone()
        .unwrap_or_else(default_docker_socket);
    let new_id = match create_container_for_project(
        &project,
        &settings,
        &docker_socket,
        settings.global_aws.aws_config_path.as_deref(),
        &base_image,
        &base_image,
        docker::CreateExtras {
            extra_labels: &[(mig::LABEL_MIGRATION_STATE, mig::MIGRATION_LABEL_IN_PROGRESS)],
        },
    )
    .await
    {
        Ok(id) => id,
        Err(e) => {
            // The old container is gone but `:latest` still holds the old
            // lineage, so putting it back is a plain recreate.
            let report = auto_rollback(&project, &settings, &mstate, &app_handle, &state, &e).await;
            return Ok(report);
        }
    };
    let _ = state
        .projects_store
        .set_container_id(&project_id, Some(new_id.clone()));

    if let Err(e) = docker::start_container(&new_id).await {
        let _ = docker::remove_container(&new_id).await;
        let report = auto_rollback(&project, &settings, &mstate, &app_handle, &state, &e).await;
        return Ok(report);
    }

    finish_migration(project, mstate, new_id, app_handle, state).await
}

async fn resume_migration(
    project_id: String,
    mstate: MigrationState,
    app_handle: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<MigrationReport, String> {
    let mut project = state
        .projects_store
        .get(&project_id)
        .ok_or_else(|| format!("Project {} not found", project_id))?;
    crate::commands::project_commands::load_secrets_for_project(&mut project);

    let container_id = match docker::find_existing_container(&project).await? {
        Some(id) => id,
        None => {
            // The swap never landed. `:latest` is still the old lineage, so the
            // next ordinary start puts the project back exactly as it was.
            let _ = migration_store::clear(&project_id);
            let _ = migration_store::clear_staging(&project_id);
            return Ok(MigrationReport::failed_preflight(
                "The interrupted migration never replaced the container, so there was nothing to resume. The project is unchanged — start it as usual.",
            ));
        }
    };
    // The container being *there* is not evidence that the swap happened. If
    // `commit_container_snapshot` failed, the old, unmigrated container is
    // still in place — and resuming into it would replay the deltas onto the
    // container that already had them, commit it as the migrated image and
    // report success. `reconcile_migration` has always checked this label; the
    // path the Migrate button reaches did not.
    let labelled = container_label(&container_id, mig::LABEL_MIGRATION_STATE)
        .await
        .as_deref()
        == Some(mig::MIGRATION_LABEL_IN_PROGRESS);
    match resume_verdict(&mstate.phase, labelled) {
        ResumeVerdict::Proceed => {}
        ResumeVerdict::RefuseUnswapped => {
            // Exactly the `SelfHeal` case `reconcile_migration` handles, so
            // handle it the same way: the record describes work that never
            // landed, `:latest` still holds the old lineage, and clearing it
            // turns a dead end into "just run the update again".
            if let Some(ref reference) = mstate.rollback_image {
                let _ = mig::untag_image(reference).await;
            }
            let _ = migration_store::clear_staging(&project_id);
            let _ = migration_store::clear(&project_id);
            return Ok(MigrationReport::failed_preflight(
                "This project's container is still the original one — the interrupted update never got as far as replacing it, so there was nothing to resume. Nothing was changed and the record has been cleared: start the project as usual, or run the update again from the beginning.",
            ));
        }
        ResumeVerdict::RefuseNotResumable => {
            return Ok(MigrationReport::failed_preflight(format!(
                "This project's migration record is in the '{}' state, which cannot be resumed. Confirm it or roll it back first.",
                mstate.phase
            )));
        }
    }

    if !docker::is_container_running(&container_id).await.unwrap_or(false) {
        emit_progress(&app_handle, &project_id, "Starting the migrated container...");
        if let Err(e) = docker::start_container(&container_id).await {
            // Leave the record alone — the container is still mid-swap and the
            // user's choices are unchanged — but do not leave the project
            // parked at a transitional status nothing will ever revisit.
            let _ = state
                .projects_store
                .update_status(&project_id, ProjectStatus::Stopped);
            return Ok(MigrationReport {
                phase: MigrationPhase::Failed,
                packages_requested: Vec::new(),
                packages_installed: Vec::new(),
                packages_failed: Vec::new(),
                paths_copied: Vec::new(),
                features_restored: Vec::new(),
                rollback_available: mstate.rollback_image.is_some(),
                message: format!(
                    "The half-migrated container would not start ({}), so the update could not be resumed. Nothing was lost — your home directory and Claude config live in volumes that were never touched. Try again, or roll back.",
                    e
                ),
            });
        }
    }
    emit_progress(&app_handle, &project_id, "Resuming the interrupted migration...");
    finish_migration(project, mstate, container_id, app_handle, state).await
}

/// Replay, restore, probe and commit. Shared by a fresh migration and a resume,
/// so both take exactly the same path from the container swap onward.
async fn finish_migration(
    project: Project,
    mut mstate: MigrationState,
    container_id: String,
    app_handle: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<MigrationReport, String> {
    let project_id = project.id.clone();
    let plan = mstate.plan.clone().unwrap_or_default();
    let options = mstate.options;

    let mut packages_requested: Vec<String> = Vec::new();
    let mut packages_installed: Vec<String> = Vec::new();
    let mut packages_failed: Vec<PackageFailure> = Vec::new();
    let mut paths_copied: Vec<String> = Vec::new();
    let mut notes: Vec<String> = Vec::new();

    if options.replay_packages {
        packages_requested.extend(plan.apt_packages.iter().cloned());
        packages_requested.extend(plan.npm_packages.iter().cloned());

        if !plan.apt_packages.is_empty() {
            emit_progress(
                &app_handle,
                &project_id,
                &format!("Reinstalling {} apt package(s)...", plan.apt_packages.len()),
            );
            let (ok, failed) = replay_apt(&container_id, &plan.apt_packages).await;
            packages_installed.extend(ok);
            packages_failed.extend(failed);
        }
        if !plan.npm_packages.is_empty() {
            emit_progress(
                &app_handle,
                &project_id,
                &format!("Reinstalling {} global npm package(s)...", plan.npm_packages.len()),
            );
            let (ok, failed) = replay_npm(&container_id, &plan.npm_packages).await;
            packages_installed.extend(ok);
            packages_failed.extend(failed);
        }
    }

    if options.copy_paths {
        if let Some(ref staging) = mstate.staging_path {
            if std::path::Path::new(staging).exists() {
                emit_progress(&app_handle, &project_id, "Restoring saved files...");
                match restore_payload(&container_id, staging).await {
                    Ok(()) => paths_copied = plan.verbatim_paths.clone(),
                    Err(e) => notes.push(format!("Some files could not be restored: {}", e)),
                }
            }
        }
    }

    // Probe the new container so the report states what it *actually* gained,
    // rather than what the base was expected to provide.
    emit_progress(&app_handle, &project_id, "Verifying the new container...");
    let mut features_restored: Vec<String> = Vec::new();
    match mig::manifest_from_container(&container_id).await {
        Ok(after) => {
            for path in &plan.missing_paths {
                if after.features.contains(path) {
                    if let Some((_, label)) =
                        mig::FEATURE_PROBES.iter().find(|(p, _)| p == path)
                    {
                        features_restored.push((*label).to_string());
                    }
                }
            }
        }
        Err(e) => notes.push(format!("Could not verify the new container: {}", e)),
    }

    emit_progress(&app_handle, &project_id, "Saving the migrated image...");
    if let Err(e) = docker::commit_container_snapshot(&container_id, &project).await {
        // `:latest` still points at the old lineage, so an ordinary start would
        // quietly undo the migration. Leave the record in place and let the
        // user decide.
        mstate.phase = MIGRATION_PHASE_INTERRUPTED.to_string();
        let report = MigrationReport {
            phase: MigrationPhase::Failed,
            packages_requested,
            packages_installed,
            packages_failed,
            paths_copied,
            features_restored,
            rollback_available: mstate.rollback_image.is_some(),
            message: format!(
                "The container is running on the new base image, but saving it failed: {}. Nothing was lost — your home directory and Claude config live in volumes that were never touched — but the migration is not finished. Resume it, or roll back.",
                e
            ),
        };
        mstate.report = Some(report.clone());
        let _ = migration_store::save(&project_id, &mstate);
        // The container really is up, so say so. Left at `Stopping` this
        // project would never be re-examined: `reconcile_project_statuses`
        // only looks at `Running` and `Error`.
        let _ = state
            .projects_store
            .update_status(&project_id, ProjectStatus::Running);
        return Ok(report);
    }

    // The staged tar has done its job and can be several GB.
    let _ = migration_store::clear_staging(&project_id);
    mstate.staging_path = None;

    // `keep_rollback` is the disk trade: snapshots share almost nothing with the
    // current base (3 of 31 layers measured), so a retained rollback holds
    // roughly a whole snapshot — 3.8 to 12.3 GB on real projects. Off means the
    // pin is dropped the moment the migration is known to have worked.
    let mut rollback_available = mstate.rollback_image.is_some();
    if !options.keep_rollback {
        if let Some(ref reference) = mstate.rollback_image.clone() {
            match mig::untag_image(reference).await {
                Ok(()) => {
                    mstate.rollback_image = None;
                    rollback_available = false;
                }
                Err(e) => log::warn!("Could not drop the rollback tag {}: {}", reference, e),
            }
        }
    }

    let phase = if packages_failed.is_empty() && notes.is_empty() {
        MigrationPhase::Succeeded
    } else {
        MigrationPhase::Partial
    };
    let report = MigrationReport {
        message: summarize(
            phase,
            &packages_installed,
            &packages_failed,
            &paths_copied,
            &features_restored,
            &notes,
            rollback_available,
            &plan.unpreserved_data,
        ),
        phase,
        packages_requested,
        packages_installed,
        packages_failed,
        paths_copied,
        features_restored,
        rollback_available,
    };

    mstate.phase = MIGRATION_PHASE_AWAITING.to_string();
    mstate.report = Some(report.clone());
    // Before the save, not after: a failed save must not leave the project
    // parked at `Stopping` with a container that is plainly running.
    let _ = state
        .projects_store
        .update_status(&project_id, ProjectStatus::Running);
    migration_store::save(&project_id, &mstate)?;

    emit_progress(&app_handle, &project_id, "Migration finished.");
    Ok(report)
}

// ─────────────────────────────────────────────────────────────────────────────
// Confirm / rollback / state
// ─────────────────────────────────────────────────────────────────────────────

/// Accept a finished migration: drop the rollback tag and the staged payload,
/// and clear the migration record. Idempotent.
#[tauri::command]
pub async fn confirm_migration(
    project_id: String,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let _ = &state;
    // Confirming drops the only way back. Doing that underneath a running
    // migration would delete the pin it is relying on mid-flight.
    let Some(_guard) = ActiveGuard::acquire(&project_id) else {
        return Err(
            "A container base update is running for this project right now. Wait for it to finish."
                .to_string(),
        );
    };
    let Some(mstate) = migration_store::load(&project_id)? else {
        return Ok(());
    };
    // An unfinished migration is not a thing that can be accepted: `:latest`
    // still points at the old lineage, so dropping the pin and the record here
    // would strand a container the app can no longer reason about.
    if mstate.phase == MIGRATION_PHASE_INTERRUPTED
        || mstate.phase == crate::models::MIGRATION_PHASE_IN_PROGRESS
    {
        return Err(
            "This project's container base update never finished, so there is nothing to accept yet. Resume it or roll it back."
                .to_string(),
        );
    }
    if let Some(ref reference) = mstate.rollback_image {
        if let Err(e) = mig::untag_image(reference).await {
            log::warn!("Could not drop the rollback tag {}: {}", reference, e);
        }
    }
    migration_store::clear_staging(&project_id)?;
    migration_store::clear(&project_id)?;
    log::info!("Migration confirmed for project {}", project_id);

    // Dropping the pin above is what turns the pre-migration image into an
    // orphan: it was the only tag holding a multi-gigabyte pre-migration
    // snapshot. Accepting the update is therefore the moment to sweep, and
    // waiting for the project's next recreation would leave it lying around
    // indefinitely.
    tauri::async_runtime::spawn(async {
        crate::docker::sweep_orphaned_snapshots().await;
    });

    Ok(())
}

/// Undo a migration: put the container back on its pre-migration image.
///
/// Restores the **system layer only**. Both named volumes were untouched
/// throughout, so anything written to `$HOME` while the migrated container was
/// running — logins, skills, transcripts, scheduler tasks — survives.
#[tauri::command]
pub async fn rollback_migration(
    project_id: String,
    app_handle: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let Some(_guard) = ActiveGuard::acquire(&project_id) else {
        return Err(
            "A container base update is running for this project right now. Wait for it to finish."
                .to_string(),
        );
    };

    let mut project = state
        .projects_store
        .get(&project_id)
        .ok_or_else(|| format!("Project {} not found", project_id))?;
    crate::commands::project_commands::load_secrets_for_project(&mut project);
    let settings = state.settings_store.get();

    let Some(mstate) = migration_store::load(&project_id)? else {
        return Err("There is no migration to roll back for this project.".to_string());
    };
    let Some(rollback_ref) = mstate.rollback_image.clone() else {
        return Err(
            "This migration kept no rollback image, so it cannot be undone. Nothing was changed."
                .to_string(),
        );
    };
    // Check the image is really there *before* removing the container. The
    // record only says a tag was created; a prune, a `docker rmi` or a failed
    // tag from an older build can all leave the reference dangling, and by the
    // time the container is gone there is nothing left to put back.
    if mig::image_id(&rollback_ref).await?.is_none() {
        return Err(format!(
            "The rollback image '{}' no longer exists, so this update cannot be undone — it may have been pruned. Nothing was changed: the container is still running on the new base.",
            rollback_ref
        ));
    }

    emit_progress(&app_handle, &project_id, "Rolling back...");
    state.auth_bridge.stop(&project_id).await;
    crate::browser_view::manager().stop(&project_id).await;
    if let Some(id) = docker::find_existing_container(&project).await? {
        state.exec_manager.close_sessions_for_container(&id).await;
        let _ = docker::stop_container(&id).await;
        docker::remove_container(&id).await?;
    }

    let snapshot_image = docker::get_snapshot_image_name(&project);
    let (repo, tag) = mig::split_image_ref(&snapshot_image);
    mig::tag_image(&rollback_ref, &repo, &tag).await?;

    let base_image = crate::models::container_config::resolve_image_name(
        &settings.image_source,
        &settings.custom_image_name,
    );
    let docker_socket = settings
        .docker_socket_path
        .clone()
        .unwrap_or_else(default_docker_socket);
    let new_id = create_container_for_project(
        &project,
        &settings,
        &docker_socket,
        settings.global_aws.aws_config_path.as_deref(),
        &snapshot_image,
        &base_image,
        docker::CreateExtras::default(),
    )
    .await?;
    docker::start_container(&new_id).await?;
    state
        .projects_store
        .set_container_id(&project_id, Some(new_id))?;
    state
        .projects_store
        .update_status(&project_id, ProjectStatus::Running)?;

    // The rollback image is now `:latest` again; the extra tag is redundant.
    let _ = mig::untag_image(&rollback_ref).await;
    migration_store::clear_staging(&project_id)?;
    migration_store::clear(&project_id)?;
    emit_progress(
        &app_handle,
        &project_id,
        "Rolled back. Your home directory and Claude config were never touched.",
    );
    Ok(())
}

/// The persisted migration record, if one exists.
#[tauri::command]
pub async fn get_migration_state(
    project_id: String,
    state: State<'_, AppState>,
) -> Result<Option<MigrationState>, String> {
    let _ = &state;
    migration_store::load(&project_id)
}

// ─────────────────────────────────────────────────────────────────────────────
// Crash recovery, called from reconcile_project_statuses
// ─────────────────────────────────────────────────────────────────────────────

/// Reconcile one project's persisted migration record against reality.
///
/// Called from `reconcile_project_statuses`, which already runs at startup.
/// Self-healing cases are cleaned up silently; anything needing a decision is
/// left in place with its phase normalised to `interrupted` so the UI can offer
/// resume or rollback.
pub async fn reconcile_migration(project: &Project, app_handle: &tauri::AppHandle) {
    // A migration running right now is indistinguishable from a crashed one
    // from the outside; only this process knows the difference.
    if is_migrating(&project.id) {
        return;
    }
    let state = match migration_store::load(&project.id) {
        Ok(Some(s)) => s,
        Ok(None) => return,
        Err(e) => {
            log::warn!("Could not read migration state for {}: {}", project.id, e);
            return;
        }
    };

    let labelled = match docker::find_existing_container(project).await {
        Ok(Some(id)) => container_label(&id, mig::LABEL_MIGRATION_STATE).await.as_deref()
            == Some(mig::MIGRATION_LABEL_IN_PROGRESS),
        _ => false,
    };

    match mig::decide_recovery(Some(state.phase.as_str()), labelled) {
        Recovery::None => {}
        Recovery::SelfHeal => {
            log::info!(
                "Project '{}' ({}) has a migration record from before the container was replaced — the snapshot still holds the old image, so it self-heals",
                project.name,
                project.id
            );
            if let Some(ref reference) = state.rollback_image {
                let _ = mig::untag_image(reference).await;
            }
            let _ = migration_store::clear_staging(&project.id);
            let _ = migration_store::clear(&project.id);
        }
        Recovery::OfferResumeOrRollback => {
            if state.phase != MIGRATION_PHASE_INTERRUPTED {
                let mut s = state.clone();
                s.phase = MIGRATION_PHASE_INTERRUPTED.to_string();
                let _ = migration_store::save(&project.id, &s);
            }
            log::warn!(
                "Project '{}' ({}) has an unfinished base-image migration — offering resume or rollback",
                project.name,
                project.id
            );
            emit_progress(
                app_handle,
                &project.id,
                "An unfinished base-image migration was found. Resume it or roll it back.",
            );
        }
        Recovery::OfferConfirmOrRollback => {
            log::info!(
                "Project '{}' ({}) has a finished migration awaiting confirmation",
                project.name,
                project.id
            );
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Internals
// ─────────────────────────────────────────────────────────────────────────────

/// Whether the rollback pin can be relied on for the rest of the migration.
///
/// Pure so the decision is testable without Docker. Both failure shapes matter:
/// `docker tag` can fail outright, and it can also "succeed" against an image
/// that is not there any more (a concurrent prune), in which case reading the
/// new reference back is the only thing that catches it. Either way the answer
/// is to stop *before* `remove_container`, which is the last moment at which
/// stopping is free.
fn rollback_pin_verdict(
    tag_error: Option<&str>,
    resolved_image_id: Option<&str>,
) -> Result<(), String> {
    if let Some(e) = tag_error {
        return Err(format!(
            "Could not pin a rollback image, so the update was stopped before it could become irreversible: {}. Nothing was changed — the container is still on its previous image. Free some disk space or check the Docker daemon, then try again.",
            e
        ));
    }
    match resolved_image_id {
        Some(id) if !id.is_empty() => Ok(()),
        _ => Err(
            "The rollback image was tagged but could not be read back, so rolling this update back could not be guaranteed. The update was stopped before it could become irreversible — nothing was changed, and the container is still on its previous image."
                .to_string(),
        ),
    }
}

/// Undo the *bookkeeping* of a migration that gave up before the container
/// swap. Everything real is still in place: the container exists, `:latest`
/// still holds the old lineage, and the volumes were never involved.
///
/// The status reset is the load-bearing part. `reconcile_project_statuses`
/// only ever re-examines projects it finds in `Running` or `Error`, so a
/// project abandoned at `Stopping` stays there for good — the Start button
/// stays disabled and nothing in the app ever puts it right.
async fn abandon_before_swap(
    project_id: &str,
    state: &State<'_, AppState>,
    rollback_ref: Option<&str>,
) {
    if let Some(reference) = rollback_ref {
        let _ = mig::untag_image(reference).await;
    }
    let _ = migration_store::clear_staging(project_id);
    let _ = migration_store::clear(project_id);
    let _ = state
        .projects_store
        .update_status(project_id, ProjectStatus::Stopped);
}

/// What a user-initiated resume is allowed to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ResumeVerdict {
    /// The swap landed — the container in place is the migrated one.
    Proceed,
    /// The container in place is the **original**. The swap never happened
    /// (typically `commit_container_snapshot` failed), so replaying into it and
    /// committing the result would silently stamp the unmigrated container as
    /// migrated and report success.
    RefuseUnswapped,
    /// The record is not one a resume can act on at all.
    RefuseNotResumable,
}

/// Gate `resume_migration` on the same two signals `reconcile_migration` uses.
///
/// `reconcile_migration` has always checked the container's
/// `triple-c.migration-state` label; the frontend-callable resume path did not,
/// and that asymmetry is the bug: clicking Migrate on a record left behind by a
/// failed commit "resumed" into the old container.
fn resume_verdict(phase: &str, container_has_in_progress_label: bool) -> ResumeVerdict {
    match mig::decide_recovery(Some(phase), container_has_in_progress_label) {
        Recovery::OfferResumeOrRollback => ResumeVerdict::Proceed,
        Recovery::SelfHeal => ResumeVerdict::RefuseUnswapped,
        Recovery::None | Recovery::OfferConfirmOrRollback => ResumeVerdict::RefuseNotResumable,
    }
}

/// Drop every host- and Docker-side trace of a migration for a project that is
/// being destroyed or reset.
///
/// Reset and Remove both delete the snapshot image and the volumes, so a
/// surviving migration record can only describe things that no longer exist —
/// and its `:pre-migration-<ts>` tag would hold an entire snapshot image (3.8
/// to 12.3 GB measured) alive with nothing left that could ever use it. The
/// staged payload tar is the same story at a smaller scale.
pub(crate) async fn purge_migration_artifacts(project_id: &str) {
    match migration_store::load(project_id) {
        Ok(Some(state)) => {
            if let Some(ref reference) = state.rollback_image {
                if let Err(e) = mig::untag_image(reference).await {
                    log::warn!("Could not drop the rollback tag {}: {}", reference, e);
                }
            }
        }
        Ok(None) => return,
        Err(e) => log::warn!(
            "Could not read the migration record for {} while cleaning up: {}",
            project_id,
            e
        ),
    }
    let _ = migration_store::clear_staging(project_id);
    let _ = migration_store::clear(project_id);
}

fn default_docker_socket() -> String {
    if cfg!(target_os = "windows") {
        "//./pipe/docker_engine".to_string()
    } else {
        "/var/run/docker.sock".to_string()
    }
}

fn human_bytes(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{} {}", n, UNITS[0])
    } else {
        format!("{:.1} {}", v, UNITS[i])
    }
}

/// Create (or truncate) a file that only the current user can read.
///
/// On Unix the mode goes on at `open(2)` time so there is no window in which
/// the file exists world-readable. Windows has no equivalent bit and inherits
/// the directory ACL, which is already per-user under `%APPDATA%`.
async fn create_private_file(path: &std::path::Path) -> std::io::Result<tokio::fs::File> {
    let mut opts = tokio::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        // `tokio::fs::OpenOptions::mode` is the inherent unix method — no
        // `OpenOptionsExt` import needed, and it is applied at `open(2)` time.
        opts.mode(0o600);
    }
    opts.open(path).await
}

/// Tar the verbatim set out of the running container into a host file.
///
/// Mirrors `download_container_backup`'s stream-an-exec's-stdout-to-a-file
/// pattern. The member list goes in via a file rather than argv, because a
/// large `/usr/local/lib` tree can produce more paths than `execve` will take.
async fn stage_payload(
    container_id: &str,
    project_id: &str,
    verbatim: &[String],
) -> Result<String, String> {
    use bollard::container::LogOutput;
    use futures_util::StreamExt;
    use tokio::io::AsyncWriteExt;

    let members = mig::tar_member_names(verbatim);
    if members.is_empty() {
        return Err("nothing to stage".to_string());
    }
    let list = format!("{}\n", members.join("\n"));
    let list_path = docker::exec::upload_bytes_to_container(
        container_id,
        "/tmp",
        "triple-c-migrate-include.txt",
        list.as_bytes(),
        0o600,
    )
    .await?;

    let host_path = migration_store::staging_path(project_id)?;
    let host_path_str = host_path.to_string_lossy().to_string();

    // `--ignore-failed-read` keeps a file that vanishes mid-walk from aborting
    // the archive. `--numeric-owner` because uid/gid are remapped to the host
    // user at runtime and names may not resolve in the new base.
    let cmd = vec![
        "tar".to_string(),
        "-cf".to_string(),
        "-".to_string(),
        "--numeric-owner".to_string(),
        "--ignore-failed-read".to_string(),
        "-C".to_string(),
        "/".to_string(),
        "-T".to_string(),
        list_path,
    ];
    let exec = docker::exec::create_attached_exec_as(container_id, cmd, false, "root", "/").await?;
    let mut output = exec.output;

    // 0600, not the default umask. This tar holds the whole of `/usr/local`,
    // `/opt`, `/srv` and every loose `/workspace` file — private keys, tokens
    // in scripts, whatever the user put there — sitting in a predictable path
    // under the data directory, possibly for the length of a long migration.
    // World-readable is the wrong default for that.
    let file = create_private_file(&host_path)
        .await
        .map_err(|e| format!("Failed to create the staging file: {}", e))?;
    let mut writer = tokio::io::BufWriter::new(file);
    let mut total: u64 = 0;
    let mut stderr_text = String::new();
    let mut stream_err: Option<String> = None;

    while let Some(msg) = output.next().await {
        match msg {
            Ok(LogOutput::StdOut { message }) => {
                if let Err(e) = writer.write_all(&message).await {
                    stream_err = Some(format!("Failed to write the staging file: {}", e));
                    break;
                }
                total += message.len() as u64;
            }
            Ok(LogOutput::StdErr { message }) => {
                stderr_text.push_str(&String::from_utf8_lossy(&message));
            }
            Ok(_) => {}
            Err(e) => {
                stream_err = Some(format!("Staging stream error: {}", e));
                break;
            }
        }
    }
    if stream_err.is_none() {
        if let Err(e) = writer.flush().await {
            stream_err = Some(format!("Failed to finalize the staging file: {}", e));
        }
    }
    drop(writer);

    // A tar that aborts mid-stream still emits bytes, so a non-zero exit has to
    // beat `total > 0`.
    let exit_code = docker::exec::wait_for_exec_exit(&exec.exec_id).await;
    if stream_err.is_none() && exit_code.is_some_and(|c| c != 0) {
        stream_err = Some(format!(
            "tar failed (exit {}){}",
            exit_code.unwrap_or(-1),
            if stderr_text.trim().is_empty() {
                String::new()
            } else {
                format!(": {}", stderr_text.trim())
            }
        ));
    }
    if stream_err.is_none() && total == 0 {
        stream_err = Some("tar produced no data".to_string());
    }

    if let Some(err) = stream_err {
        let _ = tokio::fs::remove_file(&host_path).await;
        return Err(err);
    }

    log::info!(
        "Staged {} bytes of migration payload for project {} to {}",
        total,
        project_id,
        host_path_str
    );
    Ok(host_path_str)
}

/// Stream a staged payload back into the new container.
///
/// `--skip-old-files` is the never-clobber guarantee: a copied file can never
/// replace a newer binary the base already ships. (GNU tar's `--keep-old-files`
/// refuses just as firmly but reports every pre-existing file as an error and
/// exits non-zero, which would make the ordinary, expected outcome
/// indistinguishable from a real failure. The payload is built to exclude
/// anything already present in the base, so collisions should be rare either
/// way.)
async fn restore_payload(container_id: &str, host_path: &str) -> Result<(), String> {
    use bollard::container::LogOutput;
    use futures_util::StreamExt;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let cmd = vec![
        "tar".to_string(),
        "-xf".to_string(),
        "-".to_string(),
        "--skip-old-files".to_string(),
        "--numeric-owner".to_string(),
        "-p".to_string(),
        "-C".to_string(),
        "/".to_string(),
    ];
    let exec = docker::exec::create_attached_exec_as(container_id, cmd, false, "root", "/").await?;
    let mut input = exec.input;
    let mut output = exec.output;

    let mut file = tokio::fs::File::open(host_path)
        .await
        .map_err(|e| format!("Failed to open the staged payload: {}", e))?;

    // Drain stderr concurrently: a big payload can fill the exec's output pipe
    // and deadlock the write below if nothing is reading.
    let drain = tokio::spawn(async move {
        let mut text = String::new();
        while let Some(msg) = output.next().await {
            match msg {
                Ok(LogOutput::StdErr { message }) | Ok(LogOutput::StdOut { message }) => {
                    text.push_str(&String::from_utf8_lossy(&message))
                }
                Ok(_) => {}
                Err(_) => break,
            }
        }
        text
    });

    let mut buf = vec![0u8; 256 * 1024];
    loop {
        let n = file
            .read(&mut buf)
            .await
            .map_err(|e| format!("Failed to read the staged payload: {}", e))?;
        if n == 0 {
            break;
        }
        input
            .write_all(&buf[..n])
            .await
            .map_err(|e| format!("Failed to send the payload into the container: {}", e))?;
    }
    input
        .shutdown()
        .await
        .map_err(|e| format!("Failed to close the payload stream: {}", e))?;
    drop(input);

    let stderr_text = drain.await.unwrap_or_default();
    let exit_code = docker::exec::wait_for_exec_exit(&exec.exec_id).await;
    if exit_code.is_some_and(|c| c != 0) {
        return Err(format!(
            "tar exited {}{}",
            exit_code.unwrap_or(-1),
            if stderr_text.trim().is_empty() {
                String::new()
            } else {
                format!(": {}", stderr_text.trim())
            }
        ));
    }
    Ok(())
}

/// Replay apt packages: one transaction, then per-package on failure.
///
/// A single unavailable package must never cost the whole migration, which is
/// why the bulk failure falls through to a loop instead of aborting. Replaying
/// eight packages onto the current base was measured at 69.8 s, exit 0.
async fn replay_apt(container_id: &str, packages: &[String]) -> (Vec<String>, Vec<PackageFailure>) {
    let update = format!(
        "DEBIAN_FRONTEND=noninteractive apt-get -o Acquire::Retries=3 update"
    );
    let _ = run_root(container_id, &update).await;

    let bulk = format!(
        "DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends {}",
        packages
            .iter()
            .map(|p| shell_quote(p))
            .collect::<Vec<_>>()
            .join(" ")
    );
    match run_root(container_id, &bulk).await {
        Ok((_, 0)) => return (packages.to_vec(), Vec::new()),
        Ok((out, code)) => log::warn!(
            "Bulk apt replay failed (exit {}), falling back to one package at a time: {}",
            code,
            tail(&out)
        ),
        Err(e) => log::warn!("Bulk apt replay could not run ({}), falling back", e),
    }

    let mut ok = Vec::new();
    let mut failed = Vec::new();
    for pkg in packages {
        let cmd = format!(
            "DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends {}",
            shell_quote(pkg)
        );
        match run_root(container_id, &cmd).await {
            Ok((_, 0)) => ok.push(pkg.clone()),
            Ok((out, code)) => failed.push(PackageFailure {
                name: pkg.clone(),
                reason: format!("apt-get exited {}: {}", code, tail(&out)),
            }),
            Err(e) => failed.push(PackageFailure {
                name: pkg.clone(),
                reason: e,
            }),
        }
    }
    (ok, failed)
}

/// Replay global npm packages. npm's prefix in this image is `/usr`, so these
/// live in the container's writable layer and really are lost on an image swap.
async fn replay_npm(container_id: &str, packages: &[String]) -> (Vec<String>, Vec<PackageFailure>) {
    let bulk = format!(
        "npm install -g --no-fund --no-audit {}",
        packages
            .iter()
            .map(|p| shell_quote(p))
            .collect::<Vec<_>>()
            .join(" ")
    );
    match run_root(container_id, &bulk).await {
        Ok((_, 0)) => return (packages.to_vec(), Vec::new()),
        Ok((out, code)) => log::warn!(
            "Bulk npm replay failed (exit {}), falling back to one package at a time: {}",
            code,
            tail(&out)
        ),
        Err(e) => log::warn!("Bulk npm replay could not run ({}), falling back", e),
    }

    let mut ok = Vec::new();
    let mut failed = Vec::new();
    for pkg in packages {
        let cmd = format!("npm install -g --no-fund --no-audit {}", shell_quote(pkg));
        match run_root(container_id, &cmd).await {
            Ok((_, 0)) => ok.push(pkg.clone()),
            Ok((out, code)) => failed.push(PackageFailure {
                name: pkg.clone(),
                reason: format!("npm exited {}: {}", code, tail(&out)),
            }),
            Err(e) => failed.push(PackageFailure {
                name: pkg.clone(),
                reason: e,
            }),
        }
    }
    (ok, failed)
}

async fn run_root(container_id: &str, script: &str) -> Result<(String, i64), String> {
    docker::exec::exec_oneshot_as(
        container_id,
        "root",
        vec!["/bin/sh".to_string(), "-c".to_string(), script.to_string()],
        Vec::new(),
    )
    .await
}

fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r#"'\''"#))
}

/// The last ~400 bytes of a package manager's output, for a failure reason.
///
/// The offset is a **byte** offset into arbitrary apt-get/npm output, which
/// routinely contains multi-byte UTF-8 (mirror names, `‘quoted’` package names,
/// progress glyphs). Slicing straight at `len - 400` panics the moment that
/// lands inside a character — turning a reportable per-package failure into a
/// crash of the whole migration. Walk forward to the next boundary instead.
fn tail(s: &str) -> String {
    const LIMIT: usize = 400;
    let t = s.trim();
    if t.len() <= LIMIT {
        return t.to_string();
    }
    let mut start = t.len() - LIMIT;
    while start < t.len() && !t.is_char_boundary(start) {
        start += 1;
    }
    t[start..].to_string()
}

/// Put the container back after a failure that happened *after* removal but
/// *before* the migration could finish. `:latest` still holds the old lineage
/// at that point, so this is an ordinary recreate.
async fn auto_rollback(
    project: &Project,
    settings: &crate::models::AppSettings,
    mstate: &MigrationState,
    app_handle: &tauri::AppHandle,
    state: &State<'_, AppState>,
    cause: &str,
) -> MigrationReport {
    emit_progress(
        app_handle,
        &project.id,
        "Migration failed — putting the previous container back...",
    );
    let snapshot_image = docker::get_snapshot_image_name(project);
    let base_image = crate::models::container_config::resolve_image_name(
        &settings.image_source,
        &settings.custom_image_name,
    );
    let docker_socket = settings
        .docker_socket_path
        .clone()
        .unwrap_or_else(default_docker_socket);

    let mut restored = false;
    match create_container_for_project(
        project,
        settings,
        &docker_socket,
        settings.global_aws.aws_config_path.as_deref(),
        &snapshot_image,
        &base_image,
        docker::CreateExtras::default(),
    )
    .await
    {
        Ok(id) => {
            if let Err(e) = docker::start_container(&id).await {
                log::error!("Rollback container would not start: {}", e);
            } else {
                restored = true;
            }
            let _ = state
                .projects_store
                .set_container_id(&project.id, Some(id));
            // Report what is actually true: a container that was recreated but
            // would not start is Stopped, not Running.
            let _ = state.projects_store.update_status(
                &project.id,
                if restored {
                    ProjectStatus::Running
                } else {
                    ProjectStatus::Stopped
                },
            );
        }
        Err(e) => {
            log::error!("Could not recreate the previous container: {}", e);
            // The status is `Stopping` at this point and nothing else will
            // revisit it — `reconcile_project_statuses` only re-examines
            // `Running` and `Error`. There is no container, so `Stopped` is
            // both true and the state the Start button needs.
            let _ = state
                .projects_store
                .update_status(&project.id, ProjectStatus::Stopped);
        }
    }

    if let Some(ref reference) = mstate.rollback_image {
        let _ = mig::untag_image(reference).await;
    }
    let _ = migration_store::clear_staging(&project.id);
    let _ = migration_store::clear(&project.id);

    MigrationReport {
        phase: MigrationPhase::RolledBack,
        packages_requested: Vec::new(),
        packages_installed: Vec::new(),
        packages_failed: Vec::new(),
        paths_copied: Vec::new(),
        features_restored: Vec::new(),
        rollback_available: false,
        message: if restored {
            format!(
                "The migration failed ({}) and the previous container has been put back. Your home directory and Claude config were never touched.",
                cause
            )
        } else {
            format!(
                "The migration failed ({}) and the previous container could not be restarted automatically. Its image is intact — start the project again to recreate it. Your home directory and Claude config were never touched.",
                cause
            )
        },
    }
}

fn summarize(
    phase: MigrationPhase,
    installed: &[String],
    failed: &[PackageFailure],
    copied: &[String],
    features: &[String],
    notes: &[String],
    rollback_available: bool,
    unpreserved: &[UnpreservedData],
) -> String {
    let mut parts: Vec<String> = Vec::new();
    parts.push(match phase {
        MigrationPhase::Succeeded => "This project now runs on the current base image.".to_string(),
        _ => "This project now runs on the current base image, with some gaps.".to_string(),
    });
    if !features.is_empty() {
        parts.push(format!("Gained: {}.", features.join(", ")));
    }
    if !installed.is_empty() {
        parts.push(format!("Reinstalled {} package(s).", installed.len()));
    }
    if !copied.is_empty() {
        parts.push(format!("Carried across {} item(s).", copied.len()));
    }
    if !failed.is_empty() {
        let names: Vec<&str> = failed.iter().map(|f| f.name.as_str()).take(5).collect();
        parts.push(format!(
            "{} package(s) could not be reinstalled ({}{}).",
            failed.len(),
            names.join(", "),
            if failed.len() > names.len() { ", …" } else { "" }
        ));
    }
    for note in notes {
        parts.push(note.clone());
    }
    // Stated in the outcome as well as the pre-flight. A user who clicked
    // through the warning still has to be told, in the record that persists,
    // which directories are now empty — silence here is how someone discovers
    // an empty database a week later.
    if !unpreserved.is_empty() {
        parts.push(format!(
            "Service data under {} was not carried across and cannot be restored by reinstalling the package{}.",
            unpreserved
                .iter()
                .map(|d| d.path.as_str())
                .collect::<Vec<_>>()
                .join(", "),
            if rollback_available {
                " — roll back if you needed it"
            } else {
                ""
            }
        ));
    }
    parts.push(
        "Your home directory and Claude config live in volumes that were never touched, so the login, skills, transcripts and scheduled tasks are exactly as they were."
            .to_string(),
    );
    parts.push(if rollback_available {
        "Roll back at any time until you confirm.".to_string()
    } else {
        "No rollback image was kept, so this cannot be undone.".to_string()
    });
    parts.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn byte_sizes_read_the_way_a_disk_warning_should() {
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(2 * 1024), "2.0 KiB");
        assert_eq!(human_bytes(5_368_709_120), "5.0 GiB");
    }

    #[test]
    fn package_names_cannot_break_out_of_the_replay_shell_command() {
        assert_eq!(shell_quote("socat"), "'socat'");
        assert_eq!(shell_quote("a'; rm -rf /"), r#"'a'\''; rm -rf /'"#);
    }

    #[test]
    fn the_summary_always_says_the_volumes_were_untouched() {
        let msg = summarize(
            MigrationPhase::Succeeded,
            &["socat".to_string()],
            &[],
            &[],
            &["Auth bridge tunnel (socat)".to_string()],
            &[],
            true,
            &[],
        );
        assert!(msg.contains("never touched"));
        assert!(msg.contains("Auth bridge tunnel (socat)"));
        assert!(msg.contains("Roll back at any time"));

        let msg = summarize(
            MigrationPhase::Partial,
            &[],
            &[PackageFailure {
                name: "obsolete-pkg".to_string(),
                reason: "not found".to_string(),
            }],
            &[],
            &[],
            &[],
            false,
            &[],
        );
        assert!(msg.contains("obsolete-pkg"));
        assert!(msg.contains("cannot be undone"));
        assert!(msg.contains("never touched"));
    }

    #[test]
    fn the_summary_names_the_var_data_the_migration_destroyed() {
        // The pre-flight warned about it; the record that persists has to as
        // well, or the only trace of an emptied database is a dialog the user
        // clicked through minutes ago.
        let msg = summarize(
            MigrationPhase::Succeeded,
            &["postgresql".to_string()],
            &[],
            &[],
            &[],
            &[],
            true,
            &[UnpreservedData {
                path: "/var/lib/postgresql".to_string(),
                bytes: 41_000_000,
                file_count: 912,
            }],
        );
        assert!(msg.contains("/var/lib/postgresql"));
        assert!(msg.contains("cannot be restored by reinstalling the package"));
    }

    // ── The rollback pin is not best-effort ─────────────────────────────────

    #[test]
    fn a_migration_refuses_to_continue_without_a_verified_rollback_pin() {
        // By this point the commit is the only copy of the old system layer and
        // `remove_container` is next, so anything short of a tag that reads
        // back has to stop the migration rather than warn and carry on.
        assert!(rollback_pin_verdict(None, Some("sha256:abc")).is_ok());

        let err = rollback_pin_verdict(Some("no space left on device"), None).unwrap_err();
        assert!(err.contains("no space left on device"));
        assert!(err.contains("Nothing was changed"));

        // Tagged, but the reference does not resolve — a concurrent prune, or a
        // daemon that accepted the call and did nothing.
        let err = rollback_pin_verdict(None, None).unwrap_err();
        assert!(err.contains("could not be read back"));
        assert!(err.contains("nothing was changed"));
        assert!(rollback_pin_verdict(None, Some("")).is_err());
    }

    // ── Resume must prove the swap actually happened ────────────────────────

    #[test]
    fn resuming_refuses_when_the_container_is_still_the_unmigrated_one() {
        use crate::models::MIGRATION_PHASE_IN_PROGRESS;

        // The label is the only evidence the swap landed. Without it the
        // container in place is the original: replaying into it and committing
        // would stamp an unmigrated container as migrated and report success.
        assert_eq!(
            resume_verdict(MIGRATION_PHASE_INTERRUPTED, false),
            ResumeVerdict::RefuseUnswapped
        );
        assert_eq!(
            resume_verdict(MIGRATION_PHASE_IN_PROGRESS, false),
            ResumeVerdict::RefuseUnswapped
        );
        assert_eq!(
            resume_verdict(MIGRATION_PHASE_INTERRUPTED, true),
            ResumeVerdict::Proceed
        );
        assert_eq!(
            resume_verdict(MIGRATION_PHASE_IN_PROGRESS, true),
            ResumeVerdict::Proceed
        );
        // A finished migration is a confirm/rollback decision, never a resume.
        assert_eq!(
            resume_verdict(MIGRATION_PHASE_AWAITING, true),
            ResumeVerdict::RefuseNotResumable
        );
        assert_eq!(
            resume_verdict("who-knows", true),
            ResumeVerdict::RefuseNotResumable
        );
    }

    // ── tail() ──────────────────────────────────────────────────────────────

    #[test]
    fn the_failure_tail_never_splits_a_multibyte_character() {
        // apt-get quotes package names with ‘…’ and mirrors have non-ASCII
        // names, so the 400-byte cut lands mid-character routinely. The old
        // slice panicked, turning one reportable package failure into a crash
        // of the whole migration.
        for pad in 0..8 {
            let s = format!("{}{}", "x".repeat(pad), "é".repeat(400));
            let t = tail(&s);
            assert!(s.ends_with(&t), "tail must be a suffix of the input");
            assert!(t.len() <= 400 + 1);
        }
        // Short input is returned whole, trimmed.
        assert_eq!(tail("  apt-get exited 100  "), "apt-get exited 100");
        assert_eq!(tail(""), "");
        // A single character wider than the window is still returned intact.
        let wide = "🐳".repeat(200);
        assert!(wide.ends_with(&tail(&wide)));
    }

    #[test]
    fn a_live_migration_is_distinguishable_from_a_crashed_one() {
        // The whole point: reconcile cannot tell them apart from the outside,
        // so an in-process marker is the only thing that can.
        let id = "guard-test-project";
        assert!(!is_migrating(id));
        {
            let g = ActiveGuard::acquire(id).expect("first acquire must succeed");
            assert!(is_migrating(id));
            assert!(
                ActiveGuard::acquire(id).is_none(),
                "a second concurrent migration must be refused"
            );
            drop(g);
        }
        assert!(!is_migrating(id), "the guard must release on drop");
        // …including when the migration bailed out through an early return.
        fn early_return(id: &str) -> Option<()> {
            let _g = ActiveGuard::acquire(id)?;
            None
        }
        assert!(early_return(id).is_none());
        assert!(!is_migrating(id));
    }

    #[test]
    fn a_preflight_failure_reports_no_rollback_because_nothing_was_touched() {
        let r = MigrationReport::failed_preflight("nope");
        assert_eq!(r.phase, MigrationPhase::Failed);
        assert!(!r.rollback_available);
        assert!(r.packages_requested.is_empty());
    }
}
