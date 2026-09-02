import { useEffect, useRef, useState } from "react";
import type { Note } from "../../lib/types";

export const UNTITLED = "Untitled note";

interface Props {
  notes: Note[];
  selectedId: string;
  title: string;
  onTitleChange: (value: string) => void;
  onCommit: () => void;
  onSelect: (id: string) => void;
}

/**
 * One row that both names the current note and switches to another.
 *
 * The dock has no room for a permanent list of titles, so the title field
 * doubles as the label of what is open and the chevron beside it holds the
 * rest. Renaming therefore needs no separate affordance.
 *
 * Two honest controls rather than one `role="combobox"`: a text field and a
 * button that opens a listbox. A real combobox owes its listbox keyboard
 * navigation, active-descendant tracking and an input that filters — none of
 * which this needs, and half of which is worse than not claiming the role.
 *
 * `OverflowMenu` is deliberately not reused here despite the shape being
 * close. It keys its items by label, and notes are addressed by id: two
 * untitled notes are the ordinary case and would collapse into one row.
 */
export default function NoteSwitcher({
  notes,
  selectedId,
  title,
  onTitleChange,
  onCommit,
  onSelect,
}: Props) {
  const [open, setOpen] = useState(false);
  const rootRef = useRef<HTMLDivElement>(null);

  // Same dismissal contract as `OverflowMenu`, so the two feel identical.
  useEffect(() => {
    if (!open) return;
    const onDocClick = (e: MouseEvent) => {
      if (!rootRef.current?.contains(e.target as Node)) setOpen(false);
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") setOpen(false);
    };
    document.addEventListener("mousedown", onDocClick);
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("mousedown", onDocClick);
      document.removeEventListener("keydown", onKey);
    };
  }, [open]);

  return (
    <div ref={rootRef} className="relative flex items-center gap-1 min-w-0">
      <input
        value={title}
        onChange={(e) => onTitleChange(e.target.value)}
        onBlur={onCommit}
        placeholder="Note title"
        aria-label="Note title"
        className="flex-1 min-w-0 px-2 h-7 bg-[var(--bg-primary)] border border-[var(--border-color)] rounded-[var(--radius-control)] text-[13px] text-[var(--text-primary)] focus:border-[var(--accent)] transition-colors"
      />
      <button
        type="button"
        aria-label="Switch note"
        aria-haspopup="listbox"
        aria-expanded={open}
        onClick={() => setOpen((o) => !o)}
        className="inline-flex items-center justify-center h-7 w-6 flex-shrink-0 rounded-[var(--radius-control)] border border-[var(--border-color)] bg-[var(--bg-tertiary)] text-[var(--text-secondary)] hover:text-[var(--text-primary)] hover:bg-[var(--border-color)] transition-colors"
      >
        <span aria-hidden="true" className="leading-none text-[10px]">▾</span>
      </button>
      {open && (
        <div
          role="listbox"
          aria-label="Notes"
          className="absolute right-0 top-full mt-1 z-40 w-full max-h-64 overflow-y-auto py-1 bg-[var(--bg-overlay)] border border-[var(--border-color)] rounded-[var(--radius-panel)]"
          style={{ boxShadow: "var(--shadow-overlay)" }}
        >
          {/* Buttons directly inside the listbox: wrapping each in an `<li>`
              would put an implicit `listitem` between the listbox and its
              options, which is not a child role a listbox owns. */}
          {notes.map((n) => (
            <button
              key={n.id}
              type="button"
              role="option"
              aria-selected={n.id === selectedId}
              onClick={() => {
                onSelect(n.id);
                setOpen(false);
              }}
              className={`block w-full text-left px-3 py-1.5 text-xs truncate transition-colors hover:bg-[var(--bg-tertiary)] ${
                n.id === selectedId
                  ? "text-[var(--text-primary)] bg-[var(--bg-tertiary)]"
                  : "text-[var(--text-secondary)]"
              }`}
            >
              {n.title.trim() || UNTITLED}
            </button>
          ))}
        </div>
      )}
    </div>
  );
}
