use std::collections::HashMap;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EnvVar {
    pub key: String,
    pub value: String,
}

/// Whether `key` is a name a shell will read back as an ordinary variable:
/// `[A-Za-z_][A-Za-z0-9_]*`.
///
/// ## Why a charset rule, and not just the reserved-name list
///
/// `docker::container::is_reserved_env_key` answers a different question — "is
/// this one of the names Triple-C manages itself" — and nothing anywhere asked
/// what the *characters* were. A key is joined into `KEY=VALUE` and handed to
/// the daemon, which puts it in the container's environment verbatim, so a name
/// that is not an identifier travels through unchallenged.
///
/// The one that matters is `BASH_FUNC_name%%`, bash's wire format for an
/// exported shell function: bash imports those at startup and the *body* is the
/// value. Today that is latent rather than live — the image's `/bin/sh` is
/// dash, which does not import them, and an auditor confirmed the vector fires
/// under `bash -c` and not under `sh -c` in the shipped image. But the
/// pre-commit scrub runs `/bin/sh -c` **as root**, `/bin/sh` is whatever
/// `ubuntu:24.04` points it at, and nothing pins that. One base-image change,
/// or one call site spelled `bash`, turns a stored project setting into root
/// code execution inside the container at commit time.
///
/// So the rule is the shape of the thing rather than a list of the names that
/// are known to be dangerous: `IFS`, `LD_PRELOAD` and `PATH` are all perfectly
/// good identifiers and are the user's business, while nothing legitimate needs
/// a `%`, a `(` or a space in an environment variable name.
///
/// The key is judged **trimmed**, because that is what `create_container` sends
/// — ` FOO ` already reaches the container as `FOO`, and refusing it here would
/// break a setting that works.
pub fn is_valid_env_key(key: &str) -> bool {
    let mut chars = key.trim().chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Validate a custom environment variable list that is about to be stored,
/// admitting the entries it is already stored with.
///
/// Same shape, and the same reasoning, as
/// `commands::project_commands::validate_project_paths_update`: nothing ever
/// checked these keys, so `projects.json` and `settings.json` in the field can
/// hold whatever was typed. Holding every save to the new rule would make such
/// a project unsavable *entirely* — `update_project` is the single command
/// behind the whole Config tab — and would buy nothing, because the stored key
/// is already being handed to every container that starts. An entry carried
/// over verbatim is admitted; a new or edited one is held to the rule, which is
/// what keeps the escalation closed, since escalation means *introducing* a bad
/// key through this command.
///
/// Counted rather than set-tested, for the same reason as the folder rows: a
/// second copy of an existing entry is a new entry.
///
/// The blank entry is not a violation. "+ Add variable" appends
/// `{key: "", value: ""}` and saves the list immediately, so refusing it would
/// turn the button itself into an error toast; `create_container` skips an
/// empty key, so it reaches nothing.
pub fn validate_env_vars_update(stored: &[EnvVar], incoming: &[EnvVar]) -> Result<(), String> {
    // An entry with no key is the placeholder, whatever is in its value:
    // `create_container` skips it, so it reaches nothing and there is nothing
    // to refuse. The editor saves on every blur, and typing the value before
    // the name is an ordinary way to fill a row in.
    let is_blank = |v: &EnvVar| v.key.trim().is_empty();

    let mut carried: std::collections::HashMap<(&str, &str), usize> =
        std::collections::HashMap::new();
    for v in stored.iter().filter(|v| !is_blank(v)) {
        *carried
            .entry((v.key.as_str(), v.value.as_str()))
            .or_insert(0) += 1;
    }

    for v in incoming.iter().filter(|v| !is_blank(v)) {
        match carried.get_mut(&(v.key.as_str(), v.value.as_str())) {
            Some(remaining) if *remaining > 0 => {
                *remaining -= 1;
            }
            _ => {
                if !is_valid_env_key(&v.key) {
                    return Err(format!(
                        "'{}' is not a usable environment variable name. Use a letter or \
                         underscore followed by letters, digits or underscores.",
                        v.key
                    ));
                }
            }
        }
    }

    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProjectPath {
    pub host_path: String,
    pub mount_name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PortMapping {
    pub host_port: u16,
    pub container_port: u16,
    #[serde(default = "default_protocol")]
    pub protocol: String,
}

fn default_protocol() -> String {
    "tcp".to_string()
}

fn default_full_permissions() -> bool {
    true
}

/// `use_shared_auth_token` defaults to **on**: once the user has run
/// `claude setup-token` once, every existing Anthropic-backend project should
/// pick the token up without being edited one by one. Projects deliberately
/// pinned to their own `claude login` identity opt out.
fn default_use_shared_auth_token() -> bool {
    true
}

/// How much autonomy Claude Code is granted inside the container.
///
/// Maps onto Claude Code CLI flags — see [`PermissionMode::cli_args`], which is
/// the single definition of that mapping and must be used by every call site.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub enum PermissionMode {
    /// Read-only planning mode.
    Plan,
    /// Claude Code's own default behavior (prompts for permission).
    #[default]
    Default,
    /// Auto-accept file edits, prompt for everything else.
    AcceptEdits,
    /// Skip all permission prompts.
    Bypass,
}

impl PermissionMode {
    /// The CLI flags this mode adds to a `claude` invocation.
    /// Defined once here so every call site stays in sync.
    pub fn cli_args(&self) -> Vec<String> {
        match self {
            PermissionMode::Plan => vec!["--permission-mode".to_string(), "plan".to_string()],
            PermissionMode::Default => Vec::new(),
            PermissionMode::AcceptEdits => {
                vec!["--permission-mode".to_string(), "acceptEdits".to_string()]
            }
            PermissionMode::Bypass => vec!["--dangerously-skip-permissions".to_string()],
        }
    }

    /// The wire value used for the `TRIPLE_C_PERMISSION_MODE` container env var.
    /// Matches the serde `camelCase` representation.
    pub fn as_env_value(&self) -> &'static str {
        match self {
            PermissionMode::Plan => "plan",
            PermissionMode::Default => "default",
            PermissionMode::AcceptEdits => "acceptEdits",
            PermissionMode::Bypass => "bypass",
        }
    }
}

/// Settings for Claude Code CLI behavior inside the container.
/// These map to Claude Code env vars and ~/.claude/settings.json entries.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(from = "StoredClaudeCodeSettings")]
/// Every field is three-state, and the third state is load-bearing.
///
/// `None` means "not set at this level". For a *project* that is "inherit
/// whatever the global settings say"; for the *global* settings it is "leave
/// Claude Code's own default alone". `Some(false)` is a deliberate off, which
/// is what lets a project turn a globally-enabled setting back off — with a
/// plain `bool` there is no value that can express that, which is why these
/// were widened from `bool`.
pub struct ClaudeCodeSettings {
    /// TUI renderer. `None` leaves settings.json's `tui` key unset, which is
    /// what lets Claude Code pick the renderer itself; `Some("default")` pins
    /// the classic main-screen renderer and `Some("fullscreen")` the alt-screen
    /// one. All three are distinct — "let it choose" is not "classic".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tui_mode: Option<String>,
    /// Saved `/effort` level: `None` = unset, otherwise one of
    /// `"low" | "medium" | "high" | "xhigh"`. Written to settings.json as
    /// `effortLevel` (**not** `effort`, which Claude Code has never read).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    /// Disable auto-scroll in fullscreen TUI mode. Held in the *disabled* sense
    /// because Claude Code's `autoScrollEnabled` defaults to `true`, so the
    /// zero value of this field has to mean "leave it on".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_scroll_disabled: Option<bool>,
    /// Collapse tool output to one-line summaries. Written to settings.json as
    /// `viewMode: "focus"`; there is no `focusMode` key in Claude Code.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub focus_mode: Option<bool>,
    /// Show thinking summaries in responses
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub show_thinking_summaries: Option<bool>,
    /// Turn the session recap **off**.
    ///
    /// Held in the disabled sense for the same reason as `auto_scroll_disabled`,
    /// and the rename from the old `enable_session_recap` is load-bearing rather
    /// than cosmetic. Claude Code's recap is on by default, so the old field was
    /// inverted: switching it on was a no-op and switching it off did nothing at
    /// all. Reusing the name with the opposite meaning would have read every
    /// stored `enable_session_recap: false` — which is what every project that
    /// never touched the control holds — as "the user turned the recap off" and
    /// silently disabled it for all of them. A new name lets the old key be
    /// ignored, which lands every existing project on the correct default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_recap_disabled: Option<bool>,
    /// Strip credentials from subprocess environments
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env_scrub: Option<bool>,
    /// Enable 1-hour prompt cache TTL (vs default 5-minute)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_caching_1h: Option<bool>,
}

/// `ClaudeCodeSettings` in every shape `projects.json` and `settings.json` can
/// be holding, which is what [`ClaudeCodeSettings`] is actually deserialised
/// through.
///
/// ## The upgrade this exists to survive
///
/// Before the widening, the five booleans were plain `bool`s with
/// `#[serde(default)]` and no `skip_serializing_if`, so **every** settings
/// object ever written carries an explicit `"env_scrub": false` — not because
/// anyone chose it, but because that is what a `bool` serialises to. Under the
/// old merge (`if p.x { true } else { g.x }`) that `false` carried no
/// information at all: it was the only value an unset switch could produce, and
/// the global always won.
///
/// Read as `Some(false)` by the new code it becomes a *deliberate off* that
/// beats a global `Some(true)` — so upgrading silently turned five settings off
/// for every project that had ever opened this editor, `env_scrub` ("strip
/// credentials from subprocess environments") among them. There is no store
/// migration anywhere: `projects_store` parses these structs directly.
///
/// ## How an old record is told apart from a new one
///
/// By `enable_session_recap`. It was in the struct from the day it existed and
/// was a plain `bool`, so its key is present in every pre-widening record and
/// in no other — the field was *renamed* to `session_recap_disabled` precisely
/// so the old key could be ignored (see the doc on that field), and the new
/// code has never written it. Its presence is therefore an exact statement that
/// these bytes were written by a binary in which `false` meant "unset", and the
/// booleans are read back that way: `true` is a real choice and survives,
/// `false` becomes `None` and inherits again.
///
/// Nothing marks a *new* record, and nothing needs to: absent is `None` (the
/// fields skip serialising when unset) and a present `false` is the deliberate
/// off the widening was for. That is also what keeps a downgrade survivable —
/// an older binary reads an absent key as `false` through its own
/// `#[serde(default)]`, where a `null` would fail to parse and take the whole
/// of `projects.json` down with it, since `ProjectsStore` parses all-or-nothing
/// and starts empty on an error.
#[derive(Deserialize)]
struct StoredClaudeCodeSettings {
    #[serde(default)]
    tui_mode: Option<String>,
    #[serde(default)]
    effort: Option<String>,
    #[serde(default)]
    auto_scroll_disabled: Option<bool>,
    #[serde(default)]
    focus_mode: Option<bool>,
    #[serde(default)]
    show_thinking_summaries: Option<bool>,
    #[serde(default)]
    session_recap_disabled: Option<bool>,
    #[serde(default)]
    env_scrub: Option<bool>,
    #[serde(default)]
    prompt_caching_1h: Option<bool>,
    /// The pre-widening spelling of `session_recap_disabled`, and the *only*
    /// use of its value: presence dates the record. Its meaning was inverted
    /// and it never worked, so it is read for the marker and discarded.
    #[serde(default)]
    enable_session_recap: Option<bool>,
}

impl From<StoredClaudeCodeSettings> for ClaudeCodeSettings {
    fn from(stored: StoredClaudeCodeSettings) -> Self {
        let pre_widening = stored.enable_session_recap.is_some();
        // On a pre-widening record `false` is what an untouched switch wrote,
        // so it means "not set at this level" and must inherit. A `true` was a
        // real choice either way.
        let read = |v: Option<bool>| if pre_widening { v.filter(|on| *on) } else { v };
        ClaudeCodeSettings {
            tui_mode: stored.tui_mode,
            effort: stored.effort,
            auto_scroll_disabled: read(stored.auto_scroll_disabled),
            focus_mode: read(stored.focus_mode),
            show_thinking_summaries: read(stored.show_thinking_summaries),
            session_recap_disabled: read(stored.session_recap_disabled),
            env_scrub: read(stored.env_scrub),
            prompt_caching_1h: read(stored.prompt_caching_1h),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Project {
    pub id: String,
    pub name: String,
    pub paths: Vec<ProjectPath>,
    pub container_id: Option<String>,
    pub status: ProjectStatus,
    #[serde(alias = "auth_mode")]
    pub backend: Backend,
    pub bedrock_config: Option<BedrockConfig>,
    pub ollama_config: Option<OllamaConfig>,
    #[serde(default, alias = "llama_cpp_config")]
    pub llamacpp_config: Option<LlamaCppConfig>,
    #[serde(alias = "litellm_config")]
    pub openai_compatible_config: Option<OpenAiCompatibleConfig>,
    pub allow_docker_access: bool,
    #[serde(default)]
    pub sandbox_mode_enabled: bool,
    #[serde(default)]
    pub mission_control_enabled: bool,
    /// Opt in to the auth bridge: while the container runs, its loopback
    /// listeners are mirrored onto the host's loopback so browser OAuth
    /// callbacks (`claude login`, `fly login`, `aws sso login`) can reach them.
    /// Purely host-side — it deliberately has no container-recreation label,
    /// because toggling it changes nothing about the container itself.
    #[serde(default)]
    pub auth_bridge_enabled: bool,
    /// Opt in to the browser-view pane, which watches and takes over the
    /// browser Claude drives with Playwright inside the container. Purely
    /// host-side like `auth_bridge_enabled`, so it likewise has no
    /// container-recreation label.
    #[serde(default)]
    pub browser_view_enabled: bool,
    /// Grant the container what a VPN client needs to build a tunnel:
    /// `CAP_NET_ADMIN`, the `/dev/net/tun` device, and the WireGuard
    /// `src_valid_mark` sysctl. Without all three a client (PIA, WireGuard,
    /// OpenVPN) installs and runs but its connection attempt hangs until it
    /// times out, because it cannot create the tunnel interface or touch the
    /// routing table.
    ///
    /// Off by default and deliberately opt-in: `NET_ADMIN` lets anything in the
    /// container reconfigure its own network stack, which reaches further than
    /// it sounds — see `vpn_host_config` for what it does and does not confer.
    /// Unlike `auth_bridge_enabled` this *is*
    /// container state, so it carries a `triple-c.vpn-support` label and is
    /// compared in `container_needs_recreation` — capabilities and devices are
    /// fixed at creation and can only change by recreating the container.
    #[serde(default)]
    pub vpn_support_enabled: bool,
    /// Use the shared, long-lived Claude Code OAuth token (from
    /// `claude setup-token`, held in the OS keychain) for this project instead
    /// of requiring its own `claude login`. Only consulted when `backend` is
    /// [`Backend::Anthropic`] and a token has actually been stored.
    ///
    /// Defaults to **true** so a single `setup-token` run covers every project;
    /// turn it off to pin a project to the identity it logged in with inside
    /// its own container.
    #[serde(default = "default_use_shared_auth_token")]
    pub use_shared_auth_token: bool,
    /// Legacy binary permission flag. Superseded by `permission_mode`, but kept
    /// because it is the value already stored in users' `projects.json`; it is
    /// the fallback in `effective_permission_mode()` so old projects keep
    /// behaving identically without a data migration.
    #[serde(default = "default_full_permissions")]
    pub full_permissions: bool,
    /// Per-project permission mode. `None` means "not set yet" → fall back to
    /// the legacy `full_permissions` flag.
    #[serde(default)]
    pub permission_mode: Option<PermissionMode>,
    pub ssh_key_path: Option<String>,
    /// Per-project override for the corporate CA certificate path (file or
    /// directory). Blank falls back to `AppSettings::ca_cert_path`.
    ///
    /// `#[serde(default)]` rather than a required field: every project stored
    /// before this existed must keep loading.
    #[serde(default)]
    pub ca_cert_path: Option<String>,
    #[serde(skip_serializing, default)]
    pub git_token: Option<String>,
    pub git_user_name: Option<String>,
    pub git_user_email: Option<String>,
    #[serde(default)]
    pub custom_env_vars: Vec<EnvVar>,
    #[serde(default)]
    pub port_mappings: Vec<PortMapping>,
    #[serde(default)]
    pub claude_instructions: Option<String>,
    #[serde(default)]
    pub claude_code_settings: Option<ClaudeCodeSettings>,
    /// User-defined display names for terminal tabs, keyed by session id.
    #[serde(default)]
    pub renamed_session_names: HashMap<String, String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum ProjectStatus {
    Stopped,
    Starting,
    Running,
    Stopping,
    Error,
}

/// What `remove_project` could not delete, named so the UI can say so instead
/// of reporting a clean removal that was not one.
///
/// The project record is dropped from `projects.json` regardless — see the
/// long comment on `remove_project` for why refusing is not the answer — but
/// anything named here is also written to a pending-cleanup record that
/// startup housekeeping retries, so it stays reachable after the project it
/// belonged to no longer exists.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct ProjectRemovalReport {
    /// The project's container, if it could not be removed.
    pub container: Option<String>,
    /// The `triple-c-snapshot-{id}` image, if it could not be removed.
    pub image: Option<String>,
    /// Named volumes (home, claude config) that could not be removed.
    pub volumes: Vec<String>,
}

impl ProjectRemovalReport {
    /// True when nothing was left behind.
    pub fn is_clean(&self) -> bool {
        self.container.is_none() && self.image.is_none() && self.volumes.is_empty()
    }
}

/// Which AI model backend/provider the project uses.
/// - `Anthropic`: Direct Anthropic API (user runs `claude login` inside the container)
/// - `Bedrock`: AWS Bedrock with per-project AWS credentials
/// - `Ollama`: Local or remote Ollama server
/// - `LlamaCpp`: A local or remote `llama-server` (llama.cpp)
/// - `OpenAiCompatible`: Any endpoint that speaks the Anthropic Messages API
///   (e.g. LiteLLM). See [`Backend::uses_custom_endpoint`].
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Backend {
    /// Backward compat: old projects stored as "login" or "api_key" map to Anthropic.
    #[serde(alias = "login", alias = "api_key")]
    Anthropic,
    Bedrock,
    Ollama,
    /// Serialises as `llama_cpp`; the aliases accept the spellings a
    /// hand-edited `projects.json` is likely to contain.
    #[serde(alias = "llamacpp", alias = "llama-cpp", alias = "llama.cpp")]
    LlamaCpp,
    #[serde(alias = "lite_llm", alias = "litellm")]
    OpenAiCompatible,
}

impl Default for Backend {
    fn default() -> Self {
        Self::Anthropic
    }
}

impl Backend {
    /// Whether this backend points Claude Code at a non-Anthropic HTTP endpoint
    /// via `ANTHROPIC_BASE_URL`.
    ///
    /// Those endpoints serve whatever model *they* were started with, so
    /// Claude Code's built-in `opus`/`sonnet`/`haiku`/`fable` aliases resolve to
    /// Anthropic model ids the server has never heard of. Every backend for
    /// which this returns `true` therefore gets the
    /// `ANTHROPIC_DEFAULT_*_MODEL` alias vars pinned to the configured model —
    /// see `docker::container::compute_model_aliases`.
    ///
    /// Bedrock is deliberately excluded: it talks to AWS, which does host the
    /// real Anthropic model ids, so Claude Code's own defaults are correct
    /// there. Anthropic is excluded for the same reason.
    pub fn uses_custom_endpoint(&self) -> bool {
        matches!(
            self,
            Backend::Ollama | Backend::LlamaCpp | Backend::OpenAiCompatible
        )
    }
}

/// How Bedrock authenticates with AWS.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum BedrockAuthMethod {
    StaticCredentials,
    Profile,
    BearerToken,
}

impl Default for BedrockAuthMethod {
    fn default() -> Self {
        Self::StaticCredentials
    }
}

/// AWS Bedrock configuration for a project.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BedrockConfig {
    pub auth_method: BedrockAuthMethod,
    pub aws_region: String,
    #[serde(skip_serializing, default)]
    pub aws_access_key_id: Option<String>,
    #[serde(skip_serializing, default)]
    pub aws_secret_access_key: Option<String>,
    #[serde(skip_serializing, default)]
    pub aws_session_token: Option<String>,
    pub aws_profile: Option<String>,
    #[serde(skip_serializing, default)]
    pub aws_bearer_token: Option<String>,
    pub model_id: Option<String>,
    pub disable_prompt_caching: bool,
    /// Optional value for the `ANTHROPIC_BEDROCK_SERVICE_TIER` env var
    /// (e.g. "priority"). Empty/None means leave unset.
    #[serde(default)]
    pub service_tier: Option<String>,
}

/// Ollama configuration for a project.
/// Ollama natively implements the Anthropic Messages API at `/v1/messages`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OllamaConfig {
    /// The base URL of the Ollama server (e.g., "http://host.docker.internal:11434" or "http://192.168.1.100:11434")
    pub base_url: String,
    /// Optional model override (e.g., "qwen3.5:27b")
    pub model_id: Option<String>,
    /// Optional override for the model the `haiku` alias resolves to.
    /// Blank falls back to `model_id`. See [`Backend::uses_custom_endpoint`].
    #[serde(default)]
    pub haiku_model_id: Option<String>,
}

/// llama.cpp (`llama-server`) configuration for a project.
///
/// `llama-server` natively implements the Anthropic Messages API at
/// `POST /v1/messages` (plus `/v1/messages/count_tokens`), so Claude Code can
/// talk to it directly through `ANTHROPIC_BASE_URL` — exactly like Ollama, with
/// no translation shim.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlamaCppConfig {
    /// The base URL of the llama-server instance. `llama-server`'s default
    /// listen port is 8080 (`--port PORT | port to listen (default: 8080)`).
    pub base_url: String,
    /// Optional model override. `llama-server` serves whatever model it was
    /// started with, so this is mostly the id Claude Code should *say* it is
    /// using — but it is also what the model aliases are pinned to.
    pub model_id: Option<String>,
    /// Optional override for the model the `haiku` alias resolves to.
    /// Blank falls back to `model_id`.
    #[serde(default)]
    pub haiku_model_id: Option<String>,
}

