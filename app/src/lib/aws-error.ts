import type { AwsError } from "./types";

const KINDS = ["sso_expired", "profile_not_found", "keys_missing", "invalid_credentials", "access_denied", "other"];

/** Narrow what an AWS command rejected with. Anything that is not a tagged `AwsError` becomes `other`. */
export function toAwsError(e: unknown): AwsError {
  if (e && typeof e === "object" && "kind" in e && KINDS.includes((e as { kind: string }).kind)) {
    return e as AwsError;
  }
  return { kind: "other", message: typeof e === "string" ? e : e instanceof Error ? e.message : "Unknown error" };
}
