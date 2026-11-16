import { useEffect, useState } from "react";
import type { AwsWorkspaceConfig, HostConnection, Project, RepoSource } from "../../../../lib/types";
import * as commands from "../../../../lib/tauri-commands";
import { useAppState } from "../../../../store/appState";
import {
  COPY_AT_CONNECT_GATEWAY_NOTE,
  COPY_AT_CONNECT_HINT,
  COPY_AT_CONNECT_SSH_NOTE,
  COPY_AT_CONNECT_WARNINGS,
  SECRETS_MANAGER_HINT,
  EFS_HINT,
  EPHEMERAL_WARNING,
  FARGATE_CPUS,
  defaultAwsWorkspace,
  formatCpu,
  formatMemory,
  memoryError,
  hasSavedFiles,
  hasWorkspaceFiles,
  memoryOptions,
  nearestMemory,
} from "../../../../lib/awsWorkspace";
import Button from "../../../ui/Button";
import Modal from "../../../ui/Modal";
import Toggle from "../../../ui/Toggle";
import SegmentedControl from "../../../ui/SegmentedControl";
import Field, { ConfigGroup, SwitchRow, inputClass, selectClass } from "../../../ui/Field";
import RepoListEditor from "../../RepoListEditor";

interface Props {
  project: Project;
  save: (patch: Partial<Project>) => Promise<boolean>;
  disabled: boolean;
}

const DEFAULT_IDLE_MINUTES = 60;
const STILL_RUNNING = "The task is still running — stop it first.";

/**
 * Where an AWS project runs and what its Fargate task looks like. Only shown
 * for AWS projects; a local project's host is chosen when it is added.
 */
/** Stable, so the store selector does not return a new array each render. */
const NO_HOSTS: HostConnection[] = [];

