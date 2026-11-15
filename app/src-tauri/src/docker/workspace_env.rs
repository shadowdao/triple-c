//! The container environment a project gets, built once for every host.
//!
//! Docker passes these pairs as the container's `Env`; the AWS host stores the
//! very same pairs in a per-project Secrets Manager secret. Both therefore call
//! [`workspace_env`], so a backend, a CA bundle or a custom variable cannot
//! reach one host and not the other.
//!
//! What is *not* here: `COLORTERM`, `HOST_UID` and `HOST_GID` (host-specific,
//! assembled by `create_container`), and anything that needs the host — the
//! corporate CA is resolved from disk and the shared Claude token read from
//! the keychain by the caller, then handed in through [`WorkspaceEnvInputs`].
//! Order is part of the contract: it is the order `docker inspect` shows.

use super::ca_certs::{self, ResolvedCa};
use super::container::{
    build_claude_code_settings_json, build_claude_instructions, claude_code_env_vars,
    compute_env_fingerprint, compute_model_aliases, is_reserved_env_key, merge_claude_code_settings,
    merge_custom_env_vars, resolve_with_global, split_claude_code_settings_payload, vpn_env_var,
    ANTHROPIC_DEFAULT_FABLE_MODEL, ANTHROPIC_DEFAULT_HAIKU_MODEL, ANTHROPIC_DEFAULT_OPUS_MODEL,
    ANTHROPIC_DEFAULT_SONNET_MODEL, CLAUDE_OAUTH_TOKEN_ENV,
};
use crate::models::{
    Backend, BedrockAuthMethod, ClaudeCodeSettings, EnvVar, GlobalAwsSettings,
    GlobalLlamaCppSettings, GlobalOllamaSettings, GlobalOpenAiCompatibleSettings, Project,
};

/// Everything besides the project that shapes its environment.
pub(crate) struct WorkspaceEnvInputs<'a> {
    pub global_aws: &'a GlobalAwsSettings,
    pub global_ollama: &'a GlobalOllamaSettings,
    pub global_llamacpp: &'a GlobalLlamaCppSettings,
    pub global_openai_compatible: &'a GlobalOpenAiCompatibleSettings,
    pub global_claude_instructions: Option<&'a str>,
    pub global_custom_env_vars: &'a [EnvVar],
    pub timezone: Option<&'a str>,
    pub global_claude_code_settings: Option<&'a ClaudeCodeSettings>,
    pub default_git_user_name: Option<&'a str>,
    pub default_git_user_email: Option<&'a str>,
    /// The corporate CA, already resolved from disk (`ca_certs::resolve`).
    pub resolved_ca: Option<&'a ResolvedCa>,
    /// The shared Claude OAuth token, already read from the keychain and
    /// already filtered by `shared_claude_auth` (backend, opt-out, non-blank).
    pub shared_claude_token: Option<&'a str>,
}

/// `"KEY=VALUE"` to a pair, splitting at the first `=` (values may contain `=`).
fn split_pair(entry: &str) -> (String, String) {
    match entry.split_once('=') {
        Some((k, v)) => (k.to_string(), v.to_string()),
        None => (entry.to_string(), String::new()),
    }
}

