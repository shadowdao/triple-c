import { useEffect, useRef, useState } from "react";
import type {
  AwsWorkspaceConfig,
  Ec2Capacity,
  HostConnection,
  LaunchType,
  Project,
  RepoSource,
} from "../../../../lib/types";
import * as commands from "../../../../lib/tauri-commands";
import { environmentMaxHours } from "../../../../lib/environmentMaxHours";
import { useEc2Capacity } from "../../../../lib/ec2Capacity";
import { useAppState } from "../../../../store/appState";
import {
  COPY_AT_CONNECT_GATEWAY_NOTE,
  GATEWAY_SIDECAR_MIN_CPU,
  COPY_AT_CONNECT_HINT,
  COPY_AT_CONNECT_SSH_NOTE,
  COPY_AT_CONNECT_WARNINGS,
  SECRETS_MANAGER_HINT,
  SETUP_SCRIPT_COPY_AT_CONNECT_NOTE,
  SETUP_SCRIPT_HINT,
  MAX_SETUP_SCRIPT_BYTES,
  setupScriptProblem,
  EFS_HINT,
  EPHEMERAL_WARNING,
  EC2_DOCKER_ACCESS_NOTE,
  EC2_NO_CAPACITY,
  EC2_NO_SIDECAR,
  EC2_SCALE_TO_ZERO_NOTE,
  launchTypePatch,
  type Ec2Limits,
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
import Field, { ConfigGroup, SwitchRow, inputClass, monoInputClass, selectClass } from "../../../ui/Field";
import RepoListEditor from "../../RepoListEditor";
import AwsCostEstimate from "../AwsCostEstimate";

interface Props {
  project: Project;
  save: (patch: Partial<Project>) => Promise<boolean>;
  disabled: boolean;
}

const DEFAULT_IDLE_MINUTES = 60;
const STILL_RUNNING = "The task is still running — stop it first.";

/** Why EC2 can't be chosen, from what the user's record says (null: it can). */
function ec2Unavailable(capacity: Ec2Capacity | null): string | null {
  if (capacity === null) return "Checking your EC2 capacity in this environment…";
  switch (capacity.state) {
    case "ec2":
      return null;
    case "none":
      return `You have no EC2 capacity in this environment. ${EC2_NO_CAPACITY}.`;
    case "unusable":
      return `Your EC2 capacity record can't be used (${capacity.reason}). ${EC2_NO_CAPACITY}.`;
    case "unknown":
      return "Could not read your EC2 capacity in this environment; EC2 is offered once it can be read.";
  }
}

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

  const [cpuText, setCpuText] = useState(String(aws.cpu));
  const [memory, setMemory] = useState(String(aws.memory_mib));
  const [memoryProblem, setMemoryProblem] = useState<string | null>(null);
  const [ephemeral, setEphemeral] = useState(String(aws.ephemeral_gib));
  const [ephemeralProblem, setEphemeralProblem] = useState<string | null>(null);
  const [idle, setIdle] = useState(String(aws.idle_stop_minutes ?? DEFAULT_IDLE_MINUTES));
  const [idleProblem, setIdleProblem] = useState<string | null>(null);
  const [confirmRemoveResources, setConfirmRemoveResources] = useState(false);
  const [setupScript, setSetupScript] = useState(aws.setup_script ?? "");
  const [setupProblem, setSetupProblem] = useState<string | null>(null);
  // The environment's advisory lifetime cap (Q8), from its record.
  const [maxHours, setMaxHours] = useState<number | null>(null);

  useEffect(() => {
    setCpuText(String(aws.cpu));
    setMemory(String(aws.memory_mib));
    setEphemeral(String(aws.ephemeral_gib));
    setIdle(String(aws.idle_stop_minutes ?? DEFAULT_IDLE_MINUTES));
  }, [aws.cpu, aws.memory_mib, aws.ephemeral_gib, aws.idle_stop_minutes]);

  // Phase 7: the user's own EC2 capacity in this environment (their record),
  // which decides whether EC2 can be chosen and what it can reserve.
  const capacity = useEc2Capacity(project);
  const ec2 = aws.launch_type === "ec2";
  const compute = capacity?.state === "ec2" ? capacity.compute : null;
  const limits: Ec2Limits | null =
    capacity?.state === "ec2" && capacity.available_cpu !== null && capacity.available_memory_mib !== null
      ? { cpu: capacity.available_cpu, memory_mib: capacity.available_memory_mib }
      : null;
  const ec2Reason = ec2Unavailable(capacity);

  useEffect(() => {
    setSetupScript(aws.setup_script ?? "");
  }, [aws.setup_script]);

  const region = project.aws_placement?.region;
  const environment = project.aws_placement?.environment;
  useEffect(() => {
    setMaxHours(null);
    if (!project.host_id || !region || !environment) return;
    let cancelled = false;
    // Best effort: an unreadable record just shows no cap line. Cached per
    // environment, so re-opening this section doesn't ask AWS again.
    environmentMaxHours(project.id, project.host_id, region, environment)
      .then((h) => {
        if (!cancelled) setMaxHours(h);
      })
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  }, [project.id, project.host_id, region, environment]);

  // The config the next save starts from: the stored one plus every patch
  // saved since. A blur's save followed at once by a toggle's (before the
  // store has the first back) must not spread the render-time `aws` and
  // revert the first. Re-based on the prop when it changes with no save in
  // flight.
  const latest = useRef({ base: project.aws, value: aws, inFlight: 0 });
  if (latest.current.base !== project.aws && latest.current.inFlight === 0) {
    latest.current = { base: project.aws, value: aws, inFlight: 0 };
  }
  const saveAws = async (patch: Partial<AwsWorkspaceConfig>, also: Partial<Project> = {}) => {
    const l = latest.current;
    const next = { ...l.value, ...patch };
    l.value = next;
    l.inFlight += 1;
    // A refused save is not the config any more: start again from the store.
    const refused = () => {
      if (l.value === next) l.value = l.base ?? defaultAwsWorkspace();
    };
    try {
      const ok = await save({ ...also, aws: next });
      if (!ok) refused();
      return ok;
    } catch (e) {
      refused();
      throw e;
    } finally {
      l.inFlight -= 1;
    }
  };

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

  // The launch-type switch: `launchTypePatch` keeps each side to what Rust
  // accepts there (Docker/VPN, the sidecar, Spot, the sizes).
  const switchLaunchType = (launch_type: LaunchType) => {
    if (launch_type === latest.current.value.launch_type) return;
    setMemoryProblem(null);
    const { aws: next, ...also } = launchTypePatch(
      { ...project, aws: latest.current.value },
      launch_type,
      limits,
    );
    void saveAws(next!, also);
  };

  // EC2 reservations: positive whole numbers that fit the user's instance
  // type — checked by Rust's own fit rule (`ec2_fit_problem`), so this never
  // disagrees with the start.
  const commitEc2Sizing = async () => {
    const cpu = Number(cpuText);
    const memory_mib = Number(memory);
    const whole = (n: number) => Number.isInteger(n) && n > 0;
    if (!whole(cpu) || !whole(memory_mib)) {
      setMemoryProblem("The CPU and memory reservations must be positive whole numbers");
      return;
    }
    if (compute) {
      let problem: string | null = null;
      try {
        problem = await commands.ec2FitProblem({ ...aws, cpu, memory_mib }, compute.instance_type);
      } catch {
        problem = null; // The start checks again.
      }
      if (problem) {
        setMemoryProblem(problem);
        return;
      }
    }
    setMemoryProblem(null);
    if (cpu !== aws.cpu || memory_mib !== aws.memory_mib) void saveAws({ cpu, memory_mib });
  };

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

  const commitSetupScript = () => {
    const problem = setupScriptProblem(setupScript);
    setSetupProblem(problem);
    if (problem) return;
    const value = setupScript.trim() === "" ? null : setupScript;
    if (value !== (aws.setup_script ?? null)) void saveAws({ setup_script: value });
  };

  const setupBytes = new TextEncoder().encode(setupScript).length;

  const commitIdle = () => {
    const value = Number(idle);
    const ok = Number.isInteger(value) && value >= 5 && value <= 1440;
    setIdleProblem(ok ? null : "Idle stop must be 5 to 1440 minutes");
    if (ok && value !== aws.idle_stop_minutes) void saveAws({ idle_stop_minutes: value });
  };

  return (
    <ConfigGroup
      title="Host"
      description={
        ec2
          ? "This project runs on your own EC2 instances, as an ECS task. Changes apply on its next start."
          : "This project runs as an AWS Fargate task. Changes apply on its next start."
      }
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
      {maxHours !== null && maxHours > 0 && (
        <p className="text-xs text-[var(--text-secondary)]">
          This environment stops workspaces after {maxHours} h (advisory).
        </p>
      )}

      <div>
        <span className="block text-[13px] font-medium text-[var(--text-primary)] mb-1">
          Launch type
        </span>
        <SegmentedControl
          label="Launch type"
          value={aws.launch_type}
          disabled={disabled}
          onChange={switchLaunchType}
          segments={[
            { value: "fargate", label: "Fargate" },
            {
              value: "ec2",
              label: compute ? `EC2 (your instance: ${compute.instance_type})` : "EC2",
              disabled: !compute && !ec2,
            },
          ]}
        />
        {ec2Reason && (
          <p
            className={`mt-1 text-xs ${ec2 && capacity !== null && capacity.state !== "unknown" ? "text-[var(--warning)]" : "text-[var(--text-secondary)]"}`}
          >
            {ec2 && capacity !== null && capacity.state !== "unknown"
              ? `This project can't start on EC2 now: ${ec2Reason} Or switch it to Fargate.`
              : ec2Reason}
          </p>
        )}
        {ec2 && (
          <ul
            role="note"
            aria-label="On EC2"
            className="mt-2 px-2 py-1.5 text-xs text-[var(--text-secondary)] bg-[var(--bg-secondary)] border border-[var(--border-color)] rounded-[var(--radius-control)] list-disc list-inside space-y-0.5"
          >
            <li>{EC2_SCALE_TO_ZERO_NOTE}</li>
            <li>{EC2_DOCKER_ACCESS_NOTE}</li>
            <li>{EC2_NO_SIDECAR}.</li>
          </ul>
        )}
      </div>

      {ec2 ? (
        <div className="flex flex-wrap gap-4">
          <Field
            label="CPU reservation (units)"
            hint={
              limits && compute
                ? `1024 units = 1 vCPU. Up to ${limits.cpu} on your ${compute.instance_type}.`
                : "1024 units = 1 vCPU. It must fit your instance type."
            }
          >
            {(id) => (
              <input
                id={id}
                inputMode="numeric"
                value={cpuText}
                disabled={disabled}
                aria-invalid={memoryProblem ? true : undefined}
                onChange={(e) => setCpuText(e.target.value)}
                onBlur={() => void commitEc2Sizing()}
                onKeyDown={(e) => e.key === "Enter" && void commitEc2Sizing()}
                className={`w-32 ${inputClass}`}
              />
            )}
          </Field>
          <Field
            label="Memory reservation (MiB)"
            hint={
              limits && compute
                ? `Up to ${limits.memory_mib} MiB on your ${compute.instance_type} (a tenth is kept for the instance).`
                : "It must fit your instance type."
            }
          >
            {(id) => (
              <input
                id={id}
                inputMode="numeric"
                value={memory}
                disabled={disabled}
                aria-invalid={memoryProblem ? true : undefined}
                onChange={(e) => setMemory(e.target.value)}
                onBlur={() => void commitEc2Sizing()}
                onKeyDown={(e) => e.key === "Enter" && void commitEc2Sizing()}
                className={`w-32 ${inputClass}`}
              />
            )}
          </Field>
        </div>
      ) : (
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
              {FARGATE_CPUS.filter(
                // The gateway sidecar needs 1 vCPU / 2 GiB (Rust validates it too).
                (c) => !aws.gateway_sidecar || c >= GATEWAY_SIDECAR_MIN_CPU,
              ).map((c) => (
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
      )}
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

      {!ec2 && (
      <>
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
      </>
      )}

      <section aria-label="Estimated cost">
        <h3 className="text-[13px] font-medium text-[var(--text-primary)]">Estimated cost</h3>
        <p className="mt-0.5 mb-1 text-xs text-[var(--text-secondary)] leading-snug">
          {ec2
            ? `The on-demand price in this region of your EC2 instance${compute ? ` (${compute.instance_type})` : ""}, whatever this project reserves of it.`
            : "The on-demand Fargate price in this region for the CPU, memory, architecture and disk above."}
        </p>
        <AwsCostEstimate
          connectionId={connection ? connection.id : null}
          region={project.aws_placement?.region}
          config={aws}
          instanceType={compute?.instance_type ?? null}
        />
      </section>

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

      <div className="pt-2 border-t border-[var(--border-color)]">
        <Field
          label="Setup script"
          hint={
            <>
              {SETUP_SCRIPT_HINT}
              {aws.secrets_delivery === "copy_at_connect" && <> {SETUP_SCRIPT_COPY_AT_CONNECT_NOTE}</>}
            </>
          }
        >
          {(id) => (
            <>
              <textarea
                id={id}
                value={setupScript}
                disabled={disabled}
                rows={8}
                spellCheck={false}
                placeholder={"sudo apt-get update\nsudo apt-get install -y jq"}
                aria-invalid={setupProblem ? true : undefined}
                aria-describedby={`${id}-size`}
                onChange={(e) => setSetupScript(e.target.value)}
                onBlur={commitSetupScript}
                className={`${monoInputClass} min-h-[8rem] resize-y`}
              />
              <p
                id={`${id}-size`}
                className={`mt-1 text-xs ${setupBytes > MAX_SETUP_SCRIPT_BYTES ? "text-[var(--error)]" : "text-[var(--text-secondary)]"}`}
              >
                {setupBytes.toLocaleString("en-US")} of {MAX_SETUP_SCRIPT_BYTES.toLocaleString("en-US")} bytes
                (32 KiB, UTF-8)
              </p>
            </>
          )}
        </Field>
        {setupProblem && (
          <p role="alert" className="mt-1 text-xs text-[var(--error)]">
            {setupProblem}
          </p>
        )}
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
