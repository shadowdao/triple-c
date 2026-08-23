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
  /** Grant NET_ADMIN, /dev/net/tun and the WireGuard `src_valid_mark` sysctl so
   *  a VPN client inside the container can build a tunnel. Unlike the two flags
   *  above this is container state — changing it recreates the container. */
  vpn_support_enabled: boolean;
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
  /** Per-project override for the corporate CA certificate path (a single
   *  certificate file or a directory of them). null falls back to
   *  `AppSettings.ca_cert_path`. Changing it recreates the container. */
  ca_cert_path: string | null;
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
  /** `null` = let Claude Code choose the renderer; `"default"` = classic, `"fullscreen"` = alt-screen. */
  tui_mode: string | null;
  /** `null` = unset, else `"low" | "medium" | "high" | "xhigh"`. Written as `effortLevel`. */
  effort: string | null;
  auto_scroll_disabled: boolean;
  /** Written as `viewMode: "focus"`. */
  focus_mode: boolean;
  show_thinking_summaries: boolean;
  /**
   * Turns the session recap **off**. Held in the disabled sense because Claude
   * Code's recap is on by default — see the Rust doc on `ClaudeCodeSettings`.
   */
  session_recap_disabled: boolean;
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
  /** Corporate root CA — a single certificate file or a directory of them —
   *  mounted read-only into every container and installed into the system
   *  trust store, Node's `NODE_EXTRA_CA_CERTS`, Python's
   *  `REQUESTS_CA_BUNDLE`/`SSL_CERT_FILE` and Chrome's NSS database.
   *  Needed when the host is behind a TLS-terminating corporate proxy. */
  ca_cert_path: string | null;
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

/** What `inspect_ca_cert_path` reports about a corporate CA path. Errors ride
 *  in the payload rather than rejecting, so the field can render them inline
 *  while the user is still typing. */
export interface CaCertInfo {
  exists: boolean;
  is_directory: boolean;
  cert_count: number;
  /** The `.crt` names the certificates are installed as inside the container —
   *  surfacing the silent `.pem` → `.crt` rename that
   *  `update-ca-certificates` requires. */
  installed_names: string[];
  error: string | null;
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
  /** A run is in flight right now (the runner's pid was verified live). */
  running: boolean;
  /** When that run started. Null unless `running`. */
  running_since: string | null;
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
  /** Set when only the IPv4 half of the host listener could be bound. The port
   *  is carrying traffic, but a client that resolves `localhost` to `::1` and
   *  does not fall back will still be refused — which otherwise presents as a
   *  login that hangs while the bridge reports itself healthy. */
  ipv6_warning: string | null;
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
  /** Playwright's own `cli.js`, which installs browsers and their apt libraries. */
  playwright_cli: string | null;
  /** Whether the resolved Playwright declares the `browser.bind()` live-dashboard API. */
  has_bind: boolean;
  cli_version: string | null;
  cli_entry: string | null;
  /** Browser bundles in `~/.cache/ms-playwright`, e.g. `chromium-1200`. Never `ffmpeg-*`. */
  browsers: string[];
  /** Path to Google Chrome when the `chrome` channel — what `@playwright/mcp`
   *  asks for — is installed. It is an apt package, so it is never in `browsers`. */
  chrome_channel: string | null;
  /** The Chromium the *viewer's* Playwright would launch, and whether it exists. */
  chromium_executable: string | null;
  chromium_executable_exists: boolean;
  /** What a script's `require("playwright")` resolves to — routinely a different
   *  copy, pinning a different browser revision. If its Chromium is missing,
   *  every script Claude writes fails while the pane still looks green. */
  script_playwright_version: string | null;
  script_chromium_executable: string | null;
  script_chromium_executable_exists: boolean;
  /** Module roots the probe searched, echoed back for the "not found" message.
   *  Includes the npx cache (`~/.npm/_npx/*​/node_modules`), which is where a
   *  Playwright installed through Claude Code's MCP setup actually lives. */
  searched: string[];
}

/** Result of an install action. Mirrors Rust `BrowserSetupOutcome`. */
export interface BrowserSetupOutcome {
  /** Fresh probe taken after the install, so the pane can update itself. */
  detection: PlaywrightDetection;
  /** Tail of the real npm/apt/Playwright output — shown instead of a generic message. */
  log: string;
  /** Whether a browser was actually started and closed. `null` when the step
   *  didn't try (the package step doesn't). */
  browser_launched: boolean | null;
  /** Something that didn't fail the action but the user still needs to know. */
  warning: string | null;
}

/** Browsers the pane can install. `chromium` is Playwright's own build;
 *  `chrome` is the Google Chrome channel `@playwright/mcp` asks for. */
export type BrowserInstallTarget = "chromium" | "chrome";

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

/**
 * Mirrors Rust `PopoutState` — read from the window, never remembered.
 *
 * The pane is unmounted whenever another Project Home sub-tab is selected while
 * the window carries on, so anything it holds in component state is stale by
 * the time the user comes back.
 */
