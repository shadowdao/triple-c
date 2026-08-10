import { useEffect, useRef, useState } from "react";
import type { ContainerStaleness, MigrationOptions } from "../../lib/types";
import Modal from "../ui/Modal";
import Button from "../ui/Button";
import Toggle from "../ui/Toggle";
import { SwitchRow } from "../ui/Field";
import MigrationReportCard from "./MigrationReportCard";
import MigrationInterruptedCard from "./MigrationInterruptedCard";
import type { ContainerMigration } from "../../hooks/useContainerMigration";
import {
  DATA_NOT_CARRIED,
  KEPT_AUTOMATICALLY,
  KEPT_WHY,
  LOST_WITHOUT_REPLAY,
  MID_RUN_SAFETY,
  REPLAY_COST,
  ROLLBACK_DISK_COST,
  ROLLBACK_SCOPE,
  formatDataSize,
  formatSnapshotDate,
} from "./migrationCopy";

interface Props {
  projectName: string;
  staleness: ContainerStaleness | null;
  migration: ContainerMigration;
  onClose: () => void;
}

function Section({
  title,
  children,
  control,
}: {
  title: string;
  children: React.ReactNode;
  control?: React.ReactNode;
}) {
  return (
    <section className="border border-[var(--border-color)] rounded-[var(--radius-panel)] bg-[var(--bg-secondary)] px-3.5 py-3">
      {control ? (
        <SwitchRow label={title} control={control} />
      ) : (
        <h3 className="text-[13px] font-medium text-[var(--text-primary)]">{title}</h3>
      )}
      <div className="mt-2 space-y-1.5">{children}</div>
    </section>
  );
}

function BulletList({ items, mono = false }: { items: string[]; mono?: boolean }) {
  return (
    <ul className="space-y-1 pl-4 list-disc marker:text-[var(--text-disabled)]">
      {items.map((item) => (
        <li
          key={item}
          className={`text-xs leading-snug text-[var(--text-secondary)] ${
            mono ? "font-mono break-all" : ""
          }`}
        >
          {item}
        </li>
      ))}
    </ul>
  );
}

/**
 * Pre-flight, progress and outcome for a base-image migration, in one dialog.
 *
 * Order matters here. The reassurance comes first — almost nothing painful is
 * at risk, because the two volumes re-attach untouched — and only then the
 * short list of things that genuinely have to be put back. Leading with the
 * options would read as "pick which of your data to lose".
 *
 * Once the run starts the dialog stays **dismissible**: this takes minutes, and
 * a modal that blocks the whole app for the duration is worse than no progress
 * UI at all. Closing it hides a view; the work and its log live in the hook.
 */
