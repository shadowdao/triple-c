/**
 * The one byte formatter.
 *
 * The app had four of them — `projects/home/format.ts`,
 * `projects/migrationCopy.ts`, `settings/UpdateDialog.tsx` and an inline
 * `toFixed(1)` in `useProjectActions.ts` — disagreeing about the divisor, the
 * unit labels and the precision. All four now delegate here, and there are no
 * remaining copies.
 *
 * The last two were held back because re-pointing them changes what they
 * render, and that turned out to be the argument for doing it rather than
 * against. `UpdateDialog` rendered KB at `toFixed(0)` (`512 KB` is now
 * `512.0 KB`, consistent with every other size in the app) and both stopped
 * the ladder at MB, so a 2 GB asset or backup read as a five-digit number of
 * megabytes. Both are `{ binary: true }`: they describe files, and a host file
 * browser shows the ÷1024 figure for the same bytes.
 *
 * ## Why the default is base 1000
 *
 * Anything explaining what Docker reports has to match it, and Docker formats
 * every size it prints with `units.HumanSize`, which is **base 1000**. Showing
 * 26.1 GB where the user's terminal said 28.0 GB for the same object would read
 * as a bug in the app. So decimal is the default and binary is opt-in, rather
 * than the other way round.
 *
 * Both existing conventions are preserved for every size either call site can
 * realistically produce — a file size or a payload size, i.e. a non-negative
 * finite number below a terabyte. Outside that range this deliberately differs
 * from what it replaced: a negative or `NaN` input now renders `—` rather than
 * `-1 B` or `NaN GB`, and the unit ladder continues past GB instead of
 * stopping there.
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

  // **Promote again if rounding pushed the value back up to a whole step.**
  // `toFixed` runs after the loop, so 999,999 B divides to 999.999 KB and then
  // renders as "1000.0 KB" — a unit the loop had already decided against. The
  // same happens at every boundary (999,999,999 → "1000.0 MB", and 1,048,575
  // → "1024.0 KB" in binary).
  if (unit < units.length - 1 && Number(value.toFixed(precision)) >= step) {
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
 *
 * **No caller on this branch**, for the same reason as [`formatBytesCeiling`]:
 * the Disk panel's per-project table was the last one, and it went to
 * `hold/disk-and-dragout`.
 */
export function formatBytesDelta(bytes: number, options?: FormatBytesOptions): string {
  const formatted = formatBytes(bytes, options);
  return formatted === "—" ? formatted : `+${formatted}`;
}

/**
 * `up to 12.3 GB` — for a bound rather than a measurement.
 *
 * A figure that cannot be known until an operation runs must not render like
 * one that was measured; going through a different function is what stops it
 * being read as a promise.
 *
 * **No caller on this branch.** Its last one was the Disk panel's projected
 * compaction yield, which went to `hold/disk-and-dragout`. Kept with its tests
 * because the distinction it encodes is the reusable part.
 */
export function formatBytesCeiling(bytes: number, options?: FormatBytesOptions): string {
  if (!Number.isFinite(bytes) || bytes <= 0) return "an unknown amount";
  return `up to ${formatBytes(bytes, options)}`;
}
