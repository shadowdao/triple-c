import { useState } from "react";
import Modal from "../ui/Modal";
import Button from "../ui/Button";

interface Props {
  projectName: string;
  /** An AWS project: its task, task definitions and secret go instead of Docker state. */
  aws?: boolean;
  /** AWS: the workspace may have an EFS access point (offers removing it). */
  savedFiles?: boolean;
  /** AWS: `/workspace` may be on EFS too (EFS storage, now or before). */
  workspaceFiles?: boolean;
  onConfirm: (opts: { discardData: boolean }) => void;
  onCancel: () => void;
}

const REMOVE_CLASS =
  "bg-[var(--error-emphasis)] text-white border border-transparent hover:opacity-90";

export default function ConfirmRemoveModal({
  projectName,
  aws = false,
  savedFiles = false,
  workspaceFiles = false,
  onConfirm,
  onCancel,
}: Props) {
  const [discardData, setDiscardData] = useState(false);
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
            onClick={() => onConfirm({ discardData: savedFiles && discardData })}
            className={REMOVE_CLASS}
          >
            Remove
          </Button>
        </>
      }
    >
      {aws ? (
        <>
          <p className="text-[13px] text-[var(--text-secondary)]">
            Are you sure you want to remove{" "}
            <strong className="text-[var(--text-primary)]">{projectName}</strong>? This stops its
            workspace on AWS and deletes its secret. Its task definitions stay registered (they hold no secrets and cost
            nothing); an administrator can deregister them. Work that was not pushed to a
            remote is lost.
          </p>
          {savedFiles && (
            <label className="mt-3 flex items-start gap-2 text-[13px] text-[var(--text-primary)]">
              <input
                type="checkbox"
                className="mt-0.5"
                checked={discardData}
                onChange={(e) => setDiscardData(e.target.checked)}
              />
              <span>
                Also remove the workspace&rsquo;s file-system access points
                <span className="block text-xs text-[var(--text-secondary)]">
                  The saved files themselves (Claude login, history, SSH keys
                  {workspaceFiles ? ", and the /workspace files" : ""}) stay on your own EFS file
                  system, readable only by your workspace role, until an administrator offboards
                  you.
                </span>
              </span>
            </label>
          )}
        </>
      ) : (
        <>
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
        </>
      )}
    </Modal>
  );
}

/** The second confirm when an AWS project's connection is gone. */
export function ConfirmForgetAwsModal({
  projectName,
  reason,
  onConfirm,
  onCancel,
}: {
  projectName: string;
  reason: string;
  onConfirm: () => void;
  onCancel: () => void;
}) {
  return (
    <Modal
      title="Remove without its AWS resources?"
      onClose={onCancel}
      widthClassName="w-[28rem]"
      footer={
        <>
          <Button size="md" variant="ghost" onClick={onCancel}>
            Cancel
          </Button>
          <Button size="md" onClick={onConfirm} className={REMOVE_CLASS}>
            Remove and leave them in AWS
          </Button>
        </>
      }
    >
      <p className="text-[13px] text-[var(--text-secondary)]">{reason}</p>
      <p className="mt-2 text-[13px] text-[var(--text-secondary)]">
        Removing <strong className="text-[var(--text-primary)]">{projectName}</strong> now
        leaves its AWS resources in the account: the task (which may still be running and
        billed), its task definitions, its secret and its saved files. You would have to
        remove them in AWS yourself.
      </p>
    </Modal>
  );
}
