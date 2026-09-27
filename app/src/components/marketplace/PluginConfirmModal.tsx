import Modal from "../ui/Modal";
import Button from "../ui/Button";
import type { CatalogItem } from "../../lib/types";

interface Props {
  item: CatalogItem;
  /** The commit whose components are listed; the install pins exactly this one. */
  commit: string;
  onConfirm: () => void;
  onCancel: () => void;
}

/**
 * Plugins can bring hooks, MCP servers and commands — from their catalog
 * entry as well as their folder — so installing one is always confirmed with
 * everything that will run listed (PR review #4).
 */
export default function PluginConfirmModal({ item, commit, onConfirm, onCancel }: Props) {
  return (
    <Modal
      title={`Install plugin “${item.name}”?`}
      description={`This plugin adds what is listed below to Claude Code inside the container; hooks and servers run there.${
        commit ? ` Version ${commit.slice(0, 8)}.` : ""
      }`}
      widthClassName="w-[44rem]"
      onClose={onCancel}
      footer={
        <>
          <Button size="md" variant="ghost" onClick={onCancel}>
            Cancel
          </Button>
          <Button size="md" variant="primary" onClick={onConfirm}>
            Install plugin
          </Button>
        </>
      }
    >
      {item.plugin_components.length === 0 ? (
        <p className="text-xs text-[var(--text-secondary)]">
          This plugin declares no hooks, MCP servers or commands. It may still add skills or agents.
        </p>
      ) : (
        <ul className="space-y-2 max-h-[60vh] overflow-auto">
          {item.plugin_components.map((c) => (
            <li key={c.label}>
              <p className="text-xs font-medium mb-1">{c.label}</p>
              <pre className="p-2 text-xs font-mono whitespace-pre-wrap break-all rounded-[var(--radius-control)] bg-[var(--bg-primary)] border border-[var(--border-color)]">
                {c.content}
              </pre>
            </li>
          ))}
        </ul>
      )}
    </Modal>
  );
}
