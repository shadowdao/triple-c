import type { ClaudeCodeSettings } from "../../lib/types";
import Modal from "../ui/Modal";
import Button from "../ui/Button";
import ClaudeCodeSettingsEditor from "./ClaudeCodeSettingsEditor";

interface Props {
  settings: ClaudeCodeSettings | null;
  disabled: boolean;
  onSave: (settings: ClaudeCodeSettings | null) => Promise<void>;
  onClose: () => void;
}

/** Global Claude Code settings (Settings). Per-project lives in Config → Runtime. */
export default function ClaudeCodeSettingsModal({
  settings,
  disabled,
  onSave,
  onClose,
}: Props) {
  return (
    <Modal
      title="Claude Code Settings"
      onClose={onClose}
      widthClassName="w-[34rem]"
      footer={<Button onClick={onClose}>Close</Button>}
    >
      <ClaudeCodeSettingsEditor
        settings={settings}
        disabled={disabled}
        disabledReason="Container must be stopped to change Claude Code settings."
        onSave={async (next) => {
          try {
            await onSave(next);
          } catch (err) {
            console.error("Failed to save Claude Code settings:", err);
          }
        }}
      />
    </Modal>
  );
}
