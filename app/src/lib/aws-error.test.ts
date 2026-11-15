import { describe, it, expect } from "vitest";
import { toAwsError } from "./aws-error";

describe("toAwsError", () => {
  it("passes a tagged AwsError through", () => {
    const e = { kind: "sso_expired", profile: "dev" };
    expect(toAwsError(e)).toBe(e);
  });
  it("wraps strings and Errors as other", () => {
    expect(toAwsError("boom")).toEqual({ kind: "other", message: "boom" });
    expect(toAwsError(new Error("bad"))).toEqual({ kind: "other", message: "bad" });
  });
  it("rejects unknown kinds, inherited names and null", () => {
    expect(toAwsError({ kind: "nope" }).kind).toBe("other");
    expect(toAwsError({ kind: "toString" }).kind).toBe("other");
    expect(toAwsError(null).kind).toBe("other");
  });
});
