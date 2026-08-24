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
/// Choose the recorded lineage from the two places it can be written, most
/// authoritative first: the live container's label, then the snapshot image's.
///
/// **An empty label is absence, not an answer.** `create_container` always
/// writes `triple-c.base-image-id`, even when the value is unknown — that is
/// deliberate, because Docker merges an image's labels into a container's and
/// an inherited value would otherwise ride a snapshot forever. The consequence
/// is that `Some("")` is the *common* reading from a container whose lineage
/// was never established, so treating it as an answer silently skips the
/// snapshot, which may well have recorded a real one.
fn pick_recorded_lineage(
    from_container: Option<String>,
    from_snapshot: Option<String>,
) -> Option<String> {
    from_container
        .filter(|v| !v.is_empty())
        .or_else(|| from_snapshot.filter(|v| !v.is_empty()))
}

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
    // Each source is filtered for emptiness *before* it is allowed to satisfy
    // the lookup. `create_container` always writes this label, even when the
    // value is unknown — deliberately, so an inherited image label cannot ride
    // a snapshot forever — which means the container's copy is very often
    // `Some("")`. Filtering only the final result let that empty string count
    // as an answer and skip the snapshot entirely, so a snapshot that *did*
    // record a lineage was never consulted and the project reported "unknown"
    // with the information sitting one lookup away.
    let container_id = docker::find_existing_container(&project).await.unwrap_or(None);
    let from_container = match &container_id {
        Some(id) => container_label(id, mig::LABEL_BASE_IMAGE_ID).await,
        None => None,
    };
    let from_snapshot = mig::image_labels(&snapshot_image)
        .await
        .get(mig::LABEL_BASE_IMAGE_ID)
        .cloned();
    let recorded = pick_recorded_lineage(from_container, from_snapshot);

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

/// Whether a migration for this project is running **in this process right
/// now**. Every command that stops, removes or recreates the project's
/// container has to consult it: the window between `remove_container` and the
/// create that follows looks exactly like "no container", and an ordinary
/// Start landing in it creates a second container under the same name.
///
/// **This is now a view onto [`crate::project_lock`], not a set of its own.**
/// It used to be the app's only mutual-exclusion primitive, and it was one-way:
/// a migration claimed a project, everything else merely polled this once at
/// entry and never claimed anything. Two non-migration writers of
/// `triple-c-snapshot-{id}:latest` — a compaction and a recreate — could not
/// see each other at all. Folding the set into the shared registry means there
/// is exactly one answer to "is something happening to this project", and this
/// function is the specialisation of it that reconcile still needs: a *live*
/// migration is indistinguishable from a crashed one from the outside, and only
/// this process knows which it is looking at.
///
/// No production caller on this branch: the Disk panel's survey was the last
/// one, and it went to `hold/disk-and-dragout`. Kept — and still exercised by
/// `a_live_migration_is_distinguishable_from_a_crashed_one` — because it is the
/// one named answer to that question and re-inventing it is how the two
/// disagreeing answers happened the first time.
#[allow(dead_code)]
pub(crate) fn is_migrating(project_id: &str) -> bool {
    crate::project_lock::is_held_by(project_id, crate::project_lock::ProjectOp::Migration)
}

