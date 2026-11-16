import { describe, it, expect, vi, beforeEach } from "vitest";

const awsEnvironmentMaxHours = vi.fn(async (_id: string) => 24 as number | null);
vi.mock("./tauri-commands", () => ({
  awsEnvironmentMaxHours: (id: string) => awsEnvironmentMaxHours(id),
}));

import {
  MAX_HOURS_TTL_MS,
  clearEnvironmentMaxHoursCache,
  environmentMaxHours,
} from "./environmentMaxHours";

describe("environmentMaxHours", () => {
  beforeEach(() => {
    awsEnvironmentMaxHours.mockClear();
    clearEnvironmentMaxHoursCache();
  });

  it("asks once per connection, region and environment until the TTL passes", async () => {
    expect(await environmentMaxHours("p1", "c1", "eu-west-1", "dev", 0)).toBe(24);
    // Another project in the same environment shares the read.
    expect(await environmentMaxHours("p2", "c1", "eu-west-1", "dev", 1000)).toBe(24);
    expect(awsEnvironmentMaxHours).toHaveBeenCalledTimes(1);
    await environmentMaxHours("p1", "c2", "eu-west-1", "dev", 1000);
    await environmentMaxHours("p1", "c1", "us-east-1", "dev", 1000);
    await environmentMaxHours("p1", "c1", "eu-west-1", "prod", 1000);
    expect(awsEnvironmentMaxHours).toHaveBeenCalledTimes(4);
    await environmentMaxHours("p1", "c1", "eu-west-1", "dev", MAX_HOURS_TTL_MS);
    expect(awsEnvironmentMaxHours).toHaveBeenCalledTimes(5);
  });

  it("shares a read in flight and keeps an unreadable record (null)", async () => {
    awsEnvironmentMaxHours.mockResolvedValueOnce(null);
    const a = environmentMaxHours("p1", "c1", "eu-west-1", "dev", 0);
    const b = environmentMaxHours("p1", "c1", "eu-west-1", "dev", 0);
    expect(await a).toBeNull();
    expect(await b).toBeNull();
    expect(awsEnvironmentMaxHours).toHaveBeenCalledTimes(1);
  });

  it("does not keep a rejected read", async () => {
    awsEnvironmentMaxHours.mockRejectedValueOnce(new Error("Project p1 not found"));
    await expect(environmentMaxHours("p1", "c1", "eu-west-1", "dev", 0)).rejects.toThrow();
    expect(await environmentMaxHours("p1", "c1", "eu-west-1", "dev", 1)).toBe(24);
    expect(awsEnvironmentMaxHours).toHaveBeenCalledTimes(2);
  });
});
