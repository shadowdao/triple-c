//! Lifecycle for the **model gateway** container — a pinned LiteLLM proxy that
//! Triple-C runs as a sibling of the project containers.
//!
//! Shape mirrors `docker::stt`: an image that is either pulled from a registry
//! or built locally from an embedded Dockerfile, a fixed container name, a
//! named volume, and `get_* / ensure_*_running / stop_* / pull_* / build_*`.
//!
//! Two things differ from STT, both deliberate:
//!
//! * **The published host address is *detected*, not fixed.** STT is consumed
//!   by the Tauri host process, so loopback is always enough. The gateway is
//!   consumed by *project containers*, and how a container reaches the host
//!   depends on the engine — so the bind address does too. See
//!   [`GatewayBinding`]. It is never `0.0.0.0`: the config behind this port
//!   holds a billed provider key, and Docker's published-port rules land in the
//!   `DOCKER` iptables chain *ahead* of a host firewall, so a wildcard bind is
//!   genuinely LAN-reachable even with `ufw` enabled.
//! * **The rendered config is uploaded into the container over the Docker
//!   API** rather than passed as env. It holds the provider API key, and both
//!   env vars and labels are readable by anything on the host via
//!   `docker inspect`.

use bollard::container::{
    Config, CreateContainerOptions, ListContainersOptions,
    StartContainerOptions, StopContainerOptions, UploadToContainerOptions,
};
use bollard::image::BuildImageOptions;
use bollard::models::{HostConfig, Mount, MountTypeEnum, PortBinding};
use bollard::network::InspectNetworkOptions;
use bollard::Docker;
use futures_util::StreamExt;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::io::Write;
use std::sync::OnceLock;
use tokio::sync::{Mutex, OnceCell};

use super::client::get_docker;
use crate::models::gateway_settings::{GatewaySettings, GatewayStatus};
use crate::storage::secure;

const GATEWAY_CONTAINER_NAME: &str = "triple-c-gateway";
const GATEWAY_CONFIG_VOLUME: &str = "triple-c-gateway-config";

/// Upstream LiteLLM, pinned to an exact release.
///
/// LiteLLM 1.82.7 and 1.82.8 shipped credential-harvesting malware on PyPI, so
/// nothing here may float a tag or resolve `litellm` at build time. v1.106.6 is
/// also above the floors set by the proxy auth-bypass, SSRF and salt-key CVEs —
/// see the long comment in `gateway-container/Dockerfile`, and keep the two in
/// lockstep.
const GATEWAY_REGISTRY_REPO: &str = "ghcr.io/berriai/litellm";
const GATEWAY_LITELLM_VERSION: &str = "v1.106.6";

/// The digest [`GATEWAY_REGISTRY_REPO`]:[`GATEWAY_LITELLM_VERSION`] is pinned to — the same one the
/// Dockerfile's `FROM` uses. Pulls and lookups go by digest: a tag can be
/// re-pushed upstream, a digest cannot.
const GATEWAY_REGISTRY_DIGEST: &str =
    "sha256:37e2dafb088a7723beb998590682957386dfc83ffea1848486f8e0e5cfac0b06";

fn registry_image_ref() -> String {
    format!("{}@{}", GATEWAY_REGISTRY_REPO, GATEWAY_REGISTRY_DIGEST)
}

/// Local builds are tagged with the pin plus a hash of the embedded build
/// context (`triple-c-gateway:v1.106.6-1a2b3c4d`), never a floating tag. A
/// build made by an app version with a different pin *or* a different
/// Dockerfile/config then simply doesn't match, instead of quietly winning
/// over the current image the way the old `triple-c-gateway:latest` tag did.
/// Builds tagged `latest` by earlier app versions are no longer used.
const GATEWAY_LOCAL_REPO: &str = "triple-c-gateway";

/// The LiteLLM release this app is pinned to — the tag of
/// [`GATEWAY_REGISTRY_REPO`]:[`GATEWAY_LITELLM_VERSION`], e.g. `v1.106.6`.
fn pinned_litellm_version() -> &'static str {
    GATEWAY_LITELLM_VERSION
}

fn local_image_ref() -> String {
    static REF: OnceLock<String> = OnceLock::new();
    REF.get_or_init(|| {
        // config.yaml is hashed whole: a `#` line can sit inside a block
        // scalar there, and it is rarely edited.
        let context = sha256_hex(&format!(
            "{}\0{}",
            significant_lines(GATEWAY_DOCKERFILE),
            GATEWAY_DEFAULT_CONFIG
        ));
        format!(
            "{}:{}-{}",
            GATEWAY_LOCAL_REPO,
            pinned_litellm_version(),
            &context[..8]
        )
    })
    .clone()
}

