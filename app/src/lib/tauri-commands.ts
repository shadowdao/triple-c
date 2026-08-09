import { invoke } from "@tauri-apps/api/core";
import type { Project, ProjectPath, ContainerInfo, SiblingContainer, AppSettings, UpdateInfo, ImageUpdateInfo, FileEntry, WebTerminalInfo, SttStatus, InstallOptions, ClaudeSession, ContainerCapabilities, ScheduledTask, SchedulerNotification } from "./types";

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
export const uploadFileToContainer = (projectId: string, hostPath: string, containerDir: string) =>
  invoke<void>("upload_file_to_container", { projectId, hostPath, containerDir });

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
