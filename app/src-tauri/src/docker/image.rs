use bollard::image::{BuildImageOptions, CreateImageOptions, ListImagesOptions};
use bollard::models::ImageSummary;
use futures_util::StreamExt;
use include_dir::{include_dir, Dir};
use std::collections::HashMap;
use std::io::Write;

use super::client::get_docker;
use crate::models::container_config;

const DOCKERFILE: &str = include_str!("../../../../container/Dockerfile");
const ENTRYPOINT: &str = include_str!("../../../../container/entrypoint.sh");
const SCHEDULER: &str = include_str!("../../../../container/triple-c-scheduler");
const TASK_RUNNER: &str = include_str!("../../../../container/triple-c-task-runner");
const OSC52_CLIPBOARD: &str = include_str!("../../../../container/osc52-clipboard");
const AUDIO_SHIM: &str = include_str!("../../../../container/audio-shim");
const SSO_REFRESH: &str = include_str!("../../../../container/triple-c-sso-refresh");

static MISSION_CONTROL_DIR: Dir = include_dir!("$CARGO_MANIFEST_DIR/../../container/mission-control");

pub async fn image_exists(image_name: &str) -> Result<bool, String> {
    let docker = get_docker()?;

    let filters: HashMap<String, Vec<String>> = HashMap::from([(
        "reference".to_string(),
        vec![image_name.to_string()],
    )]);

    let images: Vec<ImageSummary> = docker
        .list_images(Some(ListImagesOptions {
            filters,
            ..Default::default()
        }))
        .await
        .map_err(|e| format!("Failed to list images: {}", e))?;

    Ok(!images.is_empty())
}

/// Returns the first repo digest (e.g. "sha256:abc...") for the given image,
/// or None if the image doesn't exist locally or has no repo digests.
pub async fn get_local_image_digest(image_name: &str) -> Result<Option<String>, String> {
    let docker = get_docker()?;

    let filters: HashMap<String, Vec<String>> = HashMap::from([(
        "reference".to_string(),
        vec![image_name.to_string()],
    )]);

    let images: Vec<ImageSummary> = docker
        .list_images(Some(ListImagesOptions {
            filters,
            ..Default::default()
        }))
        .await
        .map_err(|e| format!("Failed to list images: {}", e))?;

    if let Some(img) = images.first() {
        // RepoDigests contains entries like "registry/repo@sha256:abc..."
        if let Some(digest_str) = img.repo_digests.first() {
            // Extract the sha256:... part after '@'
            if let Some(pos) = digest_str.find('@') {
                return Ok(Some(digest_str[pos + 1..].to_string()));
            }
            return Ok(Some(digest_str.clone()));
        }
    }

    Ok(None)
}

pub async fn pull_image<F>(image_name: &str, on_progress: F) -> Result<(), String>
where
    F: Fn(String) + Send + 'static,
{
    let docker = get_docker()?;

    // Parse image name into from_image and tag
    let (from_image, tag) = if let Some(pos) = image_name.rfind(':') {
        // Check that the colon is part of a tag, not a port
        let after_colon = &image_name[pos + 1..];
        if after_colon.contains('/') {
            // The colon is part of a port (e.g., host:port/repo)
            (image_name, "latest")
        } else {
            (&image_name[..pos], after_colon)
        }
    } else {
        (image_name, "latest")
    };

    let options = CreateImageOptions {
        from_image,
        tag,
        ..Default::default()
    };

    let mut stream = docker.create_image(Some(options), None, None);

    while let Some(result) = stream.next().await {
        match result {
            Ok(info) => {
                let mut msg_parts = Vec::new();
                if let Some(ref status) = info.status {
                    msg_parts.push(status.clone());
                }
                if let Some(ref progress) = info.progress {
                    msg_parts.push(progress.clone());
                }
                if !msg_parts.is_empty() {
                    on_progress(msg_parts.join(" "));
                }
                if let Some(ref error) = info.error {
                    return Err(format!("Pull error: {}", error));
                }
            }
            Err(e) => return Err(format!("Pull stream error: {}", e)),
        }
    }

    Ok(())
}

