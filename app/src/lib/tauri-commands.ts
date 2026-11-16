import { invoke } from "@tauri-apps/api/core";
import type { Project, ProjectPath, NewProjectHost, RemoveProjectOptions, WipChoice, UnsavedWork, ProjectRemovalReport, ProjectResetOutcome, ContainerInfo, AppSettings, SettingsImportPreview, SettingsImportOutcome, UpdateInfo, ImageUpdateInfo, FileEntry, FileContents, WebTerminalInfo, SttStatus, GatewayStatus, InstallOptions, ClaudeSession, ContainerCapabilities, ScheduledTask, ScheduledTaskInput, SchedulerNotification, AuthBridgeStatus, BrowserViewStatus, BrowserViewPopoutState, BrowserPageState, PlaywrightDetection, BrowserSetupOutcome, BrowserInstallTarget, ContainerStaleness, MigrationOptions, MigrationReport, MigrationState, ClearTokenOutcome, CaCertInfo, UploadOutcome, Note, ViewerFile, ViewerPoll, ViewerSaved, ViewerState, FileDiff, InstallScope, ItemUpdate, Marketplace, MarketplaceAccount, MarketplaceItemRef, MarketplaceSnapshot, ProjectSyncResult, SyncReport, HostConnection, CallerIdentity, PermissionReport, PolicyFile, Tier, EnvironmentScan, NetworkChoice, ModuleKind } from "./types";

// Docker
export const checkDocker = () => invoke<boolean>("check_docker");
export const checkImageExists = () => invoke<boolean>("check_image_exists");
export const buildImage = () => invoke<void>("build_image");
export const getContainerInfo = (projectId: string) =>
  invoke<ContainerInfo | null>("get_container_info", { projectId });

// Projects
export const listProjects = () => invoke<Project[]>("list_projects");
export const addProject = (name: string, paths: ProjectPath[], host?: NewProjectHost) =>
  invoke<Project>("add_project", {
    name,
    paths,
    hostId: host?.hostId ?? null,
    awsPlacement: host?.awsPlacement ?? null,
    aws: host?.aws ?? null,
  });
export const removeProject = (projectId: string, opts: RemoveProjectOptions = {}) =>
  invoke<ProjectRemovalReport>("remove_project", {
    projectId,
    discardData: opts.discardData ?? null,
    forgetAwsResources: opts.forgetAwsResources ?? null,
  });
export const updateProject = (project: Project) =>
  invoke<Project>("update_project", { project });
export const startProjectContainer = (projectId: string) =>
  invoke<Project>("start_project_container", { projectId });
/** `wip` is the stop guard's choice for an AWS project (a local one ignores it).
 *  Resolves to a note when the choice could not be recorded in the workspace
 *  (its own shutdown push may then still run), else null. */
export const stopProjectContainer = (projectId: string, wip?: WipChoice) =>
  invoke<string | null>("stop_project_container", { projectId, wip: wip ?? null });
export const checkUnsavedWork = (projectId: string) =>
  invoke<UnsavedWork>("check_unsaved_work", { projectId });
export const awsTaskLog = (projectId: string, limit: number) =>
  invoke<string[]>("aws_task_log", { projectId, limit });
export const removeAwsResources = (projectId: string, discardData: boolean) =>
  invoke<Project>("remove_aws_resources", { projectId, discardData });
export const rebuildProjectContainer = (projectId: string) =>
  invoke<ProjectResetOutcome>("rebuild_project_container", { projectId });
export const reconcileProjectStatuses = () =>
  invoke<Project[]>("reconcile_project_statuses");
/** AWS projects against ECS; run at startup whether or not Docker answers. */
export const reconcileAwsProjects = () => invoke<Project[]>("reconcile_aws_projects");

// Notes — per-project, host-side, readable with the container stopped.
export const listNotes = (projectId: string) =>
  invoke<Note[]>("list_notes", { projectId });
/** Insert or replace one note. `created_at` and `id` are owned by the backend. */
export const saveNote = (projectId: string, note: Note) =>
  invoke<Note>("save_note", { projectId, note });
export const deleteNote = (projectId: string, noteId: string) =>
  invoke<void>("delete_note", { projectId, noteId });

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

// AWS host connections. The commands returning `Promise<…>` below reject with an `AwsError`
// object (not a string); narrow a caught value with `toAwsError` from `./aws-error`.
export const addAwsProfileConnection = (name: string, profile: string, defaultRegion: string) =>
  invoke<HostConnection>("add_aws_profile_connection", { name, profile, defaultRegion });
export const addAwsKeysConnection = (
  name: string,
  accessKeyId: string,
  secretAccessKey: string,
  sessionToken: string | null,
  defaultRegion: string,
) =>
  invoke<HostConnection>("add_aws_keys_connection", {
    name,
    accessKeyId,
    secretAccessKey,
    sessionToken,
    defaultRegion,
  });
