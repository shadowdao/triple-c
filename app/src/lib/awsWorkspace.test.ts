import { describe, it, expect } from "vitest";
import {
  cleanRepos,
  defaultAwsWorkspace,
  hasSavedFiles,
  hasWorkspaceFiles,
  environmentSelectable,
  gatewaySidecarProblem,
  EC2_NO_SIDECAR,
  launchTypePatch,
  memoryError,
  memoryOptions,
  nearestMemory,
  repoFolderFromUrl,
  stopGuardNeeded,
} from "./awsWorkspace";
import { awsProject, environment } from "../test/awsFixtures";
// Shared with the Rust validator (`models/project.rs`).
import fargatePairs from "../test/fixtures/fargate-pairs.json";

describe("awsWorkspace", () => {
  it("agrees with the Rust validator on every pair in the shared fixture", () => {
    for (const [cpu, mem] of fargatePairs.valid) expect(memoryError(cpu, mem), `${cpu}/${mem}`).toBeNull();
    for (const [cpu, mem] of fargatePairs.invalid) expect(memoryError(cpu, mem), `${cpu}/${mem}`).not.toBeNull();
  });

  it("words the refusal as Rust does and snaps to a valid size", () => {
    expect(memoryOptions(256)).toEqual([512, 1024, 2048]);
    expect(memoryError(256, 1536)).toMatch(/use 512, 1024 or 2048 MiB$/);
    expect(memoryError(1024, 2500)).toMatch(/use 2048 to 8192 MiB in steps of 1024$/);
    expect(memoryError(300, 1024)).toMatch(/not a Fargate CPU/);
    expect(nearestMemory(256, 4096)).toBe(2048);
    expect(nearestMemory(16384, 4096)).toBe(32768);
  });

  it("derives a clone folder from a URL", () => {
    expect(repoFolderFromUrl("https://github.com/org/api.git")).toBe("api");
    expect(repoFolderFromUrl("git@github.com:org/web-app.git")).toBe("web-app");
    expect(repoFolderFromUrl("ssh://h/x/y z/")).toBe("y-z");
  });

  it("drops blank repository rows and keeps an empty branch as the default", () => {
    expect(
      cleanRepos([
        { url: " https://h/r.git ", branch: " ", folder: "" },
        { url: "", branch: null, folder: "" },
      ]),
    ).toEqual([{ url: "https://h/r.git", branch: null, folder: "r" }]);
  });

  it("guards a stop with unsaved or unknown work", () => {
    const repo = { folder: "a", branch: "main", dirty: false, unpushed: 0 };
    expect(stopGuardNeeded({ checked: true, repos: [repo], error: null })).toBe(false);
    expect(stopGuardNeeded({ checked: true, repos: [{ ...repo, dirty: true }], error: null })).toBe(true);
    expect(stopGuardNeeded({ checked: true, repos: [{ ...repo, unpushed: 1 }], error: null })).toBe(true);
    expect(stopGuardNeeded({ checked: false, repos: [], error: "x" })).toBe(true);
    // A repository still cloning has nothing to push.
    expect(stopGuardNeeded({ checked: true, repos: [{ ...repo, cloning: true }], error: null })).toBe(false);
  });

  it("selects only ready environments the user joined", () => {
    expect(environmentSelectable(environment("a", "ready", true))).toBe(true);
    expect(environmentSelectable(environment("a", "update_available", true))).toBe(true);
    expect(environmentSelectable(environment("a", "ready", false))).toBe(false);
    expect(environmentSelectable(environment("a", "deploying", true))).toBe(false);
  });
});

describe("phase 6 settings", () => {
  it("defaults mirror Rust", () => {
    const d = defaultAwsWorkspace();
    expect([d.secrets_delivery, d.setup_script, d.gateway_sidecar, d.workspace_access_point_id]).toEqual([
      "secrets_manager",
      null,
      false,
      null,
    ]);
  });
  it("EFS mode and a leftover workspace access point count as saved files", () => {
    const aws = defaultAwsWorkspace();
    expect(hasSavedFiles({ aws: { ...aws, storage: "efs" } })).toBe(true);
    expect(hasSavedFiles({ aws: { ...aws, storage: "ephemeral", workspace_access_point_id: "fsap-w" } })).toBe(true);
    expect(hasSavedFiles({ aws: { ...aws, storage: "ephemeral" } })).toBe(false);
  });

  it("knows when /workspace files may be on EFS", () => {
    const aws = defaultAwsWorkspace();
    expect(hasWorkspaceFiles({ aws: { ...aws, storage: "efs" } })).toBe(true);
    expect(hasWorkspaceFiles({ aws: { ...aws, storage: "hybrid", workspace_access_point_id: "fsap-w" } })).toBe(true);
    expect(hasWorkspaceFiles({ aws: { ...aws, storage: "hybrid" } })).toBe(false);
    expect(hasWorkspaceFiles({ aws: null })).toBe(false);
  });
});

