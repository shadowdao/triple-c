import { useEffect } from "react";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { useAppState } from "../store/appState";
import type { Project } from "../lib/types";

/** `container_watch.rs`'s `ContainerExited`. */
export interface ContainerExitedEvent {
  project: Project;
  /**
   * `exited`: it died while the app was running — sessions were just cut off.
   * `found_stopped`: found down on the app's first look this run (it stopped
   * while the app was closed, e.g. a host reboot); nothing was interrupted.
   */
  reason: "exited" | "found_stopped";
}

/**
 * App-wide: apply `project-container-exited` — `container_watch.rs` marking a
 * project Stopped because its container exited without the app asking. Before
 * it, the project kept reading Running, with every terminal dead, until the
 * app was restarted.
 */
export function useProjectStatusEvents() {
  useEffect(() => {
    let cancelled = false;
    let unlisten: UnlistenFn | null = null;
    listen<ContainerExitedEvent>("project-container-exited", (event) => {
      const { project, reason } = event.payload;
      const state = useAppState.getState();
      state.updateProjectInList(project);
      if (reason !== "exited") return;
      state.pushToast({
        kind: "error",
        message: `The container for “${project.name}” stopped`,
        detail:
          "It exited without Triple-C stopping it — open sessions were disconnected. " +
          "Start the project again to reconnect.",
        dedupeKey: `container-exited-${project.id}`,
      });
    })
      .then((fn) => {
        if (cancelled) fn();
        else unlisten = fn;
      })
      .catch((e) => console.error("Failed to listen for container exits:", e));
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, []);
}
