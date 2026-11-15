import { useCallback, useEffect, useRef, useState, type ReactNode } from "react";
import {
  deployAwsEnvironment,
  destroyIncompleteAwsEnvironment,
  detectAwsEnvironments,
  followAwsDeployment,
  onboardAwsUser,
  repairAwsEnvironment,
  unlockAwsState,
  updateAwsEnvironment,
} from "../../../lib/tauri-commands";
import { awsErrorMessage, toAwsError } from "../../../lib/aws-error";
import type {
  AwsError,
  DetectedEnvironment,
  EnvironmentScan,
  HostConnection,
  ModuleKind,
  NetworkChoice,
  PermissionSummary,
  PolicyFile,
} from "../../../lib/types";
import Button from "../../ui/Button";
import Field, { inputClass, selectClass } from "../../ui/Field";
import Modal from "../../ui/Modal";
import StatusIndicator from "../../ui/StatusIndicator";
import AdminBundleModal from "./AdminBundleModal";
import DeployEnvironmentModal from "./DeployEnvironmentModal";
import DeployProgress, { useDeployRuns, type DeployRun } from "./DeployProgress";
import PolicyFiles from "./PolicyFiles";
import {
  COMMON_REGIONS,
  environmentStateKey,
  formatAge,
  lockLine,
  onboardingStateKey,
  stateLock,
  stateStatus,
} from "./environmentFormat";

/** A typed confirmation: the action runs only once the environment's name is typed. */
function TypedConfirmModal({
  title,
  name,
  confirmLabel,
  children,
  onConfirm,
  onClose,
}: {
  title: string;
  name: string;
  confirmLabel: string;
  children: ReactNode;
  /** Resolves to the command's error, or null when done. */
  onConfirm: () => Promise<AwsError | null>;
  onClose: () => void;
}) {
  const [typed, setTyped] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<AwsError | null>(null);
  const confirm = async () => {
    setBusy(true);
    setError(null);
    const err = await onConfirm();
    setBusy(false);
    if (err) setError(err);
    else onClose();
  };
  return (
    <Modal
      title={title}
      onClose={onClose}
      footer={
        <>
          <Button size="md" onClick={onClose}>
            Cancel
          </Button>
          <Button size="md" variant="danger" onClick={confirm} disabled={typed !== name || busy}>
            {confirmLabel}
          </Button>
        </>
      }
    >
      <div className="space-y-3 text-xs text-[var(--text-secondary)] leading-snug">
        {children}
        <Field label={`Type ${name} to confirm`}>
          {(id) => (
            <input id={id} className={inputClass} autoComplete="off" value={typed} onChange={(e) => setTyped(e.target.value)} />
          )}
        </Field>
        {error && (
          <p role="alert" className="text-[var(--error)]">
            {awsErrorMessage(error)}
          </p>
        )}
      </div>
    </Modal>
  );
}

type Bundle = { kind: ModuleKind; environment?: string; ownerId?: string };

/** The environment a run's state key belongs to, if any. */
function runEnvironment(stateKey: string): string | null {
  const m = /^(?:environments|users)\/([^/]+)\//.exec(stateKey);
  return m ? m[1] : null;
}

/**
 * The environments a connection can reach in a region (spec §2): detection,
 * each environment's state and actions, and deploy/join for this caller's tier.
 */
