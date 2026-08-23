//! Host-side persistence for in-flight container base-image migrations.
//!
//! One JSON file per project under `<data_dir>/triple-c/migrations/`, written
//! with the same write-temp-then-rename dance as `projects.json` so a crash can
//! never leave a half-written state file. The staged verbatim payload tar lives
//! in the same directory.
//!
//! This is deliberately *not* part of `projects.json`: a migration is transient
//! and a migration record must survive independently of a project save racing
//! it. It is also the crash record — see
//! [`crate::models::MigrationState`] for the phase table.

use std::fs;
use std::path::PathBuf;

use crate::models::MigrationState;

/// `<data_dir>/triple-c/migrations`, created on demand.
pub fn migrations_dir() -> Result<PathBuf, String> {
    let dir = dirs::data_dir()
        .ok_or_else(|| {
            "Could not determine data directory. Set XDG_DATA_HOME on Linux.".to_string()
        })?
        .join("triple-c")
        .join("migrations");
    fs::create_dir_all(&dir)
        .map_err(|e| format!("Failed to create migrations directory: {}", e))?;
    Ok(dir)
}

fn state_path(project_id: &str) -> Result<PathBuf, String> {
    Ok(migrations_dir()?.join(format!("{}.json", sanitize(project_id))))
}

/// Host path for a project's staged verbatim payload.
pub fn staging_path(project_id: &str) -> Result<PathBuf, String> {
    Ok(migrations_dir()?.join(format!("{}-payload.tar", sanitize(project_id))))
}

/// Project ids are UUIDs, but they arrive over IPC, so refuse to let one steer
/// the write anywhere but the migrations directory.
fn sanitize(project_id: &str) -> String {
    project_id
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect()
}

/// Read a project's migration state. `Ok(None)` means no migration is in
/// flight; an unparseable file is treated the same way (and logged) rather than
/// blocking every future migration on a corrupt record.
///
/// **A corrupt record is copied aside and left in place.** An earlier version
/// *renamed* it to `.bak`, on the reasoning that a file nothing can parse
/// should stop making the project look busy. That destroyed the one signal
/// [`has_record`] exists to carry. The chain, in order:
///
/// 1. The rename makes the file vanish, so `has_record` — pure filesystem
///    presence — flips to false.
/// 2. `reconcile_migration` calls this, gets `Ok(None)`, and returns. An
///    in-flight or interrupted migration becomes invisible: no resume offer, no
///    rollback offer, and the phase is never normalised.
/// 3. Both pin reapers use `has_record` as their conservative guard, so the
///    project's `:pre-migration-*` tag — the only copy of its pre-migration
///    system layer — is now "ownerless" to both of them, and the startup sweep
///    turns the untag into a deletion.
///
/// A record that cannot be parsed is exactly the case where the *most*
/// conservative answer is wanted, not the least. So the bytes are copied to a
/// **uniquely named** backup (a fixed `.bak` meant a second corruption silently
/// overwrote the first, and nothing ever read either back) and the original
/// stays where it is. The pin it describes then ages out through the ownerless
/// tombstone in `docker::migration::reap_stale_migration_pins` rather than
/// being reaped on the next app start.
pub fn load(project_id: &str) -> Result<Option<MigrationState>, String> {
    let path = state_path(project_id)?;
    if !path.exists() {
        return Ok(None);
    }
    let data = fs::read_to_string(&path)
        .map_err(|e| format!("Failed to read migration state: {}", e))?;
    match serde_json::from_str::<MigrationState>(&data) {
        Ok(state) => Ok(Some(state)),
        Err(e) => {
            let backup = corrupt_backup_path(&path, &chrono::Utc::now());
            let copied = if backup.exists() {
                // Already kept a copy of this exact corruption this second;
                // nothing to add.
                Ok(())
            } else {
                fs::copy(&path, &backup).map(|_| ())
            };
            log::error!(
                "Failed to parse migration state for project {}: {} — treating as absent, but \
                 the record is left in place so `has_record` still protects its rollback pin{}",
                project_id,
                e,
                match copied {
                    Ok(()) => format!(" (a copy was kept at {})", backup.display()),
                    Err(ref e) => format!(" (could not keep a copy: {})", e),
                }
            );
            Ok(None)
        }
    }
}

/// Where a copy of an unparseable record is kept.
///
/// Timestamped rather than a fixed `.bak`: a second corruption used to
/// overwrite the first, so the one case where the user's bytes matter most was
/// the case where they were most likely to be gone.
fn corrupt_backup_path(path: &std::path::Path, now: &chrono::DateTime<chrono::Utc>) -> PathBuf {
    path.with_extension(format!("json.corrupt-{}.bak", now.format("%Y%m%d-%H%M%S")))
}

