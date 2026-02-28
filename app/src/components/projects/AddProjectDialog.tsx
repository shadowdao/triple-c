import { useState, useEffect, useRef, useCallback } from "react";
import { open } from "@tauri-apps/plugin-dialog";
import { useProjects } from "../../hooks/useProjects";
import type { ProjectPath } from "../../lib/types";

interface Props {
  onClose: () => void;
}

interface PathEntry {
  host_path: string;
  mount_name: string;
}

function basenameFromPath(p: string): string {
  return p.replace(/[/\\]$/, "").split(/[/\\]/).pop() || "";
}

export default function AddProjectDialog({ onClose }: Props) {
  const { add } = useProjects();
  const [name, setName] = useState("");
  const [pathEntries, setPathEntries] = useState<PathEntry[]>([
    { host_path: "", mount_name: "" },
  ]);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const nameInputRef = useRef<HTMLInputElement>(null);
  const overlayRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    nameInputRef.current?.focus();
  }, []);

  useEffect(() => {
    const handleKeyDown = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    document.addEventListener("keydown", handleKeyDown);
    return () => document.removeEventListener("keydown", handleKeyDown);
  }, [onClose]);

  const handleOverlayClick = useCallback(
    (e: React.MouseEvent<HTMLDivElement>) => {
      if (e.target === overlayRef.current) onClose();
    },
    [onClose],
  );

  const handleBrowse = async (index: number) => {
    const selected = await open({ directory: true, multiple: false });
    if (typeof selected === "string") {
      const basename = basenameFromPath(selected);
      const entries = [...pathEntries];
      entries[index] = {
        host_path: selected,
        mount_name: entries[index].mount_name || basename,
      };
      setPathEntries(entries);
      // Auto-fill project name from first folder
      if (!name && index === 0) {
        setName(basename);
      }
    }
  };

  const updateEntry = (
    index: number,
    field: keyof PathEntry,
    value: string,
  ) => {
    const entries = [...pathEntries];
    entries[index] = { ...entries[index], [field]: value };
    setPathEntries(entries);
  };

  const removeEntry = (index: number) => {
    setPathEntries(pathEntries.filter((_, i) => i !== index));
  };

  const addEntry = () => {
    setPathEntries([...pathEntries, { host_path: "", mount_name: "" }]);
  };

  const handleSubmit = async (e?: React.FormEvent) => {
    if (e) e.preventDefault();
    if (!name.trim()) {
      setError("Project name is required");
      return;
    }
    const validPaths: ProjectPath[] = pathEntries
      .filter((p) => p.host_path.trim())
      .map((p) => ({
        host_path: p.host_path.trim(),
        mount_name: p.mount_name.trim() || basenameFromPath(p.host_path),
      }));
    if (validPaths.length === 0) {
      setError("At least one folder path is required");
      return;
    }
    const mountNames = validPaths.map((p) => p.mount_name);
    if (new Set(mountNames).size !== mountNames.length) {
      setError("Mount names must be unique");
      return;
    }
    setLoading(true);
    setError(null);
    try {
      await add(name.trim(), validPaths);
      onClose();
    } catch (err) {
      setError(String(err));
    } finally {
      setLoading(false);
    }
  };

  return (
    <div
      ref={overlayRef}
      onClick={handleOverlayClick}
      className="fixed inset-0 bg-black/50 flex items-center justify-center z-50"
    >
      <div className="bg-[var(--bg-secondary)] border border-[var(--border-color)] rounded-lg p-6 w-[28rem] shadow-xl max-h-[80vh] overflow-y-auto">
        <h2 className="text-lg font-semibold mb-4">Add Project</h2>

        <form onSubmit={handleSubmit}>
          <label className="block text-sm text-[var(--text-secondary)] mb-1">
            Project Name
          </label>
          <input
            ref={nameInputRef}
            value={name}
            onChange={(e) => setName(e.target.value)}
            placeholder="my-project"
            className="w-full px-3 py-2 mb-3 bg-[var(--bg-primary)] border border-[var(--border-color)] rounded text-sm text-[var(--text-primary)] focus:outline-none focus:border-[var(--accent)]"
          />

          <label className="block text-sm text-[var(--text-secondary)] mb-1">
            Folders
          </label>
          <div className="space-y-2 mb-3">
            {pathEntries.map((entry, i) => (
              <div key={i} className="space-y-1 p-2 bg-[var(--bg-primary)] rounded border border-[var(--border-color)]">
                <div className="flex gap-1">
                  <input
                    value={entry.host_path}
                    onChange={(e) => updateEntry(i, "host_path", e.target.value)}
                    placeholder="/path/to/folder"
                    className="flex-1 px-2 py-1.5 bg-[var(--bg-secondary)] border border-[var(--border-color)] rounded text-xs text-[var(--text-primary)] focus:outline-none focus:border-[var(--accent)]"
                  />
                  <button
                    type="button"
                    onClick={() => handleBrowse(i)}
                    className="px-2 py-1.5 text-xs bg-[var(--bg-tertiary)] border border-[var(--border-color)] rounded hover:bg-[var(--border-color)] transition-colors"
                  >
                    Browse
                  </button>
                  {pathEntries.length > 1 && (
                    <button
                      type="button"
                      onClick={() => removeEntry(i)}
                      className="px-1.5 py-1.5 text-xs text-[var(--error)] hover:bg-[var(--bg-secondary)] rounded transition-colors"
                    >
                      x
                    </button>
                  )}
                </div>
                <div className="flex items-center gap-1">
                  <span className="text-xs text-[var(--text-secondary)] flex-shrink-0">/workspace/</span>
                  <input
                    value={entry.mount_name}
                    onChange={(e) => updateEntry(i, "mount_name", e.target.value)}
                    placeholder="mount-name"
                    className="flex-1 px-2 py-1 bg-[var(--bg-secondary)] border border-[var(--border-color)] rounded text-xs text-[var(--text-primary)] focus:outline-none focus:border-[var(--accent)] font-mono"
                  />
                </div>
              </div>
            ))}
          </div>
          <button
            type="button"
            onClick={addEntry}
            className="text-xs text-[var(--accent)] hover:text-[var(--accent-hover)] mb-4 transition-colors"
          >
            + Add folder
          </button>

          {error && (
            <div className="text-xs text-[var(--error)] mb-3">{error}</div>
          )}

          <div className="flex justify-end gap-2">
            <button
              type="button"
              onClick={onClose}
              className="px-4 py-2 text-sm text-[var(--text-secondary)] hover:text-[var(--text-primary)] transition-colors"
            >
              Cancel
            </button>
            <button
              type="submit"
              disabled={loading}
              className="px-4 py-2 text-sm bg-[var(--accent)] text-white rounded hover:bg-[var(--accent-hover)] disabled:opacity-50 transition-colors"
            >
              {loading ? "Adding..." : "Add Project"}
            </button>
          </div>
        </form>
      </div>
    </div>
  );
}
