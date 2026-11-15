import { useCallback, useEffect, useRef, useState, type ReactNode } from "react";
import {
  deployAwsEnvironment,
  destroyIncompleteAwsEnvironment,
  detectAwsEnvironments,
  followAwsDeployment,
  onboardAwsUser,
  pushAwsSavedState,
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
  LockInfo,
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
import DeployProgress, { runKey, useDeployRuns, type DeployRun } from "./DeployProgress";
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

type Bundle = { kind: ModuleKind; region: string; environment?: string; ownerId?: string };
/** A row-level key: the same environment name can exist in every region. */
const rowKey = (region: string, name: string) => `${region}:${name}`;

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
  const [destroyOf, setDestroyOf] = useState<{ name: string; region: string } | null>(null);
  /** An Unlock waiting for its typed confirmation: an environment's lock or the caller's onboarding's. */
  const [unlockOf, setUnlockOf] = useState<{
    name: string;
    region: string;
    stateKey: string;
    lock: LockInfo | null;
    onboarding: boolean;
  } | null>(null);
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

  /** Start a run in `runRegion` (always the region of the scan the action came from). */
  const launch = useCallback(
    async (runRegion: string, envName: string, stateKey: string, label: string, run: () => Promise<string>) => {
      const rk = rowKey(runRegion, envName);
      setRowErrors(({ [rk]: _gone, ...rest }) => rest);
      setPending((p) => ({ ...p, [rk]: true }));
      const err = await start(stateKey, runRegion, label, run);
      setPending(({ [rk]: _gone, ...rest }) => rest);
      if (err) setRowErrors((r) => ({ ...r, [rk]: err }));
      return err;
    },
    [start],
  );

  // Reattach to a deploy (or the caller's onboarding) running on this machine;
  // a re-follow replays its whole log.
  useEffect(() => {
    if (!scan) return;
    const reattach = (name: string, key: string, label: string) => {
      const seen = runKey(scan.region, key);
      if (!runs[seen] && !followed.current.has(seen)) {
        followed.current.add(seen);
        void launch(scan.region, name, key, label, () => followAwsDeployment(connection.id, scan.region, key));
      }
    };
    for (const e of scan.environments) {
      if (e.actions.includes("follow")) reattach(e.name, environmentStateKey(e.name), `Deploy of ${e.name}`);
      if (e.onboarding_actions.includes("follow")) {
        reattach(e.name, onboardingStateKey(e.name, scan.owner_key), `Joining ${e.name}`);
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

  // Rows (and every action on them) belong to the scan they came from. After a
  // region change the old scan's rows are hidden, and while any detection is in
  // flight their actions are disabled; actions always target `scan.region`.
  const current = scan && scan.region === region ? scan : null;
  const stale = detecting || !current;

  const deploy = (runRegion: string, name: string, network: NetworkChoice) =>
    launch(runRegion, name, environmentStateKey(name), `Deploy of ${name}`, () =>
      deployAwsEnvironment(connection.id, runRegion, name, network),
    );
  const repair = (runRegion: string, name: string, network?: NetworkChoice) =>
    launch(runRegion, name, environmentStateKey(name), `Repair of ${name}`, () =>
      repairAwsEnvironment(connection.id, runRegion, name, network),
    );

  const join = (found: EnvironmentScan, e: DetectedEnvironment) => {
    if (isAdmin) {
      void launch(found.region, e.name, onboardingStateKey(e.name, found.owner_key), `Joining ${e.name}`, () =>
        onboardAwsUser(connection.id, found.region, e.name),
      );
    } else {
      setBundle({ kind: "user_onboard", region: found.region, environment: e.name, ownerId: found.owner_id });
    }
  };

  const rowRuns = (name: string) =>
    Object.values(runs).filter((r) => r.region === current?.region && runEnvironment(r.stateKey) === name);
  const shown = new Set(current?.environments.map((e) => e.name) ?? []);
  const looseRuns = Object.values(runs).filter(
    (r) => r.region !== current?.region || !shown.has(runEnvironment(r.stateKey) ?? ""),
  );
  const progress = (r: DeployRun) => (
    <DeployProgress
      key={runKey(r.region, r.stateKey)}
      run={r}
      connectionId={connection.id}
      onDismiss={() => dismiss(runKey(r.region, r.stateKey))}
    />
  );

  const envRow = (found: EnvironmentScan, e: DetectedEnvironment) => {
    const at = found.region;
    const status = stateStatus(e.state);
    const lock = stateLock(e.state);
    const has = (a: DetectedEnvironment["actions"][number]) => e.actions.includes(a);
    const mine = (a: DetectedEnvironment["actions"][number]) => e.onboarding_actions.includes(a);
    const onboarding = e.onboarding?.state;
    const myKey = onboardingStateKey(e.name, found.owner_key);
    const myLock =
      e.onboarding && (e.onboarding.state === "incomplete" || e.onboarding.state === "in_progress")
        ? e.onboarding.lock
        : null;
    // Someone else's run holds the state: a re-apply or destroy would only
    // fail inside its run, so those wait for Unlock.
    const lockHeld = !!lock && !lock.local_process;
    // Join re-applies the onboarding: not under a held lock, nor before a
    // saved state is pushed back.
    const joinBlocked = (!!myLock && !myLock.local_process) || mine("push_state");
    const busy = stale || !!pending[rowKey(at, e.name)];
    const rowError = rowErrors[rowKey(at, e.name)];
    const isWaiting = waiting.has(rowKey(at, e.name));
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
            {e.state.state === "externally_managed" && (
              <div className="text-[var(--text-secondary)]">
                Its OpenTofu state isn't in this region's Triple-C state bucket (deployed from a bundle with another
                bucket, or you can't read it), so it can't be updated from here.
              </div>
            )}
            {e.state.state === "incomplete" && (
              <div className="text-[var(--text-secondary)]">
                {lockHeld
                  ? "The last change didn't finish: its run stopped while holding the state lock. Unlock first, then Repair."
                  : "The last change didn't finish: the deployment has state but no working environment record."}
              </div>
            )}
            {has("push_state") && (
              <div className="text-[var(--error)]">
                The last run on this machine couldn't save its state (OpenTofu kept it in errored.tfstate). Push it back
                before anything else runs here.
              </div>
            )}
            {onboarding === "incomplete" && (
              <div className="text-[var(--text-secondary)]">
                {myLock && !myLock.local_process
                  ? `Your last onboarding didn't finish and still holds its state lock (${lockLine(myLock)}). Unlock it, then Join again to finish.`
                  : mine("push_state")
                    ? "Your last onboarding didn't finish and couldn't save its state; push the saved state back, then Join again."
                    : "Your last onboarding didn't finish; Join again to finish it."}
              </div>
            )}
            {!isAdmin && isWaiting && onboarding !== "joined" && (
              <div className="text-[var(--text-secondary)]">Waiting for an admin to onboard you</div>
            )}
          </div>
          <div className="flex gap-1.5 flex-wrap">
            {has("join") && onboarding !== "joined" && onboarding !== "in_progress" && (
              <Button
                size="sm"
                variant="primary"
                disabled={busy || joinBlocked}
                aria-label={`Join ${e.name}`}
                onClick={() => join(found, e)}
              >
                {!isAdmin && isWaiting ? "Show onboarding bundle" : "Join"}
              </Button>
            )}
            {mine("follow") && (
              <Button
                size="sm"
                disabled={busy}
                aria-label={`Follow my onboarding to ${e.name}`}
                onClick={() =>
                  void launch(at, e.name, myKey, `Joining ${e.name}`, () => followAwsDeployment(connection.id, at, myKey))
                }
              >
                Follow
              </Button>
            )}
            {mine("push_state") && (
              <Button
                size="sm"
                disabled={busy}
                aria-label={`Push saved state of my onboarding to ${e.name}`}
                onClick={() =>
                  void launch(at, e.name, myKey, `Push of the saved state of joining ${e.name}`, () =>
                    pushAwsSavedState(connection.id, at, myKey),
                  )
                }
              >
                Push saved state
              </Button>
            )}
            {mine("unlock") && (
              <Button
                size="sm"
                variant="danger"
                disabled={busy}
                aria-label={`Unlock my onboarding to ${e.name}`}
                onClick={() => setUnlockOf({ name: e.name, region: at, stateKey: myKey, lock: myLock, onboarding: true })}
              >
                Unlock onboarding
              </Button>
            )}
            {has("push_state") && (
              <Button
                size="sm"
                disabled={busy}
                aria-label={`Push saved state of ${e.name}`}
                onClick={() => {
                  const key = environmentStateKey(e.name);
                  void launch(at, e.name, key, `Push of the saved state of ${e.name}`, () =>
                    pushAwsSavedState(connection.id, at, key),
                  );
                }}
              >
                Push saved state
              </Button>
            )}
            {has("follow") && (
              <Button
                size="sm"
                disabled={busy}
                aria-label={`Follow ${e.name}`}
                onClick={() => {
                  const key = environmentStateKey(e.name);
                  void launch(at, e.name, key, `Deploy of ${e.name}`, () => followAwsDeployment(connection.id, at, key));
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
                  void launch(at, e.name, environmentStateKey(e.name), `Update of ${e.name}`, () =>
                    updateAwsEnvironment(connection.id, at, e.name),
                  )
                }
              >
                Update
              </Button>
            )}
            {has("repair") && (
              <Button
                size="sm"
                disabled={busy || lockHeld}
                aria-label={`Repair ${e.name}`}
                onClick={() => (e.info ? void repair(at, e.name) : setRepairOf(e.name))}
              >
                Repair
              </Button>
            )}
            {has("unlock") && (
              <Button
                size="sm"
                variant="danger"
                disabled={busy}
                aria-label={`Unlock ${e.name}`}
                onClick={() =>
                  setUnlockOf({ name: e.name, region: at, stateKey: environmentStateKey(e.name), lock, onboarding: false })
                }
              >
                Unlock
              </Button>
            )}
            {has("destroy") && (
              <Button
                size="sm"
                variant="danger"
                disabled={busy || lockHeld}
                aria-label={`Destroy ${e.name}`}
                onClick={() => setDestroyOf({ name: e.name, region: at })}
              >
                Destroy
              </Button>
            )}
            {lockHeld && (has("repair") || has("destroy")) && (
              <span className="self-center text-xs text-[var(--text-secondary)]">Unlock first</span>
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

  const nothingFound = !!current && current.environments.length === 0;

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
          {current && isAdmin && current.boundary === "no" && (
            <p className="text-xs text-[var(--text-secondary)] leading-snug">
              This account has no Triple-C permissions boundary yet; deploying creates it first.
            </p>
          )}
          {current && current.environments.length > 0 && (
            <ul className="space-y-1.5">{current.environments.map((e) => envRow(current, e))}</ul>
          )}
          {nothingFound && (
            <p className="text-xs text-[var(--text-secondary)] leading-snug">
              No Triple-C environment in {current.region}.
              {!isAdmin &&
                " Deploying one needs an environment admin: attach the environment-admin policies to an identity, or send an admin a bundle to deploy."}
              {!tier && " Run Check to find out whether you can deploy one yourself."}
            </p>
          )}
          {current && isAdmin && (
            <Button
              size="md"
              variant={nothingFound ? "primary" : "secondary"}
              disabled={stale}
              onClick={() => setDeployOpen(true)}
            >
              Deploy a new environment
            </Button>
          )}
          {nothingFound && !isAdmin && (
            <div className="space-y-2">
              <PolicyFiles tier="environment_admin" label="Show the environment-admin policies" onCopy={copy} />
              <Button size="md" disabled={stale} onClick={() => setBundle({ kind: "environment", region: current.region })}>
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
      {current && (deployOpen || repairOf) && (
        <DeployEnvironmentModal
          connectionId={connection.id}
          region={current.region}
          scan={current}
          repair={repairOf ?? undefined}
          onClose={() => {
            setDeployOpen(false);
            setRepairOf(null);
          }}
          onDeploy={(name, network) =>
            repairOf ? repair(current.region, name, network) : deploy(current.region, name, network)
          }
          onDetect={() => void detect()}
        />
      )}
      {destroyOf && (
        <TypedConfirmModal
          title={`Destroy ${destroyOf.name}`}
          name={destroyOf.name}
          confirmLabel="Destroy"
          onClose={() => setDestroyOf(null)}
          onConfirm={() =>
            launch(destroyOf.region, destroyOf.name, environmentStateKey(destroyOf.name), `Destroy of ${destroyOf.name}`, () =>
              destroyIncompleteAwsEnvironment(connection.id, destroyOf.region, destroyOf.name),
            )
          }
        >
          <p>
            This environment's deployment is incomplete (no valid environment record). OpenTofu removes everything
            it created for {destroyOf.name} in {destroyOf.region}. This can't be undone.
          </p>
        </TypedConfirmModal>
      )}
      {unlockOf && (
        <TypedConfirmModal
          title={unlockOf.onboarding ? `Unlock your onboarding to ${unlockOf.name}` : `Unlock ${unlockOf.name}`}
          name={unlockOf.name}
          confirmLabel="Unlock"
          onClose={() => setUnlockOf(null)}
          onConfirm={async () => {
            try {
              await unlockAwsState(connection.id, unlockOf.region, unlockOf.stateKey);
            } catch (e) {
              return toAwsError(e);
            }
            void detect();
            return null;
          }}
        >
          {(() => {
            const lock = unlockOf.lock;
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
          region={bundle.region}
          kind={bundle.kind}
          environment={bundle.environment}
          ownerId={bundle.ownerId}
          onSaved={() => {
            if (bundle.kind === "user_onboard" && bundle.environment) {
              const rk = rowKey(bundle.region, bundle.environment);
              setWaiting((w) => new Set(w).add(rk));
            }
          }}
          onClose={() => setBundle(null)}
        />
      )}
    </div>
  );
}
