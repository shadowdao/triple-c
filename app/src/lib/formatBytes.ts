/**
 * The one byte formatter.
 *
 * Before this existed the app had four of them — `projects/home/format.ts`,
 * `projects/migrationCopy.ts`, `settings/UpdateDialog.tsx` and an inline
 * `toFixed(1)` in `useProjectActions.ts` — disagreeing about the divisor, the
 * unit labels and the precision. They are now expressed in terms of this.
 *
 * ## Why the default is base 1000
 *
 * The Disk panel exists to explain what `docker system df` reports, and Docker
 * formats every size it prints with `units.HumanSize`, which is **base 1000**.
 * A panel that showed 26.1 GB where the user's terminal said 28.0 GB for the
 * same build cache would read as a bug in the panel. So decimal is the default
 * and binary is opt-in, rather than the other way round.
 *
 * Both existing conventions are preserved exactly, so re-pointing the old
 * call sites changed no rendered string:
 *
 * - `{ }`                        → `41.0 MB`   (decimal, what migration used)
 * - `{ binary: true }`           → `1.5 GB`    (÷1024 with decimal-style
 *                                               labels, what Project Home used
 *                                               — technically a misnomer, but
 *                                               it is the app's convention and
 *                                               changing it is not this
 *                                               feature's business)
 * - `{ binary: true, iec: true }` → `1.5 GiB`  (÷1024 labelled honestly)
 */

const DECIMAL_UNITS = ["B", "KB", "MB", "GB", "TB", "PB"];
const IEC_UNITS = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"];

export interface FormatBytesOptions {
  /** Divide by 1024 instead of 1000. */
  binary?: boolean;
  /** Label binary units as `KiB`/`MiB`/`GiB` rather than `KB`/`MB`/`GB`. */
  iec?: boolean;
  /** Decimal places above `B`. Bytes are always whole. */
  precision?: number;
}

export function formatBytes(bytes: number, options: FormatBytesOptions = {}): string {
  const { binary = false, iec = false, precision = 1 } = options;

  // A negative or non-finite size is a bug upstream, not something to render as
  // `NaN GB` in the middle of a table. Docker reports -1 for "not computed",
  // and that is the case this actually catches.
  if (!Number.isFinite(bytes) || bytes < 0) return "—";

  const step = binary ? 1024 : 1000;
  const units = binary && iec ? IEC_UNITS : DECIMAL_UNITS;

  let value = bytes;
  let unit = 0;
  while (value >= step && unit < units.length - 1) {
    value /= step;
    unit += 1;
  }
  // Whole bytes never get a decimal point: `512 B`, not `512.0 B`.
  return unit === 0
    ? `${Math.round(bytes)} ${units[0]}`
    : `${value.toFixed(precision)} ${units[unit]}`;
}

/**
 * `12.3 GB` → `+12.3 GB`, for a figure that is being *added* rather than
 * measured. Used for "next commit adds …", which is the number that explains
 * why a snapshot grows.
 */
export function formatBytesDelta(bytes: number, options?: FormatBytesOptions): string {
  const formatted = formatBytes(bytes, options);
  return formatted === "—" ? formatted : `+${formatted}`;
}

/**
 * `up to 12.3 GB` / `nothing` — for a bound rather than a measurement.
 *
 * The Disk panel is careful about this distinction: every figure it shows is
 * measured except a compaction's yield, which cannot be known until it runs.
 * Rendering that one through a different function is what stops it being read
 * as a promise.
 */
export function formatBytesCeiling(bytes: number, options?: FormatBytesOptions): string {
  if (!Number.isFinite(bytes) || bytes <= 0) return "an unknown amount";
  return `up to ${formatBytes(bytes, options)}`;
}
