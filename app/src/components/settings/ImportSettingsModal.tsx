import { useState } from "react";
import Modal from "../ui/Modal";
import Button from "../ui/Button";
import Field, { inputClass } from "../ui/Field";
import { applySettingsImport, previewSettingsImport } from "../../lib/tauri-commands";
import { describeImport, describeImportWarnings } from "../../lib/settingsImportPreview";
import type { AppSettings, SettingsImportPreview } from "../../lib/types";

interface Props {
  onClose: () => void;
  /** Fired once the import is actually applied, so the caller can refresh
   *  whatever reads settings from the store. */
  onImported: (settings: AppSettings) => void;
}

/**
 * Two phases: enter the password and pick the file (backend resolves the
 * file dialog itself — see `commands::settings_export_commands`), then
 * confirm a preview before anything is actually applied. The same password
 * is reused for the second call rather than asking again; nothing about
 * that call needs a fresh secret; the backend just doesn't cache the
 * *decrypted payload* between the two.
 */
export default function ImportSettingsModal({ onClose, onImported }: Props) {
  const [password, setPassword] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [preview, setPreview] = useState<SettingsImportPreview | null>(null);
  const [applied, setApplied] = useState(false);
  const [secretWarnings, setSecretWarnings] = useState<string[]>([]);

  const handleChooseFile = async () => {
    setError(null);
    setBusy(true);
    try {
      const result = await previewSettingsImport(password);
      if (result) setPreview(result);
      else onClose(); // File picker dismissed.
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  const handleConfirm = async () => {
    setError(null);
    setBusy(true);
    try {
      const outcome = await applySettingsImport(password);
      setApplied(true);
      setSecretWarnings(outcome.secret_restore_warnings);
      onImported(outcome.settings);
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <Modal
      title="Import settings"
      description={
        preview
          ? "Review what this file will change before applying it."
          : "Choose a Triple-C settings export and enter the password it was created with."
      }
      widthClassName="w-[28rem]"
      dismissible={!busy}
      onClose={onClose}
      footer={
        applied ? (
          <Button size="md" variant="primary" onClick={onClose}>
            Done
          </Button>
        ) : preview ? (
          <>
            <Button size="md" variant="ghost" onClick={onClose} disabled={busy}>
              Cancel
            </Button>
            <Button size="md" variant="primary" onClick={() => void handleConfirm()} disabled={busy}>
              {busy ? "Importing…" : "Import"}
            </Button>
          </>
        ) : (
          <>
            <Button size="md" variant="ghost" onClick={onClose} disabled={busy}>
              Cancel
            </Button>
            <Button
              size="md"
              variant="primary"
              onClick={() => void handleChooseFile()}
              disabled={!password || busy}
            >
              {busy ? "Opening…" : "Choose file…"}
            </Button>
          </>
        )
      }
    >
      {applied ? (
        <div className="space-y-2">
          <p className="text-[13px] text-[var(--success)]">Settings imported.</p>
          {secretWarnings.map((warning) => (
            <p
              key={warning}
              className="px-2.5 py-2 text-xs text-[var(--error)] bg-[var(--error-muted)] border border-[var(--error)]/40 rounded-[var(--radius-control)] leading-snug"
            >
              {warning}
            </p>
          ))}
        </div>
      ) : preview ? (
        <div className="space-y-3">
          <p className="text-xs text-[var(--text-secondary)]">
            Exported {new Date(preview.exported_at).toLocaleString()} from Triple-C{" "}
            {preview.app_version}.
          </p>
          {/* Warnings render before the replace list, deliberately: the list
           *  below can run long, and the one thing here that most needs to
           *  stay above the fold while scrolling is "this turns on a
           *  network-listening service" or "this runs a different image" —
           *  not a bullet buried among ordinary settings. */}
          {describeImportWarnings(preview).map((warning) => (
            <p
              key={warning}
              className="px-2.5 py-2 text-xs text-[var(--warning)] bg-[var(--warning-muted)] border border-[var(--warning)]/40 rounded-[var(--radius-control)] leading-snug break-all"
            >
              {warning}
            </p>
          ))}
          <div>
            <p className="text-[13px] font-medium text-[var(--text-primary)]">This will replace:</p>
            <ul className="mt-1 list-disc pl-4 text-[13px] text-[var(--text-secondary)] space-y-0.5">
              {describeImport(preview).map((item) => (
                <li key={item} className="break-all">
                  {item}
                </li>
              ))}
            </ul>
          </div>
          {error && <p className="text-xs text-[var(--error)]">{error}</p>}
        </div>
      ) : (
        <div className="space-y-3">
          <Field label="Password">
            {(id) => (
              <input
                id={id}
                type="password"
                autoComplete="current-password"
                value={password}
                onChange={(e) => setPassword(e.target.value)}
                disabled={busy}
                className={inputClass}
              />
            )}
          </Field>
          {error && <p className="text-xs text-[var(--error)]">{error}</p>}
        </div>
      )}
    </Modal>
  );
}
