use crate::storage::secure;

#[tauri::command]
pub async fn set_api_key(key: String) -> Result<(), String> {
    secure::store_api_key(&key)
}

#[tauri::command]
pub async fn has_api_key() -> Result<bool, String> {
    secure::has_api_key()
}

#[tauri::command]
pub async fn delete_api_key() -> Result<(), String> {
    secure::delete_api_key()
}
