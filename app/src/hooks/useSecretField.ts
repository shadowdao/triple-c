import { useCallback, useEffect, useRef, useState } from "react";

/**
 * A password input whose stored value the frontend can never see.
 *
 * Secrets live in the OS keychain and are `#[serde(skip_serializing)]`, so a
 * `Project` arriving from Rust has no key for them at all and the input always
 * renders empty — whether or not a credential is stored. That is fine on its
 * own. What is not fine is the obvious blur handler:
 *
 * ```tsx
 * onBlur={() => save({ git_token: gitToken || null })}
 * ```
 *
 * An empty box sends `null`, and `null` now means **delete** (it used to mean
 * "skip", which was its own bug — a blanked token was never actually revoked).
 * So merely focusing a secret field and tabbing away destroyed the stored
 * credential, with nothing shown and nothing to undo it.
 *
 * The rule this encodes: **only a field the user actually typed in may speak
 * about a secret.** `patch()` returns `undefined` until then, and `undefined`
 * is dropped by `JSON.stringify`, so the key never reaches Rust — which
 * deliberately distinguishes an absent key ("leave it alone") from an explicit
 * `null` ("clear it"). See `explicitly_cleared_secrets` in
 * `commands/project_commands.rs`.
 */
export interface SecretField {
  /** Current input value. Always starts empty for a stored secret. */
  value: string;
  /** Whether the user has typed in this field since it was last reset. */
  edited: boolean;
  /** `onChange` handler — marks the field edited. */
  setValue: (next: string) => void;
  /**
   * What to put in the save patch, spread into it:
   * `save({ ...token.patch("git_token") })`.
   *
   * Empty when untouched, so the key is absent and the stored secret stands.
   */
  patch: <K extends string>(key: K) => Partial<Record<K, string | null>>;
}

export function useSecretField(projectId: string): SecretField {
  const [value, setValueRaw] = useState("");
  const [edited, setEdited] = useState(false);
  // Reset when the editor moves to a different project, so a value typed for
  // one project can never be saved onto another.
  const lastProject = useRef(projectId);

  useEffect(() => {
    if (lastProject.current !== projectId) {
      lastProject.current = projectId;
      setValueRaw("");
      setEdited(false);
    }
  }, [projectId]);

  const setValue = useCallback((next: string) => {
    setValueRaw(next);
    setEdited(true);
  }, []);

  const patch = useCallback(
    <K extends string>(key: K): Partial<Record<K, string | null>> =>
      edited ? ({ [key]: value || null } as Partial<Record<K, string | null>>) : {},
    [edited, value],
  );

  return { value, edited, setValue, patch };
}

/**
 * Drop secret keys the caller did not explicitly set.
 *
 * The config editors save by spreading — `save({ bedrock_config: { ...bedrock,
 * ...patch } })`. That is safe while `bedrock` comes from Rust, because secrets
 * are never serialized and the keys are simply absent. It stops being safe the
 * moment the spread falls back to a `DEFAULT_*_CONFIG` literal, because those
 * spell every secret out as `null` — and `null` means delete. Editing the AWS
 * region would then wipe the stored credentials as a side effect.
 *
 * So the merged object is filtered: a secret key survives only if it is in the
 * caller's own patch, which is to say only if a `useSecretField` that the user
 * typed into put it there.
 */
export function withoutUntouchedSecrets<T extends object>(
  merged: T,
  patch: Partial<T>,
  secretKeys: readonly (keyof T)[],
): T {
  const out = { ...merged };
  for (const key of secretKeys) {
    if (!(key in patch)) delete out[key];
  }
  return out;
}
