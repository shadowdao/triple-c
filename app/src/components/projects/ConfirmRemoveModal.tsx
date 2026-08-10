import Modal from "../ui/Modal";
import Button from "../ui/Button";

interface Props {
  projectName: string;
  onConfirm: () => void;
  onCancel: () => void;
}

export default function ConfirmRemoveModal({ projectName, onConfirm, onCancel }: Props) {
  return (
    <Modal
      title="Remove Project"
      onClose={onCancel}
      widthClassName="w-[26rem]"
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
            Remove
          </Button>
        </>
      }
    >
      <p className="text-[13px] text-[var(--text-secondary)]">
        Are you sure you want to remove{" "}
        <strong className="text-[var(--text-primary)]">{projectName}</strong>? This will
        delete the container, config volume, and stored credentials.
      </p>
    </Modal>
  );
}
