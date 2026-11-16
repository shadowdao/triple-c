import { useEffect, useState } from "react";
import type { AwsWorkspaceConfig, HourlyEstimate } from "../../../lib/types";
import { awsCostEstimate } from "../../../lib/tauri-commands";

/** Edits settle this long before the estimate is asked for again. */
export const ESTIMATE_DEBOUNCE_MS = 400;

/** "$0.058" below ten cents (two decimals would round most tasks to $0.05/$0.06), "$0.25" above. */
export function formatHourly(usd: number): string {
  return `$${usd.toFixed(usd < 0.1 ? 3 : 2)}`;
}

interface Props {
  /** The project's connection; null when it was removed. */
  connectionId: string | null;
  region: string | null | undefined;
  config: AwsWorkspaceConfig;
  /** An EC2 project's instance type (the user's); null when unknown. */
  instanceType?: string | null;
}

type State = { kind: "loading" } | { kind: "done"; estimate: HourlyEstimate | null };

/**
 * The estimated on-demand cost per hour of an AWS project's Fargate task (Q5), from the Price
 * List API through `aws_cost_estimate` (cached a day per region). Best effort: a denied call, no
 * network or no connection reads "Estimate unavailable" — never an error toast, and nothing waits
 * on it. Re-asked only when a field that changes the price changes, after edits settle.
 */
export default function AwsCostEstimate({ connectionId, region, config, instanceType = null }: Props) {
  const [state, setState] = useState<State>({ kind: "loading" });
  const { launch_type, cpu, memory_mib, arch, ephemeral_gib, spot, storage } = config;
  const ec2 = launch_type === "ec2";
  // Only what the backend prices or notes; the rest of the config is sent but changes nothing.
  // An EC2 project is priced as its instance (E10): only the type and storage matter.
  const key = JSON.stringify(
    ec2
      ? [connectionId, region, launch_type, instanceType, storage]
      : [connectionId, region, launch_type, cpu, memory_mib, arch, ephemeral_gib, spot, storage],
  );

  useEffect(() => {
    if (!connectionId || !region || (ec2 && !instanceType)) {
      setState({ kind: "done", estimate: null });
      return;
    }
    let cancelled = false;
    setState({ kind: "loading" });
    const timer = setTimeout(() => {
      awsCostEstimate(connectionId, region, config, ec2 ? instanceType : null)
        .catch(() => null)
        .then((estimate) => {
          if (!cancelled) setState({ kind: "done", estimate });
        });
    }, ESTIMATE_DEBOUNCE_MS);
    return () => {
      cancelled = true;
      clearTimeout(timer);
    };
    // `key` covers every field of `config` the estimate depends on.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [key]);

  const estimate = state.kind === "done" ? state.estimate : null;
  return (
    <div className="space-y-0.5 text-xs">
      <p className="text-[var(--text-primary)]">
        {state.kind === "loading"
          ? "Estimating…"
          : estimate
            ? `≈ ${formatHourly(estimate.usd_per_hour)}/hour${
                estimate.note_spot ? " (on-demand; Spot is usually up to 70% cheaper)" : ""
              }`
            : "Estimate unavailable"}
      </p>
      {estimate?.ec2_instance && (
        <p className="text-[var(--text-secondary)]">
          While your instance runs. Instances scale to zero when no workspace is running.
          {estimate.efs_billed_separately && " EFS storage is billed separately."}
        </p>
      )}
      {estimate && !estimate.ec2_instance && (
        <p className="text-[var(--text-secondary)]">
          While the task runs; nothing while it is stopped.
          {estimate.efs_billed_separately && " EFS storage is billed separately."}
          {config.gateway_sidecar &&
            " The gateway sidecar shares the task's CPU and memory, so it adds nothing."}
        </p>
      )}
    </div>
  );
}
