import { useState } from "react";
import type { MigrationReport } from "../../lib/types";
import Button from "../ui/Button";
import StatusIndicator from "../ui/StatusIndicator";
import {
  ROLLBACK_SCOPE,
  aptRetryCommand,
  failureReportText,
} from "./migrationCopy";

interface Props {
  report: MigrationReport;
  /** Disables the action row while confirm/rollback is in flight. */
  busy?: boolean;
  onKeep: () => void;
  onRollback: () => void;
  /** Only offered when there is nothing to keep or roll back. */
  onDismiss: () => void;
}

/**
 * The outcome of a migration, rendered identically in the Overview banner and
 * in the modal so a user who closed the modal is not shown a different story.
 *
 * A **partial** is the case this component exists for. The user arrived here
 * because containers degrade silently — a run that quietly dropped `socat` and
 * called itself a success would be exactly the same bug in a new place. So a
 * partial is painted as a warning, names every package and the reason it
 * failed, and hands over the literal `apt-get` line to finish the job.
 */
export default function MigrationReportCard({
  report,
  busy = false,
  onKeep,
  onRollback,
  onDismiss,
}: Props) {
  const [copied, setCopied] = useState<"command" | "detail" | null>(null);
  const partial = report.phase === "partial";
  const failed = report.phase === "failed";
  const rolledBack = report.phase === "rolled_back";

  const copy = async (what: "command" | "detail", text: string) => {
    try {
      await navigator.clipboard.writeText(text);
      setCopied(what);
      setTimeout(() => setCopied(null), 2000);
    } catch {
      // Clipboard can be denied; the text is selectable on screen either way.
    }
  };

  // Partial and failed are painted as failures. A partial that reads as a
  // success is precisely how a container ends up silently degraded.
  const tone = partial || failed ? "error" : rolledBack ? "off" : "ok";
  const heading = partial
    ? "Updated, but not completely"
    : failed
      ? "Update failed"
      : rolledBack
        ? "Rolled back"
        : "Container base updated";

  return (
    <div className="space-y-3">
      <div className="flex items-baseline gap-2">
        <StatusIndicator tone={tone} label={heading} className="text-[13px] font-semibold" />
      </div>

      {report.phase === "succeeded" && (
        <p className="text-[13px] text-[var(--text-secondary)]">
          {report.packages_installed.length} package
          {report.packages_installed.length === 1 ? "" : "s"} reinstalled,{" "}
          {report.features_restored.length} feature
          {report.features_restored.length === 1 ? "" : "s"} restored.
          {report.paths_copied.length > 0
            ? ` ${report.paths_copied.length} path${report.paths_copied.length === 1 ? "" : "s"} copied across.`
            : ""}
        </p>
      )}

      {failed && (
        <p className="text-[13px] text-[var(--text-secondary)]">
          {report.message ||
            "Update failed. Your container has been restored to its previous state."}
        </p>
      )}

      {rolledBack && (
        <p className="text-[13px] text-[var(--text-secondary)]">
          {report.message || "The previous system layer has been put back."}
        </p>
      )}

      {partial && (
        <div className="space-y-2.5">
          <p className="text-[13px] text-[var(--text-primary)]">
            {report.packages_installed.length} of{" "}
            {report.packages_requested.length} packages went back on.{" "}
            <strong>
              {report.packages_failed.length} did not
            </strong>
            , so this container is still missing something it had before.
          </p>

          <div
            className="rounded-[var(--radius-control)] border border-[var(--error)]/40 bg-[var(--error-muted)] px-3 py-2 select-text"
            data-testid="migration-failures"
          >
            <ul className="space-y-1.5">
              {report.packages_failed.map((failure) => (
                <li key={failure.name} className="text-xs leading-snug">
                  <span className="font-mono font-semibold text-[var(--text-primary)]">
                    {failure.name}
                  </span>
                  <span className="text-[var(--text-secondary)]"> — {failure.reason}</span>
                </li>
              ))}
            </ul>
          </div>

          {report.packages_failed.length > 0 && (
            <div className="space-y-1.5">
              <p className="text-xs text-[var(--text-secondary)]">
                Finish by hand in a shell inside the container:
              </p>
              <code className="block px-2.5 py-1.5 font-mono text-xs text-[var(--text-primary)] bg-[var(--bg-primary)] border border-[var(--border-color)] rounded-[var(--radius-control)] overflow-x-auto whitespace-pre select-text">
                {aptRetryCommand(report.packages_failed)}
              </code>
              <div className="flex flex-wrap gap-1.5">
                <Button
                  onClick={() =>
                    copy("command", aptRetryCommand(report.packages_failed))
                  }
                >
                  {copied === "command" ? "Copied ✓" : "Copy apt-get line"}
                </Button>
                <Button
                  onClick={() =>
                    copy("detail", failureReportText(report.packages_failed))
                  }
                >
                  {copied === "detail" ? "Copied ✓" : "Copy failure details"}
                </Button>
              </div>
            </div>
          )}
        </div>
      )}

      {report.features_restored.length > 0 && !failed && (
        <div>
          <h4 className="text-[11px] font-semibold uppercase tracking-wide text-[var(--text-secondary)]">
            Restored
          </h4>
          <p className="mt-0.5 text-xs text-[var(--text-secondary)]">
            {report.features_restored.join(", ")}
          </p>
        </div>
      )}

      {report.message && !failed && !rolledBack && (
        <p className="text-xs text-[var(--text-secondary)] select-text">{report.message}</p>
      )}

      {report.rollback_available && (
        <p className="text-xs text-[var(--text-secondary)] leading-snug">
          {ROLLBACK_SCOPE}
        </p>
      )}

      <div className="flex flex-wrap items-center gap-1.5 pt-0.5">
        {report.rollback_available ? (
          <>
            <Button size="md" variant="primary" disabled={busy} onClick={onKeep}>
              Keep
            </Button>
            <Button size="md" variant="danger" disabled={busy} onClick={onRollback}>
              Roll back
            </Button>
          </>
        ) : (
          <Button size="md" disabled={busy} onClick={onDismiss}>
            Dismiss
          </Button>
        )}
      </div>
    </div>
  );
}