/// The `KEY=VALUE` pairs a project's container gets, in order, excluding the
/// host-specific ones. See the module docs.
pub(crate) fn workspace_env(
    project: &Project,
    inputs: &WorkspaceEnvInputs,
) -> Vec<(String, String)> {
    let WorkspaceEnvInputs {
        global_aws,
        global_ollama,
        global_llamacpp,
        global_openai_compatible,
        global_claude_instructions,
        global_custom_env_vars,
        timezone,
        global_claude_code_settings,
        default_git_user_name,
        default_git_user_email,
        resolved_ca,
        shared_claude_token,
    } = *inputs;
    let mut env: Vec<(String, String)> = Vec::new();

    if let Some(ref token) = project.git_token {
        env.push(("GIT_TOKEN".to_string(), token.to_string()));
    }
    // Per-project git user overrides global defaults
    let effective_git_name = project.git_user_name.as_deref().or(default_git_user_name);
    let effective_git_email = project.git_user_email.as_deref().or(default_git_user_email);
    if let Some(name) = effective_git_name {
        env.push(("GIT_USER_NAME".to_string(), name.to_string()));
    }
    if let Some(email) = effective_git_email {
        env.push(("GIT_USER_EMAIL".to_string(), email.to_string()));
    }

    // Bedrock configuration
    if project.backend == Backend::Bedrock {
        if let Some(ref bedrock) = project.bedrock_config {
            env.push(("CLAUDE_CODE_USE_BEDROCK".to_string(), "1".to_string()));

            // AWS region: per-project overrides global
            let region = if !bedrock.aws_region.is_empty() {
                Some(bedrock.aws_region.clone())
            } else {
                global_aws.aws_region.clone()
            };
            if let Some(ref r) = region {
                env.push(("AWS_REGION".to_string(), r.to_string()));
            }

            match bedrock.auth_method {
                BedrockAuthMethod::StaticCredentials => {
                    // Static/session credentials are NOT injected as env vars.
                    // They are written to ~/.aws/credentials by
                    // sync_bedrock_credentials() on every container
                    // start, so rotated/updated keys are picked up without a
                    // full container recreation (and never get baked into the
                    // snapshot image). The empty values set by the
                    // MANAGED_AUTH_KEYS neutralization pass below are ignored by
                    // the AWS SDK, which falls through to the credentials file.
                }
                BedrockAuthMethod::Profile => {
                    // Per-project profile overrides global
                    let profile = bedrock.aws_profile.as_ref()
                        .or(global_aws.aws_profile.as_ref());
                    if let Some(p) = profile {
                        env.push(("AWS_PROFILE".to_string(), p.to_string()));
                    }
                    env.push(("AWS_SSO_AUTH_REFRESH_CMD".to_string(), "triple-c-sso-refresh".to_string()));
                }
                BedrockAuthMethod::BearerToken => {
                    if let Some(ref token) = bedrock.aws_bearer_token {
                        env.push(("AWS_BEARER_TOKEN_BEDROCK".to_string(), token.to_string()));
                    }
                }
            }

            if let Some(model) = resolve_with_global(
                bedrock.model_id.as_deref(),
                global_aws.default_model_id.as_deref(),
            ) {
                env.push(("ANTHROPIC_MODEL".to_string(), model.to_string()));
            }

            if bedrock.disable_prompt_caching {
                env.push(("DISABLE_PROMPT_CACHING".to_string(), "1".to_string()));
            }

            if let Some(ref tier) = bedrock.service_tier {
                let trimmed = tier.trim();
                if !trimmed.is_empty() {
                    env.push(("ANTHROPIC_BEDROCK_SERVICE_TIER".to_string(), trimmed.to_string()));
                }
            }
        }
    }

    // ── Custom-endpoint backends ─────────────────────────────────────────────
    // Ollama, llama.cpp and the OpenAI-Compatible gateway all point Claude Code
    // at a non-Anthropic server via ANTHROPIC_BASE_URL. Each resolves its model
    // id here; the model-alias vars are emitted once below, from
    // `alias_model` / `alias_haiku`, so the three backends cannot drift apart.
    let mut alias_model: Option<String> = None;
    let mut alias_haiku: Option<String> = None;

    // Ollama configuration
    if project.backend == Backend::Ollama {
        if let Some(ref ollama) = project.ollama_config {
            if let Some(url) = resolve_with_global(
                Some(&ollama.base_url),
                global_ollama.base_url.as_deref(),
            ) {
                env.push(("ANTHROPIC_BASE_URL".to_string(), url.to_string()));
            }
            env.push(("ANTHROPIC_AUTH_TOKEN".to_string(), "ollama".to_string()));
            if let Some(model) = resolve_with_global(
                ollama.model_id.as_deref(),
                global_ollama.default_model_id.as_deref(),
            ) {
                env.push(("ANTHROPIC_MODEL".to_string(), model.to_string()));
                alias_model = Some(model.to_string());
            }
            alias_haiku = resolve_with_global(
                ollama.haiku_model_id.as_deref(),
                global_ollama.default_haiku_model_id.as_deref(),
            )
            .map(str::to_string);
        }
    }

    // llama.cpp (llama-server) configuration
    if project.backend == Backend::LlamaCpp {
        if let Some(ref cfg) = project.llamacpp_config {
            if let Some(url) = resolve_with_global(
                Some(&cfg.base_url),
                global_llamacpp.base_url.as_deref(),
            ) {
                env.push(("ANTHROPIC_BASE_URL".to_string(), url.to_string()));
            }
            // llama-server only enforces an Authorization header when it was
            // started with `--api-key` (default: none), so the value here is
            // ignored in the common case. Claude Code still refuses to run
            // against a custom base URL with no credential at all, so a
            // placeholder is always sent — same trick as the Ollama branch
            // above, which sends the literal "ollama".
            env.push(("ANTHROPIC_AUTH_TOKEN".to_string(), "llama.cpp".to_string()));
            if let Some(model) = resolve_with_global(
                cfg.model_id.as_deref(),
                global_llamacpp.default_model_id.as_deref(),
            ) {
                env.push(("ANTHROPIC_MODEL".to_string(), model.to_string()));
                alias_model = Some(model.to_string());
            }
            alias_haiku = resolve_with_global(
                cfg.haiku_model_id.as_deref(),
                global_llamacpp.default_haiku_model_id.as_deref(),
            )
            .map(str::to_string);
        }
    }

    // OpenAI Compatible configuration
    if project.backend == Backend::OpenAiCompatible {
        if let Some(ref config) = project.openai_compatible_config {
            if let Some(url) = resolve_with_global(
                Some(&config.base_url),
                global_openai_compatible.base_url.as_deref(),
            ) {
                env.push(("ANTHROPIC_BASE_URL".to_string(), url.to_string()));
            }
            if let Some(ref key) = config.api_key {
                env.push(("ANTHROPIC_AUTH_TOKEN".to_string(), key.to_string()));
            }
            if let Some(model) = resolve_with_global(
                config.model_id.as_deref(),
                global_openai_compatible.default_model_id.as_deref(),
            ) {
                env.push(("ANTHROPIC_MODEL".to_string(), model.to_string()));
                alias_model = Some(model.to_string());
            }
            alias_haiku = resolve_with_global(
                config.haiku_model_id.as_deref(),
                global_openai_compatible.default_haiku_model_id.as_deref(),
            )
            .map(str::to_string);
        }
    }

    // Model aliases — the fix for background Claude Code calls against a local
    // server. Only for backends that talk to a custom endpoint: Anthropic and
    // Bedrock reach servers that really do host the Anthropic model ids, so
    // they keep Claude Code's own defaults. Anything not emitted here is
    // blanked by the MANAGED_AUTH_KEYS pass below, so switching *away* from a
    // custom endpoint clears the aliases out of the snapshot image too.
    if project.backend.uses_custom_endpoint() {
        for (key, value) in
            compute_model_aliases(alias_model.as_deref(), alias_haiku.as_deref())
        {
            env.push((key.to_string(), value));
        }
    }

    // Shared Claude Code OAuth token (Anthropic backend only, opt-out per
    // project). Injected *before* the neutralization pass below so that pass
    // sees it as already-set; when it is absent the pass actively blanks the
    // variable instead of leaving a stale one baked into the snapshot image.
    if let Some(token) = shared_claude_token {
        env.push((CLAUDE_OAUTH_TOKEN_ENV.to_string(), token.to_string()));
    }

    // ── Corporate CA certificates ───────────────────────────────────────────
    // Resolved here (rather than down with the mounts) so the env vars land
    // *before* the neutralization pass below and are seen as already-set.
    //
    // A bad path is a hard error, not a warning: behind a TLS-terminating
    // proxy a container without the CA fails every HTTPS call — npm, pip, git,
    // and Claude Code's own API requests — each in its own confusing way. One
    // message naming the path is far kinder.
    //
    // The values are set here rather than exported by the entrypoint because a
    // terminal session is a `docker exec`, which sees the container's
    // configured env and nothing the entrypoint exported. Same lesson as
    // `$BROWSER` and the URL relay shim.
    for (key, value) in ca_certs::ca_env_vars(resolved_ca) {
        env.push((key.to_string(), value));
    }

    // ── Neutralize stale backend auth env vars ──────────────────────────────
    // When a project switches backends (e.g. Bedrock → Anthropic) the container
    // is recreated *from a snapshot image* committed off the previous container,
    // and `docker commit` copies that container's full ENV into the image. So
    // any auth var set under the old backend (e.g. CLAUDE_CODE_USE_BEDROCK=1,
    // AWS_PROFILE, a model alias) survives in the image ENV and stays active
    // unless we explicitly override it at create time.
    //
    // This pass is about *staleness*, not secrecy. It fixes the container it is
    // building and does nothing to the image, so it is not — and never was —
    // a defence against a credential baked into a snapshot. That is
    // `commit_container_snapshot`'s job, via SECRET_ENV_KEYS.
    // Create-time env takes precedence over image ENV, so we set every managed
    // auth key the *current* backend did NOT set to an empty value, clearing the
    // stale baked-in one.
    const MANAGED_AUTH_KEYS: &[&str] = &[
        "CLAUDE_CODE_USE_BEDROCK",
        "AWS_REGION",
        "AWS_ACCESS_KEY_ID",
        "AWS_SECRET_ACCESS_KEY",
        "AWS_SESSION_TOKEN",
        "AWS_PROFILE",
        "AWS_BEARER_TOKEN_BEDROCK",
        "AWS_SSO_AUTH_REFRESH_CMD",
        "ANTHROPIC_BASE_URL",
        "ANTHROPIC_AUTH_TOKEN",
        "ANTHROPIC_MODEL",
        "DISABLE_PROMPT_CACHING",
        "ANTHROPIC_BEDROCK_SERVICE_TIER",
        // Switching from a custom-endpoint backend to Anthropic or Bedrock must
        // *clear* the aliases, not merely stop setting them: a stale
        // ANTHROPIC_DEFAULT_HAIKU_MODEL baked into the snapshot image would
        // keep pointing background calls at a model id the new backend has
        // never heard of.
        ANTHROPIC_DEFAULT_OPUS_MODEL,
        ANTHROPIC_DEFAULT_SONNET_MODEL,
        ANTHROPIC_DEFAULT_HAIKU_MODEL,
        ANTHROPIC_DEFAULT_FABLE_MODEL,
        // Revoking the shared token, opting a project out, or switching away
        // from the Anthropic backend must *clear* this, not merely stop setting
        // it — otherwise the value committed into the snapshot image keeps
        // authenticating the container with a credential the user removed.
        CLAUDE_OAUTH_TOKEN_ENV,
    ];
    // Same reasoning for the CA vars — `ca_env` already emits them empty
    // when no CA is configured, so this list is belt-and-braces for a snapshot
    // committed by a build that predates the feature.
    let managed_keys: Vec<&str> = MANAGED_AUTH_KEYS
        .iter()
        .copied()
        .chain(ca_certs::CA_ENV_KEYS.iter().copied())
        .collect();
    let already_set: std::collections::HashSet<String> =
        env.iter().map(|(k, _)| k.clone()).collect();
    for key in &managed_keys {
        if !already_set.contains(*key) {
            env.push((key.to_string(), String::new()));
        }
    }

    // Custom environment variables (global + per-project, project overrides global for same key)
    let merged_env = merge_custom_env_vars(global_custom_env_vars, &project.custom_env_vars);
    for env_var in &merged_env {
        let key = env_var.key.trim();
        if key.is_empty() {
            continue;
        }
        if is_reserved_env_key(key) {
            log::warn!("Skipping reserved env var: {}", key);
            continue;
        }
        env.push((key.to_string(), env_var.value.clone()));
    }
    let custom_env_fingerprint = compute_env_fingerprint(&merged_env);
    env.push(("TRIPLE_C_CUSTOM_ENV".to_string(), custom_env_fingerprint.to_string()));

    // Container timezone
    if let Some(tz) = timezone {
        if !tz.is_empty() {
            env.push(("TZ".to_string(), tz.to_string()));
        }
    }

    // Mission Control env var
    if project.mission_control_enabled {
        env.push(("MISSION_CONTROL_ENABLED".to_string(), "1".to_string()));
    }

    env.push(split_pair(&vpn_env_var(project.vpn_support_enabled)));

    // Permission mode — read by triple-c-task-runner for scheduled (headless)
    // Claude Code runs. Interactive terminals get the flags directly instead.
    env.push((
        "TRIPLE_C_PERMISSION_MODE".to_string(),
        project.effective_permission_mode().as_env_value().to_string(),
    ));

    // Claude instructions (global + per-project, plus port mapping info + scheduler docs)
    let combined_instructions = build_claude_instructions(
        global_claude_instructions,
        project.claude_instructions.as_deref(),
        &project.port_mappings,
        project.mission_control_enabled,
        project.sandbox_mode_enabled,
    );

    if let Some(ref instructions) = combined_instructions {
        env.push(("CLAUDE_INSTRUCTIONS".to_string(), instructions.to_string()));
    }

    // Claude Code settings (global + per-project merged)
    let merged_cc_settings = merge_claude_code_settings(
        global_claude_code_settings,
        project.claude_code_settings.as_ref(),
    );
    // Env-var-based settings, read directly by Claude Code. Extracted and unit
    // tested for the `vpn_host_config` reason: a container is created once, by
    // a very long function, and a variable emitted with the wrong value here is
    // invisible until someone wonders why a switch does nothing.
    env.extend(
        claude_code_env_vars(merged_cc_settings.as_ref())
            .iter()
            .map(|entry| split_pair(entry)),
    );

    // settings.json-based settings (applied by the entrypoint). Always emitted,
    // even with no `ClaudeCodeSettings` struct present: the payload asserts the
    // *whole* managed key set, so "no settings" still has to be stated over a
    // settings.json left behind on the config volume by a previous config.
    // Split so the payload is safe under an older entrypoint that has never
    // heard of the null-means-delete convention — see
    // `split_claude_code_settings_payload`.
    let (cc_settings_set, cc_settings_clear) = split_claude_code_settings_payload(
        &build_claude_code_settings_json(merged_cc_settings.as_ref(), project.sandbox_mode_enabled),
    );
    env.push(("CLAUDE_CODE_SETTINGS_JSON".to_string(), cc_settings_set.to_string()));
    env.push(("CLAUDE_CODE_SETTINGS_CLEAR".to_string(), cc_settings_clear.to_string()));

    env
}

