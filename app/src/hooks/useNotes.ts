import { useCallback, useEffect, useRef, useState } from "react";
import * as commands from "../lib/tauri-commands";
import type { Note } from "../lib/types";
import type { SaveState } from "./useSaveState";
import { useAppState } from "../store/appState";

/** A blank note, ordered to the top so the user can start typing immediately. */
function draft(): Note {
  const now = new Date().toISOString();
  return {
    // The backend keeps whatever id it is handed for a note it has not seen,
    // so this one is the note's real id from the first save onward.
    id: crypto.randomUUID(),
    title: "",
    body: "",
    pinned: false,
    created_at: now,
    updated_at: now,
  };
}

/** Stable empty list, so a project with nothing cached does not re-render on identity. */
const NO_NOTES: Note[] = [];

/**
 * Per-project mutation chain.
 *
 * A project's writes are serialised so that two of them cannot be in flight at
 * once. The Rust `write_lock` stops an upsert and a delete *interleaving*; it
 * does not order them, and the order is the part that matters here. Clicking
 * Delete while the textarea has focus fires `blur` first, so `save_note` and
 * `delete_note` are issued back to back — and if the delete wins the lock, the
 * upsert behind it re-inserts the note and it comes back on the next load.
 * "Fix a typo, decide the note is useless, delete it" is an ordinary sequence.
 *
 * Module scope, not hook scope, for the reason `useTerminal`'s input queue is:
 * several components call `useNotes` for the same project (the Project Home
 * tab and the dock), and a per-hook chain would give each its own ordering and
 * leave them racing each other — which is the bug, not the fix.
 */
const mutationChains = new Map<string, Promise<unknown>>();

function enqueueMutation<T>(projectId: string, run: () => Promise<T>): Promise<T> {
  const previous = mutationChains.get(projectId) ?? Promise.resolve();
  // `run` on both arms: a failed mutation must not stall every later one.
  const result = previous.then(run, run);
  const tail = result.then(
    () => {},
    () => {},
  );
  mutationChains.set(projectId, tail);
  void tail.then(() => {
    // Drop the entry once idle, so closed projects do not accumulate.
    if (mutationChains.get(projectId) === tail) mutationChains.delete(projectId);
  });
  return result;
}

/**
 * Re-read the canonical list into the shared cache.
 *
 * A successful save stamps a new `updated_at` and the backend sorts on it, so
 * the record's position has changed and positional patching would disagree
 * with what a reload would show. The backend owns the order; the webview never
 * sorts. A failed re-read leaves the cache alone rather than clearing it.
 */
async function refresh(projectId: string): Promise<boolean> {
  try {
    const reloaded = await commands.listNotes(projectId);
    useAppState.getState().setProjectNotes(projectId, reloaded);
    return true;
  } catch {
    return false;
  }
}

/**
 * A project's notes, cached from the backend.
 *
 * The backend is the source of truth and the zustand slice is the cache —
 * every mutation goes through a command and the returned list replaces the
 * cached one, so the list can never drift from the file. The cache lives in
 * the store rather than in this hook because two surfaces show the same
 * project's notes at once; see `notesByProject`.
 *
 * `saveState` is deliberately *not* shared: it is this panel's report of this
 * panel's write, and `ui/SaveIndicator` is per-panel. A save that fails
 * silently is a user staring at text they believe is stored.
 */
