import { invoke } from "@tauri-apps/api/core";
import type { Project, ProjectPath, ContainerInfo, SiblingContainer, AppSettings, UpdateInfo, ImageUpdateInfo, FileEntry, FileContents, WebTerminalInfo, SttStatus, GatewayStatus, InstallOptions, ClaudeSession, ContainerCapabilities, ScheduledTask, ScheduledTaskInput, SchedulerNotification, AuthBridgeStatus, BrowserViewStatus, BrowserViewPopoutState, BrowserPageState, PlaywrightDetection, BrowserSetupOutcome, BrowserInstallTarget, ContainerStaleness, MigrationOptions, MigrationReport, MigrationState, ClearTokenOutcome, CaCertInfo } from "./types";

// Docker
export const checkDocker = () => invoke<boolean>("check_docker");
export const checkImageExists = () => invoke<boolean>("check_image_exists");
export const buildImage = () => invoke<void>("build_image");
export const getContainerInfo = (projectId: string) =>
  invoke<ContainerInfo | null>("get_container_info", { projectId });
export const listSiblingContainers = () =>
  invoke<SiblingContainer[]>("list_sibling_containers");

// Projects
export const listProjects = () => invoke<Project[]>("list_projects");
export const addProject = (name: string, paths: ProjectPath[]) =>
  invoke<Project>("add_project", { name, paths });
export const removeProject = (projectId: string) =>
  invoke<void>("remove_project", { projectId });
export const updateProject = (project: Project) =>
  invoke<Project>("update_project", { project });
export const startProjectContainer = (projectId: string) =>
  invoke<Project>("start_project_container", { projectId });
export const stopProjectContainer = (projectId: string) =>
  invoke<void>("stop_project_container", { projectId });
export const rebuildProjectContainer = (projectId: string) =>
  invoke<Project>("rebuild_project_container", { projectId });
export const reconcileProjectStatuses = () =>
  invoke<Project[]>("reconcile_project_statuses");

// Settings
export const getSettings = () => invoke<AppSettings>("get_settings");
export const updateSettings = (settings: AppSettings) =>
  invoke<AppSettings>("update_settings", { settings });
export const pullImage = (imageName: string) =>
  invoke<void>("pull_image", { imageName });
export const detectAwsConfig = () =>
  invoke<string | null>("detect_aws_config");
export const listAwsProfiles = () =>
  invoke<string[]>("list_aws_profiles");
/** Check a corporate CA path and report what would be installed. Never
 *  rejects for a bad path — the reason comes back in `error`. */
export const inspectCaCertPath = (path: string) =>
  invoke<CaCertInfo>("inspect_ca_cert_path", { path });
export const detectHostTimezone = () =>
  invoke<string>("detect_host_timezone");

// AWS
export const awsSsoRefresh = (projectId: string) =>
  invoke<void>("aws_sso_refresh", { projectId });

// Terminal
export const openTerminalSession = (projectId: string, sessionId: string, sessionType?: string, sessionName?: string) =>
  invoke<void>("open_terminal_session", { projectId, sessionId, sessionType, sessionName });
export const terminalInput = (sessionId: string, data: number[]) =>
  invoke<void>("terminal_input", { sessionId, data });
export const terminalResize = (sessionId: string, cols: number, rows: number) =>
  invoke<void>("terminal_resize", { sessionId, cols, rows });
export const closeTerminalSession = (sessionId: string) =>
  invoke<void>("close_terminal_session", { sessionId });
export const pasteImageToTerminal = (sessionId: string, imageData: number[]) =>
  invoke<string>("paste_image_to_terminal", { sessionId, imageData });
export const uploadHostFileToTerminal = (sessionId: string, hostPath: string) =>
  invoke<string>("upload_host_file_to_terminal", { sessionId, hostPath });
export const startAudioBridge = (sessionId: string) =>
  invoke<void>("start_audio_bridge", { sessionId });
export const sendAudioData = (sessionId: string, data: number[]) =>
  invoke<void>("send_audio_data", { sessionId, data });
export const stopAudioBridge = (sessionId: string) =>
  invoke<void>("stop_audio_bridge", { sessionId });

// Files
export const listContainerFiles = (projectId: string, path: string) =>
  invoke<FileEntry[]>("list_container_files", { projectId, path });
export const downloadContainerFile = (projectId: string, containerPath: string, hostPath: string) =>
  invoke<void>("download_container_file", { projectId, containerPath, hostPath });
export const downloadContainerBackup = (projectId: string, hostPath: string, containerPath?: string) =>
  invoke<number>("download_container_backup", { projectId, hostPath, containerPath });
