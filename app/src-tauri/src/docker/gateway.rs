//! Lifecycle for the **model gateway** container — a pinned LiteLLM proxy that
//! Triple-C runs as a sibling of the project containers.
//!
//! Shape mirrors `docker::stt`: an image that is either pulled from a registry
//! or built locally from an embedded Dockerfile, a fixed container name, a
//! named volume, and `get_* / ensure_*_running / stop_* / pull_* / build_*`.
//!
//! Two things differ from STT, both deliberate:
//!
//! * **The port is published on `0.0.0.0`, not `127.0.0.1`.** STT is consumed
//!   by the Tauri host process, so loopback is enough. The gateway is consumed
//!   by *project containers*, which sit on Docker's default bridge and reach
//!   the host through the bridge gateway — a loopback-only bind is invisible to
//!   them. See [`gateway_base_url`].
//! * **The rendered config is uploaded into the container over the Docker
//!   API** rather than passed as env. It holds the provider API key, and both
//!   env vars and labels are readable by anything on the host via
//!   `docker inspect`.

use bollard::container::{
    Config, CreateContainerOptions, ListContainersOptions, RemoveContainerOptions,
    StartContainerOptions, StopContainerOptions, UploadToContainerOptions,
};
use bollard::image::BuildImageOptions;
use bollard::models::{HostConfig, Mount, MountTypeEnum, PortBinding};
use futures_util::StreamExt;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::io::Write;

use super::client::get_docker;
use crate::models::gateway_settings::{GatewaySettings, GatewayStatus};
use crate::storage::secure;

const GATEWAY_CONTAINER_NAME: &str = "triple-c-gateway";
const GATEWAY_CONFIG_VOLUME: &str = "triple-c-gateway-config";

/// Upstream LiteLLM, pinned to an exact release.
///
/// LiteLLM 1.82.7 and 1.82.8 shipped credential-harvesting malware on PyPI, so
/// nothing here may float a tag or resolve `litellm` at build time. v1.96.0 is
/// also above the 1.84.0 floor set by the proxy auth-bypass CVEs — see the long
/// comment in `gateway-container/Dockerfile`, and keep the two in lockstep.
const GATEWAY_REGISTRY_IMAGE: &str = "ghcr.io/berriai/litellm:v1.96.0";
const GATEWAY_LOCAL_IMAGE: &str = "triple-c-gateway:latest";

const GATEWAY_DOCKERFILE: &str = include_str!("../../../../gateway-container/Dockerfile");
const GATEWAY_DEFAULT_CONFIG: &str = include_str!("../../../../gateway-container/config.yaml");

/// Where the generated config lands inside the container. Backed by
/// [`GATEWAY_CONFIG_VOLUME`] so the file with the provider key lives in a
/// Docker-managed volume rather than an image layer.
const GATEWAY_CONFIG_DIR: &str = "/etc/litellm";
const GATEWAY_CONFIG_PATH: &str = "/etc/litellm/config.yaml";

/// Container-side port. Only the *host* port is user-configurable.
const GATEWAY_INTERNAL_PORT: u16 = 4000;

const CONFIG_FINGERPRINT_LABEL: &str = "triple-c.gateway.config-fingerprint";

/// The value a project should use as its base URL (`ANTHROPIC_BASE_URL`).
///
/// Project containers run on Docker's default bridge with no user-defined
/// network and no `--add-host`, so the only address they share with the
/// gateway is the host itself. Publishing the gateway on `0.0.0.0:<port>`
/// makes it reachable from every container network on the machine:
///
/// * Docker Desktop (macOS / Windows / WSL2) resolves `host.docker.internal`
///   from inside containers automatically — that is the portable value and the
///   one already suggested by the existing OpenAI-compatible placeholder text.
/// * On native Linux Docker `host.docker.internal` is not injected, and the
///   equivalent address is the default bridge gateway, normally
///   `http://172.17.0.1:<port>`.
pub fn gateway_base_url(port: u16) -> String {
    format!("http://host.docker.internal:{}", port)
}

fn sha256_hex(input: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(input.as_bytes());
    format!("{:x}", hasher.finalize())
}

