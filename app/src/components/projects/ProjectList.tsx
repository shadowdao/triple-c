import { useState } from "react";
import { useProjects } from "../../hooks/useProjects";
import ProjectCard from "./ProjectCard";
import AddProjectDialog from "./AddProjectDialog";

export default function ProjectList() {
  const { projects } = useProjects();
  const [showAdd, setShowAdd] = useState(false);

  return (
    <div className="p-3">
      <div className="flex items-center justify-between px-2 py-1 mb-2">
        <span className="text-xs font-semibold uppercase text-[var(--text-secondary)]">
          Projects
        </span>
        <button
          onClick={() => setShowAdd(true)}
          className="text-lg leading-none text-[var(--text-secondary)] hover:text-[var(--accent)] transition-colors"
          title="Add project"
        >
          +
        </button>
      </div>

      {projects.length === 0 ? (
        <p className="px-2 text-sm text-[var(--text-secondary)]">
          No projects yet. Click + to add one.
        </p>
      ) : (
        <div className="flex flex-col gap-1">
          {projects.map((project) => (
            <ProjectCard key={project.id} project={project} />
          ))}
        </div>
      )}

      {showAdd && <AddProjectDialog onClose={() => setShowAdd(false)} />}
    </div>
  );
}