export default function HostSection({ project, save, disabled }: Props) {
  const aws: AwsWorkspaceConfig = project.aws ?? defaultAwsWorkspace();
  const hosts = useAppState((s) => s.appSettings?.hosts ?? NO_HOSTS);
  const connection = hosts.find((h) => h.id === project.host_id);

  const [memory, setMemory] = useState(String(aws.memory_mib));
  const [memoryProblem, setMemoryProblem] = useState<string | null>(null);
  const [ephemeral, setEphemeral] = useState(String(aws.ephemeral_gib));
  const [ephemeralProblem, setEphemeralProblem] = useState<string | null>(null);
  const [idle, setIdle] = useState(String(aws.idle_stop_minutes ?? DEFAULT_IDLE_MINUTES));
  const [idleProblem, setIdleProblem] = useState<string | null>(null);
  const [confirmRemoveResources, setConfirmRemoveResources] = useState(false);

  useEffect(() => {
    setMemory(String(aws.memory_mib));
    setEphemeral(String(aws.ephemeral_gib));
    setIdle(String(aws.idle_stop_minutes ?? DEFAULT_IDLE_MINUTES));
  }, [aws.memory_mib, aws.ephemeral_gib, aws.idle_stop_minutes]);

  const saveAws = (patch: Partial<AwsWorkspaceConfig>) => save({ aws: { ...aws, ...patch } });

  const hasResources = !!(
    aws.task_arn ||
    aws.access_point_id ||
    aws.workspace_access_point_id ||
    aws.task_definition_arn
  );
  // A removal that kept the saved files keeps their access points stored.
  const onlyAccessPoint =
    !!(aws.access_point_id || aws.workspace_access_point_id) &&
    !aws.task_arn &&
    !aws.task_definition_arn;
  // Switched away from EFS: its /workspace files are still on their access point.
  const keptWorkspaceFiles = aws.storage !== "efs" && !!aws.workspace_access_point_id;
  // Not `disabled` (which also allows Error): on AWS, Error can mean a live task.
  const stopped = project.status === "stopped";

  const commitMemory = () => {
    const value = Number(memory);
    const problem = memoryError(aws.cpu, value);
    setMemoryProblem(problem);
    if (problem || value === aws.memory_mib) return;
    void saveAws({ memory_mib: value });
  };

  const commitEphemeral = () => {
    const value = Number(ephemeral);
    const ok = Number.isInteger(value) && value >= 21 && value <= 200;
    setEphemeralProblem(ok ? null : "Ephemeral storage must be 21 to 200 GiB");
    if (ok && value !== aws.ephemeral_gib) void saveAws({ ephemeral_gib: value });
  };

  const commitIdle = () => {
    const value = Number(idle);
    const ok = Number.isInteger(value) && value >= 5 && value <= 1440;
    setIdleProblem(ok ? null : "Idle stop must be 5 to 1440 minutes");
    if (ok && value !== aws.idle_stop_minutes) void saveAws({ idle_stop_minutes: value });
  };

  return (
    <ConfigGroup
      title="Host"
      description="This project runs as an AWS Fargate task. Changes apply on its next start."
    >
      <dl className="grid grid-cols-[auto_1fr] gap-x-4 gap-y-1 text-[13px]">
        <dt className="text-[var(--text-secondary)]">Connection</dt>
        <dd className="text-[var(--text-primary)]">
          {connection?.name ?? (
            <span className="text-[var(--warning)]">
              Removed — add it again in Settings → Hosts to start this project
            </span>
          )}
        </dd>
        <dt className="text-[var(--text-secondary)]">Region</dt>
        <dd className="font-mono text-[var(--text-primary)]">{project.aws_placement?.region}</dd>
        <dt className="text-[var(--text-secondary)]">Environment</dt>
        <dd className="font-mono text-[var(--text-primary)]">
          {project.aws_placement?.environment}
        </dd>
      </dl>

      <div className="flex flex-wrap gap-4">
        <Field label="CPU">
          {(id) => (
            <select
              id={id}
              value={aws.cpu}
              disabled={disabled}
              className={selectClass}
              onChange={(e) => {
                const cpu = Number(e.target.value);
                const memory_mib = memoryError(cpu, aws.memory_mib)
                  ? nearestMemory(cpu, aws.memory_mib)
                  : aws.memory_mib;
                setMemoryProblem(null);
                void saveAws({ cpu, memory_mib });
              }}
            >
              {FARGATE_CPUS.map((c) => (
                <option key={c} value={c}>
                  {formatCpu(c)}
                </option>
              ))}
            </select>
          )}
        </Field>
        <Field
          label="Memory (MiB)"
          hint={(() => {
            const opts = memoryOptions(aws.cpu);
            return `${formatMemory(opts[0])} to ${formatMemory(opts[opts.length - 1])} with ${formatCpu(aws.cpu)}.`;
          })()}
        >
          {(id) => (
            <>
              <input
                id={id}
                inputMode="numeric"
                value={memory}
                disabled={disabled}
                list={`${id}-options`}
                aria-invalid={memoryProblem ? true : undefined}
                onChange={(e) => setMemory(e.target.value)}
                onBlur={commitMemory}
                onKeyDown={(e) => e.key === "Enter" && commitMemory()}
                className={`w-32 ${inputClass}`}
              />
              <datalist id={`${id}-options`}>
                {memoryOptions(aws.cpu).map((m) => (
                  <option key={m} value={m} />
                ))}
              </datalist>
            </>
          )}
        </Field>
        <div>
          <span className="block text-[13px] font-medium text-[var(--text-primary)] mb-1">
            Architecture
          </span>
          <SegmentedControl
            label="Architecture"
            value={aws.arch}
            disabled={disabled}
            onChange={(arch) => void saveAws({ arch })}
            segments={[
              { value: "x86_64", label: "x86_64" },
              { value: "arm64", label: "ARM64 (Graviton)" },
            ]}
          />
        </div>
      </div>
      {memoryProblem && (
        <p role="alert" className="text-xs text-[var(--error)]">
          {memoryProblem}
        </p>
      )}

      <div>
        <span className="block text-[13px] font-medium text-[var(--text-primary)] mb-1">
          Storage
        </span>
        <div className="flex flex-wrap items-center gap-3">
          <SegmentedControl
            label="Storage"
            value={aws.storage}
            disabled={disabled}
            onChange={(storage) => void saveAws({ storage })}
            segments={[
              { value: "ephemeral", label: "Ephemeral", caution: true },
              { value: "hybrid", label: "Hybrid" },
              { value: "efs", label: "EFS" },
            ]}
          />
        </div>
        {aws.storage === "ephemeral" ? (
          <p
            role="note"
            className="mt-2 px-2 py-1.5 text-xs text-[var(--warning)] bg-[var(--warning-muted)] border border-[var(--warning)]/30 rounded-[var(--radius-control)]"
          >
            {EPHEMERAL_WARNING} Push your work to a remote before stopping.
          </p>
        ) : aws.storage === "efs" ? (
          <p className="mt-1 text-xs text-[var(--text-secondary)]">
            <span>{EFS_HINT}</span>. The Claude login and history
            {aws.secrets_delivery === "copy_at_connect" ? "" : ", and SSH keys,"} are kept on EFS too.
          </p>
        ) : (
          <p className="mt-1 text-xs text-[var(--text-secondary)]">
            {aws.secrets_delivery === "copy_at_connect"
              ? "The Claude login and history are kept on EFS between starts; everything else, repositories and SSH keys included, starts fresh."
              : "The Claude login, history and SSH keys are kept on EFS between starts; everything else, repositories included, starts fresh."}
          </p>
        )}
        {keptWorkspaceFiles && (
          <p
            role="note"
            aria-label="EFS files kept"
            className="mt-2 px-2 py-1.5 text-xs text-[var(--text-secondary)] bg-[var(--bg-secondary)] border border-[var(--border-color)] rounded-[var(--radius-control)]"
          >
            The /workspace files from when this project used EFS storage stay on their access
            point until &ldquo;Remove AWS resources&rdquo; removes it (with the access points
            option checked). Switching back to EFS picks them up again.
          </p>
        )}
      </div>

      <Field
        label="Ephemeral storage (GiB)"
        hint={
          aws.storage === "efs"
            ? "The task's local disk: 21 to 200 GiB. With EFS storage, /workspace is on EFS instead."
            : "The task's local disk: 21 to 200 GiB. Repositories and builds live here."
        }
      >
        {(id) => (
          <input
            id={id}
            inputMode="numeric"
            value={ephemeral}
            disabled={disabled}
            aria-invalid={ephemeralProblem ? true : undefined}
            onChange={(e) => setEphemeral(e.target.value)}
            onBlur={commitEphemeral}
            className={`w-24 ${inputClass}`}
          />
        )}
      </Field>
      {ephemeralProblem && (
        <p role="alert" className="text-xs text-[var(--error)]">
          {ephemeralProblem}
        </p>
      )}

      <SwitchRow
        label="Spot"
        hint={
          aws.wip_push_on_stop
            ? "Runs on spare capacity at up to 70% off. AWS may reclaim it with two minutes' notice; the work-in-progress push below still runs."
            : "Runs on spare capacity at up to 70% off. AWS may reclaim it with two minutes' notice, and with the work-in-progress push below off, unpushed work is lost."
        }
        control={
          <Toggle
            label="Spot"
            checked={aws.spot}
            disabled={disabled}
            onChange={(spot) => void saveAws({ spot })}
          />
        }
      />

      <SwitchRow
        label="Stop when idle"
        hint={
          aws.wip_push_on_stop
            ? "Stops the workspace after this many minutes without terminal input or output, Claude running or a scheduled job. Unsaved git work is pushed first; if that push fails, it stays up."
            : "Stops the workspace after this many minutes without terminal input or output, Claude running or a scheduled job. With the work-in-progress push off, unsaved git work keeps it running."
        }
        control={
          <Toggle
            label="Stop when idle"
            checked={aws.idle_stop_minutes !== null}
            disabled={disabled}
            onChange={(on) =>
              void saveAws({ idle_stop_minutes: on ? Number(idle) || DEFAULT_IDLE_MINUTES : null })
            }
          />
        }
      />
      {aws.idle_stop_minutes !== null && (
        <Field label="Idle minutes" hint="5 to 1440.">
          {(id) => (
            <input
              id={id}
              inputMode="numeric"
              value={idle}
              disabled={disabled}
              aria-invalid={idleProblem ? true : undefined}
              onChange={(e) => setIdle(e.target.value)}
              onBlur={commitIdle}
              className={`w-24 ${inputClass}`}
            />
          )}
        </Field>
      )}
      {idleProblem && (
        <p role="alert" className="text-xs text-[var(--error)]">
          {idleProblem}
        </p>
      )}

      <SwitchRow
        label="Push work in progress when the workspace stops"
        hint="On any stop (idle, Spot, or from AWS), each repository's uncommitted and unpushed work is pushed to a triple-c/wip/… branch on its remote."
        control={
          <Toggle
            label="Push work in progress when the workspace stops"
            checked={aws.wip_push_on_stop}
            disabled={disabled}
            onChange={(wip_push_on_stop) => void saveAws({ wip_push_on_stop })}
          />
        }
      />

      <div>
        <span className="block text-[13px] font-medium text-[var(--text-primary)] mb-1">
          Credentials
        </span>
        <SegmentedControl
          label="Credentials"
          value={aws.secrets_delivery}
          disabled={disabled}
          onChange={(secrets_delivery) => void saveAws({ secrets_delivery })}
          segments={[
            { value: "secrets_manager", label: "Stored in AWS (Secrets Manager)" },
            { value: "copy_at_connect", label: "Copied when you connect" },
          ]}
        />
        {aws.secrets_delivery === "copy_at_connect" ? (
          <>
            <p className="mt-1 text-xs text-[var(--text-secondary)]">{COPY_AT_CONNECT_HINT}</p>
            <ul
              role="note"
              aria-label="Copied when you connect"
              className="mt-2 px-2 py-1.5 text-xs text-[var(--warning)] bg-[var(--warning-muted)] border border-[var(--warning)]/30 rounded-[var(--radius-control)] list-disc list-inside"
            >
              {COPY_AT_CONNECT_WARNINGS.map((w) => (
                <li key={w}>{w}</li>
              ))}
              {aws.storage !== "ephemeral" && <li>{COPY_AT_CONNECT_SSH_NOTE}</li>}
              {aws.gateway_sidecar && <li>{COPY_AT_CONNECT_GATEWAY_NOTE}</li>}
            </ul>
          </>
        ) : (
          <p className="mt-1 text-xs text-[var(--text-secondary)]">{SECRETS_MANAGER_HINT}</p>
        )}
      </div>

      <div className="pt-2 border-t border-[var(--border-color)]">
        <span className="block text-[13px] font-medium text-[var(--text-primary)]">
          Repositories
        </span>
        <p className="mt-0.5 mb-2 text-xs text-[var(--text-secondary)] leading-snug">
          Cloned into <span className="font-mono">/workspace/&lt;folder&gt;</span> when the
          workspace starts, with the project&rsquo;s SSH key or Git token.
        </p>
        <RepoListEditor
          repos={aws.repos}
          disabled={disabled}
          onSave={(repos: RepoSource[]) => saveAws({ repos })}
        />
      </div>

      <div className="pt-2 border-t border-[var(--border-color)] flex flex-wrap items-center gap-2">
        <Button
          size="md"
          variant="danger"
          unavailable={!stopped || !hasResources}
          unavailableReason={
            !stopped
              ? STILL_RUNNING
              : "This project has no AWS resources to remove."
          }
          onClick={() => setConfirmRemoveResources(true)}
        >
          Remove AWS resources…
        </Button>
        <Button
          size="md"
          unavailable={!stopped || hasResources}
          unavailableReason={
            !stopped
              ? STILL_RUNNING
              : onlyAccessPoint
                ? "The saved files' access point is still on AWS. Remove the AWS resources with \u201cAlso remove the workspace\u2019s file-system access points\u201d first."
                : "Remove the workspace's AWS resources first, so nothing is left running or billed."
          }
          onClick={() => void save({ host_id: null, aws_placement: null, aws: null })}
        >
          Run on this machine instead
        </Button>
      </div>

      {confirmRemoveResources && (
        <RemoveAwsResourcesModal
          project={project}
          onClose={() => setConfirmRemoveResources(false)}
        />
      )}
    </ConfigGroup>
  );
}