pub async fn get_gateway_status(settings: &GatewaySettings) -> Result<GatewayStatus, String> {
    let image_exists = super::image::image_exists(GATEWAY_REGISTRY_IMAGE)
        .await
        .unwrap_or(false)
        || super::image::image_exists(GATEWAY_LOCAL_IMAGE)
            .await
            .unwrap_or(false);

    let (container_exists, running) = match find_gateway_container().await? {
        Some((_, state, _)) => (true, state == "running"),
        None => (false, false),
    };

    Ok(GatewayStatus {
        container_exists,
        running,
        port: settings.port,
        image_exists,
        model_count: settings.valid_models().len(),
        has_api_key: secure::has_gateway_api_key(),
        base_url: gateway_base_url(settings.port),
    })
}

/// `(id, state, config fingerprint label)` for the gateway container, if any.
async fn find_gateway_container() -> Result<Option<(String, String, String)>, String> {
    let docker = get_docker()?;

    let filters: HashMap<String, Vec<String>> = HashMap::from([(
        "name".to_string(),
        vec![format!("/{}", GATEWAY_CONTAINER_NAME)],
    )]);

    let containers = docker
        .list_containers(Some(ListContainersOptions {
            all: true,
            filters,
            ..Default::default()
        }))
        .await
        .map_err(|e| format!("Failed to list containers: {}", e))?;

    if let Some(container) = containers.first() {
        let id = container.id.clone().unwrap_or_default();
        let state = container.state.clone().unwrap_or_default();
        let fingerprint = container
            .labels
            .as_ref()
            .and_then(|l| l.get(CONFIG_FINGERPRINT_LABEL))
            .cloned()
            .unwrap_or_default();

        return Ok(Some((id, state, fingerprint)));
    }

    Ok(None)
}

// ─────────────────────────────────────────────────────────────────────────────
// Config generation
// ─────────────────────────────────────────────────────────────────────────────