export default function MigrateContainerModal({
  projectName,
  staleness,
  migration,
  onClose,
}: Props) {
  const [replayPackages, setReplayPackages] = useState(true);
  const [copyPaths, setCopyPaths] = useState(true);
  const [keepRollback, setKeepRollback] = useState(true);
  const logRef = useRef<HTMLDivElement>(null);

  const { running, report, interrupted, log, phaseMessage, busy, probeSettled } =
    migration;
  const aptDelta = staleness?.apt_delta ?? [];
  const npmDelta = staleness?.npm_global_delta ?? [];
  const verbatim = staleness?.verbatim_paths ?? [];
  const atRisk = staleness?.unpreserved_data ?? [];
  const gains = staleness?.missing_features ?? [];
  const snapshot = formatSnapshotDate(staleness?.snapshot_created_at ?? null);

  // Follow the tail of the apt output, the way a terminal would.
  useEffect(() => {
    const el = logRef.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, [log.length]);

  const start = () => {
    const options: MigrationOptions = {
      // Deliberately *not* `&& verbatim.length > 0`. That looked like a
      // harmless optimisation but read the toggle's meaning off a probe that
      // may not have landed, so a null `staleness` sent `copy_paths: false`
      // and the backend — which recomputes the real set but honours the flag —
      // skipped files that did exist. The backend already skips the step when
      // its own set comes out empty; that is the only place that knows.
      replay_packages: replayPackages,
      copy_paths: copyPaths,
      keep_rollback: keepRollback,
    };
    void migration.start(options);
  };

  // ---- Unfinished ---------------------------------------------------------
  // Ahead of the report, for the reason spelled out in MigrationInterruptedCard:
  // Keep is not a legitimate action on a container that is mid-swap.
  if (interrupted) {
    return (
      <Modal
        title={`Update container base — ${projectName}`}
        onClose={onClose}
        widthClassName="w-[34rem]"
        footer={
          <Button size="md" variant="ghost" onClick={onClose}>
            Close
          </Button>
        }
      >
        <MigrationInterruptedCard
          record={interrupted}
          busy={busy || running}
          onResume={() => void migration.resume()}
          onRollback={() => void migration.rollback().then(onClose)}
        />
      </Modal>
    );
  }

  // ---- Outcome ------------------------------------------------------------
  if (report) {
    return (
      <Modal
        title={`Update container base — ${projectName}`}
        onClose={onClose}
        widthClassName="w-[34rem]"
        footer={
          <Button size="md" variant="ghost" onClick={onClose}>
            Close
          </Button>
        }
      >
        <MigrationReportCard
          report={report}
          busy={busy}
          onKeep={() => void migration.keep().then(onClose)}
          onRollback={() => void migration.rollback().then(onClose)}
          onDismiss={() => void migration.dismiss().then(onClose)}
        />
      </Modal>
    );
  }

  // ---- Progress -----------------------------------------------------------
  if (running) {
    return (
      <Modal
        title={`Updating container base — ${projectName}`}
        description="This keeps running if you close it. You can carry on using the app."
        onClose={onClose}
        widthClassName="w-[34rem]"
        footer={
          <Button size="md" variant="ghost" onClick={onClose}>
            Hide
          </Button>
        }
      >
        <div className="space-y-3">
          <p
            role="status"
            aria-live="polite"
            className="text-[13px] text-[var(--text-primary)]"
          >
            {phaseMessage ?? "Starting…"}
          </p>
          <div
            ref={logRef}
            data-testid="migration-log"
            className="h-56 overflow-y-auto px-2.5 py-2 font-mono text-[11px] leading-relaxed text-[var(--text-secondary)] bg-[var(--bg-primary)] border border-[var(--border-color)] rounded-[var(--radius-control)] whitespace-pre-wrap break-all select-text"
          >
            {log.length === 0 ? "Waiting for the first step…" : log.join("\n")}
          </div>
          <p className="text-xs text-[var(--text-secondary)] leading-snug">
            {MID_RUN_SAFETY}
          </p>
        </div>
      </Modal>
    );
  }

  // ---- Pre-flight ---------------------------------------------------------
  return (
    <Modal
      title={`Update container base — ${projectName}`}
      description={
        snapshot
          ? `Rebuilds this container on the current base image. It is running on a saved image from ${snapshot}.`
          : "Rebuilds this container on the current base image."
      }
      onClose={onClose}
      widthClassName="w-[36rem]"
      footer={
        <>
          <Button size="md" variant="ghost" onClick={onClose}>
            Cancel
          </Button>
          <Button
            size="md"
            variant="primary"
            disabled={!probeSettled}
            onClick={start}
          >
            Update container base
          </Button>
        </>
      }
    >
      <div className="space-y-3">
        {/* 0. Until the probe lands, every list below is "not known" wearing
            "empty"'s clothes. Say which one it is, and do not let the run
            start on an unread delta. */}
        {!probeSettled && (
          <section
            className="border border-[var(--warning)]/40 bg-[var(--warning-muted)] rounded-[var(--radius-panel)] px-3.5 py-3"
            role="status"
            aria-live="polite"
          >
            <p className="text-xs text-[var(--text-primary)] leading-snug">
              Still working out what this container has that the current base
              does not. The lists below are not complete until it finishes, so
              the update cannot start yet.
            </p>
          </section>
        )}

        {/* 1. Reassurance first. Not a choice — a statement of fact. */}
        <Section title="Kept automatically">
          <BulletList items={KEPT_AUTOMATICALLY} />
          <p className="text-xs text-[var(--text-secondary)] leading-snug">{KEPT_WHY}</p>
          <p className="text-xs text-[var(--text-secondary)] leading-snug">
            {LOST_WITHOUT_REPLAY}
          </p>
        </Section>

        {/* 1b. The one thing that is genuinely destroyed. Directly under the
            reassurance, because a user who reads only the top of this dialog
            must not come away thinking nothing is at stake. */}
        <section
          className="border border-[var(--error)]/40 bg-[var(--error-muted)] rounded-[var(--radius-panel)] px-3.5 py-3 space-y-2"
          data-testid="migration-unpreserved"
        >
          <h3 className="text-[13px] font-medium text-[var(--text-primary)]">
            {atRisk.length > 0
              ? `Destroyed, and not restored by this update (${atRisk.length})`
              : "Not carried across"}
          </h3>
          <p className="text-xs text-[var(--text-secondary)] leading-snug">
            {DATA_NOT_CARRIED}
          </p>
          {probeSettled ? (
            atRisk.length > 0 ? (
              <ul className="space-y-1 pl-4 list-disc marker:text-[var(--text-disabled)]">
                {atRisk.map((d) => (
                  <li
                    key={d.path}
                    className="text-xs leading-snug text-[var(--text-primary)]"
                  >
                    <span className="font-mono break-all">{d.path}</span>
                    <span className="text-[var(--text-secondary)]">
                      {" "}
                      — {formatDataSize(d.bytes)} in {d.file_count} file
                      {d.file_count === 1 ? "" : "s"}
                    </span>
                  </li>
                ))}
              </ul>
            ) : (
              <p className="text-xs text-[var(--text-secondary)]">
                Nothing was found under <code className="font-mono">/var</code>{" "}
                on this container, so there is nothing here to lose.
              </p>
            )
          ) : (
            <p className="text-xs text-[var(--text-secondary)]">
              Not checked yet.
            </p>
          )}
        </section>

        {/* 2. The apt replay. */}
        <Section
          title={`Reinstalled from the new base's repos (${aptDelta.length})`}
          control={
            <Toggle
              label="Reinstall system packages from the new base's repositories"
              checked={replayPackages}
              onChange={setReplayPackages}
            />
          }
        >
          {aptDelta.length === 0 ? (
            <p className="text-xs text-[var(--text-secondary)]">
              {/* "None found" and "not looked yet" are different sentences.
                  Printing the first while the probe is still running is how a
                  user ends up believing a delta was empty when it was unread. */}
              {probeSettled
                ? "No extra apt packages were found on this container."
                : "Still checking which apt packages this container added."}
            </p>
          ) : (
            <BulletList items={aptDelta} mono />
          )}
          {npmDelta.length > 0 && (
            <>
              <p className="text-xs text-[var(--text-secondary)] pt-1">
                Global npm packages ({npmDelta.length}):
              </p>
              <BulletList items={npmDelta} mono />
            </>
          )}
          <p className="text-xs text-[var(--text-secondary)]">{REPLAY_COST}</p>
        </Section>

        {/* 3. Verbatim copies — usually nothing once the probe has settled, so
            usually not shown at all. Shown while it has not, because a hidden
            section reads as "there is nothing here". */}
        {(verbatim.length > 0 || !probeSettled) && (
          <Section
            title={
              probeSettled
                ? `Copied across as-is (${verbatim.length})`
                : "Copied across as-is"
            }
            control={
              <Toggle
                label="Copy user-authored files across as-is"
                checked={copyPaths}
                onChange={setCopyPaths}
              />
            }
          >
            <p className="text-xs text-[var(--text-secondary)]">
              Content under <code className="font-mono">/usr/local</code>,{" "}
              <code className="font-mono">/opt</code>,{" "}
              <code className="font-mono">/srv</code> and non-bind-mounted{" "}
              <code className="font-mono">/workspace</code> that belongs to no
              package, so it cannot be reinstalled from a repository.
            </p>
            {probeSettled ? (
              <BulletList items={verbatim} mono />
            ) : (
              <p className="text-xs text-[var(--text-secondary)]">
                Still checking what is there.
              </p>
            )}
          </Section>
        )}

        {/* 4. The rollback image, with its real disk cost stated. */}
        <Section
          title="Keep a rollback image until I confirm"
          control={
            <Toggle
              label="Keep a rollback image until I confirm"
              checked={keepRollback}
              onChange={setKeepRollback}
              tone="caution"
            />
          }
        >
          <p className="text-xs text-[var(--text-secondary)] leading-snug">
            {ROLLBACK_DISK_COST}
          </p>
          <p className="text-xs text-[var(--text-secondary)] leading-snug">
            {ROLLBACK_SCOPE}
          </p>
        </Section>

        {gains.length > 0 && (
          <section className="border border-[var(--success)]/40 bg-[var(--success-muted)] rounded-[var(--radius-panel)] px-3.5 py-3">
            <h3 className="text-[13px] font-medium text-[var(--text-primary)]">
              You will gain
            </h3>
            <ul className="mt-1.5 space-y-1">
              {gains.map((feature) => (
                <li
                  key={feature}
                  className="text-xs leading-snug text-[var(--text-secondary)]"
                >
                  <span aria-hidden="true" className="text-[var(--success)]">
                    +{" "}
                  </span>
                  {feature}
                </li>
              ))}
            </ul>
            {/* "A different version", not "behind" — the count measures drift
                from the base, not a guarantee that each one is an upgrade. */}
            {(staleness?.outdated_package_count ?? 0) > 0 && (
              <p className="mt-1.5 text-xs text-[var(--text-secondary)]">
                Plus {staleness?.outdated_package_count} package
                {staleness?.outdated_package_count === 1 ? "" : "s"} the current
                base carries at a different version, security updates among them.
              </p>
            )}
          </section>
        )}
      </div>
    </Modal>
  );
}
