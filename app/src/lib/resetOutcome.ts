import type { ProjectResetOutcome } from "./types";

/**
 * Names what a `ProjectResetOutcome` says Reset could not clear, for
 * `useProjectActions`'s Reset toast.
 *
 * The image is named first and phrased as "its previous container image"
 * rather than folded in with the volumes — it is the more serious of the
 * two: the new container is built from it whenever it exists, so a
 * surviving image means Reset silently rebuilt the exact system layer it
 * was asked to discard, while a surviving volume only means old data rides
 * along.
 */
export function describeResetLeftovers(outcome: ProjectResetOutcome): string {
  const parts: string[] = [];
  if (outcome.leftover_image) parts.push("its previous container image");
  if (outcome.leftover_volumes.length === 1) parts.push("a volume");
  else if (outcome.leftover_volumes.length > 1) parts.push(`${outcome.leftover_volumes.length} volumes`);
  return parts.join(" and ");
}

/** How many distinct things `describeResetLeftovers` is describing — the
 *  image counts as one, however many volumes are named alongside it. */
function resetLeftoverCount(outcome: ProjectResetOutcome): number {
  return (outcome.leftover_image ? 1 : 0) + outcome.leftover_volumes.length;
}

/** Pronoun agreement for referring back to `describeResetLeftovers`'s
 *  output — "remove it manually" for one thing, "remove them" for more. */
export function resetLeftoverPronoun(outcome: ProjectResetOutcome): "it" | "them" {
  return resetLeftoverCount(outcome) === 1 ? "it" : "them";
}
