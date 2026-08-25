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

/// Every per-project secret this app stores, and therefore every one it has to
/// be able to delete.
///
/// This list is the **only** definition. It used to exist twice — once
/// implicitly, as whatever `store_secrets_for_project` happened to write, and
/// once explicitly, as a literal array inside `delete_project_secrets` — and
/// the two drifted: `openai-compatible-api-key` was added to the writer and
/// never to the deleter, so removing a project left a live provider API key in
/// the user's login keychain with nothing left in the app that referenced it,
/// or would ever offer to clean it up.
///
/// Drift is now a compile-time-shaped error rather than a review-time one:
/// [`project_secret_entry`] refuses a key that is not in this list, so a new
/// secret cannot be stored until it has been added here, and adding it here is
/// what makes [`delete_project_secrets`] cover it.
pub const PROJECT_SECRET_KEYS: &[&str] = &[
    "git-token",
    "aws-access-key-id",
    "aws-secret-access-key",
    "aws-session-token",
    "aws-bearer-token",
    "openai-compatible-api-key",
];

/// The keychain entry for one per-project secret, rejecting any key name not in
/// [`PROJECT_SECRET_KEYS`]. See that constant for why the rejection matters.
fn project_secret_entry(project_id: &str, key_name: &str) -> Result<keyring::Entry, String> {
    if !PROJECT_SECRET_KEYS.contains(&key_name) {
        return Err(format!(
            "Unknown project secret '{}'. Add it to PROJECT_SECRET_KEYS so project deletion \
             clears it too.",
            key_name
        ));
    }
    let service = format!("triple-c-project-{}-{}", project_id, key_name);
    keyring::Entry::new(&service, KEYCHAIN_ACCOUNT).map_err(|e| format!("Keyring error: {}", e))
}

/// Store a per-project secret in the OS keychain.
pub fn store_project_secret(project_id: &str, key_name: &str, value: &str) -> Result<(), String> {
    project_secret_entry(project_id, key_name)?
        .set_password(value)
        .map_err(|e| format!("Failed to store project secret '{}': {}", key_name, e))
}

/// Retrieve a per-project secret from the OS keychain.
pub fn get_project_secret(project_id: &str, key_name: &str) -> Result<Option<String>, String> {
    match project_secret_entry(project_id, key_name)?.get_password() {
        Ok(value) => Ok(Some(value)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(format!("Failed to retrieve project secret '{}': {}", key_name, e)),
    }
}

/// Delete one per-project secret, treating "wasn't there" as success.
pub fn delete_project_secret(project_id: &str, key_name: &str) -> Result<(), String> {
    match project_secret_entry(project_id, key_name)?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(format!("Failed to delete project secret '{}': {}", key_name, e)),
    }
}

/// Write a per-project secret, or **clear** it when there is nothing to write.
///
/// This is the function every save path should call, and the reason it exists
/// is that the obvious `if let Some(v) = … { store(v) }` is wrong. The editors
/// in `components/projects/home/config/` send a blanked field as `null`
/// (`AccessSection.tsx`: `save({ git_token: gitToken || null })`), so a `None`
/// is a user asking for the secret to be *removed* — and skipping it left the
/// old value in the keychain, where `load_secrets_for_project` read it straight
/// back out and put it back on the project. Clearing a credential through the
/// UI was therefore impossible: the field looked empty and the container kept
/// getting the old token.
///
/// `Some("")` and `Some("   ")` are treated the same as `None` — a field the
/// user emptied, whichever shape it arrives in — because a stored empty secret
/// is not a secret, and `container_config` would inject it as an env var that
/// overrides the unset case with a blank.
// TODO(handoff): `commands/project_commands.rs::store_secrets_for_project` is
// the one caller this is for, and it still uses the `if let Some(v) = … ` shape
// that cannot clear anything. That file belongs to another change in this round,
// so the switch is deliberately left to it; the six call sites there become
// `store_or_clear_project_secret(&project.id, "<key>", field.as_deref())?`.
#[allow(dead_code)]
pub fn store_or_clear_project_secret(
    project_id: &str,
    key_name: &str,
    value: Option<&str>,
) -> Result<(), String> {
    match secret_to_store(value) {
        Some(v) => store_project_secret(project_id, key_name, v),
        None => delete_project_secret(project_id, key_name),
    }
}

