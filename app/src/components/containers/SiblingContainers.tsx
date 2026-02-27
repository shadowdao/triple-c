import { useState, useEffect, useCallback } from "react";
import { listSiblingContainers } from "../../lib/tauri-commands";
import type { SiblingContainer } from "../../lib/types";

export default function SiblingContainers() {
  const [containers, setContainers] = useState<SiblingContainer[]>([]);
  const [loading, setLoading] = useState(false);

  const refresh = useCallback(async () => {
    setLoading(true);
    try {
      const list = await listSiblingContainers();
      setContainers(list);
    } catch {
      // Silently fail
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    refresh();
  }, [refresh]);

  return (
    <div className="p-4">
      <div className="flex items-center justify-between mb-3">
        <h3 className="text-sm font-medium">Sibling Containers</h3>
        <button
          onClick={refresh}
          disabled={loading}
          className="text-xs text-[var(--text-secondary)] hover:text-[var(--text-primary)] transition-colors"
        >
          {loading ? "..." : "Refresh"}
        </button>
      </div>
      {containers.length === 0 ? (
        <p className="text-xs text-[var(--text-secondary)]">No other containers found.</p>
      ) : (
        <div className="space-y-2">
          {containers.map((c) => (
            <div
              key={c.id}
              className="px-3 py-2 bg-[var(--bg-primary)] border border-[var(--border-color)] rounded text-xs"
            >
              <div className="flex items-center gap-2">
                <span
                  className={`w-2 h-2 rounded-full flex-shrink-0 ${
                    c.state === "running"
                      ? "bg-[var(--success)]"
                      : "bg-[var(--text-secondary)]"
                  }`}
                />
                <span className="font-medium truncate">
                  {c.names?.[0]?.replace(/^\//, "") ?? c.id.slice(0, 12)}
                </span>
              </div>
              <div className="text-[var(--text-secondary)] mt-0.5 ml-4">
                {c.image} — {c.status}
              </div>
            </div>
          ))}
        </div>
      )}
    </div>
  );
}
