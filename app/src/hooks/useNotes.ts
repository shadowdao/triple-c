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
 * Per-project write ordering for the shared notes cache.
 *
 * `mutationChains` orders a project's *writes* against each other. It says
 * nothing about reads, and the mount load is a read that runs outside it — so
 * one gesture can put two requests in flight at once and let the slower one
 * win. Clicking the dock toggle with the textarea focused fires `blur` →
 * `saveNote` and the dock's mount → `list_notes` in the same tick: the save
 * finishes, its re-read writes the post-save list, and then the mount's read —
 * issued earlier, still out — lands its pre-save snapshot on top. Both panels
 * show stale text until something else refreshes. It needs the plain read to
 * be slower than `save_note`'s double-fsync write plus a second read, so it is
 * narrow, but it was reproduced.
 *
 * The fix is a sequence number rather than a chain, because the two requests
 * are not competing for a resource — the loser's result is simply *older*, and
 * the cheapest correct thing to do with it is throw it away. Every write
 * claims a sequence when the request behind it is issued, and `commitNotes`
 * drops one whose sequence predates what is already cached. That also closes a
 * hole identity comparison cannot: on a `p1 → p2 → p1` switch a read from the
 * *first* p1 era is indistinguishable from a current one by project id, and
 * would land its stale list on the second era's.
 *
 * Note what this deliberately does **not** replace. `isCurrent()` asks whether
 * this *panel* is still showing the project a save was made for, which governs
 * a per-panel `SaveIndicator` and not the shared cache at all; a per-project
 * counter cannot answer it. Ordering and panel identity are two questions, and
 * they keep two guards.
 *
 * Entries are two integers per project and are never pruned: they must outlive
 * every request that could still land, and the map is monotone, so a stale
 * sequence can never be reissued.
 */
const notesSequences = new Map<string, { issued: number; committed: number }>();

function sequenceFor(projectId: string): { issued: number; committed: number } {
  let seq = notesSequences.get(projectId);
  if (!seq) notesSequences.set(projectId, (seq = { issued: 0, committed: 0 }));
  return seq;
}

/**
 * Claim the sequence for a write about to be issued.
 *
 * Called immediately before the request whose result it will commit, so that
 * ordering is by *issue* time. Resolution order is exactly what cannot be
 * trusted here.
 */
function issueNotesWrite(projectId: string): number {
  const seq = sequenceFor(projectId);
  seq.issued += 1;
  return seq.issued;
}

/**
 * Write a list into the cache under the sequence it was issued at, unless
 * something newer has already been committed.
 *
 * A local patch — the filter behind a confirmed delete, say — is authoritative
 * at the moment it applies rather than derived from an earlier read, so it
 * claims its sequence here: `issued` is never below `committed`, so a freshly
 * claimed one always wins, and anything still in flight behind it is correctly
 * treated as stale.
 */
function commitNotes(projectId: string, seq: number, notes: Note[]): boolean {
  const sequence = sequenceFor(projectId);
  if (seq <= sequence.committed) return false;
  sequence.committed = seq;
  useAppState.getState().setProjectNotes(projectId, notes);
  return true;
}

/**
 * Re-read the canonical list into the shared cache.
 *
 * A successful save stamps a new `updated_at` and the backend sorts on it, so
 * the record's position has changed and positional patching would disagree
 * with what a reload would show. The backend owns the order; the webview never
 * sorts. A failed re-read leaves the cache alone rather than clearing it.
 *
 * `true` means "the cache is current", which is why a superseded commit still
 * returns it: whatever beat this read was issued later and therefore read the
 * same write or a later one.
 */
async function refresh(projectId: string): Promise<boolean> {
  const seq = issueNotesWrite(projectId);
  try {
    const reloaded = await commands.listNotes(projectId);
    commitNotes(projectId, seq, reloaded);
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
    const seq = issueNotesWrite(projectId);
    commands
      .listNotes(projectId)
      .then((loaded) => {
        commitNotes(projectId, seq, loaded);
      })
      .catch((e) => {
        // A project that has never been read caches the empty list, so a panel
        // does not sit on "Loading notes…" forever. One that *has* been read
        // keeps what it has: this load is a refresh behind a list already on
        // screen — the second surface mounting, say — and a failed refresh
        // must not blank both of them. Same rule as `refresh()`. Neither
        // branch has a stale-project hazard, because the write is keyed by the
        // project it belongs to.
        //
        // The commit goes under this read's own sequence, not a fresh one: a
        // *later* read still in flight has the newer answer and must not be
        // dropped in favour of this failure's empty list.
        if (useAppState.getState().notesByProject[projectId] === undefined) {
          commitNotes(projectId, seq, []);
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
            commitNotes(projectId, issueNotesWrite(projectId), [
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
          commitNotes(
            projectId,
            issueNotesWrite(projectId),
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