/**
 * Copy a host file into a container directory.
 *
 * `overwrite` is opt-in because a drop is aimed with a mouse: the backend
 * refuses by default when the name is already taken (see `lib/uploadErrors.ts`
 * for the marker that refusal carries), and the caller re-runs with `true`
 * only once the user has said "Replace" to that specific file. Leaving it off
 * is the safe default every existing caller gets.
 */
export const uploadFileToContainer = (
  projectId: string,
  hostPath: string,
  containerDir: string,
  overwrite?: boolean,
) => invoke<void>("upload_file_to_container", { projectId, hostPath, containerDir, overwrite });
export const readContainerFile = (projectId: string, path: string, maxBytes?: number) =>
  invoke<FileContents>("read_container_file", { projectId, path, maxBytes });
/** `toPath` is the new *name*, not a destination — renames never move. */
export const renameContainerPath = (projectId: string, fromPath: string, toPath: string) =>
  invoke<string>("rename_container_path", { projectId, fromPath, toPath });
export const createContainerDirectory = (projectId: string, parentPath: string, name: string) =>
  invoke<string>("create_container_directory", { projectId, parentPath, name });

// Updates
export const getAppVersion = () => invoke<string>("get_app_version");
export const checkForUpdates = () =>
  invoke<UpdateInfo | null>("check_for_updates");
export const checkImageUpdate = () =>
  invoke<ImageUpdateInfo | null>("check_image_update");

// Help
export const getHelpContent = () => invoke<string>("get_help_content");

// Web Terminal
export const startWebTerminal = () =>
  invoke<WebTerminalInfo>("start_web_terminal");
export const stopWebTerminal = () =>
  invoke<void>("stop_web_terminal");
export const getWebTerminalStatus = () =>
  invoke<WebTerminalInfo>("get_web_terminal_status");
export const regenerateWebTerminalToken = () =>
  invoke<WebTerminalInfo>("regenerate_web_terminal_token");

// STT
export const getSttStatus = () => invoke<SttStatus>("get_stt_status");
export const startStt = () => invoke<SttStatus>("start_stt");
export const stopStt = () => invoke<void>("stop_stt");
export const buildSttImage = () => invoke<void>("build_stt_image");
export const pullSttImage = () => invoke<void>("pull_stt_image");
export const transcribeAudio = (audioData: number[]) =>
  invoke<string>("transcribe_audio", { audioData });

// Model gateway (LiteLLM)
export const getGatewayStatus = () => invoke<GatewayStatus>("get_gateway_status");
export const startGateway = () => invoke<GatewayStatus>("start_gateway");
export const stopGateway = () => invoke<void>("stop_gateway");
export const checkGatewayHealth = () => invoke<boolean>("check_gateway_health");
export const buildGatewayImage = () => invoke<void>("build_gateway_image");
export const pullGatewayImage = () => invoke<void>("pull_gateway_image");
/** Write-only: the provider API key is never read back out of the keychain. */
export const setGatewayApiKey = (apiKey: string) =>
  invoke<void>("set_gateway_api_key", { apiKey });
export const clearGatewayApiKey = () => invoke<void>("clear_gateway_api_key");
export const getGatewayAuthToken = () => invoke<string>("get_gateway_auth_token");
export const regenerateGatewayAuthToken = () =>
  invoke<string>("regenerate_gateway_auth_token");

// Docker install helper
export const detectInstallOptions = () =>
  invoke<InstallOptions>("detect_install_options");
export const runDockerInstall = () => invoke<void>("run_docker_install");

// Container introspection — sessions
export const listClaudeSessions = (projectId: string) =>
  invoke<ClaudeSession[]>("list_claude_sessions", { projectId });
export const resumeSessionCommand = (projectId: string, sessionId: string) =>
  invoke<string>("resume_session_command", { projectId, sessionId });

// Container introspection — capabilities
export const listContainerCapabilities = (projectId: string) =>
  invoke<ContainerCapabilities>("list_container_capabilities", { projectId });

// Container introspection — scheduler
export const listScheduledTasks = (projectId: string) =>
  invoke<ScheduledTask[]>("list_scheduled_tasks", { projectId });
/** Returns the new task's id. */
export const addScheduledTask = (projectId: string, input: ScheduledTaskInput) =>
  invoke<string>("add_scheduled_task", { projectId, ...input });
/** Edit = add + remove, so this returns a *new* task id (see the Rust doc). */
export const updateScheduledTask = (
  projectId: string,
  taskId: string,
  input: ScheduledTaskInput,
  enabled: boolean,
) => invoke<string>("update_scheduled_task", { projectId, taskId, enabled, ...input });
export const getScheduledTaskLog = (projectId: string, taskId: string, tailLines?: number) =>
  invoke<string>("get_scheduled_task_log", { projectId, taskId, tailLines });
export const setScheduledTaskEnabled = (projectId: string, taskId: string, enabled: boolean) =>
  invoke<string>("set_scheduled_task_enabled", { projectId, taskId, enabled });
