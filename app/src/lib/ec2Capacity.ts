import { useEffect, useState } from "react";
import * as commands from "./tauri-commands";
import type { Ec2Capacity, Project } from "./types";

/** How long a user's EC2 capacity is reused before AWS is asked again. */
export const EC2_CAPACITY_TTL_MS = 10 * 60 * 1000;

interface Entry {
  at: number;
  value: Promise<Ec2Capacity>;
}

/** Keyed by connection, region and environment: the record is the caller's
 *  in that environment, so every project placed there shares one read. */
const cache = new Map<string, Entry>();

/**
 * `aws_ec2_capacity`, cached per (connection, region, environment) for
 * [`EC2_CAPACITY_TTL_MS`]. A read in flight is shared; an `unknown` answer
 * (offline, slow) or a rejection is not kept, so the next mount asks again.
 */
export function ec2Capacity(
  projectId: string,
  connectionId: string,
  region: string,
  environment: string,
  now: number = Date.now(),
): Promise<Ec2Capacity> {
  const key = JSON.stringify([connectionId, region, environment]);
  const hit = cache.get(key);
  if (hit && now - hit.at < EC2_CAPACITY_TTL_MS) return hit.value;
  const value = Promise.resolve().then(() => commands.awsEc2Capacity(projectId));
  const entry = { at: now, value };
  cache.set(key, entry);
  const forget = () => {
    if (cache.get(key) === entry) cache.delete(key);
  };
  value.then((c) => c.state === "unknown" && forget(), forget);
  return value;
}

/** After an onboarding apply changed the user's EC2 capacity, and for tests. */
export function clearEc2CapacityCache() {
  cache.clear();
}

/** The project's EC2 capacity (null while it is read); only asked when `enabled`. */
export function useEc2Capacity(
  project: Pick<Project, "id" | "host_id" | "aws_placement">,
  enabled = true,
): Ec2Capacity | null {
  const [capacity, setCapacity] = useState<Ec2Capacity | null>(null);
  const region = project.aws_placement?.region;
  const environment = project.aws_placement?.environment;
  useEffect(() => {
    setCapacity(null);
    if (!enabled) return;
    if (!project.host_id || !region || !environment) {
      setCapacity({ state: "unknown" });
      return;
    }
    let cancelled = false;
    ec2Capacity(project.id, project.host_id, region, environment)
      .catch((): Ec2Capacity => ({ state: "unknown" }))
      .then((c) => {
        if (!cancelled) setCapacity(c);
      });
    return () => {
      cancelled = true;
    };
  }, [enabled, project.id, project.host_id, region, environment]);
  return capacity;
}
