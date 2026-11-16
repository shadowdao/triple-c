import type {
  AwsWorkspaceConfig,
  DetectedEnvironment,
  GatewaySettings,
  LaunchType,
  Project,
  RepoSource,
  UnsavedWork,
} from "./types";

/** Why a Local-only option is off on an AWS project (P6). */
export const NOT_ON_AWS = "Not available on AWS (Fargate)";
/** Q6: an AWS project's port mappings bind loopback only (Docker's bind 0.0.0.0). */
export const AWS_PORTS_LOCALHOST_ONLY = "Reachable only from this machine (localhost)";
/** R4: the AWS host forces Claude Code's sandbox off until it is verified on Fargate. */
export const SANDBOX_OFF_ON_AWS = "Off on AWS for now";
export const EPHEMERAL_WARNING =
  "Claude login and history are lost when the workspace stops.";

export function isAwsProject(p: Pick<Project, "host_id">): boolean {
  return p.host_id != null;
}

/** Fargate CPU sizes (units of 1/1024 vCPU). */
export const FARGATE_CPUS = [256, 512, 1024, 2048, 4096, 8192, 16384] as const;

/** Mirrors Rust `fargate_memory_sizes`: every memory size (MiB) valid with
 *  `cpu`, smallest first. 256 CPU is irregular: 512, 1024 or 2048 only. */
export function memoryOptions(cpu: number): number[] {
  if (cpu === 256) return [512, 1024, 2048];
  const ranges: Record<number, [number, number, number]> = {
    512: [1024, 4096, 1024],
    1024: [2048, 8192, 1024],
    2048: [4096, 16384, 1024],
    4096: [8192, 30720, 1024],
    8192: [16384, 61440, 4096],
    16384: [32768, 122880, 8192],
  };
  const range = ranges[cpu];
  if (!range) return [];
  const [min, max, step] = range;
  const out: number[] = [];
  for (let m = min; m <= max; m += step) out.push(m);
  return out;
}

/** Mirrors Rust `describe_sizes`. */
function describeSizes(sizes: number[]): string {
  const last = sizes[sizes.length - 1];
  if (sizes.length <= 3) return `${sizes.slice(0, -1).join(", ")} or ${last} MiB`;
  return `${sizes[0]} to ${last} MiB in steps of ${sizes[1] - sizes[0]}`;
}

/** Null when `memory` MiB is valid with `cpu`; otherwise the reason (as Rust words it). */
export function memoryError(cpu: number, memory: number): string | null {
  const sizes = memoryOptions(cpu);
  if (sizes.length === 0) return `${cpu} is not a Fargate CPU size`;
  if (!sizes.includes(memory)) {
    return `${memory} MiB of memory is not valid with ${cpu} CPU units: use ${describeSizes(sizes)}`;
  }
  return null;
}

/** The valid memory size nearest `memory` for `cpu` (used when the CPU changes). */
export function nearestMemory(cpu: number, memory: number): number {
  const opts = memoryOptions(cpu);
  return opts.reduce((best, m) => (Math.abs(m - memory) < Math.abs(best - memory) ? m : best), opts[0]);
}

export function formatCpu(cpu: number): string {
  return `${cpu / 1024} vCPU`;
}

export function formatMemory(mib: number): string {
  return mib % 1024 === 0 ? `${mib / 1024} GiB` : `${mib} MiB`;
}

/** Mirrors Rust `AwsWorkspaceConfig::default()`. */
export function defaultAwsWorkspace(): AwsWorkspaceConfig {
  return {
    launch_type: "fargate",
    cpu: 1024,
    memory_mib: 4096,
    arch: "x86_64",
    storage: "hybrid",
    ephemeral_gib: 21,
    spot: false,
    idle_stop_minutes: null,
    repos: [],
    wip_push_on_stop: true,
    secrets_delivery: "secrets_manager",
    setup_script: null,
    gateway_sidecar: false,
    task_arn: null,
    access_point_id: null,
    task_definition_arn: null,
    workspace_access_point_id: null,
  };
}

/** The folder a clone of `url` would get: its last path part without `.git`,
 *  reduced to the characters Rust's folder rule allows. */
export function repoFolderFromUrl(url: string): string {
  const last = url.trim().replace(/\/+$/, "").split(/[/:]/).pop() ?? "";
  return last.replace(/\.git$/, "").replace(/[^A-Za-z0-9._-]/g, "-").slice(0, 64);
}

/** Rows the editors hold, minus blank ones; a branch left empty is the default branch. */
export function cleanRepos(rows: RepoSource[]): RepoSource[] {
  return rows
    .filter((r) => r.url.trim() || r.folder.trim() || (r.branch ?? "").trim())
    .map((r) => ({
      url: r.url.trim(),
      branch: (r.branch ?? "").trim() || null,
      folder: r.folder.trim() || repoFolderFromUrl(r.url),
    }));
}

