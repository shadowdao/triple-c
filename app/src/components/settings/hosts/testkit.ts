// Shared fixtures for the environments UI tests (not a test file itself).
import type {
  DeployProgress,
  DetectedEnvironment,
  EnvironmentInfo,
  EnvironmentScan,
  HostConnection,
  TofuEvent,
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
