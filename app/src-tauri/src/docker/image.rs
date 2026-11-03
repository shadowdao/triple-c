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

/// Split an image reference into the `fromImage` and `tag` the pull API takes.
/// A digest reference (`repo@sha256:…`, optionally `repo:tag@sha256:…`) is
/// pulled by digest: the API takes the digest in the `tag` field.
fn split_image_ref(image_name: &str) -> (&str, &str) {
    if let Some((name, digest)) = image_name.split_once('@') {
        let repo = match name.rfind(':') {
            Some(pos) if !name[pos + 1..].contains('/') => &name[..pos],
            _ => name,
        };
        return (repo, digest);
    }
    match image_name.rfind(':') {
        // The colon is part of a port (e.g., host:port/repo), not a tag.
        Some(pos) if image_name[pos + 1..].contains('/') => (image_name, "latest"),
        Some(pos) => (&image_name[..pos], &image_name[pos + 1..]),
        None => (image_name, "latest"),
    }
}

pub async fn pull_image<F>(image_name: &str, on_progress: F) -> Result<(), String>
where
    F: Fn(String) + Send + 'static,
{
    let docker = get_docker()?;

    let (from_image, tag) = split_image_ref(image_name);

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

#[cfg(test)]
mod apt_refresh_tests {
    use super::DOCKERFILE;

    const BUILD_WORKFLOW: &str = include_str!("../../../../.gitea/workflows/build.yml");

    /// The `RUN` instruction (with its `\` continuations) that contains `needle`.
    fn run_containing(needle: &str) -> String {
        let mut run = String::new();
        let mut current = String::new();
        for line in DOCKERFILE.lines() {
            if line.starts_with("RUN ") {
                current.clear();
            }
            current.push_str(line);
            current.push('\n');
            if !line.trim_end().ends_with('\\') {
                if current.contains(needle) && current.starts_with("RUN ") {
                    run = current.clone();
                }
                current.clear();
            }
        }
        run
    }

    /// CI builds with `cache-from: type=gha`, so the `apt-get upgrade` layer is
    /// only re-run when its cache key changes. An ARG changes a RUN's key only
    /// where it is *used*, so the upgrade's RUN must reference it, after an
    /// `ARG APT_REFRESH` declared in its stage.
    #[test]
    fn the_apt_upgrade_layer_is_keyed_on_apt_refresh() {
        let arg = DOCKERFILE.find("\nARG APT_REFRESH").expect("ARG APT_REFRESH declared");
        let run = run_containing("upgrade -y");
        assert!(!run.is_empty(), "no RUN with apt-get upgrade");
        assert!(DOCKERFILE.find(&run).unwrap() > arg, "ARG must come before the RUN");
        assert!(run.contains("${APT_REFRESH"), "RUN must use APT_REFRESH: {run}");
    }

    /// …and CI must actually pass a value that changes at least weekly, and
    /// rebuild on a schedule so a week with no commits still gets the fixes.
    #[test]
    fn ci_passes_a_weekly_apt_refresh_and_rebuilds_on_a_schedule() {
        assert!(BUILD_WORKFLOW.contains("APT_REFRESH=${{ steps.apt.outputs.stamp }}"));
        assert!(BUILD_WORKFLOW.contains("date -u +%G-W%V"));
        assert!(BUILD_WORKFLOW.contains("schedule:"));
        // A scheduled run must publish, a PR run must not.
        assert!(BUILD_WORKFLOW.contains("push: ${{ gitea.event_name != 'pull_request' }}"));
    }
}

#[cfg(test)]
mod pull_ref_tests {
    use super::split_image_ref;

    #[test]
    fn split_image_ref_handles_tags_ports_and_digests() {
        assert_eq!(split_image_ref("repo:v1"), ("repo", "v1"));
        assert_eq!(split_image_ref("repo"), ("repo", "latest"));
        assert_eq!(split_image_ref("host:5000/repo"), ("host:5000/repo", "latest"));
        assert_eq!(split_image_ref("host:5000/repo:v1"), ("host:5000/repo", "v1"));
        assert_eq!(split_image_ref("ghcr.io/a/b@sha256:abc"), ("ghcr.io/a/b", "sha256:abc"));
        assert_eq!(split_image_ref("ghcr.io/a/b:v1@sha256:abc"), ("ghcr.io/a/b", "sha256:abc"));
        assert_eq!(split_image_ref("host:5000/b@sha256:abc"), ("host:5000/b", "sha256:abc"));
    }
}