export interface BrowserViewPopoutState {
  open: boolean;
  always_on_top: boolean;
}

/** Mirrors Rust `page::Viewport` — CSS pixels, clamped backend-side. */
export interface BrowserPageViewport {
  width: number;
  height: number;
}

/**
 * Mirrors Rust `page::PageState`: what the container-side helper reports about
 * the page Triple-C opened. `ready: false` with no error means there is none.
 */
export interface BrowserPageState {
  ready: boolean;
  url: string | null;
  viewport: BrowserPageViewport | null;
  error: string | null;
}

/**
 * Payload of the `browser-view-popout-changed` event: a `BrowserViewPopoutState`
 * plus the project it belongs to.
 *
 * The pop-out window can close without the pane asking — the user hits its X,
 * or the session tears down and takes it — so this is the only reliable way to
 * know whether it is on screen.
 */
export interface BrowserViewPopoutChangedEvent extends BrowserViewPopoutState {
  project_id: string;
}

/** Payload of the `claude-token-progress` event: milestones during
 *  `acquire_claude_token`. Never contains the token. */
/**
 * Result of `clear_claude_token`.
 *
 * Revoking is not one action but three: delete the keychain entry (always
 * succeeds or throws), let container recreation clear the env var, and rewrite
 * any snapshot image that still has the token baked into its `Config.Env`.
 * Only the last one can partly fail, and when it does the user has to be told
 * — a token sitting in an image is readable by `docker image inspect` for as
 * long as the image exists.
 */
export interface ClearTokenOutcome {
  /** Snapshot images that were holding the token and have been rewritten. */
  snapshots_scrubbed: string[];
  /** Images still holding it, each with the reason. Non-empty = incomplete. */
  snapshots_failed: string[];
  /** Rewritten, but the pre-rewrite image object could not be deleted because a
   *  container still runs off it. Clears itself when that container is
   *  recreated — worth mentioning, not worth alarming about. */
  snapshots_superseded: string[];
  /** Set when Docker could not be reached, so nothing is known. */
  docker_unavailable: string | null;
}

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

/** Payload of the `claude-token-link`: a sign-in URL taken from an OSC 8
 *  hyperlink parameter, which is the only place the CLI emits it whole — the
 *  visible text is sliced to the terminal width. **Untrusted**: it is container
 *  output, so it goes through `sanitizeRelayUrl` with the
 *  `ANTHROPIC_SIGN_IN_HOSTS` allowlist before it is shown or opened. */
export interface ClaudeTokenLinkEvent {
  project_id: string;
  url: string;
}

/** Payload of `claude-token-code-rejected`: `claude setup-token` refused the
 *  submitted code and is parked waiting for another one. The flow is still
 *  alive, so this is recoverable — `attempts_remaining` is how many more codes
 *  the backend will pass on before giving up. */
export interface ClaudeTokenCodeRejectedEvent {
  project_id: string;
  message: string;
  attempts_remaining: number;
}

// ── Container base-image migration ───────────────────────────────────────────
//
// A project's container is created from its own `triple-c-snapshot-<id>:latest`
// image and re-committed on every recreation, so it stays on the base image it
// was first built from forever. Migration moves it onto the *current* base
// **without touching either named volume** — unlike Reset, which deletes them
// and takes the login, skills and transcripts with it.
//
// Because `/home/claude` is a volume and the image's copy of it is masked after
// the first mount, almost nothing needs replaying: Claude Code itself, cargo,
// uv, ruff, `~/.claude.json`, the OAuth credential, skills, transcripts,
// scheduled tasks and SSH keys all re-attach for free. What is genuinely lost
// on an image swap is confined to the writable layer: root-level apt installs,
// `npm -g` packages, `/usr/local`, `/opt`, `/srv`, and non-bind-mounted
// `/workspace` content. Those are exactly what `MigrationOptions` replays.
//
// Mirrors Rust `models/migration.rs` (serde snake_case).

/** How a finished migration attempt ended. Mirrors Rust `MigrationPhase`. */
export type MigrationPhase = "succeeded" | "partial" | "failed" | "rolled_back";

/** One package that could not be replayed onto the new base. */
export interface PackageFailure {
  name: string;
  /** Tail of the package manager's own error output. */
  reason: string;
}

/** A data-bearing directory a migration destroys and cannot put back.
 *
 *  Service state lives under /var — a database's files in /var/lib/<service>,
 *  a site in /var/www — and none of it is carried across: replaying the apt
 *  delta reinstalls the *package* onto the new base and hands back an empty
 *  data directory. The ordinary recreate path does not have this problem
 *  because it creates from the project's own snapshot, so migration has to say
 *  so out loud before anything is touched. */
export interface UnpreservedData {
  /** Absolute path, e.g. `/var/lib/postgresql`. */
  path: string;
  /** Total size of the non-package files beneath it. */
  bytes: number;
  /** How many non-package files it holds. */
  file_count: number;
}

