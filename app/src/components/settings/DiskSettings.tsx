import { useState } from "react";
import Button from "../ui/Button";
import StatusIndicator, { type StatusTone } from "../ui/StatusIndicator";
import Modal from "../ui/Modal";
import TypedConfirmModal from "../ui/TypedConfirmModal";
import DiskProjectTable from "./DiskProjectTable";
import { useDiskUsage } from "../../hooks/useDiskUsage";
import { formatBytes, formatBytesCeiling } from "../../lib/formatBytes";
import type { DestructiveItem, ReclaimItem, ReclaimTarget } from "../../lib/types";

/** A stable key for a target, so ticks survive a re-plan. */
function targetKey(target: ReclaimTarget): string {
  return JSON.stringify(target);
}

/**
 * Where the disk went, and how to get it back.
 *
 * ## Why the scan is a button
 *
 * `getDockerDiskUsage` is `GET /system/df`, which walks every image, container
 * and volume on the daemon computing shared-layer sizes — seconds on a 100 GB
 * store, and the only call that produces those numbers at all. So nothing here
 * runs on open, on a timer, or on a re-render.
 *
 * ## Why the buckets are separated the way they are
 *
 * Safe work (dangling images, ownerless pins, build cache, volumes whose
 * project id is not in the project store) gets one list of ticks and one
 * button, because none of it can lose anything a user has. Note what the last
 * of those is derived from: membership in Triple-C's own project list, never
 * "this project has no container" — an idle live project looks exactly like a
 * deleted one from the daemon's side, and mistaking the two would delete
 * credentials and transcripts. Semi-safe work (compaction, cache clearing) is a rewrite or a
 * re-download and is confirmed one at a time. Destructive work — a live
 * project's volumes, its snapshot, a live rollback pin — is not in either list:
 * it is reached only from that project's own row, behind a typed confirmation,
 * and the backend refuses it in bulk by taking a different type entirely.
 */
