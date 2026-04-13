use tauri::{AppHandle, Emitter, State};

use crate::docker::stt;
use crate::models::app_settings::SttStatus;
use crate::AppState;

#[tauri::command]
pub async fn get_stt_status(state: State<'_, AppState>) -> Result<SttStatus, String> {
    let settings = state.settings_store.get();
    stt::get_stt_status(&settings.stt).await
}

#[tauri::command]
pub async fn start_stt(state: State<'_, AppState>) -> Result<SttStatus, String> {
    let settings = state.settings_store.get();
    stt::ensure_stt_running(&settings.stt).await
}

#[tauri::command]
pub async fn stop_stt() -> Result<(), String> {
    stt::stop_stt_container().await
}

#[tauri::command]
pub async fn build_stt_image(app_handle: AppHandle) -> Result<(), String> {
    stt::build_stt_image(move |msg| {
        let _ = app_handle.emit("stt-build-progress", &msg);
    })
    .await
}

#[tauri::command]
pub async fn pull_stt_image(app_handle: AppHandle) -> Result<(), String> {
    stt::pull_stt_image(move |msg| {
        let _ = app_handle.emit("stt-pull-progress", &msg);
    })
    .await
}

#[tauri::command]
pub async fn transcribe_audio(
    audio_data: Vec<u8>,
    state: State<'_, AppState>,
) -> Result<String, String> {
    let settings = state.settings_store.get();
    if !settings.stt.enabled {
        return Err("STT is not enabled".to_string());
    }

    let url = format!("http://127.0.0.1:{}/transcribe", settings.stt.port);

    let file_part = reqwest::multipart::Part::bytes(audio_data)
        .file_name("recording.wav")
        .mime_str("audio/wav")
        .map_err(|e| format!("Failed to create multipart: {}", e))?;

    let mut form = reqwest::multipart::Form::new().part("file", file_part);

    if let Some(ref lang) = settings.stt.language {
        form = form.text("language", lang.clone());
    }

    let client = reqwest::Client::new();
    let response = client
        .post(&url)
        .multipart(form)
        .send()
        .await
        .map_err(|e| {
            if e.is_connect() {
                "STT container is not running. Start it from Settings.".to_string()
            } else {
                format!("Transcription request failed: {}", e)
            }
        })?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(format!("Transcription failed ({}): {}", status, body));
    }

    let result: serde_json::Value = response
        .json()
        .await
        .map_err(|e| format!("Failed to parse transcription response: {}", e))?;

    result["text"]
        .as_str()
        .map(|s| s.to_string())
        .ok_or_else(|| "No text in transcription response".to_string())
}
