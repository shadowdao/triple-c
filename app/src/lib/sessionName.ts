import type { Project, TerminalSession } from "./types";

/**
 * What a terminal session is called on screen.
 *
 * The rule used to be written twice inside `MainTabs.tsx` — once in `tabLabel`
 * for the drag ghost, once inline in `renderTab` — both local and neither
 * exported, so the two could disagree the moment either was edited. It is here
 * because a third caller (the note send-target picker) would have made that
 * three.
 *
 * A user-set name wins and is prefixed with the project, because a custom name
 * is usually about the work rather than the project and needs the context. The
 * `(bash)` marker only appears on the fallback: a session someone bothered to
 * name does not need to be told apart from its neighbours.
 */
export function sessionDisplayName(
  session: TerminalSession,
  project?: Project,
): string {
  const custom = project?.renamed_session_names?.[session.id];
  if (custom) return `${session.projectName}: ${custom}`;
  return (
    (session.sessionName ?? session.projectName) +
    (session.sessionType === "bash" ? " (bash)" : "")
  );
}