/// RAII marker: releases the project's [`crate::project_lock`] claim however
/// the migration ends, including an early `?`.
///
/// Kept as a named type rather than using [`crate::project_lock::ProjectGuard`]
/// directly so the migration path keeps reading as "take the migration guard",
/// and so the one place that decides what a migration's claim *is* stays here.
struct ActiveGuard(#[allow(dead_code)] crate::project_lock::ProjectGuard);

impl ActiveGuard {
    /// `Err` with the registry's own refusal when a migration — **or anything
    /// else** — already holds this project.
    ///
    /// The error string is the point. This returned `Option`, and all three
    /// callers replaced the discarded reason with a sentence about a migration
    /// — so a user blocked by a *compaction*, a reset or a cache clear was told
    /// to wait for a base update that was not running, with nothing in the UI
    /// that could ever name what actually held the project.
    /// `project_lock::try_acquire` already composes "what holds it" with "what
    /// you were trying to do"; there is nothing to add to it here.
    ///
    /// The tail it composes is `ProjectOp::Migration`'s — "…before starting a
    /// base update" — for confirm and rollback as well as for the migration
    /// itself. That is the class all three belong to, and splitting it would
    /// mean a `ProjectOp` variant per command: the wrong place to encode a
    /// verb, for a phrase that is at worst imprecise where the old one was
    /// simply wrong.
    fn acquire(project_id: &str) -> Result<Self, String> {
        crate::project_lock::try_acquire(project_id, crate::project_lock::ProjectOp::Migration)
            .map(Self)
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
    let _guard = match ActiveGuard::acquire(&project_id) {
        Ok(guard) => guard,
        Err(busy) => return Ok(MigrationReport::failed_preflight(&busy)),
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

    // Scrub *before* the stop, not inside the commit below. `scrub_writable_layer`
    // is a `docker exec`, which only works on a running container — and the
    // pre-swap commit is the single largest snapshot Triple-C ever takes, so
    // letting this one path commit unscrubbed is what the scrub exists to
    // prevent. Failure is swallowed inside; it must never block a migration.
    //
    // The outcome is logged rather than discarded: this is the one scrub whose
    // silence would be expensive, because the layer it declined to clean is
    // about to be committed into a snapshot that outlives the migration.
    //
    // H2: **bind it first.** `ScrubOutcome` is `#[must_use]`, and the way that
    // was satisfied here was by folding the awaited call into `log::info!`'s
    // argument list. `log::info!` expands to
    // `if Info <= max_level() { … }` — so the arguments, this data-integrity
    // step among them, live inside the level check and do not run at all when
    // the global filter is `Off`. That is reachable: `logging::init` tolerates
    // `dispatch.apply()` failing, and fern returns *before* `set_max_level` on
    // error, so a process that failed to install a logger sits at `Off` with
    // every `log::` argument list silently dead. The scrub would then never
    // run, and the unscrubbed layer would be committed into the longest-lived
    // snapshot the app takes. `commit_container_snapshot` gets this right for
    // the same reason; nothing may put an effect inside a log macro's
    // arguments. (`logging::init` now also restores the level on failure, but
    // the call site is not allowed to depend on that.)
    let scrub = docker::scrub_writable_layer(&container_id).await;
    log::info!("Pre-migration scrub of {}{}", container_id, scrub.commit_log_suffix());

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
    let _guard = ActiveGuard::acquire(&project_id)?;
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
        crate::docker::sweep_orphaned_snapshots_logged("after migration confirmed").await;
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
    let _guard = ActiveGuard::acquire(&project_id)?;

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

    // Retagging above moved `:latest` off the *migrated* snapshot, and the
    // container that was built from it was removed a few lines up — so a
    // multi-gigabyte image is sitting there untagged and unreferenced with
    // nothing else in the app that would ever look at it again. The confirm
    // path sweeps for exactly this reason; rolling back orphans just as much
    // and did not.
    tauri::async_runtime::spawn(async {
        crate::docker::sweep_orphaned_snapshots_logged("after migration rollback").await;
    });
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
    //
    // **Any holder, not just a migration.** This asked `is_migrating`, which is
    // `held() == Some(Migration)` — so a compaction, a reset or a destroy
    // holding the project made this fall straight through and start rewriting
    // the migration record's phase and untagging its rollback image underneath
    // whatever was running. `reconcile_project_statuses` is a command, not just
    // a startup step, so "nothing else can be running yet" is not available as
    // an argument.
    //
    // Yielding is right; yielding *forever* was not. The only caller fires once
    // per "Docker became available", so a project that happened to be held at
    // that instant was never looked at again for the rest of the session: its
    // phase stayed un-normalised, no resume or rollback was ever offered, and
    // its `:pre-migration-*` pin stayed `Claimed`. So the visit is deferred
    // rather than dropped — see [`defer_migration_reconcile`].
    if let Some(holder) = crate::project_lock::held(&project.id) {
        log::debug!(
            "Deferring migration reconcile for '{}' ({}): {}",
            project.name,
            project.id,
            holder.describe()
        );
        defer_migration_reconcile(project, app_handle);
        return;
    }
    reconcile_migration_now(project, app_handle).await;
}

/// How long [`defer_migration_reconcile`] waits between looks, and how many
/// times it looks.
///
/// The operations it is waiting behind are minutes long — a Reset recreates a
/// container from a base image, a compaction rebuilds a multi-gigabyte
/// snapshot — so the interval is coarse on purpose: this is a `held()` read
/// against an in-process map, but every wakeup is a task and the point is to
/// catch the release, not to catch it promptly. Twenty seconds × ninety is
/// thirty minutes, comfortably past the longest measured compaction, after
/// which the project is left for the next `reconcile_project_statuses`.
const RECONCILE_RETRY_INTERVAL: std::time::Duration = std::time::Duration::from_secs(20);
const RECONCILE_RETRY_ATTEMPTS: usize = 90;

/// Project ids with a deferred reconcile already waiting.
///
/// `reconcile_project_statuses` is a command the frontend can call more than
/// once — every "Docker became available" — and each call walks every project.
/// Without this, a project held for a few minutes would accumulate one waiting
/// task per call, all of which would then reconcile the same record in a row.
fn reconcile_retries() -> &'static std::sync::Mutex<std::collections::HashSet<String>> {
    static RETRIES: std::sync::OnceLock<std::sync::Mutex<std::collections::HashSet<String>>> =
        std::sync::OnceLock::new();
    RETRIES.get_or_init(|| std::sync::Mutex::new(std::collections::HashSet::new()))
}

/// One project's place in [`reconcile_retries`], handed back on drop.
///
/// RAII for the reason [`crate::project_lock::ProjectGuard`] sets out, and this
/// claim is the case that proves the rule: the release used to be a trailing
/// statement at the bottom of the spawned task in
/// [`defer_migration_reconcile`], sitting after an `.await` on
/// [`reconcile_migration_now`]. A panic in there — or the future simply being
/// dropped, which is what happens to every in-flight task at shutdown — skips
/// the statement, and nothing else ever removes an id from that set. The
/// project is then fenced off from *every* later deferral for the rest of the
/// process: each `reconcile_project_statuses` pass finds it held, fails to
/// claim, and returns, so the phase stays un-normalised, no resume or rollback
/// is offered, and the `:pre-migration-*` pin stays `Claimed`. That is the
/// session-long silence deferring was written to end, reintroduced one panic
/// later and lasting until the app is restarted.
///
/// Dropping this hands the claim straight back, so a caller that discards the
/// value has claimed nothing while reading as though it had; `#[must_use]`
/// makes that a compile warning rather than a second waiter on one record.
#[must_use = "the claim is handed back the moment this guard drops; bind it inside the waiting task, for the whole task"]
struct ReconcileRetryClaim {
    project_id: String,
}

impl Drop for ReconcileRetryClaim {
    fn drop(&mut self) {
        // `into_inner` past poisoning, as in `project_lock`: the only thing
        // ever done while holding this mutex is a single `HashSet` insert or
        // remove, so a panic on another thread cannot have left it half
        // written — and declining to release here would strand the project
        // permanently, which is the exact failure the guard exists to stop.
        reconcile_retries()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&self.project_id);
    }
}

/// Claim the right to be the one deferred reconcile for `project_id`.
/// `None` means somebody else already is.
fn claim_reconcile_retry(project_id: &str) -> Option<ReconcileRetryClaim> {
    reconcile_retries()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(project_id.to_string())
        .then(|| ReconcileRetryClaim {
            project_id: project_id.to_string(),
        })
}

/// Come back to a project that was held when [`reconcile_migration`] reached it.
///
/// Only for projects that have a record on disk: [`migration_store::has_record`]
/// is filesystem presence, so it costs nothing and is deliberately the *cheap*
/// question — every project is walked on every reconcile and almost none of
/// them have a migration in flight. A record that exists but cannot be parsed
/// answers `true` here and is handled, conservatively, by `load` when the
/// retry lands.
///
/// The wait is a poll rather than a notification because `project_lock` has no
/// release hook and giving it one would mean a guard's `Drop` waking tasks
/// while it still holds the map's mutex. A read of an in-process `HashMap`
/// every twenty seconds, for as long as one operation is running on one
/// project, is not worth a condvar.
fn defer_migration_reconcile(project: &Project, app_handle: &tauri::AppHandle) {
    // No record means nothing to come back for. An unreadable migrations
    // directory answers "maybe", and maybe is worth a look.
    if !migration_store::has_record(&project.id).unwrap_or(true) {
        return;
    }
    let Some(claim) = claim_reconcile_retry(&project.id) else {
        return;
    };

    let project = project.clone();
    let app_handle = app_handle.clone();
    tauri::async_runtime::spawn(async move {
        // Moved in and bound for the whole body, rather than released by a
        // statement at the bottom: everything below this line can panic or be
        // dropped mid-await, and a claim that only comes back on the happy path
        // is a claim that eventually does not come back at all. See
        // [`ReconcileRetryClaim`].
        let _claim = claim;
        let released =
            await_release(&project.id, RECONCILE_RETRY_INTERVAL, RECONCILE_RETRY_ATTEMPTS).await;
        if released {
            // Whatever was holding it may have finished the migration itself or
            // cleared the record — `reconcile_migration_now` loads the record
            // first and returns on `None`, so that is a no-op rather than a
            // special case here.
            reconcile_migration_now(&project, &app_handle).await;
        } else {
            log::warn!(
                "Gave up waiting to reconcile the migration record for '{}' ({}): it has been \
                 held for {} minutes. Its phase is unchanged and its rollback pin is still \
                 claimed; the next reconcile pass will try again.",
                project.name,
                project.id,
                RECONCILE_RETRY_INTERVAL.as_secs() as usize * RECONCILE_RETRY_ATTEMPTS / 60
            );
        }
    });
}

/// Wait for `project_id` to stop being held: `attempts` looks, the first
/// immediate and the rest `interval` apart. `true` means it was released,
/// `false` that the budget ran out with it still held.
///
/// Split out of [`defer_migration_reconcile`] so the waiting can be tested
/// against a real [`crate::project_lock`] guard on a paused clock — the parts
/// that are easy to get wrong are "gives up while still holding the claim",
/// "never looks again", and the ordering of the look against the sleep, none of
/// which is visible from the constants.
async fn await_release(
    project_id: &str,
    interval: std::time::Duration,
    attempts: usize,
) -> bool {
    for attempt in 0..attempts {
        // Look first, sleep second. Sleeping first charged every deferral a
        // full interval before anyone read the map even once, and the common
        // case is a holder that has already let go: `held()` is sampled in
        // `reconcile_migration`, a task is spawned, and by the time it is first
        // polled the Reset that was on its last step is frequently finished.
        // That bought nothing and cost twenty seconds of a startup pass waiting
        // on a lock nobody holds, in front of a check that is one `HashMap`
        // lookup.
        if crate::project_lock::held(project_id).is_none() {
            return true;
        }
        // And no sleep after the final look: nothing reads the map again
        // afterwards, so it is twenty seconds of delay in front of a `false`
        // that has already been decided. The budget is still `attempts` looks,
        // which is what the constants above are chosen against.
        if attempt + 1 < attempts {
            tokio::time::sleep(interval).await;
        }
    }
    false
}

/// [`reconcile_migration`] with the "is anything holding this project" question
/// already answered.
async fn reconcile_migration_now(project: &Project, app_handle: &tauri::AppHandle) {
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
        Ok(None) => {
            // **`Ok(None)` is not the same as "no file".** `migration_store::load`
            // now reports an *unparseable* record as absent while deliberately
            // leaving it on disk, so that `has_record` goes on protecting the
            // rollback pin it describes. Returning here on that would leave the
            // file — and therefore a permanently "claimed" pin — behind a Reset
            // that has just deleted the snapshot and both volumes the record
            // could possibly refer to.
            if !migration_store::has_record(project_id).unwrap_or(false) {
                return;
            }
            log::warn!(
                "Project {} has a migration record that could not be read; removing it anyway \
                 because a Reset supersedes it",
                project_id
            );
        }
        Err(e) => log::warn!(
            "Could not read the migration record for {} while cleaning up: {}",
            project_id,
            e
        ),
    }
    let _ = migration_store::clear_staging(project_id);
    let _ = migration_store::clear(project_id);
    // The pins this project had are gone with the snapshot; their grace clocks
    // are meaningless and would otherwise sit in the migrations directory
    // forever.
    migration_store::clear_ownerless_for_project(project_id);
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
    fn an_empty_lineage_label_is_absence_and_falls_through_to_the_snapshot() {
        let some = |s: &str| Some(s.to_string());

        // The regression: the container always carries the label, so an
        // unknown lineage reads as `Some("")`. Letting that satisfy the lookup
        // skipped a snapshot that had recorded the real thing.
        assert_eq!(
            pick_recorded_lineage(some(""), some("sha256:base")),
            some("sha256:base")
        );

        // Ordinary precedence still holds: the container wins when it has one.
        assert_eq!(
            pick_recorded_lineage(some("sha256:container"), some("sha256:snapshot")),
            some("sha256:container")
        );
        assert_eq!(pick_recorded_lineage(None, some("sha256:snap")), some("sha256:snap"));

        // Genuinely unknown stays unknown — "probe instead", never a lineage
        // invented to make the comparison succeed.
        assert_eq!(pick_recorded_lineage(None, None), None);
        assert_eq!(pick_recorded_lineage(some(""), some("")), None);
        assert_eq!(pick_recorded_lineage(some(""), None), None);
    }

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
            let refused = ActiveGuard::acquire(id)
                .err()
                .expect("a second concurrent migration must be refused");
            // The refusal has to say what is holding the project, not what the
            // caller happens to be — the three commands used to substitute
            // their own sentence for this and lost the distinction.
            assert!(
                refused.contains("base update"),
                "the refusal must name the holder: {}",
                refused
            );
            drop(g);
        }
        assert!(!is_migrating(id), "the guard must release on drop");
        // …including when the migration bailed out through an early return.
        fn early_return(id: &str) -> Option<()> {
            let _g = ActiveGuard::acquire(id).ok()?;
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

    /// No `await` may sit inside a `log::*!` argument list — H2, generalised.
    ///
    /// `log::info!(a, b)` expands to `if Info <= max_level() { … a … b … }`, so
    /// an argument is only evaluated while the level admits the record. Folding
    /// `scrub_writable_layer(&id).await.commit_log_suffix()` into the arguments
    /// here — done to satisfy `#[must_use]` on `ScrubOutcome` — therefore made
    /// the pre-migration scrub conditional on the log level, and
    /// `logging::init` deliberately tolerates failing to install a logger,
    /// which leaves `max_level()` at `Off`. A scrub that never runs before the
    /// largest snapshot the app takes is not something a log level may decide.
    ///
    /// Scanned over the source rather than asserted at one call site: the bug
    /// is a shape, and it is reintroduced by whoever next has a `#[must_use]`
    /// value they only want to log.
    #[test]
    fn nothing_awaits_inside_a_log_macros_arguments() {
        let sources: &[(&str, &str)] = &[
            ("commands/migration_commands.rs", include_str!("migration_commands.rs")),
            ("docker/container.rs", include_str!("../docker/container.rs")),
            ("docker/migration.rs", include_str!("../docker/migration.rs")),
            ("logging.rs", include_str!("../logging.rs")),
        ];
        let macros = ["log::error!(", "log::warn!(", "log::info!(", "log::debug!(", "log::trace!("];
        let mut scanned = 0usize;
        for (name, src) in sources {
            for mac in macros {
                let mut from = 0usize;
                while let Some(at) = src[from..].find(mac) {
                    let start = from + at + mac.len();
                    // Balance the macro's own parentheses. String literals in
                    // these call sites never contain an unbalanced one, and a
                    // `(` inside a format string would only ever widen the
                    // slice, i.e. fail safe.
                    let mut depth = 1usize;
                    let mut end = start;
                    for (i, c) in src[start..].char_indices() {
                        match c {
                            '(' => depth += 1,
                            ')' => {
                                depth -= 1;
                                if depth == 0 {
                                    end = start + i;
                                    break;
                                }
                            }
                            _ => {}
                        }
                    }
                    let args = &src[start..end];
                    // This test's own name mentions the thing it forbids.
                    assert!(
                        !args.contains(".await"),
                        "{}: an `.await` inside a `{}` argument list stops happening whenever the \
                         log level does not admit the record:\n{}",
                        name,
                        mac.trim_end_matches('('),
                        args
                    );
                    scanned += 1;
                    from = end.max(start);
                }
            }
        }
        // A scanner that matched nothing would pass silently forever.
        assert!(scanned > 80, "only {} log call sites were scanned", scanned);
    }

    /// MEDIUM: a project held when the reconcile pass reached it must be
    /// revisited, not dropped for the session.
    ///
    /// `reconcile_project_statuses` fires once per "Docker became available",
    /// so the old `return` meant a project that happened to be mid-Reset at
    /// that instant never had its migration phase normalised, was never offered
    /// resume or rollback, and kept its `:pre-migration-*` pin `Claimed` — for
    /// the rest of the session. On a paused clock, so the thirty-minute budget
    /// costs nothing.
    #[tokio::test(start_paused = true)]
    async fn a_held_project_is_revisited_once_the_holder_lets_go() {
        let id = format!("await-release-{}", uuid::Uuid::new_v4().simple());
        let guard = crate::project_lock::try_acquire(&id, crate::project_lock::ProjectOp::Reset)
            .expect("a fresh project id is not held");

        let waiting = {
            let id = id.clone();
            tokio::spawn(async move {
                await_release(&id, RECONCILE_RETRY_INTERVAL, RECONCILE_RETRY_ATTEMPTS).await
            })
        };

        // Long enough that several looks have already happened and found it
        // held, so this cannot pass by the waiter never having polled.
        tokio::time::sleep(RECONCILE_RETRY_INTERVAL * 3).await;
        assert!(!waiting.is_finished(), "the waiter returned while the project was held");

        drop(guard);
        assert!(
            waiting.await.expect("the waiter task"),
            "the holder let go and the reconcile never came back"
        );
    }

    /// And the budget is finite: a project held indefinitely does not leave a
    /// task waiting on it forever, and the claim is handed back either way.
    #[tokio::test(start_paused = true)]
    async fn waiting_for_a_holder_gives_up_eventually() {
        let id = format!("await-release-{}", uuid::Uuid::new_v4().simple());
        let _guard =
            crate::project_lock::try_acquire(&id, crate::project_lock::ProjectOp::Migration)
                .expect("a fresh project id is not held");
        assert!(!await_release(&id, RECONCILE_RETRY_INTERVAL, RECONCILE_RETRY_ATTEMPTS).await);
        // Thirty minutes: past the longest measured compaction, and the thing
        // being waited on is always a bounded, user-initiated operation.
        assert!(RECONCILE_RETRY_ATTEMPTS > 0, "deferring would be a no-op");
        let budget = RECONCILE_RETRY_INTERVAL * RECONCILE_RETRY_ATTEMPTS as u32;
        assert!(budget >= std::time::Duration::from_secs(15 * 60), "{:?}", budget);
    }

    /// MEDIUM: an unheld project is reconciled now, not in twenty seconds.
    ///
    /// The wait slept before its first look, so a holder that let go between
    /// `reconcile_migration` sampling `held()` and this task being polled — the
    /// *common* case, since a deferral is only taken when something was on its
    /// way out — still cost a full `RECONCILE_RETRY_INTERVAL` of a startup pass
    /// waiting on a lock nobody held. On a paused clock the assertion is exact:
    /// the fixed shape returns without the clock moving at all, the sleep-first
    /// shape cannot return before it has advanced one interval.
    #[tokio::test(start_paused = true)]
    async fn an_unheld_project_is_seen_without_waiting_out_an_interval() {
        let id = format!("await-release-{}", uuid::Uuid::new_v4().simple());
        assert!(
            crate::project_lock::held(&id).is_none(),
            "a fresh uuid is not held"
        );

        let before = tokio::time::Instant::now();
        assert!(await_release(&id, RECONCILE_RETRY_INTERVAL, RECONCILE_RETRY_ATTEMPTS).await);
        let waited = tokio::time::Instant::now() - before;
        assert_eq!(
            waited,
            std::time::Duration::ZERO,
            "an already-released project cost {:?} before anyone looked",
            waited
        );
    }

    #[test]
    fn only_one_deferred_reconcile_waits_per_project() {
        // Every "Docker became available" walks every project, so without the
        // claim a project held for a few minutes accumulates one waiting task
        // per call — all of which then reconcile the same record in a row.
        let id = format!("retry-claim-{}", uuid::Uuid::new_v4().simple());
        let other = format!("retry-claim-{}", uuid::Uuid::new_v4().simple());
        let first = claim_reconcile_retry(&id).expect("a fresh project id is unclaimed");
        assert!(
            claim_reconcile_retry(&id).is_none(),
            "a second waiter was allowed in"
        );
        let other_claim =
            claim_reconcile_retry(&other).expect("the claim is not per-project");
        drop(first);
        // Bound rather than discarded: the guard releases on drop, so
        // `claim_reconcile_retry(&id);` as a bare statement would test nothing
        // — which is what `#[must_use]` is there to catch in real callers.
        let retaken = claim_reconcile_retry(&id).expect("the claim was never handed back");
        drop(retaken);
        drop(other_claim);
        // And the other project's claim was never the same claim.
        drop(claim_reconcile_retry(&other).expect("released independently"));
    }

    /// MEDIUM: the claim survives the task that holds it dying badly.
    ///
    /// The release used to be a trailing statement after
    /// `reconcile_migration_now(...).await` at the bottom of the spawned task,
    /// so a panic anywhere in that call — or the future being dropped at
    /// shutdown — skipped it and left the id in the set with no task behind it.
    /// Nothing removes it afterwards, so that project could never be deferred
    /// again for the rest of the process: exactly the state deferring was added
    /// to prevent, now permanent instead of one pass long. Fails against the
    /// trailing-statement shape, which is the point.
    #[tokio::test]
    async fn a_panicking_deferred_reconcile_hands_its_claim_back() {
        let id = format!("retry-claim-{}", uuid::Uuid::new_v4().simple());
        let claimed = claim_reconcile_retry(&id).expect("a fresh project id is unclaimed");

        // Spawned, not just called: the real claim is held across an await
        // inside a `tauri::async_runtime::spawn`, and a task panic is caught by
        // the runtime rather than unwinding the caller.
        let task = {
            let id = id.clone();
            tokio::spawn(async move {
                let _claim = claimed;
                tokio::task::yield_now().await;
                panic!("reconcile_migration_now blew up on '{}'", id);
            })
        };
        assert!(task.await.is_err(), "the task was supposed to panic");

        let after = claim_reconcile_retry(&id);
        assert!(
            after.is_some(),
            "a panicking reconcile stranded the claim — this project can never be \
             deferred again for the rest of the process"
        );
        drop(after);

        // The other half of the same failure: a task that is simply dropped
        // mid-flight, which is every in-flight task at shutdown.
        let claimed = claim_reconcile_retry(&id).expect("released above");
        let never_finishes = tokio::spawn(async move {
            let _claim = claimed;
            std::future::pending::<()>().await;
        });
        never_finishes.abort();
        let _ = never_finishes.await;
        assert!(
            claim_reconcile_retry(&id).is_some(),
            "a dropped task stranded the claim"
        );
    }
}
