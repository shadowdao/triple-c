export interface Project {
  id: string;
  name: string;
  path: string;
  container_id: string | null;
  status: ProjectStatus;
  auth_mode: AuthMode;
  allow_docker_access: boolean;
  ssh_key_path: string | null;
  git_token: string | null;
  git_user_name: string | null;
  git_user_email: string | null;
  created_at: string;
  updated_at: string;
}

export type ProjectStatus =
  | "stopped"
  | "starting"
  | "running"
  | "stopping"
  | "error";

export type AuthMode = "login" | "api_key";

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
}
