import { useCallback, useEffect, useRef, useState } from "react";
import * as commands from "../lib/tauri-commands";
import type { Note } from "../lib/types";
import type { SaveState } from "./useSaveState";
import { useAppState } from "../store/appState";

/** A blank note, ordered to the top so the user can start typing immediately. */
function draft(): Note {
  const now = new Date().toISOString();
  return {
    // The backend owns the real id; this one only has to be unique enough to
    // key the list until the first save returns.
    id: crypto.randomUUID(),
    title: "",
    body: "",
    pinned: false,
    created_at: now,
    updated_at: now,
  };
}

/**
 * A project's notes, cached from the backend.
 *
 * The backend is the source of truth and this is a cache — every mutation goes
 * through a command and the returned record replaces the local one, so the
 * list can never drift from the file. `saveState` mirrors `useProjectSave` so
 * `ui/SaveIndicator` can report the outcome: a save that fails silently is a
 * user staring at text they believe is stored.
 */
export function useNotes(projectId: string) {
  const [notes, setNotes] = useState<Note[]>([]);
  const [loading, setLoading] = useState(true);
  const [saveState, setSaveState] = useState<SaveState>({ status: "idle", error: null });
  const pushToast = useAppState((s) => s.pushToast);
  const resetTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const currentProjectId = useRef(projectId);
  currentProjectId.current = projectId;

  useEffect(() => {
    if (!projectId) {
      setNotes([]);
      setLoading(false);
      return;
    }
    let cancelled = false;
    setLoading(true);
    setNotes([]);
    commands
      .listNotes(projectId)
      .then((loaded) => {
        if (!cancelled) setNotes(loaded);
      })
      .catch((e) => {
        if (cancelled) return;
        setNotes([]);
        pushToast({
          kind: "error",
          message: "Could not load notes for this project",
          detail: String(e),
        });
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [projectId, pushToast]);

  useEffect(
    () => () => {
      if (resetTimer.current) clearTimeout(resetTimer.current);
    },
    [],
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
    async (note: Note) => {
      if (resetTimer.current) clearTimeout(resetTimer.current);
      setSaveState({ status: "saving", error: null });
      try {
        await commands.saveNote(projectId, note);
        // Re-read the canonical list from the backend. A successful save stamps a new
        // `updated_at`, and the backend sorts unpinned notes by `updated_at` descending,
        // so the record's position has changed and positional patching would disagree with
        // what a reload would show.
        try {
          const reloaded = await commands.listNotes(projectId);
          // Guard against a stale callback: if the project changed while the save was in
          // flight, don't overwrite the new project's list with the old one. The save itself
          // still succeeded, so succeeded() still fires; only the list replacement is skipped.
          if (currentProjectId.current === projectId) {
            setNotes(reloaded);
          }
        } catch {
          // Keep the save reported as successful (it was) and leave the existing list alone
          // rather than clearing it if the re-read fails.
        }
        succeeded();
        return true;
      } catch (e) {
        const message = String(e);
        setSaveState({ status: "failed", error: message });
        pushToast({ kind: "error", message: "Could not save note", detail: message });
        return false;
      }
    },
    [projectId, pushToast, succeeded],
  );

  const createNote = useCallback(async () => {
    const note = draft();
    // Held locally first so the editor can focus it immediately; the save
    // happens on blur like every other edit. The note does not exist backend-side,
    // so no re-read can place it and no backend ordering applies to it yet. Prepending
    // puts it at the top where the user can see it immediately, and on the first save
    // its canonical position is established.
    setNotes((current) => [note, ...current]);
    return note;
  }, []);

  const deleteNote = useCallback(
    async (noteId: string) => {
      try {
        await commands.deleteNote(projectId, noteId);
        // Guard against a stale callback: if the project changed while the delete was in
        // flight, don't modify the list. Note ids are UUIDs so a stale filter cannot match
        // another project's note, but applying the same guard for consistency.
        if (currentProjectId.current === projectId) {
          setNotes((current) => current.filter((n) => n.id !== noteId));
        }
        return true;
      } catch (e) {
        pushToast({ kind: "error", message: "Could not delete note", detail: String(e) });
        return false;
      }
    },
    [projectId, pushToast],
  );

  return { notes, loading, saveState, createNote, saveNote, deleteNote };
}
