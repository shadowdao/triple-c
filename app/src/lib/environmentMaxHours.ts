import * as commands from "./tauri-commands";

/** How long an environment's lifetime cap is reused before AWS is asked again. */
export const MAX_HOURS_TTL_MS = 10 * 60 * 1000;

interface Entry {
  at: number;
  value: Promise<number | null>;
}

/** Keyed by connection, region and environment: the record is the
 *  environment's, so every project placed there shares one read. */
const cache = new Map<string, Entry>();

/**
 * `aws_environment_max_hours`, cached per (connection, region, environment)
 * for [`MAX_HOURS_TTL_MS`], so re-mounting Host settings does not read the
 * environment record from AWS again. A read in flight is shared; a rejected
 * one is not kept (the next mount asks again).
 */
export function environmentMaxHours(
  projectId: string,
  connectionId: string,
  region: string,
  environment: string,
  now: number = Date.now(),
): Promise<number | null> {
  const key = JSON.stringify([connectionId, region, environment]);
  const hit = cache.get(key);
  if (hit && now - hit.at < MAX_HOURS_TTL_MS) return hit.value;
  const value = Promise.resolve()
    .then(() => commands.awsEnvironmentMaxHours(projectId))
    .then((h) => h ?? null);
  const entry = { at: now, value };
  cache.set(key, entry);
  value.catch(() => {
    if (cache.get(key) === entry) cache.delete(key);
  });
  return value;
}

/** For tests. */
export function clearEnvironmentMaxHoursCache() {
  cache.clear();
}
