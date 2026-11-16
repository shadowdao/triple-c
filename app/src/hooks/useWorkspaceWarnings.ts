import { useEffect } from "react";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { useAppState } from "../store/appState";

/** `project_commands.rs`'s `workspace-warning`: an AWS workspace started, but
 *  its clone or setup script failed (or was still running after 35 minutes).
 *  `detail` carries the failed repos and the setup log's last lines. */
export interface WorkspaceWarningEvent {
  project_id: string;
  message: string;
  detail: string;
  /** Set when the warning is about something else than the start's own
   *  follow (`"ports"`): keeps it from replacing that toast. */
  topic?: string;
}

/**
 * App-wide: show `workspace-warning` as a warning toast. The start command
 * returns the stored project, so the start's warning arrives as this event
 * rather than in its result; one app-wide listener (not one per sidebar row)
 * shows it once, whichever surface started the project.
 */
export function useWorkspaceWarnings() {
  useEffect(() => {
    let cancelled = false;
    let unlisten: UnlistenFn | null = null;
    listen<WorkspaceWarningEvent>("workspace-warning", (event) => {
      const { project_id, message, detail, topic } = event.payload;
      useAppState.getState().pushToast({
        kind: "warning",
        message,
        detail,
        dedupeKey: topic
          ? `workspace-warning-${project_id}-${topic}`
          : `workspace-warning-${project_id}`,
      });
    })
      .then((fn) => {
        if (cancelled) fn();
        else unlisten = fn;
      })
      .catch((e) => console.error("Failed to listen for workspace warnings:", e));
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, []);
}
