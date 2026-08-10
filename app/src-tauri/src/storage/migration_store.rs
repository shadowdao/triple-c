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
            log::error!(
                "Failed to parse migration state for project {}: {} — treating as absent",
                project_id,
                e
            );
            Ok(None)
        }
    }
}

/// Atomically write a project's migration state.
pub fn save(project_id: &str, state: &MigrationState) -> Result<(), String> {
    let path = state_path(project_id)?;
    let data = serde_json::to_string_pretty(state)
        .map_err(|e| format!("Failed to serialize migration state: {}", e))?;
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, data).map_err(|e| format!("Failed to write migration state: {}", e))?;
    fs::rename(&tmp, &path).map_err(|e| format!("Failed to commit migration state: {}", e))?;
    Ok(())
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
