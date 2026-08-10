import { useState } from "react";
import { useProjects } from "../../hooks/useProjects";
import ProjectRow from "./ProjectRow";
import AddProjectDialog from "./AddProjectDialog";
import Button from "../ui/Button";

export default function ProjectList() {
  const { projects } = useProjects();
  const [showAdd, setShowAdd] = useState(false);

  return (
    <div className="p-2">
      <div className="flex items-center justify-between px-1 py-1 mb-1.5">
        <span className="text-[11px] font-semibold uppercase tracking-wide text-[var(--text-secondary)]">
          Projects
        </span>
        <Button onClick={() => setShowAdd(true)} aria-label="Add project">
          + Add
        </Button>
      </div>

      {projects.length === 0 ? (
        <p className="px-1 text-xs text-[var(--text-secondary)]">
          No projects yet — use “+ Add” to create one.
        </p>
      ) : (
        <div className="flex flex-col gap-0.5">
          {projects.map((project) => (
            <ProjectRow key={project.id} project={project} />
          ))}
        </div>
      )}

      {showAdd && <AddProjectDialog onClose={() => setShowAdd(false)} />}
    </div>
  );
}
