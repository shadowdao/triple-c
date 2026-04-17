import { useEffect, useRef } from "react";

interface Props {
  x: number;
  y: number;
  onCopyTrimmed: () => void;
  onCopyRaw: () => void;
  onDismiss: () => void;
}

export default function TerminalContextMenu({ x, y, onCopyTrimmed, onCopyRaw, onDismiss }: Props) {
  const menuRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const handleOutsideClick = (e: MouseEvent) => {
      if (menuRef.current && !menuRef.current.contains(e.target as Node)) {
        onDismiss();
      }
    };
    const handleKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onDismiss();
    };
    document.addEventListener("mousedown", handleOutsideClick, true);
    document.addEventListener("keydown", handleKey);
    return () => {
      document.removeEventListener("mousedown", handleOutsideClick, true);
      document.removeEventListener("keydown", handleKey);
    };
  }, [onDismiss]);

  return (
    <div
      ref={menuRef}
      className="fixed z-[60] min-w-[160px] py-1 rounded-md border border-[#30363d] bg-[#1f2937] shadow-lg text-xs text-[#e6edf3]"
      style={{ left: x, top: y }}
      onContextMenu={(e) => e.preventDefault()}
    >
      <button
        className="w-full text-left px-3 py-1.5 hover:bg-[#2d3748] cursor-pointer"
        onClick={onCopyTrimmed}
      >
        Copy trimmed
        <kbd className="float-right ml-3 px-1 py-0.5 text-[10px] bg-[#0d1117] border border-[#30363d] rounded font-mono">
          Ctrl+Shift+C
        </kbd>
      </button>
      <button
        className="w-full text-left px-3 py-1.5 hover:bg-[#2d3748] cursor-pointer"
        onClick={onCopyRaw}
      >
        Copy raw
        <kbd className="float-right ml-3 px-1 py-0.5 text-[10px] bg-[#0d1117] border border-[#30363d] rounded font-mono">
          Ctrl+Shift+Alt+C
        </kbd>
      </button>
    </div>
  );
}