/// Whether a project has a migration record on disk *at all*, without parsing
/// it.
///
/// The pin reaper needs "is this project's rollback image still somebody's only
/// copy?" and must answer it conservatively. [`load`] cannot be used for that
/// question on its own — it deliberately reports a corrupt record as absent —
/// so this asks the filesystem instead. `load` moving a corrupt record aside is
/// what keeps the two answers from disagreeing forever.
pub fn has_record(project_id: &str) -> Result<bool, String> {
    Ok(state_path(project_id)?.exists())
}

/// Atomically **and durably** write a project's migration state.
///
/// Write-temp-then-rename alone is only half of it, and the missing half is the
/// half this record exists for. `fs::write` returns once the bytes are in the
/// page cache; a rename over them is atomic *with respect to other readers*,
/// not with respect to power loss. Losing power in that window leaves the
/// rename applied and the data not yet written — i.e. a 0-byte or truncated
/// `{id}.json` — which is precisely the corrupt-record case above, produced by
/// the code whose job is to make that case impossible.
///
/// So: fsync the file before the rename, and fsync the *directory* after it,
/// because the rename itself is directory metadata and is not durable until the
/// directory is synced. A sync that fails is reported rather than swallowed —
/// this is the crash record, and "probably written" is not a state it may be
/// in.
pub fn save(project_id: &str, state: &MigrationState) -> Result<(), String> {
    let path = state_path(project_id)?;
    let data = serde_json::to_string_pretty(state)
        .map_err(|e| format!("Failed to serialize migration state: {}", e))?;
    let tmp = path.with_extension("json.tmp");

    {
        use std::io::Write;
        let mut file = fs::File::create(&tmp)
            .map_err(|e| format!("Failed to write migration state: {}", e))?;
        file.write_all(data.as_bytes())
            .map_err(|e| format!("Failed to write migration state: {}", e))?;
        file.sync_all()
            .map_err(|e| format!("Failed to flush migration state to disk: {}", e))?;
    }

    fs::rename(&tmp, &path).map_err(|e| format!("Failed to commit migration state: {}", e))?;
    sync_dir(&path);
    // A project with a record is not ownerless, whatever a reaper concluded
    // before this write — so the grace clock is thrown away rather than left to
    // expire against a pin that now has an owner again.
    clear_ownerless_for_project(project_id);
    Ok(())
}

/// fsync the directory holding `path`, so a rename into it survives power loss.
///
/// Best effort *only* on the platforms where it is meaningless: Windows has no
/// directory handle to sync and returns an error for the attempt, so a failure
/// is logged rather than propagated. The file's own `sync_all` above is the
/// part that carries the data, and it is not best effort.
fn sync_dir(path: &std::path::Path) {
    let Some(dir) = path.parent() else {
        return;
    };
    match fs::File::open(dir).and_then(|d| d.sync_all()) {
        Ok(()) => {}
        Err(e) => log::debug!(
            "Could not fsync the migrations directory {}: {} — the record itself was flushed",
            dir.display(),
            e
        ),
    }
}

// ---------------------------------------------------------------------------
// Ownerless-pin tombstones
// ---------------------------------------------------------------------------

/// Marker recording **when a rollback pin was first seen with no record behind
/// it**.
///
/// ## Why the grace period cannot be measured from the tag
///
/// `docker::migration::pin_is_reapable` used to date a pin from the timestamp
/// encoded in `pre-migration-<YYYYmmdd-HHMMSS>` — i.e. from when the migration
/// *started*. That is the wrong epoch by a whole feature. A migration is
/// allowed to sit at `awaiting-confirmation` indefinitely; `keep_rollback`
/// exists precisely so a user can run on the new base for a month before
/// deciding. If that project's record is then lost — a corrupt file, a deleted
/// state file, a half-restored data directory — the pin is fourteen days old on
/// the very first check, so it is untagged on the next app start and the
/// startup sweep deletes the image two lines later. The fourteen-day grace
/// period the constant promises is zero in the only situation it was written
/// for.
///
/// The clock has to start when the *claim* was lost, and nothing on the daemon
/// records that moment. So it is written down here, the first time a reaper
/// notices, and the age is measured from the marker.
///
/// One file per `(project_id, tag)` in the migrations directory, holding an
/// RFC3339 instant. Tiny, and losing one costs a fresh fourteen days rather
/// than a deletion — the failure direction that keeps somebody's only rollback
/// copy.
fn ownerless_marker_path(project_id: &str, tag: &str) -> Result<PathBuf, String> {
    Ok(migrations_dir()?.join(format!(
        "{}.{}.ownerless",
        sanitize(project_id),
        sanitize(tag)
    )))
}

