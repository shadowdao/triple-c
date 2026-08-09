export interface EnvVar {
  key: string;
  value: string;
}

export interface ProjectPath {
  host_path: string;
  mount_name: string;
}

export interface PortMapping {
  host_port: number;
  container_port: number;
  protocol: string;
}

export interface Project {
  id: string;
  name: string;
  paths: ProjectPath[];
  container_id: string | null;
  status: ProjectStatus;
  backend: Backend;
  bedrock_config: BedrockConfig | null;
  ollama_config: OllamaConfig | null;
  llamacpp_config: LlamaCppConfig | null;
  openai_compatible_config: OpenAiCompatibleConfig | null;
  allow_docker_access: boolean;
  sandbox_mode_enabled: boolean;
  mission_control_enabled: boolean;
  /** Mirror container loopback listeners onto host loopback so in-container
   *  browser OAuth logins can complete. Host-side only — no container recreate. */
  auth_bridge_enabled: boolean;
  /** Opt in to the browser-view pane. Host-side only, like `auth_bridge_enabled`. */
  browser_view_enabled: boolean;
  /** Use the shared long-lived Claude Code token (from `claude setup-token`,
   *  held in the OS keychain) instead of this project's own `claude login`.
   *  Defaults to true; only applies when `backend` is "anthropic" and a token
   *  has actually been stored. */
  use_shared_auth_token: boolean;
  /** Legacy binary permission flag; superseded by `permission_mode`, kept for
   *  existing projects.json data. */
  full_permissions: boolean;
  /** null = not set → falls back to `full_permissions` (true → "bypass"). */
  permission_mode: PermissionMode | null;
  ssh_key_path: string | null;
  git_token: string | null;
  git_user_name: string | null;
  git_user_email: string | null;
  custom_env_vars: EnvVar[];
  port_mappings: PortMapping[];
  claude_instructions: string | null;
  claude_code_settings: ClaudeCodeSettings | null;
  renamed_session_names: Record<string, string>;
  created_at: string;
  updated_at: string;
}

export type ProjectStatus =
  | "stopped"
  | "starting"
  | "running"
  | "stopping"
  | "error";

export type Backend =
  | "anthropic"
  | "bedrock"
  | "ollama"
  | "llama_cpp"
  | "open_ai_compatible";

/** Backends that point Claude Code at a non-Anthropic endpoint via
 *  `ANTHROPIC_BASE_URL`. These get the `ANTHROPIC_DEFAULT_*_MODEL` aliases
 *  pinned to their configured model; Anthropic and Bedrock do not. Mirrors
 *  Rust `Backend::uses_custom_endpoint`. */
export const CUSTOM_ENDPOINT_BACKENDS: readonly Backend[] = [
  "ollama",
  "llama_cpp",
  "open_ai_compatible",
];

/** Mirrors Rust `PermissionMode` (serde camelCase). */
export type PermissionMode = "plan" | "default" | "acceptEdits" | "bypass";

export type BedrockAuthMethod = "static_credentials" | "profile" | "bearer_token";

export interface BedrockConfig {
  auth_method: BedrockAuthMethod;
  aws_region: string;
  aws_access_key_id: string | null;
  aws_secret_access_key: string | null;
  aws_session_token: string | null;
  aws_profile: string | null;
  aws_bearer_token: string | null;
  model_id: string | null;
  disable_prompt_caching: boolean;
  service_tier: string | null;
}

export interface OllamaConfig {
  base_url: string;
  model_id: string | null;
  /** Optional override for the model the `haiku` alias resolves to (the alias
   *  Claude Code uses for background work). Blank falls back to `model_id`. */
  haiku_model_id: string | null;
}

/** llama.cpp (`llama-server`) — it natively implements the Anthropic Messages
 *  API at `POST /v1/messages`, so Claude Code talks to it directly. */
export interface LlamaCppConfig {
  base_url: string;
  model_id: string | null;
  /** See `OllamaConfig.haiku_model_id`. */
  haiku_model_id: string | null;
}

/** Despite the name (kept for existing project data), the endpoint must
 *  implement the **Anthropic** Messages API — e.g. LiteLLM. */
export interface OpenAiCompatibleConfig {
  base_url: string;
  api_key: string | null;
  model_id: string | null;
  /** See `OllamaConfig.haiku_model_id`. */
  haiku_model_id: string | null;
}

export interface ClaudeCodeSettings {
  tui_mode: string | null;
  effort: string | null;
  auto_scroll_disabled: boolean;
  focus_mode: boolean;
  show_thinking_summaries: boolean;
  enable_session_recap: boolean;
  env_scrub: boolean;
  prompt_caching_1h: boolean;
}