/// Render a YAML double-quoted scalar.
///
/// Everything that reaches the config comes from user input (model names, base
/// URLs, keys), so nothing may be interpolated raw — a stray `"` or newline
/// would otherwise rewrite the document.
fn yaml_str(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\x{:02x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// The parts of the config that are safe to hash into a Docker label — i.e.
/// everything except the two secrets, whose changes are tracked by the
/// keychain rotation id instead.
fn config_shape(settings: &GatewaySettings) -> String {
    let models: Vec<String> = settings
        .valid_models()
        .iter()
        .map(|m| format!("{}={}", m.name.trim(), m.model_id.trim()))
        .collect();
    format!(
        "provider={};api_base={};port={};models={}",
        settings.provider.trim(),
        settings.api_base.as_deref().unwrap_or("").trim(),
        settings.port,
        models.join(",")
    )
}

/// Render the LiteLLM config for the current settings.
///
/// `api_key` and `master_key` come from the keychain. The returned string
/// contains both — it goes straight into the Docker upload and must never be
/// logged or surfaced.
fn render_config(settings: &GatewaySettings, api_key: &str, master_key: &str) -> String {
    let provider = settings.provider.trim();
    let api_base = settings
        .api_base
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());

    let mut out = String::from(
        "# Generated by Triple-C — do not edit by hand; it is overwritten on every\n\
         # gateway (re)start from Settings → Model Gateway.\n\
         model_list:\n",
    );

    for model in settings.valid_models() {
        out.push_str(&format!("  - model_name: {}\n", yaml_str(model.name.trim())));
        out.push_str("    litellm_params:\n");
        out.push_str(&format!(
            "      model: {}\n",
            yaml_str(&format!("{}/{}", provider, model.model_id.trim()))
        ));
        out.push_str(&format!("      api_key: {}\n", yaml_str(api_key)));
        if let Some(base) = api_base {
            out.push_str(&format!("      api_base: {}\n", yaml_str(base)));
        }
    }

    out.push_str("general_settings:\n");
    out.push_str(&format!("  master_key: {}\n", yaml_str(master_key)));
    out.push_str("litellm_settings:\n");
    // Claude Code's Anthropic-format requests carry fields some providers
    // reject outright; dropping the unsupported ones is what lets the
    // translation survive across providers.
    out.push_str("  drop_params: true\n");

    out
}

/// Upload the rendered config into the container's config volume.
///
/// Runs against a *created but not yet started* container, which is when the
/// volume already exists but LiteLLM has not read anything from it.
async fn upload_config(container_id: &str, config: &str) -> Result<(), String> {
    let docker = get_docker()?;

    let mut buf = Vec::new();
    {
        let mut archive = tar::Builder::new(&mut buf);
        let mut header = tar::Header::new_gnu();
        header.set_size(config.len() as u64);
        // World-readable: the upstream image may run LiteLLM as a non-root
        // user, and a root-owned 0600 file would simply be unreadable. The
        // secret is only exposed to the gateway container itself, which is
        // the one process that needs it.
        header.set_mode(0o644);
        header.set_cksum();
        archive
            .append_data(&mut header, "config.yaml", config.as_bytes())
            .map_err(|e| format!("Failed to build the gateway config archive: {}", e))?;
        archive
            .finish()
            .map_err(|e| format!("Failed to build the gateway config archive: {}", e))?;
    }
    let _ = buf.flush();

    docker
        .upload_to_container(
            container_id,
            Some(UploadToContainerOptions {
                path: GATEWAY_CONFIG_DIR,
                ..Default::default()
            }),
            buf.into(),
        )
        .await
        .map_err(|e| format!("Failed to upload the gateway config: {}", e))
}

// ─────────────────────────────────────────────────────────────────────────────
// Lifecycle
// ─────────────────────────────────────────────────────────────────────────────

async fn create_gateway_container(
    settings: &GatewaySettings,
    fingerprint: &str,
) -> Result<String, String> {
    let docker = get_docker()?;

    // Local build first, then the pinned upstream image — same precedence as
    // the STT container.
    let image = if super::image::image_exists(GATEWAY_LOCAL_IMAGE)
        .await
        .unwrap_or(false)
    {
        GATEWAY_LOCAL_IMAGE.to_string()
    } else if super::image::image_exists(GATEWAY_REGISTRY_IMAGE)
        .await
        .unwrap_or(false)
    {
        GATEWAY_REGISTRY_IMAGE.to_string()
    } else {
        return Err(
            "Gateway image not found. Please pull or build the image first.".to_string(),
        );
    };

    let mut port_bindings = HashMap::new();
    port_bindings.insert(
        format!("{}/tcp", GATEWAY_INTERNAL_PORT),
        Some(vec![PortBinding {
            // Not loopback — project containers reach this through the host.
            // See `gateway_base_url`.
            host_ip: Some("0.0.0.0".to_string()),
            host_port: Some(settings.port.to_string()),
        }]),
    );

    let mut exposed_ports: HashMap<String, HashMap<(), ()>> = HashMap::new();
    exposed_ports.insert(format!("{}/tcp", GATEWAY_INTERNAL_PORT), HashMap::new());

    let host_config = HostConfig {
        port_bindings: Some(port_bindings),
        mounts: Some(vec![Mount {
            target: Some(GATEWAY_CONFIG_DIR.to_string()),
            source: Some(GATEWAY_CONFIG_VOLUME.to_string()),
            typ: Some(MountTypeEnum::VOLUME),
            ..Default::default()
        }]),
        init: Some(true),
        ..Default::default()
    };

    // Non-secret only. Labels are readable by anything on the host.
    let mut labels = HashMap::new();
    labels.insert(CONFIG_FINGERPRINT_LABEL.to_string(), fingerprint.to_string());
    labels.insert(
        "triple-c.gateway.port".to_string(),
        settings.port.to_string(),
    );
    labels.insert(
        "triple-c.gateway.provider".to_string(),
        settings.provider.trim().to_string(),
    );

    let config = Config {
        image: Some(image),
        // The upstream entrypoint (`docker/prod_entrypoint.sh`) execs
        // `litellm "$@"`. Passed explicitly so the pulled upstream image and
        // our locally built one behave identically.
        cmd: Some(vec![
            "--config".to_string(),
            GATEWAY_CONFIG_PATH.to_string(),
            "--host".to_string(),
            "0.0.0.0".to_string(),
            "--port".to_string(),
            GATEWAY_INTERNAL_PORT.to_string(),
        ]),
        exposed_ports: Some(exposed_ports),
        host_config: Some(host_config),
        labels: Some(labels),
        ..Default::default()
    };

    let options = CreateContainerOptions {
        name: GATEWAY_CONTAINER_NAME,
        ..Default::default()
    };

    let response = docker
        .create_container(Some(options), config)
        .await
        .map_err(|e| format!("Failed to create gateway container: {}", e))?;

    Ok(response.id)
}

pub async fn ensure_gateway_running(settings: &GatewaySettings) -> Result<GatewayStatus, String> {
    let docker = get_docker()?;

    if settings.valid_models().is_empty() {
        return Err(
            "The gateway has no models configured. Add at least one model in Settings."
                .to_string(),
        );
    }

    let api_key = secure::get_gateway_api_key()?
        .filter(|k| !k.trim().is_empty())
        .ok_or_else(|| {
            "No provider API key stored for the gateway. Add one in Settings.".to_string()
        })?;
    let master_key = secure::get_or_create_gateway_master_key()?;

    // Rotation id, not a hash of either secret — see `storage::secure`.
    let secret_version = secure::get_gateway_secret_version()?.unwrap_or_default();
    let fingerprint = sha256_hex(&format!(
        "{}|{}",
        config_shape(settings),
        secret_version
    ));

    if let Some((id, state, existing_fingerprint)) = find_gateway_container().await? {
        if existing_fingerprint == fingerprint {
            if state == "running" {
                return get_gateway_status(settings).await;
            }
            docker
                .start_container(&id, None::<StartContainerOptions<String>>)
                .await
                .map_err(|e| format!("Failed to start gateway container: {}", e))?;
            return get_gateway_status(settings).await;
        }

        // Config or a secret changed — recreate so the new config is uploaded.
        if state == "running" {
            docker
                .stop_container(&id, None::<StopContainerOptions>)
                .await
                .map_err(|e| format!("Failed to stop gateway container: {}", e))?;
        }
        docker
            .remove_container(
                &id,
                Some(RemoveContainerOptions {
                    force: true,
                    ..Default::default()
                }),
            )
            .await
            .map_err(|e| format!("Failed to remove gateway container: {}", e))?;
    }

    let id = create_gateway_container(settings, &fingerprint).await?;

    // Upload before the first start: LiteLLM reads the config once at boot.
    let rendered = render_config(settings, &api_key, &master_key);
    if let Err(e) = upload_config(&id, &rendered).await {
        // Don't leave a half-configured container behind for the next run to
        // mistake for a good one.
        let _ = docker
            .remove_container(
                &id,
                Some(RemoveContainerOptions {
                    force: true,
                    ..Default::default()
                }),
            )
            .await;
        return Err(e);
    }

    docker
        .start_container(&id, None::<StartContainerOptions<String>>)
        .await
        .map_err(|e| format!("Failed to start gateway container: {}", e))?;

    log::info!(
        "Model gateway started on port {} ({} model(s))",
        settings.port,
        settings.valid_models().len()
    );

    get_gateway_status(settings).await
}

pub async fn stop_gateway_container() -> Result<(), String> {
    let docker = get_docker()?;

    if let Some((id, state, _)) = find_gateway_container().await? {
        if state == "running" {
            docker
                .stop_container(&id, None::<StopContainerOptions>)
                .await
                .map_err(|e| format!("Failed to stop gateway container: {}", e))?;
        }
    }

    Ok(())
}

/// Ask the running gateway whether it is up. LiteLLM takes several seconds to
/// boot, so "container running" and "gateway answering" are not the same thing.
pub async fn check_gateway_health(port: u16) -> Result<bool, String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .map_err(|e| format!("Failed to build HTTP client: {}", e))?;

    match client
        .get(format!("http://127.0.0.1:{}/health/liveliness", port))
        .send()
        .await
    {
        Ok(response) => Ok(response.status().is_success()),
        Err(e) if e.is_connect() || e.is_timeout() => Ok(false),
        Err(e) => Err(format!("Gateway health check failed: {}", e)),
    }
}

