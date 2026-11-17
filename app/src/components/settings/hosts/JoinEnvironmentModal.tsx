import { useState } from "react";
import { awsErrorMessage } from "../../../lib/aws-error";
import type { AwsError, Ec2Choice, Ec2Compute, UserCompute } from "../../../lib/types";
import Button from "../../ui/Button";
import Modal from "../../ui/Modal";
import Ec2CapacityFields from "./Ec2CapacityFields";
import { ec2DraftFrom, ec2FromDraft, UNCONFIRMED_NOTE, type Ec2Draft } from "./environmentFormat";

/**
 * Self-onboarding: join an environment, or re-apply an onboarding to change
 * its EC2 capacity. Starts from the recorded capacity, so a re-apply keeps it
 * unless changed. Open to anyone: AWS refuses the run when a permission is
 * missing, and an onboarding bundle for an admin is the alternative.
 */
export default function JoinEnvironmentModal({
  environment,
  region,
  recorded,
  reapply,
  incomplete = false,
  lastChoice = null,
  confirmedAdmin = true,
  onBundle,
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
  /** The permission check confirmed environment-admin rights; otherwise a note says AWS may refuse. */
  confirmedAdmin?: boolean;
  /** Switch to an onboarding bundle for an admin instead. */
  onBundle?: () => void;
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
        {!confirmedAdmin && <p data-testid="join-unconfirmed-note">{UNCONFIRMED_NOTE}</p>}
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
          <p role="alert" className="text-[var(--error)] break-words">
            {awsErrorMessage(error)}
          </p>
        )}
        {onBundle && (
          <div className="space-y-1.5 pt-1 border-t border-[var(--border-color)]">
            <p>
              {error?.kind === "access_denied"
                ? "AWS refused this with your permissions. An admin can onboard you from a bundle instead."
                : "Can't onboard yourself? An admin can onboard you from a bundle."}
            </p>
            <Button size="sm" wrap onClick={onBundle}>
              Save an onboarding bundle for an admin…
            </Button>
          </div>
        )}
      </div>
    </Modal>
  );
}
