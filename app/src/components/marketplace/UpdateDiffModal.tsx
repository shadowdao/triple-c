import { useEffect, useState } from "react";
import Modal from "../ui/Modal";
import Button from "../ui/Button";
import { marketplaceItemDiff } from "../../lib/tauri-commands";
import { formatItemRef } from "../../lib/marketplace";
import type { FileDiff, MarketplaceItemRef } from "../../lib/types";

interface Props {
  item: MarketplaceItemRef;
  fromCommit: string;
  toCommit: string;
  scopeLabel: string;
  /**
   * Hooks only: the rendered commands the item runs at `toCommit` (head), from
   * the marketplace snapshot's catalog entry. An update can change what a hook
   * runs without going back through the install-time confirm list, so this is
   * shown alongside the file diff — spec §3. Undefined for non-hook items.
   */
  hookCommands?: string[];
  onClose: () => void;
  /** Resolves true when the update was applied. */
  onAccept: () => Promise<boolean>;
}

const CHANGE_LABEL: Record<FileDiff["change"], string> = {
  added: "added",
  removed: "removed",
  modified: "modified",
};

export default function UpdateDiffModal({
  item,
  fromCommit,
  toCommit,
  scopeLabel,
  hookCommands,
  onClose,
  onAccept,
}: Props) {
  const [diffs, setDiffs] = useState<FileDiff[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    let cancelled = false;
    marketplaceItemDiff(item, fromCommit, toCommit)
      .then((d) => {
        if (!cancelled) setDiffs(d);
      })
      .catch((e) => {
        if (!cancelled) setError(typeof e === "string" ? e : String(e));
      });
    return () => {
      cancelled = true;
    };
  }, [item, fromCommit, toCommit]);

  const accept = async () => {
    setBusy(true);
    try {
      if (await onAccept()) onClose();
    } finally {
      setBusy(false);
    }
  };

  return (
    <Modal
      title={`Update ${formatItemRef(item)}`}
      description={`${scopeLabel}: ${fromCommit.slice(0, 8)} → ${toCommit.slice(0, 8)}. Review the changes before accepting.`}
      widthClassName="w-[52rem]"
      dismissible={!busy}
      onClose={onClose}
      footer={
        <>
          <Button size="md" variant="ghost" onClick={onClose} disabled={busy}>
            Cancel
          </Button>
          <Button size="md" variant="primary" onClick={() => void accept()} disabled={busy || diffs === null}>
            Update
          </Button>
        </>
      }
    >
      {error && <p role="alert" className="text-xs text-[var(--error)]">{error}</p>}
      {!error && diffs === null && <p className="text-xs text-[var(--text-secondary)]">Loading changes…</p>}
      {hookCommands && (
        <div className="mb-3">
          <p className="text-xs font-medium mb-1">Commands after this update</p>
          {hookCommands.length === 0 ? (
            <p className="text-xs text-[var(--text-secondary)]">This hook declares no commands.</p>
          ) : (
            <ul className="space-y-1">
              {hookCommands.map((c) => (
                <li key={c}>
                  <code className="block font-mono text-xs break-all px-2 py-1 rounded-[var(--radius-control)] bg-[var(--bg-primary)] border border-[var(--border-color)]">
                    {c}
                  </code>
                </li>
              ))}
            </ul>
          )}
        </div>
      )}
      {diffs && diffs.length === 0 && (
        <p className="text-xs text-[var(--text-secondary)]">No changes to the item's files or catalog entry.</p>
      )}
      {diffs && diffs.length > 0 && (
        <div className="space-y-3 max-h-[60vh] overflow-auto">
          {diffs.map((d) => (
            <div key={d.path}>
              <p className="text-xs font-mono mb-1">
                {d.path} <span className="text-[var(--text-secondary)]">({CHANGE_LABEL[d.change]})</span>
              </p>
              {d.unified === null ? (
                <p className="text-xs text-[var(--text-secondary)]">Binary file — no text diff</p>
              ) : (
                <pre className="p-2 text-xs font-mono whitespace-pre overflow-auto rounded-[var(--radius-control)] bg-[var(--bg-primary)] border border-[var(--border-color)]">
                  {d.unified}
                </pre>
              )}
            </div>
          ))}
        </div>
      )}
    </Modal>
  );
}
