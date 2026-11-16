import { useState } from "react";
import { awsErrorMessage } from "../../../lib/aws-error";
import type { AwsError } from "../../../lib/types";
import Button from "../../ui/Button";
import Modal from "../../ui/Modal";
import LifetimeCapField, { parseCapHours } from "./LifetimeCapField";

/**
 * Update an environment to the app's module version. The lifetime cap is
 * pre-filled from the environment's record, so an update keeps it unless
 * the admin changes it.
 */
export default function UpdateEnvironmentModal({
  name,
  region,
  currentHours,
  onClose,
  onUpdate,
}: {
  name: string;
  region: string;
  /** The cap in the environment's record (0 = none). */
  currentHours: number;
  onClose: () => void;
  /** Starts the run; resolves to the command's error, or null once started. */
  onUpdate: (hours: number) => Promise<AwsError | null>;
}) {
  const [text, setText] = useState(String(currentHours));
  const [submitting, setSubmitting] = useState(false);
  const [error, setError] = useState<AwsError | null>(null);
  const hours = parseCapHours(text);

  const submit = async () => {
    if (hours === null) return;
    setSubmitting(true);
    setError(null);
    const err = await onUpdate(hours);
    setSubmitting(false);
    if (err) setError(err);
    else onClose();
  };

  return (
    <Modal
      title={`Update ${name}`}
      description={`In ${region}.`}
      onClose={onClose}
      footer={
        <>
          <Button size="md" onClick={onClose}>
            Cancel
          </Button>
          <Button size="md" variant="primary" onClick={submit} disabled={hours === null || submitting}>
            {submitting ? "Updating…" : "Update"}
          </Button>
        </>
      }
    >
      <div className="space-y-4">
        <LifetimeCapField value={text} onChange={setText} />
        {error && (
          <p role="alert" className="text-xs text-[var(--error)] leading-snug">
            {awsErrorMessage(error)}
          </p>
        )}
      </div>
    </Modal>
  );
}