#[cfg(test)]
mod tests {
    use super::super::ca_certs::ResolvedCa;
    use crate::models::*;
    use std::path::PathBuf;

    /// Every input the env block reads, owned, so a table row is one value.
    pub(crate) struct Case {
        pub name: &'static str,
        pub project: Project,
        pub global_aws: GlobalAwsSettings,
        pub global_ollama: GlobalOllamaSettings,
        pub global_llamacpp: GlobalLlamaCppSettings,
        pub global_openai: GlobalOpenAiCompatibleSettings,
        pub global_instructions: Option<&'static str>,
        pub global_env: Vec<EnvVar>,
        pub timezone: Option<&'static str>,
        pub global_cc: Option<ClaudeCodeSettings>,
        pub default_git_name: Option<&'static str>,
        pub default_git_email: Option<&'static str>,
        pub ca: Option<ResolvedCa>,
        pub shared_token: Option<(String, String)>,
    }

    fn case(name: &'static str, project: Project) -> Case {
        Case {
            name,
            project,
            global_aws: GlobalAwsSettings::default(),
            global_ollama: GlobalOllamaSettings::default(),
            global_llamacpp: GlobalLlamaCppSettings::default(),
            global_openai: GlobalOpenAiCompatibleSettings::default(),
            global_instructions: None,
            global_env: Vec::new(),
            timezone: None,
            global_cc: None,
            default_git_name: None,
            default_git_email: None,
            ca: None,
            shared_token: None,
        }
    }

