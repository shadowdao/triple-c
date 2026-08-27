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
  if (preview.ollama_base_url) items.push(`Ollama server: ${preview.ollama_base_url}`);
  if (preview.llamacpp_base_url) items.push(`llama.cpp server: ${preview.llamacpp_base_url}`);
  if (preview.openai_compatible_base_url) {
    items.push(`OpenAI-compatible server: ${preview.openai_compatible_base_url}`);
  }
  if (preview.gateway_api_base) items.push(`Gateway upstream: ${preview.gateway_api_base}`);
  return items;
}

/**
 * Things about an import that deserve more attention than a bullet in a
 * long list — deliberately its own function rather than a flag inside
 * `describeImport`: a setting that turns on a network-listening service is
 * exactly the kind of change a "your settings were replaced" summary is bad
 * at surfacing, on purpose or (if the file came from someone else) not.
 *
 * A token that arrives with the terminal left *off* gets its own warning
 * too, distinct from the "enables it now" one: `start_web_terminal` only
 * mints a fresh token when none is already set, so a planted token here
 * would silently become live the next time someone flips the terminal on
 * through the UI, with no import-time signal that it wasn't freshly
 * generated.
 */
export function describeImportWarnings(preview: SettingsImportPreview): string[] {
  const warnings: string[] = [];
  if (preview.enables_web_terminal) {
    warnings.push("Enables the remote web terminal, which listens on your network.");
  } else if (preview.has_web_terminal_access_token) {
    warnings.push(
      "Includes a web terminal access token that will activate the next time the web terminal is turned on.",
    );
  }
  return warnings;
}
