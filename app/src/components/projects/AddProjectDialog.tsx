import { useState } from "react";
import { open } from "@tauri-apps/plugin-dialog";
import { useProjects } from "../../hooks/useProjects";

interface Props {
  onClose: () => void;
}

export default function AddProjectDialog({ onClose }: Props) {
  const { add } = useProjects();
  const [name, setName] = useState("");
  const [path, setPath] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);

  const handleBrowse = async () => {
    const selected = await open({ directory: true, multiple: false });
    if (typeof selected === "string") {
      setPath(selected);
      if (!name) {
        const parts = selected.replace(/[/\\]$/, "").split(/[/\\]/);
        setName(parts[parts.length - 1]);
      }
    }
  };

  const handleSubmit = async () => {
    if (!name.trim() || !path.trim()) {
      setError("Name and path are required");
      return;
    }
    setLoading(true);
    setError(null);
    try {
      await add(name.trim(), path.trim());
      onClose();
    } catch (e) {
      setError(String(e));
    } finally {
      setLoading(false);
    }
  };

  return (
    <div className="fixed inset-0 bg-black/50 flex items-center justify-center z-50">
      <div className="bg-[var(--bg-secondary)] border border-[var(--border-color)] rounded-lg p-6 w-96 shadow-xl">
        <h2 className="text-lg font-semibold mb-4">Add Project</h2>

        <label className="block text-sm text-[var(--text-secondary)] mb-1">
          Project Name
        </label>
        <input
          value={name}
          onChange={(e) => setName(e.target.value)}
          placeholder="my-project"
          className="w-full px-3 py-2 mb-3 bg-[var(--bg-primary)] border border-[var(--border-color)] rounded text-sm text-[var(--text-primary)] focus:outline-none focus:border-[var(--accent)]"
        />

        <label className="block text-sm text-[var(--text-secondary)] mb-1">
          Project Path
        </label>
        <div className="flex gap-2 mb-4">
          <input
            value={path}
            onChange={(e) => setPath(e.target.value)}
            placeholder="/path/to/project"
            className="flex-1 px-3 py-2 bg-[var(--bg-primary)] border border-[var(--border-color)] rounded text-sm text-[var(--text-primary)] focus:outline-none focus:border-[var(--accent)]"
          />
          <button
            onClick={handleBrowse}
            className="px-3 py-2 bg-[var(--bg-tertiary)] border border-[var(--border-color)] rounded text-sm hover:bg-[var(--border-color)] transition-colors"
          >
            Browse
          </button>
        </div>

        {error && (
          <div className="text-xs text-[var(--error)] mb-3">{error}</div>
        )}

        <div className="flex justify-end gap-2">
          <button
            onClick={onClose}
            className="px-4 py-2 text-sm text-[var(--text-secondary)] hover:text-[var(--text-primary)] transition-colors"
          >
            Cancel
          </button>
          <button
            onClick={handleSubmit}
            disabled={loading}
            className="px-4 py-2 text-sm bg-[var(--accent)] text-white rounded hover:bg-[var(--accent-hover)] disabled:opacity-50 transition-colors"
          >
            {loading ? "Adding..." : "Add Project"}
          </button>
        </div>
      </div>
    </div>
  );
}
