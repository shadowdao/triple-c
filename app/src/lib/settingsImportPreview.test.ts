import { describe, it, expect } from "vitest";
import { describeImport } from "./settingsImportPreview";
import type { SettingsImportPreview } from "./types";

function preview(overrides: Partial<SettingsImportPreview> = {}): SettingsImportPreview {
  return {
    exported_at: "2026-08-27T00:00:00Z",
    app_version: "0.4.14",
    custom_env_var_count: 0,
    gateway_model_count: 0,
    has_claude_code_settings: false,
    has_claude_oauth_token: false,
    has_gateway_api_key: false,
    has_gateway_master_key: false,
    ...overrides,
  };
}

describe("describeImport", () => {
  it("always names the settings replacement, even with nothing else set", () => {
    expect(describeImport(preview())).toEqual([
      "Your global settings (all of them — this replaces what's here now)",
    ]);
  });

  it("singularizes a count of exactly one", () => {
    const items = describeImport(preview({ custom_env_var_count: 1, gateway_model_count: 1 }));
    expect(items).toContain("1 global custom env var");
    expect(items).toContain("1 gateway model");
  });

  it("pluralizes counts greater than one", () => {
    const items = describeImport(preview({ custom_env_var_count: 3, gateway_model_count: 2 }));
    expect(items).toContain("3 global custom env vars");
    expect(items).toContain("2 gateway models");
  });

  it("names every present secret and setting without naming absent ones", () => {
    const items = describeImport(
      preview({
        has_claude_code_settings: true,
        has_claude_oauth_token: true,
        has_gateway_api_key: true,
        has_gateway_master_key: true,
      }),
    );
    expect(items).toContain("Global Claude Code settings");
    expect(items).toContain("Your shared Claude login");
    expect(items).toContain("The gateway provider API key");
    expect(items).toContain("The gateway master key");
    // None of the count-based items, since both counts are 0.
    expect(items.some((i) => i.includes("env var"))).toBe(false);
    expect(items.some((i) => i.includes("gateway model"))).toBe(false);
  });
});
