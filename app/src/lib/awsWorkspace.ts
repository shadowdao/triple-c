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

/** Mirrors Rust `fargate_memory_range`: `[min, max, step]` MiB for a CPU size. */
export function fargateMemoryRange(cpu: number): [number, number, number] | null {
  switch (cpu) {
    case 256:
      return [512, 2048, 512];
    case 512:
      return [1024, 4096, 1024];
    case 1024:
      return [2048, 8192, 1024];
    case 2048:
      return [4096, 16384, 1024];
    case 4096:
      return [8192, 30720, 1024];
    case 8192:
      return [16384, 61440, 4096];
    case 16384:
      return [32768, 122880, 8192];
    default:
      return null;
  }
}

/** Every memory size valid with `cpu`, smallest first. */
export function memoryOptions(cpu: number): number[] {
  const range = fargateMemoryRange(cpu);
  if (!range) return [];
  const [min, max, step] = range;
  const out: number[] = [];
  for (let m = min; m <= max; m += step) out.push(m);
  return out;
}

/** Null when `memory` MiB is valid with `cpu`; otherwise the reason (as Rust words it). */
export function memoryError(cpu: number, memory: number): string | null {
  const range = fargateMemoryRange(cpu);
  if (!range) return `${cpu} is not a Fargate CPU size`;
  const [min, max, step] = range;
  if (!Number.isInteger(memory) || memory < min || memory > max || (memory - min) % step !== 0) {
    return `${memory} MiB of memory is not valid with ${cpu} CPU units: use ${min} to ${max} MiB in steps of ${step}`;
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
    task_arn: null,
    access_point_id: null,
    task_definition_arn: null,
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

/** Whether Stop must ask first (mirrors Rust `needs_guard`). */
export function stopGuardNeeded(work: UnsavedWork): boolean {
  return !work.checked || work.repos.some((r) => r.dirty || r.unpushed > 0);
}

/** An environment a project can be placed in: ready (or merely behind) and joined. */
export function environmentSelectable(env: DetectedEnvironment): boolean {
  const ready = env.state.state === "ready" || env.state.state === "update_available";
  return ready && env.onboarding?.state === "joined";
}
