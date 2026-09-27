import Modal from "../ui/Modal";
import Button from "../ui/Button";
import type { CatalogItem } from "../../lib/types";

interface Props {
  item: CatalogItem;
  /** The commit whose commands are listed; the install pins exactly this one. */
  commit: string;
  onConfirm: () => void;
  onCancel: () => void;
}

/** Hooks run shell commands in every Claude session, so installing one is always confirmed. */
export default function HookConfirmModal({ item, commit, onConfirm, onCancel }: Props) {
  return (
    <Modal
      title={`Install hook “${item.name}”?`}
      description={`This hook runs the commands below inside the container whenever its event fires.${
        commit ? ` Version ${commit.slice(0, 8)}.` : ""
      }`}
      widthClassName="w-[40rem]"
      onClose={onCancel}
      footer={
        <>
          <Button size="md" variant="ghost" onClick={onCancel}>
            Cancel
          </Button>
          <Button size="md" variant="primary" onClick={onConfirm}>
            Install hook
          </Button>
        </>
      }
    >
      {item.hook_commands.length === 0 ? (
        <p className="text-xs text-[var(--text-secondary)]">This hook declares no commands.</p>
      ) : (
        <ul className="space-y-1">
          {item.hook_commands.map((c) => (
            <li key={c}>
              <code className="block font-mono text-xs break-all px-2 py-1 rounded-[var(--radius-control)] bg-[var(--bg-primary)] border border-[var(--border-color)]">
                {c}
              </code>
            </li>
          ))}
        </ul>
      )}
    </Modal>
  );
}