export function useNotes(projectId: string) {
  const cached = useAppState((s) => s.notesByProject[projectId]);
  const pushToast = useAppState((s) => s.pushToast);
  const [saveState, setSaveState] = useState<SaveState>({ status: "idle", error: null });
  const resetTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const currentProjectId = useRef(projectId);
  currentProjectId.current = projectId;

  useEffect(() => {
    if (!projectId) return;
    // Read through `getState` rather than through subscribed values: the
    // effect must fire once per project, not again every time the flag it sets
    // changes. Two panels mounting for the same project therefore make one
    // read, and the second renders from the cache with no loading flash.
    const store = useAppState.getState();
    if (store.notesLoading[projectId]) return;
    store.setNotesLoading(projectId, true);
    commands
      .listNotes(projectId)
      .then((loaded) => {
        useAppState.getState().setProjectNotes(projectId, loaded);
      })
      .catch((e) => {
        // A project that has never been read caches the empty list, so a panel
        // does not sit on "Loading notes…" forever. One that *has* been read
        // keeps what it has: this load is a refresh behind a list already on
        // screen — the second surface mounting, say — and a failed refresh
        // must not blank both of them. Same rule as `refresh()`. Neither
        // branch has a stale-project hazard, because the write is keyed by the
        // project it belongs to.
        const store = useAppState.getState();
        if (store.notesByProject[projectId] === undefined) {
          store.setProjectNotes(projectId, []);
        }
        pushToast({
          kind: "error",
          message: "Could not load notes for this project",
          detail: String(e),
        });
      })
      .finally(() => {
        useAppState.getState().setNotesLoading(projectId, false);
      });
  }, [projectId, pushToast]);

  useEffect(
    () => () => {
      if (resetTimer.current) clearTimeout(resetTimer.current);
    },
    [],
  );

  // The indicator belongs to whatever project this panel is showing *now*.
  // Without this, switching project mid-save leaves the new project's
  // SaveIndicator stuck on the old project's "Saving…" — the same wrong-project
  // report as flashing its "Saved ✓", just in the other direction.
  useEffect(() => {
    if (resetTimer.current) clearTimeout(resetTimer.current);
    setSaveState({ status: "idle", error: null });
  }, [projectId]);

  /**
   * Whether this hook is still looking at the project a queued mutation was
   * issued for. Only the *reporting* is gated on it — the cache write is not,
   * because it is keyed by project and belongs to that project either way.
   * Without this, the new project's SaveIndicator flashes "Saved ✓" for the
   * old project's write.
   */
  const isCurrent = useCallback(
    () => currentProjectId.current === projectId,
    [projectId],
  );

  const succeeded = useCallback(() => {
    setSaveState({ status: "saved", error: null });
    if (resetTimer.current) clearTimeout(resetTimer.current);
    resetTimer.current = setTimeout(
      () => setSaveState({ status: "idle", error: null }),
      2500,
    );
  }, []);

  const saveNote = useCallback(
    (note: Note) =>
      enqueueMutation(projectId, async () => {
        if (isCurrent()) {
          if (resetTimer.current) clearTimeout(resetTimer.current);
          setSaveState({ status: "saving", error: null });
        }
        try {
          await commands.saveNote(projectId, note);
          await refresh(projectId);
          if (isCurrent()) succeeded();
          return true;
        } catch (e) {
          const message = String(e);
          if (isCurrent()) setSaveState({ status: "failed", error: message });
          // The toast is not project-scoped — it names the failure and stays
          // readable after a switch — so it fires either way.
          pushToast({ kind: "error", message: "Could not save note", detail: message });
          return false;
        }
      }),
    [projectId, pushToast, succeeded, isCurrent],
  );

  /**
   * Create a note by persisting it, rather than holding it locally until the
   * first blur.
   *
   * The draft used to live only in the list, which meant any *other* note
   * being saved replaced the list with the backend's and the unsaved draft
   * silently vanished — click "New note" twice, type in the second, blur, and
   * the first row is gone. Sharing one cache between two surfaces makes that
   * worse rather than better: a local-only row would exist in whichever panel
   * created it and nowhere else. Letting the backend own the row from the
   * start removes the whole class: there is no such thing as a note in the
   * list that the file does not have.
   */
  const createNote = useCallback(
    () =>
      enqueueMutation(projectId, async () => {
        const note = draft();
        try {
          const saved = await commands.saveNote(projectId, note);
          if (!(await refresh(projectId))) {
            // The note exists; only the re-read failed. Show it rather than
            // leaving the user with a button that did nothing visible.
            const store = useAppState.getState();
            store.setProjectNotes(projectId, [
              saved,
              ...(store.notesByProject[projectId] ?? []),
            ]);
          }
          return saved;
        } catch (e) {
          pushToast({
            kind: "error",
            message: "Could not create note",
            detail: String(e),
          });
          return null;
        }
      }),
    [projectId, pushToast],
  );

  const deleteNote = useCallback(
    (noteId: string) =>
      enqueueMutation(projectId, async () => {
        try {
          await commands.deleteNote(projectId, noteId);
          const store = useAppState.getState();
          store.setProjectNotes(
            projectId,
            (store.notesByProject[projectId] ?? []).filter((n) => n.id !== noteId),
          );
          return true;
        } catch (e) {
          pushToast({ kind: "error", message: "Could not delete note", detail: String(e) });
          return false;
        }
      }),
    [projectId, pushToast],
  );

  return {
    notes: cached ?? NO_NOTES,
    // Only "loading" before the project has ever been read — never on a
    // refresh behind a list that is already on screen, and never on the second
    // panel to mount for a project the first one already fetched. A failed
    // load caches the empty list, so this cannot latch on.
    loading: Boolean(projectId) && cached === undefined,
    saveState,
    createNote,
    saveNote,
    deleteNote,
  };
}
