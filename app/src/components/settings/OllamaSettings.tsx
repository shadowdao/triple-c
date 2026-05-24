import { useSettings } from "../../hooks/useSettings";
import Tooltip from "../ui/Tooltip";

export default function OllamaSettings() {
  const { appSettings, saveSettings } = useSettings();

  const globalOllama = appSettings?.global_ollama ?? {
    base_url: null,
    default_model_id: null,
  };

  const handleChange = async (field: "base_url" | "default_model_id", value: string) => {
    if (!appSettings) return;
    await saveSettings({
      ...appSettings,
      global_ollama: { ...globalOllama, [field]: value || null },
    });
  };

  return (
    <div>
      <label className="block text-sm font-medium mb-2">Ollama Configuration</label>
      <div className="space-y-3 text-sm">
        <p className="text-xs text-[var(--text-secondary)]">
          Global Ollama defaults. Used when a per-project field is blank.
          Changes here require a container rebuild to take effect.
        </p>

        <div>
          <span className="text-[var(--text-secondary)] text-xs block mb-1">Default Base URL<Tooltip text="URL of your Ollama server. Used when a per-project Ollama base URL is blank." /></span>
          <input
            type="text"
            value={globalOllama.base_url ?? ""}
            onChange={(e) => handleChange("base_url", e.target.value)}
            placeholder="http://host.docker.internal:11434"
            className="w-full px-2 py-1.5 text-xs bg-[var(--bg-primary)] border border-[var(--border-color)] rounded focus:outline-none focus:border-[var(--accent)]"
          />
        </div>

        <div>
          <span className="text-[var(--text-secondary)] text-xs block mb-1">Default Model<Tooltip text="Default Ollama model name. Used when a per-project Ollama model is blank." /></span>
          <input
            type="text"
            value={globalOllama.default_model_id ?? ""}
            onChange={(e) => handleChange("default_model_id", e.target.value)}
            placeholder="qwen3.5:27b"
            className="w-full px-2 py-1.5 text-xs bg-[var(--bg-primary)] border border-[var(--border-color)] rounded focus:outline-none focus:border-[var(--accent)]"
          />
        </div>
      </div>
    </div>
  );
}
