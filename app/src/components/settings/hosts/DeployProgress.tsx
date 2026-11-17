import { useCallback, useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { cancelAwsDeployment } from "../../../lib/tauri-commands";
import { awsErrorMessage, toAwsError } from "../../../lib/aws-error";
import {
  AWS_DEPLOY_PROGRESS_EVENT,
  type AwsError,
  type DeployProgress as ProgressPayload,
  type TofuEvent,
  type TofuOutcome,
} from "../../../lib/types";
import Button from "../../ui/Button";
import Modal from "../../ui/Modal";
import StatusIndicator from "../../ui/StatusIndicator";

/**
 * What a run does. Decisions about a run (such as offering an admin bundle
 * when AWS refuses it) key on this, never on its label: a follow of an
 * existing environment's deploy is labelled like a new deploy.
 */
export type RunKind = "deploy" | "follow" | "repair" | "update" | "destroy" | "onboard" | "push" | "boundary";

/** One OpenTofu run this view is showing: a start, or a follow of one already running. */
export interface DeployRun {
  stateKey: string;
  region: string;
  kind: RunKind;
  label: string;
  /** Null until the starting command resolves. */
  opId: string | null;
  events: TofuEvent[];
  done: boolean;
  cancellable: boolean;
  result: TofuOutcome | null;
}

type Subscription = { unlisten: (() => void) | null };

/** Runs are tracked per region and state key: the same key exists in every region. */
export const runKey = (region: string, stateKey: string) => `${region}:${stateKey}`;

/**
 * Runs keyed by `runKey(region, stateKey)`. `start` subscribes to `aws-deploy-progress` before
 * invoking, keeps every event for the state key it started until the op id
 * is known (the backend may emit just before the invoke resolves), then keeps
 * only that op's events. Resolves to the command's error, or null once started.
 */
export function useDeployRuns(
  onDone?: (run: { stateKey: string; region: string; result: TofuOutcome | null }) => void,
) {
  const [runs, setRuns] = useState<Record<string, DeployRun>>({});
  const subs = useRef(new Map<string, Subscription>());
  const mounted = useRef(true);
  const onDoneRef = useRef(onDone);
  onDoneRef.current = onDone;

  useEffect(() => {
    mounted.current = true;
    const all = subs.current;
    return () => {
      mounted.current = false;
      for (const s of all.values()) s.unlisten?.();
      all.clear();
    };
  }, []);

  const start = useCallback(
    async (
      stateKey: string,
      region: string,
      kind: RunKind,
      label: string,
      run: () => Promise<string>,
    ): Promise<AwsError | null> => {
      const k = runKey(region, stateKey);
      subs.current.get(k)?.unlisten?.();
      const sub: Subscription = { unlisten: null };
      subs.current.set(k, sub);
      const current = () => mounted.current && subs.current.get(k) === sub;
      const stop = () => {
        sub.unlisten?.();
        sub.unlisten = null;
        if (subs.current.get(k) === sub) subs.current.delete(k);
      };

      let opId: string | null = null;
      const early: ProgressPayload[] = [];
      const apply = (p: ProgressPayload) => {
        setRuns((prev) => {
          const r = prev[k];
          if (!r || r.opId !== p.op_id || r.done) return prev;
          return {
            ...prev,
            [k]: {
              ...r,
              events: p.events.length ? [...r.events, ...p.events] : r.events,
              done: p.done,
              cancellable: p.done ? false : p.cancellable,
              result: p.result ?? r.result,
            },
          };
        });
        if (p.done) {
          stop();
          onDoneRef.current?.({ stateKey, region, result: p.result ?? null });
        }
      };

      setRuns((prev) => ({
        ...prev,
        [k]: { stateKey, region, kind, label, opId: null, events: [], done: false, cancellable: false, result: null },
      }));

      const unlisten = await listen<ProgressPayload>(AWS_DEPLOY_PROGRESS_EVENT, ({ payload }) => {
        if (!current() || payload.state_key !== stateKey) return;
        if (opId === null) early.push(payload);
        else if (payload.op_id === opId) apply(payload);
      });
      if (!current()) {
        unlisten();
        return null;
      }
      sub.unlisten = unlisten;

      try {
        opId = await run();
      } catch (e) {
        stop();
        if (mounted.current) {
          setRuns((prev) => {
            const { [k]: _gone, ...rest } = prev;
            return rest;
          });
        }
        return toAwsError(e);
      }
      if (!current()) return null;
      const id = opId;
      setRuns((prev) => (prev[k] ? { ...prev, [k]: { ...prev[k], opId: id } } : prev));
      for (const p of early.splice(0)) if (p.op_id === id) apply(p);
      return null;
    },
    [],
  );

  /** `key` is a `runKey`. */
  const dismiss = useCallback((key: string) => {
    setRuns((prev) => {
      if (!prev[key]?.done) return prev;
      const { [key]: _gone, ...rest } = prev;
      return rest;
    });
  }, []);

  return { runs, start, dismiss };
}

function EventLine({ e }: { e: TofuEvent }) {
  const isError = e.level === "error";
  return (
    <li className="text-xs leading-snug">
      {(e.resource || e.action) && (
        <span className="mr-1.5 break-all">
          {e.resource && <span className="font-mono text-[var(--text-primary)]">{e.resource}</span>}
          {e.action && <span className="ml-1 text-[var(--text-secondary)]">{e.action}</span>}
        </span>
      )}
      {isError ? (
        <>
          <span className="font-semibold text-[var(--error)]">Error: </span>
          <pre className="max-w-full overflow-x-auto whitespace-pre-wrap break-words font-mono text-[var(--error)]">{e.message}</pre>
        </>
      ) : (
        <span className="text-[var(--text-secondary)] break-words">{e.message}</span>
      )}
    </li>
  );
}

/** A run's events in order, its final result, and Cancel (after a confirmation). */
export default function DeployProgress({
  run,
  connectionId,
  onDismiss,
}: {
  run: DeployRun;
  connectionId: string;
  onDismiss?: () => void;
}) {
  const [confirming, setConfirming] = useState(false);
  const [cancelling, setCancelling] = useState(false);
  const [cancelError, setCancelError] = useState<AwsError | null>(null);
  const listRef = useRef<HTMLOListElement>(null);

  useEffect(() => {
    const el = listRef.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, [run.events.length]);

  const cancel = async () => {
    setCancelling(true);
    setCancelError(null);
    try {
      await cancelAwsDeployment(connectionId, run.region, run.stateKey);
      setConfirming(false);
    } catch (e) {
      setCancelError(toAwsError(e));
    } finally {
      setCancelling(false);
    }
  };

  const status = run.done
    ? null
    : run.opId === null
      ? { tone: "busy" as const, label: "Starting…" }
      : { tone: "busy" as const, label: "Running" };

  return (
    <section
      aria-label={`${run.label} progress`}
      className="space-y-1.5 min-w-0 border border-[var(--border-color)] rounded-[var(--radius-control)] p-2 bg-[var(--bg-primary)]"
    >
      <div className="flex items-center justify-between gap-2 flex-wrap" data-testid="deploy-progress-header">
        <div className="flex items-center gap-2 text-xs flex-wrap min-w-0">
          <span className="font-medium text-[var(--text-primary)] break-words min-w-0">{run.label}</span>
          {status && <StatusIndicator tone={status.tone} label={status.label} />}
        </div>
        <div className="flex gap-1.5 flex-wrap">
          {!run.done && (
            <Button
              size="sm"
              variant="danger"
              disabled={!run.cancellable}
              title={run.cancellable ? undefined : "This run can't be cancelled from here"}
              onClick={() => setConfirming(true)}
            >
              Cancel deployment
            </Button>
          )}
          {run.done && onDismiss && (
            <Button size="sm" variant="ghost" onClick={onDismiss}>
              Dismiss
            </Button>
          )}
        </div>
      </div>
      {run.events.length > 0 && (
        <ol ref={listRef} className="max-h-64 overflow-y-auto space-y-0.5">
          {run.events.map((e, i) => (
            <EventLine key={i} e={e} />
          ))}
        </ol>
      )}
      {run.done && run.result && (
        <p role="status" className="text-xs break-words">
          {run.result.ok ? (
            <StatusIndicator wrap tone="ok" label={`Finished${run.result.summary ? `: ${run.result.summary}` : ""}`} />
          ) : (
            <StatusIndicator
              wrap
              tone="error"
              label={`Failed (exit ${run.result.exit_code})${run.result.summary ? `: ${run.result.summary}` : ""}`}
            />
          )}
        </p>
      )}
      {cancelError && !confirming && (
        <p role="alert" className="text-xs text-[var(--error)] break-words">
          {awsErrorMessage(cancelError)}
        </p>
      )}
      {confirming && (
        <Modal
          title="Cancel this deployment?"
          onClose={() => setConfirming(false)}
          footer={
            <>
              <Button size="md" onClick={() => setConfirming(false)}>
                Keep running
              </Button>
              <Button size="md" variant="danger" onClick={cancel} disabled={cancelling}>
                Cancel deployment
              </Button>
            </>
          }
        >
          <div className="space-y-2 text-xs text-[var(--text-secondary)] leading-snug">
            <p>
              OpenTofu stops gracefully and saves its state. Anything it already created stays; Repair or Update
              finishes the job later.
            </p>
            {cancelError && (
              <p role="alert" className="text-[var(--error)]">
                {awsErrorMessage(cancelError)}
              </p>
            )}
          </div>
        </Modal>
      )}
    </section>
  );
}
