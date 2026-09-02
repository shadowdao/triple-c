import { useEffect, useRef, useState } from "react";
import type { Note } from "../../lib/types";

/**
 * Draft text for the note being edited, committed when a field loses focus.
 *
 * This is the half the dock and the tab must never disagree on, so it lives
 * here rather than in either layout. The two surfaces differ in how they show
 * notes; they must not differ in when a keystroke becomes a save.
 *
 * The draft is "untouched" exactly while it still matches what was last copied
 * out of the store, which is what lets an edit made on the *other* surface
 * reach this one's editor without ever discarding half-typed text.
 */
export function useNoteDraft(
  selected: Note | null,
  saveNote: (note: Note) => Promise<unknown>,
) {
  const [title, setTitle] = useState("");
  const [body, setBody] = useState("");
  const seeded = useRef<{ id: string | null; title: string; body: string }>({
    id: null,
    title: "",
    body: "",
  });

  // Re-seed on a change of note, and on a change to the *stored* text of the
  // note already open — the second case is the dock and the tab showing one
  // project at once.
  useEffect(() => {
    if (!selected) {
      seeded.current = { id: null, title: "", body: "" };
      setTitle("");
      setBody("");
      return;
    }
    const untouched =
      title === seeded.current.title && body === seeded.current.body;
    if (seeded.current.id !== selected.id || untouched) {
      seeded.current = {
        id: selected.id,
        title: selected.title,
        body: selected.body,
      };
      setTitle(selected.title);
      setBody(selected.body);
    }
  }, [selected?.id, selected?.title, selected?.body]); // eslint-disable-line react-hooks/exhaustive-deps

  const commit = () => {
    if (!selected) return;
    // Reading is not editing: clicking through notes must not rewrite the file.
    if (title === selected.title && body === selected.body) return;
    // Mark the draft as matching what was just committed, so the store update
    // this save produces reads as "no change" rather than as a stale re-seed.
    seeded.current = { id: selected.id, title, body };
    void saveNote({ ...selected, title, body });
  };

  return { title, body, setTitle, setBody, commit };
}
