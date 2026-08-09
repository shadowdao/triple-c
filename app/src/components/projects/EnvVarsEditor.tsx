import { useEffect, useState } from "react";
import type { EnvVar } from "../../lib/types";
import Button from "../ui/Button";
import { monoInputClass } from "../ui/Field";

interface Props {
  envVars: EnvVar[];
  disabled: boolean;
  disabledReason?: string;
  onSave: (vars: EnvVar[]) => Promise<unknown>;
}

/** Env-var table. Used inline in Project Home → Config and in global Settings. */
export default function EnvVarsEditor({
  envVars: initial,
  disabled,
  disabledReason,
  onSave,
}: Props) {
  const [vars, setVars] = useState<EnvVar[]>(initial);

  useEffect(() => {
    setVars(initial);
  }, [initial]);

  const updateVar = (index: number, field: keyof EnvVar, value: string) => {
    const updated = [...vars];
    updated[index] = { ...updated[index], [field]: value };
    setVars(updated);
  };

  return (
    <div className="space-y-2">
      {disabled && disabledReason && (
        <p className="px-2 py-1.5 bg-[var(--warning-muted)] border border-[var(--warning)]/30 rounded-[var(--radius-control)] text-xs text-[var(--warning)]">
          {disabledReason}
        </p>
      )}

      {vars.length === 0 && (
        <p className="text-xs text-[var(--text-secondary)]">
          No environment variables configured.
        </p>
      )}

      {vars.map((ev, i) => (
        <div key={i} className="flex gap-2 items-center">
          <input
            value={ev.key}
            onChange={(e) => updateVar(i, "key", e.target.value)}
            onBlur={() => onSave(vars)}
            placeholder="KEY"
            aria-label={`Environment variable ${i + 1} name`}
            disabled={disabled}
            className={`w-2/5 ${monoInputClass}`}
          />
          <input
            value={ev.value}
            onChange={(e) => updateVar(i, "value", e.target.value)}
            onBlur={() => onSave(vars)}
            placeholder="value"
            aria-label={`Environment variable ${i + 1} value`}
            disabled={disabled}
            className={`flex-1 ${monoInputClass}`}
          />
          <Button
            variant="danger"
            disabled={disabled}
            aria-label={`Remove environment variable ${ev.key || i + 1}`}
            onClick={() => {
              const updated = vars.filter((_, j) => j !== i);
              setVars(updated);
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
          const updated = [...vars, { key: "", value: "" }];
          setVars(updated);
          onSave(updated);
        }}
      >
        + Add variable
      </Button>
    </div>
  );
}