/** Whether Stop must ask first (the one place this rule lives). */
export function stopGuardNeeded(work: UnsavedWork): boolean {
  return !work.checked || work.repos.some((r) => !r.cloning && (r.dirty || r.unpushed > 0));
}

/** An environment a project can be placed in: ready (or merely behind) and joined. */
export function environmentSelectable(env: DetectedEnvironment): boolean {
  const ready = env.state.state === "ready" || env.state.state === "update_available";
  return ready && env.onboarding?.state === "joined";
}

/** Whether an AWS project may have saved files on EFS (Hybrid now, or an
 *  access point left from when it was). Ephemeral-only projects have none. */
export function hasSavedFiles(p: Pick<Project, "aws">): boolean {
  const a = p.aws;
  return a?.storage === "hybrid" || a?.storage === "efs" || !!a?.access_point_id || !!a?.workspace_access_point_id;
}

/** Whether an AWS project may have `/workspace` files on EFS (EFS storage now,
 *  or its access point left from when it was). */
export function hasWorkspaceFiles(p: Pick<Project, "aws">): boolean {
  const a = p.aws;
  return a?.storage === "efs" || !!a?.workspace_access_point_id;
}

/** Shown with EFS storage selected. */
export const EFS_HINT = "Everything in /workspace survives a stop; git and builds are slower over NFS";

/** Copy-at-connect (phase 6 Q2): what it means, in HostSection. */
export const COPY_AT_CONNECT_HINT =
  "API keys, the Git token, custom environment variables and the SSH and CA files are not stored in AWS: Triple-C copies them into the running workspace each time it connects.";
export const COPY_AT_CONNECT_WARNINGS = [
  "Private repositories clone, and the setup script runs, after you connect (or after 5 minutes without).",
  "Scheduled jobs and the setup script never get the API keys or custom variables; Git and SSH work once the app has connected.",
  "Until the first connect the workspace has no corporate CA: behind a TLS-inspecting proxy, Claude Code's self-update may fail until then.",
  "SSH keys, a Git token or variables you remove from the project stay in a running workspace until it stops.",
  "The setup script is not run again when a later connect re-clones a repository that failed.",
];
/** Mirrors Rust `MAX_SETUP_SCRIPT_BYTES` (UTF-8 bytes). */
export const MAX_SETUP_SCRIPT_BYTES = 32_768;

/** Why `script` can't be saved, or null. Mirrors (and words like) Rust
 *  `validate_aws_workspace`'s setup-script checks. */
export function setupScriptProblem(script: string): string | null {
  const bytes = new TextEncoder().encode(script).length;
  if (bytes > MAX_SETUP_SCRIPT_BYTES) {
    return `The setup script is ${bytes} bytes; the limit is ${MAX_SETUP_SCRIPT_BYTES}`;
  }
  if (script.includes("\0")) return "The setup script must not contain NUL characters";
  return null;
}

/** What the setup script is, under its editor (phase 6 Q3). */
export const SETUP_SCRIPT_HINT =
  "Runs on every start, as claude, with bash -e in /workspace once the repositories are cloned. It is stopped after 30 minutes, and its output shows in the Overview's start-up card. claude can use sudo (apt-get install …).";
export const SETUP_SCRIPT_COPY_AT_CONNECT_NOTE =
  "With credentials copied when you connect, the setup script waits for the app's first connect (at most 5 minutes) and never gets the API keys or custom variables.";

/** Hybrid/EFS under copy-at-connect (ruling R6): ~/.ssh stays off EFS. */
export const COPY_AT_CONNECT_SSH_NOTE =
  "~/.ssh is not kept on EFS: SSH keys copied in, or made in the workspace, are gone when it stops. Keys an earlier start left in /mnt/triple-c-home/.ssh stay there until you remove them.";
export const COPY_AT_CONNECT_GATEWAY_NOTE =
  "The LLM gateway's keys are still stored in AWS: the gateway starts before Triple-C connects. So is Claude Code's token for it (ANTHROPIC_AUTH_TOKEN), which scheduled jobs need.";

/** The LLM gateway sidecar (phase 6 Q7), in ModelSection. */
export const GATEWAY_SIDECAR_LABEL = "Run Triple-C's LLM gateway in the workspace";
export const GATEWAY_SIDECAR_HINT =
  "Runs the gateway from Settings → Model Gateway (same models, provider and API key) next to the workspace, on its own localhost:4000. Claude Code uses it instead of the base URL and API key below; set the model to one of the gateway's model names.";
export const GATEWAY_SIDECAR_ON_NOTE =
  "Claude Code talks to the gateway in the workspace (http://127.0.0.1:4000) with a key made for each start. The provider API key is stored in the project's AWS secret, whichever way credentials are delivered.";
/** Mirrors Rust `validate_aws_workspace`'s sidecar minimum. */
export const GATEWAY_SIDECAR_MIN_CPU = 1024;
export const GATEWAY_SIDECAR_MIN_MEMORY_MIB = 2048;

