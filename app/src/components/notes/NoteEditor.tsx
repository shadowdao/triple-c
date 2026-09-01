import SendToAgentButton from "./SendToAgentButton";
import Button from "../ui/Button";

interface Props {
  projectId: string;
  title: string;
  body: string;
  onTitleChange: (value: string) => void;
  onBodyChange: (value: string) => void;
  onCommit: () => void;
  onDelete: () => void;
}

/**
 * Title and body, saved when a field loses focus.
 *
 * Plain text on purpose. There is no markdown rendering and no view/edit split,
 * so there is no moment where the text on screen is not the text that would be
 * sent — which is what makes "the agent gets exactly what you see" true rather
 * than nearly true.
 */
export default function NoteEditor({
  projectId,
  title,
  body,
  onTitleChange,
  onBodyChange,
  onCommit,
  onDelete,
}: Props) {
  return (
    <div className="flex flex-col h-full min-h-0 gap-2 p-3">
      <div className="flex items-center gap-2">
        <input
          value={title}
          onChange={(e) => onTitleChange(e.target.value)}
          onBlur={onCommit}
          placeholder="Note title"
          aria-label="Note title"
          className="flex-1 min-w-0 px-2 h-8 bg-[var(--bg-primary)] border border-[var(--border-color)] rounded-[var(--radius-control)] text-[13px] text-[var(--text-primary)] focus:border-[var(--accent)] transition-colors"
        />
        {/* The live editor text, not `note.body` — what is on screen is what
            gets sent. */}
        <SendToAgentButton projectId={projectId} body={body} />
        <Button variant="danger" onClick={onDelete} aria-label="Delete note">
          Delete
        </Button>
      </div>
      <textarea
        value={body}
        onChange={(e) => onBodyChange(e.target.value)}
        onBlur={onCommit}
        placeholder="Reminders, gotchas, a prompt worth keeping…"
        aria-label="Note body"
        className="flex-1 min-h-0 w-full px-3 py-2 bg-[var(--bg-primary)] border border-[var(--border-color)] rounded-[var(--radius-control)] text-[13px] text-[var(--text-primary)] focus:border-[var(--accent)] resize-none font-mono transition-colors"
      />
      <p className="text-xs text-[var(--text-secondary)]">
        Notes save when a field loses focus. Sending puts the note in the agent&rsquo;s
        prompt — you press Enter.
      </p>
    </div>
  );
}
