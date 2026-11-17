import type { DetectedEnvironment, Project } from "../lib/types";
import { defaultAwsWorkspace } from "../lib/awsWorkspace";

/** A complete local project for component tests. */
export function localProject(over: Partial<Project> = {}): Project {
  return {
    id: "p1",
    name: "api-server",
    paths: [{ host_path: "/src/api", mount_name: "api" }],
    container_id: null,
    status: "stopped",
    backend: "anthropic",
    bedrock_config: null,
    ollama_config: null,
    llamacpp_config: null,
    openai_compatible_config: null,
    allow_docker_access: true,
    sandbox_mode_enabled: true,
    mission_control_enabled: false,
    auth_bridge_enabled: false,
    browser_view_enabled: false,
    vpn_support_enabled: true,
    use_shared_auth_token: true,
    full_permissions: false,
    permission_mode: null,
    ssh_key_path: null,
    ca_cert_path: null,
    git_token: null,
    git_user_name: null,
    git_user_email: null,
    custom_env_vars: [],
    port_mappings: [{ host_port: 3000, container_port: 3000, protocol: "tcp" }],
    claude_instructions: null,
    claude_code_settings: null,
    renamed_session_names: {},
    marketplace_installs: [],
    marketplace_disabled: [],
    host_id: null,
    aws_placement: null,
    aws: null,
    created_at: "2026-01-01T00:00:00Z",
    updated_at: "2026-01-01T00:00:00Z",
    ...over,
  };
}

/** The same project placed on an AWS connection `c1`. */
export function awsProject(over: Partial<Project> = {}): Project {
  return localProject({
    paths: [],
    host_id: "c1",
    aws_placement: { region: "eu-west-1", environment: "dev" },
    aws: {
      ...defaultAwsWorkspace(),
      repos: [{ url: "https://example.com/org/api.git", branch: null, folder: "api" }],
    },
    ...over,
  });
}

export function environment(
  name: string,
  state: DetectedEnvironment["state"]["state"],
  joined: boolean,
): DetectedEnvironment {
  return {
    name,
    template_version: 1,
    state: { state } as DetectedEnvironment["state"],
    actions: [],
    info: null,
    onboarding: joined ? { state: "joined", ec2: null } : { state: "not_joined" },
    onboarding_actions: [],
    onboarded_users: 1,
  };
}
