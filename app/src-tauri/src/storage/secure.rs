const SERVICE_NAME: &str = "triple-c";
const API_KEY_USER: &str = "anthropic-api-key";

pub fn store_api_key(key: &str) -> Result<(), String> {
    let entry = keyring::Entry::new(SERVICE_NAME, API_KEY_USER)
        .map_err(|e| format!("Keyring error: {}", e))?;
    entry
        .set_password(key)
        .map_err(|e| format!("Failed to store API key: {}", e))
}

pub fn get_api_key() -> Result<Option<String>, String> {
    let entry = keyring::Entry::new(SERVICE_NAME, API_KEY_USER)
        .map_err(|e| format!("Keyring error: {}", e))?;
    match entry.get_password() {
        Ok(key) => Ok(Some(key)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(format!("Failed to retrieve API key: {}", e)),
    }
}

pub fn delete_api_key() -> Result<(), String> {
    let entry = keyring::Entry::new(SERVICE_NAME, API_KEY_USER)
        .map_err(|e| format!("Keyring error: {}", e))?;
    match entry.delete_credential() {
        Ok(()) => Ok(()),
        Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(format!("Failed to delete API key: {}", e)),
    }
}

pub fn has_api_key() -> Result<bool, String> {
    match get_api_key() {
        Ok(Some(_)) => Ok(true),
        Ok(None) => Ok(false),
        Err(e) => Err(e),
    }
}