/// The Dockerfile minus full-line comments and blank lines, so rewording a
/// comment in a later release doesn't make every local build look outdated.
/// Parser directives (`# syntax=`, `# escape=`, `# check=`) look like comments
/// but change the build, so they are kept.
fn significant_lines(text: &str) -> String {
    text.lines()
        .map(str::trim_end)
        .filter(|l| {
            let t = l.trim_start();
            if t.is_empty() {
                return false;
            }
            match t.strip_prefix('#') {
                None => true,
                Some(rest) => rest
                    .split_once('=')
                    .map(|(key, _)| key.trim())
                    .is_some_and(|key| matches!(key, "syntax" | "escape" | "check")),
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The image the gateway container should run, and its content id. The id goes
/// into the container fingerprint so that a newly pulled or rebuilt image
/// recreates the container instead of leaving the old one running.
#[derive(Debug, Clone, PartialEq)]
struct ChosenImage {
    reference: String,
    id: String,
}

/// Local build first (same precedence as the STT container), then the pinned
/// upstream image. Both references name the pin, so either is current.
fn choose_gateway_image(local_id: Option<String>, registry_id: Option<String>) -> Option<ChosenImage> {
    local_id
        .map(|id| ChosenImage {
            reference: local_image_ref(),
            id,
        })
        .or_else(|| {
            registry_id.map(|id| ChosenImage {
                reference: registry_image_ref(),
                id,
            })
        })
}

/// The existing gateway container, as far as `ensure` and status need it.
#[derive(Debug, Clone)]
struct GatewayContainer {
    id: String,
    state: String,
    /// The config-fingerprint label it was created with.
    fingerprint: String,
    image_id: Option<String>,
}

/// Ids of the locally present images that carry the current pin.
#[derive(Debug, Clone, Default, PartialEq)]
struct PinnedImages {
    local: Option<String>,
    registry: Option<String>,
}

impl PinnedImages {
    fn chosen(&self) -> Option<ChosenImage> {
        choose_gateway_image(self.local.clone(), self.registry.clone())
    }
}

/// A failed lookup of one image doesn't hide the other. An error is returned
/// only when it leaves the answer unknown — so "Docker is unreachable" is not
/// mistaken for "image missing".
async fn pinned_images() -> Result<PinnedImages, String> {
    let (local_ref, registry_ref) = (local_image_ref(), registry_image_ref());
    let (local, registry) = tokio::join!(
        super::migration::image_id(&local_ref),
        super::migration::image_id(&registry_ref)
    );
    match (local, registry) {
        (Ok(local), Ok(registry)) => Ok(PinnedImages { local, registry }),
        // The local build is preferred, so it alone decides when present.
        (Ok(Some(local)), Err(e)) => {
            log::warn!("{}", e);
            Ok(PinnedImages {
                local: Some(local),
                registry: None,
            })
        }
        // But a failed lookup of the preferred local build leaves the answer
        // unknown even when the registry image is there: choosing the
        // registry image would swap a working local-build gateway over.
        (Err(e), _) | (_, Err(e)) => Err(e),
    }
}

/// Whether the container should be moved onto `pinned` — the image half of
/// [`needs_recreate`]. A container whose image id is unknown counts as not on
/// it, in both places.
fn image_switch_pending(existing_image_id: Option<&str>, pinned: &ChosenImage) -> bool {
    existing_image_id != Some(pinned.id.as_str())
}

/// Status's `image_outdated`: the container exists and isn't on a pinned
/// image. With a pinned image present this is exactly [`image_switch_pending`]
/// — what the next start or settings save acts on. With none (`Some(None)`) it
/// is outdated, but `ensure` keeps it as is until one is pulled or built.
/// `None` means the image check failed: unknown is reported as not outdated
/// rather than guessed.
fn container_image_outdated(
    container_exists: bool,
    container_image_id: Option<&str>,
    chosen: Option<Option<&ChosenImage>>,
) -> bool {
    match chosen {
        _ if !container_exists => false,
        Some(Some(pinned)) => image_switch_pending(container_image_id, pinned),
        Some(None) => true,
        None => false,
    }
}

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

/// The default bridge gateway address on a stock native-Linux engine. Only a
/// fallback: the real value is read from the `bridge` network's IPAM config.
const DEFAULT_BRIDGE_GATEWAY: &str = "172.17.0.1";

/// Where the gateway's published port is bound on the host, and the address a
/// *project container* uses to reach it.
///
/// Project containers run on Docker's default bridge with no user-defined
/// network and no `--add-host`, so the only address they share with the gateway
/// is the host itself — but *which* host address works is engine-specific, and
/// the whole point of this type is that the two answers are derived together so
/// they cannot drift apart:
///
/// * **Docker Desktop** (macOS / Windows / WSL2) resolves `host.docker.internal`
///   from inside containers automatically, and its port forwarder reaches the
///   host's *loopback*. So: bind `127.0.0.1`, hand out `host.docker.internal`.
/// * **Native Linux Docker** injects no `host.docker.internal`, and the address
///   containers share with the host is the default bridge gateway (normally
///   `172.17.0.1`). So: bind that address, and hand out the same literal.
///
/// Neither case binds `0.0.0.0`. The bridge-gateway bind is reachable from
/// every container on the default bridge — which is the requirement — without
/// publishing a key-bearing proxy to the LAN.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatewayBinding {
    /// Host address the published port is bound to (`HostIp`).
    pub host_ip: String,
    /// Host address a project container should dial.
    pub container_host: String,
}

impl GatewayBinding {
    fn desktop() -> Self {
        Self {
            host_ip: "127.0.0.1".to_string(),
            container_host: "host.docker.internal".to_string(),
        }
    }

    fn bridge(gateway_ip: &str) -> Self {
        Self {
            host_ip: gateway_ip.to_string(),
            container_host: gateway_ip.to_string(),
        }
    }

    /// The value a project should use as its base URL (`ANTHROPIC_BASE_URL`).
    pub fn base_url(&self, port: u16) -> String {
        format!("http://{}:{}", self.container_host, port)
    }

    /// The address the *host* process (health checks) should dial.
    fn host_url(&self, port: u16) -> String {
        format!("http://{}:{}", self.host_ip, port)
    }
}

/// Decide the binding from what the daemon reports. Pure, so the engine-shape
/// matrix is testable without a daemon.
fn binding_for(operating_system: &str, bridge_gateway: Option<&str>) -> GatewayBinding {
    // Docker Desktop reports exactly "Docker Desktop" here on every platform it
    // ships for; matched loosely so a future suffix doesn't silently flip us
    // onto the bridge path.
    if operating_system.to_ascii_lowercase().contains("docker desktop") {
        return GatewayBinding::desktop();
    }
    GatewayBinding::bridge(
        bridge_gateway
            .map(str::trim)
            .filter(|g| !g.is_empty())
            .unwrap_or(DEFAULT_BRIDGE_GATEWAY),
    )
}

/// Detection is one `info` + one `inspect_network` per process; the answer
/// cannot change without the engine being replaced under us.
static GATEWAY_BINDING: OnceCell<GatewayBinding> = OnceCell::const_new();

/// The gateway's host binding, detected once and cached.
///
/// When Docker is unreachable the *loopback* answer is returned without being
/// cached: it is the conservative one (nothing is published anywhere yet, and
/// the only caller in that state is status reporting), and the next call
/// re-detects once the daemon is up.
pub async fn gateway_binding() -> GatewayBinding {
    if let Some(binding) = GATEWAY_BINDING.get() {
        return binding.clone();
    }
    match detect_binding().await {
        Ok(binding) => {
            let _ = GATEWAY_BINDING.set(binding.clone());
            binding
        }
        Err(e) => {
            log::debug!("Gateway bind detection deferred ({}), assuming loopback", e);
            GatewayBinding::desktop()
        }
    }
}

async fn detect_binding() -> Result<GatewayBinding, String> {
    let docker = get_docker()?;
    let info = docker
        .info()
        .await
        .map_err(|e| format!("Failed to query the Docker daemon: {}", e))?;
    let operating_system = info.operating_system.unwrap_or_default();
    let gateway_ip = bridge_gateway_ip(&docker).await;
    let binding = binding_for(&operating_system, gateway_ip.as_deref());
    log::info!(
        "Model gateway will publish on {} (engine OS: {})",
        binding.host_ip,
        if operating_system.is_empty() {
            "unknown"
        } else {
            &operating_system
        }
    );
    Ok(binding)
}

/// The default bridge's gateway address, straight from its IPAM config, so a
/// host whose bridge subnet was customised still gets a reachable bind.
async fn bridge_gateway_ip(docker: &Docker) -> Option<String> {
    let network = docker
        .inspect_network("bridge", None::<InspectNetworkOptions<String>>)
        .await
        .ok()?;
    network
        .ipam?
        .config?
        .into_iter()
        .find_map(|c| c.gateway.filter(|g| !g.trim().is_empty()))
}

/// Config and secret version only — the same format earlier versions wrote,
/// so updating the app doesn't by itself recreate every gateway. Image changes
/// are handled separately by [`needs_recreate`].
fn gateway_fingerprint(config_shape: &str, secret_version: &str) -> String {
    sha256_hex(&format!("{}|{}", config_shape, secret_version))
}

/// The one rule for replacing an existing container: its config changed, or a
/// pinned image is present and the container runs something else. With no
/// pinned image (`pinned: None`) the container keeps its current image.
/// [`container_image_outdated`] reports the image half of this to the UI.
fn needs_recreate(
    existing_fingerprint: &str,
    fingerprint: &str,
    existing_image_id: Option<&str>,
    pinned: Option<&ChosenImage>,
) -> bool {
    existing_fingerprint != fingerprint
        || pinned.is_some_and(|p| image_switch_pending(existing_image_id, p))
}

fn sha256_hex(input: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(input.as_bytes());
    format!("{:x}", hasher.finalize())
}

pub async fn get_gateway_status(settings: &GatewaySettings) -> Result<GatewayStatus, String> {
    let (pinned, container) = tokio::join!(pinned_images(), find_gateway_container());
    let chosen = match pinned {
        Ok(p) => Some(p.chosen()),
        Err(e) => {
            log::warn!("Gateway image check failed: {}", e);
            None
        }
    };
    let image_exists = matches!(chosen, Some(Some(_)));
    let container = container?;
    let (container_exists, running) = match &container {
        Some(c) => (true, c.state == "running"),
        None => (false, false),
    };
    let image_outdated = container_image_outdated(
        container_exists,
        container.as_ref().and_then(|c| c.image_id.as_deref()),
        chosen.as_ref().map(|c| c.as_ref()),
    );

    Ok(GatewayStatus {
        container_exists,
        running,
        port: settings.port,
        image_exists,
        image_outdated,
        model_count: settings.valid_models().len(),
        has_api_key: secure::has_gateway_api_key(),
        base_url: gateway_binding().await.base_url(settings.port),
    })
}

/// Whether a gateway container exists, and whether it is running. Used by the
/// settings reconcile, which must not start anything the user never started.
pub async fn gateway_container_presence() -> Result<(bool, bool), String> {
    Ok(match find_gateway_container().await? {
        Some(c) => (true, c.state == "running"),
        None => (false, false),
    })
}

/// Whether a container summary's names contain *exactly* our container.
///
/// Docker's `name` filter is an unanchored regex, so listing with it also
/// returns `triple-c-gateway-backup`, `my-triple-c-gateway`, and anything else
/// containing the string. Taking `.first()` of that would let this module
/// adopt — and then force-remove — a container it does not own.
/// `container::find_existing_container` matches exactly for the same reason.
fn is_gateway_container(names: Option<&Vec<String>>) -> bool {
    let expected = format!("/{}", GATEWAY_CONTAINER_NAME);
    names.is_some_and(|names| names.iter().any(|n| n == &expected))
}

/// The gateway container, if any.
async fn find_gateway_container() -> Result<Option<GatewayContainer>, String> {
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

    // The filter is a prefilter only — the exact-name check is what decides.
    for container in &containers {
        if !is_gateway_container(container.names.as_ref()) {
            continue;
        }
        let id = container.id.clone().unwrap_or_default();
        let state = container.state.clone().unwrap_or_default();
        let fingerprint = container
            .labels
            .as_ref()
            .and_then(|l| l.get(CONFIG_FINGERPRINT_LABEL))
            .cloned()
            .unwrap_or_default();

        let image_id = container.image_id.clone().filter(|s| !s.is_empty());

        return Ok(Some(GatewayContainer {
            id,
            state,
            fingerprint,
            image_id,
        }));
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
fn config_shape(settings: &GatewaySettings, binding: &GatewayBinding) -> String {
    let models: Vec<String> = settings
        .valid_models()
        .iter()
        .map(|m| format!("{}={}", m.name.trim(), m.model_id.trim()))
        .collect();
    // `bind` is part of the shape so that moving between engines (or a bridge
    // subnet change) recreates the container instead of leaving it published on
    // an address the new environment doesn't use.
    format!(
        "provider={};api_base={};port={};bind={};models={}",
        settings.provider.trim(),
        settings.api_base.as_deref().unwrap_or("").trim(),
        settings.port,
        binding.host_ip,
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
    binding: &GatewayBinding,
    image: &ChosenImage,
    fingerprint: &str,
) -> Result<String, String> {
    let docker = get_docker()?;

    let mut port_bindings = HashMap::new();
    port_bindings.insert(
        format!("{}/tcp", GATEWAY_INTERNAL_PORT),
        Some(vec![PortBinding {
            // Never `0.0.0.0`: the narrowest host address project containers
            // can still reach. See `GatewayBinding`.
            host_ip: Some(binding.host_ip.clone()),
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
    labels.insert("triple-c.gateway.bind".to_string(), binding.host_ip.clone());
    labels.insert(
        "triple-c.gateway.provider".to_string(),
        settings.provider.trim().to_string(),
    );

    let config = Config {
        // The resolved id, not the tag: a build or pull retagging the image
        // mid-create can't make the container disagree with its fingerprint.
        image: Some(image.id.clone()),
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

/// Stops a running container gracefully first, with Docker's default grace as
/// before (so in-flight LiteLLM requests can drain; the shorter
/// [`GATEWAY_STOP_GRACE_SECS`] is for app exit), then removes it, tolerating
/// one already gone.
async fn remove_gateway_container(docker: &Docker, id: &str, state: &str) -> Result<(), String> {
    if state == "running" {
        docker
            .stop_container(id, None::<StopContainerOptions>)
            .await
            .map_err(|e| format!("Failed to stop gateway container: {}", e))?;
    }
    super::container::remove_container(id).await
}

/// Serialises every mutation of the single fixed-name gateway container.
///
/// `ensure_gateway_running` is check-then-act over one container name, so two
/// concurrent callers — the setup auto-start and the user's Start button is the
/// realistic pair — would both see `None` and both try to create it, and the
/// loser would surface a raw Docker 409. Migration guards the same shape with
/// `ActiveGuard`; here the right behaviour is to *serialise* rather than
/// refuse, because the second caller then observes the first's container, finds
/// a matching fingerprint, and returns its status — which is exactly what it
/// asked for.
fn gateway_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

pub async fn ensure_gateway_running(settings: &GatewaySettings) -> Result<GatewayStatus, String> {
    let _guard = gateway_lock().lock().await;
    ensure_gateway_running_locked(settings).await
}

async fn ensure_gateway_running_locked(
    settings: &GatewaySettings,
) -> Result<GatewayStatus, String> {
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

    let binding = gateway_binding().await;

    // Rotation id, not a hash of either secret — see `storage::secure`.
    let secret_version = secure::get_gateway_secret_version()?.unwrap_or_default();
    let (existing, pinned) = tokio::join!(find_gateway_container(), pinned_images());
    let existing = existing?;
    // A failed image lookup must not fail a gateway that's fine as it is: it
    // takes the same path as "no pinned image" and keeps the current image.
    let mut lookup_error = None;
    let chosen = match pinned {
        Ok(pinned) => pinned.chosen(),
        Err(e) => {
            log::warn!("Gateway image check failed: {}", e);
            lookup_error = Some(e);
            None
        }
    };
    let on_pin = chosen.is_some();
    let image = match chosen {
        Some(image) => image,
        // No pinned image yet (after an app update moved the pin, until the
        // user pulls or builds). Never tear down a working gateway over that:
        // keep it on the image it already runs — recreated with the new
        // config if that changed — and let status report it as outdated.
        None => match existing.as_ref().and_then(|c| c.image_id.clone()) {
            Some(id) => {
                log::warn!(
                    "Gateway image for LiteLLM {} not present; keeping the gateway on its current image {}",
                    pinned_litellm_version(),
                    id
                );
                ChosenImage {
                    reference: id.clone(),
                    id,
                }
            }
            None => {
                return Err(match lookup_error {
                    Some(e) => e,
                    None => format!(
                        "Gateway image for LiteLLM {} not found. Please pull or build the image first.",
                        pinned_litellm_version()
                    ),
                })
            }
        },
    };

    let fingerprint = gateway_fingerprint(&config_shape(settings, &binding), &secret_version);

    if let Some(GatewayContainer {
        id,
        state,
        fingerprint: existing_fingerprint,
        image_id: existing_image_id,
    }) = existing
    {
        let pinned = on_pin.then_some(&image);
        if !needs_recreate(
            &existing_fingerprint,
            &fingerprint,
            existing_image_id.as_deref(),
            pinned,
        ) {
            if state == "running" {
                return get_gateway_status(settings).await;
            }
            docker
                .start_container(&id, None::<StartContainerOptions<String>>)
                .await
                .map_err(|e| format!("Failed to start gateway container: {}", e))?;
            return get_gateway_status(settings).await;
        }

        // Config, a secret or the image changed — recreate so the new config
        // is uploaded and the pinned image is used.
        remove_gateway_container(&docker, &id, &state).await?;
    }

    let id = create_gateway_container(settings, &binding, &image, &fingerprint).await?;

    // Upload before the first start: LiteLLM reads the config once at boot.
    let rendered = render_config(settings, &api_key, &master_key);
    if let Err(e) = upload_config(&id, &rendered).await {
        // Don't leave a half-configured container behind for the next run to
        // mistake for a good one.
        if let Err(cleanup) = super::container::remove_container(&id).await {
            log::warn!("{}", cleanup);
        }
        return Err(e);
    }

    docker
        .start_container(&id, None::<StartContainerOptions<String>>)
        .await
        .map_err(|e| format!("Failed to start gateway container: {}", e))?;

    log::info!(
        "Model gateway started on {}:{} ({} model(s), {})",
        binding.host_ip,
        settings.port,
        settings.valid_models().len(),
        image.reference
    );
    get_gateway_status(settings).await
}

/// Grace period given to LiteLLM on stop. The Docker default is 10s, which app
/// exit cannot afford to spend on a proxy that holds no state worth flushing.
const GATEWAY_STOP_GRACE_SECS: i64 = 3;

pub async fn stop_gateway_container() -> Result<(), String> {
    // Same lock as `ensure_gateway_running`, so a stop can't interleave with a
    // create/start and leave a container running behind a "stopped" return.
    let _guard = gateway_lock().lock().await;
    let docker = get_docker()?;

    if let Some(GatewayContainer { id, state, .. }) = find_gateway_container().await? {
        if state == "running" {
            docker
                .stop_container(
                    &id,
                    Some(StopContainerOptions {
                        t: GATEWAY_STOP_GRACE_SECS,
                    }),
                )
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

    // Dial whatever the container is actually published on — with a
    // bridge-gateway bind, the host's loopback answers nothing.
    let base = gateway_binding().await.host_url(port);

    match client
        .get(format!("{}/health/liveliness", base))
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
    super::image::pull_image(&registry_image_ref(), on_progress).await?;
    // A pull by digest leaves the image untagged, which `docker image prune`
    // treats as dangling and deletes. Tag it locally; lookups still go by
    // digest, so a re-pushed upstream tag changes nothing.
    // Not fatal: the image is present and will be used either way.
    if let Err(e) = get_docker()?
        .tag_image(
            &registry_image_ref(),
            Some(bollard::image::TagImageOptions {
                repo: GATEWAY_REGISTRY_REPO,
                tag: GATEWAY_LITELLM_VERSION,
            }),
        )
        .await
    {
        log::warn!(
            "Pulled the gateway image but could not tag it (a prune may remove it): {}",
            e
        );
    }
    Ok(())
}

pub async fn build_gateway_image<F>(on_progress: F) -> Result<(), String>
where
    F: Fn(String) + Send + 'static,
{
    let docker = get_docker()?;

    let tar_bytes = create_gateway_build_context()
        .map_err(|e| format!("Failed to create gateway build context: {}", e))?;

    let tag = local_image_ref();
    let options = BuildImageOptions {
        t: tag.as_str(),
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

    fn chosen(reference: &str, id: &str) -> ChosenImage {
        ChosenImage {
            reference: reference.to_string(),
            id: id.to_string(),
        }
    }

    #[test]
    fn local_builds_are_tagged_with_the_pin_and_context() {
        let v = pinned_litellm_version();
        assert!(v.starts_with('v') && !v.contains(['/', '@', ':']), "bad pin tag {v:?}");
        let tag = local_image_ref();
        let suffix = tag
            .strip_prefix(&format!("triple-c-gateway:{}-", v))
            .expect("tag is repo:pin-hash");
        assert_eq!(suffix.len(), 8);
        assert!(suffix.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn registry_lookups_go_by_digest() {
        assert_eq!(
            registry_image_ref(),
            format!("ghcr.io/berriai/litellm@{}", GATEWAY_REGISTRY_DIGEST)
        );
    }

    #[test]
    fn dockerfile_from_matches_the_registry_pin() {
        // Local builds are tagged with the pin; that is only truthful while the
        // Dockerfile builds FROM the same tag.
        let from = GATEWAY_DOCKERFILE
            .lines()
            .find(|l| l.starts_with("FROM "))
            .expect("Dockerfile has a FROM line");
        let expected = format!(
            "FROM {}:{}@{}",
            GATEWAY_REGISTRY_REPO, GATEWAY_LITELLM_VERSION, GATEWAY_REGISTRY_DIGEST
        );
        assert_eq!(from.trim(), expected, "gateway Dockerfile FROM is out of lockstep with the pin");
    }

    #[test]
    fn local_build_wins_then_registry_then_nothing() {
        assert_eq!(
            choose_gateway_image(Some("sha256:local".into()), Some("sha256:reg".into())),
            Some(chosen(&local_image_ref(), "sha256:local"))
        );
        assert_eq!(
            choose_gateway_image(None, Some("sha256:reg".into())),
            Some(chosen(&registry_image_ref(), "sha256:reg"))
        );
        assert_eq!(choose_gateway_image(None, None), None);
    }

    #[test]
    fn outdated_matches_what_ensure_would_recreate() {
        let pinned = chosen(&registry_image_ref(), "sha256:new");
        assert!(!container_image_outdated(true, Some("sha256:new"), Some(Some(&pinned))));
        assert!(container_image_outdated(true, Some("sha256:old"), Some(Some(&pinned))));
        // No pinned image at all: whatever runs is not it.
        assert!(container_image_outdated(true, Some("sha256:old"), Some(None)));
        // No container, or the check failed: don't claim it.
        assert!(!container_image_outdated(false, None, Some(Some(&pinned))));
        assert!(!container_image_outdated(true, Some("sha256:old"), None));
        // A container whose image id is unknown: status and ensure agree
        // that it isn't on the pinned image.
        assert!(container_image_outdated(true, None, Some(Some(&pinned))));
        assert!(needs_recreate("fp", "fp", None, Some(&pinned)));
    }

    #[test]
    fn comment_edits_dont_change_the_local_build_tag() {
        let a = "# about\nFROM x\n\n  # indented note\nRUN y\n";
        let b = "# reworded\nFROM x\nRUN y\n# trailing\n";
        assert_eq!(significant_lines(a), significant_lines(b));
        assert_ne!(significant_lines(a), significant_lines("FROM x\nRUN z\n"));
        // A parser directive changes the build.
        assert_ne!(
            significant_lines(b),
            significant_lines(&format!("# syntax=docker/dockerfile:1.7\n{b}"))
        );
    }

    #[test]
    fn fingerprint_format_is_unchanged_from_earlier_versions() {
        // What main wrote: sha256 of "shape|secret". Keeping it means the app
        // update alone recreates nothing.
        assert_eq!(gateway_fingerprint("shape", "1"), sha256_hex("shape|1"));
    }

    #[test]
    fn recreate_on_config_change_or_a_present_new_pinned_image() {
        let pinned = chosen(&registry_image_ref(), "sha256:new");
        // Unchanged config, already on the pinned image: keep.
        assert!(!needs_recreate("fp", "fp", Some("sha256:new"), Some(&pinned)));
        // Pinned image present, container on another: recreate.
        assert!(needs_recreate("fp", "fp", Some("sha256:old"), Some(&pinned)));
        // No pinned image: keep the working gateway on its image.
        assert!(!needs_recreate("fp", "fp", Some("sha256:old"), None));
        // Config changed: always recreate.
        assert!(needs_recreate("old", "fp", Some("sha256:new"), Some(&pinned)));
        assert!(needs_recreate("old", "fp", Some("sha256:old"), None));
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
        let binding = GatewayBinding::desktop();
        let a = config_shape(&settings(), &binding);
        let mut s = settings();
        s.models[0].model_id = "gpt-4.1".to_string();
        assert_ne!(a, config_shape(&s, &binding));
        assert!(!a.contains("sk-"));
    }

    #[test]
    fn config_shape_tracks_the_bind_address() {
        // Moving between engines must recreate the container rather than leave
        // it published on an address the new environment doesn't use.
        let s = settings();
        assert_ne!(
            config_shape(&s, &GatewayBinding::desktop()),
            config_shape(&s, &GatewayBinding::bridge("172.17.0.1"))
        );
    }

    #[test]
    fn docker_desktop_binds_loopback_and_hands_out_host_docker_internal() {
        let binding = binding_for("Docker Desktop", None);
        assert_eq!(binding.host_ip, "127.0.0.1");
        assert_eq!(binding.base_url(4000), "http://host.docker.internal:4000");
        // Detection must not depend on the bridge answer on this engine.
        assert_eq!(binding, binding_for("Docker Desktop", Some("172.17.0.1")));
    }

    #[test]
    fn native_linux_binds_the_bridge_gateway_it_reports() {
        // A project container can't reach the host's loopback here, but it can
        // reach the bridge gateway — and so can nothing on the LAN.
        let binding = binding_for("Ubuntu 24.04.1 LTS", Some("172.19.0.1"));
        assert_eq!(binding.host_ip, "172.19.0.1");
        assert_eq!(binding.base_url(4000), "http://172.19.0.1:4000");
        assert_eq!(binding.host_url(4000), "http://172.19.0.1:4000");
    }

    #[test]
    fn a_missing_bridge_answer_falls_back_to_the_documented_default() {
        for reported in [None, Some(""), Some("   ")] {
            assert_eq!(
                binding_for("Ubuntu 24.04.1 LTS", reported).host_ip,
                "172.17.0.1"
            );
        }
    }

    #[test]
    fn no_engine_shape_ever_binds_a_wildcard_address() {
        // The regression this guards: the published port fronts a container
        // config holding a billed provider key, and Docker's rules sit ahead of
        // the host firewall.
        for os in ["Docker Desktop", "Ubuntu 24.04.1 LTS", "", "Rancher Desktop"] {
            for gw in [None, Some("172.17.0.1"), Some("10.0.0.1")] {
                let host_ip = binding_for(os, gw).host_ip;
                assert_ne!(host_ip, "0.0.0.0", "os={:?} gw={:?}", os, gw);
                assert_ne!(host_ip, "::", "os={:?} gw={:?}", os, gw);
                assert!(!host_ip.is_empty());
            }
        }
    }

    #[test]
    fn only_the_exact_container_name_is_adopted() {
        // Docker's `name` filter is an unanchored regex: all of these come back
        // from a filtered list. Adopting one would force-remove a user's
        // container.
        assert!(is_gateway_container(Some(&vec![
            "/triple-c-gateway".to_string()
        ])));
        assert!(is_gateway_container(Some(&vec![
            "/something-else".to_string(),
            "/triple-c-gateway".to_string(),
        ])));
        for impostor in [
            "/triple-c-gateway-backup",
            "/my-triple-c-gateway",
            "/triple-c-gateway2",
            "triple-c-gateway",
        ] {
            assert!(
                !is_gateway_container(Some(&vec![impostor.to_string()])),
                "{} must not be adopted",
                impostor
            );
        }
        assert!(!is_gateway_container(None));
        assert!(!is_gateway_container(Some(&vec![])));
    }

    #[tokio::test]
    async fn the_gateway_lock_serialises_concurrent_callers() {
        // The auto-start racing the Start button: both would otherwise see no
        // container and both create one, and the loser gets a Docker 409.
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;

        let inside = Arc::new(AtomicUsize::new(0));
        let overlaps = Arc::new(AtomicUsize::new(0));

        let mut tasks = Vec::new();
        for _ in 0..8 {
            let inside = inside.clone();
            let overlaps = overlaps.clone();
            tasks.push(tokio::spawn(async move {
                let _guard = gateway_lock().lock().await;
                if inside.fetch_add(1, Ordering::SeqCst) != 0 {
                    overlaps.fetch_add(1, Ordering::SeqCst);
                }
                tokio::task::yield_now().await;
                tokio::time::sleep(std::time::Duration::from_millis(1)).await;
                inside.fetch_sub(1, Ordering::SeqCst);
            }));
        }
        for t in tasks {
            t.await.unwrap();
        }

        assert_eq!(overlaps.load(Ordering::SeqCst), 0);
        assert_eq!(inside.load(Ordering::SeqCst), 0);
    }
}