export interface ContainerInfo {
  container_id: string;
  project_id: string;
  status: string;
  image: string;
}

export interface SiblingContainer {
  id: string;
  names: string[] | null;
  image: string;
  state: string;
  status: string;
}

export interface TerminalSession {
  id: string;
  projectId: string;
  projectName: string;
  sessionType: "claude" | "bash";
  sessionName: string | null;
}

export type ImageSource = "registry" | "local_build" | "custom";

export interface GlobalAwsSettings {
  aws_config_path: string | null;
  aws_profile: string | null;
  aws_region: string | null;
  default_model_id: string | null;
}

export interface GlobalOllamaSettings {
  base_url: string | null;
  default_model_id: string | null;
  /** Global fallback for the `haiku` alias override; blank means "use the
   *  resolved model id". */
  default_haiku_model_id: string | null;
}

export interface GlobalLlamaCppSettings {
  base_url: string | null;
  default_model_id: string | null;
  default_haiku_model_id: string | null;
}

export interface GlobalOpenAiCompatibleSettings {
  base_url: string | null;
  default_model_id: string | null;
  default_haiku_model_id: string | null;
}

export interface AppSettings {
  default_ssh_key_path: string | null;
  default_git_user_name: string | null;
  default_git_user_email: string | null;
  docker_socket_path: string | null;
  image_source: ImageSource;
  custom_image_name: string | null;
  global_aws: GlobalAwsSettings;
  global_ollama: GlobalOllamaSettings;
  global_llamacpp: GlobalLlamaCppSettings;
  global_openai_compatible: GlobalOpenAiCompatibleSettings;
  global_claude_instructions: string | null;
  global_custom_env_vars: EnvVar[];
  auto_check_updates: boolean;
  dismissed_update_version: string | null;
  timezone: string | null;
  default_microphone: string | null;
  dismissed_image_digest: string | null;
  web_terminal: WebTerminalSettings;
  stt: SttSettings;
  gateway: GatewaySettings;
  global_claude_code_settings: ClaudeCodeSettings | null;
}

export interface SttSettings {
  enabled: boolean;
  model: string;
  port: number;
  language: string | null;
}

export interface SttStatus {
  container_exists: boolean;
  running: boolean;
  port: number;
  model: string;
  image_exists: boolean;
}

/** One entry of the gateway's LiteLLM `model_list`. */
export interface GatewayModel {
  /** Friendly name a project puts in its model field. */
  name: string;
  /** Provider-side model id, e.g. `gpt-5.1`. */
  model_id: string;
}

export interface GatewaySettings {
  enabled: boolean;
  port: number;
  /** LiteLLM provider prefix — `openai`, `azure`, `gemini`, … */
  provider: string;
  api_base: string | null;
  models: GatewayModel[];
}

export interface GatewayStatus {
  container_exists: boolean;
  running: boolean;
  port: number;
  image_exists: boolean;
  model_count: number;
  /** Presence only — the provider API key never leaves the keychain. */
  has_api_key: boolean;
  /** The value a project should use as its base URL. */
  base_url: string;
}

export interface WebTerminalSettings {
  enabled: boolean;
  port: number;
  access_token: string | null;
}

export interface WebTerminalInfo {
  running: boolean;
  port: number;
  access_token: string;
  local_ip: string | null;
  url: string | null;
}

export interface UpdateInfo {
  version: string;
  tag_name: string;
  release_url: string;
  body: string;
  assets: ReleaseAsset[];
  published_at: string;
}

export interface ReleaseAsset {
  name: string;
  browser_download_url: string;
  size: number;
}

export interface ImageUpdateInfo {
  remote_digest: string;
  local_digest: string | null;
  remote_updated_at: string | null;
}

export interface FileEntry {
  name: string;
  path: string;
  is_directory: boolean;
  size: number;
  modified: string;
  permissions: string;
}

export interface InstallOptions {
  os: "linux" | "macos" | "windows" | "unknown";
  product_name: string;
  can_auto_install: boolean;
  auto_install_method: string | null;
  auto_install_blocker: string | null;
  docs_url: string;
  manual_steps: string[];
  post_install_notes: string[];
}

// Container introspection (read-only) — see src-tauri/src/commands/inspect_commands.rs

/** A Claude Code session transcript stored on the container's config volume. */
export interface ClaudeSession {
  id: string;
  /** User-set display name (`claude -n <name>`), if any. */
  name: string | null;
  /** Claude's auto-generated title, else the session's last prompt. */
  summary: string | null;
  last_modified: string;
  size_bytes: number;
  message_count: number;
  cwd: string | null;
}

export type CapabilityScope = "user" | "project";

