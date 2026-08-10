import { useState, useEffect } from "react";
import { startWebTerminal, stopWebTerminal, getWebTerminalStatus, regenerateWebTerminalToken } from "../../lib/tauri-commands";
import type { WebTerminalInfo } from "../../lib/types";
import Tooltip from "../ui/Tooltip";
import Toggle from "../ui/Toggle";

export default function WebTerminalSettings() {
  const [info, setInfo] = useState<WebTerminalInfo | null>(null);
  const [loading, setLoading] = useState(false);
  const [copied, setCopied] = useState(false);

  useEffect(() => {
    getWebTerminalStatus().then(setInfo).catch(console.error);
  }, []);

  const handleToggle = async () => {
    setLoading(true);
    try {
      if (info?.running) {
        await stopWebTerminal();
        const updated = await getWebTerminalStatus();
        setInfo(updated);
      } else {
        const updated = await startWebTerminal();
        setInfo(updated);
      }
    } catch (e) {
      console.error("Web terminal toggle failed:", e);
    } finally {
      setLoading(false);
    }
  };

  const handleRegenerate = async () => {
    try {
      const updated = await regenerateWebTerminalToken();
      setInfo(updated);
    } catch (e) {
      console.error("Token regeneration failed:", e);
    }
  };

  const handleCopyUrl = async () => {
    if (info?.url) {
      await navigator.clipboard.writeText(info.url);
      setCopied(true);
      setTimeout(() => setCopied(false), 2000);
    }
  };

  const handleCopyToken = async () => {
    if (info?.access_token) {
      await navigator.clipboard.writeText(info.access_token);
      setCopied(true);
      setTimeout(() => setCopied(false), 2000);
    }
  };

  return (
    <div>
      <label className="block text-sm font-medium mb-1">
        Web Terminal
        <Tooltip text="Access your terminals from a tablet or phone on the local network via a web browser." />
      </label>
      <p className="text-xs text-[var(--text-secondary)] mb-2">
        Serves a browser-based terminal UI on your local network for remote access to running projects.
      </p>

      <div className="space-y-2">
        {/* Toggle */}
        <div className="flex items-center gap-2">
          <Toggle
            label="Web terminal"
            checked={!!info?.running}
            disabled={loading}
            onChange={handleToggle}
          />
          <span className="text-xs text-[var(--text-secondary)]">
            {info?.running
              ? `Running on port ${info.port}`
              : "Stopped"}
          </span>
        </div>

        {/* URL + Copy */}
        {info?.running && info.url && (
          <div className="flex items-center gap-2">
            <code className="text-xs text-[var(--accent)] bg-[var(--bg-primary)] px-2 py-1 rounded border border-[var(--border-color)] truncate flex-1">
              {info.url}
            </code>
            <button
              onClick={handleCopyUrl}
              className="text-xs px-2 py-0.5 text-[var(--accent)] hover:text-[var(--accent-hover)] hover:bg-[var(--bg-primary)] rounded transition-colors"
            >
              {copied ? "Copied!" : "Copy URL"}
            </button>
          </div>
        )}

        {/* Token */}
        {info && (
          <div className="flex items-center gap-2">
            <span className="text-xs text-[var(--text-secondary)]">Token:</span>
            <code className="text-xs text-[var(--text-primary)] bg-[var(--bg-primary)] px-2 py-0.5 rounded border border-[var(--border-color)] truncate max-w-[160px]">
              {info.access_token ? `${info.access_token.slice(0, 12)}...` : "None"}
            </code>
            <button
              onClick={handleCopyToken}
              className="text-xs px-2 py-0.5 text-[var(--accent)] hover:text-[var(--accent-hover)] hover:bg-[var(--bg-primary)] rounded transition-colors"
            >
              Copy
            </button>
            <button
              onClick={handleRegenerate}
              className="text-xs px-2 py-0.5 text-[var(--warning)] hover:bg-[var(--bg-primary)] rounded transition-colors"
            >
              Regenerate
            </button>
          </div>
        )}
      </div>
    </div>
  );
}
