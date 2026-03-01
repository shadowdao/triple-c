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
