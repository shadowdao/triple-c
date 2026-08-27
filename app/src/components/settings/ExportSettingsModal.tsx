import { useState } from "react";
import Modal from "../ui/Modal";
import Button from "../ui/Button";
import Field, { inputClass } from "../ui/Field";
import { exportSettings } from "../../lib/tauri-commands";

interface Props {
  onClose: () => void;
}

const MIN_PASSWORD_LENGTH = 8;

/**
 * Password entry for exporting global settings. The save dialog itself opens
 * from Rust once a password is confirmed here — see the doc comment on
 * `commands::settings_export_commands` for why the host path never
 * round-trips through this component.
 */
export default function ExportSettingsModal({ onClose }: Props) {
  const [password, setPassword] = useState("");
  const [confirmPassword, setConfirmPassword] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [done, setDone] = useState(false);

  const mismatch = confirmPassword.length > 0 && password !== confirmPassword;
  const tooShort = password.length > 0 && password.length < MIN_PASSWORD_LENGTH;
  const canSubmit = password.length >= MIN_PASSWORD_LENGTH && password === confirmPassword;

  const handleExport = async () => {
    setError(null);
    setBusy(true);
    try {
      const saved = await exportSettings(password);
      if (saved) setDone(true);
      // `false` means the save dialog was dismissed — close quietly, same as
      // if the user had cancelled the modal itself.
      else onClose();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <Modal
      title="Export settings"
      description="Saves your global settings and any stored credentials (a shared Claude login, gateway keys) to one encrypted file. Project-specific settings and container data are not included."
      widthClassName="w-[28rem]"
      dismissible={!busy}
      onClose={onClose}
      footer={
        done ? (
          <Button size="md" variant="primary" onClick={onClose}>
            Done
          </Button>
        ) : (
          <>
            <Button size="md" variant="ghost" onClick={onClose} disabled={busy}>
              Cancel
            </Button>
            <Button
              size="md"
              variant="primary"
              onClick={() => void handleExport()}
              disabled={!canSubmit || busy}
            >
              {busy ? "Exporting…" : "Choose where to save…"}
            </Button>
          </>
        )
      }
    >
      {done ? (
        <p className="text-[13px] text-[var(--success)]">
          Settings exported. Keep the password somewhere safe — there is no way to recover
          the file without it.
        </p>
      ) : (
        <div className="space-y-3">
          <Field label="Password" hint={`At least ${MIN_PASSWORD_LENGTH} characters. You'll need this exact password to import the file later.`}>
            {(id) => (
              <input
                id={id}
                type="password"
                autoComplete="new-password"
                value={password}
                onChange={(e) => setPassword(e.target.value)}
                disabled={busy}
                className={inputClass}
              />
            )}
          </Field>
          <Field label="Confirm password">
            {(id) => (
              <input
                id={id}
                type="password"
                autoComplete="new-password"
                value={confirmPassword}
                onChange={(e) => setConfirmPassword(e.target.value)}
                disabled={busy}
                className={inputClass}
              />
            )}
          </Field>
          {tooShort && (
            <p className="text-xs text-[var(--error)]">
              Use at least {MIN_PASSWORD_LENGTH} characters.
            </p>
          )}
          {mismatch && <p className="text-xs text-[var(--error)]">Passwords don't match.</p>}
          {error && <p className="text-xs text-[var(--error)]">{error}</p>}
        </div>
      )}
    </Modal>
  );
}