export const updateAwsConnection = (id: string, name: string, defaultRegion: string) =>
  invoke<HostConnection>("update_aws_connection", { id, name, defaultRegion });
/** Rejects with a string. Returns the settings after removal. */
export const removeHostConnection = (id: string) =>
  invoke<AppSettings>("remove_host_connection", { id });
export const awsConnectionIdentity = (id: string) =>
  invoke<CallerIdentity>("aws_connection_identity", { id });
/** Profile connections only; rejects with a string. */
export const awsConnectionSsoLogin = (id: string) =>
  invoke<void>("aws_connection_sso_login", { id });
/** `region` omitted: the connection's default region. */
export const checkAwsPermissions = (id: string, region?: string) =>
  invoke<PermissionReport>("check_aws_permissions", { id, region: region ?? null });
/** Every policy file `tier` needs, in order: attach all of them. */
export const awsPolicyDocument = (tier: Tier) =>
  invoke<PolicyFile[]>("aws_policy_document", { tier });

// AWS environments (OpenTofu). Reject with an `AwsError`. Starting commands (and follow) resolve
// to an op id; progress arrives on the `AWS_DEPLOY_PROGRESS_EVENT` event (`DeployProgress`, in
// ./types). Contract: subscribe to aws-deploy-progress BEFORE invoking; events carry `op_id` and
// `state_key`. The backend holds a run's first event until the command is about to return, so a
// listener registered before the invoke sees the whole run, `done` included. A re-follow (e.g.
// after a reload) replays the whole log under a new op id.
export const detectAwsEnvironments = (connectionId: string, region: string) =>
  invoke<EnvironmentScan>("detect_aws_environments", { connectionId, region });
export const deployAwsEnvironment = (
  connectionId: string,
  region: string,
  name: string,
  network: NetworkChoice,
  maxWorkspaceHours = 0,
) => invoke<string>("deploy_aws_environment", { connectionId, region, name, network, maxWorkspaceHours });
/** Omit `maxWorkspaceHours` to keep the environment's recorded cap. */
export const updateAwsEnvironment = (connectionId: string, region: string, name: string, maxWorkspaceHours?: number) =>
  invoke<string>(
    "update_aws_environment",
    maxWorkspaceHours === undefined
      ? { connectionId, region, name }
      : { connectionId, region, name, maxWorkspaceHours },
  );
/** `network` is required only when the environment has no record; a record's cap is kept unless `maxWorkspaceHours` is given. */
export const repairAwsEnvironment = (
  connectionId: string,
  region: string,
  name: string,
  network?: NetworkChoice,
  maxWorkspaceHours?: number,
) =>
  invoke<string>(
    "repair_aws_environment",
    maxWorkspaceHours === undefined
      ? { connectionId, region, name, network: network ?? null }
      : { connectionId, region, name, network: network ?? null, maxWorkspaceHours },
  );
export const destroyIncompleteAwsEnvironment = (connectionId: string, region: string, name: string) =>
  invoke<string>("destroy_incomplete_aws_environment", { connectionId, region, name });
export const unlockAwsState = (connectionId: string, region: string, stateKey: string) =>
  invoke<void>("unlock_aws_state", { connectionId, region, stateKey });
export const followAwsDeployment = (connectionId: string, region: string, stateKey: string) =>
  invoke<string>("follow_aws_deployment", { connectionId, region, stateKey });
export const cancelAwsDeployment = (connectionId: string, region: string, stateKey: string) =>
  invoke<void>("cancel_aws_deployment", { connectionId, region, stateKey });
/** Push `errored.tfstate` (a failed run's unsaved state) back to the bucket; resolves to an op id like a deploy. */
export const pushAwsSavedState = (connectionId: string, region: string, stateKey: string) =>
  invoke<string>("push_aws_saved_state", { connectionId, region, stateKey });
export const onboardAwsUser = (connectionId: string, region: string, environment: string) =>
  invoke<string>("onboard_aws_user", { connectionId, region, environment });
export const deployAwsPermissionsBoundary = (connectionId: string, region: string) =>
  invoke<string>("deploy_aws_permissions_boundary", { connectionId, region });
/** Opens a folder picker in Rust; resolves to the folder written, or null if dismissed. */
export const saveAwsAdminBundle = (
  connectionId: string,
  region: string,
  kind: ModuleKind,
  options: { environment?: string; ownerId?: string; network?: NetworkChoice; maxWorkspaceHours?: number } = {},
) =>
  invoke<string | null>("save_aws_admin_bundle", {
    connectionId,
    region,
    kind,
    environment: options.environment ?? null,
    ownerId: options.ownerId ?? null,
    network: options.network ?? null,
    ...(options.maxWorkspaceHours === undefined ? {} : { maxWorkspaceHours: options.maxWorkspaceHours }),
  });
