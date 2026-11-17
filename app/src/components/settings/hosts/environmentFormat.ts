// Pure helpers shared by the environments UI. The name and id rules mirror
// `aws/tofu_modules.rs`; the server re-validates everything.
import type { Ec2Compute, EnvironmentState, LockInfo, NetworkChoice, UserCompute } from "../../../lib/types";
import type { StatusTone } from "../../ui/StatusIndicator";

export const COMMON_REGIONS = [
  "us-east-1", "us-east-2", "us-west-1", "us-west-2", "ca-central-1",
  "eu-west-1", "eu-west-2", "eu-west-3", "eu-central-1", "eu-north-1",
  "ap-southeast-1", "ap-southeast-2", "ap-northeast-1", "ap-northeast-2", "ap-south-1",
  "sa-east-1",
];

/** `validate_environment_name`: null when valid, else why not. */
export function environmentNameError(name: string): string | null {
  const ok =
    /^[a-z][a-z0-9-]{0,19}$/.test(name) && !name.endsWith("-") && !name.includes("--");
  return ok
    ? null
    : "Use 1 to 20 lowercase letters, digits and single hyphens, starting with a letter and not ending in a hyphen.";
}

/** `is_aws_id`: the prefix, then 8 or 17 lowercase hex characters. */
export function isAwsId(id: string, prefix: "vpc-" | "subnet-"): boolean {
  return new RegExp(`^${prefix}([0-9a-f]{8}|[0-9a-f]{17})$`).test(id);
}

/** The fields behind the network picker, kept as text while the user types. */
export interface NetworkDraft {
  mode: NetworkChoice["mode"];
  vpcId: string;
  subnet1: string;
  subnet2: string;
  assignPublicIp: boolean;
}

export const DEFAULT_NETWORK: NetworkDraft = {
  mode: "public",
  vpcId: "",
  subnet1: "",
  subnet2: "",
  assignPublicIp: false,
};

/** The draft as a `NetworkChoice`, or why it can't be one yet. */
export function networkFromDraft(d: NetworkDraft): { network: NetworkChoice } | { error: string } {
  if (d.mode === "public") return { network: { mode: "public" } };
  if (d.mode === "private-nat") return { network: { mode: "private-nat" } };
  const vpc = d.vpcId.trim();
  const subnets = [d.subnet1.trim(), d.subnet2.trim()];
  if (!isAwsId(vpc, "vpc-")) return { error: "Enter a VPC ID (vpc- and 8 or 17 hex characters)." };
  if (!subnets.every((s) => isAwsId(s, "subnet-"))) {
    return { error: "Enter two subnet IDs (subnet- and 8 or 17 hex characters), in different availability zones." };
  }
  if (subnets[0] === subnets[1]) return { error: "The two subnets must be different." };
  return { network: { mode: "existing", vpc_id: vpc, subnet_ids: subnets, assign_public_ip: d.assignPublicIp } };
}

export const environmentStateKey = (env: string) => `environments/${env}/terraform.tfstate`;
export const onboardingStateKey = (env: string, ownerKey: string) => `users/${env}/${ownerKey}/terraform.tfstate`;
export const BOUNDARY_STATE_KEY = "boundary/terraform.tfstate";

/** "45 s", "12 min", "3 h 5 min", "2 days". */
export function formatAge(secs: number): string {
  if (secs < 60) return `${Math.max(0, Math.floor(secs))} s`;
  const mins = Math.floor(secs / 60);
  if (mins < 60) return `${mins} min`;
  const hours = Math.floor(mins / 60);
  if (hours < 48) return mins % 60 ? `${hours} h ${mins % 60} min` : `${hours} h`;
  return `${Math.floor(hours / 24)} days`;
}

/** "Locked by <who> · <age> ago (<operation>)". */
export function lockLine(lock: LockInfo): string {
  const op = lock.operation ? ` (${lock.operation})` : "";
  return `Locked by ${lock.who || "unknown"} · ${formatAge(lock.age_secs)} ago${op}`;
}

export function stateStatus(s: EnvironmentState): { tone: StatusTone; label: string } {
  switch (s.state) {
    case "ready":
      return { tone: "ok", label: "Ready" };
    case "update_available":
      return { tone: "ok", label: "Update available" };
    case "deploying":
      return { tone: "busy", label: "Deploying" };
    case "incomplete":
      return { tone: "error", label: "Incomplete" };
    case "externally_managed":
      // Its state isn't in this region's Triple-C bucket (an admin bundle's
      // own bucket, by hand, or not readable here), so Update isn't offered.
      return { tone: "ok", label: "Managed outside this account's Triple-C state" };
    case "unreadable_record":
      return { tone: "error", label: "Unreadable record" };
  }
}

export function stateLock(s: EnvironmentState): LockInfo | null {
  return s.state === "deploying" || s.state === "incomplete" ? s.lock : null;
}

/** The most instances a user's EC2 capacity may run at once (`EC2_MAX_INSTANCES`). */
export const EC2_MAX_INSTANCES = 4;

/** The EC2 capacity picker's fields. An empty `instanceType` means the table's default. */
export interface Ec2Draft {
  enabled: boolean;
  instanceType: string;
  maxInstances: number;
  userTag: string;
}

/** The picker's start: the recorded capacity (a re-apply keeps it), or none. */
export function ec2DraftFrom(
  recorded: Pick<UserCompute, "instance_type" | "max_instances" | "user_tag"> | null | undefined,
): Ec2Draft {
  return recorded
    ? { enabled: true, instanceType: recorded.instance_type, maxInstances: recorded.max_instances, userTag: recorded.user_tag }
    : { enabled: false, instanceType: "", maxInstances: 1, userTag: "" };
}

/** `validate_ec2_user_tag`: null when valid, else why not. */
export function userTagError(tag: string): string | null {
  if (!tag) return "Enter your triple-c-user value.";
  const ok = /^[A-Za-z0-9-]{1,32}$/.test(tag) && !tag.startsWith("-") && !tag.endsWith("-") && !tag.includes("--");
  return ok ? null : "Use 1 to 32 letters, digits and single hyphens, not starting or ending with a hyphen.";
}

/** The capacity a draft asks for (null = none), or why it can't be used yet. */
export function ec2FromDraft(d: Ec2Draft): { compute: Ec2Compute | null } | { error: string } {
  if (!d.enabled) return { compute: null };
  if (!d.instanceType) return { error: "Choose an instance type." };
  if (!Number.isInteger(d.maxInstances) || d.maxInstances < 1 || d.maxInstances > EC2_MAX_INSTANCES) {
    return { error: `Choose 1 to ${EC2_MAX_INSTANCES} instances.` };
  }
  const tag = d.userTag.trim();
  const tagError = userTagError(tag);
  if (tagError) return { error: tagError };
  return { compute: { instance_type: d.instanceType, max_instances: d.maxInstances, user_tag: tag } };
}

/** "m7g.large, up to 3 instances (triple-c-user alice)". */
export function computeLine(c: UserCompute): string {
  const n = c.max_instances === 1 ? "1 instance" : `up to ${c.max_instances} instances`;
  return `${c.instance_type}, ${n} (triple-c-user ${c.user_tag})`;
}