    fn proj(backend: Backend) -> Project {
        let mut p = Project::new("demo".to_string(), Vec::new());
        p.backend = backend;
        p
    }

    fn bedrock(method: BedrockAuthMethod) -> BedrockConfig {
        BedrockConfig {
            auth_method: method,
            aws_region: "us-east-1".into(),
            aws_access_key_id: Some("AKIAFAKE".into()),
            aws_secret_access_key: Some("secretfake".into()),
            aws_session_token: None,
            aws_profile: Some("proj-profile".into()),
            aws_bearer_token: Some("bearerfake".into()),
            model_id: Some("anthropic.claude-x".into()),
            disable_prompt_caching: true,
            service_tier: Some(" priority ".into()),
        }
    }

    pub(crate) fn cases() -> Vec<Case> {
        let mut v = Vec::new();

        let mut c = case("anthropic_shared_token", proj(Backend::Anthropic));
        c.shared_token = Some(("sk-shared-token".into(), "v7".into()));
        v.push(c);

        v.push(case("anthropic_plain", proj(Backend::Anthropic)));

        let mut p = proj(Backend::Bedrock);
        p.bedrock_config = Some(bedrock(BedrockAuthMethod::StaticCredentials));
        let mut c = case("bedrock_static_keys", p);
        c.global_aws.default_model_id = Some("global-model".into());
        v.push(c);

        let mut p = proj(Backend::Bedrock);
        let mut b = bedrock(BedrockAuthMethod::Profile);
        b.aws_region = String::new();
        b.aws_profile = None;
        b.model_id = None;
        b.disable_prompt_caching = false;
        b.service_tier = Some("  ".into());
        p.bedrock_config = Some(b);
        let mut c = case("bedrock_profile_global_fallbacks", p);
        c.global_aws.aws_region = Some("eu-west-1".into());
        c.global_aws.aws_profile = Some("global-profile".into());
        c.global_aws.default_model_id = Some("global-model".into());
        v.push(c);

        let mut p = proj(Backend::Bedrock);
        p.bedrock_config = Some(bedrock(BedrockAuthMethod::BearerToken));
        v.push(case("bedrock_bearer", p));

        let mut p = proj(Backend::Ollama);
        p.ollama_config = Some(OllamaConfig {
            base_url: "http://ollama.local:11434".into(),
            model_id: Some("qwen3".into()),
            haiku_model_id: None,
        });
        let mut c = case("ollama", p);
        c.global_ollama.default_haiku_model_id = Some("tiny".into());
        v.push(c);

        let mut p = proj(Backend::LlamaCpp);
        p.llamacpp_config = Some(LlamaCppConfig {
            base_url: String::new(),
            model_id: None,
            haiku_model_id: Some("llama-small".into()),
        });
        let mut c = case("llamacpp_global_url", p);
        c.global_llamacpp.base_url = Some("http://llama:8080".into());
        c.global_llamacpp.default_model_id = Some("llama-big".into());
        v.push(c);

        let mut p = proj(Backend::OpenAiCompatible);
        p.openai_compatible_config = Some(OpenAiCompatibleConfig {
            base_url: "https://gw.example/v1".into(),
            api_key: Some("sk-gateway".into()),
            model_id: Some("gpt-x".into()),
            haiku_model_id: Some("gpt-mini".into()),
        });
        v.push(case("openai_compatible", p));

        let mut p = proj(Backend::Anthropic);
        p.custom_env_vars = vec![
            EnvVar { key: "FOO".into(), value: "project".into() },
            EnvVar { key: "  SPACED ".into(), value: "x y".into() },
            EnvVar { key: "".into(), value: "dropped".into() },
            EnvVar { key: "HOME".into(), value: "reserved".into() },
            EnvVar { key: "CLAUDE_CODE_OAUTH_TOKEN".into(), value: "reserved".into() },
        ];
        let mut c = case("custom_env", p);
        c.global_env = vec![
            EnvVar { key: "FOO".into(), value: "global".into() },
            EnvVar { key: "BAR".into(), value: "b".into() },
        ];
        v.push(c);

        let mut p = proj(Backend::Anthropic);
        p.claude_instructions = Some("project rules".into());
        p.mission_control_enabled = true;
        p.sandbox_mode_enabled = true;
        p.port_mappings = vec![PortMapping {
            host_port: 8080,
            container_port: 3000,
            protocol: "tcp".into(),
        }];
        let mut c = case("instructions_and_mission_control", p);
        c.global_instructions = Some("global rules\nline two");
        v.push(c);

        let mut p = proj(Backend::Anthropic);
        p.claude_code_settings = Some(ClaudeCodeSettings {
            effort: Some("high".into()),
            focus_mode: Some(false),
            ..Default::default()
        });
        let mut c = case("claude_code_settings", p);
        c.global_cc = Some(ClaudeCodeSettings {
            tui_mode: Some("fullscreen".into()),
            env_scrub: Some(true),
            prompt_caching_1h: Some(true),
            ..Default::default()
        });
        v.push(c);

        let mut p = proj(Backend::Anthropic);
        p.vpn_support_enabled = true;
        p.git_token = Some("ghp_faketoken".into());
        p.git_user_name = Some("Project User".into());
        let mut c = case("vpn_git_timezone", p);
        c.timezone = Some("Europe/Berlin");
        c.default_git_name = Some("Default Name");
        c.default_git_email = Some("default@example.com");
        v.push(c);

        let mut c = case("corporate_ca", proj(Backend::Anthropic));
        c.ca = Some(ResolvedCa {
            host_path: "/certs".into(),
            is_dir: true,
            mount_target: "/tmp/.host-ca".into(),
            cert_files: vec![PathBuf::from("/certs/a.crt")],
        });
        v.push(c);

        v
    }

