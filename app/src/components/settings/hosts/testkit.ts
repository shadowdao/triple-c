// Shared fixtures for the environments UI tests (not a test file itself).
import type {
  DeployProgress,
  DetectedEnvironment,
  Ec2InstanceTable,
  EnvironmentInfo,
  EnvironmentScan,
  HostConnection,
  TofuEvent,
  UserCompute,
} from "../../../lib/types";

export const conn: HostConnection = {
  id: "h1",
  name: "Prod",
  kind: { type: "aws", auth: { method: "profile", name: "prod-sso" }, default_region: "eu-west-1" },
};

export function scan(over: Partial<EnvironmentScan> = {}): EnvironmentScan {
  return {
    region: "eu-west-1",
    account: "123456789012",
    owner_id: "AIDAEXAMPLEEXAMPLE01",
    owner_key: "abc123abc123",
    state_bucket: "yes",
    boundary: "yes",
    environments: [],
    ...over,
  };
}

export function info(name = "dev"): EnvironmentInfo {
  return {
    schema: 1,
    name,
    template_version: 1,
    cluster_arn: `arn:aws:ecs:eu-west-1:123456789012:cluster/triple-c-${name}`,
    security_group_id: "sg-0123abcd",
    subnet_ids: ["subnet-0123abcd", "subnet-4567abcd"],
    assign_public_ip: true,
    kms_key_arn: "arn:aws:kms:eu-west-1:123456789012:key/k",
    log_group: `/triple-c/${name}`,
    network_mode: "public",
    tenancy: "strict",
    max_workspace_hours: 0,
  };
}

export function env(over: Partial<DetectedEnvironment> = {}): DetectedEnvironment {
  return {
    name: "dev",
    template_version: 1,
    state: { state: "ready" },
    actions: ["join"],
    info: info(over.name ?? "dev"),
    onboarding: { state: "not_joined" },
    onboarding_actions: [],
    onboarded_users: 0,
    ...over,
  };
}

export function tofuEvent(message: string, over: Partial<TofuEvent> = {}): TofuEvent {
  return { ts: "2026-11-14T10:00:00Z", level: "info", kind: "apply_progress", resource: null, action: null, message, ...over };
}

export function progress(over: Partial<DeployProgress>): DeployProgress {
  return { op_id: "op1", state_key: "environments/dev/terraform.tfstate", events: [], done: false, cancellable: true, ...over };
}

/** A slice of `list_ec2_instance_types`'s answer: x86 and ARM rows. */
export const instanceTable: Ec2InstanceTable = {
  default_type: "m7i.large",
  types: [
    { instance_type: "m7i.large", arch: "x86_64", vcpu: 2, memory_mib: 8192 },
    { instance_type: "m7i.xlarge", arch: "x86_64", vcpu: 4, memory_mib: 16384 },
    { instance_type: "m7g.large", arch: "arm64", vcpu: 2, memory_mib: 8192 },
    { instance_type: "m7g.xlarge", arch: "arm64", vcpu: 4, memory_mib: 16384 },
  ],
};

export function compute(over: Partial<UserCompute> = {}): UserCompute {
  const tag = over.user_tag ?? "alice";
  return {
    instance_type: "m7g.xlarge",
    max_instances: 3,
    user_tag: tag,
    cluster_arn: `arn:aws:ecs:eu-west-1:123456789012:cluster/triple-c-dev--u-${tag}`,
    capacity_provider: `triple-c-dev--u-${tag}`,
    ...over,
  };
}
