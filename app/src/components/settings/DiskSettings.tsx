import { useEffect, useState } from "react";
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

/** The same, for a destructive object — never ticked, but still listed. */
function destructiveKey(item: DestructiveItem): string {
  return JSON.stringify(item.target);
}

/**
 * An orphaned volume is confirmed against **its own name**, not a project's.
 *
 * There is no project to name: the whole definition of the variant is that its
 * id matches nothing in the store, and `disk.rs`'s `destroy` takes the orphan
 * arm before it ever looks a project up. `DestructiveItem.project_name` carries
 * the volume name for exactly these items, which is what the gate compares.
 */
function isOrphanVolume(item: DestructiveItem): boolean {
  return item.target.kind === "orphan_volume";
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
 * Safe work (dangling images, ownerless pins, build cache) gets one list of
 * ticks and one button, because none of it can lose anything a user has.
 * Semi-safe work (compaction, cache clearing) is a rewrite or a re-download and
 * is confirmed one at a time. Destructive work — a live project's volumes, its
 * snapshot, a live rollback pin, **and an orphaned volume** — is not in either
 * list: it is reached one object at a time, behind a typed confirmation, and
 * the backend refuses it in bulk by taking a different type entirely.
 *
 * ## Why orphaned volumes are down there and not in the tick list
 *
 * They used to be a `ReclaimTarget` at `Safety::Safe`: a tick and the group
 * Reclaim button, no confirmation. The object behind that tick is a
 * `triple-c-claude-config-*` volume holding a Claude OAuth credential, every
 * plugin and skill installed into that project, and every conversation
 * transcript it ever had — and the *same volume* for a project still in the
 * store required typing the project's name. The only difference between the two
 * is a lookup against `projects.json`, which this app has been wrong about
 * before: a second instance's project is absent from an in-memory list, a
 * corrupt store empties it, a restored data directory empties it too. It once
 * flagged two live projects as orphaned.
 *
 * So "no matching project" means one thing only — the id is not in the project
 * list. It is never inferred from a project being stopped, having no container
 * or having no image; an idle live project looks identical from the daemon's
 * side. Each volume is deleted on its own, against its own name typed out.
 */
export default function DiskSettings() {
  const {
    report,
    plan,
    scanning,
    working,
    error,
    outcome,
    scan,
    runReclaim,
    destroy,
    runSweep,
    clearOutcome,
  } = useDiskUsage();
  const [ticked, setTicked] = useState<Set<string>>(new Set());
  const [confirming, setConfirming] = useState<ReclaimItem | null>(null);
  const [destroying, setDestroying] = useState<DestructiveItem | null>(null);
  // A dialog whose action failed stays open and says so *inside itself*. The
  // hook's `error` is rendered at the top of a panel that is metres of scroll
  // long, so a user who reached a project row through the table would have
  // watched the dialog vanish and seen nothing take its place. This flag is
  // what distinguishes "this dialog's action just failed" from a stale scan
  // error that happened to still be sitting in `error` when it opened.
  const [actionFailed, setActionFailed] = useState(false);

  // The plan is dropped after any reclaim, so a tick can never outlive the row
  // it was made against and be re-fired at an object that is already gone.
  useEffect(() => {
    if (!plan) setTicked(new Set());
  }, [plan]);

  // Split before anything renders. The per-project table keys off
  // `project_id`, and an orphan's id matches no row by definition — so without
  // this split those items are simply invisible, which is how a variant that
  // moved from the tick list to the destructive list can vanish from the UI
  // entirely rather than reappear behind a confirmation.
  const orphanVolumes = plan?.destructive.filter(isOrphanVolume) ?? [];
  // A destructive item is rendered inside its project's row, so one whose
  // project id matches no row would be measured and shown nowhere. That is not
  // hypothetical: `survey_rollback_pins` walks *images*, not projects, and
  // deliberately tolerates an absent project by falling back to the raw id as
  // the display name — so a pin left behind by a deleted project is exactly
  // this case, and it is the multi-GB kind. Anything unmatched gets its own
  // section rather than being silently dropped.
  const rowIds = new Set((report?.projects ?? []).map((r) => r.project_id));
  const projectDestructive =
    plan?.destructive.filter((d) => !isOrphanVolume(d) && rowIds.has(d.project_id)) ?? [];
  const unmatchedDestructive =
    plan?.destructive.filter((d) => !isOrphanVolume(d) && !rowIds.has(d.project_id)) ?? [];

  const safeItems = plan?.items.filter((i) => i.safety === "safe") ?? [];
  const semiItems = plan?.items.filter((i) => i.safety === "semi_safe") ?? [];
  const selected = safeItems.filter(
    (i) => i.blocked === null && ticked.has(targetKey(i.target)),
  );
  const selectedBytes = selected.reduce((sum, i) => sum + i.bytes, 0);

  // Opening or closing either dialog clears the in-dialog failure with it, so
  // one never starts out showing the previous attempt's error.
  const openConfirming = (item: ReclaimItem) => {
    setConfirming(item);
    setActionFailed(false);
  };
  const openDestroying = (item: DestructiveItem) => {
    setDestroying(item);
    setActionFailed(false);
  };
  const closeConfirming = () => {
    setConfirming(null);
    setActionFailed(false);
  };
  const closeDestroying = () => {
    setDestroying(null);
    setActionFailed(false);
  };

  const toggle = (item: ReclaimItem) => {
    setTicked((prev) => {
      const next = new Set(prev);
      const key = targetKey(item.target);
      if (next.has(key)) next.delete(key);
      else next.add(key);
      return next;
    });
  };

  // Counted from the per-result list rather than from a flag: a reclaim of
  // five targets can come back with two failures and a real byte total.
  const failedCount = outcome?.results.filter((r) => !r.ok).length ?? 0;

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
        {/* Disabled while a mutation runs, not only while scanning: a scan
            started on top of a reclaim measures a daemon that is being changed
            underneath it, and the hook can only discard such a result — better
            not to spend the seconds. */}
        <Button variant="primary" size="md" onClick={scan} disabled={scanning || working}>
          {scanning ? "Scanning…" : report ? "Scan again" : "Scan"}
        </Button>
        {/* The status flips between "Scanning", "Scanned HH:MM:SS" and "Not
            scanned" with no other signal. The live region is mounted here
            unconditionally — wrapping it around the indicator only once there
            is something to say would make the region *appear* already
            populated, which is the one shape assistive tech does not announce. */}
        <span role="status" aria-live="polite">
          <StatusIndicator tone={tone} label={statusLabel} className="text-xs" />
        </span>
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
              {/* `StatusIndicator` has no warning tone — `error` would put a
                  red glyph in a warning-toned panel. This is advisory, so it
                  carries its own glyph beside the words rather than relying on
                  the panel's colour. */}
              <p className="text-xs font-medium text-[var(--text-primary)]">
                <span aria-hidden="true">&#9650;</span> Warning: reclaiming here will not
                shrink your C: drive
              </p>
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
              destructive={projectDestructive}
              onDestroy={openDestroying}
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
                {/* Live information about where the figure came from, not a
                    disabled control — `--text-disabled` is ~4.1:1 and fails AA
                    at this size. */}
                <span className="text-[var(--text-secondary)]">
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
            {report.build_cache.cli_error && (
              <p className="text-[11px] text-[var(--warning)]">
                {/* Without this the panel silently shows `docker system df`'s
                    under-reported build-cache figure and the user has no way
                    to know why it disagrees with their terminal. */}
                Build-cache figures fell back to <code>docker system df</code>, which
                under-reports what a prune would free: {report.build_cache.cli_error}
              </p>
            )}
            {report.orphan_volumes.length > 0 && (
              <p className="text-[11px] text-[var(--text-secondary)] leading-relaxed">
                &ldquo;Volumes with no matching project&rdquo; above means only that the
                volume&rsquo;s project id is not in your project list &mdash; it is{" "}
                <em>not</em> inferred from a project being stopped or having no image. A project you have not opened in a
                while has no container and no snapshot either, and that is normal, so
                nothing here is deleted in a group: each one is listed below on its own,
                with the date Docker created it, and removing it takes typing that
                volume&rsquo;s name.
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
              className="border border-[var(--error)]/40 bg-[var(--error-muted)] rounded-[var(--radius-panel)] px-3.5 py-3"
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

          {/* --- The plan was dropped by a reclaim -------------------------- */}
          {!plan && (
            <p className="text-xs text-[var(--text-secondary)]" data-testid="disk-plan-stale">
              The totals above were measured before that last action. Scan again to see
              what is left to reclaim.
            </p>
          )}

          {/* --- Safe reclaim ---------------------------------------------- */}
          {plan && (
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
                            // A tick that survived onto a now-blocked row is
                            // excluded from `selected`, so showing it checked
                            // would make the count disagree with the screen.
                            checked={item.blocked === null && ticked.has(key)}
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
          )}

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
                        onClick={() => openConfirming(item)}
                      >
                        Run…
                      </Button>
                    </span>
                  </li>
                ))}
              </ul>
            </section>
          )}

          {/* --- Destructive leftovers with no project row ------------------- */}
          {unmatchedDestructive.length > 0 && (
            <section className="space-y-2" data-testid="disk-unmatched-bucket">
              <h3 className="text-[13px] font-medium text-[var(--text-primary)]">
                Leftovers from projects no longer in Triple-C
              </h3>
              <p className="text-xs text-[var(--text-secondary)] leading-relaxed">
                These belong to a project id that is not in your project list, so there
                is no row above to show them under. The same caveat as the volumes below
                applies: &ldquo;not in your project list&rdquo; is the only thing this
                means, and an idle live project is indistinguishable from a deleted one
                from Docker&rsquo;s side. Because there is no project name to type, each
                one is confirmed against its project <em>id</em>.
              </p>
              <ul className="space-y-1.5">
                {unmatchedDestructive.map((item) => (
                  <li
                    key={destructiveKey(item)}
                    className="flex items-start justify-between gap-3"
                    data-testid={`disk-unmatched-${destructiveKey(item)}`}
                  >
                    <span className="flex-1 min-w-0">
                      <span className="block text-[var(--text-primary)] font-mono break-all">
                        {item.label}
                      </span>
                      <span className="block text-xs text-[var(--text-secondary)] leading-snug">
                        {item.loses}
                      </span>
                      {item.blocked && (
                        <span className="block text-xs text-[var(--text-secondary)]">
                          {item.blocked}
                        </span>
                      )}
                    </span>
                    <span className="flex items-center gap-2 whitespace-nowrap">
                      <span className="text-xs text-[var(--text-secondary)] tabular-nums">
                        {formatBytes(item.bytes)}
                      </span>
                      <Button
                        size="sm"
                        disabled={item.blocked !== null || working}
                        onClick={() => openDestroying(item)}
                      >
                        Delete&hellip;
                      </Button>
                    </span>
                  </li>
                ))}
              </ul>
            </section>
          )}

          {/* --- Orphaned volumes: destructive, one at a time ---------------- */}
          {orphanVolumes.length > 0 && (
            <section className="space-y-2" data-testid="disk-orphan-bucket">
              <h3 className="text-[13px] font-medium text-[var(--text-primary)]">
                Volumes with no matching project
              </h3>
              <p className="text-xs text-[var(--text-secondary)] leading-relaxed">
                A volume here is one whose project id is not in your project list. That
                is <em>all</em> it means &mdash; it is <em>not</em> inferred from a
                project being stopped, having no container or having no image. An idle
                live project looks exactly the same from Docker&rsquo;s side, and that
                inference has already flagged two live projects here once.
              </p>
              <p className="text-xs text-[var(--text-secondary)] leading-relaxed">
                Deleting a{" "}
                <span className="font-mono">triple-c-claude-config-*</span> volume
                deletes{" "}
                <strong className="text-[var(--text-primary)]">
                  the Claude login credential that project signed in with, every plugin
                  and skill installed into it, and every conversation transcript it ever
                  had
                </strong>
                . A <span className="font-mono">triple-c-home-*</span> volume holds its
                dotfiles, shell history and installed toolchains. There is no other copy
                of either and nothing regenerates, so each one is deleted on its own,
                against that volume&rsquo;s name typed out &mdash; never as part of a
                group.
              </p>
              <ul className="space-y-1.5">
                {orphanVolumes.map((item) => (
                  <li
                    key={destructiveKey(item)}
                    className="flex items-start justify-between gap-3"
                    data-testid={`disk-orphan-${item.project_name}`}
                  >
                    <span className="flex-1 min-w-0">
                      <span className="block text-[var(--text-primary)] font-mono break-all">
                        {item.label}
                      </span>
                      <span className="block text-xs text-[var(--text-secondary)] leading-snug">
                        {item.loses}
                      </span>
                      {item.blocked && (
                        <span className="block text-xs text-[var(--text-disabled)]">
                          {item.blocked}
                        </span>
                      )}
                    </span>
                    <span className="flex items-center gap-2 whitespace-nowrap">
                      <span className="text-xs text-[var(--text-secondary)] tabular-nums">
                        {formatBytes(item.bytes)}
                      </span>
                      <Button
                        size="sm"
                        disabled={item.blocked !== null || working}
                        onClick={() => openDestroying(item)}
                      >
                        Delete&hellip;
                      </Button>
                    </span>
                  </li>
                ))}
              </ul>
            </section>
          )}

          {/* --- Sweep ------------------------------------------------------ */}
          <section className="flex items-center gap-3 flex-wrap">
            <Button size="sm" disabled={working} onClick={runSweep}>
              Sweep superseded images now
            </Button>
            <span className="text-xs text-[var(--text-secondary)]">
              The same sweep that runs at startup and after every recreation. Unlike the
              tick above it also reports what it <em>refused</em> to remove, which is how
              a superseded image pinned by a stopped project shows itself.
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
          <div className="flex items-center justify-between gap-3">
            {/* The headline has to carry the failure in words. A partial
                reclaim that freed something still has a byte figure worth
                printing, so the count is appended to it rather than replacing
                it — and the per-result lines below say *which* ones and why,
                so this stops at how many. */}
            <StatusIndicator
              tone={failedCount === 0 ? "ok" : "error"}
              label={
                failedCount === 0
                  ? `Reclaimed ${formatBytes(outcome.total_freed_bytes)}`
                  : `Reclaimed ${formatBytes(outcome.total_freed_bytes)} — ${failedCount} of ${outcome.results.length} failed`
              }
              className="text-xs"
            />
            <Button size="sm" variant="ghost" onClick={clearOutcome}>
              Dismiss
            </Button>
          </div>
          <ul className="space-y-1 text-xs text-[var(--text-secondary)]">
            {outcome.results.map((result, index) => (
              <li key={index}>
                {result.message}
                {result.projected_bytes !== null && (
                  <>
                    {" "}
                    {/* The comparison that makes a compaction's yield
                        readable — live information, so not the disabled ink. */}
                    <span className="text-[var(--text-secondary)]">
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
          onClose={closeConfirming}
          widthClassName="w-[30rem]"
          footer={
            <>
              <Button size="md" variant="ghost" onClick={closeConfirming}>
                Cancel
              </Button>
              <Button
                size="md"
                variant="primary"
                disabled={working}
                onClick={async () => {
                  // Same reasoning as the destructive modal: a compaction takes
                  // minutes, and the dialog reporting it beats it vanishing —
                  // and if it fails, the dialog is the only place the user is
                  // still looking, so it stays open and reports it here.
                  const ok = await runReclaim([confirming.target]);
                  setActionFailed(!ok);
                  if (ok) setConfirming(null);
                }}
              >
                {working ? "Working…" : "Run it"}
              </Button>
            </>
          }
        >
          <div className="space-y-2.5 text-[13px] text-[var(--text-secondary)]">
            {/* The failure lands here rather than only in the panel's error
                line, which this dialog is covering. */}
            {actionFailed && (
              <p role="alert" className="text-[var(--error)]">
                {error ?? "That did not run. Nothing was changed."}
              </p>
            )}
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
      {destroying && (() => {
        // An orphaned volume has no project, so nothing about this dialog can
        // be phrased in terms of one: the gate takes the volume's own name (as
        // `disk.rs`'s `destroy` does), and the name is never lower-cased on its
        // way to the title, because the comparison the backend makes is
        // case-sensitive and a mangled name in the heading is a name the user
        // cannot type.
        const orphan = isOrphanVolume(destroying);
        // A leftover whose project is gone has no name either. `project_name`
        // is the raw id in that case — which is deliberate on the Rust side and
        // is exactly what `destroy` compares against — so the gate works, but
        // the label has to say "id" or it asks for something that does not
        // exist.
        const ownerless = !orphan && !rowIds.has(destroying.project_id);
        return (
        <TypedConfirmModal
          title={
            orphan
              ? `Delete volume ${destroying.project_name}`
              : `Delete ${destroying.label.toLowerCase()}`
          }
          expected={destroying.project_name}
          subject={orphan ? "volume name" : ownerless ? "project id" : "project name"}
          confirmLabel={orphan ? "Delete volume" : `Delete ${destroying.label.toLowerCase()}`}
          busy={working}
          // A failure here has to land inside the dialog. The panel's own
          // error line is at the top of several screens of scroll, and this
          // dialog was reached from a project row far below it.
          error={actionFailed ? (error ?? "That did not run. Nothing was deleted.") : null}
          onCancel={closeDestroying}
          onConfirm={async (typed) => {
            // The modal stays mounted until the call settles, so its `busy`
            // state is what the user sees while a multi-second volume removal
            // runs. Clearing it first made the whole busy path dead code.
            const ok = await destroy(destroying.target, typed);
            setActionFailed(!ok);
            if (ok) setDestroying(null);
          }}
        >
          {orphan ? (
            <p>
              This removes the volume{" "}
              <strong className="text-[var(--text-primary)] font-mono break-all">
                {destroying.project_name}
              </strong>
              , freeing {formatBytes(destroying.bytes)}. It is offered here for one
              reason only: no project in your list has its id. That is a lookup against
              a file, not a judgement about whether anything is using the volume.
            </p>
          ) : (
            <p>
              This removes{" "}
              <strong className="text-[var(--text-primary)]">
                {destroying.project_name}
              </strong>
              &rsquo;s {destroying.label.toLowerCase()}, freeing{" "}
              {formatBytes(destroying.bytes)}.
            </p>
          )}
          <p className="text-[var(--error)]">{destroying.loses}</p>
          {orphan && (
            <p>
              Nothing here can undo this. If you recognise that project id, close this
              and leave the volume alone until you are certain.
            </p>
          )}
          <p>
            Your mounted project folders live on the host and are not affected by this.
          </p>
        </TypedConfirmModal>
        );
      })()}
    </div>
  );
}
