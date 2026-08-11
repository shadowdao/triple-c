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

      {/* The row's widths live on wrapper divs, not on the inputs. `inputClass`
          carries `w-full`, and a width utility on the input itself does not beat
          it — class-attribute order is not what resolves the conflict, stylesheet
          order is. Sizing the key input directly left it asking for the whole row
          and collapsed the value input, whose `flex-1` basis of 0 gave it only the
          leftover space, to an unusable sliver. */}
      {vars.map((ev, i) => (
        <div key={i} className="flex gap-2 items-center">
          <div className="w-2/5 shrink-0">
            <input
              value={ev.key}
              onChange={(e) => updateVar(i, "key", e.target.value)}
              onBlur={() => onSave(vars)}
              placeholder="KEY"
              aria-label={`Environment variable ${i + 1} name`}
              disabled={disabled}
              className={monoInputClass}
            />
          </div>
          <div className="flex-1 min-w-0">
            <input
              value={ev.value}
              onChange={(e) => updateVar(i, "value", e.target.value)}
              onBlur={() => onSave(vars)}
              placeholder="value"
              aria-label={`Environment variable ${i + 1} value`}
              disabled={disabled}
              className={monoInputClass}
            />
          </div>
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
