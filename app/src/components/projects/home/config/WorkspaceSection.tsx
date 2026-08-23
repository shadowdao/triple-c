import { useEffect, useState } from "react";
import { open } from "@tauri-apps/plugin-dialog";
import type { Project, ProjectPath } from "../../../../lib/types";
import Button from "../../../ui/Button";
import Field, { ConfigGroup, inputClass, monoInputClass } from "../../../ui/Field";

interface Props {
  project: Project;
  save: (patch: Partial<Project>) => Promise<boolean>;
  disabled: boolean;
}

export default function WorkspaceSection({ project, save, disabled }: Props) {
  const [name, setName] = useState(project.name);
  const [paths, setPaths] = useState<ProjectPath[]>(project.paths ?? []);

  useEffect(() => {
    setName(project.name);
    setPaths(project.paths ?? []);
  }, [project]);

  /**
   * Save only when every row is fully filled in.
   *
   * Both inputs save on blur, so tabbing from the host path to the mount name
   * fires a save with the name still empty. `update_project` now validates —
   * a half-filled row is refused — so the unconditional save turned an ordinary
   * keystroke into an error toast. A blank row is *not* incomplete: the
   * "+ Add folder" button adds one deliberately, and it is dropped on save.
   */
  const saveIfComplete = () => {
    const filled = paths.filter((p) => p.host_path.trim() || p.mount_name.trim());
    const halfFilled = filled.some((p) => !p.host_path.trim() || !p.mount_name.trim());
    if (halfFilled) return;
    return save({ paths });
  };

  return (
    <ConfigGroup
      title="Workspace"
      description="What this sandbox is called and which host folders it can see."
    >
      <Field
        label="Project name"
        hint="Shown in the sidebar and on terminal tabs."
      >
        {(id) => (
          <input
            id={id}
            value={name}
            onChange={(e) => setName(e.target.value)}
            onBlur={() => {
              const trimmed = name.trim();
              if (!trimmed) {
                setName(project.name);
                return;
              }
              if (trimmed !== project.name) save({ name: trimmed });
            }}
            onKeyDown={(e) => {
              if (e.key === "Enter") (e.target as HTMLInputElement).blur();
              if (e.key === "Escape") setName(project.name);
            }}
            className={inputClass}
          />
        )}
      </Field>

      <div>
        <span className="block text-[13px] font-medium text-[var(--text-primary)]">
          Folders
        </span>
        <p className="mt-0.5 mb-2 text-xs text-[var(--text-secondary)] leading-snug">
          Each host folder is mounted at <span className="font-mono">/workspace/&lt;name&gt;</span>{" "}
          inside the container.
        </p>

        <div className="space-y-3">
          {paths.map((pp, i) => (
            <div key={i} className="flex flex-col gap-1.5 sm:flex-row sm:items-center">
              <input
                value={pp.host_path}
                aria-label={`Folder ${i + 1} host path`}
                onChange={(e) => {
                  const updated = [...paths];
                  updated[i] = { ...updated[i], host_path: e.target.value };
                  setPaths(updated);
                }}
                onBlur={() => saveIfComplete()}
                placeholder="/path/to/folder"
                disabled={disabled}
                className={`flex-1 min-w-0 ${inputClass}`}
              />
              <div className="flex items-center gap-1.5">
                <Button
                  size="md"
                  disabled={disabled}
                  onClick={async () => {
                    const selected = await open({ directory: true, multiple: false });
                    if (typeof selected === "string") {
                      const updated = [...paths];
                      const basename =
                        selected.replace(/[/\\]$/, "").split(/[/\\]/).pop() || "";
                      updated[i] = {
                        host_path: selected,
                        mount_name: updated[i].mount_name || basename,
                      };
                      setPaths(updated);
                      save({ paths: updated });
                    }
                  }}
                >
                  Browse
                </Button>
                <span className="text-xs text-[var(--text-secondary)] font-mono flex-shrink-0">
                  /workspace/
                </span>
                <input
                  value={pp.mount_name}
                  aria-label={`Folder ${i + 1} mount name`}
                  onChange={(e) => {
                    const updated = [...paths];
                    updated[i] = { ...updated[i], mount_name: e.target.value };
                    setPaths(updated);
                  }}
                  onBlur={() => saveIfComplete()}
                  placeholder="name"
                  disabled={disabled}
                  className={`w-40 ${monoInputClass}`}
                />
                {paths.length > 1 && (
                  <Button
                    size="md"
                    variant="danger"
                    disabled={disabled}
                    aria-label={`Remove folder ${i + 1}`}
                    onClick={() => {
                      const updated = paths.filter((_, j) => j !== i);
                      setPaths(updated);
                      save({ paths: updated });
                    }}
                  >
                    Remove
                  </Button>
                )}
              </div>
            </div>
          ))}
        </div>

        <Button
          className="mt-2"
          disabled={disabled}
          onClick={() => setPaths([...paths, { host_path: "", mount_name: "" }])}
        >
          + Add folder
        </Button>
      </div>
    </ConfigGroup>
  );
}