export const runScheduledTaskNow = (projectId: string, taskId: string) =>
  invoke<string>("run_scheduled_task_now", { projectId, taskId });
export const removeScheduledTask = (projectId: string, taskId: string) =>
  invoke<string>("remove_scheduled_task", { projectId, taskId });
export const getSchedulerNotifications = (projectId: string) =>
  invoke<SchedulerNotification[]>("get_scheduler_notifications", { projectId });
export const clearSchedulerNotifications = (projectId: string) =>
  invoke<void>("clear_scheduler_notifications", { projectId });

// Auth bridge — mirrors container loopback listeners onto host loopback so
// browser OAuth logins started inside the container can complete.
export const setAuthBridgeEnabled = (projectId: string, enabled: boolean) =>
  invoke<AuthBridgeStatus>("set_auth_bridge_enabled", { projectId, enabled });
export const getAuthBridgeStatus = (projectId: string) =>
  invoke<AuthBridgeStatus>("get_auth_bridge_status", { projectId });

// Browser view — watch and take over the browser Claude drives with Playwright
// inside the container. Off by default, per project. Enabling probes the
// container, starts the Playwright dashboard in it, and puts a token-gated
// listener on the host's loopback in front of it; the returned `url` is the
// only way in, and it is never reachable off the machine.
export const setBrowserViewEnabled = (projectId: string, enabled: boolean) =>
  invoke<BrowserViewStatus>("set_browser_view_enabled", { projectId, enabled });
export const getBrowserViewStatus = (projectId: string) =>
  invoke<BrowserViewStatus>("get_browser_view_status", { projectId });
/** Probe for Playwright without starting anything — used to re-check after installing it. */
export const checkBrowserViewSupport = (projectId: string) =>
  invoke<PlaywrightDetection>("check_browser_view_support", { projectId });
/**
 * Install `playwright` + `@playwright/cli` into the container's `/workspace`.
 *
 * A container mutation, so it only ever runs from an explicit click. Progress
 * streams on the existing `container-progress` event; the result carries a
 * fresh probe. Browsers are a separate action — see below.
 */
export const installBrowserViewSupport = (projectId: string) =>
  invoke<BrowserSetupOutcome>("install_browser_view_support", { projectId });
/**
 * Install a browser and the apt libraries it needs, then verify it launches.
 * `chromium` is Playwright's own build; `chrome` is the channel
 * `@playwright/mcp` asks for. Hundreds of MB — never call this implicitly.
 */
export const installBrowserViewBrowser = (
  projectId: string,
  browser: BrowserInstallTarget,
) => invoke<BrowserSetupOutcome>("install_browser_view_browser", { projectId, browser });

/**
 * Detach the live view into its own OS window, or raise it if already open.
 *
 * Window-only: the viewer, the proxy and the container are untouched, so
 * popping out and back costs nothing. The window loads the same token-bearing
 * loopback URL as the pane, and has no IPC access.
 */
export const openBrowserViewPopout = (projectId: string, alwaysOnTop: boolean) =>
  invoke<void>("open_browser_view_popout", { projectId, alwaysOnTop });
/** Close the pop-out and put the view back in the tab. No-op if it isn't open. */
export const closeBrowserViewPopout = (projectId: string) =>
  invoke<void>("close_browser_view_popout", { projectId });
/**
 * Whether the pop-out is open and whether it is pinned, read from the window.
 *
 * Asked on every mount: the pane is unmounted whenever another Project Home
 * sub-tab is selected, while the window carries on — so neither fact can live
 * in component state and survive.
 */
export const getBrowserViewPopoutState = (projectId: string) =>
  invoke<BrowserViewPopoutState>("get_browser_view_popout_state", { projectId });
/**
 * Open a URL in a browser *inside* the container, published so the pane shows it.
 *
 * The same action serves an auth URL — the OAuth callback listener is in the
 * container too, so the loop closes without the host — and a dev server on
 * container loopback, which is how you watch a UI Claude is building. Only
 * http/https; the backend rejects anything else.
 *
 * The viewer is started if it isn't already: asking for a page is asking to
 * watch it, and leaving the user to go and press Start themselves — with no
 * hint that they had to — is what the first version did.
 */
export const openPageInContainerBrowser = (
  projectId: string,
  url: string,
  width: number,
  height: number,
  /** Also raise the pop-out window — for callers with no pane on screen. */
  showWindow = false,
) =>
  invoke<BrowserPageState>("open_page_in_container_browser", {
    projectId,
    url,
    width,
    height,
    showWindow,
  });
/** Resize that page. Real reflow, not a scaled screencast — see BrowserTab. */
export const setContainerPageViewport = (projectId: string, width: number, height: number) =>
  invoke<void>("set_container_page_viewport", { projectId, width, height });
