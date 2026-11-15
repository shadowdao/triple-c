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

/// `auth_bridge_enabled` defaults to **on**, and the default is what makes
/// `claude login` work at all.
///
/// The login flow binds a *random* ephemeral loopback port inside the
/// container and then sends the host's browser to `127.0.0.1:<that port>`.
/// On the host nothing is listening there, so the callback lands on a closed
/// port and the CLI waits for a redirect that can never arrive. The bridge
/// mirrors the container's loopback listeners onto the same host port, which
/// is the only thing that closes that loop — so off-by-default made a hang the
/// out-of-the-box experience.
///
/// Returning `true` from a `#[serde(default)]` helper (rather than flipping the
/// constructor alone) is deliberate: existing `projects.json` records were
/// written before this field existed, or while it was off, and an absent key is
/// what the default is read for. A project that wants the old behaviour turns
/// the toggle off, which persists an explicit `false`.
fn default_auth_bridge_enabled() -> bool {
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
    /// Claude Code's classifier approves safe actions and blocks risky ones,
    /// without prompting.
    Auto,
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
            PermissionMode::Auto => vec!["--permission-mode".to_string(), "auto".to_string()],
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
            PermissionMode::Auto => "auto",
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
    /// The auth bridge: while the container runs, its loopback listeners are
    /// mirrored onto the host's loopback so browser OAuth callbacks
    /// (`claude login`, `fly login`, `aws sso login`) can reach them.
    /// Purely host-side — it deliberately has no container-recreation label,
    /// because toggling it changes nothing about the container itself.
    ///
    /// **On by default**, and opt-*out* rather than opt-in — see
    /// [`default_auth_bridge_enabled`] for why the default is the feature.
    #[serde(default = "default_auth_bridge_enabled")]
    pub auth_bridge_enabled: bool,
    /// Opt in to the browser-view pane, which watches and takes over the
    /// browser Claude drives with Playwright inside the container. Purely
    /// host-side like `auth_bridge_enabled`, so it likewise has no
    /// container-recreation label.
    ///
    /// This is the *durable* home of the flag: `BrowserViewManager` reads it
    /// rather than keeping its own copy, so the pane comes back the way it was
    /// left. Off by default, and unlike the auth bridge it stays that way — a
    /// view costs a container exec, a Node daemon and a host port, and a
    /// container without Playwright cannot serve one at all.
    ///
    /// Durable does **not** mean auto-started: nothing brings a viewer up on
    /// app start, so a project left enabled reports `enabled` with a state of
    /// `Off` until the pane (or `open_page_in_container_browser`) asks for one.
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
    /// Marketplace items installed for this project only (spec §2).
    #[serde(default)]
    pub marketplace_installs: Vec<super::marketplace::MarketplaceInstall>,
    /// Global marketplace installs this project opts out of.
    #[serde(default)]
    pub marketplace_disabled: Vec<super::marketplace::MarketplaceItemRef>,
    /// The host connection this project runs on. `None` is the local Docker
    /// host; `Some(id)` names an entry of `AppSettings.hosts`.
    #[serde(default)]
    pub host_id: Option<String>,
    /// Region and environment of an AWS-hosted project. Set together with
    /// `host_id` and `aws`, or not at all.
    #[serde(default)]
    pub aws_placement: Option<AwsPlacement>,
    #[serde(default)]
    pub aws: Option<AwsWorkspaceConfig>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AwsPlacement {
    pub region: String,
    pub environment: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum Arch {
    #[default]
    X86_64,
    Arm64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum StorageMode {
    Ephemeral,
    #[default]
    Hybrid,
}

/// One git repository cloned into the workspace. `branch: None` means the
/// remote's default branch.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RepoSource {
    pub url: String,
    #[serde(default)]
    pub branch: Option<String>,
    pub folder: String,
}

fn default_aws_cpu() -> u32 {
    1024
}
fn default_aws_memory_mib() -> u32 {
    4096
}
fn default_aws_ephemeral_gib() -> u32 {
    21
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AwsWorkspaceConfig {
    #[serde(default = "default_aws_cpu")]
    pub cpu: u32,
    #[serde(default = "default_aws_memory_mib")]
    pub memory_mib: u32,
    #[serde(default)]
    pub arch: Arch,
    #[serde(default)]
    pub storage: StorageMode,
    #[serde(default = "default_aws_ephemeral_gib")]
    pub ephemeral_gib: u32,
    #[serde(default)]
    pub spot: bool,
    #[serde(default)]
    pub idle_stop_minutes: Option<u32>,
    #[serde(default)]
    pub repos: Vec<RepoSource>,
    /// Push uncommitted work to a WIP branch when the task stops. Defaults on.
    #[serde(default = "default_full_permissions")]
    pub wip_push_on_stop: bool,
    /// Store-owned runtime state: see `ProjectsStore::set_aws_runtime`.
    #[serde(default)]
    pub task_arn: Option<String>,
    #[serde(default)]
    pub access_point_id: Option<String>,
    #[serde(default)]
    pub task_definition_arn: Option<String>,
}

impl Default for AwsWorkspaceConfig {
    fn default() -> Self {
        Self {
            cpu: default_aws_cpu(),
            memory_mib: default_aws_memory_mib(),
            arch: Arch::default(),
            storage: StorageMode::default(),
            ephemeral_gib: default_aws_ephemeral_gib(),
            spot: false,
            idle_stop_minutes: None,
            repos: Vec::new(),
            wip_push_on_stop: true,
            task_arn: None,
            access_point_id: None,
            task_definition_arn: None,
        }
    }
}

/// The valid Fargate memory range (min, max, step) in MiB for a vCPU size.
fn fargate_memory_range(cpu: u32) -> Option<(u32, u32, u32)> {
    match cpu {
        256 => Some((512, 2048, 512)),
        512 => Some((1024, 4096, 1024)),
        1024 => Some((2048, 8192, 1024)),
        2048 => Some((4096, 16384, 1024)),
        4096 => Some((8192, 30720, 1024)),
        8192 => Some((16384, 61440, 4096)),
        16384 => Some((32768, 122880, 8192)),
        _ => None,
    }
}

fn validate_repo_url(url: &str) -> Result<(), String> {
    let bad = |m: &str| Err(format!("Repository URL \"{}\": {}", url.escape_debug(), m));
    if url.is_empty() || url.len() > 2048 {
        return bad("must be 1 to 2048 characters");
    }
    if url.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return bad("must not contain whitespace or control characters");
    }
    if url.starts_with('-') {
        return bad("must not start with '-'");
    }
    let ok = if let Some(rest) = url.strip_prefix("https://") {
        !rest.is_empty()
    } else if let Some(rest) = url.strip_prefix("ssh://") {
        !rest.is_empty()
    } else if let Some((host, path)) = url.split_once(':') {
        // scp-like `git@host:path`
        !host.is_empty() && !host.contains('/') && !path.is_empty() && host.contains('@')
    } else {
        false
    };
    if ok {
        Ok(())
    } else {
        bad("use https://..., ssh://... or git@host:path")
    }
}

/// `git check-ref-format --branch` rules.
fn validate_branch(branch: &str) -> Result<(), String> {
    let bad = |m: &str| Err(format!("Branch \"{}\": {}", branch.escape_debug(), m));
    if branch.is_empty() || branch.len() > 255 {
        return bad("must be 1 to 255 characters");
    }
    if branch.starts_with('-') || branch.starts_with('/') || branch.ends_with('/') {
        return bad("must not start with '-' or start/end with '/'");
    }
    if branch == "@" || branch.ends_with('.') || branch.contains("..") || branch.contains("@{") || branch.contains("//") {
        return bad("is not a valid branch name");
    }
    if branch
        .chars()
        .any(|c| c.is_control() || c.is_whitespace() || "~^:?*[\\".contains(c))
    {
        return bad("contains a character git does not allow");
    }
    for comp in branch.split('/') {
        if comp.starts_with('.') || comp.ends_with(".lock") {
            return bad("components must not start with '.' or end with '.lock'");
        }
    }
    Ok(())
}

fn validate_repo_folder(folder: &str) -> Result<(), String> {
    let ok = !folder.is_empty()
        && folder.len() <= 64
        && folder != "."
        && folder != ".."
        && folder
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'-'));
    if ok {
        Ok(())
    } else {
        Err(format!(
            "Repository folder \"{}\" must be 1 to 64 characters of letters, digits, '.', '_' or '-', and not '.' or '..'",
            folder.escape_debug()
        ))
    }
}

