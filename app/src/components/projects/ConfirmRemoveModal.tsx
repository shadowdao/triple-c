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
      {/*
        Everything remove_project() destroys, named. It removes the container,
        *both* named volumes (triple-c-home-{id} and triple-c-claude-config-{id}),
        the triple-c-snapshot-{id} image and the project's keychain secrets — so
        an accurate warning has to reach past "the config volume". The last
        sentence is the reassuring half and matters just as much: project folders
        are bind mounts from the host and nothing here touches them.
      */}
      <p className="text-[13px] text-[var(--text-secondary)]">
        Are you sure you want to remove{" "}
        <strong className="text-[var(--text-primary)]">{projectName}</strong>? This deletes
        its container, both of its volumes and its saved container image — so the home
        directory, the Claude login and config, installed skills, session transcripts,
        scheduled tasks and any stored credentials all go with it.
      </p>
      <p className="mt-2 text-[13px] text-[var(--text-secondary)]">
        Your project folders on this machine are mounted in, not copied, and are left
        untouched.
      </p>
    </Modal>
  );
}
