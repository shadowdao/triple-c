import Modal from "../ui/Modal";
import Button from "../ui/Button";
import ClaudeInstructionsEditor from "./ClaudeInstructionsEditor";

interface Props {
  instructions: string;
  disabled: boolean;
  onSave: (instructions: string) => Promise<void>;
  onClose: () => void;
}

/** Global Claude instructions (Settings). Per-project lives in Config → Runtime. */
export default function ClaudeInstructionsModal({
  instructions,
  disabled,
  onSave,
  onClose,
}: Props) {
  return (
    <Modal
      title="Claude Instructions"
      description="Written to ~/.claude/CLAUDE.md inside containers."
      onClose={onClose}
      widthClassName="w-[40rem]"
      footer={<Button onClick={onClose}>Close</Button>}
    >
      <ClaudeInstructionsEditor
        instructions={instructions}
        disabled={disabled}
        disabledReason="Container must be stopped to change Claude instructions."
        rows={14}
        autoFocus
        onSave={async (value) => {
          try {
            await onSave(value);
          } catch (err) {
            console.error("Failed to update Claude instructions:", err);
          }
        }}
      />
    </Modal>
  );
}
