//! OS keychain access, via the `keyring` crate.
//!
//! Two kinds of secret live here:
//!   * **per-project** secrets (git token, AWS keys, …), keyed by project id;
//!   * the **shared Claude Code OAuth token**, which is global — one
//!     `claude setup-token` run authenticates every Anthropic-backend project.
//!
//! Nothing in this module ever logs a secret or folds one into an error string.

/// Keychain service for the single, global Claude Code OAuth token minted by
/// `claude setup-token` and consumed via `CLAUDE_CODE_OAUTH_TOKEN`.
const CLAUDE_TOKEN_SERVICE: &str = "triple-c-claude-oauth-token";

/// Keychain service for the token's **rotation id** — a fresh random value
/// written every time the token is stored.
///
/// Container recreation is driven off Docker labels, which anything on the host
/// can read with `docker inspect`. The token itself must obviously not go in a
/// label, and neither should a bare hash of it: a hash is a verification oracle
/// (holding a candidate token, you could confirm it). This id is not derived
/// from the token at all — it is unrelated random data that merely *changes*
/// whenever the token does, which is exactly (and only) what change detection
/// needs.
const CLAUDE_TOKEN_VERSION_SERVICE: &str = "triple-c-claude-oauth-token-version";

/// Fixed account name used for every triple-c keychain entry.
const KEYCHAIN_ACCOUNT: &str = "secret";

/// Store a per-project secret in the OS keychain.
pub fn store_project_secret(project_id: &str, key_name: &str, value: &str) -> Result<(), String> {
    let service = format!("triple-c-project-{}-{}", project_id, key_name);
    let entry = keyring::Entry::new(&service, "secret")
        .map_err(|e| format!("Keyring error: {}", e))?;
    entry
        .set_password(value)
        .map_err(|e| format!("Failed to store project secret '{}': {}", key_name, e))
}

/// Retrieve a per-project secret from the OS keychain.
pub fn get_project_secret(project_id: &str, key_name: &str) -> Result<Option<String>, String> {
    let service = format!("triple-c-project-{}-{}", project_id, key_name);
    let entry = keyring::Entry::new(&service, "secret")
        .map_err(|e| format!("Keyring error: {}", e))?;
    match entry.get_password() {
        Ok(value) => Ok(Some(value)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(format!("Failed to retrieve project secret '{}': {}", key_name, e)),
    }
}

/// Delete all known secrets for a project from the OS keychain.
pub fn delete_project_secrets(project_id: &str) -> Result<(), String> {
    let secret_keys = [
        "git-token",
        "aws-access-key-id",
        "aws-secret-access-key",
        "aws-session-token",
        "aws-bearer-token",
    ];
    for key_name in &secret_keys {
        let service = format!("triple-c-project-{}-{}", project_id, key_name);
        let entry = keyring::Entry::new(&service, "secret")
            .map_err(|e| format!("Keyring error: {}", e))?;
        match entry.delete_credential() {
            Ok(()) => {}
            Err(keyring::Error::NoEntry) => {}
            Err(e) => {
                log::warn!("Failed to delete project secret '{}': {}", key_name, e);
            }
        }
    }
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// Shared Claude Code OAuth token (global, not per project)
// ─────────────────────────────────────────────────────────────────────────────

/// Read a single-value keychain entry. `Ok(None)` when the entry is absent.
/// The error text names the entry, never its value.
fn read_entry(service: &str, label: &str) -> Result<Option<String>, String> {
    let entry = keyring::Entry::new(service, KEYCHAIN_ACCOUNT)
        .map_err(|e| format!("Keyring error: {}", e))?;
    match entry.get_password() {
        Ok(value) => Ok(Some(value)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(format!("Failed to retrieve {}: {}", label, e)),
    }
}

/// Delete a keychain entry, treating "wasn't there" as success.
fn delete_entry(service: &str, label: &str) -> Result<(), String> {
    let entry = keyring::Entry::new(service, KEYCHAIN_ACCOUNT)
        .map_err(|e| format!("Keyring error: {}", e))?;
    match entry.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(format!("Failed to delete {}: {}", label, e)),
    }
}

/// Store the shared Claude Code OAuth token, replacing any previous one, and
/// mint a fresh rotation id so containers holding the old token are flagged for
/// recreation. Blank input is rejected rather than silently stored.
pub fn store_claude_oauth_token(token: &str) -> Result<(), String> {
    if token.trim().is_empty() {
        return Err("Refusing to store an empty Claude authentication token.".to_string());
    }

    let entry = keyring::Entry::new(CLAUDE_TOKEN_SERVICE, KEYCHAIN_ACCOUNT)
        .map_err(|e| format!("Keyring error: {}", e))?;
    entry
        .set_password(token)
        .map_err(|e| format!("Failed to store the Claude authentication token: {}", e))?;

    // Rotation id second: if this fails the token is still usable, and the
    // stale id only costs one extra container recreation later.
    let version = uuid::Uuid::new_v4().to_string();
    let version_entry = keyring::Entry::new(CLAUDE_TOKEN_VERSION_SERVICE, KEYCHAIN_ACCOUNT)
        .map_err(|e| format!("Keyring error: {}", e))?;
    version_entry
        .set_password(&version)
        .map_err(|e| format!("Failed to store the Claude token rotation id: {}", e))?;

    Ok(())
}

/// Retrieve the shared Claude Code OAuth token, if one has been stored.
pub fn get_claude_oauth_token() -> Result<Option<String>, String> {
    read_entry(CLAUDE_TOKEN_SERVICE, "the Claude authentication token")
}

/// The rotation id of the currently stored token. Opaque random data — safe to
/// put in a Docker label, unlike the token or any hash of it.
pub fn get_claude_oauth_token_version() -> Result<Option<String>, String> {
    read_entry(
        CLAUDE_TOKEN_VERSION_SERVICE,
        "the Claude token rotation id",
    )
}

/// Whether a shared Claude Code OAuth token is currently stored. A keychain
/// failure is reported as "no token" rather than surfacing as an error, so the
/// UI degrades to the un-authenticated state instead of breaking.
pub fn has_claude_oauth_token() -> bool {
    matches!(get_claude_oauth_token(), Ok(Some(t)) if !t.trim().is_empty())
}

/// Delete the shared Claude Code OAuth token and its rotation id. Both are
/// attempted even if the first fails, so a partial failure cannot strand the
/// token behind a deleted id.
pub fn delete_claude_oauth_token() -> Result<(), String> {
    let token_result = delete_entry(CLAUDE_TOKEN_SERVICE, "the Claude authentication token");
    let version_result = delete_entry(
        CLAUDE_TOKEN_VERSION_SERVICE,
        "the Claude token rotation id",
    );
    token_result.and(version_result)
}
