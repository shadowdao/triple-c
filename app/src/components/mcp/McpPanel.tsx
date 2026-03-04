import { useState, useEffect } from "react";
import { useMcpServers } from "../../hooks/useMcpServers";
import McpServerCard from "./McpServerCard";

export default function McpPanel() {
  const { mcpServers, refresh, add, update, remove } = useMcpServers();
  const [newName, setNewName] = useState("");
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    refresh();
  }, []); // eslint-disable-line react-hooks/exhaustive-deps

  const handleAdd = async () => {
    const name = newName.trim();
    if (!name) return;
    setError(null);
    try {
      await add(name);
      setNewName("");
    } catch (e) {
      setError(String(e));
    }
  };

  return (
    <div className="space-y-3 p-2">
      <div>
        <h2 className="text-sm font-semibold text-[var(--text-primary)]">MCP Servers</h2>
        <p className="text-xs text-[var(--text-secondary)] mt-0.5">
          Define MCP servers globally, then enable them per-project.
        </p>
      </div>

      {/* Add new server */}
      <div className="flex gap-1">
        <input
          value={newName}
          onChange={(e) => setNewName(e.target.value)}
          onKeyDown={(e) => { if (e.key === "Enter") handleAdd(); }}
          placeholder="Server name..."
          className="flex-1 px-2 py-1 bg-[var(--bg-primary)] border border-[var(--border-color)] rounded text-xs text-[var(--text-primary)] focus:outline-none focus:border-[var(--accent)]"
        />
        <button
          onClick={handleAdd}
          disabled={!newName.trim()}
          className="px-3 py-1 text-xs bg-[var(--accent)] text-white rounded hover:bg-[var(--accent-hover)] disabled:opacity-50 transition-colors"
        >
          Add
        </button>
      </div>

      {error && (
        <div className="text-xs text-[var(--error)]">{error}</div>
      )}

      {/* Server list */}
      <div className="space-y-2">
        {mcpServers.length === 0 ? (
          <p className="text-xs text-[var(--text-secondary)] italic">
            No MCP servers configured.
          </p>
        ) : (
          mcpServers.map((server) => (
            <McpServerCard
              key={server.id}
              server={server}
              onUpdate={update}
              onRemove={remove}
            />
          ))
        )}
      </div>
    </div>
  );
}