pub fn validate_aws_workspace(cfg: &AwsWorkspaceConfig) -> Result<(), String> {
    let (min, max, step) = fargate_memory_range(cfg.cpu).ok_or_else(|| {
        format!(
            "{} is not a Fargate CPU size (256, 512, 1024, 2048, 4096, 8192 or 16384)",
            cfg.cpu
        )
    })?;
    if cfg.memory_mib < min || cfg.memory_mib > max || (cfg.memory_mib - min) % step != 0 {
        return Err(format!(
            "{} MiB of memory is not valid with {} CPU units: use {} to {} MiB in steps of {}",
            cfg.memory_mib, cfg.cpu, min, max, step
        ));
    }
    if !(21..=200).contains(&cfg.ephemeral_gib) {
        return Err("Ephemeral storage must be 21 to 200 GiB".into());
    }
    if let Some(m) = cfg.idle_stop_minutes {
        if !(5..=1440).contains(&m) {
            return Err("Idle stop must be 5 to 1440 minutes".into());
        }
    }
    if cfg.repos.len() > 20 {
        return Err("At most 20 repositories are allowed".into());
    }
    let mut seen = std::collections::HashSet::new();
    for r in &cfg.repos {
        validate_repo_url(&r.url)?;
        if let Some(b) = &r.branch {
            validate_branch(b)?;
        }
        validate_repo_folder(&r.folder)?;
        if !seen.insert(r.folder.as_str()) {
            return Err(format!("Repository folder \"{}\" is used twice", r.folder));
        }
    }
    Ok(())
}

