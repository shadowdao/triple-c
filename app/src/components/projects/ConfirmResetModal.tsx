import Modal from "../ui/Modal";
import Button from "../ui/Button";

interface Props {
  projectName: string;
  onConfirm: () => void;
  onCancel: () => void;
}

/**
 * Reset is destructive in a way its name does not advertise.
 *
 * `rebuild_project_container` calls `remove_project_volumes`, which deletes
 * both `triple-c-home-{id}` and `triple-c-claude-config-{id}` — so the OAuth
 * login, any skills or agents installed in the container, and every session
 * transcript go with them. That is intentional (Reset exists to get back to a
 * clean base image), but it is not recoverable, so it gets the same
 * confirmation gate as Remove.
 */
export default function ConfirmResetModal({ projectName, onConfirm, onCancel }: Props) {
  return (
    <Modal
      title="Reset container"
      onClose={onCancel}
      widthClassName="w-[28rem]"
      footer={
        <>
          <Button size="md" variant="ghost" onClick={onCancel}>
            Cancel
          </Button>
          <Button
            size="md"
            onClick={onConfirm}
            className="bg-[var(--error-emphasis)] text-white border border-transparent hover:opacity-90"
          >
            Reset container
          </Button>
        </>
      }
    >
      <div className="space-y-2.5 text-[13px] text-[var(--text-secondary)]">
        <p>
          Rebuild{" "}
          <strong className="text-[var(--text-primary)]">{projectName}</strong>&rsquo;s
          container from the clean base image.
        </p>
        <p>
          This deletes the container&rsquo;s volumes, which means you will lose:
        </p>
        <ul className="list-disc pl-5 space-y-1">
          <li>
            your <code className="font-mono">claude login</code> &mdash; you will need to
            sign in again
          </li>
          <li>any skills, agents or plugins installed inside the container</li>
          <li>every saved session transcript, so past sessions cannot be resumed</li>
          <li>anything installed with <code className="font-mono">apt</code>, <code className="font-mono">pip</code> or <code className="font-mono">npm</code></li>
        </ul>
        <p>
          Your mounted project folders are on the host and are{" "}
          <strong className="text-[var(--text-primary)]">not</strong> affected.
        </p>
      </div>
    </Modal>
  );
}
