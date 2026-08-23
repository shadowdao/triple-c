import { useCallback, useEffect, useRef, useState } from "react";
import { save, open as openDialog } from "@tauri-apps/plugin-dialog";
import type { FileEntry } from "../lib/types";
import * as commands from "../lib/tauri-commands";
import { useAppState } from "../store/appState";
import {
  errorText,
  fileExistsPath,
  isFileExistsError,
  readableRefusal,
  type OverwriteChoice,
} from "../lib/uploadErrors";

/**
 * One upload waiting on the user to say whether it may replace what is there.
 * `remaining` is how many files are queued behind this one, which is what
 * decides whether the blanket answers are worth offering.
 */
export interface UploadConflict {
  /** Host file being uploaded. */
  hostPath: string;
  /** Bare name, for the prompt. */
  name: string;
  /** Container directory it is going into. */
  directory: string;
  remaining: number;
}

/** `/a/b/c.txt` and `C:\a\b\c.txt` both give `c.txt`. */
function baseName(path: string): string {
  const parts = path.split(/[\\/]/);
  return parts[parts.length - 1] || path;
}

/**
 * ## Where failures are reported
 *
 * Two audiences, two places, and the split is deliberate.
 *
 * The **initial listing** failure stays in `error`, rendered inline above the
 * (empty) grid. It is on screen, it is in context, it explains why there are
 * no rows, and it is not transient — it stands until the directory lists.
 *
 * Every **transient operation** failure — upload, rename, create folder,
 * save-to-host — goes to `ToastHost` instead. Those used to land
 * in the same inline `error` div, which is the first child of the *scrolling*
 * list: three hundred rows down, a refused rename produced no visible change
 * at all, just a rename box that stayed open for no stated reason. Worse, the
 * file viewer routes its "Save to host…" through the same call, and the viewer
 * is a `fixed inset-0` portal at `z-50` — so that failure reported *behind* the
 * dialog that caused it. The toast host is a persistent `aria-live` region at
 * `z-[60]`, i.e. the one place in the app that is above a modal and does not
 * scroll away.
 *
 * ## Where the current directory lives
 *
 * `currentPath` is state (the UI renders it) *and* a ref (async work reads it
 * after an await). Every long operation captures the directory it targets at
 * the start and compares it against the ref at the end: a 200 MB upload into
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
  /** Transient "uploading 3 files…" style note, shown beside the breadcrumb. */
  const [busy, setBusy] = useState<string | null>(null);
  /**
   * What just finished. A live region that only ever says "uploading…" tells a
   * screen reader user when to start waiting and never when to stop.
   */
  const [completed, setCompleted] = useState<string | null>(null);
  const [conflict, setConflict] = useState<UploadConflict | null>(null);

  const currentPathRef = useRef(currentPath);

  /**
   * A slow listing can land after a newer one and set both the rows and the
   * breadcrumb back to a directory the user already left. Same generation
   * guard `useContainerMigration` uses: every async write
   * checks it is still the newest before it lands.
   */
  const navGeneration = useRef(0);

  const startWork = useCallback((note: string) => {
    setBusy(note);
    setCompleted(null);
  }, []);

  /**
   * Report a failed operation, given the headline this hook would write and the
   * raw failures behind it.
   *
   * The headline is what the *hook* knows ("Could not rename …"); it is a
   * category, not an explanation. Some backend refusals are already a finished
   * sentence written for the person reading it — a hidden host folder, a
   * container path outside the roots this panel may change — and those used to
   * arrive as the toast's `detail`, which `ToastHost` renders as collapsed
   * monospace behind a "Details" button. So the sentence that said what was
   * wrong and what to do about it was hidden under a headline that said
   * neither. When every failure reduces to the *same* such sentence — which is
   * the normal case, since these refusals are about the target directory and so
   * fail identically for every file in a batch — it becomes the headline and
   * there is nothing left to hide.
   */
  const report = useCallback((message: string, ...causes: unknown[]) => {
    const refusals = causes.map(readableRefusal);
    const shared =
      causes.length > 0 && refusals.every((r) => r !== null)
        ? [...new Set(refusals as string[])]
        : [];
    const promoted = shared.length === 1 ? shared[0] : null;
    useAppState.getState().pushToast({
      kind: "error",
      message: promoted ?? message,
      detail: promoted || causes.length === 0 ? undefined : causes.map(errorText).join("\n"),
    });
  }, []);

  const confirm = useCallback((message: string) => {
    useAppState.getState().pushToast({ kind: "success", message });
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

  /** Copy an entry out to a host path the user picks. */
  const downloadFile = useCallback(
    async (entry: FileEntry) => {
      try {
        const hostPath = await save({ defaultPath: entry.name });
        if (!hostPath) return;
        // Every sibling operation sets `busy`; this one did not, so a 200 MB
        // copy was a click, then a frozen-looking pane, then nothing.
        startWork(`Saving "${entry.name}" to the host…`);
        try {
          await commands.downloadContainerFile(projectId, entry.path, hostPath);
          setCompleted(`Saved "${entry.name}" to ${hostPath}.`);
          confirm(`Saved "${entry.name}" to the host.`);
        } finally {
          setBusy(null);
        }
      } catch (e) {
        report(`Could not save "${entry.name}" to the host`, e);
      }
    },
    [projectId, startWork, report, confirm],
  );

  /**
   * The pending answer to `conflict`. Kept in a ref rather than state because
   * the upload loop is `await`ing it — it needs the resolver, not a re-render.
   */
  const conflictResolver = useRef<((choice: OverwriteChoice) => void) | null>(null);

  const resolveConflict = useCallback((choice: OverwriteChoice) => {
    const resolve = conflictResolver.current;
    conflictResolver.current = null;
    setConflict(null);
    resolve?.(choice);
  }, []);

  // A pane unmounted mid-prompt (the tab was closed, the container stopped)
  // would otherwise leave the upload loop awaiting an answer that can never
  // come. Skipping is the safe reading of "the dialog went away".
  useEffect(
    () => () => {
      conflictResolver.current?.("skip-all");
      conflictResolver.current = null;
    },
    [],
  );

  const askOverwrite = useCallback(
    (hostPath: string, directory: string, remaining: number, containerPath: string | null) =>
      new Promise<OverwriteChoice>((resolve) => {
        // One batch asks one question at a time — the loop awaits each answer —
        // so a resolver still sitting here belongs to a *different* batch (two
        // drops in flight at once, or a drop landing while the Upload button's
        // batch is still copying). Installing over it would leave that batch
        // awaiting an answer no dialog can ever produce: a silent hang, with
        // its file neither uploaded nor skipped. Skipping it is the same
        // reading of "the dialog went away" the unmount cleanup uses.
        conflictResolver.current?.("skip");
        conflictResolver.current = resolve;
        setConflict({
          hostPath,
          name: baseName(containerPath ?? hostPath),
          directory,
          remaining,
        });
      }),
    [],
  );

  /**
   * Copy host files into the current directory. Shared by the Upload button and
   * the native drag-drop listener, so a dropped file and a picked one take the
   * same path — including the one refresh at the end rather than one per file.
   *
   * The backend refuses to overwrite unless asked to, so a name clash is not a
   * failure here: it is a question, and the answer can be given once for the
   * whole batch.
   */
  const uploadPaths = useCallback(
    async (hostPaths: string[]) => {
      if (hostPaths.length === 0) return;
      // The directory this upload is *for*. Compared against the live ref at
      // the end, because the user is free to walk away while it copies.
      const target = currentPathRef.current;
      startWork(`Uploading ${hostPaths.length} item${hostPaths.length > 1 ? "s" : ""}…`);
      /** Raw failures, kept unstringified so `report` can read their shape. */
      const failures: unknown[] = [];
      let uploaded = 0;
      let skipped = 0;
      /** A "…all" answer, applied to every remaining clash without asking. */
      let blanket: OverwriteChoice | null = null;
      try {
        for (let i = 0; i < hostPaths.length; i++) {
          const hostPath = hostPaths[i];
          try {
            await commands.uploadFileToContainer(projectId, hostPath, target);
            uploaded++;
            continue;
          } catch (e) {
            if (!isFileExistsError(e)) {
              failures.push(e);
              continue;
            }
            const choice: OverwriteChoice =
              blanket ??
              (await askOverwrite(
                hostPath,
                target,
                hostPaths.length - i - 1,
                fileExistsPath(e),
              ));
            if (choice === "replace-all" || choice === "skip-all") blanket = choice;
            if (choice === "skip" || choice === "skip-all") {
              skipped++;
              continue;
            }
          }
          try {
            await commands.uploadFileToContainer(projectId, hostPath, target, true);
            uploaded++;
          } catch (e) {
            failures.push(e);
          }
        }
      } finally {
        setBusy(null);
      }

      const summary =
        `Uploaded ${uploaded} item${uploaded === 1 ? "" : "s"}` +
        (skipped > 0 ? `, skipped ${skipped}` : "") +
        (failures.length > 0 ? `, ${failures.length} failed` : "") +
        ".";
      setCompleted(summary);

      if (failures.length > 0) {
        report(
          failures.length === 1 ? "A file could not be uploaded" : `${failures.length} files could not be uploaded`,
          ...failures,
        );
      }
      // Only re-list if the user is still looking at the directory this went
      // into. Navigating away during a slow copy used to drag the pane back.
      if (currentPathRef.current === target) await navigate(target);
    },
    [projectId, navigate, startWork, report, askOverwrite],
  );

  const uploadFile = useCallback(async () => {
    try {
      const selected = await openDialog({ multiple: true, directory: false });
      if (!selected) return;
      await uploadPaths(Array.isArray(selected) ? selected : [selected as string]);
    } catch (e) {
      report("Could not open the file picker", e);
    }
  }, [uploadPaths, report]);

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

  return {
    currentPath,
    entries,
    loading,
    /** Inline, in-context: why the listing on screen is empty. */
    error,
    busy,
    /** What the last operation finished doing, for the live region. */
    completed,
    /** An upload waiting for a Replace / Skip answer, or `null`. */
    conflict,
    resolveConflict,
    setError,
    navigate,
    goUp,
    refresh,
    downloadFile,
    uploadFile,
    uploadPaths,
    renameEntry,
    createFolder,
  };
}
