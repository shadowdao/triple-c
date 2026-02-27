import { useState } from "react";
import { useDocker } from "../../hooks/useDocker";
import { useSettings } from "../../hooks/useSettings";
import type { ImageSource } from "../../lib/types";

const REGISTRY_IMAGE = "repo.anhonesthost.net/cybercovellc/triple-c/triple-c-sandbox:latest";

const IMAGE_SOURCE_OPTIONS: { value: ImageSource; label: string; description: string }[] = [
  { value: "registry", label: "Registry", description: "Pull from container registry" },
  { value: "local_build", label: "Local Build", description: "Build from embedded Dockerfile" },
  { value: "custom", label: "Custom", description: "Specify a custom image" },
];

export default function DockerSettings() {
  const { dockerAvailable, imageExists, checkDocker, checkImage, buildImage, pullImage } =
    useDocker();
  const { appSettings, saveSettings } = useSettings();
  const [working, setWorking] = useState(false);
  const [log, setLog] = useState<string[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [customInput, setCustomInput] = useState(appSettings?.custom_image_name ?? "");

  const imageSource = appSettings?.image_source ?? "registry";

  const resolvedImageName = (() => {
    switch (imageSource) {
      case "registry": return REGISTRY_IMAGE;
      case "local_build": return "triple-c:latest";
      case "custom": return customInput || REGISTRY_IMAGE;
    }
  })();

  const handleSourceChange = async (source: ImageSource) => {
    if (!appSettings) return;
    await saveSettings({ ...appSettings, image_source: source });
    // Re-check image existence after changing source
    setTimeout(() => checkImage(), 100);
  };

  const handleCustomChange = async (value: string) => {
    setCustomInput(value);
    if (!appSettings) return;
    await saveSettings({ ...appSettings, custom_image_name: value || null });
  };

  const handlePull = async () => {
    setWorking(true);
    setLog([]);
    setError(null);
    try {
      await pullImage(resolvedImageName, (msg) => {
        setLog((prev) => [...prev, msg]);
      });
      await checkImage();
    } catch (e) {
      setError(String(e));
    } finally {
      setWorking(false);
    }
  };

  const handleBuild = async () => {
    setWorking(true);
    setLog([]);
    setError(null);
    try {
      await buildImage((msg) => {
        setLog((prev) => [...prev, msg]);
      });
      await checkImage();
    } catch (e) {
      setError(String(e));
    } finally {
      setWorking(false);
    }
  };

  return (
    <div>
      <label className="block text-sm font-medium mb-2">Docker</label>
      <div className="space-y-3 text-sm">
        <div className="flex items-center justify-between">
          <span className="text-[var(--text-secondary)]">Docker Status</span>
          <span className={dockerAvailable ? "text-[var(--success)]" : "text-[var(--error)]"}>
            {dockerAvailable === null ? "Checking..." : dockerAvailable ? "Connected" : "Not Available"}
          </span>
        </div>

        {/* Image Source Selector */}
        <div>
          <span className="text-[var(--text-secondary)] text-xs block mb-1.5">Image Source</span>
          <div className="flex gap-1">
            {IMAGE_SOURCE_OPTIONS.map((opt) => (
              <button
                key={opt.value}
                onClick={() => handleSourceChange(opt.value)}
                className={`flex-1 px-2 py-1.5 text-xs rounded border transition-colors ${
                  imageSource === opt.value
                    ? "bg-[var(--accent)] text-white border-[var(--accent)]"
                    : "bg-[var(--bg-tertiary)] border-[var(--border-color)] hover:bg-[var(--border-color)]"
                }`}
                title={opt.description}
              >
                {opt.label}
              </button>
            ))}
          </div>
        </div>

        {/* Custom image input */}
        {imageSource === "custom" && (
          <div>
            <span className="text-[var(--text-secondary)] text-xs block mb-1">Custom Image</span>
            <input
              type="text"
              value={customInput}
              onChange={(e) => handleCustomChange(e.target.value)}
              placeholder="e.g., myregistry.com/image:tag"
              className="w-full px-2 py-1.5 text-xs bg-[var(--bg-primary)] border border-[var(--border-color)] rounded focus:outline-none focus:border-[var(--accent)]"
            />
          </div>
        )}

        {/* Resolved image display */}
        <div className="flex items-center justify-between">
          <span className="text-[var(--text-secondary)]">Image</span>
          <span className="text-xs text-[var(--text-secondary)] truncate max-w-[200px]" title={resolvedImageName}>
            {resolvedImageName}
          </span>
        </div>

        <div className="flex items-center justify-between">
          <span className="text-[var(--text-secondary)]">Status</span>
          <span className={imageExists ? "text-[var(--success)]" : "text-[var(--text-secondary)]"}>
            {imageExists === null ? "Checking..." : imageExists ? "Ready" : "Not Found"}
          </span>
        </div>

        {/* Action buttons */}
        <div className="flex gap-2">
          <button
            onClick={async () => { await checkDocker(); await checkImage(); }}
            className="px-3 py-1.5 text-xs bg-[var(--bg-tertiary)] border border-[var(--border-color)] rounded hover:bg-[var(--border-color)] transition-colors"
          >
            Refresh
          </button>

          {imageSource === "local_build" ? (
            <button
              onClick={handleBuild}
              disabled={working || !dockerAvailable}
              className="px-3 py-1.5 text-xs bg-[var(--accent)] text-white rounded hover:bg-[var(--accent-hover)] disabled:opacity-50 transition-colors"
            >
              {working ? "Building..." : imageExists ? "Rebuild Image" : "Build Image"}
            </button>
          ) : (
            <button
              onClick={handlePull}
              disabled={working || !dockerAvailable}
              className="px-3 py-1.5 text-xs bg-[var(--accent)] text-white rounded hover:bg-[var(--accent-hover)] disabled:opacity-50 transition-colors"
            >
              {working ? "Pulling..." : imageExists ? "Re-pull Image" : "Pull Image"}
            </button>
          )}
        </div>

        {/* Log output */}
        {log.length > 0 && (
          <div className="max-h-40 overflow-y-auto bg-[var(--bg-primary)] border border-[var(--border-color)] rounded p-2 text-xs font-mono text-[var(--text-secondary)]">
            {log.map((line, i) => (
              <div key={i}>{line}</div>
            ))}
          </div>
        )}

        {error && <div className="text-xs text-[var(--error)]">{error}</div>}
      </div>
    </div>
  );
}
