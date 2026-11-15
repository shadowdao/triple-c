import { useState } from "react";
import { deployAwsPermissionsBoundary } from "../../../lib/tauri-commands";
import { awsErrorMessage } from "../../../lib/aws-error";
import type { AwsError, EnvironmentScan, NetworkChoice } from "../../../lib/types";
import Button from "../../ui/Button";
import Field, { inputClass } from "../../ui/Field";
import Modal from "../../ui/Modal";
import AdminBundleModal from "./AdminBundleModal";
import DeployProgress, { useDeployRuns } from "./DeployProgress";
import NetworkFields from "./NetworkFields";
import {
  BOUNDARY_STATE_KEY,
  DEFAULT_NETWORK,
  environmentNameError,
  networkFromDraft,
  type NetworkDraft,
} from "./environmentFormat";

/**
 * Deploy a new environment (name + D4 network), or — with `repair` — pick the
 * network for an environment that has no record to take it from. A missing
 * permissions boundary is a first step before the form.
 */
export default function DeployEnvironmentModal({
  connectionId,
  region,
  scan,
  repair,
  onClose,
  onDeploy,
  onDetect,
}: {
  connectionId: string;
  region: string;
  scan: EnvironmentScan;
  /** The environment to repair; absent for a new deploy. */
  repair?: string;
  onClose: () => void;
  /** Starts the run; resolves to the command's error, or null once started. */
  onDeploy: (name: string, network: NetworkChoice) => Promise<AwsError | null>;
  onDetect: () => void;
}) {
  const [name, setName] = useState("main");
  const [draft, setDraft] = useState<NetworkDraft>(DEFAULT_NETWORK);
  const [submitting, setSubmitting] = useState(false);
  const [error, setError] = useState<AwsError | null>(null);
  const [boundaryDone, setBoundaryDone] = useState(false);
  const [boundaryError, setBoundaryError] = useState<AwsError | null>(null);
  const [bundle, setBundle] = useState<{ reason?: string } | null>(null);
  const boundary = useDeployRuns(({ result }) => {
    if (result?.ok) setBoundaryDone(true);
    onDetect();
  });
  const boundaryRun = boundary.runs[BOUNDARY_STATE_KEY];

  if (bundle) {
    return (
      <AdminBundleModal
        connectionId={connectionId}
        region={region}
        kind="permissions_boundary"
        reason={bundle.reason}
        onClose={() => setBundle(null)}
      />
    );
  }

  const needsBoundary = !repair && scan.boundary === "no" && !boundaryDone;
  const target = repair ?? name;
  const nameError = repair
    ? null
    : environmentNameError(name) ??
      (scan.environments.some((e) => e.name === name) ? `An environment named ${name} already exists here.` : null);
  const picked = networkFromDraft(draft);
  const ready = !nameError && "network" in picked && !submitting;

  const createBoundary = async () => {
    setBoundaryError(null);
    const err = await boundary.start(BOUNDARY_STATE_KEY, region, "Permissions boundary", () =>
      deployAwsPermissionsBoundary(connectionId, region),
    );
    if (err?.kind === "access_denied") setBundle({ reason: awsErrorMessage(err) });
    else if (err) setBoundaryError(err);
  };

  const submit = async () => {
    if (!("network" in picked)) return;
    setSubmitting(true);
    setError(null);
    const err = await onDeploy(target, picked.network);
    setSubmitting(false);
    if (err) setError(err);
    else onClose();
  };

  const verb = repair ? "Repair" : "Deploy";
  return (
    <Modal
      title={repair ? `Repair ${repair}` : "Deploy a new environment"}
      description={`In ${region}.`}
      onClose={onClose}
      footer={
        <>
          <Button size="md" onClick={onClose}>
            Cancel
          </Button>
          {!needsBoundary && (
            <Button size="md" variant="primary" onClick={submit} disabled={!ready}>
              {submitting ? `${verb}ing…` : verb}
            </Button>
          )}
        </>
      }
    >
      {needsBoundary ? (
        <div className="space-y-3 text-xs text-[var(--text-secondary)] leading-snug">
          <p>
            This account has no Triple-C permissions boundary yet. Every role an environment creates is capped by
            it, so it comes first.
          </p>
          <div className="flex gap-2 flex-wrap">
            <Button
              size="md"
              variant="primary"
              onClick={createBoundary}
              disabled={!!boundaryRun && !boundaryRun.done}
            >
              Create the permissions boundary (needs an account administrator)
            </Button>
            <Button size="md" onClick={() => setBundle({})}>
              Save a bundle for an administrator instead
            </Button>
          </div>
          {boundaryError && (
            <p role="alert" className="text-[var(--error)]">
              {awsErrorMessage(boundaryError)}
            </p>
          )}
          {boundaryRun && <DeployProgress run={boundaryRun} connectionId={connectionId} />}
        </div>
      ) : (
        <div className="space-y-4">
          {repair ? (
            <p className="text-xs text-[var(--warning)] leading-snug">
              {repair} has no record, so Triple-C can't tell which network it was deployed with. Choose the same one:
              a different network than the one it was deployed with replaces the network resources (VPC, subnets,
              NAT gateway).
            </p>
          ) : (
            <Field label="Name" hint={nameError ?? "Lowercase letters, digits and hyphens; up to 20 characters."}>
              {(id) => <input id={id} className={inputClass} value={name} onChange={(e) => setName(e.target.value)} />}
            </Field>
          )}
          <NetworkFields value={draft} onChange={setDraft} error={"error" in picked ? picked.error : null} />
          <p className="text-xs text-[var(--text-secondary)] leading-snug">
            First deploy downloads OpenTofu (~30 MB) and the AWS provider (~150 MB).
          </p>
          {error && (
            <p role="alert" className="text-xs text-[var(--error)] leading-snug">
              {awsErrorMessage(error)}
            </p>
          )}
        </div>
      )}
    </Modal>
  );
}
