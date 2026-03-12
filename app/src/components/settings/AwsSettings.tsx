import { useState, useEffect } from "react";
import { useSettings } from "../../hooks/useSettings";
import * as commands from "../../lib/tauri-commands";
import Tooltip from "../ui/Tooltip";

export default function AwsSettings() {
  const { appSettings, saveSettings } = useSettings();
  const [profiles, setProfiles] = useState<string[]>([]);
  const [detecting, setDetecting] = useState(false);

  const globalAws = appSettings?.global_aws ?? {
    aws_config_path: null,
    aws_profile: null,
    aws_region: null,
  };

  // Load profiles when component mounts or aws_config_path changes
  useEffect(() => {
    commands.listAwsProfiles().then(setProfiles).catch(() => setProfiles([]));
  }, [globalAws.aws_config_path]);

  const handleDetect = async () => {
    setDetecting(true);
    try {
      const path = await commands.detectAwsConfig();
      if (path && appSettings) {
        const updated = {
          ...appSettings,
          global_aws: { ...globalAws, aws_config_path: path },
        };
        await saveSettings(updated);
        // Refresh profiles after detection
        const p = await commands.listAwsProfiles();
        setProfiles(p);
      }
    } finally {
      setDetecting(false);
    }
  };

  const handleChange = async (field: string, value: string | null) => {
    if (!appSettings) return;
    await saveSettings({
      ...appSettings,
      global_aws: { ...globalAws, [field]: value || null },
    });
  };

  return (
    <div>
      <label className="block text-sm font-medium mb-2">AWS Configuration</label>
      <div className="space-y-3 text-sm">
        <p className="text-xs text-[var(--text-secondary)]">
          Global AWS defaults for Bedrock projects. Per-project settings override these.
          Changes here require a container rebuild to take effect.
        </p>

        {/* AWS Config Path */}
        <div>
          <span className="text-[var(--text-secondary)] text-xs block mb-1">AWS Config Path<Tooltip text="Path to your AWS config/credentials directory. Mounted into containers for Bedrock auth." /></span>
          <div className="flex gap-2">
            <input
              type="text"
              value={globalAws.aws_config_path ?? ""}
              onChange={(e) => handleChange("aws_config_path", e.target.value)}
              placeholder="~/.aws"
              className="flex-1 px-2 py-1.5 text-xs bg-[var(--bg-primary)] border border-[var(--border-color)] rounded focus:outline-none focus:border-[var(--accent)]"
            />
            <button
              onClick={handleDetect}
              disabled={detecting}
              className="px-3 py-1.5 text-xs bg-[var(--bg-tertiary)] border border-[var(--border-color)] rounded hover:bg-[var(--border-color)] transition-colors"
            >
              {detecting ? "..." : "Detect"}
            </button>
          </div>
          {globalAws.aws_config_path && (
            <span className="text-xs text-[var(--success)] mt-0.5 block">Found</span>
          )}
        </div>

        {/* AWS Profile */}
        <div>
          <span className="text-[var(--text-secondary)] text-xs block mb-1">Default Profile<Tooltip text="AWS named profile to use by default. Per-project settings can override this." /></span>
          <select
            value={globalAws.aws_profile ?? ""}
            onChange={(e) => handleChange("aws_profile", e.target.value)}
            className="w-full px-2 py-1.5 text-xs bg-[var(--bg-primary)] text-[var(--text-primary)] border border-[var(--border-color)] rounded focus:outline-none focus:border-[var(--accent)]"
          >
            <option value="">None (use default)</option>
            {profiles.map((p) => (
              <option key={p} value={p}>{p}</option>
            ))}
          </select>
        </div>

        {/* AWS Region */}
        <div>
          <span className="text-[var(--text-secondary)] text-xs block mb-1">Default Region<Tooltip text="Default AWS region for Bedrock API calls (e.g. us-east-1). Can be overridden per project." /></span>
          <input
            type="text"
            value={globalAws.aws_region ?? ""}
            onChange={(e) => handleChange("aws_region", e.target.value)}
            placeholder="e.g., us-east-1"
            className="w-full px-2 py-1.5 text-xs bg-[var(--bg-primary)] border border-[var(--border-color)] rounded focus:outline-none focus:border-[var(--accent)]"
          />
        </div>
      </div>
    </div>
  );
}
