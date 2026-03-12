use std::sync::OnceLock;
use tokio::sync::Mutex;

const HELP_URL: &str =
    "https://repo.anhonesthost.net/cybercovellc/triple-c/raw/branch/main/HOW-TO-USE.md";

const EMBEDDED_HELP: &str = include_str!("../../../../HOW-TO-USE.md");

/// Cached help content fetched from the remote repo (or `None` if not yet fetched).
static CACHED_HELP: OnceLock<Mutex<Option<String>>> = OnceLock::new();

/// Return the help markdown content.
///
/// On the first call, tries to fetch the latest version from the gitea repo.
/// If that fails (network error, timeout, etc.), falls back to the version
/// embedded at compile time. The result is cached for the rest of the session.
#[tauri::command]
pub async fn get_help_content() -> Result<String, String> {
    let mutex = CACHED_HELP.get_or_init(|| Mutex::new(None));
    let mut guard = mutex.lock().await;

    if let Some(ref cached) = *guard {
        return Ok(cached.clone());
    }

    let content = match fetch_remote_help().await {
        Ok(md) => {
            log::info!("Loaded help content from remote repo");
            md
        }
        Err(e) => {
            log::info!("Using embedded help content (remote fetch failed: {})", e);
            EMBEDDED_HELP.to_string()
        }
    };

    *guard = Some(content.clone());
    Ok(content)
}

async fn fetch_remote_help() -> Result<String, String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|e| format!("Failed to create HTTP client: {}", e))?;

    let resp = client
        .get(HELP_URL)
        .send()
        .await
        .map_err(|e| format!("Failed to fetch help content: {}", e))?;

    if !resp.status().is_success() {
        return Err(format!("Remote returned status {}", resp.status()));
    }

    resp.text()
        .await
        .map_err(|e| format!("Failed to read response body: {}", e))
}