/// What Stop does with an AWS workspace's unsaved git work (the stop guard).
/// A local project ignores it.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WipChoice {
    /// Run `triple-c-wip-push --reason stop` first; a failure aborts the stop.
    Push,
    /// Stop without pushing from the app.
    Discard,
}

/// One repository line of `triple-c-wip-push --dry-run`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RepoState {
    pub folder: String,
    pub dirty: bool,
    pub unpushed: u64,
    pub branch: String,
}

/// What `check_unsaved_work` found. `checked: false` (with `error`) when the
/// workspace could not be asked — the stop guard then cannot offer a push.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct UnsavedWork {
    pub checked: bool,
    pub repos: Vec<RepoState>,
    pub error: Option<String>,
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
    /// The project's container, if it could not be removed. Named by its
    /// deterministic `triple-c-{id}` name (see `Project::container_name`),
    /// not the container id, since the id can be stale or absent and the
    /// name is what a later retry can still resolve.
    pub container: Option<String>,
    /// The `triple-c-snapshot-{id}` image, if it could not be removed.
    pub image: Option<String>,
    /// Named volumes (home, claude config) that could not be removed.
    pub volumes: Vec<String>,
    /// True once the leftovers above were durably recorded for automatic
    /// retry on the next launch. False means the pending-cleanup record
    /// itself could not be written — nothing will retry these, and the UI
    /// must say so rather than promising a retry that will not happen.
    /// Meaningless (and left at its default) when `is_clean()` is true.
    pub retry_scheduled: bool,
}

