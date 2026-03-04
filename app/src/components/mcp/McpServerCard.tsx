import { useState, useEffect } from "react";
import type { McpServer, McpTransportType } from "../../lib/types";

interface Props {
  server: McpServer;
  onUpdate: (server: McpServer) => Promise<McpServer | void>;
  onRemove: (id: string) => Promise<void>;
}

export default function McpServerCard({ server, onUpdate, onRemove }: Props) {
  const [expanded, setExpanded] = useState(false);
  const [name, setName] = useState(server.name);
  const [transportType, setTransportType] = useState<McpTransportType>(server.transport_type);
  const [command, setCommand] = useState(server.command ?? "");
  const [args, setArgs] = useState(server.args.join(" "));
  const [envPairs, setEnvPairs] = useState<[string, string][]>(Object.entries(server.env));
  const [url, setUrl] = useState(server.url ?? "");
  const [headerPairs, setHeaderPairs] = useState<[string, string][]>(Object.entries(server.headers));

  useEffect(() => {
    setName(server.name);
    setTransportType(server.transport_type);
    setCommand(server.command ?? "");
    setArgs(server.args.join(" "));
    setEnvPairs(Object.entries(server.env));
    setUrl(server.url ?? "");
    setHeaderPairs(Object.entries(server.headers));
  }, [server]);

  const saveServer = async (patch: Partial<McpServer>) => {
    try {
      await onUpdate({ ...server, ...patch });
    } catch (err) {
      console.error("Failed to update MCP server:", err);
    }
  };

  const handleNameBlur = () => {
    if (name !== server.name) saveServer({ name });
  };

  const handleTransportChange = (t: McpTransportType) => {
    setTransportType(t);
    saveServer({ transport_type: t });
  };

  const handleCommandBlur = () => {
    saveServer({ command: command || null });
  };

  const handleArgsBlur = () => {
    const parsed = args.trim() ? args.trim().split(/\s+/) : [];
    saveServer({ args: parsed });
  };

  const handleUrlBlur = () => {
    saveServer({ url: url || null });
  };

  const saveEnv = (pairs: [string, string][]) => {
    const env: Record<string, string> = {};
    for (const [k, v] of pairs) {
      if (k.trim()) env[k.trim()] = v;
    }
    saveServer({ env });
  };

  const saveHeaders = (pairs: [string, string][]) => {
    const headers: Record<string, string> = {};
    for (const [k, v] of pairs) {
      if (k.trim()) headers[k.trim()] = v;
    }
    saveServer({ headers });
  };

  const inputCls = "w-full px-2 py-1 bg-[var(--bg-primary)] border border-[var(--border-color)] rounded text-xs text-[var(--text-primary)] focus:outline-none focus:border-[var(--accent)]";

  const transportBadge = {
    stdio: "Stdio",
    http: "HTTP",
    sse: "SSE",
  }[transportType];

  return (
    <div className="border border-[var(--border-color)] rounded bg-[var(--bg-primary)]">
      {/* Header */}
      <div className="flex items-center gap-2 px-3 py-2">
        <button
          onClick={() => setExpanded(!expanded)}
          className="flex-1 flex items-center gap-2 text-left min-w-0"
        >
          <span className="text-xs text-[var(--text-secondary)]">{expanded ? "\u25BC" : "\u25B6"}</span>
          <span className="text-sm font-medium truncate">{server.name}</span>
          <span className="text-xs px-1.5 py-0.5 rounded bg-[var(--bg-secondary)] text-[var(--text-secondary)]">
            {transportBadge}
          </span>
        </button>
        <button
          onClick={() => { if (confirm(`Remove MCP server "${server.name}"?`)) onRemove(server.id); }}
          className="text-xs px-2 py-0.5 text-[var(--error)] hover:bg-[var(--bg-secondary)] rounded transition-colors"
        >
          Remove
        </button>
      </div>

      {/* Expanded config */}
      {expanded && (
        <div className="px-3 pb-3 space-y-2 border-t border-[var(--border-color)] pt-2">
          {/* Name */}
          <div>
            <label className="block text-xs text-[var(--text-secondary)] mb-0.5">Name</label>
            <input
              value={name}
              onChange={(e) => setName(e.target.value)}
              onBlur={handleNameBlur}
              className={inputCls}
            />
          </div>

          {/* Transport type */}
          <div>
            <label className="block text-xs text-[var(--text-secondary)] mb-0.5">Transport</label>
            <div className="flex items-center gap-1">
              {(["stdio", "http", "sse"] as McpTransportType[]).map((t) => (
                <button
                  key={t}
                  onClick={() => handleTransportChange(t)}
                  className={`px-2 py-0.5 text-xs rounded transition-colors ${
                    transportType === t
                      ? "bg-[var(--accent)] text-white"
                      : "text-[var(--text-secondary)] hover:text-[var(--text-primary)] hover:bg-[var(--bg-secondary)]"
                  }`}
                >
                  {t === "stdio" ? "Stdio" : t === "http" ? "HTTP" : "SSE"}
                </button>
              ))}
            </div>
          </div>

          {/* Stdio fields */}
          {transportType === "stdio" && (
            <>
              <div>
                <label className="block text-xs text-[var(--text-secondary)] mb-0.5">Command</label>
                <input
                  value={command}
                  onChange={(e) => setCommand(e.target.value)}
                  onBlur={handleCommandBlur}
                  placeholder="npx"
                  className={inputCls}
                />
              </div>
              <div>
                <label className="block text-xs text-[var(--text-secondary)] mb-0.5">Arguments (space-separated)</label>
                <input
                  value={args}
                  onChange={(e) => setArgs(e.target.value)}
                  onBlur={handleArgsBlur}
                  placeholder="-y @modelcontextprotocol/server-filesystem /path"
                  className={inputCls}
                />
              </div>
              <KeyValueEditor
                label="Environment Variables"
                pairs={envPairs}
                onChange={(pairs) => { setEnvPairs(pairs); }}
                onSave={saveEnv}
              />
            </>
          )}

          {/* HTTP/SSE fields */}
          {(transportType === "http" || transportType === "sse") && (
            <>
              <div>
                <label className="block text-xs text-[var(--text-secondary)] mb-0.5">URL</label>
                <input
                  value={url}
                  onChange={(e) => setUrl(e.target.value)}
                  onBlur={handleUrlBlur}
                  placeholder="http://localhost:3000/mcp"
                  className={inputCls}
                />
              </div>
              <KeyValueEditor
                label="Headers"
                pairs={headerPairs}
                onChange={(pairs) => { setHeaderPairs(pairs); }}
                onSave={saveHeaders}
              />
            </>
          )}
        </div>
      )}
    </div>
  );
}

