import { useShallow } from "zustand/react/shallow";
import TerminalTabs from "../terminal/TerminalTabs";
import { useAppState } from "../../store/appState";

export default function TopBar() {
  const { dockerAvailable, imageExists } = useAppState(
    useShallow(s => ({ dockerAvailable: s.dockerAvailable, imageExists: s.imageExists }))
  );

  return (
    <div className="flex items-center h-10 bg-[var(--bg-secondary)] border border-[var(--border-color)] rounded-lg overflow-hidden">
      <div className="flex-1 overflow-x-auto pl-2">
        <TerminalTabs />
      </div>
      <div className="flex items-center gap-2 px-4 flex-shrink-0 text-xs text-[var(--text-secondary)]">
        <StatusDot ok={dockerAvailable === true} label="Docker" />
        <StatusDot ok={imageExists === true} label="Image" />
      </div>
    </div>
  );
}

function StatusDot({ ok, label }: { ok: boolean; label: string }) {
  return (
    <span className="flex items-center gap-1">
      <span
        className={`inline-block w-2 h-2 rounded-full ${
          ok ? "bg-[var(--success)]" : "bg-[var(--text-secondary)]"
        }`}
      />
      {label}
    </span>
  );
}