export default function DiskSettings() {
  const { report, plan, scanning, working, error, outcome, scan, runReclaim, destroy } =
    useDiskUsage();
  const [ticked, setTicked] = useState<Set<string>>(new Set());
  const [confirming, setConfirming] = useState<ReclaimItem | null>(null);
  const [destroying, setDestroying] = useState<DestructiveItem | null>(null);

  const safeItems = plan?.items.filter((i) => i.safety === "safe") ?? [];
  const semiItems = plan?.items.filter((i) => i.safety === "semi_safe") ?? [];
  const selected = safeItems.filter(
    (i) => i.blocked === null && ticked.has(targetKey(i.target)),
  );
  const selectedBytes = selected.reduce((sum, i) => sum + i.bytes, 0);

  const toggle = (item: ReclaimItem) => {
    setTicked((prev) => {
      const next = new Set(prev);
      const key = targetKey(item.target);
      if (next.has(key)) next.delete(key);
      else next.add(key);
      return next;
    });
  };

  const tone: StatusTone = scanning ? "unknown" : report ? "ok" : "off";
  const statusLabel = scanning
    ? "Scanning"
    : report
      ? `Scanned ${new Date(report.scanned_at).toLocaleTimeString()}`
      : "Not scanned";

  return (
    <div className="space-y-4 text-[13px]">
      {/* --- Why this section exists ------------------------------------- */}
      <p className="text-xs text-[var(--text-secondary)] leading-relaxed">
        Every time a container is recreated, Triple-C commits it — and a commit{" "}
        <strong className="text-[var(--text-primary)]">stacks a new layer</strong> rather
        than rewriting the old one. Deleting a file afterwards writes a whiteout; the
        bytes underneath stay forever. Twenty-four different settings changes trigger a
        recreation, so a project can quietly accumulate a dozen multi-gigabyte layers it
        no longer uses any of.
      </p>

      {/* --- Scan --------------------------------------------------------- */}
      <div className="flex items-center gap-3 flex-wrap">
        <Button variant="primary" size="md" onClick={scan} disabled={scanning}>
          {scanning ? "Scanning…" : report ? "Scan again" : "Scan"}
        </Button>
        <StatusIndicator tone={tone} label={statusLabel} className="text-xs" />
        <span className="text-xs text-[var(--text-secondary)]">
          Reads the whole Docker store; takes a few seconds on a large one.
        </span>
      </div>

      {error && (
        <p className="text-xs text-[var(--error)]" role="alert">
          {error}
        </p>
      )}

      {!report && !scanning && (
        <p className="text-xs text-[var(--text-secondary)]">
          Nothing has been measured yet. Scanning is the only thing here that costs
          anything, so it is never done for you.
        </p>
      )}

      {report && (
        <>
          {/* --- Windows / WSL2, mandatory when it applies ----------------- */}
          {report.host.vhdx_applies && (
            <section
              className="border border-[var(--warning)]/40 bg-[var(--warning-muted)] rounded-[var(--radius-panel)] px-3.5 py-3 space-y-2"
              data-testid="disk-vhdx-note"
            >
              <StatusIndicator
                tone="error"
                label="Reclaiming here will not shrink your C: drive"
                className="text-xs"
              />
              <p className="text-xs text-[var(--text-primary)] leading-relaxed">
                {report.host.vhdx_note}
              </p>
              <p className="text-xs text-[var(--text-secondary)]">
                To actually give the space back to C:, run these in PowerShell as
                administrator after reclaiming:
              </p>
              <pre className="text-[11px] font-mono bg-[var(--bg-primary)] border border-[var(--border-color)] rounded-[var(--radius-control)] px-2.5 py-2 overflow-x-auto select-text">
                {report.host.vhdx_fix.join("\n")}
              </pre>
              <p className="text-xs text-[var(--text-secondary)]">
                Or, without Hyper-V: {report.host.vhdx_fix_gui}.
              </p>
            </section>
          )}

          {/* --- Per-project table ---------------------------------------- */}
          <section className="space-y-2">
            <h3 className="text-[13px] font-medium text-[var(--text-primary)]">
              By project
            </h3>
            <DiskProjectTable
              rows={report.projects}
              destructive={plan?.destructive ?? []}
              onDestroy={setDestroying}
            />
          </section>

          {/* --- Globals --------------------------------------------------- */}
          <section className="space-y-2" data-testid="disk-globals">
            <h3 className="text-[13px] font-medium text-[var(--text-primary)]">
              Shared and left over
            </h3>
            <dl className="grid grid-cols-[1fr_auto] gap-x-4 gap-y-1 text-xs">
              <dt className="text-[var(--text-secondary)]">
                Base images ({report.base_images.length}) — shared by every project
              </dt>
              <dd className="text-right tabular-nums">
                {formatBytes(report.base_images_bytes)}
              </dd>

              <dt className="text-[var(--text-secondary)]">
                Superseded images from past recreations ({report.orphan_image_count})
              </dt>
              <dd className="text-right tabular-nums">
                {formatBytes(report.orphan_image_bytes)}
              </dd>

              <dt className="text-[var(--text-secondary)]">
                Volumes with no matching project in Triple-C (
                {report.orphan_volumes.length})
              </dt>
              <dd className="text-right tabular-nums">
                {formatBytes(report.orphan_volume_bytes)}
              </dd>

              <dt className="text-[var(--text-secondary)]">
                Build cache — <strong className="text-[var(--warning)]">whole daemon</strong>,
                not just Triple-C{" "}
                <span className="text-[var(--text-disabled)]">
                  (via {report.build_cache.source})
                </span>
              </dt>
              <dd className="text-right tabular-nums">
                {formatBytes(report.build_cache.reclaimable_bytes)} of{" "}
                {formatBytes(report.build_cache.total_bytes)}
              </dd>

              <dt className="text-[var(--text-primary)] font-medium pt-1 border-t border-[var(--border-color)]">
                Attributable to Triple-C
              </dt>
              <dd className="text-right tabular-nums text-[var(--text-primary)] font-medium pt-1 border-t border-[var(--border-color)]">
                {formatBytes(report.triple_c_total_bytes)}
              </dd>

              <dt className="text-[var(--text-secondary)]">
                Everything on this daemon, yours included
              </dt>
              <dd className="text-right tabular-nums">
                {formatBytes(
                  report.images_total_bytes +
                    report.containers_total_bytes +
                    report.volumes_total_bytes,
                )}
              </dd>
            </dl>
            {report.orphan_volumes.length > 0 && (
              <p className="text-[11px] text-[var(--text-secondary)] leading-relaxed">
                That last figure means only that the volume&rsquo;s project id is not in
                your project list &mdash; it is <em>not</em> inferred from a project
                being stopped or having no image. A project you have not opened in a
                while has no container and no snapshot either, and that is normal, so
                each of these is ticked individually and shows the date Docker created
                it.
              </p>
            )}
            <p className="text-[11px] text-[var(--text-secondary)]">
              Docker stores this at{" "}
              <span className="font-mono">{report.host.docker_root_dir || "an unknown path"}</span>
              {report.host.is_docker_desktop && " — a path inside the Docker Desktop VM, not on your filesystem"}.
            </p>
          </section>

          {/* --- Store failure, if any ------------------------------------ */}
          {report.orphan_volumes_unavailable && (
            <section
              className="border border-[var(--warning)]/40 bg-[var(--warning-muted)] rounded-[var(--radius-panel)] px-3.5 py-3"
              data-testid="disk-store-error"
            >
              <StatusIndicator
                tone="error"
                label="Could not read the project list"
                className="text-xs"
              />
              <p className="mt-1.5 text-xs text-[var(--text-primary)] leading-relaxed">
                {report.orphan_volumes_unavailable}
              </p>
            </section>
          )}

          {/* --- Safe reclaim ---------------------------------------------- */}
          <section className="space-y-2" data-testid="disk-safe-bucket">
            <h3 className="text-[13px] font-medium text-[var(--text-primary)]">
              Safe to reclaim
            </h3>
            {safeItems.length === 0 ? (
              <p className="text-xs text-[var(--text-secondary)]">
                Nothing here — no leftovers were found.
              </p>
            ) : (
              <>
                <p className="text-xs text-[var(--text-secondary)]">
                  None of this is reachable any more, or all of it regenerates on demand.
                  Nothing you have made is in this list.
                </p>
                <ul className="space-y-1.5">
                  {safeItems.map((item) => {
                    const key = targetKey(item.target);
                    return (
                      <li key={key}>
                        <label className="flex items-start gap-2.5 cursor-pointer">
                          <input
                            type="checkbox"
                            checked={ticked.has(key)}
                            disabled={item.blocked !== null}
                            onChange={() => toggle(item)}
                            className="mt-0.5 accent-[var(--accent-emphasis)]"
                          />
                          <span className="flex-1 min-w-0">
                            <span className="flex items-baseline justify-between gap-3">
                              <span
                                className={
                                  item.blocked
                                    ? "text-[var(--text-disabled)]"
                                    : "text-[var(--text-primary)]"
                                }
                              >
                                {item.label}
                                {item.daemon_wide && (
                                  <span className="ml-1.5 text-[11px] text-[var(--warning)] border border-[var(--warning)]/40 rounded-[var(--radius-control)] px-1 py-px">
                                    whole daemon
                                  </span>
                                )}
                              </span>
                              <span className="tabular-nums whitespace-nowrap text-[var(--text-secondary)]">
                                {formatBytes(item.bytes)}
                              </span>
                            </span>
                            <span className="block text-xs text-[var(--text-secondary)] leading-snug">
                              {item.detail}
                            </span>
                            {item.blocked && (
                              <span className="block text-xs text-[var(--text-disabled)]">
                                {item.blocked}
                              </span>
                            )}
                          </span>
                        </label>
                      </li>
                    );
                  })}
                </ul>
                <div className="flex items-center gap-3">
                  <Button
                    variant="primary"
                    size="md"
                    disabled={selected.length === 0 || working}
                    onClick={() => runReclaim(selected.map((i) => i.target))}
                  >
                    {working ? "Reclaiming…" : "Reclaim"}
                  </Button>
                  <span className="text-xs text-[var(--text-secondary)]">
                    {selected.length === 0
                      ? "Nothing ticked."
                      : `${selected.length} selected, ${formatBytes(selectedBytes)}.`}
                  </span>
                </div>
              </>
            )}
          </section>

          {/* --- Semi-safe -------------------------------------------------- */}
          {semiItems.length > 0 && (
            <section className="space-y-2" data-testid="disk-semi-bucket">
              <h3 className="text-[13px] font-medium text-[var(--text-primary)]">
                Worth doing, one at a time
              </h3>
              <p className="text-xs text-[var(--text-secondary)]">
                Nothing here loses anything you have installed. Compacting rewrites a
                project&rsquo;s stacked layers into one; clearing caches deletes files
                that refill themselves. Both take a moment and both are confirmed
                separately.
              </p>
              <ul className="space-y-1.5">
                {semiItems.map((item) => (
                  <li
                    key={targetKey(item.target)}
                    className="flex items-start justify-between gap-3"
                  >
                    <span className="flex-1 min-w-0">
                      <span className="block text-[var(--text-primary)]">{item.label}</span>
                      <span className="block text-xs text-[var(--text-secondary)] leading-snug">
                        {item.detail}
                      </span>
                      {item.blocked && (
                        <span className="block text-xs text-[var(--text-disabled)]">
                          {item.blocked}
                        </span>
                      )}
                    </span>
                    <span className="flex items-center gap-2 whitespace-nowrap">
                      <span className="text-xs text-[var(--text-secondary)] tabular-nums">
                        {/* A bound, not a measurement — rendered through a
                            different helper so it cannot read as a promise. */}
                        {item.bytes_are_exact
                          ? formatBytes(item.bytes)
                          : formatBytesCeiling(item.bytes)}
                      </span>
                      <Button
                        size="sm"
                        disabled={item.blocked !== null || working}
                        onClick={() => setConfirming(item)}
                      >
                        Run…
                      </Button>
                    </span>
                  </li>
                ))}
              </ul>
            </section>
          )}

          {/* --- Sweep ------------------------------------------------------ */}
          <section className="flex items-center gap-3 flex-wrap">
            <Button
              size="sm"
              disabled={working}
              onClick={() => runReclaim([{ kind: "dangling_snapshots" }])}
            >
              Sweep superseded images now
            </Button>
            <span className="text-xs text-[var(--text-secondary)]">
              The same sweep that runs at startup and after every recreation — here you
              can see what it found.
            </span>
          </section>
        </>
      )}

      {/* --- Outcome ------------------------------------------------------- */}
      {outcome && (
        <section
          className="border border-[var(--border-color)] bg-[var(--bg-primary)] rounded-[var(--radius-panel)] px-3.5 py-3 space-y-1.5"
          role="status"
          aria-live="polite"
          data-testid="disk-outcome"
        >
          <StatusIndicator
            tone={outcome.results.every((r) => r.ok) ? "ok" : "error"}
            label={`Reclaimed ${formatBytes(outcome.total_freed_bytes)}`}
            className="text-xs"
          />
          <ul className="space-y-1 text-xs text-[var(--text-secondary)]">
            {outcome.results.map((result, index) => (
              <li key={index}>
                {result.message}
                {result.projected_bytes !== null && (
                  <>
                    {" "}
                    <span className="text-[var(--text-disabled)]">
                      (projected {formatBytesCeiling(result.projected_bytes)}, actually{" "}
                      {formatBytes(result.freed_bytes)})
                    </span>
                  </>
                )}
              </li>
            ))}
          </ul>
        </section>
      )}

      {/* --- Semi-safe confirmation ---------------------------------------- */}
      {confirming && (
        <Modal
          title={confirming.label}
          onClose={() => setConfirming(null)}
          widthClassName="w-[30rem]"
          footer={
            <>
              <Button size="md" variant="ghost" onClick={() => setConfirming(null)}>
                Cancel
              </Button>
              <Button
                size="md"
                variant="primary"
                disabled={working}
                onClick={() => {
                  const target = confirming.target;
                  setConfirming(null);
                  void runReclaim([target]);
                }}
              >
                {working ? "Working…" : "Run it"}
              </Button>
            </>
          }
        >
          <div className="space-y-2.5 text-[13px] text-[var(--text-secondary)]">
            <p>{confirming.detail}</p>
            {confirming.target.kind === "compact_snapshot" && (
              <>
                <p>
                  The snapshot is rebuilt into a single layer while the old one is left
                  in place, so a failure at any point leaves this project exactly as it
                  is now.
                </p>
                <p>
                  How much comes back depends on how much of those layers a later one
                  already replaced &mdash; it could be{" "}
                  {formatBytesCeiling(confirming.bytes)}, and it could be nothing at all.
                  You will be told the real figure when it finishes.
                </p>
                <p>
                  One thing worth knowing: the rewritten image no longer shares the base
                  image with your other projects, so it carries its own copy of it. That
                  cost is already subtracted from the figure above, and if the rewrite
                  turns out not to come out ahead it is thrown away and the snapshot is
                  left exactly as it is.
                </p>
              </>
            )}
            {confirming.target.kind === "clear_caches" &&
              confirming.target.include_rustup && (
                <p>
                  Rust toolchains are included in this one. They are regenerable, but
                  getting them back is a download rather than a rebuild.
                </p>
              )}
          </div>
        </Modal>
      )}

      {/* --- Destructive confirmation --------------------------------------- */}
      {destroying && (
        <TypedConfirmModal
          title={`Delete ${destroying.label.toLowerCase()}`}
          expected={destroying.project_name}
          confirmLabel={`Delete ${destroying.label.toLowerCase()}`}
          busy={working}
          onCancel={() => setDestroying(null)}
          onConfirm={(typed) => {
            const target = destroying.target;
            setDestroying(null);
            void destroy(target, typed);
          }}
        >
          <p>
            This removes{" "}
            <strong className="text-[var(--text-primary)]">
              {destroying.project_name}
            </strong>
            &rsquo;s {destroying.label.toLowerCase()}, freeing{" "}
            {formatBytes(destroying.bytes)}.
          </p>
          <p className="text-[var(--error)]">{destroying.loses}</p>
          <p>
            Your mounted project folders live on the host and are not affected by this.
          </p>
        </TypedConfirmModal>
      )}
    </div>
  );
}
