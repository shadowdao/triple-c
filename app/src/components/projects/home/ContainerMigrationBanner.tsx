import type { ContainerMigration } from "../../../hooks/useContainerMigration";
import Button from "../../ui/Button";
import StatusIndicator from "../../ui/StatusIndicator";
import MigrationReportCard from "../MigrationReportCard";
import { ROLLBACK_SCOPE, formatSnapshotDate, joinFeatures } from "../migrationCopy";

interface Props {
  migration: ContainerMigration;
  /** Migration mirrors Reset's gate: the container has to be stopped. */
  canMigrate: boolean;
  onOpen: () => void;
}

const SHELL =
  "border rounded-[var(--radius-panel)] px-3.5 py-3 space-y-2";

/**
 * The Overview answer to "why is this container behaving oddly?".
 *
 * It leads with the *features* that are missing, not image digests: a user does
 * not know or care that `sha256:abc…` differs from `sha256:def…`, they care
 * that host-browser opening and the auth bridge do not work. Digests are the
 * evidence, not the message.
 *
 * It also has to survive the run: an in-flight migration, an interrupted one,
 * and the report are all shown here, because the modal is dismissable and the
 * outcome must not vanish with it.
 */
export default function ContainerMigrationBanner({
  migration,
  canMigrate,
  onOpen,
}: Props) {
  const { staleness, running, recovered, interrupted, report, phaseMessage, busy } =
    migration;

  // The report outranks staleness: after a run, the outcome is the news.
  if (report) {
    return (
      <section
        className={`${SHELL} ${
          report.phase === "partial" || report.phase === "failed"
            ? "border-[var(--error)]/40 bg-[var(--error-muted)]"
            : "border-[var(--border-color)] bg-[var(--bg-secondary)]"
        }`}
        aria-label="Container base update result"
      >
        <MigrationReportCard
          report={report}
          busy={busy}
          onKeep={() => void migration.keep()}
          onRollback={() => void migration.rollback()}
          onDismiss={migration.dismiss}
        />
      </section>
    );
  }

  if (running) {
    return (
      <section
        className={`${SHELL} border-[var(--warning)]/40 bg-[var(--warning-muted)]`}
        aria-label="Container base update in progress"
      >
        <div className="flex items-start justify-between gap-3">
          <div className="min-w-0">
            <StatusIndicator
              tone="busy"
              label={
                recovered
                  ? "A container base update was already running"
                  : "Updating container base"
              }
              className="text-[13px] font-semibold"
            />
            <p className="mt-1 text-xs text-[var(--text-secondary)] truncate">
              {phaseMessage ?? "Starting…"}
            </p>
            {recovered && (
              <p className="mt-1 text-xs text-[var(--text-secondary)]">
                It was still in progress when the app last closed. Picking it back up.
              </p>
            )}
          </div>
          <Button size="md" onClick={onOpen}>
            Show progress
          </Button>
        </div>
      </section>
    );
  }

  // Nothing is driving this one. It outranks staleness because the container is
  // sitting mid-swap, and the one thing it must never do is look like a normal
  // out-of-date container that the user can take or leave.
  if (interrupted) {
    return (
      <section
        className={`${SHELL} border-[var(--error)]/40 bg-[var(--error-muted)]`}
        aria-label="Container base update was interrupted"
      >
        <StatusIndicator
          tone="error"
          label="A container base update was interrupted"
          className="text-[13px] font-semibold"
        />
        <p className="text-xs text-[var(--text-secondary)] leading-snug">
          It started{" "}
          {formatSnapshotDate(interrupted.started_at) ?? "earlier"} and the app
          closed before it finished, so this container is part-way onto the new
          base. Resuming replays the same plan it was given.
        </p>
        <p className="text-xs text-[var(--text-secondary)] leading-snug">
          {ROLLBACK_SCOPE}
        </p>
        <div className="flex flex-wrap gap-1.5">
          <Button
            size="md"
            variant="primary"
            disabled={busy}
            onClick={() => void migration.resume()}
          >
            Resume update
          </Button>
          {interrupted.rollback_image && (
            <Button
              size="md"
              variant="danger"
              disabled={busy}
              onClick={() => void migration.rollback()}
            >
              Roll back
            </Button>
          )}
        </div>
      </section>
    );
  }

  if (!staleness) return null;

  // `stale` is deliberately false whenever `known` is false — an unestablished
  // lineage is not a claim of staleness. But a container with no base-image
  // label is exactly the old container most likely to be missing things, and
  // the probe says so directly. So the probe's own findings are grounds to
  // speak up even though the version comparison never happened.
  const probeFoundGaps =
    !staleness.known &&
    (staleness.missing_features.length > 0 || staleness.missing_paths.length > 0);
  if (!staleness.stale && !probeFoundGaps) return null;

  const snapshot = formatSnapshotDate(staleness.snapshot_created_at);
  const features = joinFeatures(staleness.missing_features);

  return (
    <section
      className={`${SHELL} border-[var(--warning)]/40 bg-[var(--warning-muted)]`}
      aria-label="Container base is out of date"
    >
      <div className="flex items-start justify-between gap-3">
        <div className="min-w-0 space-y-1">
          <StatusIndicator
            tone="error"
            label={
              staleness.known
                ? "Container base is out of date"
                : "Container is missing things the current base ships"
            }
            className="text-[13px] font-semibold"
          />

          <p className="text-xs text-[var(--text-secondary)] leading-snug">
            {staleness.known
              ? snapshot
                ? `Running on a saved image from ${snapshot}.`
                : "Running on a saved image older than the current base."
              : "This container predates base-image tracking, so it was probed directly."}
          </p>

          {staleness.missing_features.length > 0 && (
            <p className="text-xs leading-snug text-[var(--text-primary)]">
              {staleness.known ? "Missing: " : "The probe found these missing: "}
              <span className="text-[var(--text-secondary)]">{features}.</span>
            </p>
          )}

          {staleness.missing_features.length === 0 &&
            staleness.missing_paths.length > 0 && (
              <p className="text-xs leading-snug text-[var(--text-primary)]">
                {staleness.known ? "Missing: " : "The probe found these missing: "}
                <span className="font-mono text-[var(--text-secondary)]">
                  {staleness.missing_paths.join(", ")}
                </span>
              </p>
            )}

          {/* Deliberately "differ" rather than "behind": the count is a drift
              measure, not a promise that every one of them is newer. */}
          {staleness.outdated_package_count > 0 && (
            <p className="text-xs text-[var(--text-secondary)] leading-snug">
              {staleness.outdated_package_count} package
              {staleness.outdated_package_count === 1 ? "" : "s"} differ from the
              versions on the current base, where security updates land.
            </p>
          )}

          {staleness.probe_error && (
            <p className="text-xs text-[var(--text-secondary)] leading-snug">
              Some checks did not complete: {staleness.probe_error}
            </p>
          )}

          {!canMigrate && (
            <p className="text-xs text-[var(--text-secondary)] leading-snug">
              Stop the container to update its base.
            </p>
          )}
        </div>

        <Button
          size="md"
          variant="primary"
          disabled={!canMigrate}
          onClick={onOpen}
          className="flex-shrink-0"
        >
          Update container base…
        </Button>
      </div>
    </section>
  );
}
