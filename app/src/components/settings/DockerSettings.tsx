import { useState } from "react";
import { useDocker } from "../../hooks/useDocker";

export default function DockerSettings() {
  const { dockerAvailable, imageExists, checkDocker, checkImage, buildImage } =
    useDocker();
  const [building, setBuilding] = useState(false);
  const [buildLog, setBuildLog] = useState<string[]>([]);
  const [error, setError] = useState<string | null>(null);

  const handleBuild = async () => {
    setBuilding(true);
    setBuildLog([]);
    setError(null);
    try {
      await buildImage((msg) => {
        setBuildLog((prev) => [...prev, msg]);
      });
    } catch (e) {
      setError(String(e));
    } finally {
      setBuilding(false);
    }
  };

  return (
    <div>
      <label className="block text-sm font-medium mb-2">Docker</label>
      <div className="space-y-2 text-sm">
        <div className="flex items-center justify-between">
          <span className="text-[var(--text-secondary)]">Docker Status</span>
          <span className={dockerAvailable ? "text-[var(--success)]" : "text-[var(--error)]"}>
            {dockerAvailable === null ? "Checking..." : dockerAvailable ? "Connected" : "Not Available"}
          </span>
        </div>

        <div className="flex items-center justify-between">
          <span className="text-[var(--text-secondary)]">Image</span>
          <span className={imageExists ? "text-[var(--success)]" : "text-[var(--text-secondary)]"}>
            {imageExists === null ? "Checking..." : imageExists ? "Built" : "Not Built"}
          </span>
        </div>

        <div className="flex gap-2">
          <button
            onClick={async () => { await checkDocker(); await checkImage(); }}
            className="px-3 py-1.5 text-xs bg-[var(--bg-tertiary)] border border-[var(--border-color)] rounded hover:bg-[var(--border-color)] transition-colors"
          >
            Refresh Status
          </button>
          <button
            onClick={handleBuild}
            disabled={building || !dockerAvailable}
            className="px-3 py-1.5 text-xs bg-[var(--accent)] text-white rounded hover:bg-[var(--accent-hover)] disabled:opacity-50 transition-colors"
          >
            {building ? "Building..." : imageExists ? "Rebuild Image" : "Build Image"}
          </button>
        </div>

        {buildLog.length > 0 && (
          <div className="max-h-40 overflow-y-auto bg-[var(--bg-primary)] border border-[var(--border-color)] rounded p-2 text-xs font-mono text-[var(--text-secondary)]">
            {buildLog.map((line, i) => (
              <div key={i}>{line}</div>
            ))}
          </div>
        )}

        {error && <div className="text-xs text-[var(--error)]">{error}</div>}
      </div>
    </div>
  );
}