/// OpenAI Compatible endpoint configuration for a project.
///
/// Despite the name (kept for backward compatibility with existing
/// `projects.json` data), the endpoint must implement the **Anthropic Messages
/// API** — Claude Code only ever speaks `POST /v1/messages`. Gateways such as
/// LiteLLM expose an Anthropic-shaped route and work; a bare
/// `/v1/chat/completions` server does not.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenAiCompatibleConfig {
    /// The base URL of the endpoint (e.g., "http://host.docker.internal:4000" or "https://api.example.com")
    pub base_url: String,
    /// API key for the endpoint
    #[serde(skip_serializing, default)]
    pub api_key: Option<String>,
    /// Optional model override
    pub model_id: Option<String>,
    /// Optional override for the model the `haiku` alias resolves to.
    /// Blank falls back to `model_id`.
    #[serde(default)]
    pub haiku_model_id: Option<String>,
}

impl Project {
    pub fn new(name: String, paths: Vec<ProjectPath>) -> Self {
        let now = chrono::Utc::now().to_rfc3339();
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            name,
            paths,
            container_id: None,
            status: ProjectStatus::Stopped,
            backend: Backend::default(),
            bedrock_config: None,
            ollama_config: None,
            llamacpp_config: None,
            openai_compatible_config: None,
            allow_docker_access: false,
            sandbox_mode_enabled: false,
            mission_control_enabled: false,
            auth_bridge_enabled: false,
            browser_view_enabled: false,
            vpn_support_enabled: false,
            use_shared_auth_token: default_use_shared_auth_token(),
            full_permissions: false,
            permission_mode: None,
            ssh_key_path: None,
            ca_cert_path: None,
            git_token: None,
            git_user_name: None,
            git_user_email: None,
            custom_env_vars: Vec::new(),
            port_mappings: Vec::new(),
            claude_instructions: None,
            claude_code_settings: None,
            renamed_session_names: HashMap::new(),
            created_at: now.clone(),
            updated_at: now,
        }
    }

    /// The permission mode to actually use for this project.
    /// Falls back to the legacy `full_permissions` boolean when the newer
    /// `permission_mode` field has never been set.
    pub fn effective_permission_mode(&self) -> PermissionMode {
        self.permission_mode.unwrap_or(if self.full_permissions {
            PermissionMode::Bypass
        } else {
            PermissionMode::Default
        })
    }

    pub fn container_name(&self) -> String {
        format!("triple-c-{}", self.id)
    }

    /// Migrate a project JSON value from old single-`path` format to new `paths` format.
    /// If the value already has `paths`, it is returned unchanged.
    pub fn migrate_from_value(mut val: serde_json::Value) -> serde_json::Value {
        if let Some(obj) = val.as_object_mut() {
            if obj.contains_key("paths") {
                return val;
            }
            if let Some(path_val) = obj.remove("path") {
                let path_str = path_val.as_str().unwrap_or("").to_string();
                let mount_name = path_str
                    .trim_end_matches(['/', '\\'])
                    .rsplit(['/', '\\'])
                    .next()
                    .unwrap_or("workspace")
                    .to_string();
                let project_path = serde_json::json!([{
                    "host_path": path_str,
                    "mount_name": if mount_name.is_empty() { "workspace".to_string() } else { mount_name },
                }]);
                obj.insert("paths".to_string(), project_path);
            }
        }
        val
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── ProjectRemovalReport ────────────────────────────────────────────────

    #[test]
    fn a_report_is_clean_only_with_nothing_left_behind() {
        assert!(ProjectRemovalReport::default().is_clean());

        let mut r = ProjectRemovalReport::default();
        r.container = Some("abc123".to_string());
        assert!(!r.is_clean(), "a leftover container must not read as clean");

        let mut r = ProjectRemovalReport::default();
        r.image = Some("triple-c-snapshot-x:latest".to_string());
        assert!(!r.is_clean(), "a leftover image must not read as clean");

        let mut r = ProjectRemovalReport::default();
        r.volumes.push("triple-c-home-x".to_string());
        assert!(!r.is_clean(), "a leftover volume must not read as clean");
    }

    // ── Custom environment variable names ─────────────────────────────────

    #[test]
    fn an_env_var_name_has_to_be_a_shell_identifier() {
        for ok in ["PATH", "_", "_x", "MY_VAR2", "a", " SPACED_BY_THE_EDITOR "] {
            assert!(is_valid_env_key(ok), "'{}' should be a usable name", ok);
        }
        for bad in [
            // bash's wire format for an exported shell function: the value is
            // the body, and a `bash` that imports it runs it. The scrub exec is
            // `/bin/sh -c` as root, and nothing pins `/bin/sh` to dash.
            "BASH_FUNC_stat%%",
            "BASH_FUNC_ls()",
            "MY VAR",
            "2FAST",
            "WITH-DASH",
            "WITH.DOT",
            "",
            "   ",
            "$(id)",
            "A=B",
        ] {
            assert!(!is_valid_env_key(bad), "'{}' should be refused", bad);
        }
    }

    fn env(key: &str, value: &str) -> EnvVar {
        EnvVar { key: key.to_string(), value: value.to_string() }
    }

    #[test]
    fn a_bad_env_var_name_cannot_be_introduced_but_a_stored_one_does_not_brick_the_editor() {
        let bad = [env("BASH_FUNC_stat%%", "() { id; }")];
        // Introducing it through the Config tab is the escalation.
        assert!(validate_env_vars_update(&[], &bad).is_err());
        // Already stored: it is handed to every container that starts whether
        // or not an unrelated save is allowed through, and refusing the save
        // would make every toggle on the Config tab fail.
        assert!(validate_env_vars_update(&bad, &bad).is_ok());
        // Editing its value is a new entry, and refused again.
        assert!(
            validate_env_vars_update(&bad, &[env("BASH_FUNC_stat%%", "() { rm -rf /; }")]).is_err()
        );
        // Fixing the name is what the message asks for, and it saves.
        assert!(validate_env_vars_update(&bad, &[env("STAT", "() { id; }")]).is_ok());
        // Dropping it entirely is always fine.
        assert!(validate_env_vars_update(&bad, &[]).is_ok());
    }

    #[test]
    fn the_blank_row_the_add_button_saves_is_not_an_error() {
        // "+ Add variable" appends an empty entry and saves the list at once,
        // so this is the button, not an attempt at anything.
        assert!(validate_env_vars_update(&[], &[env("", "")]).is_ok());
        // Typing the value before the name is an ordinary way to fill it in,
        // and an entry with no name reaches no container either way.
        assert!(validate_env_vars_update(&[], &[env("", "value-first")]).is_ok());
        assert!(validate_env_vars_update(&[], &[env("GOOD", "v"), env("", "")]).is_ok());
    }

    #[test]
    fn a_stored_entry_may_be_kept_but_not_multiplied() {
        let stored = [env("BAD NAME", "v")];
        assert!(validate_env_vars_update(&stored, &stored).is_ok());
        // A second copy is a new entry, and held to the rule.
        assert!(
            validate_env_vars_update(&stored, &[env("BAD NAME", "v"), env("BAD NAME", "v")])
                .is_err()
        );
    }

    // ── Claude Code settings written before the fields were widened ───────

    /// `projects.json` exactly as the shipped `main` binary wrote it: the five
    /// booleans were plain `bool`s that always serialised, so every project
    /// that ever opened the editor carries `false` for the ones it never
    /// touched.
    const MAIN_SHAPE_PROJECT: &str = r#"{
        "id": "p1",
        "name": "demo",
        "paths": [{ "host_path": "/home/u/demo", "mount_name": "demo" }],
        "container_id": null,
        "status": "stopped",
        "backend": "anthropic",
        "bedrock_config": null,
        "ollama_config": null,
        "openai_compatible_config": null,
        "allow_docker_access": false,
        "ssh_key_path": null,
        "git_user_name": null,
        "git_user_email": null,
        "claude_code_settings": {
            "tui_mode": "fullscreen",
            "effort": null,
            "auto_scroll_disabled": false,
            "focus_mode": false,
            "show_thinking_summaries": false,
            "enable_session_recap": false,
            "env_scrub": false,
            "prompt_caching_1h": false
        },
        "created_at": "2026-01-01T00:00:00Z",
        "updated_at": "2026-01-01T00:00:00Z"
    }"#;

    #[test]
    fn a_setting_stored_as_false_by_the_old_binary_still_inherits_the_global() {
        let project: Project = serde_json::from_str(MAIN_SHAPE_PROJECT).unwrap();
        let stored = project.claude_code_settings.expect("settings should parse");

        // Read verbatim these would be `Some(false)`, which under
        // `docker::container::merge_claude_code_settings` beats the global.
        assert_eq!(stored.env_scrub, None);
        assert_eq!(stored.auto_scroll_disabled, None);
        assert_eq!(stored.focus_mode, None);
        assert_eq!(stored.show_thinking_summaries, None);
        assert_eq!(stored.prompt_caching_1h, None);
        assert_eq!(stored.session_recap_disabled, None);
        // A value the user did choose is untouched.
        assert_eq!(stored.tui_mode.as_deref(), Some("fullscreen"));

        // The merge rule itself, spelled the way
        // `merge_claude_code_settings` spells it. `main` resolved this with
        // `if p.env_scrub { true } else { g.env_scrub }`, i.e. the global won —
        // and it has to go on winning, because the user never turned this off.
        let global = ClaudeCodeSettings { env_scrub: Some(true), ..Default::default() };
        assert_eq!(
            stored.env_scrub.or(global.env_scrub),
            Some(true),
            "upgrading silently turned off 'strip credentials from subprocess environments'"
        );
    }

    #[test]
    fn an_off_chosen_in_the_new_editor_still_beats_a_global_on() {
        // Same record without the pre-widening key: this `false` is the
        // deliberate off the widening exists to make expressible.
        let json = r#"{ "env_scrub": false }"#;
        let chosen: ClaudeCodeSettings = serde_json::from_str(json).unwrap();
        assert_eq!(chosen.env_scrub, Some(false));
        let global = ClaudeCodeSettings { env_scrub: Some(true), ..Default::default() };
        assert_eq!(chosen.env_scrub.or(global.env_scrub), Some(false));
    }

    #[test]
    fn an_unset_setting_is_written_as_absent_rather_than_null() {
        // A downgrade parses these fields as plain `bool` with
        // `#[serde(default)]`: an absent key is `false`, a `null` is a parse
        // error — and `ProjectsStore` parses all-or-nothing, so one project
        // with one null empties the whole list and the next save persists that.
        let json = serde_json::to_string(&ClaudeCodeSettings::default()).unwrap();
        assert_eq!(json, "{}");
        assert!(!json.contains("null"));

        let partial = ClaudeCodeSettings { env_scrub: Some(false), ..Default::default() };
        let json = serde_json::to_string(&partial).unwrap();
        assert_eq!(json, r#"{"env_scrub":false}"#);
        // And it reads back as what it is.
        let round_tripped: ClaudeCodeSettings = serde_json::from_str(&json).unwrap();
        assert_eq!(round_tripped, partial);
    }
}