/// When this pin was first observed ownerless, **without recording anything**.
///
/// For the survey paths, which describe the world and must not change it.
/// `None` means "no reaper has seen it yet", which is not the same as "seen
/// just now" and must not be treated as a start date.
pub fn peek_ownerless_since(
    project_id: &str,
    tag: &str,
) -> Option<chrono::DateTime<chrono::Utc>> {
    let path = ownerless_marker_path(project_id, tag).ok()?;
    let raw = fs::read_to_string(path).ok()?;
    chrono::DateTime::parse_from_rfc3339(raw.trim())
        .ok()
        .map(|t| t.with_timezone(&chrono::Utc))
}

/// Read the first-observed instant for a pin, creating the marker if this is
/// the first sighting. Returns `None` when the clock has not started yet.
///
/// **Clock skew is handled here rather than at the comparison.** A host clock
/// that was running fast when the marker was written leaves a timestamp in the
/// future; measured naively that is a negative age, which a `num_days() >= 14`
/// test reads as "never reapable" — a pin that can never be collected, forever.
/// A marker dated after `now` is therefore rewritten to `now`, restarting the
/// grace period. The other direction — a clock jumping forward — cannot shorten
/// the period below what has actually elapsed on the *marker's* terms, because
/// there is nothing to compare against but wall time; what it cannot do any
/// more is make every pin instantly reapable, which dating from the tag did.
pub fn note_ownerless_since(
    project_id: &str,
    tag: &str,
    now: &chrono::DateTime<chrono::Utc>,
) -> Option<chrono::DateTime<chrono::Utc>> {
    let path = ownerless_marker_path(project_id, tag).ok()?;
    let existing = fs::read_to_string(&path).ok().and_then(|raw| {
        chrono::DateTime::parse_from_rfc3339(raw.trim())
            .ok()
            .map(|t| t.with_timezone(&chrono::Utc))
    });
    match existing {
        Some(seen) if seen <= *now => Some(seen),
        // Absent, unparseable, or dated in the future: (re)start the clock.
        _ => {
            if let Err(e) = fs::write(&path, now.to_rfc3339()) {
                log::warn!(
                    "Could not record that rollback pin {}:{} is ownerless: {} — its grace \
                     period restarts on the next check",
                    project_id,
                    tag,
                    e
                );
            }
            None
        }
    }
}

/// Forget a pin's ownerless marker. Missing is success.
///
/// Called when the pin is untagged, and when a record reappears for the
/// project — a re-migrated project must not inherit the previous run's clock.
pub fn clear_ownerless(project_id: &str, tag: &str) {
    let Ok(path) = ownerless_marker_path(project_id, tag) else {
        return;
    };
    match fs::remove_file(&path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => log::warn!("Could not remove {}: {}", path.display(), e),
    }
}

/// Drop every ownerless marker belonging to one project.
///
/// A project that has a record again is by definition not ownerless, whatever
/// a reaper concluded before.
pub fn clear_ownerless_for_project(project_id: &str) {
    let Ok(dir) = migrations_dir() else {
        return;
    };
    let prefix = format!("{}.", sanitize(project_id));
    let Ok(entries) = fs::read_dir(&dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with(&prefix) && name.ends_with(".ownerless") {
            let _ = fs::remove_file(entry.path());
        }
    }
}

/// Remove a project's migration state file. Missing is success.
pub fn clear(project_id: &str) -> Result<(), String> {
    let path = state_path(project_id)?;
    match fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("Failed to remove migration state: {}", e)),
    }
}

/// Remove a project's staged payload. Missing is success.
pub fn clear_staging(project_id: &str) -> Result<(), String> {
    let path = staging_path(project_id)?;
    match fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("Failed to remove staged migration payload: {}", e)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_ids_cannot_escape_the_migrations_directory() {
        assert_eq!(sanitize("../../etc/passwd"), "______etc_passwd");
        assert_eq!(sanitize("a/b"), "a_b");
        // The real shape — a UUID — must survive untouched, or state files
        // would move the first time this function changed.
        assert_eq!(
            sanitize("ab62cd24-51aa-4645-8f5c-17a124062050"),
            "ab62cd24-51aa-4645-8f5c-17a124062050"
        );
    }
}
