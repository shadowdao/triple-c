import { useCallback, useRef, useState } from "react";
import type { FileEntry } from "../lib/types";
import * as commands from "../lib/tauri-commands";
import { useAppState } from "../store/appState";
import { errorText, readableRefusal } from "../lib/refusalText";
import { formatBytes } from "../lib/formatBytes";

/**
 * ## Where failures are reported
 *
 * Two audiences, two places, and the split is deliberate.
 *
 * The **initial listing** failure stays in `error`, rendered inline above the
 * (empty) grid. It is on screen, it is in context, it explains why there are
 * no rows, and it is not transient — it stands until the directory lists.
 *
 * Every **transient operation** failure — rename, create folder, upload, save
 * to host — goes to `ToastHost` instead. Those used to land in the same inline `error` div, which
 * is the first child of the *scrolling* list: three hundred rows down, a
 * refused rename produced no visible change at all, just a rename box that
 * stayed open for no stated reason. The toast host is a persistent `aria-live`
 * region at `z-[60]`, i.e. the one place in the app that is above a modal and
 * does not scroll away.
 *
 * ## Where the current directory lives
 *
 * `currentPath` is state (the UI renders it) *and* a ref (async work reads it
 * after an await). Every long operation captures the directory it targets at
 * the start and compares it against the ref at the end: a slow rename in
 * `/workspace` must not drag the pane back out of `src/` because that is where
 * the closure happened to be created. The ref moves at the *start* of a
 * navigation rather than when the listing lands, because the question being
 * asked is "where is the user going", not "what is on screen right now" — and
 * it is put back if that navigation fails.
 */
