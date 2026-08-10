import { useEffect, useState } from "react";
import type { PortMapping } from "../../lib/types";
import Button from "../ui/Button";
import { monoInputClass, selectClass } from "../ui/Field";

interface Props {
  portMappings: PortMapping[];
  disabled: boolean;
  disabledReason?: string;
  onSave: (mappings: PortMapping[]) => Promise<unknown>;
}

export default function PortMappingsEditor({
  portMappings: initial,
  disabled,
  disabledReason,
  onSave,
}: Props) {
  const [mappings, setMappings] = useState<PortMapping[]>(initial);

  useEffect(() => {
    setMappings(initial);
  }, [initial]);

  const updatePort = (
    index: number,
    field: "host_port" | "container_port",
    value: string,
  ) => {
    const updated = [...mappings];
    const num = parseInt(value, 10);
    updated[index] = { ...updated[index], [field]: isNaN(num) ? 0 : num };
    setMappings(updated);
  };

  return (
    <div className="space-y-2">
      {disabled && disabledReason && (
        <p className="px-2 py-1.5 bg-[var(--warning-muted)] border border-[var(--warning)]/30 rounded-[var(--radius-control)] text-xs text-[var(--warning)]">
          {disabledReason}
        </p>
      )}

      {mappings.length === 0 && (
        <p className="text-xs text-[var(--text-secondary)]">No port mappings configured.</p>
      )}

      {mappings.length > 0 && (
        <div className="flex gap-2 items-center text-xs text-[var(--text-secondary)] px-0.5">
          <span className="w-[28%]">Host port</span>
          <span className="w-[28%]">Container port</span>
          <span className="w-[22%]">Protocol</span>
          <span className="flex-1" />
        </div>
      )}

      {mappings.map((pm, i) => (
        <div key={i} className="flex gap-2 items-center">
          <input
            type="number"
            min="1"
            max="65535"
            value={pm.host_port || ""}
            onChange={(e) => updatePort(i, "host_port", e.target.value)}
            onBlur={() => onSave(mappings)}
            placeholder="8080"
            aria-label={`Host port ${i + 1}`}
            disabled={disabled}
            className={`w-[28%] ${monoInputClass}`}
          />
          <input
            type="number"
            min="1"
            max="65535"
            value={pm.container_port || ""}
            onChange={(e) => updatePort(i, "container_port", e.target.value)}
            onBlur={() => onSave(mappings)}
            placeholder="8080"
            aria-label={`Container port ${i + 1}`}
            disabled={disabled}
            className={`w-[28%] ${monoInputClass}`}
          />
          <select
            value={pm.protocol}
            aria-label={`Protocol ${i + 1}`}
            onChange={(e) => {
              const updated = [...mappings];
              updated[i] = { ...updated[i], protocol: e.target.value };
              setMappings(updated);
              onSave(updated);
            }}
            disabled={disabled}
            className={`w-[22%] ${selectClass}`}
          >
            <option value="tcp">TCP</option>
            <option value="udp">UDP</option>
          </select>
          <Button
            variant="danger"
            disabled={disabled}
            aria-label={`Remove port mapping ${i + 1}`}
            onClick={() => {
              const updated = mappings.filter((_, j) => j !== i);
              setMappings(updated);
              onSave(updated);
            }}
          >
            Remove
          </Button>
        </div>
      ))}

      <Button
        disabled={disabled}
        onClick={() => {
          const updated = [
            ...mappings,
            { host_port: 0, container_port: 0, protocol: "tcp" },
          ];
          setMappings(updated);
          onSave(updated);
        }}
      >
        + Add port mapping
      </Button>
    </div>
  );
}