describe("gatewaySidecarProblem", () => {
  const models = { models: [{ name: "gpt", model_id: "gpt-5" }] };
  const aws = defaultAwsWorkspace();
  it("needs the OpenAI-compatible backend, a complete model and 1 vCPU / 2 GiB", () => {
    expect(gatewaySidecarProblem({ backend: "open_ai_compatible", aws }, models)).toBeNull();
    expect(gatewaySidecarProblem({ backend: "anthropic", aws }, models)).toMatch(/OpenAI Compatible/);
    expect(gatewaySidecarProblem({ backend: "open_ai_compatible", aws }, null)).toMatch(/Model Gateway/);
    expect(
      gatewaySidecarProblem({ backend: "open_ai_compatible", aws }, { models: [{ name: "gpt", model_id: " " }] }),
    ).toMatch(/Model Gateway/);
    expect(
      gatewaySidecarProblem({ backend: "open_ai_compatible", aws: { ...aws, cpu: 512, memory_mib: 2048 } }, models),
    ).toMatch(/1 vCPU and 2 GiB/);
  });
});

describe("launchTypePatch (phase 7)", () => {
  const base = () => {
    const p = awsProject({ allow_docker_access: true, vpn_support_enabled: true });
    return { ...p, aws: { ...p.aws!, gateway_sidecar: true, spot: true, cpu: 4096, memory_mib: 16384 } };
  };

  it("to EC2: keeps Docker/VPN, clears the sidecar and Spot, and fits the reservations to the instance", () => {
    const patch = launchTypePatch(base(), "ec2", { cpu: 1920, memory_mib: 7373 });
    expect(patch.allow_docker_access).toBeUndefined();
    expect(patch.vpn_support_enabled).toBeUndefined();
    expect(patch.aws).toMatchObject({ launch_type: "ec2", gateway_sidecar: false, spot: false, cpu: 1920, memory_mib: 7373 });
    // Reservations that already fit stay; unknown limits change nothing.
    const small = { ...base(), aws: { ...base().aws, cpu: 1024, memory_mib: 4096 } };
    expect(launchTypePatch(small, "ec2", { cpu: 1920, memory_mib: 7373 }).aws).toMatchObject({ cpu: 1024, memory_mib: 4096 });
    expect(launchTypePatch(base(), "ec2", null).aws).toMatchObject({ cpu: 4096, memory_mib: 16384 });
  });

  it("to Fargate: clears Docker/VPN and snaps the reservations to a Fargate pair", () => {
    const p = base();
    const ec2 = { ...p, aws: { ...p.aws, launch_type: "ec2" as const, cpu: 1920, memory_mib: 7373, spot: false } };
    const patch = launchTypePatch(ec2, "fargate");
    expect(patch).toMatchObject({ allow_docker_access: false, vpn_support_enabled: false });
    expect(patch.aws).toMatchObject({ launch_type: "fargate", cpu: 1024, memory_mib: 7168 });
    expect(memoryError(patch.aws!.cpu, patch.aws!.memory_mib)).toBeNull();
    // Below the smallest Fargate size: the smallest.
    const tiny = { ...ec2, aws: { ...ec2.aws, cpu: 100, memory_mib: 300 } };
    expect(launchTypePatch(tiny, "fargate").aws).toMatchObject({ cpu: 256, memory_mib: 512 });
    // A valid pair is kept.
    const ok = { ...ec2, aws: { ...ec2.aws, cpu: 1024, memory_mib: 4096 } };
    expect(launchTypePatch(ok, "fargate").aws).toMatchObject({ cpu: 1024, memory_mib: 4096 });
  });

  it("the sidecar is not available on EC2 (words it as Rust's EC2_NO_SIDECAR)", () => {
    const p = { ...base(), backend: "open_ai_compatible" as const };
    const ec2 = { ...p, aws: { ...p.aws, launch_type: "ec2" as const } };
    const gw = { models: [{ name: "m", model_id: "x" }] } as never;
    expect(gatewaySidecarProblem(ec2, gw)).toBe(EC2_NO_SIDECAR);
    expect(EC2_NO_SIDECAR).toBe(
      "The LLM gateway sidecar is not available on EC2 yet: turn it off, or run the project on Fargate",
    );
    expect(gatewaySidecarProblem(p, gw)).toBeNull();
  });
});
