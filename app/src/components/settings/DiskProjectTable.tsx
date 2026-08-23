import OverflowMenu from "../ui/OverflowMenu";
import Tooltip from "../ui/Tooltip";
import StatusIndicator from "../ui/StatusIndicator";
import { formatBytes, formatBytesDelta } from "../../lib/formatBytes";
import type { DestructiveItem, ProjectDiskRow } from "../../lib/types";

interface Props {
  rows: ProjectDiskRow[];
  /** Per-project destructive objects, keyed off the same rows. */
  destructive: DestructiveItem[];
  onDestroy: (item: DestructiveItem) => void;
}

const LAYERS_HELP =
  "Commit layers stacked above the base image — one for every time this project's container was recreated. Nothing merges them, so each one is paid for permanently until the snapshot is compacted.";

const NEXT_COMMIT_HELP =
  "The container's writable layer. This is exactly what the next recreation will stack onto the snapshot, and it never comes back after that.";

/** Why a layer count reads "unknown" rather than as a number. */
const layersUnknownHelp = (layers: number) =>
  `${layers} layers in total, but this project predates the base-image label, so there is no way to tell which of them are commits. Migrating it to the current base restores the count.`;

/** `—` for a column with nothing in it, so an empty cell never reads as zero. */
function cell(bytes: number, present: boolean) {
  return present ? formatBytes(bytes) : "—";
}

/**
 * The per-project table — the mental model users actually have of this app.
 *
 * ## Why "Layers" is a column and not a detail
 *
 * A total tells a user their disk is full. The layer count tells them *why*:
 * every container recreation runs `docker commit`, a commit stacks a layer and
 * never rewrites one, and 24 different settings changes trigger a recreation.
 * A project sitting at 14 layers has paid for fourteen full copies of whatever
 * changed, and no total on its own ever says that.
 *
 * "Next commit adds" is the same fact from the other end: it is the container's
 * writable layer, i.e. exactly what the *next* recreation will bake in
 * permanently. Seeing 868 MB there is what makes Compact worth doing before the
 * next settings change rather than after it.
 */
