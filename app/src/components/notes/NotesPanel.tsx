import { useEffect, useMemo, useState } from "react";
import { useNotes } from "../../hooks/useNotes";
import NoteEditor from "./NoteEditor";
import Button from "../ui/Button";
import SaveIndicator from "../ui/SaveIndicator";

interface Props {
  projectId: string;
}

const UNTITLED = "Untitled note";

/**
 * The notes surface itself, shared by the Project Home tab and the dock so the
 * two cannot drift into different behaviour.
 *
 * Master/detail: titles on the left, one editor on the right. The editor holds
 * draft text locally and commits on blur, which is how every other editable
 * field in the app behaves (`ClaudeInstructionsEditor`, the Config tab).
 */
export default function NotesPanel({ projectId }: Props) {
  const { notes, loading, saveState, createNote, saveNote, deleteNote } =
    useNotes(projectId);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [title, setTitle] = useState("");
  const [body, setBody] = useState("");

  const selected = useMemo(
    () => notes.find((n) => n.id === selectedId) ?? notes[0] ?? null,
    [notes, selectedId],
  );

  // Load the selected note's stored text into the draft. Keyed on the id, not
  // the note object, so a save round trip does not stomp what is being typed.
  useEffect(() => {
    if (!selected) {
      setTitle("");
      setBody("");
      return;
    }
    setTitle(selected.title);
    setBody(selected.body);
  }, [selected?.id]); // eslint-disable-line react-hooks/exhaustive-deps

  const commit = () => {
    if (!selected) return;
    // Reading is not editing: clicking through notes must not rewrite the file.
    if (title === selected.title && body === selected.body) return;
    void saveNote({ ...selected, title, body });
  };

  const onCreate = async () => {
    const note = await createNote();
    if (note) setSelectedId(note.id);
  };

  if (loading) {
    return (
      <p className="p-4 text-xs text-[var(--text-secondary)]">Loading notes…</p>
    );
  }

  return (
    <div className="flex flex-col h-full min-h-0">
      <div className="flex items-center justify-between gap-2 px-3 py-2 border-b border-[var(--border-color)]">
        <Button variant="primary" onClick={onCreate}>
          New note
        </Button>
        <SaveIndicator state={saveState} />
      </div>

      {notes.length === 0 ? (
        <div className="flex-1 flex items-center justify-center p-4">
          <p className="text-[13px] text-[var(--text-secondary)] text-center">
            No notes yet. Keep reminders here, and send any of them straight to a
            running Claude session.
          </p>
        </div>
      ) : (
        <div className="flex-1 min-h-0 flex">
          <ul className="w-48 flex-shrink-0 overflow-y-auto border-r border-[var(--border-color)] py-1">
            {notes.map((n) => (
              <li key={n.id}>
                <button
                  type="button"
                  onClick={() => setSelectedId(n.id)}
                  className={`w-full text-left px-3 py-1.5 text-xs truncate transition-colors ${
                    selected?.id === n.id
                      ? "bg-[var(--bg-tertiary)] text-[var(--text-primary)]"
                      : "text-[var(--text-secondary)] hover:text-[var(--text-primary)]"
                  }`}
                >
                  {n.title.trim() || UNTITLED}
                </button>
              </li>
            ))}
          </ul>
          <div className="flex-1 min-w-0">
            {selected && (
              <NoteEditor
                projectId={projectId}
                title={title}
                body={body}
                onTitleChange={setTitle}
                onBodyChange={setBody}
                onCommit={commit}
                onDelete={() => void deleteNote(selected.id)}
              />
            )}
          </div>
        </div>
      )}
    </div>
  );
}
