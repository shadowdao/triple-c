import type {
  AwsWorkspaceConfig,
  DetectedEnvironment,
  Project,
  RepoSource,
  UnsavedWork,
} from "./types";

/** Why a Local-only option is off on an AWS project (P6). */
export const NOT_ON_AWS = "Not available on AWS (Fargate)";
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
  "Private repositories clone after you connect.",
  "Scheduled jobs can't use these credentials while the app is closed.",
];
export const COPY_AT_CONNECT_GATEWAY_NOTE =
  "The LLM gateway's keys are still stored in AWS: the gateway starts before Triple-C connects.";
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
