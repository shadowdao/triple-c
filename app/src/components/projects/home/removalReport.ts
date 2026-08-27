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

/** How many distinct things `describeLeftovers` is describing — a container
 *  and an image each count as one, however many volumes are named. Shared by
 *  `leftoverVerb` and `leftoverPronoun` so the two can never disagree about
 *  singular vs. plural. */
function leftoverCount(report: ProjectRemovalReport): number {
  return (report.container ? 1 : 0) + (report.image ? 1 : 0) + report.volumes.length;
}

/** Verb agreement for `describeLeftovers`'s output — "its container" needs
 *  "was", "its container, a volume" needs "were". */
export function leftoverVerb(report: ProjectRemovalReport): "was" | "were" {
  return leftoverCount(report) === 1 ? "was" : "were";
}

/** Pronoun agreement for referring back to `describeLeftovers`'s output —
 *  "remove it manually" for one thing, "remove them manually" for more. */
export function leftoverPronoun(report: ProjectRemovalReport): "it" | "them" {
  return leftoverCount(report) === 1 ? "it" : "them";
}
