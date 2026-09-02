import { useMemo, useState } from "react";
import { useNotes } from "../../hooks/useNotes";
import { useNoteDraft } from "./useNoteDraft";
import NoteSwitcher from "./NoteSwitcher";
import SendToAgentButton from "./SendToAgentButton";
import Button from "../ui/Button";
import OverflowMenu from "../ui/OverflowMenu";
import SaveIndicator from "../ui/SaveIndicator";

interface Props {
  projectId: string;
}

/**
 * Notes at dock width.
 *
 * Deliberately not `NotesPanel` in a narrower box. The tab can afford a column
 * of titles beside the editor; the dock cannot, and shrinking that layout
 * spends its height on chrome — a title strip, a wrapped button row and a
 * paragraph of help — for a body that ends up a few words wide.
 *
 * So the dock shows exactly one note. The title row names it and switches to
 * another, the actions that are not writing live in the overflow menu, and
 * everything left over is the body. Roughly 240px of height comes back.
 *
 * What the two surfaces share is the part that must not drift: `useNotes` for
 * the cache and its write ordering, and `useNoteDraft` for when a keystroke
 * becomes a save. Only the layout is different.
 */
export default function NotesDockPanel({ projectId }: Props) {
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

  if (!selected) {
    return (
      <div className="flex-1 flex flex-col items-center justify-center gap-3 p-4">
        <p className="text-[13px] text-[var(--text-secondary)] text-center">
          Keep reminders here, and send any of them straight to a running Claude
          session.
        </p>
        <Button variant="primary" onClick={onCreate}>
          New note
        </Button>
      </div>
    );
  }

  return (
    <div className="flex flex-col h-full min-h-0">
      <div className="flex items-center gap-1 px-2 py-1.5 flex-shrink-0 border-b border-[var(--border-color)]">
        <div className="flex-1 min-w-0">
          <NoteSwitcher
            notes={notes}
            selectedId={selected.id}
            title={title}
            onTitleChange={setTitle}
            onCommit={commit}
            onSelect={setSelectedId}
          />
        </div>
        {/* Renders nothing while idle, so it costs no width until it matters. */}
        <SaveIndicator state={saveState} />
        <OverflowMenu
          label="Note actions"
          items={[
            { label: "New note", onSelect: () => void onCreate() },
            {
              label: "Delete note",
              danger: true,
              onSelect: () => void deleteNote(selected.id),
            },
          ]}
        />
      </div>

      <textarea
        value={body}
        onChange={(e) => setBody(e.target.value)}
        onBlur={commit}
        placeholder="Reminders, gotchas, a prompt worth keeping…"
        aria-label="Note body"
        className="flex-1 min-h-0 w-full px-3 py-2 bg-transparent text-[13px] text-[var(--text-primary)] resize-none font-mono"
      />

      <div className="px-2 py-2 flex-shrink-0 border-t border-[var(--border-color)]">
        {/* The live draft, not `selected.body` — what is on screen is what gets
            sent. `dropUp` because the dock clips its own overflow. */}
        <SendToAgentButton
          projectId={projectId}
          body={body}
          fullWidth
          dropUp
        />
      </div>
    </div>
  );
}
