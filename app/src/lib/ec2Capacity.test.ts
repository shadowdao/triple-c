import { describe, it, expect, vi, beforeEach } from "vitest";
import type { Ec2Capacity } from "./types";

const EC2: Ec2Capacity = {
  state: "ec2",
  compute: {
    instance_type: "m7i.large",
    max_instances: 1,
    user_tag: "alice",
    cluster_arn: "arn:aws:ecs:eu-west-1:1:cluster/triple-c-dev--u-alice",
    capacity_provider: "triple-c-dev--u-alice",
  },
  available_cpu: 1920,
  available_memory_mib: 7373,
};
const awsEc2Capacity = vi.fn(async (_id: string): Promise<Ec2Capacity> => EC2);
vi.mock("./tauri-commands", () => ({
  awsEc2Capacity: (id: string) => awsEc2Capacity(id),
}));

import {
  EC2_CAPACITY_SHORT_TTL_MS,
  EC2_CAPACITY_TTL_MS,
  clearEc2CapacityCache,
  ec2Capacity,
} from "./ec2Capacity";

describe("ec2Capacity", () => {
  beforeEach(() => {
    awsEc2Capacity.mockClear();
    clearEc2CapacityCache();
  });

  it("asks once per connection, region and environment until the TTL passes", async () => {
    expect(await ec2Capacity("p1", "c1", "eu-west-1", "dev", 0)).toEqual(EC2);
    expect(await ec2Capacity("p2", "c1", "eu-west-1", "dev", 1000)).toEqual(EC2);
    expect(awsEc2Capacity).toHaveBeenCalledTimes(1);
    await ec2Capacity("p1", "c1", "eu-west-1", "prod", 1000);
    expect(awsEc2Capacity).toHaveBeenCalledTimes(2);
    await ec2Capacity("p1", "c1", "eu-west-1", "dev", EC2_CAPACITY_TTL_MS);
    expect(awsEc2Capacity).toHaveBeenCalledTimes(3);
  });

  it("keeps none and unusable for a minute only, ec2 for ten", async () => {
    expect(EC2_CAPACITY_SHORT_TTL_MS).toBe(60 * 1000);
    expect(EC2_CAPACITY_TTL_MS).toBe(10 * 60 * 1000);
    for (const answer of [{ state: "none" }, { state: "unusable", reason: "x" }] as Ec2Capacity[]) {
      clearEc2CapacityCache();
      awsEc2Capacity.mockClear();
      awsEc2Capacity.mockResolvedValueOnce(answer);
      expect(await ec2Capacity("p1", "c1", "eu-west-1", "dev", 0)).toEqual(answer);
      await ec2Capacity("p1", "c1", "eu-west-1", "dev", EC2_CAPACITY_SHORT_TTL_MS - 1);
      expect(awsEc2Capacity).toHaveBeenCalledTimes(1);
      expect(await ec2Capacity("p1", "c1", "eu-west-1", "dev", EC2_CAPACITY_SHORT_TTL_MS)).toEqual(EC2);
      expect(awsEc2Capacity).toHaveBeenCalledTimes(2);
      // The ec2 answer that replaced it is kept for the long TTL.
      await ec2Capacity("p1", "c1", "eu-west-1", "dev", EC2_CAPACITY_TTL_MS);
      expect(awsEc2Capacity).toHaveBeenCalledTimes(2);
    }
  });

  it("keeps none, but not unknown or a rejection", async () => {
    awsEc2Capacity.mockResolvedValueOnce({ state: "none" });
    expect(await ec2Capacity("p1", "c1", "eu-west-1", "dev", 0)).toEqual({ state: "none" });
    await ec2Capacity("p1", "c1", "eu-west-1", "dev", 1);
    expect(awsEc2Capacity).toHaveBeenCalledTimes(1);

    clearEc2CapacityCache();
    awsEc2Capacity.mockResolvedValueOnce({ state: "unknown" });
    expect(await ec2Capacity("p1", "c1", "eu-west-1", "dev", 0)).toEqual({ state: "unknown" });
    expect(await ec2Capacity("p1", "c1", "eu-west-1", "dev", 1)).toEqual(EC2);
    expect(awsEc2Capacity).toHaveBeenCalledTimes(3);

    awsEc2Capacity.mockRejectedValueOnce(new Error("x"));
    clearEc2CapacityCache();
    await expect(ec2Capacity("p1", "c1", "eu-west-1", "dev", 0)).rejects.toThrow("x");
    expect(await ec2Capacity("p1", "c1", "eu-west-1", "dev", 1)).toEqual(EC2);
  });
});