/**
 * Why `project` can't turn the gateway sidecar on, or null. Mirrors Rust
 * `gateway_sidecar_problem` (OpenAI-compatible backend, a complete gateway
 * model) plus the 1 vCPU / 2 GiB minimum.
 */
export function gatewaySidecarProblem(
  project: Pick<Project, "backend" | "aws">,
  gateway: Pick<GatewaySettings, "models"> | null,
): string | null {
  if (project.aws?.launch_type === "ec2") return EC2_NO_SIDECAR;
  if (project.backend !== "open_ai_compatible") {
    return "The LLM gateway sidecar needs the OpenAI Compatible backend.";
  }
  const models = (gateway?.models ?? []).filter((m) => m.name.trim() && m.model_id.trim());
  if (models.length === 0) {
    return "Add at least one model in Settings → Model Gateway first (a name and a provider model id).";
  }
  const aws = project.aws;
  if (aws && (aws.cpu < GATEWAY_SIDECAR_MIN_CPU || aws.memory_mib < GATEWAY_SIDECAR_MIN_MEMORY_MIB)) {
    return "The LLM gateway sidecar needs at least 1 vCPU and 2 GiB: raise them in the Host section.";
  }
  return null;
}
export const SECRETS_MANAGER_HINT =
  "API keys, the Git token, custom environment variables and the SSH and CA files are kept in the project's Secrets Manager secret, so the workspace has them from the moment it starts.";

/** Hosts that only mean something on this machine or its Docker bridge. */
const LOCAL_HOSTS = /^(localhost|127(\.\d{1,3}){3}|0\.0\.0\.0|\[?::1\]?|host\.docker\.internal|172\.17\.0\.1)$/i;

/**
 * Why `url` cannot work from an AWS task, or null: a loopback / Docker-host
 * address, or the app's own LLM gateway (it runs on this machine).
 */
export function localEndpointProblem(url: string, gatewayPort: number | null): string | null {
  let parsed: URL;
  try {
    parsed = new URL(url.trim());
  } catch {
    return null;
  }
  const host = parsed.hostname;
  const port = Number(parsed.port || (parsed.protocol === "https:" ? 443 : 80));
  if (LOCAL_HOSTS.test(host)) {
    if (gatewayPort !== null && port === gatewayPort) {
      return "This is Triple-C's own LLM gateway, which runs on this machine; an AWS workspace can't reach it.";
    }
    return `${host} is this machine (or its Docker host) — an AWS workspace can't reach it.`;
  }
  return null;
}

/** What an EC2 instance leaves for a workspace's reservations (Rust `ec2_available`). */
export interface Ec2Limits {
  cpu: number;
  memory_mib: number;
}

/** The largest Fargate CPU size at most `cpu` (the smallest when below them all). */
function fargateCpuAtMost(cpu: number): number {
  return [...FARGATE_CPUS].reverse().find((c) => c <= cpu) ?? FARGATE_CPUS[0];
}

/**
 * The save patch for switching an AWS project's launch type — the one place
 * the switch is made. Each side keeps only what Rust accepts there:
 * - to Fargate: Docker access and VPN off (they need EC2), and the
 *   reservations snapped to a Fargate CPU/memory pair;
 * - to EC2: the gateway sidecar and Spot off (neither is available on EC2),
 *   and the reservations lowered to what the instance leaves (`limits`, when
 *   known).
 */
export function launchTypePatch(
  project: Project,
  launch_type: LaunchType,
  limits: Ec2Limits | null = null,
): Partial<Project> {
  const aws: AwsWorkspaceConfig = { ...project.aws!, launch_type };
  const patch: Partial<Project> = { aws };
  if (launch_type === "ec2") {
    aws.gateway_sidecar = false;
    aws.spot = false;
    if (limits) {
      aws.cpu = Math.min(aws.cpu, limits.cpu);
      aws.memory_mib = Math.min(aws.memory_mib, limits.memory_mib);
    }
  } else {
    patch.allow_docker_access = false;
    patch.vpn_support_enabled = false;
    aws.cpu = fargateCpuAtMost(aws.cpu);
    if (memoryError(aws.cpu, aws.memory_mib)) aws.memory_mib = nearestMemory(aws.cpu, aws.memory_mib);
  }
  return patch;
}

/** Mirrors Rust `aws::workspace::EC2_NO_SIDECAR`. */
export const EC2_NO_SIDECAR =
  "The LLM gateway sidecar is not available on EC2 yet: turn it off, or run the project on Fargate";

/** Host settings' notes for an EC2 project (phase 7). */
export const EC2_SCALE_TO_ZERO_NOTE =
  "Your instances scale to zero when no workspace is running, so the first start after idle boots an instance — about 3 minutes.";
export const EC2_DOCKER_ACCESS_NOTE =
  "Docker access mounts your instance's Docker socket: that is root on your own instance, which nobody else's workspace runs on.";
export const EC2_NO_CAPACITY = "Ask an admin to add EC2 capacity to your onboarding";