export default function DiskProjectTable({ rows, destructive, onDestroy }: Props) {
  if (rows.length === 0) {
    return (
      <p className="text-xs text-[var(--text-secondary)]">
        No projects to account for.
      </p>
    );
  }

  return (
    // Wide content scrolls inside its own container; the panel itself must
    // never scroll sideways.
    <div className="overflow-x-auto">
      <table className="w-full text-[13px] border-collapse">
        <caption className="sr-only">
          Disk used by each project, largest first
        </caption>
        <thead>
          <tr className="text-left text-xs text-[var(--text-secondary)]">
            <th scope="col" className="font-medium py-1.5 pr-3">
              Project
            </th>
            <th scope="col" className="font-medium py-1.5 px-3 text-right">
              Snapshot
            </th>
            <th scope="col" className="font-medium py-1.5 px-3 text-right whitespace-nowrap">
              Layers
              {/* `Tooltip` renders a portalled div with no `role` and no
                  `aria-describedby`, so its text reaches no assistive tech and
                  the trigger announces as "Help". These two headers are
                  meaningless without their explanation, so it is also emitted
                  as screen-reader-only text. */}
              <Tooltip text={LAYERS_HELP} />
              <span className="sr-only"> — {LAYERS_HELP}</span>
            </th>
            <th scope="col" className="font-medium py-1.5 px-3 text-right whitespace-nowrap">
              Next commit adds
              <Tooltip text={NEXT_COMMIT_HELP} />
              <span className="sr-only"> — {NEXT_COMMIT_HELP}</span>
            </th>
            <th scope="col" className="font-medium py-1.5 px-3 text-right">
              Home vol
            </th>
            <th scope="col" className="font-medium py-1.5 px-3 text-right">
              Config vol
            </th>
            <th scope="col" className="font-medium py-1.5 px-3 text-right">
              Total
            </th>
            <th scope="col" className="font-medium py-1.5 pl-3">
              <span className="sr-only">Actions</span>
            </th>
          </tr>
        </thead>
        <tbody>
          {rows.map((row) => {
            const mine = destructive.filter((d) => d.project_id === row.project_id);
            return (
              <tr
                key={row.project_id}
                className="border-t border-[var(--border-color)] align-top"
                data-testid={`disk-row-${row.project_id}`}
              >
                <th scope="row" className="font-normal py-1.5 pr-3 text-[var(--text-primary)]">
                  <div className="flex items-center gap-1.5">
                    <span className="truncate max-w-[10rem]">{row.project_name}</span>
                    {row.migrating && (
                      <StatusIndicator
                        tone="busy"
                        label="Migrating"
                        className="text-[11px]"
                      />
                    )}
                  </div>
                  <span className="block text-[11px] text-[var(--text-secondary)] font-mono truncate max-w-[12rem]">
                    {row.project_id}
                  </span>
                </th>
                <td className="py-1.5 px-3 text-right tabular-nums whitespace-nowrap">
                  {/* `null` means the split could not be measured. Rendering it
                      as 0 B would be the one guessed number in this table. */}
                  {cell(
                    row.snapshot_above_base_bytes ?? -1,
                    row.snapshot_exists && row.snapshot_above_base_bytes !== null,
                  )}
                  {row.snapshot_exists && (
                    <span className="block text-[11px] text-[var(--text-secondary)]">
                      {/* The base is shared by every project, so charging it to
                          each row would show the same 4.7 GB eight times. The
                          headline figure is what is unique to this project;
                          the total is here for anyone reconciling against
                          `docker images`. */}
                      {formatBytes(row.snapshot_bytes)} with base
                    </span>
                  )}
                </td>
                <td className="py-1.5 px-3 text-right tabular-nums">
                  {!row.snapshot_exists ? (
                    "—"
                  ) : !row.base_lineage_known ? (
                    // The base this descends from is unknown, so the count
                    // includes the base's own layers and does not mean
                    // "recreations". Saying so beats printing a wrong number.
                    //
                    // The explanation is the only thing standing between
                    // "unknown" and reading as a bug, so it cannot live in the
                    // tooltip alone: `Tooltip` portals a plain div with no
                    // `role` and no `aria-describedby`, and wrapped around
                    // children it has no focus handlers either — so on hover-
                    // less input it is unreachable and to a screen reader it
                    // does not exist. Same treatment as the column headers
                    // above: tooltip for the mouse, `sr-only` text for
                    // everything else.
                    <>
                      <Tooltip text={layersUnknownHelp(row.snapshot_commit_layers)}>
                        <span className="text-[var(--text-secondary)]">unknown</span>
                      </Tooltip>
                      <span className="sr-only">
                        {" "}
                        &mdash; {layersUnknownHelp(row.snapshot_commit_layers)}
                      </span>
                    </>
                  ) : (
                    <span className="text-[var(--text-primary)]">
                      {row.snapshot_commit_layers}
                      {/* Never colour alone: a count worth acting on says so in
                          a word, which is also what a screen reader gets. */}
                      {row.snapshot_commit_layers > 5 && (
                        <span className="ml-1 text-[11px] text-[var(--warning)]">
                          stacked
                        </span>
                      )}
                    </span>
                  )}
                </td>
                <td className="py-1.5 px-3 text-right tabular-nums whitespace-nowrap">
                  {row.container_exists
                    ? formatBytesDelta(row.container_writable_bytes)
                    : "—"}
                </td>
                <td className="py-1.5 px-3 text-right tabular-nums whitespace-nowrap">
                  {cell(row.home_volume_bytes, row.home_volume_present)}
                </td>
                <td className="py-1.5 px-3 text-right tabular-nums whitespace-nowrap">
                  {cell(row.config_volume_bytes, row.config_volume_present)}
                </td>
                <td className="py-1.5 px-3 text-right tabular-nums whitespace-nowrap text-[var(--text-primary)] font-medium">
                  {formatBytes(row.total_bytes)}
                </td>
                <td className="py-1.5 pl-3">
                  {mine.length > 0 && (
                    <OverflowMenu
                      label={`Delete ${row.project_name} data`}
                      items={mine.map((item) => ({
                        label: `Delete ${item.label.toLowerCase()} (${formatBytes(item.bytes)})…`,
                        onSelect: () => onDestroy(item),
                        danger: true,
                        disabled: item.blocked !== null,
                      }))}
                    />
                  )}
                </td>
              </tr>
            );
          })}
        </tbody>
      </table>
    </div>
  );
}