pub async fn build_image<F>(on_progress: F) -> Result<(), String>
where
    F: Fn(String) + Send + 'static,
{
    let docker = get_docker()?;
    let full_name = container_config::local_build_image_name();

    let tar_bytes = create_build_context().map_err(|e| format!("Failed to create build context: {}", e))?;

    let options = BuildImageOptions {
        t: full_name.as_str(),
        rm: true,
        forcerm: true,
        ..Default::default()
    };

    let mut stream = docker.build_image(options, None, Some(tar_bytes.into()));

    while let Some(result) = stream.next().await {
        match result {
            Ok(output) => {
                if let Some(stream) = output.stream {
                    on_progress(stream);
                }
                if let Some(error) = output.error {
                    return Err(format!("Build error: {}", error));
                }
            }
            Err(e) => return Err(format!("Build stream error: {}", e)),
        }
    }

    Ok(())
}

fn append_file_to_archive(
    archive: &mut tar::Builder<&mut Vec<u8>>,
    path: &str,
    content: &[u8],
    mode: u32,
) -> Result<(), std::io::Error> {
    let mut header = tar::Header::new_gnu();
    header.set_size(content.len() as u64);
    header.set_mode(mode);
    header.set_cksum();
    archive.append_data(&mut header, path, content)
}

fn append_embedded_dir(
    archive: &mut tar::Builder<&mut Vec<u8>>,
    dir: &Dir,
    prefix: &str,
) -> Result<(), std::io::Error> {
    for file in dir.files() {
        let path = format!("{}/{}", prefix, file.path().display());
        append_file_to_archive(archive, &path, file.contents(), 0o644)?;
    }
    for subdir in dir.dirs() {
        append_embedded_dir(archive, subdir, prefix)?;
    }
    Ok(())
}

fn create_build_context() -> Result<Vec<u8>, std::io::Error> {
    let mut buf = Vec::new();
    {
        let mut archive = tar::Builder::new(&mut buf);

        append_file_to_archive(&mut archive, "Dockerfile", DOCKERFILE.as_bytes(), 0o644)?;
        append_file_to_archive(&mut archive, "entrypoint.sh", ENTRYPOINT.as_bytes(), 0o755)?;
        append_file_to_archive(&mut archive, "triple-c-scheduler", SCHEDULER.as_bytes(), 0o755)?;
        append_file_to_archive(&mut archive, "triple-c-task-runner", TASK_RUNNER.as_bytes(), 0o755)?;
        append_file_to_archive(&mut archive, "osc52-clipboard", OSC52_CLIPBOARD.as_bytes(), 0o755)?;
        append_file_to_archive(&mut archive, "audio-shim", AUDIO_SHIM.as_bytes(), 0o755)?;
        append_file_to_archive(&mut archive, "triple-c-sso-refresh", SSO_REFRESH.as_bytes(), 0o755)?;

        append_embedded_dir(&mut archive, &MISSION_CONTROL_DIR, "mission-control")?;

        archive.finish()?;
    }

    let _ = buf.flush();
    Ok(buf)
}

#[cfg(all(test, unix))]
mod tests {
    use super::ENTRYPOINT;
    use std::fs;
    use std::path::Path;
    use std::process::Command;

