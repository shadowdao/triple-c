/**
 * The viewer's reload/dirty/conflict rules as a pure reducer (spec §5).
 *
 * Two hashes, deliberately: `baseHash` is what the buffer was loaded from or
 * last saved as -- the save's precondition. `diskHash` is the last full-file
 * hash the poll reported. They differ only for a truncated (read-only) load,
 * where the read's hash covers a prefix and can never equal `sha256sum`; the
 * poll then seeds `diskHash` without triggering a reload.
 *
 * The same prefix-vs-full-file split applies to a poll-driven reload of a
 * truncated file: the fresh read's hash is still only a prefix hash, so a
 * `reloaded` action for a truncated file adopts the *polled* hash as the new
 * `diskHash` rather than the read's own hash. Without this, a large file
 * would re-download on every poll tick forever (spec Decision 2).
 */
import type { ViewerPoll } from "../lib/types";
import { NOT_RUNNING_PREFIX } from "./ipcMessages";

export type DocStatus = "clean" | "dirty";
export type DiskStatus = "same" | "changed" | "gone";

export interface ViewerDocState {
  doc: DocStatus;
  disk: DiskStatus;
  /** Hash the buffer was loaded from / last saved as. */
  baseHash: string | null;
  /** Last known full-file hash on disk (null until known). */
  diskHash: string | null;
  /** The last poll was refused because the project's container is not running. */
  containerDown: boolean;
  /**
   * The last poll failed for any other reason (an unreadable file, a Docker
   * hiccup), with the backend's sentence. Changes on disk go unseen until a
   * poll succeeds, but saving stays possible: the write re-checks the hash.
   */
  pollError: string | null;
  /** Set for one render after a clean reload; UI shows "Reloaded". */
  justReloaded: boolean;
  /** True when the user chose "Overwrite on save" after a disk change. */
  overwrite: boolean;
}

export type ViewerAction =
  | { type: "loaded"; hash: string; truncated: boolean }
  | { type: "edited" }
  | { type: "polled"; poll: ViewerPoll }
  | { type: "poll_failed"; message: string }
  | { type: "reloaded"; hash: string; truncated: boolean; polledHash: string | null }
  | { type: "overwrite_on_save" }
  | { type: "saved"; hash: string; diskHash: string }
  | { type: "save_conflict" }
  | { type: "save_gone" };

export const initialViewerState: ViewerDocState = {
  doc: "clean",
  disk: "same",
  baseHash: null,
  diskHash: null,
  containerDown: false,
  pollError: null,
  justReloaded: false,
  overwrite: false,
};

export function reduceViewer(state: ViewerDocState, action: ViewerAction): ViewerDocState {
  const s = { ...state, justReloaded: false };
  switch (action.type) {
    case "loaded":
      return { ...initialViewerState, baseHash: action.hash, diskHash: action.truncated ? null : action.hash };
    case "edited":
      return { ...s, doc: "dirty" };
    case "polled": {
      const ok = { ...s, containerDown: false, pollError: null };
      if (!action.poll.exists) return { ...ok, disk: "gone" };
      const hash = action.poll.hash;
      if (hash === null) return ok;
      if (ok.diskHash === null) return { ...ok, diskHash: hash, disk: ok.disk === "gone" ? "same" : ok.disk };
      if (hash === ok.diskHash) return { ...ok, disk: ok.disk === "gone" ? "same" : ok.disk };
      // Changed on disk. "Overwrite on save" adopted a base; a further change
      // on disk invalidates it again.
      return { ...ok, diskHash: hash, disk: "changed", overwrite: false };
    }
    case "poll_failed":
      // Only the backend's "Start the project before …" refusal means the
      // container is down; anything else is reported as what it says.
      return action.message.startsWith(NOT_RUNNING_PREFIX)
        ? { ...s, containerDown: true, pollError: null }
        : { ...s, containerDown: false, pollError: action.message };
    case "reloaded":
      return {
        ...s,
        doc: "clean",
        disk: "same",
        baseHash: action.hash,
        diskHash: action.truncated ? action.polledHash : action.hash,
        justReloaded: true,
        overwrite: false,
      };
    case "overwrite_on_save":
      return { ...s, baseHash: s.diskHash, disk: "same", overwrite: true };
    case "saved":
      // The base is always the hash of the bytes written. If the disk already
      // held something else right after the swap, another writer landed after
      // us: the buffer is not what is on disk, so say "Changed on disk" (with
      // Reload / Overwrite) rather than adopt the other writer's hash (M2).
      if (action.diskHash !== action.hash) {
        return { ...s, doc: "dirty", disk: "changed", baseHash: action.hash, diskHash: action.diskHash, overwrite: false };
      }
      return { ...s, doc: "clean", disk: "same", baseHash: action.hash, diskHash: action.hash, overwrite: false };
    case "save_conflict":
      return { ...s, disk: "changed", overwrite: false };
    case "save_gone":
      return { ...s, disk: "gone" };
  }
}

/** What EditorPane does after a poll: nothing, reload silently, or show the banner. */
export function pollEffect(before: ViewerDocState, after: ViewerDocState): "none" | "reload" | "banner" {
  if (after.disk === "gone") return before.disk === "gone" ? "none" : "banner";
  if (after.disk !== "changed") return "none";
  // A clean doc still marked "changed" means its reload failed; retry it
  // rather than leave stale text under a "Changed on disk" badge.
  if (after.doc === "clean") return "reload";
  return after.diskHash === before.diskHash ? "none" : "banner";
}

export function canSave(state: ViewerDocState, editable: boolean): boolean {
  return editable && state.doc === "dirty" && state.disk === "same" && !state.containerDown;
}