/** Why a project is worth migrating, and what migrating would carry across.
 *
 *  An empty array always means "nothing found", never "not checked" —
 *  `probe_error` is the single place a failed inspection is reported. */
export interface ContainerStaleness {
  /** The container's lineage is not the current base. Always false when
   *  `known` is false: an unknown lineage is not a claim of staleness. */
  stale: boolean;
  /** Whether the lineage could be established at all. False means the
   *  container predates the `triple-c.base-image-id` label — "unknown, probe
   *  instead", not "stale". */
  known: boolean;
  base_image_id: string | null;
  current_base_image_id: string | null;
  /** `Created` of the project's snapshot image, RFC 3339. */
  snapshot_created_at: string | null;
  /** Concrete paths the base ships and this container lacks, e.g. `/usr/bin/socat`. */
  missing_paths: string[];
  /** Human labels for the same, e.g. "Auth bridge tunnel (socat)". */
  missing_features: string[];
  /** apt packages the project added on top of the base; migration replays these. */
  apt_delta: string[];
  /** Global npm packages the base does not ship. */
  npm_global_delta: string[];
  /** Non-package paths under /usr/local, /opt, /srv and /workspace that would
   *  be carried across. Empty when nothing user-authored was found — which is
   *  the common case. */
  verbatim_paths: string[];
  /** Data under /var that the migration destroys and cannot restore. Empty on
   *  an ordinary container; when it is not, the pre-flight has to lead with it. */
  unpreserved_data: UnpreservedData[];
  /** dpkg packages the base carries at a different version. A drift measure,
   *  not a promise that every one is newer. */
  outdated_package_count: number;
  /** Set when the container/image could not be inspected; everything else is
   *  then at its default. */
  probe_error: string | null;
}

/** What a migration should replay. All default to false. */
export interface MigrationOptions {
  /** Replay the apt and `npm -g` deltas onto the new base. */
  replay_packages: boolean;
  /** Copy the verbatim payload (/usr/local, /opt, /srv, non-bind-mounted /workspace). */
  copy_paths: boolean;
  /** Keep the `:pre-migration-<ts>` rollback tag after the migration reports
   *  success, so it can still be undone. Costs roughly a whole snapshot on disk
   *  (3.8–12.3 GB on real projects) because snapshots share almost no layers
   *  with the current base. When false the tag is dropped as soon as the
   *  migration is known to have worked, and `rollback_available` is false. */
  keep_rollback: boolean;
}

/** The outcome of one migration attempt. */
export interface MigrationReport {
  phase: MigrationPhase;
  packages_requested: string[];
  packages_installed: string[];
  packages_failed: PackageFailure[];
  paths_copied: string[];
  /** Human labels for base features the container gained. */
  features_restored: string[];
  /** A `:pre-migration-<ts>` image still exists, so `rollbackMigration` works. */
  rollback_available: boolean;
  /** One paragraph fit to show the user verbatim. */
  message: string;
}

/** In-flight phases of `MigrationState.phase`. Distinct from `MigrationPhase`,
 *  which describes *outcomes*.
 *
 *  These are **hyphenated**, matching the `triple-c.migration-state=in-progress`
 *  container label so there is exactly one spelling in the system. Compare
 *  against the constants below rather than writing the literals — that is what
 *  they are for. */
export type MigrationStatePhase =
  | "in-progress"
  | "interrupted"
  | "awaiting-confirmation";

/** A migration is running right now. Poll `getMigrationState` until it changes. */
export const MIGRATION_PHASE_IN_PROGRESS = "in-progress";
/** The app died after the container was swapped. Offer resume (call
 *  `migrateProjectToBase` again — it picks the interrupted run up) or rollback. */
export const MIGRATION_PHASE_INTERRUPTED = "interrupted";
/** Finished; `report` is populated. Offer confirm or rollback. */
export const MIGRATION_PHASE_AWAITING_CONFIRMATION = "awaiting-confirmation";

/** What a migration decided to do, frozen at pre-flight time so a resume
 *  replays the same thing (the deltas cannot be recomputed after the swap). */
export interface MigrationPlan {
  apt_packages: string[];
  npm_packages: string[];
  verbatim_paths: string[];
  missing_paths: string[];
  /** What the pre-flight found under /var that the migration would destroy,
   *  frozen so the finished report can still name it. */
  unpreserved_data: UnpreservedData[];
}

/** Persisted host-side migration record. Present only while a migration is in
 *  flight or waiting for a decision; `confirmMigration` and `rollbackMigration`
 *  both clear it. */
export interface MigrationState {
  /** One of `MigrationStatePhase`; typed loosely because an unrecognised value
   *  from a future build must not crash the UI. */
  phase: string;
  from_image_id: string | null;
  to_base_id: string | null;
  started_at: string;
  report: MigrationReport | null;
  /** The `:pre-migration-<ts>` tag holding the old system layer, if kept. */
  rollback_image: string | null;
  /** Host path of the staged payload tar, while one exists. */
  staging_path: string | null;
  options: MigrationOptions;
  plan: MigrationPlan | null;
}
