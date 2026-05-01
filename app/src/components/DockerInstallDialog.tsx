import { useCallback, useEffect, useRef, useState } from "react";
import { openUrl } from "@tauri-apps/plugin-opener";
import { useInstallHelper } from "../hooks/useInstallHelper";
import { useDocker } from "../hooks/useDocker";

interface Props {
  onClose: () => void;
}

type Phase = "idle" | "installing" | "done" | "error";

export default function DockerInstallDialog({ onClose }: Props) {
  const { options, loadOptions, runInstall } = useInstallHelper();
  const { checkDocker } = useDocker();
  const [showManual, setShowManual] = useState(false);
  const [phase, setPhase] = useState<Phase>("idle");
  const [log, setLog] = useState<string[]>([]);
  const [error, setError] = useState<string | null>(null);
  const overlayRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    loadOptions();
  }, [loadOptions]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape" && phase !== "installing") onClose();
    };
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, [onClose, phase]);

  const handleOverlayClick = useCallback(
    (e: React.MouseEvent<HTMLDivElement>) => {
      if (e.target === overlayRef.current && phase !== "installing") onClose();
    },
    [onClose, phase],
  );

  const handleInstall = async () => {
    setPhase("installing");
    setLog([]);
    setError(null);
    try {
      await runInstall((line) => setLog((prev) => [...prev, line]));
      setPhase("done");
      // Re-check Docker so the rest of the app can proceed without a reload.
      await checkDocker();
    } catch (e) {
      setError(String(e));
      setPhase("error");
    }
  };

  const handleOpenDocs = async () => {
    if (!options) return;
    try {
      await openUrl(options.docs_url);
    } catch (e) {
      console.error("Failed to open docs URL:", e);
    }
  };

  const handleRecheck = async () => {
    const available = await checkDocker();
    if (available) onClose();
  };

  if (!options) {
    return null;
  }

  const installVerb = phase === "installing" ? "Installing…" : `Install ${options.product_name}`;

  return (
    <div
      ref={overlayRef}
      onClick={handleOverlayClick}
      className="fixed inset-0 bg-black/50 flex items-center justify-center z-50"
    >
      <div className="bg-[var(--bg-secondary)] border border-[var(--border-color)] rounded-lg p-6 w-[32rem] max-h-[85vh] overflow-y-auto shadow-xl">
        <h2 className="text-lg font-semibold mb-1">Docker not detected</h2>
        <p className="text-sm text-[var(--text-secondary)] mb-4">
          Triple-C needs a Docker-compatible runtime to manage sandboxed project containers.
          We can install <span className="text-[var(--text-primary)]">{options.product_name}</span>{" "}
          for you, or you can follow the official instructions.
        </p>

        {phase === "idle" && (
          <div className="flex flex-col gap-2">
            {options.can_auto_install ? (
              <button
                onClick={handleInstall}
                className="px-3 py-2 text-sm bg-[var(--accent)] text-white rounded hover:bg-[var(--accent-hover)] transition-colors"
              >
                {installVerb} ({options.auto_install_method})
              </button>
            ) : (
              <div className="text-xs text-[var(--text-secondary)] bg-[var(--bg-primary)] border border-[var(--border-color)] rounded p-2">
                One-click install unavailable:{" "}
                <span className="text-[var(--text-primary)]">
                  {options.auto_install_blocker ?? "required tooling missing."}
                </span>
              </div>
            )}

            <button
              onClick={() => setShowManual((s) => !s)}
              className="px-3 py-2 text-sm bg-[var(--bg-tertiary)] border border-[var(--border-color)] rounded hover:bg-[var(--border-color)] transition-colors"
            >
              {showManual ? "Hide manual instructions" : "Show manual instructions"}
            </button>

            <button
              onClick={handleOpenDocs}
              className="px-3 py-2 text-sm bg-[var(--bg-tertiary)] border border-[var(--border-color)] rounded hover:bg-[var(--border-color)] transition-colors"
            >
              Open official documentation ↗
            </button>
          </div>
        )}

        {phase === "installing" && (
          <div className="text-xs text-[var(--text-secondary)]">
            Installing… a system password prompt may appear. Do not close this window.
          </div>
        )}

        {phase === "done" && (
          <div className="flex flex-col gap-2">
            <div className="text-sm text-[var(--success)]">Install finished.</div>
            {options.post_install_notes.length > 0 && (
              <ul className="text-xs text-[var(--text-secondary)] list-disc list-inside space-y-1">
                {options.post_install_notes.map((note, i) => (
                  <li key={i}>{note}</li>
                ))}
              </ul>
            )}
            <div className="flex gap-2 mt-2">
              <button
                onClick={handleRecheck}
                className="px-3 py-2 text-sm bg-[var(--accent)] text-white rounded hover:bg-[var(--accent-hover)] transition-colors"
              >
                Re-check Docker
              </button>
              <button
                onClick={onClose}
                className="px-3 py-2 text-sm bg-[var(--bg-tertiary)] border border-[var(--border-color)] rounded hover:bg-[var(--border-color)] transition-colors"
              >
                Close
              </button>
            </div>
          </div>
        )}

        {phase === "error" && (
          <div className="flex flex-col gap-2">
            <div className="text-sm text-[var(--error)]">Install failed.</div>
            {error && <div className="text-xs font-mono text-[var(--error)]">{error}</div>}
            <div className="flex gap-2 mt-2">
              <button
                onClick={() => setPhase("idle")}
                className="px-3 py-2 text-sm bg-[var(--bg-tertiary)] border border-[var(--border-color)] rounded hover:bg-[var(--border-color)] transition-colors"
              >
                Back
              </button>
              <button
                onClick={handleOpenDocs}
                className="px-3 py-2 text-sm bg-[var(--accent)] text-white rounded hover:bg-[var(--accent-hover)] transition-colors"
              >
                Open official docs ↗
              </button>
            </div>
          </div>
        )}

        {(showManual || phase === "error") && (
          <div className="mt-4">
            <div className="text-xs font-medium mb-1.5 text-[var(--text-secondary)]">
              Manual install steps
            </div>
            <ol className="text-xs text-[var(--text-secondary)] list-decimal list-inside space-y-1 bg-[var(--bg-primary)] border border-[var(--border-color)] rounded p-2">
              {options.manual_steps.map((step, i) => (
                <li key={i}>{step}</li>
              ))}
            </ol>
          </div>
        )}

        {log.length > 0 && (
          <div className="mt-4 max-h-48 overflow-y-auto bg-[var(--bg-primary)] border border-[var(--border-color)] rounded p-2 text-xs font-mono text-[var(--text-secondary)]">
            {log.map((line, i) => (
              <div key={i}>{line}</div>
            ))}
          </div>
        )}

        {phase === "idle" && (
          <div className="mt-4 flex justify-end">
            <button
              onClick={onClose}
              className="text-xs text-[var(--text-secondary)] hover:text-[var(--text-primary)]"
            >
              Dismiss
            </button>
          </div>
        )}
      </div>
    </div>
  );
}
