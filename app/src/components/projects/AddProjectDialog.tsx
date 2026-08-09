import { useId, useRef, useState } from "react";
import { open } from "@tauri-apps/plugin-dialog";
import { useProjects } from "../../hooks/useProjects";
import type { ProjectPath } from "../../lib/types";
import Modal from "../ui/Modal";
import Button from "../ui/Button";
import { inputClass, monoInputClass } from "../ui/Field";

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
  const formId = useId();

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

  const updateEntry = (index: number, field: keyof PathEntry, value: string) => {
    const entries = [...pathEntries];
    entries[index] = { ...entries[index], [field]: value };
    setPathEntries(entries);
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
    <Modal
      title="Add Project"
      onClose={onClose}
      widthClassName="w-[30rem]"
      initialFocusRef={nameInputRef}
      footer={
        <>
          <Button size="md" variant="ghost" onClick={onClose}>
            Cancel
          </Button>
          <Button size="md" variant="primary" type="submit" form={formId} disabled={loading}>
            {loading ? "Adding…" : "Add Project"}
          </Button>
        </>
      }
    >
      <form id={formId} onSubmit={handleSubmit} className="space-y-4">
        <div>
          <label
            htmlFor={`${formId}-name`}
            className="block text-[13px] font-medium mb-1"
          >
            Project name
          </label>
          <input
            id={`${formId}-name`}
            ref={nameInputRef}
            value={name}
            onChange={(e) => setName(e.target.value)}
            placeholder="my-project"
            className={inputClass}
          />
        </div>

        <div>
          <span className="block text-[13px] font-medium mb-1">Folders</span>
          <div className="space-y-2">
            {pathEntries.map((entry, i) => (
              <div
                key={i}
                className="space-y-1.5 p-2 bg-[var(--bg-primary)] rounded-[var(--radius-control)] border border-[var(--border-color)]"
              >
                <div className="flex gap-1.5">
                  <input
                    value={entry.host_path}
                    onChange={(e) => updateEntry(i, "host_path", e.target.value)}
                    placeholder="/path/to/folder"
                    aria-label={`Folder ${i + 1} host path`}
                    className={inputClass}
                  />
                  <Button size="md" onClick={() => handleBrowse(i)}>
                    Browse
                  </Button>
                  {pathEntries.length > 1 && (
                    <Button
                      size="md"
                      variant="danger"
                      aria-label={`Remove folder ${i + 1}`}
                      onClick={() =>
                        setPathEntries(pathEntries.filter((_, j) => j !== i))
                      }
                    >
                      Remove
                    </Button>
                  )}
                </div>
                <div className="flex items-center gap-1.5">
                  <span className="text-xs text-[var(--text-secondary)] flex-shrink-0 font-mono">
                    /workspace/
                  </span>
                  <input
                    value={entry.mount_name}
                    onChange={(e) => updateEntry(i, "mount_name", e.target.value)}
                    placeholder="mount-name"
                    aria-label={`Folder ${i + 1} mount name`}
                    className={monoInputClass}
                  />
                </div>
              </div>
            ))}
          </div>
          <Button
            className="mt-2"
            onClick={() =>
              setPathEntries([...pathEntries, { host_path: "", mount_name: "" }])
            }
          >
            + Add folder
          </Button>
        </div>

        {error && (
          <div
            role="alert"
            className="px-2 py-1.5 text-xs text-[var(--error)] bg-[var(--error-muted)] border border-[var(--error)]/30 rounded-[var(--radius-control)]"
          >
            {error}
          </div>
        )}
      </form>
    </Modal>
  );
}
