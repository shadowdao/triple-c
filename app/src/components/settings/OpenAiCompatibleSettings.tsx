import { useSettings } from "../../hooks/useSettings";
import Tooltip from "../ui/Tooltip";

export default function OpenAiCompatibleSettings() {
  const { appSettings, saveSettings } = useSettings();

  const globalOai = appSettings?.global_openai_compatible ?? {
    base_url: null,
    default_model_id: null,
  };

  const handleChange = async (field: "base_url" | "default_model_id", value: string) => {
    if (!appSettings) return;
    await saveSettings({
      ...appSettings,
      global_openai_compatible: { ...globalOai, [field]: value || null },
    });
  };

  return (
    <div>
      <label className="block text-sm font-medium mb-2">OpenAI Compatible Configuration</label>
      <div className="space-y-3 text-sm">
        <p className="text-xs text-[var(--text-secondary)]">
          Global defaults for any OpenAI-compatible endpoint (LiteLLM, OpenRouter, vLLM, etc.).
          Used when a per-project field is blank. Changes require a container rebuild.
        </p>

        <div>
          <span className="text-[var(--text-secondary)] text-xs block mb-1">Default Base URL<Tooltip text="Default OpenAI-compatible endpoint URL. Used when a per-project base URL is blank." /></span>
          <input
            type="text"
            value={globalOai.base_url ?? ""}
            onChange={(e) => handleChange("base_url", e.target.value)}
            placeholder="http://host.docker.internal:4000"
            className="w-full px-2 py-1.5 text-xs bg-[var(--bg-primary)] border border-[var(--border-color)] rounded focus:outline-none focus:border-[var(--accent)]"
          />
        </div>

        <div>
          <span className="text-[var(--text-secondary)] text-xs block mb-1">Default Model<Tooltip text="Default model identifier. Used when a per-project model is blank." /></span>
          <input
            type="text"
            value={globalOai.default_model_id ?? ""}
            onChange={(e) => handleChange("default_model_id", e.target.value)}
            placeholder="gpt-4o / gemini-pro / etc."
            className="w-full px-2 py-1.5 text-xs bg-[var(--bg-primary)] border border-[var(--border-color)] rounded focus:outline-none focus:border-[var(--accent)]"
          />
        </div>
      </div>
    </div>
  );
}
