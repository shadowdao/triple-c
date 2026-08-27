import type { ProjectRemovalReport } from "../../../lib/types";

/**
 * Names what a `ProjectRemovalReport` says survived, for the leftover toast.
 *
 * Worded as "could not confirm" rather than "is still on disk": the same
 * report shape covers a genuine leftover (a locked volume) and a daemon that
 * was simply unreachable at the time, in which case nothing was ever created
 * and there is nothing to find — asserting certainty either way would be
 * wrong in one of those cases.
 */
export function describeLeftovers(report: ProjectRemovalReport): string {
  const parts: string[] = [];
  if (report.container) parts.push("its container");
  if (report.image) parts.push("its saved image");
  if (report.volumes.length === 1) parts.push("a volume");
  else if (report.volumes.length > 1) parts.push(`${report.volumes.length} volumes`);
  return parts.join(", ");
}

/** Verb agreement for `describeLeftovers`'s output — "its container" needs
 *  "was", "its container, a volume" needs "were". */
export function leftoverVerb(report: ProjectRemovalReport): "was" | "were" {
  const count = (report.container ? 1 : 0) + (report.image ? 1 : 0) + report.volumes.length;
  return count === 1 ? "was" : "were";
}
