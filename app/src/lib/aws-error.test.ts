import { describe, it, expect } from "vitest";
import { awsErrorMessage, looksDenied, toAwsError } from "./aws-error";

describe("toAwsError", () => {
  it("passes a tagged AwsError through", () => {
    const e = { kind: "sso_expired", profile: "dev" };
    expect(toAwsError(e)).toBe(e);
  });
  it("passes a quota error through with its message", () => {
    const e = { kind: "quota_exceeded", message: "An account limit was reached" };
    expect(toAwsError(e)).toBe(e);
    expect(awsErrorMessage(toAwsError(e))).toBe("An account limit was reached");
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

describe("access denied", () => {
  it("shows AWS's message, naming the action when the message doesn't", () => {
    expect(awsErrorMessage({ kind: "access_denied", action: "iam:CreateRole", message: "User: x is not authorized" })).toBe(
      "AWS denied iam:CreateRole: User: x is not authorized",
    );
    expect(awsErrorMessage({ kind: "access_denied", action: "s3:PutObject", message: "denied s3:PutObject" })).toBe(
      "AWS denied this request: denied s3:PutObject",
    );
  });
  it("recognises refusals in OpenTofu output", () => {
    expect(looksDenied("Error: creating IAM Role: AccessDenied: User is not authorized")).toBe(true);
    expect(looksDenied("api error UnauthorizedOperation: You are not authorized")).toBe(true);
    expect(looksDenied("Error: creating VPC: VpcLimitExceeded")).toBe(false);
    expect(looksDenied(null)).toBe(false);
    expect(looksDenied("blocked by an explicit deny in a service control policy")).toBe(false);
  });
});