export const getContainerPageState = (projectId: string) =>
  invoke<BrowserPageState>("get_container_page_state", { projectId });
export const closeContainerPage = (projectId: string) =>
  invoke<void>("close_container_page", { projectId });

/**
 * Make the page track the pop-out window's size as it is dragged.
 *
 * Only affects a page this app opened: a bound browser admits no second client,
 * so one `@playwright/mcp` launched keeps the viewport it was given.
 */
export const setBrowserViewMatchWindow = (projectId: string, enabled: boolean) =>
  invoke<void>("set_browser_view_match_window", { projectId, enabled });
export const getBrowserViewMatchWindow = (projectId: string) =>
  invoke<boolean>("get_browser_view_match_window", { projectId });

/** Pin the pop-out above other windows — the point of popping it out at all. */
export const setBrowserViewPopoutAlwaysOnTop = (projectId: string, onTop: boolean) =>
  invoke<void>("set_browser_view_popout_always_on_top", { projectId, onTop });

// Shared Claude Code auth token — one `claude setup-token` run authenticates
// every Anthropic-backend project. The token itself is never exposed here: it
// lives in the OS keychain and is injected as a container env var.
//
// `acquireClaudeToken` borrows the given project's running container to run the
// login and streams progress on the `claude-token-progress` and
// `claude-token-output` events. It deliberately does *not* touch the project's
// auth bridge: `setup-token` finishes on an Anthropic-hosted page and pastes a
// code back, so there is no loopback callback for a bridge to carry — and an
// earlier version that enabled it "just in case" persisted that flag to
// projects.json and left it latched on whenever the flow was killed. See the
// module comment in `commands/auth_token_commands.rs`. It resolves only
// once the whole flow finishes, so call it without awaiting the UI on it.
//
// Partway through, `claude setup-token` prints a sign-in URL and then waits at
// a "Paste code here" prompt: the user signs in, copies the code shown by
// Anthropic, and it is delivered with `submitClaudeTokenCode`. One flow at a
// time — a second `acquireClaudeToken` call rejects while one is in progress.
export const acquireClaudeToken = (projectId: string) =>
  invoke<void>("acquire_claude_token", { projectId });
export const submitClaudeTokenCode = (code: string) =>
  invoke<void>("submit_claude_token_code", { code });
/** Abort an in-flight acquisition and release the single-flight guard. No-op if nothing is running. */
export const cancelClaudeToken = () => invoke<void>("cancel_claude_token");
export const hasClaudeToken = () => invoke<boolean>("has_claude_token");
/** Revoke the shared token. Also rewrites any snapshot image that still has it
 *  baked into its env — see `ClearTokenOutcome` for what may be left behind. */
export const clearClaudeToken = () =>
  invoke<ClearTokenOutcome>("clear_claude_token");

// Container base-image migration — move a project onto the current base image
// without deleting its volumes. Reset is the destructive alternative: it wipes
// ~/.claude, the OAuth credential, installed skills and every transcript.
//
// Flow: getContainerStaleness (read-only, ~6s — two filesystem probes, so call
// it on demand rather than polling) → migrateProjectToBase → the project sits
// in "awaiting-confirmation" while the user tries it → confirmMigration or
// rollbackMigration.
//
// Rollback restores the **system layer only**. Both named volumes are untouched
// throughout, so anything written to $HOME during the migrated session — a new
// login, new skills, new transcripts — survives a rollback.
//
// Progress arrives on the existing `container-progress` event.

/** Read-only. Runs two container/image filesystem probes; not for polling. */
export const getContainerStaleness = (projectId: string) =>
  invoke<ContainerStaleness>("get_container_staleness", { projectId });

/** Runs the whole migration and resolves with its report. Long-running — the
 *  apt replay alone was measured at ~70s for 8 packages. Calling it again while
 *  a migration is `interrupted` resumes that one instead of starting a new one. */
export const migrateProjectToBase = (projectId: string, options: MigrationOptions) =>
  invoke<MigrationReport>("migrate_project_to_base", { projectId, options });

/** Accept the migration: drops the rollback tag and the staged payload, and
 *  clears the record. Idempotent. */
export const confirmMigration = (projectId: string) =>
  invoke<void>("confirm_migration", { projectId });

/** Undo the migration: recreates the container from its pre-migration image.
 *  Fails if the migration kept no rollback image (`keep_rollback: false`). */
export const rollbackMigration = (projectId: string) =>
  invoke<void>("rollback_migration", { projectId });

/** The persisted record, or null when no migration is in flight. Worth calling
 *  after `reconcileProjectStatuses` at startup: a migration interrupted by an
 *  app crash shows up here as phase "interrupted". */
export const getMigrationState = (projectId: string) =>
  invoke<MigrationState | null>("get_migration_state", { projectId });
