import { describe, it, expect } from "vitest";
import { renderHook, act } from "@testing-library/react";
import { useSecretField, withoutUntouchedSecrets } from "./useSecretField";

describe("useSecretField", () => {
  it("says nothing about a secret the user never touched", () => {
    // The bug this exists for: the input always renders empty (secrets are
    // never serialized to the frontend), so the obvious `value || null` blur
    // handler sent `null` — which means *delete* — merely because the user
    // focused the field and tabbed away. No warning, nothing to undo.
    const { result } = renderHook(() => useSecretField("p1"));
    expect(result.current.value).toBe("");
    expect(result.current.edited).toBe(false);
    expect(result.current.patch("git_token")).toEqual({});
    // Absent, not null: `JSON.stringify` drops the key entirely, and Rust
    // distinguishes an absent key ("leave it") from an explicit null ("clear").
    expect("git_token" in result.current.patch("git_token")).toBe(false);
  });

  it("sends the value once the user types", () => {
    const { result } = renderHook(() => useSecretField("p1"));
    act(() => result.current.setValue("ghp_secret"));
    expect(result.current.patch("git_token")).toEqual({ git_token: "ghp_secret" });
  });

  it("sends null only when the user cleared a field they had typed in", () => {
    // This is the one case where deleting the stored secret is what was asked
    // for, and it has to keep working — the previous behaviour skipped `None`
    // entirely, so a blanked token was never actually revoked.
    const { result } = renderHook(() => useSecretField("p1"));
    act(() => result.current.setValue("typed"));
    act(() => result.current.setValue(""));
    expect(result.current.edited).toBe(true);
    expect(result.current.patch("git_token")).toEqual({ git_token: null });
  });

  it("forgets a half-typed secret when the editor moves to another project", () => {
    const { result, rerender } = renderHook(({ id }) => useSecretField(id), {
      initialProps: { id: "p1" },
    });
    act(() => result.current.setValue("for-project-one"));
    rerender({ id: "p2" });
    expect(result.current.value).toBe("");
    expect(result.current.edited).toBe(false);
    expect(result.current.patch("api_key")).toEqual({});
  });
});

describe("withoutUntouchedSecrets", () => {
  it("drops a secret key the caller did not set", () => {
    // `saveBedrock` spreads `{ ...bedrock, ...patch }`, and when `bedrock`
    // falls back to DEFAULT_BEDROCK_CONFIG that literal spells every secret out
    // as `null`. Without this filter, editing the AWS *region* would delete the
    // stored credentials as a side effect.
    const merged = {
      aws_region: "eu-west-1",
      aws_access_key_id: null,
      aws_secret_access_key: null,
    };
    const out = withoutUntouchedSecrets(merged, { aws_region: "eu-west-1" }, [
      "aws_access_key_id",
      "aws_secret_access_key",
    ]);
    expect(out).toEqual({ aws_region: "eu-west-1" });
    expect("aws_access_key_id" in out).toBe(false);
  });

  it("keeps a secret key the caller set, including an explicit null", () => {
    const merged = { aws_region: "eu-west-1", aws_access_key_id: null };
    const out = withoutUntouchedSecrets(
      merged,
      { aws_access_key_id: null },
      ["aws_access_key_id"],
    );
    expect(out).toEqual({ aws_region: "eu-west-1", aws_access_key_id: null });
  });
});