export function useFileManager(projectId: string) {
  const [currentPath, setCurrentPath] = useState("/workspace");
  const [entries, setEntries] = useState<FileEntry[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  /**
   * What just finished, for the live region — a rename or a new folder is a
   * change a sighted user sees in the grid and a screen reader user does not.
   */
  const [completed, setCompleted] = useState<string | null>(null);
  /**
   * Which host transfers are in flight.
   *
   * Both actions open an OS dialog and can then run for a long time on a large
   * file, with nothing on screen to say so. Without this the buttons stay live:
   * a second click opens a second dialog and runs a second concurrent exec
   * against the same file, and a multi-gigabyte save is indistinguishable from
   * a click that did nothing.
   *
   * `savingPaths` is a **set**, not one path. Keeping only the row being
   * disabled is what makes the pane usable during a big transfer — and that is
   * precisely what makes a *second* save startable, so the state has to be able
   * to hold two. As a scalar it could not: starting a save on `notes.txt` while
   * `big.bin` was still streaming overwrote it, so `big.bin`'s button went live
   * again mid-transfer; and whichever save finished first cleared the flag for
   * both. Dismissing the second dialog was enough to do it.
   *
   * Paths are unique within a listing, so a path is a usable key — `FilesTab`
   * relies on the same fact for its row keys.
   */
  const [uploading, setUploading] = useState(false);
  const [savingPaths, setSavingPaths] = useState<ReadonlySet<string>>(new Set());

  const currentPathRef = useRef(currentPath);

  /**
   * A slow listing can land after a newer one and set both the rows and the
   * breadcrumb back to a directory the user already left. Same generation
   * guard `useContainerMigration` uses: every async write
   * checks it is still the newest before it lands.
   */
  const navGeneration = useRef(0);

  /**
   * Report a failed operation, given the headline this hook would write and the
   * raw failure behind it.
   *
   * The headline is what the *hook* knows ("Could not rename …"); it is a
   * category, not an explanation. Some backend refusals are already a finished
   * sentence written for the person reading it — a container path outside the
   * roots this panel may change, a name it will not create — and those used to
   * arrive as the toast's `detail`, which `ToastHost` renders as collapsed
   * monospace behind a "Details" button. So the sentence that said what was
   * wrong and what to do about it was hidden under a headline that said
   * neither. When there is such a sentence it becomes the headline, and there
   * is nothing left to hide.
   */
  const report = useCallback((message: string, cause: unknown) => {
    const promoted = readableRefusal(cause);
    useAppState.getState().pushToast({
      kind: "error",
      message: promoted ?? message,
      detail: promoted ? undefined : errorText(cause),
    });
  }, []);

  const navigate = useCallback(
    async (path: string) => {
      const mine = ++navGeneration.current;
      const previous = currentPathRef.current;
      currentPathRef.current = path;
      setLoading(true);
      setError(null);
      try {
        const result = await commands.listContainerFiles(projectId, path);
        if (navGeneration.current !== mine) return;
        setEntries(result);
        setCurrentPath(path);
      } catch (e) {
        if (navGeneration.current !== mine) return;
        // The move did not happen, so the pane is still where it was — the ref
        // has to agree with the breadcrumb or the next operation will decide
        // it targeted a directory nobody is looking at.
        currentPathRef.current = previous;
        setError(String(e));
      } finally {
        if (navGeneration.current === mine) setLoading(false);
      }
    },
    [projectId],
  );

  const goUp = useCallback(() => {
    const here = currentPathRef.current;
    if (here === "/") return;
    const parent = here.replace(/\/[^/]+$/, "") || "/";
    navigate(parent);
  }, [navigate]);

  const refresh = useCallback(() => {
    navigate(currentPathRef.current);
  }, [navigate]);

  /**
   * Rename in place. `newName` is a bare name — Rust rejects anything with a
   * `/` in it, so this can never turn into a move. Resolves true on success so
   * the caller knows whether to leave edit mode.
   */
  const renameEntry = useCallback(
    async (entry: FileEntry, newName: string) => {
      const trimmed = newName.trim();
      if (!trimmed || trimmed === entry.name) return true;
      const target = currentPathRef.current;
      try {
        await commands.renameContainerPath(projectId, entry.path, trimmed);
        setCompleted(`Renamed "${entry.name}" to "${trimmed}".`);
        if (currentPathRef.current === target) await navigate(target);
        return true;
      } catch (e) {
        report(`Could not rename "${entry.name}"`, e);
        return false;
      }
    },
    [projectId, navigate, report],
  );

  const createFolder = useCallback(
    async (name: string) => {
      const trimmed = name.trim();
      if (!trimmed) return true;
      const target = currentPathRef.current;
      try {
        await commands.createContainerDirectory(projectId, target, trimmed);
        setCompleted(`Created "${trimmed}".`);
        if (currentPathRef.current === target) await navigate(target);
        return true;
      } catch (e) {
        report(`Could not create "${trimmed}"`, e);
        return false;
      }
    },
    [projectId, navigate, report],
  );

  /**
   * Copy host files into the directory on screen.
   *
   * The picker is opened by **Rust**, not here — `upload_files_to_container`
   * shows it, reads what the user chose and never lets a host path near IPC.
   * So this passes a directory and gets back an outcome; `null` means the user
   * dismissed the dialog, which is not a failure and says nothing.
   *
   * One dialog can select several files and they need not agree, hence two
   * lists. Every failure is reported, because "3 of 5 uploaded" without saying
   * which two is not a report. The listing is refreshed once, at the end, and
   * only if the user is still looking at the directory that was targeted.
   */
  const uploadFiles = useCallback(async () => {
    const target = currentPathRef.current;
    setUploading(true);
    try {
      let outcome;
      try {
        outcome = await commands.uploadFilesToContainer(projectId, target);
      } catch (e) {
        // A failure *before* the picker: no container, not running, or a
        // directory this pane may not write to. One toast, not one per file.
        report("Could not upload", e);
        return;
      }
      if (!outcome) return;
      for (const failure of outcome.failures) {
        useAppState.getState().pushToast({ kind: "error", message: failure });
      }
      if (outcome.uploaded.length === 0) return;
      // The directory is named, not implied. `target` is captured at click time
      // and the picker is a modal OS dialog — the user has all the time in the
      // world to browse somewhere else while it is open, and the files land
      // where they started. "Uploaded 2 files." in front of a grid that does
      // not contain them is a worse answer than no message at all.
      const count = outcome.uploaded.length;
      setCompleted(
        `Uploaded ${count === 1 ? "1 file" : `${count} files`} to ${target}.`,
      );
      if (currentPathRef.current === target) await navigate(target);
    } finally {
      // Around the *whole* body, refresh included. Clearing it the moment the
      // command settled put the button back before the re-listing had run, so
      // a second click landed mid-refresh on a grid that was still the old one.
      setUploading(false);
    }
  }, [projectId, navigate, report]);

  /**
   * Save one file out to the host, with Rust opening the save dialog.
   *
   * No refresh: nothing in the container changed. The save dialog is also what
   * asks about overwriting an existing host file, which is why the backend has
   * no collision handling of its own to get wrong. `null` is a dismissal.
   */
  const saveToHost = useCallback(
    async (entry: FileEntry) => {
      setSavingPaths((live) => new Set(live).add(entry.path));
      try {
        const bytes = await commands.downloadContainerFile(projectId, entry.path);
        // `0` is a real answer — an empty file saved is a success — so this
        // tests for the dismissal sentinel, not for falsiness.
        if (bytes === null) return;
        setCompleted(`Saved "${entry.name}" (${formatBytes(bytes)}).`);
      } catch (e) {
        report(`Could not save "${entry.name}"`, e);
      } finally {
        // Remove only this one. A save that finishes while another is still
        // streaming must not re-enable the other's row.
        setSavingPaths((live) => {
          const next = new Set(live);
          next.delete(entry.path);
          return next;
        });
      }
    },
    [projectId, report],
  );

  return {
    currentPath,
    entries,
    loading,
    /** Inline, in-context: why the listing on screen is empty. */
    error,
    /** What the last operation finished doing, for the live region. */
    completed,
    setError,
    navigate,
    goUp,
    refresh,
    renameEntry,
    createFolder,
    uploadFiles,
    saveToHost,
    /** A host transfer is in flight — see the state declarations above. */
    uploading,
    savingPaths,
  };
}
