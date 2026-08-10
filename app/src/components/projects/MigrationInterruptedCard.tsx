import type { MigrationState } from "../../lib/types";
import Button from "../ui/Button";
import StatusIndicator from "../ui/StatusIndicator";
import { ROLLBACK_SCOPE, formatSnapshotDate } from "./migrationCopy";

interface Props {
  record: MigrationState;
  /** Disables the action row while resume/rollback is in flight. */
  busy?: boolean;
  onResume: () => void;
  onRollback: () => void;
}

/**
 * A migration that got past the container swap and stopped there.
 *
 * This is deliberately **not** [`MigrationReportCard`]. That card's primary
 * action is Keep, which means "accept this and drop the rollback image" — and
 * on an unfinished migration `triple-c-snapshot-<id>:latest` still points at
 * the *old* lineage, so Keep would delete the only way back while leaving a
 * container the app can no longer reason about. The backend's own message on
 * this record says to resume; offering Keep beside it was the UI contradicting
 * the backend and losing.
 *
 * So the two actions here are Resume and Roll back, and nothing else. It is
 * shown ahead of any report, whether the record was found on mount or produced
 * by a run that just failed — those are the same situation.
 */
export default function MigrationInterruptedCard({
  record,
  busy = false,
  onResume,
  onRollback,
}: Props) {
  const started = formatSnapshotDate(record.started_at);

  return (
    <div className="space-y-2">
      <StatusIndicator
        tone="error"
        label="The container base update did not finish"
        className="text-[13px] font-semibold"
      />

      <p className="text-xs text-[var(--text-secondary)] leading-snug">
        This container is part-way onto the new base: it was replaced, but the
        result was never saved
        {started ? `. The update started ${started}` : ""}. Resuming replays the
        same plan it was given — it is the only way to finish it.
      </p>

      {record.report?.message && (
        <p className="text-xs text-[var(--text-secondary)] leading-snug select-text">
          {record.report.message}
        </p>
      )}

      <p className="text-xs text-[var(--text-secondary)] leading-snug">
        {ROLLBACK_SCOPE}
      </p>

      <div className="flex flex-wrap gap-1.5 pt-0.5">
        <Button size="md" variant="primary" disabled={busy} onClick={onResume}>
          Resume update
        </Button>
        {record.rollback_image && (
          <Button size="md" variant="danger" disabled={busy} onClick={onRollback}>
            Roll back
          </Button>
        )}
      </div>
    </div>
  );
}
