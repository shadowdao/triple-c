import { invoke } from "@tauri-apps/api/core";
import type { Project, ContainerInfo, SiblingContainer } from "./types";

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
export const addProject = (name: string, path: string) =>
  invoke<Project>("add_project", { name, path });
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

// Settings
export const setApiKey = (key: string) =>
  invoke<void>("set_api_key", { key });
export const hasApiKey = () => invoke<boolean>("has_api_key");
export const deleteApiKey = () => invoke<void>("delete_api_key");

// Terminal
export const openTerminalSession = (projectId: string, sessionId: string) =>
  invoke<void>("open_terminal_session", { projectId, sessionId });
export const terminalInput = (sessionId: string, data: number[]) =>
  invoke<void>("terminal_input", { sessionId, data });
export const terminalResize = (sessionId: string, cols: number, rows: number) =>
  invoke<void>("terminal_resize", { sessionId, cols, rows });
export const closeTerminalSession = (sessionId: string) =>
  invoke<void>("close_terminal_session", { sessionId });