function KeyValueEditor({
  label,
  pairs,
  onChange,
  onSave,
}: {
  label: string;
  pairs: [string, string][];
  onChange: (pairs: [string, string][]) => void;
  onSave: (pairs: [string, string][]) => void;
}) {
  const inputCls = "flex-1 min-w-0 px-2 py-1 bg-[var(--bg-primary)] border border-[var(--border-color)] rounded text-xs text-[var(--text-primary)] focus:outline-none focus:border-[var(--accent)]";

  return (
    <div>
      <label className="block text-xs text-[var(--text-secondary)] mb-0.5">{label}</label>
      {pairs.map(([key, value], i) => (
        <div key={i} className="flex gap-1 items-center mb-1">
          <input
            value={key}
            onChange={(e) => {
              const updated = [...pairs] as [string, string][];
              updated[i] = [e.target.value, value];
              onChange(updated);
            }}
            onBlur={() => onSave(pairs)}
            placeholder="KEY"
            className={inputCls}
          />
          <span className="text-xs text-[var(--text-secondary)]">=</span>
          <input
            value={value}
            onChange={(e) => {
              const updated = [...pairs] as [string, string][];
              updated[i] = [key, e.target.value];
              onChange(updated);
            }}
            onBlur={() => onSave(pairs)}
            placeholder="value"
            className={inputCls}
          />
          <button
            onClick={() => {
              const updated = pairs.filter((_, j) => j !== i);
              onChange(updated);
              onSave(updated);
            }}
            className="flex-shrink-0 px-1.5 py-1 text-xs text-[var(--error)] hover:bg-[var(--bg-secondary)] rounded transition-colors"
          >
            x
          </button>
        </div>
      ))}
      <button
        onClick={() => {
          onChange([...pairs, ["", ""]]);
        }}
        className="text-xs text-[var(--accent)] hover:text-[var(--accent-hover)] transition-colors"
      >
        + Add
      </button>
    </div>
  );
}
