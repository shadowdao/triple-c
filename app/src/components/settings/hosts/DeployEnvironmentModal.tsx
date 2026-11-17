import { useState } from "react";
import { deployAwsPermissionsBoundary } from "../../../lib/tauri-commands";
import { awsErrorMessage } from "../../../lib/aws-error";
import type { AwsError, EnvironmentScan, NetworkChoice } from "../../../lib/types";
import Button from "../../ui/Button";
import Field, { inputClass } from "../../ui/Field";
import Modal from "../../ui/Modal";
import AdminBundleModal from "./AdminBundleModal";
import DeployProgress, { runKey, useDeployRuns } from "./DeployProgress";
import LifetimeCapField, { parseCapHours } from "./LifetimeCapField";
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
  onBundle,
}: {
  connectionId: string;
  region: string;
  scan: EnvironmentScan;
  /** The environment to repair; absent for a new deploy. */
  repair?: string;
  onClose: () => void;
  /** Starts the run; resolves to the command's error, or null once started. */
  onDeploy: (name: string, network: NetworkChoice, maxWorkspaceHours: number) => Promise<AwsError | null>;
  onDetect: () => void;
  /** Switch to an environment bundle for an admin (offered when AWS refuses the deploy). */
  onBundle?: () => void;
}) {
  const [name, setName] = useState("main");
  const [draft, setDraft] = useState<NetworkDraft>(DEFAULT_NETWORK);
  const [capText, setCapText] = useState("0");
  const [submitting, setSubmitting] = useState(false);
  const [error, setError] = useState<AwsError | null>(null);
  const [boundaryDone, setBoundaryDone] = useState(false);
  const [boundaryError, setBoundaryError] = useState<AwsError | null>(null);
  const [bundle, setBundle] = useState<{ reason?: string } | null>(null);
  const boundary = useDeployRuns(({ result }) => {
    if (result?.ok) setBoundaryDone(true);
    onDetect();
  });
  const boundaryRun = boundary.runs[runKey(region, BOUNDARY_STATE_KEY)];
  const boundaryFailed = boundaryRun?.done && boundaryRun.result && !boundaryRun.result.ok ? boundaryRun.result : null;

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
  const capHours = parseCapHours(capText);
  const ready = !nameError && "network" in picked && capHours !== null && !submitting;

  const createBoundary = async () => {
    setBoundaryError(null);
    const err = await boundary.start(BOUNDARY_STATE_KEY, region, "Permissions boundary", () =>
      deployAwsPermissionsBoundary(connectionId, region),
    );
    if (err?.kind === "access_denied") setBundle({ reason: awsErrorMessage(err) });
    else if (err) setBoundaryError(err);
  };

  const submit = async () => {
    if (!("network" in picked) || capHours === null) return;
    setSubmitting(true);
    setError(null);
    const err = await onDeploy(target, picked.network, capHours);
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
              wrap
              variant="primary"
              onClick={createBoundary}
              disabled={!!boundaryRun && !boundaryRun.done}
            >
              Create the permissions boundary (needs an account administrator)
            </Button>
            <Button size="md" wrap onClick={() => setBundle({})}>
              Save a bundle for an administrator instead
            </Button>
          </div>
          {boundaryError && (
            <p role="alert" className="text-[var(--error)] break-words">
              {awsErrorMessage(boundaryError)}
            </p>
          )}
          {boundaryRun && <DeployProgress run={boundaryRun} connectionId={connectionId} />}
          {boundaryFailed && (
            <div className="space-y-1.5">
              <p>
                Creating the boundary failed. If it was refused, an account administrator can create it from a
                bundle.
              </p>
              <Button
                size="md"
                wrap
                onClick={() =>
                  setBundle({
                    reason: boundaryFailed.summary ?? `OpenTofu exited with code ${boundaryFailed.exit_code}`,
                  })
                }
              >
                Get the boundary bundle for an administrator
              </Button>
            </div>
          )}
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
          <LifetimeCapField value={capText} onChange={setCapText} />
          <p className="text-xs text-[var(--text-secondary)] leading-snug">
            First deploy downloads OpenTofu (~30 MB) and the AWS provider (~150 MB).
          </p>
          {error && (
            <p role="alert" className="text-xs text-[var(--error)] leading-snug break-words">
              {awsErrorMessage(error)}
            </p>
          )}
          {error?.kind === "access_denied" && onBundle && !repair && (
            <div className="space-y-1.5 text-xs text-[var(--text-secondary)] leading-snug">
              <p>An admin can deploy this environment for you from a bundle.</p>
              <Button size="sm" wrap onClick={onBundle}>
                Save an environment bundle for an admin…
              </Button>
            </div>
          )}
        </div>
      )}
    </Modal>
  );
}