    /// The `sync_host_ssh` function from `entrypoint.sh`, cut out between its
    /// markers so it can be run against temp dirs instead of the real ~/.ssh.
    fn sync_fn() -> &'static str {
        let start = ENTRYPOINT.find("# >>> sync_host_ssh").expect("start marker");
        let end = ENTRYPOINT.find("# <<< sync_host_ssh").expect("end marker");
        &ENTRYPOINT[start..end]
    }

    fn sync(src: &Path, dest: &Path) -> String {
        let script = format!("{}\nsync_host_ssh \"$1\" \"$2\"\n", sync_fn());
        let out = Command::new("bash")
            .arg("-c")
            .arg(script)
            .arg("sync")
            .arg(src)
            .arg(dest)
            .output()
            .expect("run bash");
        assert!(out.status.success(), "sync failed: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    fn read(p: &Path) -> String {
        fs::read_to_string(p).unwrap()
    }

    #[test]
    fn container_created_keys_survive_restart() {
        let tmp = tempfile::tempdir().unwrap();
        let (src, dest) = (tmp.path().join("host"), tmp.path().join("ssh"));
        fs::create_dir_all(&src).unwrap();
        fs::write(src.join("id_host"), "host-key").unwrap();

        sync(&src, &dest);
        fs::write(dest.join("id_container"), "made-in-container").unwrap();
        sync(&src, &dest);

        assert_eq!(read(&dest.join("id_container")), "made-in-container");
        assert_eq!(read(&dest.join("id_host")), "host-key");
    }

    #[test]
    fn container_file_wins_over_same_named_host_file() {
        let tmp = tempfile::tempdir().unwrap();
        let (src, dest) = (tmp.path().join("host"), tmp.path().join("ssh"));
        fs::create_dir_all(&src).unwrap();
        fs::write(src.join("id_host"), "host-key").unwrap();
        sync(&src, &dest);

        fs::write(dest.join("id_ed25519"), "container-key").unwrap();
        fs::write(src.join("id_ed25519"), "host-ed25519").unwrap();
        let out = sync(&src, &dest);

        assert_eq!(read(&dest.join("id_ed25519")), "container-key");
        assert!(out.contains("keeping container's"), "{out}");
    }

    #[test]
    fn host_changes_propagate_until_edited_in_container() {
        let tmp = tempfile::tempdir().unwrap();
        let (src, dest) = (tmp.path().join("host"), tmp.path().join("ssh"));
        fs::create_dir_all(&src).unwrap();
        fs::write(src.join("config"), "v1").unwrap();
        fs::write(src.join("id_old"), "old").unwrap();
        sync(&src, &dest);

        fs::write(src.join("config"), "v2").unwrap();
        fs::remove_file(src.join("id_old")).unwrap();
        sync(&src, &dest);
        assert_eq!(read(&dest.join("config")), "v2");
        assert!(!dest.join("id_old").exists(), "host-removed, unedited file is dropped");

        fs::write(dest.join("config"), "edited in container").unwrap();
        fs::write(src.join("config"), "v3").unwrap();
        sync(&src, &dest);
        assert_eq!(read(&dest.join("config")), "edited in container");

        fs::remove_file(src.join("config")).unwrap();
        sync(&src, &dest);
        assert_eq!(read(&dest.join("config")), "edited in container");
    }

    #[test]
    fn known_hosts_is_unioned() {
        let tmp = tempfile::tempdir().unwrap();
        let (src, dest) = (tmp.path().join("host"), tmp.path().join("ssh"));
        fs::create_dir_all(&src).unwrap();
        fs::create_dir_all(&dest).unwrap();
        fs::write(src.join("known_hosts"), "hostA key\n").unwrap();
        fs::write(dest.join("known_hosts"), "containerB key\n").unwrap();

        sync(&src, &dest);
        let kh = read(&dest.join("known_hosts"));
        assert!(kh.contains("hostA key") && kh.contains("containerB key"), "{kh}");
    }

    #[test]
    fn upgrade_from_wipe_and_copy_backs_up_differing_files() {
        // No manifest yet: the state an existing project is in on its first
        // start with this entrypoint.
        let tmp = tempfile::tempdir().unwrap();
        let (src, dest) = (tmp.path().join("host"), tmp.path().join("ssh"));
        fs::create_dir_all(&src).unwrap();
        fs::create_dir_all(&dest).unwrap();
        fs::write(src.join("id_rsa"), "host").unwrap();
        fs::write(dest.join("id_rsa"), "container").unwrap();
        fs::write(dest.join("id_extra"), "extra").unwrap();

        sync(&src, &dest);

        assert_eq!(read(&dest.join("id_rsa")), "host");
        assert_eq!(read(&dest.join("id_rsa.triple-c-backup")), "container");
        assert_eq!(read(&dest.join("id_extra")), "extra");
    }

    #[test]
    fn nested_host_files_are_copied() {
        let tmp = tempfile::tempdir().unwrap();
        let (src, dest) = (tmp.path().join("host"), tmp.path().join("ssh"));
        fs::create_dir_all(src.join("config.d")).unwrap();
        fs::write(src.join("config.d/work"), "Host work").unwrap();

        sync(&src, &dest);
        assert_eq!(read(&dest.join("config.d/work")), "Host work");
    }
}
