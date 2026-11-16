import Field, { inputClass } from "../../ui/Field";

/** The longest cap the environment module accepts (`MAX_WORKSPACE_HOURS` in `aws/tofu_modules.rs`). */
export const MAX_WORKSPACE_HOURS = 168;

/** The hours in `text`, or null when it isn't a whole number from 0 to 168. */
export function parseCapHours(text: string): number | null {
  if (!/^\d{1,3}$/.test(text.trim())) return null;
  const n = Number(text);
  return n <= MAX_WORKSPACE_HOURS ? n : null;
}

/** "Maximum workspace lifetime (hours, 0 = none)": the environment's advisory cap. */
export default function LifetimeCapField({
  value,
  onChange,
}: {
  value: string;
  onChange: (text: string) => void;
}) {
  const bad = parseCapHours(value) === null;
  return (
    <Field
      label="Maximum workspace lifetime (hours, 0 = none)"
      hint={
        bad
          ? `Enter a whole number of hours from 0 to ${MAX_WORKSPACE_HOURS}.`
          : "A workspace stops once it has run this long (after a best-effort WIP push). Advisory: it is enforced inside the workspace, which its user controls."
      }
    >
      {(id) => (
        <input
          id={id}
          type="text"
          inputMode="numeric"
          className={inputClass}
          value={value}
          aria-invalid={bad}
          onChange={(e) => onChange(e.target.value)}
        />
      )}
    </Field>
  );
}