    fn golden_path(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("src/docker/workspace_env_golden")
            .join(format!("{name}.txt"))
    }

    /// `merge_custom_env_vars` collects a `HashMap`, so the custom variables
    /// come out in a different order every run (before the extraction as
    /// well). Sort that one run — between the last managed blank and the
    /// fingerprint — so the rest of the order is still compared exactly.
    fn normalise(mut env: Vec<String>) -> Vec<String> {
        let end = env.iter().position(|e| e.starts_with("TRIPLE_C_CUSTOM_ENV=")).unwrap();
        let start = env.iter().position(|e| e.starts_with("CLAUDE_CODE_OAUTH_TOKEN=")).unwrap() + 1;
        env[start..end].sort();
        env
    }

    /// The pre-refactor block's output, recorded before the extraction (see
    /// `workspace_env_golden/`), must be reproduced exactly, order included.
    #[test]
    fn workspace_env_matches_what_create_container_built_before_the_extraction() {
        for c in cases() {
            let inputs = super::WorkspaceEnvInputs {
                global_aws: &c.global_aws,
                global_ollama: &c.global_ollama,
                global_llamacpp: &c.global_llamacpp,
                global_openai_compatible: &c.global_openai,
                global_claude_instructions: c.global_instructions,
                global_custom_env_vars: &c.global_env,
                timezone: c.timezone,
                global_claude_code_settings: c.global_cc.as_ref(),
                default_git_user_name: c.default_git_name,
                default_git_user_email: c.default_git_email,
                resolved_ca: c.ca.as_ref(),
                shared_claude_token: c.shared_token.as_ref().map(|(t, _)| t.as_str()),
            };
            let got: Vec<String> = super::workspace_env(&c.project, &inputs)
                .into_iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect();
            let want = std::fs::read_to_string(golden_path(c.name)).unwrap();
            let want: Vec<String> = want
                .lines()
                .filter(|l| *l != "[" && *l != "]")
                .map(|l| {
                    let l = l.trim().trim_end_matches(',');
                    serde_json::from_str::<String>(l).unwrap_or_else(|_| panic!("{l}"))
                })
                .collect();
            assert_eq!(normalise(got), normalise(want), "case {}", c.name);
        }
    }
}
