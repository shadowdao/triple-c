import { useEffect, useState } from "react";
import { listEc2InstanceTypes } from "../../../lib/tauri-commands";
import type { Ec2InstanceTable, Ec2InstanceType } from "../../../lib/types";
import Field, { monoInputClass, selectClass } from "../../ui/Field";
import SegmentedControl from "../../ui/SegmentedControl";
import { EC2_MAX_INSTANCES, userTagError, type Ec2Draft } from "./environmentFormat";

const label = (t: Ec2InstanceType) => `${t.instance_type} — ${t.vcpu} vCPU, ${t.memory_mib / 1024} GiB`;

/**
 * An onboarding's EC2 capacity (phase 7): none, or the user's own instances
 * of one type from the app's table, up to a maximum, on the cluster named
 * after their `triple-c-user` sign-in tag. Shared by self-onboarding and the
 * onboarding bundle.
 */
export default function Ec2CapacityFields({
  value,
  onChange,
}: {
  value: Ec2Draft;
  onChange: (next: Ec2Draft) => void;
}) {
  const [table, setTable] = useState<Ec2InstanceTable | null>(null);
  const [loadError, setLoadError] = useState(false);
  const set = (patch: Partial<Ec2Draft>) => onChange({ ...value, ...patch });

  useEffect(() => {
    let live = true;
    listEc2InstanceTypes()
      .then((t) => live && setTable(t))
      .catch(() => live && setLoadError(true));
    return () => {
      live = false;
    };
  }, []);

  // The table's default fills an empty choice once the table is known.
  useEffect(() => {
    if (table && !value.instanceType) onChange({ ...value, instanceType: table.default_type });
  }, [table, value, onChange]);

  const tagError = value.enabled && value.userTag ? userTagError(value.userTag.trim()) : null;
  const x86 = table?.types.filter((t) => t.arch === "x86_64") ?? [];
  const arm = table?.types.filter((t) => t.arch === "arm64") ?? [];

  return (
    <div className="space-y-2">
      <div className="text-[13px] font-medium text-[var(--text-primary)]">EC2 capacity</div>
      <SegmentedControl<"none" | "ec2">
        label="EC2 capacity"
        className="flex-wrap max-w-full"
        value={value.enabled ? "ec2" : "none"}
        onChange={(v) => set({ enabled: v === "ec2" })}
        segments={[
          { value: "none", label: "None (Fargate only)" },
          { value: "ec2", label: "My own EC2 instances" },
        ]}
      />
      <p className="text-xs text-[var(--text-secondary)] leading-snug">
        {value.enabled
          ? "Projects set to EC2 run on instances only you use. Instances scale to zero when none of your workspaces run; a start after idle takes 2–3 minutes longer. You pay the instance's hourly price while it runs."
          : "Every project runs on Fargate. You can add EC2 capacity later by applying again."}
      </p>
      {value.enabled && (
        <div className="space-y-2">
          <Field label="Instance type">
            {(id) => (
              <select
                id={id}
                className={`${selectClass} w-full`}
                value={value.instanceType}
                disabled={!table}
                onChange={(e) => set({ instanceType: e.target.value })}
              >
                {!table && <option value={value.instanceType}>{value.instanceType || "Loading…"}</option>}
                {x86.length > 0 && (
                  <optgroup label="x86 (Intel)">
                    {x86.map((t) => (
                      <option key={t.instance_type} value={t.instance_type}>
                        {label(t)}
                      </option>
                    ))}
                  </optgroup>
                )}
                {arm.length > 0 && (
                  <optgroup label="ARM (Graviton)">
                    {arm.map((t) => (
                      <option key={t.instance_type} value={t.instance_type}>
                        {label(t)}
                      </option>
                    ))}
                  </optgroup>
                )}
              </select>
            )}
          </Field>
          {loadError && (
            <p role="alert" className="text-xs text-[var(--error)]">
              Could not load the instance types
              {value.instanceType ? `; keeping ${value.instanceType}.` : "; detect again to choose one."}
            </p>
          )}
          <Field label="Maximum instances" hint="How many of your EC2 workspaces can run at once (one per instance).">
            {(id) => (
              <select
                id={id}
                className={`${selectClass} w-full`}
                value={value.maxInstances}
                onChange={(e) => set({ maxInstances: Number(e.target.value) })}
              >
                {Array.from({ length: EC2_MAX_INSTANCES }, (_, i) => i + 1).map((n) => (
                  <option key={n} value={n}>
                    {n}
                  </option>
                ))}
              </select>
            )}
          </Field>
          <Field
            label="Your triple-c-user value"
            hint={tagError ?? "The value an admin set as your triple-c-user sign-in tag. Your EC2 cluster is named after it."}
          >
            {(id) => (
              <input
                id={id}
                className={monoInputClass}
                autoComplete="off"
                spellCheck={false}
                value={value.userTag}
                aria-invalid={!!tagError}
                onChange={(e) => set({ userTag: e.target.value })}
              />
            )}
          </Field>
          <p className="text-xs text-[var(--warning)] leading-snug">
            Triple-C can't read your sign-in's tags to check this value. If it isn't exactly your triple-c-user tag,
            EC2 workspaces fail to start with access denied.
          </p>
        </div>
      )}
    </div>
  );
}
