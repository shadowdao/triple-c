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
  auth_mode: AuthMode;
  bedrock_config: BedrockConfig | null;
  allow_docker_access: boolean;
  mission_control_enabled: boolean;
  ssh_key_path: string | null;
  git_token: string | null;
  git_user_name: string | null;
  git_user_email: string | null;
  custom_env_vars: EnvVar[];
  port_mappings: PortMapping[];
  claude_instructions: string | null;
  enabled_mcp_servers: string[];
  created_at: string;
  updated_at: string;
}

export type ProjectStatus =
  | "stopped"
  | "starting"
  | "running"
  | "stopping"
  | "error";

export type AuthMode = "anthropic" | "bedrock";

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
}

export type ImageSource = "registry" | "local_build" | "custom";

export interface GlobalAwsSettings {
  aws_config_path: string | null;
  aws_profile: string | null;
  aws_region: string | null;
}

export interface AppSettings {
  default_ssh_key_path: string | null;
  default_git_user_name: string | null;
  default_git_user_email: string | null;
  docker_socket_path: string | null;
  image_source: ImageSource;
  custom_image_name: string | null;
  global_aws: GlobalAwsSettings;
  global_claude_instructions: string | null;
  global_custom_env_vars: EnvVar[];
  auto_check_updates: boolean;
  dismissed_update_version: string | null;
  timezone: string | null;
  default_microphone: string | null;
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

export type McpTransportType = "stdio" | "http";

export interface McpServer {
  id: string;
  name: string;
  transport_type: McpTransportType;
  command: string | null;
  args: string[];
  env: Record<string, string>;
  url: string | null;
  headers: Record<string, string>;
  docker_image: string | null;
  container_port: number | null;
  created_at: string;
  updated_at: string;
}