export default function EnvironmentsPanel({
  connection,
  tier,
}: {
  connection: HostConnection;
  /** The connection's last permission report; absent means unknown, treated as not an admin. */
  tier?: PermissionSummary;
}) {
  const isAdmin = tier === "environment_admin";
  const [open, setOpen] = useState(false);
  const [region, setRegion] = useState(connection.kind.default_region);
  const [scan, setScan] = useState<EnvironmentScan | null>(null);
  const [detecting, setDetecting] = useState(false);
  const [detectError, setDetectError] = useState<AwsError | null>(null);
  const [rowErrors, setRowErrors] = useState<Record<string, AwsError>>({});
  const [pending, setPending] = useState<Record<string, boolean>>({});
  const [waiting, setWaiting] = useState<Set<string>>(new Set());
  const [deployOpen, setDeployOpen] = useState(false);
  const [repairOf, setRepairOf] = useState<string | null>(null);
  const [destroyOf, setDestroyOf] = useState<string | null>(null);
  const [unlockOf, setUnlockOf] = useState<DetectedEnvironment | null>(null);
  const [bundle, setBundle] = useState<Bundle | null>(null);
  const [copyNote, setCopyNote] = useState<string | null>(null);
  const request = useRef(0);
  const followed = useRef(new Set<string>());

  const detect = useCallback(async () => {
    const id = ++request.current;
    setDetecting(true);
    setDetectError(null);
    try {
      const found = await detectAwsEnvironments(connection.id, region);
      if (id === request.current) setScan(found);
    } catch (e) {
      if (id === request.current) {
        setScan(null);
        setDetectError(toAwsError(e));
      }
    } finally {
      if (id === request.current) setDetecting(false);
    }
  }, [connection.id, region]);

  // A finished run (its result line shown) re-detects.
  const detectRef = useRef(detect);
  detectRef.current = detect;
  const { runs, start, dismiss } = useDeployRuns(() => void detectRef.current());

  useEffect(() => {
    if (open) void detect();
  }, [open, detect]);

  const launch = useCallback(
    async (envName: string, stateKey: string, label: string, run: () => Promise<string>) => {
      setRowErrors(({ [envName]: _gone, ...rest }) => rest);
      setPending((p) => ({ ...p, [envName]: true }));
      const err = await start(stateKey, region, label, run);
      setPending(({ [envName]: _gone, ...rest }) => rest);
      if (err) setRowErrors((r) => ({ ...r, [envName]: err }));
      return err;
    },
    [start, region],
  );

  // Reattach to a deploy running on this machine (a re-follow replays its whole log).
  useEffect(() => {
    if (!scan) return;
    for (const e of scan.environments) {
      const key = environmentStateKey(e.name);
      const seen = `${scan.region}:${key}`;
      if (e.actions.includes("follow") && !runs[key] && !followed.current.has(seen)) {
        followed.current.add(seen);
        void launch(e.name, key, `Deploy of ${e.name}`, () => followAwsDeployment(connection.id, scan.region, key));
      }
    }
  }, [scan, runs, launch, connection.id]);

  const regions = COMMON_REGIONS.includes(connection.kind.default_region)
    ? COMMON_REGIONS
    : [connection.kind.default_region, ...COMMON_REGIONS];

  const copy = async (file: PolicyFile) => {
    try {
      await navigator.clipboard.writeText(file.json);
      setCopyNote(`${file.name} copied to clipboard.`);
    } catch (e) {
      setCopyNote(`Could not copy ${file.name}: ${awsErrorMessage(toAwsError(e))}`);
    }
  };

  const deploy = (name: string, network: NetworkChoice) =>
    launch(name, environmentStateKey(name), `Deploy of ${name}`, () =>
      deployAwsEnvironment(connection.id, region, name, network),
    );
  const repair = (name: string, network?: NetworkChoice) =>
    launch(name, environmentStateKey(name), `Repair of ${name}`, () =>
      repairAwsEnvironment(connection.id, region, name, network),
    );

  const join = (e: DetectedEnvironment) => {
    if (!scan) return;
    if (isAdmin) {
      void launch(e.name, onboardingStateKey(e.name, scan.owner_key), `Joining ${e.name}`, () =>
        onboardAwsUser(connection.id, region, e.name),
      );
    } else {
      setWaiting((w) => new Set(w).add(e.name));
      setBundle({ kind: "user_onboard", environment: e.name, ownerId: scan.owner_id });
    }
  };

  const rowRuns = (name: string) =>
    Object.values(runs).filter((r) => r.region === region && runEnvironment(r.stateKey) === name);
  const shown = new Set(scan?.environments.map((e) => e.name) ?? []);
  const looseRuns = Object.values(runs).filter(
    (r) => r.region !== region || !shown.has(runEnvironment(r.stateKey) ?? ""),
  );
  const progress = (r: DeployRun) => (
    <DeployProgress
      key={`${r.region}:${r.stateKey}`}
      run={r}
      connectionId={connection.id}
      onDismiss={() => dismiss(r.stateKey)}
    />
  );

  const envRow = (e: DetectedEnvironment) => {
    const status = stateStatus(e.state);
    const lock = stateLock(e.state);
    const has = (a: DetectedEnvironment["actions"][number]) => e.actions.includes(a);
    const onboarding = e.onboarding?.state;
    const busy = !!pending[e.name];
    const rowError = rowErrors[e.name];
    return (
      <li
        key={e.name}
        data-testid={`env-${e.name}`}
        className="border border-[var(--border-color)] rounded-[var(--radius-control)] p-2 space-y-1.5"
      >
        <div className="flex items-start justify-between gap-2 flex-wrap">
          <div className="min-w-0 space-y-0.5 text-xs">
            <div className="flex items-center gap-2 flex-wrap">
              <span className="text-[13px] font-medium text-[var(--text-primary)]">{e.name}</span>
              <StatusIndicator tone={status.tone} label={status.label} />
              {onboarding === "joined" && <StatusIndicator tone="ok" label="Joined" />}
              {onboarding === "in_progress" && <StatusIndicator tone="busy" label="Onboarding in progress" />}
              {onboarding === "incomplete" && <StatusIndicator tone="error" label="Onboarding incomplete" />}
            </div>
            <div className="text-[var(--text-secondary)]">
              {e.template_version !== null ? `Module version ${e.template_version}` : "Module version unknown"}
              {e.info && ` · ${e.info.network_mode} network`}
            </div>
            {lock && (
              <div className="text-[var(--text-secondary)]">
                {lockLine(lock)}
                {lock.local_process && " · running on this machine"}
              </div>
            )}
            {e.state.state === "unreadable_record" && (
              <div className="text-[var(--error)]">The environment's record can't be used: {e.state.reason}</div>
            )}
            {!isAdmin && waiting.has(e.name) && onboarding !== "joined" && (
              <div className="text-[var(--text-secondary)]">Waiting for an admin to onboard you</div>
            )}
          </div>
          <div className="flex gap-1.5 flex-wrap">
            {has("join") && onboarding !== "joined" && onboarding !== "in_progress" && (
              <Button size="sm" variant="primary" disabled={busy} aria-label={`Join ${e.name}`} onClick={() => join(e)}>
                {!isAdmin && waiting.has(e.name) ? "Show onboarding bundle" : "Join"}
              </Button>
            )}
            {has("follow") && (
              <Button
                size="sm"
                disabled={busy}
                aria-label={`Follow ${e.name}`}
                onClick={() => {
                  const key = environmentStateKey(e.name);
                  void launch(e.name, key, `Deploy of ${e.name}`, () => followAwsDeployment(connection.id, region, key));
                }}
              >
                Follow
              </Button>
            )}
            {has("update") && (
              <Button
                size="sm"
                disabled={busy}
                aria-label={`Update ${e.name}`}
                onClick={() =>
                  void launch(e.name, environmentStateKey(e.name), `Update of ${e.name}`, () =>
                    updateAwsEnvironment(connection.id, region, e.name),
                  )
                }
              >
                Update
              </Button>
            )}
            {has("repair") && (
              <Button
                size="sm"
                disabled={busy}
                aria-label={`Repair ${e.name}`}
                onClick={() => (e.info ? void repair(e.name) : setRepairOf(e.name))}
              >
                Repair
              </Button>
            )}
            {has("unlock") && (
              <Button size="sm" variant="danger" disabled={busy} aria-label={`Unlock ${e.name}`} onClick={() => setUnlockOf(e)}>
                Unlock
              </Button>
            )}
            {has("destroy") && (
              <Button size="sm" variant="danger" disabled={busy} aria-label={`Destroy ${e.name}`} onClick={() => setDestroyOf(e.name)}>
                Destroy
              </Button>
            )}
          </div>
        </div>
        {rowError && (
          <p role="alert" className="text-xs text-[var(--error)] leading-snug">
            {awsErrorMessage(rowError)}
          </p>
        )}
        {rowRuns(e.name).map(progress)}
      </li>
    );
  };

  const nothingFound = !!scan && scan.environments.length === 0;

  return (
    <div className="pt-1.5 space-y-2">
      <Button size="sm" variant="ghost" aria-expanded={open} onClick={() => setOpen((o) => !o)}>
        <span aria-hidden="true">{open ? "▾" : "▸"}</span>
        Environments
      </Button>
      {open && (
        <div className="space-y-2 pl-1">
          <div className="flex items-end gap-2 flex-wrap">
            <Field label="Region">
              {(id) => (
                <select id={id} className={selectClass} value={region} onChange={(e) => setRegion(e.target.value)}>
                  {regions.map((r) => (
                    <option key={r} value={r}>
                      {r}
                    </option>
                  ))}
                </select>
              )}
            </Field>
            <Button size="md" onClick={() => void detect()} disabled={detecting}>
              Detect again
            </Button>
          </div>
          {detecting && (
            <p role="status" className="text-xs text-[var(--text-secondary)]">
              Detecting…
            </p>
          )}
          {detectError && (
            <p role="alert" className="text-xs text-[var(--error)] leading-snug">
              {awsErrorMessage(detectError)}
            </p>
          )}
          {scan && isAdmin && scan.boundary === "no" && (
            <p className="text-xs text-[var(--text-secondary)] leading-snug">
              This account has no Triple-C permissions boundary yet; deploying creates it first.
            </p>
          )}
          {scan && scan.environments.length > 0 && <ul className="space-y-1.5">{scan.environments.map(envRow)}</ul>}
          {nothingFound && (
            <p className="text-xs text-[var(--text-secondary)] leading-snug">
              No Triple-C environment in {scan.region}.
              {!isAdmin &&
                " Deploying one needs an environment admin: attach the environment-admin policies to an identity, or send an admin a bundle to deploy."}
              {!tier && " Run Check to find out whether you can deploy one yourself."}
            </p>
          )}
          {scan && isAdmin && (
            <Button size="md" variant={nothingFound ? "primary" : "secondary"} onClick={() => setDeployOpen(true)}>
              Deploy a new environment
            </Button>
          )}
          {nothingFound && !isAdmin && (
            <div className="space-y-2">
              <PolicyFiles tier="environment_admin" label="Show the environment-admin policies" onCopy={copy} />
              <Button size="md" onClick={() => setBundle({ kind: "environment" })}>
                Request an environment from an admin…
              </Button>
              {copyNote && (
                <p role="status" className="text-xs text-[var(--text-secondary)]">
                  {copyNote}
                </p>
              )}
            </div>
          )}
          {looseRuns.map(progress)}
        </div>
      )}
      {scan && (deployOpen || repairOf) && (
        <DeployEnvironmentModal
          connectionId={connection.id}
          region={region}
          scan={scan}
          repair={repairOf ?? undefined}
          onClose={() => {
            setDeployOpen(false);
            setRepairOf(null);
          }}
          onDeploy={repairOf ? (name, network) => repair(name, network) : deploy}
          onDetect={() => void detect()}
        />
      )}
      {destroyOf && (
        <TypedConfirmModal
          title={`Destroy ${destroyOf}`}
          name={destroyOf}
          confirmLabel="Destroy"
          onClose={() => setDestroyOf(null)}
          onConfirm={() =>
            launch(destroyOf, environmentStateKey(destroyOf), `Destroy of ${destroyOf}`, () =>
              destroyIncompleteAwsEnvironment(connection.id, region, destroyOf),
            )
          }
        >
          <p>
            {destroyOf} never finished deploying, so OpenTofu removes everything it created for it. This can't be
            undone.
          </p>
        </TypedConfirmModal>
      )}
      {unlockOf && (
        <TypedConfirmModal
          title={`Unlock ${unlockOf.name}`}
          name={unlockOf.name}
          confirmLabel="Unlock"
          onClose={() => setUnlockOf(null)}
          onConfirm={async () => {
            try {
              await unlockAwsState(connection.id, region, environmentStateKey(unlockOf.name));
            } catch (e) {
              return toAwsError(e);
            }
            void detect();
            return null;
          }}
        >
          {(() => {
            const lock = stateLock(unlockOf.state);
            return lock ? (
              <p>
                Held by <span className="font-mono text-[var(--text-primary)]">{lock.who || "unknown"}</span> since{" "}
                {formatAge(lock.age_secs)} ago ({lock.created}
                {lock.operation && `, ${lock.operation}`}).
              </p>
            ) : null;
          })()}
          <p className="text-[var(--warning)]">
            Unlock only if that deployment has stopped. Removing the lock of a run that is still going lets two runs
            write the state at once.
          </p>
        </TypedConfirmModal>
      )}
      {bundle && (
        <AdminBundleModal
          connectionId={connection.id}
          region={region}
          kind={bundle.kind}
          environment={bundle.environment}
          ownerId={bundle.ownerId}
          onClose={() => setBundle(null)}
        />
      )}
    </div>
  );
}
