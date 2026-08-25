/** Shared formatting helpers for the Project Home views. */

import { formatBytes as shared } from "../../../lib/formatBytes";

/**
 * File sizes in Project Home, ÷1024 with `KB`/`MB`/`GB` labels.
 *
 * Kept as a named re-export rather than deleted: three modules import it from
 * here, and the binary/decimal-label pairing is a Project Home convention
 * rather than the app-wide default. The implementation is `lib/formatBytes`.
 */
export function formatBytes(bytes: number): string {
  return shared(bytes, { binary: true });
}

/** "2h ago" / "3d ago". Returns null for unparseable timestamps. */
export function formatAge(iso: string | null | undefined): string | null {
  if (!iso) return null;
  const then = Date.parse(iso);
  if (Number.isNaN(then)) return null;
  return formatElapsed(Date.now() - then);
}

export function formatElapsed(ms: number): string {
  const seconds = Math.max(0, Math.floor(ms / 1000));
  if (seconds < 60) return "just now";
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return `${minutes}m ago`;
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return `${hours}h ${minutes % 60}m ago`;
  const days = Math.floor(hours / 24);
  return `${days}d ago`;
}

/** "for 42s" / "for 4m" / "for 1h 12m" — elapsed phrasing for a run in flight.
 *  Seconds are kept below a minute because the first thing anyone wants from a
 *  freshly triggered run is evidence that it started at all. */
export function formatRunningFor(iso: string | null | undefined): string | null {
  if (!iso) return null;
  const started = Date.parse(iso);
  if (Number.isNaN(started)) return null;
  const seconds = Math.max(0, Math.floor((Date.now() - started) / 1000));
  if (seconds < 60) return `for ${seconds}s`;
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return `for ${minutes}m`;
  const hours = Math.floor(minutes / 60);
  return `for ${hours}h ${minutes % 60}m`;
}

/** Uptime phrasing for a known start timestamp. */
export function formatUptime(startedAtMs: number | undefined): string | null {
  if (startedAtMs === undefined) return null;
  const seconds = Math.floor((Date.now() - startedAtMs) / 1000);
  if (seconds < 60) return "just started";
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return `up ${minutes}m`;
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return `up ${hours}h ${minutes % 60}m`;
  return `up ${Math.floor(hours / 24)}d`;
}
