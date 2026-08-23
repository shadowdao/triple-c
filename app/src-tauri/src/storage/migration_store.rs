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
            let copied = if backup.exists() || corrupt_backups_full(&path) {
                // Already kept a copy of this exact corruption this second, or
                // kept as many as are worth keeping. Either way nothing to add.
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

/// How many timestamped copies of one project's corrupt record are kept.
///
/// Timestamping fixed the "second corruption overwrote the first" bug and
/// introduced its opposite: [`load`] runs on every reconcile, every survey and
/// every reaper pass, so a record that is *persistently* unparseable — the
/// normal case, since nothing repairs it — mints a new copy every time the
/// clock's second changes. Nothing ever reads them back and nothing ever
/// removed them.
///
/// Four is enough for the only use there is: a human looking at what the file
/// held. See [`corrupt_backups_full`] for why the cap is applied before the
/// copy rather than by pruning after it.
const MAX_CORRUPT_BACKUPS: usize = 4;

/// Whether [`MAX_CORRUPT_BACKUPS`] copies of this record already exist.
///
/// Asked *before* the copy rather than pruning after it, so the cap is not
/// implemented by writing a file and deleting it again on every pass — and so
/// the copies that survive are the oldest, which are the ones taken closest to
/// whatever produced the corruption.
///
/// A directory that cannot be listed answers "not full": failing open here
/// costs at most one extra file, and failing closed would drop the very first
/// copy of a record nothing else has kept.
fn corrupt_backups_full(path: &std::path::Path) -> bool {
    let (Some(dir), Some(stem)) = (path.parent(), path.file_stem()) else {
        return false;
    };
    // `{stem}.json.corrupt-` — the same shape `corrupt_backup_path` builds, so
    // this can never match another project's copies or an unrelated `.bak`.
    let prefix = format!("{}.json.corrupt-", stem.to_string_lossy());
    let Ok(entries) = fs::read_dir(dir) else {
        return false;
    };
    entries
        .flatten()
        .filter(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            name.starts_with(&prefix) && name.ends_with(".bak")
        })
        .count()
        >= MAX_CORRUPT_BACKUPS
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
///
/// ## Why the write re-checks `has_record`
///
/// Both reapers ask [`has_record`] and only call this when the answer is no,
/// which leaves a window: a [`save`] landing between the two runs its
/// `clear_ownerless_for_project` against a marker that does not exist yet, and
/// this then plants one — dated *now* — behind a perfectly valid record. The
/// marker is invisible while the record stands, so nothing notices. It only
/// matters later, if that record is legitimately lost: the pin is then already
/// fourteen days ownerless on its very first check and is reaped with **zero**
/// grace, which is the exact failure the tombstone exists to prevent.
///
/// So the write is followed by a second `has_record`, and a marker that turns
/// out to sit behind a record is removed again. The two orderings that remain
/// are both safe: a `save` completing *after* this re-check clears the marker
/// itself, and one completing before it is what the re-check sees.
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
                return None;
            }
            // A record that appeared while this was being written owns the pin,
            // and a tombstone behind an owned pin is a fourteen-day head start
            // on reaping it the moment that record is next lost.
            if has_record(project_id).unwrap_or(false) {
                log::debug!(
                    "A migration record for {} appeared while marking {} ownerless; \
                     the marker was dropped again",
                    project_id,
                    tag
                );
                clear_ownerless(project_id, tag);
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
    fn corrupt_copies_of_one_record_are_capped() {
        // `load` runs on every reconcile, every survey and every reaper pass,
        // and nothing repairs an unparseable record — so a persistently corrupt
        // one minted a new timestamped copy every time the clock's second
        // changed, and nothing ever removed them.
        let dir = std::env::temp_dir().join(format!(
            "triple-c-corrupt-cap-{}",
            uuid::Uuid::new_v4().simple()
        ));
        fs::create_dir_all(&dir).expect("temp dir");
        let record = dir.join("some-project.json");

        assert!(!corrupt_backups_full(&record), "an empty directory is not full");
        for n in 0..MAX_CORRUPT_BACKUPS {
            fs::write(
                dir.join(format!("some-project.json.corrupt-2026010{}-000000.bak", n)),
                "x",
            )
            .unwrap();
        }
        assert!(corrupt_backups_full(&record));

        // Another project's copies, and an unrelated `.bak`, are not this
        // record's — the prefix is the whole point of the naming.
        let other = dir.join("other-project.json");
        assert!(!corrupt_backups_full(&other));
        fs::write(dir.join("some-project.json.bak"), "x").unwrap();
        assert!(!corrupt_backups_full(&other));

        fs::remove_dir_all(&dir).ok();
    }

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
