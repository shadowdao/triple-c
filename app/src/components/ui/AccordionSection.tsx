import { useEffect, useState, type ReactNode } from "react";

interface Props {
  id: string;
  title: string;
  defaultOpen?: boolean;
  storageKey?: string;
  children: ReactNode;
}

function loadOpenState(storageKey: string, fallback: boolean): boolean {
  try {
    const stored = localStorage.getItem(storageKey);
    if (stored === null) return fallback;
    return stored === "1";
  } catch {
    return fallback;
  }
}

function persistOpenState(storageKey: string, open: boolean) {
  try {
    localStorage.setItem(storageKey, open ? "1" : "0");
  } catch {
    // ignore
  }
}

export default function AccordionSection({ id, title, defaultOpen = true, storageKey, children }: Props) {
  const key = storageKey ?? `triple-c.accordion.${id}`;
  const [open, setOpen] = useState<boolean>(() => loadOpenState(key, defaultOpen));

  useEffect(() => {
    persistOpenState(key, open);
  }, [key, open]);

  return (
    <div className="border border-[var(--border-color)] rounded">
      <button
        type="button"
        onClick={() => setOpen(o => !o)}
        aria-expanded={open}
        className="w-full flex items-center justify-between px-3 py-2 text-left text-sm font-medium text-[var(--text-primary)] hover:bg-[var(--bg-primary)] transition-colors"
      >
        <span>{title}</span>
        <svg
          className={`w-4 h-4 text-[var(--text-secondary)] transition-transform ${open ? "rotate-90" : ""}`}
          viewBox="0 0 24 24"
          fill="none"
          stroke="currentColor"
          strokeWidth="2"
          strokeLinecap="round"
          strokeLinejoin="round"
        >
          <polyline points="9 18 15 12 9 6" />
        </svg>
      </button>
      {open && (
        <div className="px-3 py-3 border-t border-[var(--border-color)] space-y-4">
          {children}
        </div>
      )}
    </div>
  );
}
