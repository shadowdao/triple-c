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

  useEffect(() => {
    if (!projectId) {
      setNotes([]);
      setLoading(false);
      return;
    }
    let cancelled = false;
    setLoading(true);
    commands
      .listNotes(projectId)
      .then((loaded) => {
        if (!cancelled) setNotes(loaded);
      })
      .catch((e) => {
        if (cancelled) return;
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
        const saved = await commands.saveNote(projectId, note);
        setNotes((current) => {
          const index = current.findIndex((n) => n.id === saved.id);
          if (index === -1) return [saved, ...current];
          const next = [...current];
          next[index] = saved;
          return next;
        });
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
    // happens on blur like every other edit.
    setNotes((current) => [note, ...current]);
    return note;
  }, []);

  const deleteNote = useCallback(
    async (noteId: string) => {
      try {
        await commands.deleteNote(projectId, noteId);
        setNotes((current) => current.filter((n) => n.id !== noteId));
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
