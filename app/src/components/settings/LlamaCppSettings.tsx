import { useSettings } from "../../hooks/useSettings";
import Tooltip from "../ui/Tooltip";

type Field = "base_url" | "default_model_id" | "default_haiku_model_id";

export default function LlamaCppSettings() {
  const { appSettings, saveSettings } = useSettings();

  const globalLlamaCpp = appSettings?.global_llamacpp ?? {
    base_url: null,
    default_model_id: null,
    default_haiku_model_id: null,
  };

  const handleChange = async (field: Field, value: string) => {
    if (!appSettings) return;
    await saveSettings({
      ...appSettings,
      global_llamacpp: { ...globalLlamaCpp, [field]: value || null },
    });
  };

  return (
    <div>
      <label className="block text-sm font-medium mb-2">llama.cpp Configuration</label>
      <div className="space-y-3 text-sm">
        <p className="text-xs text-[var(--text-secondary)]">
          Global defaults for a local or remote <code>llama-server</code>, which serves
          the Anthropic Messages API directly. Used when a per-project field is blank.
          Changes here require a container rebuild to take effect.
        </p>

        <div>
          <span className="text-[var(--text-secondary)] text-xs block mb-1">Default Base URL<Tooltip text="URL of your llama-server. Used when a per-project llama.cpp base URL is blank. llama-server listens on port 8080 by default." /></span>
          <input
            type="text"
            value={globalLlamaCpp.base_url ?? ""}
            onChange={(e) => handleChange("base_url", e.target.value)}
            placeholder="http://host.docker.internal:8080"
            className="w-full px-2 py-1.5 text-xs bg-[var(--bg-primary)] border border-[var(--border-color)] rounded focus:border-[var(--accent)]"
          />
        </div>

        <div>
          <span className="text-[var(--text-secondary)] text-xs block mb-1">Default Model<Tooltip text="Default model identifier. Used when a per-project llama.cpp model is blank." /></span>
          <input
            type="text"
            value={globalLlamaCpp.default_model_id ?? ""}
            onChange={(e) => handleChange("default_model_id", e.target.value)}
            placeholder="qwen3.5-coder-30b"
            className="w-full px-2 py-1.5 text-xs bg-[var(--bg-primary)] border border-[var(--border-color)] rounded focus:border-[var(--accent)]"
          />
        </div>

        <div>
          <span className="text-[var(--text-secondary)] text-xs block mb-1">Default Background Model<span className="text-[var(--text-disabled)]"> (optional)</span><Tooltip text="What the `haiku` alias resolves to, which is also what Claude Code uses for background work such as titles and summaries. Leave blank to reuse the model above — only set this if you serve a second, smaller model." /></span>
          <input
            type="text"
            value={globalLlamaCpp.default_haiku_model_id ?? ""}
            onChange={(e) => handleChange("default_haiku_model_id", e.target.value)}
            placeholder="(same as the model above)"
            className="w-full px-2 py-1.5 text-xs bg-[var(--bg-primary)] border border-[var(--border-color)] rounded focus:border-[var(--accent)]"
          />
        </div>
      </div>
    </div>
  );
}
