import type { ContainerMigration } from "../../../hooks/useContainerMigration";
import Button from "../../ui/Button";
import StatusIndicator from "../../ui/StatusIndicator";
import MigrationReportCard from "../MigrationReportCard";
import MigrationInterruptedCard from "../MigrationInterruptedCard";
import { formatSnapshotDate, joinFeatures } from "../migrationCopy";

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
  const {
    staleness,
    probing,
    probeSettled,
    running,
    recovered,
    interrupted,
    report,
    phaseMessage,
    busy,
  } = migration;

  // An unfinished migration outranks its own report. The report's action row
  // offers Keep, and Keep on a mid-swap container drops the rollback image
  // while `:latest` still points at the old lineage — the backend's message on
  // the very same record says to resume. Resume is the only honest primary
  // action here, so the report card is not rendered at all.
  if (interrupted) {
    return (
      <section
        className={`${SHELL} border-[var(--error)]/40 bg-[var(--error-muted)]`}
        aria-label="Container base update was interrupted"
      >
        <MigrationInterruptedCard
          record={interrupted}
          busy={busy || running}
          onResume={() => void migration.resume()}
          onRollback={() => void migration.rollback()}
        />
      </section>
    );
  }

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
          onDismiss={() => void migration.dismiss()}
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

          {/* An out-of-date container that also has data under /var is the one
              case where updating can cost something, so it is said here and not
              only behind the button. */}
          {staleness.unpreserved_data.length > 0 && (
            <p className="text-xs text-[var(--text-primary)] leading-snug">
              Not carried across:{" "}
              <span className="font-mono text-[var(--text-secondary)]">
                {staleness.unpreserved_data.map((d) => d.path).join(", ")}
              </span>
              <span className="text-[var(--text-secondary)]">
                {" "}
                — back this up before updating.
              </span>
            </p>
          )}

          {!canMigrate && (
            <p className="text-xs text-[var(--text-secondary)] leading-snug">
              {/* Distinguishing these matters: "stop the container" on a
                  container that is already stopped, because the probe has not
                  landed, reads as a bug. */}
              {!probeSettled
                ? probing
                  ? "Checking what this container has that the current base does not…"
                  : "That check did not complete, so what would be carried across is not known. Updating stays disabled until it does — try again once the container can be inspected."
                : "Stop the container to update its base."}
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