function RemoveAwsResourcesModal({ project, onClose }: { project: Project; onClose: () => void }) {
  const [discardData, setDiscardData] = useState(false);
  const [running, setRunning] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const confirm = async () => {
    if (running) return;
    setRunning(true);
    setError(null);
    try {
      const updated = await commands.removeAwsResources(project.id, hasSavedFiles(project) && discardData);
      useAppState.getState().updateProjectInList(updated);
      onClose();
    } catch (e) {
      setError(String(e));
    } finally {
      setRunning(false);
    }
  };

  return (
    <Modal
      title="Remove AWS resources"
      onClose={onClose}
      widthClassName="w-[28rem]"
      footer={
        <>
          <Button size="md" variant="ghost" onClick={onClose}>
            Cancel
          </Button>
          <Button
            size="md"
            variant="danger"
            unavailable={running}
            unavailableReason="Removing…"
            onClick={() => void confirm()}
          >
            {running ? "Removing…" : "Remove"}
          </Button>
        </>
      }
    >
      <p className="text-[13px] text-[var(--text-secondary)]">
        Removes <strong className="text-[var(--text-primary)]">{project.name}</strong>&rsquo;s
        secret from AWS and keeps the project, so it can run somewhere else. Its task
        definitions stay registered for an administrator to deregister. Its next start on AWS
        sets everything up again.
      </p>
      {hasSavedFiles(project) && (
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
              {hasWorkspaceFiles(project)
                ? "Both go: the one for the saved home files and the one for /workspace. "
                : ""}
              The files stay on your own EFS file system (only your workspace role can read
              them) until an administrator offboards you. Left unchecked, the access point
              stays too: the next start here reuses it, and the project can&rsquo;t move to
              another host until it is removed.
            </span>
          </span>
        </label>
      )}
      {error && (
        <p
          role="alert"
          className="mt-3 px-2 py-1.5 text-xs text-[var(--error)] bg-[var(--error-muted)] border border-[var(--error)]/30 rounded-[var(--radius-control)]"
        >
          {error}
        </p>
      )}
    </Modal>
  );
}
