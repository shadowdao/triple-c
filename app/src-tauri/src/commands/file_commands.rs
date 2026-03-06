use bollard::container::{DownloadFromContainerOptions, UploadToContainerOptions};
use futures_util::StreamExt;
use serde::Serialize;
use tauri::State;

use crate::docker::client::get_docker;
use crate::docker::exec::exec_oneshot;
use crate::AppState;

#[derive(Debug, Serialize)]
pub struct FileEntry {
    pub name: String,
    pub path: String,
    pub is_directory: bool,
    pub size: u64,
    pub modified: String,
    pub permissions: String,
}

#[tauri::command]
pub async fn list_container_files(
    project_id: String,
    path: String,
    state: State<'_, AppState>,
) -> Result<Vec<FileEntry>, String> {
    let project = state
        .projects_store
        .get(&project_id)
        .ok_or_else(|| format!("Project {} not found", project_id))?;

    let container_id = project
        .container_id
        .as_ref()
        .ok_or_else(|| "Container not running".to_string())?;

    let cmd = vec![
        "find".to_string(),
        path.clone(),
        "-maxdepth".to_string(),
        "1".to_string(),
        "-not".to_string(),
        "-name".to_string(),
        ".".to_string(),
        "-printf".to_string(),
        "%f\t%y\t%s\t%T@\t%m\n".to_string(),
    ];

    let output = exec_oneshot(container_id, cmd).await?;

    let mut entries: Vec<FileEntry> = output
        .lines()
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| {
            let parts: Vec<&str> = line.split('\t').collect();
            if parts.len() < 5 {
                return None;
            }
            let name = parts[0].to_string();
            let is_directory = parts[1] == "d";
            let size = parts[2].parse::<u64>().unwrap_or(0);
            let modified_epoch = parts[3].parse::<f64>().unwrap_or(0.0);
            let permissions = parts[4].to_string();

            // Convert epoch to ISO-ish string
            let modified = {
                let secs = modified_epoch as i64;
                let dt = chrono::DateTime::from_timestamp(secs, 0)
                    .unwrap_or_default();
                dt.format("%Y-%m-%d %H:%M:%S").to_string()
            };

            let entry_path = if path.ends_with('/') {
                format!("{}{}", path, name)
            } else {
                format!("{}/{}", path, name)
            };

            Some(FileEntry {
                name,
                path: entry_path,
                is_directory,
                size,
                modified,
                permissions,
            })
        })
        .collect();

    // Sort: directories first, then alphabetical
    entries.sort_by(|a, b| {
        b.is_directory
            .cmp(&a.is_directory)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });

    Ok(entries)
}

#[tauri::command]
pub async fn download_container_file(
    project_id: String,
    container_path: String,
    host_path: String,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let project = state
        .projects_store
        .get(&project_id)
        .ok_or_else(|| format!("Project {} not found", project_id))?;

    let container_id = project
        .container_id
        .as_ref()
        .ok_or_else(|| "Container not running".to_string())?;

    let docker = get_docker()?;

    let mut stream = docker.download_from_container(
        container_id,
        Some(DownloadFromContainerOptions {
            path: container_path.clone(),
        }),
    );

    let mut tar_bytes = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| format!("Failed to download file: {}", e))?;
        tar_bytes.extend_from_slice(&chunk);
    }

    // Extract single file from tar archive
    let mut archive = tar::Archive::new(&tar_bytes[..]);
    let mut found = false;
    for entry in archive
        .entries()
        .map_err(|e| format!("Failed to read tar entries: {}", e))?
    {
        let mut entry = entry.map_err(|e| format!("Failed to read tar entry: {}", e))?;
        let mut contents = Vec::new();
        std::io::Read::read_to_end(&mut entry, &mut contents)
            .map_err(|e| format!("Failed to read file contents: {}", e))?;
        std::fs::write(&host_path, &contents)
            .map_err(|e| format!("Failed to write file to host: {}", e))?;
        found = true;
        break;
    }

    if !found {
        return Err("File not found in tar archive".to_string());
    }

    Ok(())
}

#[tauri::command]
pub async fn upload_file_to_container(
    project_id: String,
    host_path: String,
    container_dir: String,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let project = state
        .projects_store
        .get(&project_id)
        .ok_or_else(|| format!("Project {} not found", project_id))?;

    let container_id = project
        .container_id
        .as_ref()
        .ok_or_else(|| "Container not running".to_string())?;

    let docker = get_docker()?;

    let file_data = std::fs::read(&host_path)
        .map_err(|e| format!("Failed to read host file: {}", e))?;

    let file_name = std::path::Path::new(&host_path)
        .file_name()
        .ok_or_else(|| "Invalid file path".to_string())?
        .to_string_lossy()
        .to_string();

    // Build tar archive in memory
    let mut tar_buf = Vec::new();
    {
        let mut builder = tar::Builder::new(&mut tar_buf);
        let mut header = tar::Header::new_gnu();
        header.set_size(file_data.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        builder
            .append_data(&mut header, &file_name, &file_data[..])
            .map_err(|e| format!("Failed to create tar entry: {}", e))?;
        builder
            .finish()
            .map_err(|e| format!("Failed to finalize tar: {}", e))?;
    }

    docker
        .upload_to_container(
            container_id,
            Some(UploadToContainerOptions {
                path: container_dir,
                ..Default::default()
            }),
            tar_buf.into(),
        )
        .await
        .map_err(|e| format!("Failed to upload file to container: {}", e))?;

    Ok(())
}
