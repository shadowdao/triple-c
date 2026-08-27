import type { SettingsImportPreview } from "./types";

/** Named things a `SettingsImportPreview` says an import will change, for
 *  `ImportSettingsModal`'s confirmation list. */
export function describeImport(preview: SettingsImportPreview): string[] {
  const items: string[] = ["Your global settings (all of them — this replaces what's here now)"];
  if (preview.custom_env_var_count > 0) {
    items.push(
      `${preview.custom_env_var_count} global custom env var${preview.custom_env_var_count === 1 ? "" : "s"}`,
    );
  }
  if (preview.has_claude_code_settings) items.push("Global Claude Code settings");
  if (preview.gateway_model_count > 0) {
    items.push(`${preview.gateway_model_count} gateway model${preview.gateway_model_count === 1 ? "" : "s"}`);
  }
  if (preview.has_claude_oauth_token) items.push("Your shared Claude login");
  if (preview.has_gateway_api_key) items.push("The gateway provider API key");
  if (preview.has_gateway_master_key) items.push("The gateway master key");
  return items;
}
