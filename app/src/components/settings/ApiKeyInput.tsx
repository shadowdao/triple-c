import { useState } from "react";
import { useSettings } from "../../hooks/useSettings";

export default function ApiKeyInput() {
  const { hasKey, saveApiKey, removeApiKey } = useSettings();
  const [key, setKey] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);

  const handleSave = async () => {
    if (!key.trim()) return;
    setSaving(true);
    setError(null);
    try {
      await saveApiKey(key.trim());
      setKey("");
    } catch (e) {
      setError(String(e));
    } finally {
      setSaving(false);
    }
  };

  return (
    <div>
      <label className="block text-sm font-medium mb-1">Authentication</label>
      <p className="text-xs text-[var(--text-secondary)] mb-3">
        Each project can use <strong>claude login</strong> (OAuth, run inside the terminal), an <strong>API key</strong>, or <strong>AWS Bedrock</strong>. Set auth mode per-project.
      </p>

      <label className="block text-xs text-[var(--text-secondary)] mb-1 mt-3">
        API Key (for projects using API key mode)
      </label>
      {hasKey ? (
        <div className="flex items-center gap-2">
          <span className="text-sm text-[var(--success)]">Key configured</span>
          <button
            onClick={async () => {
              try { await removeApiKey(); } catch (e) { setError(String(e)); }
            }}
            className="text-xs text-[var(--error)] hover:underline"
          >
            Remove
          </button>
        </div>
      ) : (
        <div className="space-y-2">
          <input
            type="password"
            value={key}
            onChange={(e) => setKey(e.target.value)}
            placeholder="sk-ant-..."
            className="w-full px-3 py-2 bg-[var(--bg-primary)] border border-[var(--border-color)] rounded text-sm text-[var(--text-primary)] focus:outline-none focus:border-[var(--accent)]"
            onKeyDown={(e) => e.key === "Enter" && handleSave()}
          />
          <button
            onClick={handleSave}
            disabled={saving || !key.trim()}
            className="px-3 py-1.5 text-xs bg-[var(--accent)] text-white rounded hover:bg-[var(--accent-hover)] disabled:opacity-50 transition-colors"
          >
            {saving ? "Saving..." : "Save Key"}
          </button>
        </div>
      )}
      {error && <div className="text-xs text-[var(--error)] mt-1">{error}</div>}
    </div>
  );
}
