import { useEffect, useState } from "react";
import { openUrl } from "@tauri-apps/plugin-opener";
import { useInstallHelper } from "../hooks/useInstallHelper";
import { useDocker } from "../hooks/useDocker";
import Modal from "./ui/Modal";
import Button from "./ui/Button";

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

  useEffect(() => {
    loadOptions();
  }, [loadOptions]);

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

  const installVerb =
    phase === "installing" ? "Installing…" : `Install ${options.product_name}`;

  return (
    <Modal
      title="Docker not detected"
      onClose={onClose}
      widthClassName="w-[34rem]"
      // Closing mid-install would orphan a privileged installer.
      dismissible={phase !== "installing"}
      footer={
        phase === "idle" ? (
          <Button variant="ghost" onClick={onClose}>
            Dismiss
          </Button>
        ) : undefined
      }
    >
      <p className="text-[13px] text-[var(--text-secondary)] mb-4">
        Triple-C needs a Docker-compatible runtime to manage sandboxed project
        containers. We can install{" "}
        <span className="text-[var(--text-primary)]">{options.product_name}</span> for
        you, or you can follow the official instructions.
      </p>

      {phase === "idle" && (
        <div className="flex flex-col gap-2">
          {options.can_auto_install ? (
            <Button size="md" variant="primary" onClick={handleInstall}>
              {installVerb} ({options.auto_install_method})
            </Button>
          ) : (
            <div className="text-xs text-[var(--text-secondary)] bg-[var(--bg-primary)] border border-[var(--border-color)] rounded-[var(--radius-control)] p-2">
              One-click install unavailable:{" "}
              <span className="text-[var(--text-primary)]">
                {options.auto_install_blocker ?? "required tooling missing."}
              </span>
            </div>
          )}

          <Button size="md" onClick={() => setShowManual((s) => !s)}>
            {showManual ? "Hide manual instructions" : "Show manual instructions"}
          </Button>

          <Button size="md" onClick={handleOpenDocs}>
            Open official documentation ↗
          </Button>
        </div>
      )}

      {phase === "installing" && (
        <div className="text-xs text-[var(--text-secondary)]">
          Installing… a system password prompt may appear. Do not close this window.
        </div>
      )}

      {phase === "done" && (
        <div className="flex flex-col gap-2">
          <div className="text-[13px] text-[var(--success)]">Install finished.</div>
          {options.post_install_notes.length > 0 && (
            <ul className="text-xs text-[var(--text-secondary)] list-disc list-inside space-y-1">
              {options.post_install_notes.map((note, i) => (
                <li key={i}>{note}</li>
              ))}
            </ul>
          )}
          <div className="flex gap-2 mt-2">
            <Button size="md" variant="primary" onClick={handleRecheck}>
              Re-check Docker
            </Button>
            <Button size="md" onClick={onClose}>
              Close
            </Button>
          </div>
        </div>
      )}

      {phase === "error" && (
        <div className="flex flex-col gap-2">
          <div className="text-[13px] text-[var(--error)]">Install failed.</div>
          {error && (
            <div className="text-xs font-mono text-[var(--error)] break-words">
              {error}
            </div>
          )}
          <div className="flex gap-2 mt-2">
            <Button size="md" onClick={() => setPhase("idle")}>
              Back
            </Button>
            <Button size="md" variant="primary" onClick={handleOpenDocs}>
              Open official docs ↗
            </Button>
          </div>
        </div>
      )}

      {(showManual || phase === "error") && (
        <div className="mt-4">
          <div className="text-xs font-medium mb-1.5 text-[var(--text-secondary)]">
            Manual install steps
          </div>
          <ol className="text-xs text-[var(--text-secondary)] list-decimal list-inside space-y-1 bg-[var(--bg-primary)] border border-[var(--border-color)] rounded-[var(--radius-control)] p-2">
            {options.manual_steps.map((step, i) => (
              <li key={i}>{step}</li>
            ))}
          </ol>
        </div>
      )}

      {log.length > 0 && (
        <div className="mt-4 max-h-48 overflow-y-auto bg-[var(--bg-primary)] border border-[var(--border-color)] rounded-[var(--radius-control)] p-2 text-xs font-mono text-[var(--text-secondary)]">
          {log.map((line, i) => (
            <div key={i}>{line}</div>
          ))}
        </div>
      )}
    </Modal>
  );
}
