import type { EnvVar } from "../../lib/types";
import Modal from "../ui/Modal";
import Button from "../ui/Button";
import EnvVarsEditor from "./EnvVarsEditor";

interface Props {
  envVars: EnvVar[];
  disabled: boolean;
  onSave: (vars: EnvVar[]) => Promise<void>;
  onClose: () => void;
}

/** Global env vars (Settings). Per-project vars live inline in Config → Access. */
export default function EnvVarsModal({ envVars, disabled, onSave, onClose }: Props) {
  return (
    <Modal
      title="Environment Variables"
      onClose={onClose}
      widthClassName="w-[36rem]"
      footer={<Button onClick={onClose}>Close</Button>}
    >
      <EnvVarsEditor
        envVars={envVars}
        disabled={disabled}
        disabledReason="Container must be stopped to change environment variables."
        onSave={async (vars) => {
          try {
            await onSave(vars);
          } catch (err) {
            console.error("Failed to update environment variables:", err);
          }
        }}
      />
    </Modal>
  );
}
