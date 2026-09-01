import type { Project } from "../../../lib/types";
import NotesPanel from "../../notes/NotesPanel";

interface Props {
  project: Project;
}

/**
 * Notes as a Project Home sub-tab.
 *
 * The same panel the dock shows. This is the roomy view for writing; the dock
 * is the one that stays visible while the agent works.
 */
export default function NotesTab({ project }: Props) {
  return (
    <div className="h-full min-h-0">
      <NotesPanel projectId={project.id} />
    </div>
  );
}