/** Check a corporate CA path and report what would be installed. Never
 *  rejects for a bad path — the reason comes back in `error`. */
export const inspectCaCertPath = (path: string) =>
  invoke<CaCertInfo>("inspect_ca_cert_path", { path });
export const detectHostTimezone = () =>
  invoke<string>("detect_host_timezone");

// Settings export/import — `false`/`null` mean the save/open dialog was
// dismissed, not an error.
export const exportSettings = (password: string) =>
  invoke<boolean>("export_settings", { password });
export const previewSettingsImport = (password: string) =>
  invoke<SettingsImportPreview | null>("preview_settings_import", { password });
export const applySettingsImport = (password: string) =>
  invoke<SettingsImportOutcome>("apply_settings_import", { password });

// AWS
export const awsSsoRefresh = (projectId: string) =>
  invoke<void>("aws_sso_refresh", { projectId });

// Terminal
export const openTerminalSession = (
  projectId: string,
  sessionId: string,
  sessionType?: string,
  sessionName?: string,
  resumeSessionId?: string,
) =>
  invoke<void>("open_terminal_session", { projectId, sessionId, sessionType, sessionName, resumeSessionId });
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
/**
 * Save one container file to the host.
 *
 * The **backend** opens the save dialog, so this call cannot name a place on
 * the host — that is the point (see `pick_save_path` in `file_commands.rs`).
 * Paths do come *back* inside error text; what is closed is the inbound
 * direction.
 * Resolves to the number of bytes written, or `null` if the user dismissed the
 * dialog. Zero bytes is a success: an empty file is a file.
 */
export const downloadContainerFile = (projectId: string, containerPath: string) =>
  invoke<number | null>("download_container_file", { projectId, containerPath });
/**
 * Upload host files into `containerDir`, with the backend opening the file
 * picker. Resolves to `null` if the user dismissed it, otherwise to what
 * happened — one dialog can select several files and they need not all succeed.
 */
export const uploadFilesToContainer = (projectId: string, containerDir: string) =>
  invoke<UploadOutcome | null>("upload_files_to_container", { projectId, containerDir });
export const downloadContainerBackup = (projectId: string, hostPath: string, containerPath?: string) =>
  invoke<number>("download_container_backup", { projectId, hostPath, containerPath });
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
/** Revoke the shared token: delete the keychain entry **first**, then rewrite
 *  any snapshot image that still has it baked into its env — see
 *  `ClearTokenOutcome` for what may be left behind. Destructive; confirm it. */
export const clearClaudeToken = () =>
  invoke<ClearTokenOutcome>("clear_claude_token");
/** Rewrite snapshot images that still carry a credential, **without touching
 *  the keychain**. This is the retry behind an incomplete revocation, and the
 *  standalone "check my images" sweep; it never deletes a token. */
export const sweepClaudeTokenSnapshots = () =>
  invoke<ClearTokenOutcome>("sweep_claude_token_snapshots");

// Container base-image migration — move a project onto the current base image
// without deleting its volumes. Reset is the destructive alternative: it wipes
// ~/.claude, the OAuth credential, installed skills and every transcript.
//
// Flow: getContainerStaleness (~6s — two filesystem probes, so call it on demand
// rather than polling) → migrateProjectToBase → the project sits
// in "awaiting-confirmation" while the user tries it → confirmMigration or
// rollbackMigration.
//
// Rollback restores the **system layer only**. Both named volumes are untouched
// throughout, so anything written to $HOME during the migrated session — a new
// login, new skills, new transcripts — survives a rollback.
//
// Progress arrives on the existing `container-progress` event.

/**
 * Runs two container/image filesystem probes; not for polling.
 *
 * **Not read-only, despite only reporting.** When the container is *stopped*
 * the backend has to commit its writable layer to a throwaway image before it
 * can read anything — `docker exec` needs a running container — so this writes
 * (and then removes) an image. The result is cached per stop, so repeat calls
 * while the container stays stopped are cheap, but the first one after each stop
 * pays for a commit of the whole layer: seconds on a small project, tens of
 * seconds on a large one. Do not add a caller that fires more often than "the
 * container settled into a new state" without re-reading
 * `get_container_staleness`'s doc comment first.
 */
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

/** Open a URL in the user's own browser.
 *
 *  Replaces `openUrl` from `@tauri-apps/plugin-opener` at every call site. On
 *  Linux the app ships as an AppImage whose environment leaks into everything
 *  it spawns, which kills a *cold-launched* browser before it paints while
 *  `xdg-open` still exits 0 — so the plugin path reported success and did
 *  nothing (triple-c#34). The Rust side hands the child a repaired environment
 *  and re-validates the URL, which matters because these URLs originate in an
 *  untrusted container. macOS and Windows still reach the plugin, just from
 *  Rust, so there is no platform branch here.
 *
 *  Rejects with a string already phrased for a toast. */