impl ProjectRemovalReport {
    /// True when nothing was left behind.
    pub fn is_clean(&self) -> bool {
        self.container.is_none() && self.image.is_none() && self.volumes.is_empty()
    }
}

/// What `rebuild_project_container` (Reset) produced: the project as it
/// stands after restarting, and anything Reset could not clear.
///
/// Reset's contract is "back to a clean base image", so a leftover volume or
/// image here is reused/rebuilt-from as-is by the container this creates —
/// the opposite of what was asked for — and unlike [`ProjectRemovalReport`]
/// there is no pending-cleanup record for either: the project id survives
/// Reset, so a later Reset attempt can retry them itself.
#[derive(Debug, Clone, Serialize)]
pub struct ProjectResetOutcome {
    pub project: Project,
    /// The `triple-c-snapshot-{id}` image, if Reset could not remove it. The
    /// more serious of the two leftovers here: the new container is created
    /// from this image whenever it exists, so a surviving image means Reset
    /// silently rebuilt the exact system layer it was asked to discard.
    pub leftover_image: Option<String>,
    /// Volumes that survived Reset and were mounted into the new container
    /// unchanged.
    pub leftover_volumes: Vec<String>,
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
            auth_bridge_enabled: default_auth_bridge_enabled(),
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
            marketplace_installs: Vec::new(),
            marketplace_disabled: Vec::new(),
            host_id: None,
            aws_placement: None,
            aws: None,
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

