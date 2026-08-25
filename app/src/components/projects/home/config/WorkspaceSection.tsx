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

/** Whether two folder lists are the same rows in the same order. */
function sameRows(a: ProjectPath[], b: ProjectPath[]): boolean {
  return (
    a.length === b.length &&
    a.every((row, i) => row.host_path === b[i].host_path && row.mount_name === b[i].mount_name)
  );
}

export default function WorkspaceSection({ project, save, disabled }: Props) {
  const [name, setName] = useState(project.name);
  const [paths, setPaths] = useState<ProjectPath[]>(project.paths ?? []);

  useEffect(() => {
    setName(project.name);
    setPaths(project.paths ?? []);
  }, [project]);

  /**
   * Persist a folder list, minus the rows that are only in it because the UI
   * put them there.
   *
   * **The blank row must never reach the store.** "+ Add folder" inserts
   * `{host_path: "", mount_name: ""}` deliberately, and `create_container`
   * mounts every stored row unfiltered — a stored blank one becomes
   * `{"Target": "/workspace/", "Source": ""}`, which the daemon rejects with
   * `field Source must not be empty`. The project then cannot be started or
   * recreated at all, from a click and a blur. `AddProjectDialog` has always
   * filtered this; this section computed the filtered list and then saved the
   * unfiltered one.
   *
   * Every save goes through here for that reason — Browse and Remove write the
   * list too, and either can be holding a blank row from an earlier click.
   */
  const persist = (rows: ProjectPath[]) => {
    const filled = rows.filter((p) => p.host_path.trim() || p.mount_name.trim());
    return save({ paths: filled });
  };

  /**
   * Save only when every row is fully filled in.
   *
   * Both inputs save on blur, so tabbing from the host path to the mount name
   * fires a save with the name still empty. `update_project` now validates —
   * a half-filled row is refused — so the unconditional save turned an ordinary
   * keystroke into an error toast. A blank row is *not* incomplete: the
   * "+ Add folder" button adds one deliberately, and it is dropped on save.
   *
   * A blur that changed nothing saves nothing, which is what keeps the blank
   * row on screen while it is being filled in: persisting the filtered list
   * would round-trip through `project` and take the empty row away under the
   * cursor.
   */
  const saveIfComplete = () => {
    const filled = paths.filter((p) => p.host_path.trim() || p.mount_name.trim());
    const halfFilled = filled.some((p) => !p.host_path.trim() || !p.mount_name.trim());
    if (halfFilled) return;
    if (sameRows(filled, project.paths ?? [])) return;
    return persist(paths);
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
                      persist(updated);
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
                      persist(updated);
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