pub async fn pull_gateway_image<F>(on_progress: F) -> Result<(), String>
where
    F: Fn(String) + Send + 'static,
{
    super::image::pull_image(GATEWAY_REGISTRY_IMAGE, on_progress).await
}

pub async fn build_gateway_image<F>(on_progress: F) -> Result<(), String>
where
    F: Fn(String) + Send + 'static,
{
    let docker = get_docker()?;

    let tar_bytes = create_gateway_build_context()
        .map_err(|e| format!("Failed to create gateway build context: {}", e))?;

    let options = BuildImageOptions {
        t: GATEWAY_LOCAL_IMAGE,
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

fn create_gateway_build_context() -> Result<Vec<u8>, std::io::Error> {
    let mut buf = Vec::new();
    {
        let mut archive = tar::Builder::new(&mut buf);

        let mut dockerfile_header = tar::Header::new_gnu();
        dockerfile_header.set_size(GATEWAY_DOCKERFILE.len() as u64);
        dockerfile_header.set_mode(0o644);
        dockerfile_header.set_cksum();
        archive.append_data(
            &mut dockerfile_header,
            "Dockerfile",
            GATEWAY_DOCKERFILE.as_bytes(),
        )?;

        let mut config_header = tar::Header::new_gnu();
        config_header.set_size(GATEWAY_DEFAULT_CONFIG.len() as u64);
        config_header.set_mode(0o644);
        config_header.set_cksum();
        archive.append_data(
            &mut config_header,
            "config.yaml",
            GATEWAY_DEFAULT_CONFIG.as_bytes(),
        )?;

        archive.finish()?;
    }

    let _ = buf.flush();
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::gateway_settings::GatewayModel;

    fn settings() -> GatewaySettings {
        GatewaySettings {
            enabled: true,
            port: 4000,
            provider: "openai".to_string(),
            api_base: None,
            models: vec![
                GatewayModel {
                    name: "gpt-5.1".to_string(),
                    model_id: "gpt-5.1".to_string(),
                },
                // Half-filled rows must not reach the YAML.
                GatewayModel {
                    name: "  ".to_string(),
                    model_id: "gpt-4o".to_string(),
                },
            ],
        }
    }

    #[test]
    fn valid_models_skips_incomplete_rows() {
        assert_eq!(settings().valid_models().len(), 1);
    }

    #[test]
    fn render_config_composes_provider_and_model_id() {
        let yaml = render_config(&settings(), "sk-provider", "sk-master");
        assert!(yaml.contains("model_name: \"gpt-5.1\""));
        assert!(yaml.contains("model: \"openai/gpt-5.1\""));
        assert!(yaml.contains("api_key: \"sk-provider\""));
        assert!(yaml.contains("master_key: \"sk-master\""));
        assert!(yaml.contains("drop_params: true"));
        // The skipped row must be absent.
        assert!(!yaml.contains("gpt-4o"));
    }

    #[test]
    fn render_config_emits_api_base_only_when_set() {
        let mut s = settings();
        assert!(!render_config(&s, "k", "m").contains("api_base"));
        s.api_base = Some("https://example.test/v1".to_string());
        assert!(render_config(&s, "k", "m").contains("api_base: \"https://example.test/v1\""));
        // Blank is treated as unset rather than emitted as an empty URL.
        s.api_base = Some("   ".to_string());
        assert!(!render_config(&s, "k", "m").contains("api_base"));
    }

    #[test]
    fn yaml_str_escapes_injection_attempts() {
        let hostile = "a\"\nmaster_key: \"pwned";
        let quoted = yaml_str(hostile);
        assert!(quoted.starts_with('"') && quoted.ends_with('"'));
        // No raw newline can escape the scalar and start a new YAML key.
        assert!(!quoted[1..quoted.len() - 1].contains('\n'));
        assert!(quoted.contains("\\\""));
    }

    #[test]
    fn config_shape_excludes_secrets_and_tracks_changes() {
        let a = config_shape(&settings());
        let mut s = settings();
        s.models[0].model_id = "gpt-4.1".to_string();
        assert_ne!(a, config_shape(&s));
        assert!(!a.contains("sk-"));
    }

    #[test]
    fn base_url_points_at_the_host_not_loopback() {
        // A project container cannot reach the host's loopback interface.
        let url = gateway_base_url(4000);
        assert_eq!(url, "http://host.docker.internal:4000");
        assert!(!url.contains("127.0.0.1"));
    }
}