export const openUrlExternal = (url: string) =>
  invoke<void>("open_url_external", { url });

// ---- Terminal file viewer ----

export const openFileViewer = (
  projectId: string,
  path: string,
  line?: number,
  col?: number,
  endLine?: number,
) => invoke<void>("open_file_viewer", { projectId, path, line, col, endLine });

export const viewerGetState = () => invoke<ViewerState>("viewer_get_state");
export const viewerReadFile = (maxBytes: number) =>
  invoke<ViewerFile>("viewer_read_file", { maxBytes });
export const viewerPollFile = () => invoke<ViewerPoll>("viewer_poll_file");
export const viewerWriteFile = (contentsBase64: string, baseHash: string) =>
  invoke<ViewerSaved>("viewer_write_file", { contentsBase64, baseHash });
export const viewerChooseFile = (index: number) =>
  invoke<ViewerState>("viewer_choose_file", { index });

// ---- Marketplace ----

export const listMarketplaceSnapshots = () =>
  invoke<MarketplaceSnapshot[]>("list_marketplace_snapshots");
export const refreshMarketplaces = (marketplaceId?: string) =>
  invoke<MarketplaceSnapshot[]>("refresh_marketplaces", { marketplaceId: marketplaceId ?? null });
export const addMarketplace = (
  name: string,
  url: string,
  branch: string | null,
  accountId: string | null,
) => invoke<MarketplaceSnapshot>("add_marketplace", { name, url, branch, accountId });
export const updateMarketplace = (marketplace: Marketplace) =>
  invoke<AppSettings>("update_marketplace", { marketplace });
export const removeMarketplace = (marketplaceId: string) =>
  invoke<AppSettings>("remove_marketplace", { marketplaceId });
/** `expectedCommit`: the head the user reviewed; the backend refuses if it moved. */
export const installMarketplaceItem = (item: MarketplaceItemRef, scope: InstallScope, expectedCommit: string) =>
  invoke<AppSettings>("install_marketplace_item", { item, scope, expectedCommit });
export const uninstallMarketplaceItem = (item: MarketplaceItemRef, scope: InstallScope) =>
  invoke<void>("uninstall_marketplace_item", { item, scope });
export const setGlobalItemDisabled = (
  projectId: string,
  item: MarketplaceItemRef,
  disabled: boolean,
) => invoke<Project>("set_global_item_disabled", { projectId, item, disabled });
export const forgetMarketplaceInstalls = (marketplaceId: string) =>
  invoke<void>("forget_marketplace_installs", { marketplaceId });
export const listMarketplaceUpdates = () => invoke<ItemUpdate[]>("list_marketplace_updates");
export const marketplaceItemDiff = (
  item: MarketplaceItemRef,
  fromCommit: string,
  toCommit: string,
) => invoke<FileDiff[]>("marketplace_item_diff", { item, fromCommit, toCommit });
/** `expectedCommit`: the head whose diff the user accepted; the backend refuses if it moved. */
export const updateMarketplaceItem = (item: MarketplaceItemRef, scope: InstallScope, expectedCommit: string) =>
  invoke<void>("update_marketplace_item", { item, scope, expectedCommit });
export const applyMarketplaceNow = (projectId?: string) =>
  invoke<ProjectSyncResult[]>("apply_marketplace_now", { projectId: projectId ?? null });
export const getMarketplaceSyncReport = (projectId: string) =>
  invoke<SyncReport | null>("get_marketplace_sync_report", { projectId });
export const addMarketplaceTokenAccount = (label: string, host: string, token: string) =>
  invoke<MarketplaceAccount>("add_marketplace_token_account", { label, host, token });
export const addMarketplaceGhHostAccount = (label: string, host: string) =>
  invoke<MarketplaceAccount>("add_marketplace_gh_host_account", { label, host });
export const startMarketplaceGhContainerLogin = (label: string, host: string, projectId: string) =>
  invoke<MarketplaceAccount>("start_marketplace_gh_container_login", { label, host, projectId });
export const cancelMarketplaceGhLogin = () => invoke<void>("cancel_marketplace_gh_login");
export const testMarketplaceAccount = (accountId: string) =>
  invoke<string>("test_marketplace_account", { accountId });
export const removeMarketplaceAccount = (accountId: string) =>
  invoke<AppSettings>("remove_marketplace_account", { accountId });
export const marketplaceGhHostAvailable = () => invoke<boolean>("marketplace_gh_host_available");
