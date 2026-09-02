import { useMemo, useState } from "react";
import { useNotes } from "../../hooks/useNotes";
import { useNoteDraft } from "./useNoteDraft";
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
 * Master/detail: titles beside the editor when there is room, stacked above it
 * when there is not. That is a **container** query, not a viewport one, because
 * the two surfaces differ in width while sharing a viewport — the dock opens at
 * 352px and the tab is the width of the main area. A `md:` breakpoint would
 * read the window and give both the same answer, which is the wrong answer for
 * one of them.
 *
 * The threshold is arithmetic, not taste: side by side needs the 192px list,
 * plus an editor wide enough for its own action row (~280px), plus the divider.
 * Below ~473px the editor is narrower than its buttons, so `@lg` (512px) is the
 * first stop that clears it.
 *
 * The editor holds draft text locally and commits on blur, which is how every
 * other editable field in the app behaves (`ClaudeInstructionsEditor`, the
 * Config tab).
 */
export default function NotesPanel({ projectId }: Props) {
  const { notes, loading, saveState, createNote, saveNote, deleteNote } =
    useNotes(projectId);
  const [selectedId, setSelectedId] = useState<string | null>(null);

  const selected = useMemo(
    () => notes.find((n) => n.id === selectedId) ?? notes[0] ?? null,
    [notes, selectedId],
  );

  const { title, body, setTitle, setBody, commit } = useNoteDraft(
    selected,
    saveNote,
  );

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
    <div className="@container flex flex-col h-full min-h-0">
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
        <div className="flex-1 min-h-0 flex flex-col @lg:flex-row">
          {/* Stacked: a capped strip of titles above the editor, so the note
              being written keeps most of the height. Side by side: a full-height
              column of the fixed width the editor's arithmetic assumes. */}
          <ul className="flex-shrink-0 overflow-y-auto py-1 max-h-32 border-b @lg:max-h-none @lg:w-48 @lg:border-b-0 @lg:border-r border-[var(--border-color)]">
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
