import { invoke } from "@tauri-apps/api/core";
import type { Project, ProjectPath, ContainerInfo, SiblingContainer, AppSettings, UpdateInfo, ImageUpdateInfo, McpServer, FileEntry } from "./types";

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
export const openTerminalSession = (projectId: string, sessionId: string, sessionType?: string) =>
  invoke<void>("open_terminal_session", { projectId, sessionId, sessionType });
export const terminalInput = (sessionId: string, data: number[]) =>
  invoke<void>("terminal_input", { sessionId, data });
export const terminalResize = (sessionId: string, cols: number, rows: number) =>
  invoke<void>("terminal_resize", { sessionId, cols, rows });
export const closeTerminalSession = (sessionId: string) =>
  invoke<void>("close_terminal_session", { sessionId });
export const pasteImageToTerminal = (sessionId: string, imageData: number[]) =>
  invoke<string>("paste_image_to_terminal", { sessionId, imageData });
export const startAudioBridge = (sessionId: string) =>
  invoke<void>("start_audio_bridge", { sessionId });
export const sendAudioData = (sessionId: string, data: number[]) =>
  invoke<void>("send_audio_data", { sessionId, data });
export const stopAudioBridge = (sessionId: string) =>
  invoke<void>("stop_audio_bridge", { sessionId });

// MCP Servers
export const listMcpServers = () => invoke<McpServer[]>("list_mcp_servers");
export const addMcpServer = (name: string) =>
  invoke<McpServer>("add_mcp_server", { name });
export const updateMcpServer = (server: McpServer) =>
  invoke<McpServer>("update_mcp_server", { server });
export const removeMcpServer = (serverId: string) =>
  invoke<void>("remove_mcp_server", { serverId });

// Files
export const listContainerFiles = (projectId: string, path: string) =>
  invoke<FileEntry[]>("list_container_files", { projectId, path });
export const downloadContainerFile = (projectId: string, containerPath: string, hostPath: string) =>
  invoke<void>("download_container_file", { projectId, containerPath, hostPath });
export const uploadFileToContainer = (projectId: string, hostPath: string, containerDir: string) =>
  invoke<void>("upload_file_to_container", { projectId, hostPath, containerDir });

// Updates
export const getAppVersion = () => invoke<string>("get_app_version");
export const checkForUpdates = () =>
  invoke<UpdateInfo | null>("check_for_updates");
export const checkImageUpdate = () =>
  invoke<ImageUpdateInfo | null>("check_image_update");
