import { useState } from "react";
import Modal from "../ui/Modal";
import Button from "../ui/Button";
import Field, { inputClass, selectClass } from "../ui/Field";
import { addMarketplace } from "../../lib/tauri-commands";
import { useAppState } from "../../store/appState";
import type { MarketplaceSnapshot } from "../../lib/types";

interface Props {
  onClose: () => void;
  onAdded: (snapshot: MarketplaceSnapshot) => void;
}

export default function AddMarketplaceModal({ onClose, onAdded }: Props) {
  const accounts = useAppState((s) => s.appSettings?.marketplace_accounts ?? []);
  const [name, setName] = useState("");
  const [url, setUrl] = useState("");
  const [branch, setBranch] = useState("");
  const [accountId, setAccountId] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const trimmedUrl = url.trim();
  const urlProblem =
    trimmedUrl !== "" && !trimmedUrl.startsWith("https://")
      ? "The repository URL must start with https:// (SSH URLs are not supported)."
      : null;
  const canSubmit = name.trim() !== "" && trimmedUrl !== "" && !urlProblem && !busy;

  const submit = async () => {
    setBusy(true);
    setError(null);
    try {
      const snap = await addMarketplace(
        name.trim(),
        trimmedUrl,
        branch.trim() === "" ? null : branch.trim(),
        accountId === "" ? null : accountId,
      );
      onAdded(snap);
      onClose();
    } catch (e) {
      setError(typeof e === "string" ? e : String(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <Modal
      title="Add marketplace"
      description="Triple-C fetches the repository now to check it can be read. Nothing is saved if that fails."
      widthClassName="w-[36rem]"
      dismissible={!busy}
      onClose={onClose}
      footer={
        <>
          <Button size="md" variant="ghost" onClick={onClose} disabled={busy}>
            Cancel
          </Button>
          <Button size="md" variant="primary" onClick={() => void submit()} disabled={!canSubmit}>
            {busy ? "Checking…" : "Add marketplace"}
          </Button>
        </>
      }
    >
      <div className="space-y-3">
        <Field label="Name">
          {(id) => (
            <input id={id} value={name} onChange={(e) => setName(e.target.value)} className={inputClass} placeholder="Team marketplace" />
          )}
        </Field>
        <Field label="Repository URL" hint={urlProblem ?? "HTTPS clone URL, e.g. https://github.com/owner/repo.git"}>
          {(id) => (
            <input id={id} value={url} onChange={(e) => setUrl(e.target.value)} className={inputClass} placeholder="https://github.com/owner/repo.git" />
          )}
        </Field>
        <Field label="Branch" hint="Leave empty to use the repository's default branch.">
          {(id) => (
            <input id={id} value={branch} onChange={(e) => setBranch(e.target.value)} className={inputClass} placeholder="main" />
          )}
        </Field>
        <Field label="Account" hint="Needed for private repositories. Add accounts on the Accounts tab.">
          {(id) => (
            <select id={id} value={accountId} onChange={(e) => setAccountId(e.target.value)} className={selectClass}>
              <option value="">None (public repository)</option>
              {accounts.map((a) => (
                <option key={a.id} value={a.id}>
                  {a.label} — {a.host}
                  {a.username ? ` (${a.username})` : ""}
                </option>
              ))}
            </select>
          )}
        </Field>
        {error && (
          <p role="alert" className="text-xs text-[var(--error)] whitespace-pre-wrap leading-snug">
            {error}
          </p>
        )}
      </div>
    </Modal>
  );
}