export interface CapabilityItem {
  name: string;
  description: string | null;
  scope: CapabilityScope;
}

export interface CapabilityGroup {
  count: number;
  items: CapabilityItem[];
}

export interface ContainerCapabilities {
  skills: CapabilityGroup;
  agents: CapabilityGroup;
  commands: CapabilityGroup;
  /** One item per hook event; `count` totals the individual handlers. */
  hooks: CapabilityGroup;
  plugins: CapabilityGroup;
  mcp_servers: CapabilityGroup;
}

export interface ScheduledTask {
  id: string;
  name: string;
  prompt: string;
  /** Cron expression (one-shot tasks are stored as cron too — see `at`). */
  schedule: string;
  task_type: "recurring" | "once";
  /** Original `--at` value (`"YYYY-MM-DD HH:MM"`) for one-shot tasks. */
  at: string | null;
  enabled: boolean;
  working_dir: string;
  created_at: string | null;
  last_run: string | null;
  /** Known only for enabled one-shot tasks; cron is not evaluated. */
  next_run: string | null;
}

/** Mirrors Rust `ScheduleKind` — which of the scheduler's two `add` flags to
 *  use: `--schedule "<cron>"` or `--at "YYYY-MM-DD HH:MM"`. */
export type ScheduleKind = "recurring" | "once";

/** The editable fields of a scheduled task, as `add`/`update` take them. */
export interface ScheduledTaskInput {
  name: string;
  prompt: string;
  scheduleKind: ScheduleKind;
  /** A cron expression when `scheduleKind` is `recurring`, otherwise the
   *  `YYYY-MM-DD HH:MM` one-shot time. */
  schedule: string;
  /** Absolute path inside the container; blank means `/workspace`. */
  workingDir: string;
}

export interface SchedulerNotification {
  task_id: string;
  task_name: string | null;
  status: string | null;
  time: string | null;
  task_type: string | null;
  summary: string | null;
  body: string;
  created_at: string;
}

// ── Auth bridge ──────────────────────────────────────────────────────────────

/** Which loopback family the container-side listener was found on.
 *  Mirrors Rust `PortFamily` (serde lowercase). */
export type AuthBridgePortFamily = "v4" | "v6" | "dual";

/** A container loopback port currently mirrored onto the host's loopback. */
export interface BridgedPort {
  port: number;
  family: AuthBridgePortFamily;
  /** RFC 3339 timestamp of when the host listener was bound. */
  bridged_at: string;
}

/** A discovered loopback listener that could not be bridged (host port taken). */
export interface PortConflict {
  port: number;
  reason: string;
}

export interface AuthBridgeStatus {
  enabled: boolean;
  active_ports: BridgedPort[];
  conflicts: PortConflict[];
}

/** Payload of the `auth-bridge-changed` event, emitted whenever the bridged
 *  port set or the conflict set changes for a project. */
export interface AuthBridgeChangedEvent {
  project_id: string;
  status: AuthBridgeStatus;
}

// ── Browser view ─────────────────────────────────────────────────────────────

/** What the container has, as reported by the in-container Playwright probe.
 *  Mirrors Rust `PlaywrightDetection`. */
export interface PlaywrightDetection {
  node_version: string | null;
  playwright_version: string | null;
  playwright_path: string | null;
  /** Whether the resolved Playwright declares the `browser.bind()` live-dashboard API. */
  has_bind: boolean;
  cli_version: string | null;
  cli_entry: string | null;
  /** Module roots the probe searched, echoed back for the "not found" message. */
  searched: string[];
}

/** Mirrors Rust `BrowserViewState` (serde snake_case). */
export type BrowserViewState = "off" | "running" | "unavailable";

export interface BrowserViewStatus {
  enabled: boolean;
  state: BrowserViewState;
  /** Token-bearing loopback URL for the pane's iframe. Never leaves the host. */
  url: string | null;
  host_port: number | null;
  container_port: number | null;
  started_at: string | null;
  detection: PlaywrightDetection | null;
  /** Why the view isn't running, and what to do about it. */
  message: string | null;
}

/** Payload of the `browser-view-changed` event. */
export interface BrowserViewChangedEvent {
  project_id: string;
  status: BrowserViewStatus;
}

/** Payload of the `claude-token-progress` event: milestones during
 *  `acquire_claude_token`. Never contains the token. */
export interface ClaudeTokenProgressEvent {
  project_id: string;
  message: string;
}

/** Payload of the `claude-token-output` event: output from
 *  `claude setup-token`, so the UI can show the URL the user must visit.
 *  Credentials are redacted backend-side before the event is emitted. */
export interface ClaudeTokenOutputEvent {
  project_id: string;
  chunk: string;
}
