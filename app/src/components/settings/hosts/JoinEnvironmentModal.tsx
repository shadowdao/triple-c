import { useState } from "react";
import { awsErrorMessage } from "../../../lib/aws-error";
import type { AwsError, Ec2Choice, Ec2Compute, UserCompute } from "../../../lib/types";
import Button from "../../ui/Button";
import Modal from "../../ui/Modal";
import Ec2CapacityFields from "./Ec2CapacityFields";
import { ec2DraftFrom, ec2FromDraft, type Ec2Draft } from "./environmentFormat";

/**
 * Self-onboarding (environment admins): join an environment, or re-apply an
 * onboarding to change its EC2 capacity. Starts from the recorded capacity,
 * so a re-apply keeps it unless changed.
 */
export default function JoinEnvironmentModal({
  environment,
  region,
  recorded,
  reapply,
  incomplete = false,
  lastChoice = null,
  onJoin,
  onClose,
}: {
  environment: string;
  region: string;
  /** The caller's recorded EC2 capacity, if any. */
  recorded: UserCompute | null;
  /** Already joined: this changes the onboarding's EC2 capacity. */
  reapply: boolean;
  /** The last onboarding didn't finish (it may have built EC2 capacity
   *  before writing the record). */
  incomplete?: boolean;
  /** The EC2 capacity last chosen here in this run of the app, if any:
   *  the starting point when there is no record. */
  lastChoice?: Ec2Compute | null;
  /** Starts the run; resolves to the command's error, or null once started. */
  onJoin: (ec2: Ec2Choice) => Promise<AwsError | null>;
  onClose: () => void;
}) {
  const [draft, setDraft] = useState<Ec2Draft>(() => ec2DraftFrom(recorded ?? lastChoice));
  const [submitting, setSubmitting] = useState(false);
  const [error, setError] = useState<AwsError | null>(null);
  const picked = ec2FromDraft(draft);
  const ready = "compute" in picked && !submitting;
  const verb = reapply ? "Apply" : "Join";

  const submit = async () => {
    if (!("compute" in picked)) return;
    setSubmitting(true);
    setError(null);
    const choice: Ec2Choice = picked.compute ? { kind: "ec2", ...picked.compute } : { kind: "none" };
    const err = await onJoin(choice);
    setSubmitting(false);
    if (err) setError(err);
    else onClose();
  };

  return (
    <Modal
      title={reapply ? `EC2 capacity in ${environment}` : `Join ${environment}`}
      description={`In ${region}.`}
      onClose={onClose}
      footer={
        <>
          <Button size="md" onClick={onClose}>
            Cancel
          </Button>
          <Button size="md" variant="primary" onClick={submit} disabled={!ready}>
            {submitting ? `${verb}ing…` : verb}
          </Button>
        </>
      }
    >
      <div className="space-y-3 text-xs text-[var(--text-secondary)] leading-snug">
        {!reapply && <p>Joining creates your own task role and file system in {environment}.</p>}
        {recorded && (
          <p>Stop your EC2 workspaces before removing EC2 capacity or changing your triple-c-user value: that change can't finish while one runs.</p>
        )}
        <Ec2CapacityFields value={draft} onChange={setDraft} />
        {recorded && !draft.enabled && (
          <p role="alert" className="text-[var(--warning)]">
            This removes your EC2 capacity ({recorded.instance_type}) in {environment}.
          </p>
        )}
        {incomplete && !recorded && !draft.enabled && (
          <p role="alert" className="text-[var(--warning)]">
            Your last onboarding in {environment} didn't finish, and it may have built part of your EC2 capacity.
            Choosing None removes anything it built; to keep it, choose your own EC2 instances with the same values.
          </p>
        )}
        {error && (
          <p role="alert" className="text-[var(--error)]">
            {awsErrorMessage(error)}
          </p>
        )}
      </div>
    </Modal>
  );
}
