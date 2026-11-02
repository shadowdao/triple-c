import { useEffect } from "react";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { useAppState } from "../store/appState";
import type { Project } from "../lib/types";

/**
 * App-wide: apply status changes the backend makes on its own. Today that is
 * `container_watch.rs` marking a project Stopped when its container exited
 * without the app asking — before it, the project kept reading Running, with
 * every terminal dead, until the app was restarted.
 */
export function useProjectStatusEvents() {
  useEffect(() => {
    let cancelled = false;
    let unlisten: UnlistenFn | null = null;
    void listen<Project>("project-status-changed", (event) => {
      const project = event.payload;
      const state = useAppState.getState();
      state.updateProjectInList(project);
      if (project.status === "stopped") {
        state.pushToast({
          kind: "error",
          message: `The container for “${project.name}” stopped`,
          detail:
            "It exited without Triple-C stopping it — open sessions were disconnected. " +
            "Start the project again to reconnect.",
          dedupeKey: `container-exited-${project.id}`,
        });
      }
    }).then((fn) => {
      if (cancelled) fn();
      else unlisten = fn;
    });
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, []);
}
