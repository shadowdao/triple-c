import type { SettingsImportPreview } from "./types";

/** Named things a `SettingsImportPreview` says an import will change, for
 *  `ImportSettingsModal`'s confirmation list. Does not include anything
 *  `describeImportWarnings` covers — those get their own, more visible
 *  treatment rather than blending into this list. */
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
  if (preview.has_web_terminal_access_token) items.push("The web terminal access token");
  return items;
}

/**
 * Things about an import that deserve more attention than a bullet in a
 * long list — currently just the one, but deliberately its own function
 * rather than a flag inside `describeImport`: a setting that turns on a
 * network-listening service is exactly the kind of change a "your settings
 * were replaced" summary is bad at surfacing, on purpose or (if the file
 * came from someone else) not.
 */
export function describeImportWarnings(preview: SettingsImportPreview): string[] {
  const warnings: string[] = [];
  if (preview.enables_web_terminal) {
    warnings.push("Enables the remote web terminal, which listens on your network.");
  }
  return warnings;
}
