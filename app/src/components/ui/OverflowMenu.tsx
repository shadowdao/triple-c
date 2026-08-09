import { useEffect, useRef, useState } from "react";

export interface OverflowItem {
  label: string;
  onSelect: () => void;
  danger?: boolean;
  disabled?: boolean;
}

interface Props {
  items: OverflowItem[];
  label?: string;
  align?: "left" | "right";
}

/** The `⋯` menu that keeps destructive actions out of the main button row. */
export default function OverflowMenu({
  items,
  label = "More actions",
  align = "right",
}: Props) {
  const [open, setOpen] = useState(false);
  const rootRef = useRef<HTMLDivElement>(null);

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
    <div ref={rootRef} className="relative inline-block">
      <button
        type="button"
        aria-haspopup="menu"
        aria-expanded={open}
        aria-label={label}
        onClick={() => setOpen((o) => !o)}
        className="inline-flex items-center justify-center h-6 w-7 rounded-[var(--radius-control)] border border-[var(--border-color)] bg-[var(--bg-tertiary)] text-[var(--text-secondary)] hover:text-[var(--text-primary)] hover:bg-[var(--border-color)] transition-colors"
      >
        <span aria-hidden="true" className="leading-none">⋯</span>
      </button>
      {open && (
        <div
          role="menu"
          className={`absolute z-40 mt-1 min-w-[11rem] py-1 bg-[var(--bg-overlay)] border border-[var(--border-color)] rounded-[var(--radius-panel)] ${
            align === "right" ? "right-0" : "left-0"
          }`}
          style={{ boxShadow: "var(--shadow-overlay)" }}
        >
          {items.map((item) => (
            <button
              key={item.label}
              type="button"
              role="menuitem"
              disabled={item.disabled}
              onClick={() => {
                setOpen(false);
                item.onSelect();
              }}
              className={`w-full text-left px-3 py-1.5 text-xs transition-colors disabled:text-[var(--text-disabled)] disabled:hover:bg-transparent hover:bg-[var(--bg-tertiary)] ${
                item.danger ? "text-[var(--error)]" : "text-[var(--text-primary)]"
              }`}
            >
              {item.label}
            </button>
          ))}
        </div>
      )}
    </div>
  );
}
