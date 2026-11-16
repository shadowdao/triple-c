import { useState } from "react";
import { saveAwsAdminBundle } from "../../../lib/tauri-commands";
import { awsErrorMessage, toAwsError } from "../../../lib/aws-error";
import type { AwsError, ModuleKind, NetworkChoice } from "../../../lib/types";
import Button from "../../ui/Button";
import Field, { inputClass } from "../../ui/Field";
import Modal from "../../ui/Modal";
import LifetimeCapField, { parseCapHours } from "./LifetimeCapField";
import NetworkFields from "./NetworkFields";
import { DEFAULT_NETWORK, environmentNameError, networkFromDraft, type NetworkDraft } from "./environmentFormat";

const COMMANDS = "tofu init -backend-config=backend.hcl\ntofu apply";

const TITLE: Record<ModuleKind, string> = {
  environment: "Environment bundle",
  user_onboard: "Onboarding bundle",
  permissions_boundary: "Permissions boundary bundle",
};

/**
 * An admin request (D5): Triple-C writes the module, `backend.hcl`,
 * `terraform.tfvars.json` and a README into a folder picked in Rust, and the
 * admin runs the two commands from there.
 */
export default function AdminBundleModal({
  connectionId,
  region,
  kind,
  environment,
  ownerId,
  network,
  reason,
  onSaved,
  onClose,
}: {
  connectionId: string;
  region: string;
  kind: ModuleKind;
  /** Fixed environment; when absent for an environment bundle, the user picks it here. */
  environment?: string;
  ownerId?: string;
  network?: NetworkChoice;
  /** Why the bundle is needed (e.g. the refusal that led here). */
  reason?: string;
  /** Called with the folder once a bundle was actually written. */
  onSaved?: (folder: string) => void;
  onClose: () => void;
}) {
  const askEnvironment = kind === "environment" && !environment;
  const [name, setName] = useState("main");
  const [draft, setDraft] = useState<NetworkDraft>(DEFAULT_NETWORK);
  const [capText, setCapText] = useState("0");
  const [saving, setSaving] = useState(false);
  const [saved, setSaved] = useState<string | null>(null);
  const [error, setError] = useState<AwsError | null>(null);
  const [copyNote, setCopyNote] = useState<string | null>(null);

  const nameError = askEnvironment ? environmentNameError(name) : null;
  const picked = askEnvironment ? networkFromDraft(draft) : null;
  const capHours = parseCapHours(capText);
  const ready = !nameError && (!picked || "network" in picked) && (!askEnvironment || capHours !== null);

  const save = async () => {
    setSaving(true);
    setError(null);
    try {
      const folder = await saveAwsAdminBundle(connectionId, region, kind, {
        environment: askEnvironment ? name : environment,
        ownerId,
        network: picked && "network" in picked ? picked.network : network,
        ...(askEnvironment && capHours !== null ? { maxWorkspaceHours: capHours } : {}),
      });
      if (folder) {
        setSaved(folder);
        onSaved?.(folder);
      }
    } catch (e) {
      setError(toAwsError(e));
    } finally {
      setSaving(false);
    }
  };

  const copy = async () => {
    try {
      await navigator.clipboard.writeText(COMMANDS);
      setCopyNote("Commands copied.");
    } catch (e) {
      setCopyNote(`Could not copy: ${awsErrorMessage(toAwsError(e))}`);
    }
  };

  return (
    <Modal
      title={TITLE[kind]}
      onClose={onClose}
      footer={
        <Button size="md" onClick={onClose}>
          Close
        </Button>
      }
    >
      <div className="space-y-3 text-xs text-[var(--text-secondary)] leading-snug">
        {reason && (
          <p role="alert" className="text-[var(--warning)]">
            Doing this from here was refused: {reason}
          </p>
        )}
        {kind === "environment" && (
          <p>
            Deploying an environment needs an environment admin. Triple-C writes the OpenTofu module with its
            backend settings and variables into a folder; send it to an admin, who deploys it from there.
          </p>
        )}
        {kind === "user_onboard" && (
          <p>
            Joining {environment} needs your own task role and file system, which an environment admin creates
            by applying this bundle. Send them the folder; once it's applied, Detect again shows you as joined.
          </p>
        )}
        {kind === "permissions_boundary" && (
          <p>
            Every role Triple-C creates is capped by the account's permissions boundary, and creating it needs an
            account administrator. Send them this folder.
          </p>
        )}
        <p>The bundle works with OpenTofu or Terraform 1.10 and later. Its README lists every step.</p>
        {askEnvironment && (
          <div className="space-y-3">
            <Field label="Name" hint={nameError ?? undefined}>
              {(id) => <input id={id} className={inputClass} value={name} onChange={(e) => setName(e.target.value)} />}
            </Field>
            <NetworkFields value={draft} onChange={setDraft} error={picked && "error" in picked ? picked.error : null} />
            <LifetimeCapField value={capText} onChange={setCapText} />
          </div>
        )}
        <Button size="md" variant="primary" onClick={save} disabled={saving || !ready}>
          {saving ? "Saving…" : "Save bundle to folder…"}
        </Button>
        {error && (
          <p role="alert" className="text-[var(--error)]">
            {awsErrorMessage(error)}
          </p>
        )}
        {saved && (
          <div className="space-y-1.5">
            <p>
              Saved to <code className="font-mono text-[var(--text-primary)] break-all">{saved}</code>
            </p>
            <p>From that folder, run:</p>
            <pre className="font-mono text-[var(--text-primary)] bg-[var(--bg-primary)] border border-[var(--border-color)] rounded-[var(--radius-control)] p-2 whitespace-pre-wrap">
              {COMMANDS}
            </pre>
            <Button size="sm" onClick={copy}>
              Copy commands
            </Button>
            {copyNote && <p role="status">{copyNote}</p>}
          </div>
        )}
      </div>
    </Modal>
  );
}
