/**
 * Shared wording for container base-image migration.
 *
 * The banner, the pre-flight modal and the report all have to make the same
 * promise about what survives, or the feature reads as another Reset. It is
 * written once here so the three surfaces cannot drift apart.
 */

import type { PackageFailure } from "../../lib/types";
import { formatBytes } from "../../lib/formatBytes";

/**
 * What re-attaches untouched. These are not copied, rebuilt or re-authenticated
 * — they live on the two Docker volumes, which the new container mounts as-is.
 */
export const KEPT_AUTOMATICALLY = [
  "Your claude login and ~/.claude.json — no signing in again",
  "Skills, agents, commands, hooks, plugins and MCP config",
  "Every saved session transcript, so past sessions still resume",
  "Scheduler tasks and their logs",
  "SSH keys, git config and shell history",
  "Claude Code itself, plus Rust/cargo, uv and ruff in your home directory",
];

export const KEPT_WHY =
  "/home/claude and ~/.claude are Docker volumes. They detach from the old container and re-attach to the new one unchanged.";

/**
 * The honest list of what the writable layer holds, because the modal's own
 * sections name more than one thing and copy that says "the only thing" while
 * the section below it offers to copy files is copy the user cannot trust.
 */
export const LOST_WITHOUT_REPLAY =
  "What a new base does not carry over is what lives in the container itself: system packages you installed with apt, global npm packages, and files under /usr/local, /opt, /srv or loose in /workspace. This update puts those back.";

/**
 * The exception, and it is not a small one — so it gets its own line wherever
 * the update is offered. Reinstalling `postgresql` gets the package back and an
 * empty cluster with it; the ordinary Reset-free recreate keeps /var because it
 * builds from the project's own saved image, so this is the one way in which
 * updating the base is more destructive than leaving it alone.
 */
export const DATA_NOT_CARRIED =
  "Data written under /var is not carried across and reinstalling the package does not bring it back — a database in /var/lib, a site in /var/www. Back it up from inside the container before you update.";

/**
 * Said plainly everywhere rollback is offered. Rollback is not a time machine:
 * it swaps the system layer back and leaves both volumes exactly where the
 * migrated session left them.
 */
export const ROLLBACK_SCOPE =
  "Rollback restores the system layer only. Your volumes are never touched, so anything Claude wrote to your home directory or a mounted workspace during the migrated session stays as it is.";

export const ROLLBACK_DISK_COST =
  "A rollback image is close to a full second copy of the container — snapshots here run 3.8–12.3 GB and share almost nothing with the new base, so it costs nearly its full size on disk. It is deleted the moment you press Keep.";

/** Shown mid-run, where rollback is not a button but is still the safety net. */
export const MID_RUN_SAFETY =
  "If this fails, the container is put back on its previous system layer automatically. Your volumes are not touched at any point.";

export const REPLAY_COST =
  "Needs network access and usually takes 1–2 minutes.";

/** `41.0 MB`. Sizes here are informational, so the friendlier decimal unit. */
export function formatDataSize(bytes: number): string {
  return formatBytes(bytes);
}

/** `1 Mar` — short enough to sit inline in the banner sentence. */
export function formatSnapshotDate(iso: string | null): string | null {
  if (!iso) return null;
  const ms = Date.parse(iso);
  if (Number.isNaN(ms)) return null;
  return new Date(ms).toLocaleDateString(undefined, {
    day: "numeric",
    month: "short",
  });
}

/** Join a list into prose: "a, b and c". Used for the missing-features line. */
export function joinFeatures(features: string[]): string {
  if (features.length === 0) return "";
  if (features.length === 1) return features[0];
  return `${features.slice(0, -1).join(", ")} and ${features[features.length - 1]}`;
}

/** The exact line to paste into a shell to finish a partial migration by hand. */
export function aptRetryCommand(failures: PackageFailure[]): string {
  return `sudo apt-get install -y ${failures.map((f) => f.name).join(" ")}`;
}

/** Plain-text form of a partial report, for the copy button. */
export function failureReportText(failures: PackageFailure[]): string {
  const lines = failures.map((f) => `${f.name}: ${f.reason}`);
  return [
    "Packages that could not be reinstalled:",
    ...lines,
    "",
    aptRetryCommand(failures),
  ].join("\n");
}
