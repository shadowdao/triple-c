import type { NetworkChoice } from "../../../lib/types";
import Field, { SwitchRow, monoInputClass } from "../../ui/Field";
import SegmentedControl from "../../ui/SegmentedControl";
import Toggle from "../../ui/Toggle";
import type { NetworkDraft } from "./environmentFormat";

/** D4's modes and their cost lines. */
const COST: Record<NetworkChoice["mode"], string> = {
  public: "$0 — tasks get a public IP, no inbound access",
  "private-nat": "about $33/month for the NAT gateway",
  existing: "No new network resources — tasks run in your VPC and two subnets in different availability zones",
};

/** The network picker shared by deploy, repair and the environment bundle. */
export default function NetworkFields({
  value,
  onChange,
  error,
}: {
  value: NetworkDraft;
  onChange: (next: NetworkDraft) => void;
  /** Why the existing-VPC fields aren't usable yet. */
  error?: string | null;
}) {
  const set = (patch: Partial<NetworkDraft>) => onChange({ ...value, ...patch });
  return (
    <div className="space-y-2">
      <div className="text-[13px] font-medium text-[var(--text-primary)]">Network</div>
      <SegmentedControl<NetworkChoice["mode"]>
        label="Network"
        value={value.mode}
        onChange={(mode) => set({ mode })}
        segments={[
          { value: "public", label: "Public" },
          { value: "private-nat", label: "Private + NAT" },
          { value: "existing", label: "Existing VPC" },
        ]}
      />
      <p className="text-xs text-[var(--text-secondary)] leading-snug">{COST[value.mode]}</p>
      {value.mode === "existing" && (
        <div className="space-y-2">
          <Field label="VPC ID">
            {(id) => (
              <input id={id} className={monoInputClass} placeholder="vpc-0123456789abcdef0" value={value.vpcId} onChange={(e) => set({ vpcId: e.target.value })} />
            )}
          </Field>
          <Field label="Subnet ID 1">
            {(id) => (
              <input id={id} className={monoInputClass} placeholder="subnet-…" value={value.subnet1} onChange={(e) => set({ subnet1: e.target.value })} />
            )}
          </Field>
          <Field label="Subnet ID 2">
            {(id) => (
              <input id={id} className={monoInputClass} placeholder="subnet-…" value={value.subnet2} onChange={(e) => set({ subnet2: e.target.value })} />
            )}
          </Field>
          <SwitchRow
            label="Assign a public IP"
            hint="Needed when the subnets have no NAT route to the internet. Tasks still get no inbound access."
            control={<Toggle label="Assign a public IP" checked={value.assignPublicIp} onChange={(v) => set({ assignPublicIp: v })} />}
          />
          {error && <p className="text-xs text-[var(--warning)] leading-snug">{error}</p>}
        </div>
      )}
    </div>
  );
}