    /// Whether this project runs on an AWS host rather than local Docker.
    pub fn is_aws(&self) -> bool {
        self.host_id.is_some()
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
        EnvVar {
            key: key.to_string(),
            value: value.to_string(),
        }
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
        let global = ClaudeCodeSettings {
            env_scrub: Some(true),
            ..Default::default()
        };
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
        let global = ClaudeCodeSettings {
            env_scrub: Some(true),
            ..Default::default()
        };
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

        let partial = ClaudeCodeSettings {
            env_scrub: Some(false),
            ..Default::default()
        };
        let json = serde_json::to_string(&partial).unwrap();
        assert_eq!(json, r#"{"env_scrub":false}"#);
        // And it reads back as what it is.
        let round_tripped: ClaudeCodeSettings = serde_json::from_str(&json).unwrap();
        assert_eq!(round_tripped, partial);
    }

    // ── The host-side per-project toggles ─────────────────────────────────

    #[test]
    fn a_project_stored_before_the_auth_bridge_existed_gets_it_turned_on() {
        // The whole point of the serde default: `MAIN_SHAPE_PROJECT` is a real
        // record written by a shipped binary and has no `auth_bridge_enabled`
        // key at all. Without this, every existing project keeps hanging on
        // `claude login` until its owner finds the toggle.
        assert!(!MAIN_SHAPE_PROJECT.contains("auth_bridge_enabled"));
        let project: Project = serde_json::from_str(MAIN_SHAPE_PROJECT).unwrap();
        assert!(project.auth_bridge_enabled);

        // The browser view is the other way round and must stay so: it costs a
        // Node daemon, a container exec loop and a host port, and most
        // containers have no Playwright to serve it with.
        assert!(!project.browser_view_enabled);
    }

    #[test]
    fn turning_the_auth_bridge_off_survives_the_default() {
        // Opt-out has to be expressible, or the toggle does nothing across a
        // restart. An explicit `false` in the file beats the default.
        let json = r#"{ "auth_bridge_enabled": false }"#;
        #[derive(Deserialize)]
        struct JustTheFlag {
            #[serde(default = "default_auth_bridge_enabled")]
            auth_bridge_enabled: bool,
        }
        let parsed: JustTheFlag = serde_json::from_str(json).unwrap();
        assert!(!parsed.auth_bridge_enabled);

        // And a saved project always writes the key, so the choice is pinned
        // rather than re-defaulted on the next load.
        let mut p = Project::new("demo".to_string(), Vec::new());
        p.auth_bridge_enabled = false;
        let round_tripped: Project =
            serde_json::from_str(&serde_json::to_string(&p).unwrap()).unwrap();
        assert!(!round_tripped.auth_bridge_enabled);
    }

    #[test]
    fn a_new_project_starts_with_the_bridge_on_and_the_view_off() {
        let p = Project::new("demo".to_string(), Vec::new());
        assert!(p.auth_bridge_enabled);
        assert!(!p.browser_view_enabled);
    }

    #[test]
    fn the_path_migration_never_writes_the_flags_and_so_cannot_defeat_the_default() {
        // `ProjectsStore::new` runs every record through this before
        // deserialising. If it inserted either key — even as `false` — the
        // serde default above would never be consulted for an existing project
        // and this change would be a no-op on exactly the projects it is for.
        let legacy = serde_json::json!({
            "id": "p1",
            "name": "demo",
            "path": "/home/u/demo",
        });
        let migrated = Project::migrate_from_value(legacy);
        let obj = migrated.as_object().unwrap();
        assert!(
            obj.contains_key("paths"),
            "the migration should still do its own job"
        );
        assert!(!obj.contains_key("auth_bridge_enabled"));
        assert!(!obj.contains_key("browser_view_enabled"));
    }

    // ── AWS host ────────────────────────────────────────────────────────────

    fn cfg(cpu: u32, mem: u32) -> AwsWorkspaceConfig {
        AwsWorkspaceConfig { cpu, memory_mib: mem, ..Default::default() }
    }

    fn repo(url: &str, branch: Option<&str>, folder: &str) -> RepoSource {
        RepoSource { url: url.into(), branch: branch.map(String::from), folder: folder.into() }
    }

    #[test]
    fn an_old_project_without_host_fields_loads_as_local() {
        let p = Project::new("demo".to_string(), Vec::new());
        let mut v = serde_json::to_value(&p).unwrap();
        let o = v.as_object_mut().unwrap();
        o.remove("host_id");
        o.remove("aws_placement");
        o.remove("aws");
        let loaded: Project = serde_json::from_value(v).unwrap();
        assert_eq!(loaded.host_id, None);
        assert!(loaded.aws.is_none() && loaded.aws_placement.is_none());
        assert!(!loaded.is_aws());
    }

    #[test]
    fn an_empty_aws_object_gets_the_defaults() {
        let c: AwsWorkspaceConfig = serde_json::from_str("{}").unwrap();
        assert!(c.wip_push_on_stop);
        assert_eq!((c.cpu, c.memory_mib, c.ephemeral_gib), (1024, 4096, 21));
        assert_eq!(c.arch, Arch::X86_64);
        assert_eq!(c.storage, StorageMode::Hybrid);
        assert!(!c.spot && c.idle_stop_minutes.is_none() && c.repos.is_empty());
        assert!(validate_aws_workspace(&c).is_ok());
        let off: AwsWorkspaceConfig = serde_json::from_str(r#"{"wip_push_on_stop":false}"#).unwrap();
        assert!(!off.wip_push_on_stop);
    }

    #[test]
    fn enums_serialize_snake_case_and_is_aws_follows_host_id() {
        assert_eq!(serde_json::to_string(&Arch::Arm64).unwrap(), "\"arm64\"");
        assert_eq!(serde_json::to_string(&Arch::X86_64).unwrap(), "\"x86_64\"");
        assert_eq!(serde_json::to_string(&StorageMode::Hybrid).unwrap(), "\"hybrid\"");
        let mut p = Project::new("d".into(), Vec::new());
        p.host_id = Some("h1".into());
        assert!(p.is_aws());
    }

    #[test]
    fn cpu_and_memory_must_be_a_fargate_pair() {
        for (c, m) in [(256, 512), (256, 2048), (1024, 2048), (1024, 8192), (2048, 4096), (4096, 30720), (8192, 61440), (16384, 32768), (16384, 122880)] {
            assert!(validate_aws_workspace(&cfg(c, m)).is_ok(), "{c}/{m} should be valid");
        }
        for (c, m) in [(256, 4096), (1024, 1024), (1024, 9216), (1024, 2500), (300, 1024), (16384, 114688 + 1), (8192, 18432)] {
            assert!(validate_aws_workspace(&cfg(c, m)).is_err(), "{c}/{m} should be rejected");
        }
    }

    #[test]
    fn ephemeral_storage_is_21_to_200() {
        for (g, ok) in [(20, false), (21, true), (200, true), (201, false)] {
            let c = AwsWorkspaceConfig { ephemeral_gib: g, ..Default::default() };
            assert_eq!(validate_aws_workspace(&c).is_ok(), ok, "{g}");
        }
    }

    #[test]
    fn idle_stop_is_5_to_1440_or_off() {
        for (m, ok) in [(None, true), (Some(4), false), (Some(5), true), (Some(1440), true), (Some(1441), false)] {
            let c = AwsWorkspaceConfig { idle_stop_minutes: m, ..Default::default() };
            assert_eq!(validate_aws_workspace(&c).is_ok(), ok, "{m:?}");
        }
    }

    fn with_repos(repos: Vec<RepoSource>) -> AwsWorkspaceConfig {
        AwsWorkspaceConfig { repos, ..Default::default() }
    }

    #[test]
    fn repo_folders_follow_the_charset_and_are_unique() {
        let ok = |f: &str| validate_aws_workspace(&with_repos(vec![repo("https://h/r.git", None, f)])).is_ok();
        assert!(ok("my-repo_1.x"));
        assert!(ok(&"a".repeat(64)));
        for bad in ["", ".", "..", "a/b", "a b", "ü", &"a".repeat(65)] {
            assert!(!ok(bad), "{bad:?}");
        }
        let dup = with_repos(vec![repo("https://h/a", None, "x"), repo("https://h/b", None, "x")]);
        assert!(validate_aws_workspace(&dup).unwrap_err().contains("twice"));
        let two = with_repos(vec![repo("https://h/a", None, "x"), repo("https://h/b", None, "y")]);
        assert!(validate_aws_workspace(&two).is_ok());
    }

    #[test]
    fn repo_urls_are_https_scp_or_ssh() {
        let ok = |u: &str| validate_aws_workspace(&with_repos(vec![repo(u, None, "x")])).is_ok();
        for good in ["https://github.com/o/r.git", "git@github.com:o/r.git", "ssh://git@h/o/r.git"] {
            assert!(ok(good), "{good}");
        }
        for bad in ["", "http://h/r", "file:///etc", "/local/path", "--upload-pack=x", "github.com:o/r", "https://h/r x", "ext::sh -c id", "https://"] {
            assert!(!ok(bad), "{bad:?}");
        }
    }

    #[test]
    fn branches_follow_check_ref_format() {
        let ok = |b: Option<&str>| validate_aws_workspace(&with_repos(vec![repo("https://h/r", b, "x")])).is_ok();
        for good in [None, Some("main"), Some("feat/aws-1"), Some("v1.2.3"), Some("a@b")] {
            assert!(ok(good), "{good:?}");
        }
        for bad in ["", "-x", "a..b", "a b", "a~1", "a^", "a:b", "a?", "a*", "a[", "a\\b", "a.lock", "x/.hidden", "a/", "/a", "a//b", "a@{1}", "@", "a."] {
            assert!(!ok(Some(bad)), "{bad:?}");
        }
    }

    #[test]
    fn at_most_20_repos() {
        let mk = |n: usize| with_repos((0..n).map(|i| repo("https://h/r", None, &format!("r{i}"))).collect());
        assert!(validate_aws_workspace(&mk(20)).is_ok());
        assert!(validate_aws_workspace(&mk(21)).is_err());
    }
}
