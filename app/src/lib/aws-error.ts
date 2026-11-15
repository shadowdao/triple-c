import type { AwsError } from "./types";

// A `Record` over every kind: adding or removing a kind in `AwsError` fails to compile here.
const KINDS: Record<AwsError["kind"], true> = {
  sso_expired: true,
  profile_not_found: true,
  keys_missing: true,
  invalid_credentials: true,
  access_denied: true,
  state_bucket_taken: true,
  other: true,
};

/** Narrow what an AWS command rejected with. Anything that is not a tagged `AwsError` becomes `other`. */
export function toAwsError(e: unknown): AwsError {
  if (e && typeof e === "object" && "kind" in e) {
    const kind = (e as { kind: unknown }).kind;
    if (typeof kind === "string" && Object.prototype.hasOwnProperty.call(KINDS, kind)) {
      return e as AwsError;
    }
  }
  return { kind: "other", message: typeof e === "string" ? e : e instanceof Error ? e.message : "Unknown error" };
}