/// The store-or-clear decision, split out so it can be tested without a
/// keychain backend: `Some` means "write this", `None` means "remove whatever
/// is there".
#[allow(dead_code)]
fn secret_to_store(value: Option<&str>) -> Option<&str> {
    match value.map(str::trim) {
        Some(v) if !v.is_empty() => Some(v),
        _ => None,
    }
}

/// Delete every known secret for a project from the OS keychain.
///
/// Called when a project is removed, so it must cover [`PROJECT_SECRET_KEYS`]
/// exhaustively — a key missed here outlives the project that explained it.
/// One key failing does not stop the rest: a partial cleanup that keeps going
/// leaves strictly fewer credentials behind than one that gives up.
pub fn delete_project_secrets(project_id: &str) -> Result<(), String> {
    for key_name in PROJECT_SECRET_KEYS {
        if let Err(e) = delete_project_secret(project_id, key_name) {
            log::warn!("Failed to delete project secret '{}': {}", key_name, e);
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

// ─────────────────────────────────────────────────────────────────────────────
// Model gateway secrets (global, not per project)
// ─────────────────────────────────────────────────────────────────────────────

/// Keychain service for the upstream provider API key (OpenAI etc.) the
/// LiteLLM gateway authenticates to the model provider with. This value is
/// written into the gateway's generated `config.yaml`, which is uploaded
/// straight into the container over the Docker API — it is never an env var,
/// never a Docker label, and is never returned to the frontend.
const GATEWAY_API_KEY_SERVICE: &str = "triple-c-gateway-provider-api-key";

/// Keychain service for the gateway's **master key** — the credential a
/// *project* presents to the gateway as `ANTHROPIC_AUTH_TOKEN`. Unlike the
/// provider key this one is minted by Triple-C and must be readable by the
/// user, since they have to paste it into a project's model config.
const GATEWAY_MASTER_KEY_SERVICE: &str = "triple-c-gateway-master-key";

/// Rotation id covering *both* gateway secrets, on the same reasoning as
/// `CLAUDE_TOKEN_VERSION_SERVICE`: container recreation is driven off Docker
/// labels, labels are world-readable via `docker inspect`, and a hash of a
/// secret is a verification oracle. This is unrelated random data that merely
/// changes whenever either secret does.
const GATEWAY_SECRET_VERSION_SERVICE: &str = "triple-c-gateway-secret-version";

/// Mint a fresh gateway rotation id. Called after either gateway secret moves.
fn bump_gateway_secret_version() -> Result<(), String> {
    let version = uuid::Uuid::new_v4().to_string();
    let entry = keyring::Entry::new(GATEWAY_SECRET_VERSION_SERVICE, KEYCHAIN_ACCOUNT)
        .map_err(|e| format!("Keyring error: {}", e))?;
    entry
        .set_password(&version)
        .map_err(|e| format!("Failed to store the gateway secret rotation id: {}", e))
}

/// The rotation id of the currently stored gateway secrets. Opaque random
/// data — safe to put in a Docker label, unlike either secret.
pub fn get_gateway_secret_version() -> Result<Option<String>, String> {
    read_entry(
        GATEWAY_SECRET_VERSION_SERVICE,
        "the gateway secret rotation id",
    )
}

/// Store the provider API key, replacing any previous one. Blank input is
/// rejected rather than silently stored.
pub fn store_gateway_api_key(key: &str) -> Result<(), String> {
    if key.trim().is_empty() {
        return Err("Refusing to store an empty gateway provider API key.".to_string());
    }

    let entry = keyring::Entry::new(GATEWAY_API_KEY_SERVICE, KEYCHAIN_ACCOUNT)
        .map_err(|e| format!("Keyring error: {}", e))?;
    entry
        .set_password(key.trim())
        .map_err(|e| format!("Failed to store the gateway provider API key: {}", e))?;

    // Rotation id second: if this fails the key is still usable, and the stale
    // id only costs one extra container recreation later.
    bump_gateway_secret_version()
}

/// Retrieve the provider API key. **Host-side only** — this is consumed when
/// rendering the gateway config and must not be handed to the frontend.
pub fn get_gateway_api_key() -> Result<Option<String>, String> {
    read_entry(GATEWAY_API_KEY_SERVICE, "the gateway provider API key")
}

/// Whether a provider API key is stored. A keychain failure is reported as
/// "no key" so the UI degrades to the unconfigured state instead of breaking.
pub fn has_gateway_api_key() -> bool {
    matches!(get_gateway_api_key(), Ok(Some(k)) if !k.trim().is_empty())
}

/// Delete the provider API key and rotate the id so a running gateway holding
/// the old key is flagged for recreation.
pub fn delete_gateway_api_key() -> Result<(), String> {
    let delete_result = delete_entry(GATEWAY_API_KEY_SERVICE, "the gateway provider API key");
    let version_result = bump_gateway_secret_version();
    delete_result.and(version_result)
}

/// The gateway master key, minting one on first use.
///
/// The gateway is published on a host port so project containers can reach it,
/// which means an unauthenticated gateway would be an open proxy onto the
/// user's provider account for anything that can route to the host. LiteLLM
/// only enforces auth when a master key is configured, so Triple-C always
/// configures one.
pub fn get_or_create_gateway_master_key() -> Result<String, String> {
    if let Some(existing) = read_entry(GATEWAY_MASTER_KEY_SERVICE, "the gateway master key")? {
        if !existing.trim().is_empty() {
            return Ok(existing);
        }
    }
    regenerate_gateway_master_key()
}

/// Mint a new gateway master key, invalidating the old one. Projects using the
/// previous value must be updated.
pub fn regenerate_gateway_master_key() -> Result<String, String> {
    // LiteLLM requires the master key to start with `sk-`.
    let key = format!("sk-triple-c-{}", uuid::Uuid::new_v4().simple());

    let entry = keyring::Entry::new(GATEWAY_MASTER_KEY_SERVICE, KEYCHAIN_ACCOUNT)
        .map_err(|e| format!("Keyring error: {}", e))?;
    entry
        .set_password(&key)
        .map_err(|e| format!("Failed to store the gateway master key: {}", e))?;

    bump_gateway_secret_version()?;
    Ok(key)
}


#[cfg(test)]
mod tests {
    use super::*;

    /// The regression this list exists for. `openai-compatible-api-key` was
    /// written by `store_secrets_for_project` and missing from the delete list,
    /// so it survived project deletion.
    #[test]
    fn every_secret_the_app_writes_is_one_it_can_delete() {
        for key in [
            "git-token",
            "aws-access-key-id",
            "aws-secret-access-key",
            "aws-session-token",
            "aws-bearer-token",
            "openai-compatible-api-key",
        ] {
            assert!(
                PROJECT_SECRET_KEYS.contains(&key),
                "{} is written by commands/project_commands.rs but would outlive the project",
                key
            );
        }
    }

    #[test]
    fn the_key_list_has_no_duplicates() {
        let mut seen = std::collections::HashSet::new();
        for key in PROJECT_SECRET_KEYS {
            assert!(seen.insert(*key), "duplicate project secret key {}", key);
        }
    }

    /// A key that is not in the list is refused *before* any keychain entry is
    /// constructed, which is what makes the list authoritative rather than
    /// advisory. Without this, a new secret can be stored under a name nothing
    /// ever deletes.
    #[test]
    fn an_unlisted_key_cannot_be_stored_at_all() {
        let err = store_project_secret("some-project", "brand-new-token", "value")
            .expect_err("an unlisted key must be refused");
        assert!(
            err.contains("PROJECT_SECRET_KEYS"),
            "the refusal should say how to fix it: {}",
            err
        );

        let err = get_project_secret("some-project", "brand-new-token")
            .expect_err("an unlisted key must be refused on read too");
        assert!(err.contains("brand-new-token"), "{}", err);

        let err = delete_project_secret("some-project", "brand-new-token")
            .expect_err("an unlisted key must be refused on delete too");
        assert!(err.contains("brand-new-token"), "{}", err);
    }

    /// The blanked-field case. `AccessSection.tsx` sends `gitToken || null`, so
    /// a cleared field arrives as `None` — and before this existed, `None` was
    /// skipped and the old secret stayed in the keychain forever.
    #[test]
    fn a_blanked_field_clears_rather_than_being_skipped() {
        assert_eq!(secret_to_store(None), None);
        assert_eq!(secret_to_store(Some("")), None);
        assert_eq!(secret_to_store(Some("   \t\n")), None);
    }

    #[test]
    fn a_real_value_is_stored_trimmed() {
        assert_eq!(secret_to_store(Some("ghp_abc123")), Some("ghp_abc123"));
        // Pasted credentials routinely carry a trailing newline.
        assert_eq!(secret_to_store(Some("  ghp_abc123\n")), Some("ghp_abc123"));
    }
}
