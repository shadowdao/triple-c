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
  /** AWS: why ECS says the task stopped (`StopNote`); absent on Docker. */
  stop?: StopNote | null;
}

/** `container_watch.rs`'s `StopNote`. */
export interface StopNote {
  /** `user`: someone stopped it (console, CLI, another machine) — no toast. */
  kind: "spot" | "idle" | "lifetime" | "failed" | "user" | "exited";
  message: string | null;
}

/** The toast for an exit, or null for none. */
export function exitToast(
  name: string,
  reason: ContainerExitedEvent["reason"],
  stop: StopNote | null | undefined,
): { kind: "error" | "info"; message: string; detail?: string } | null {
  if (stop?.kind === "user") return null;
  if (stop?.kind === "failed") {
    return {
      kind: "error",
      message: `The AWS workspace for “${name}” failed to start`,
      detail: stop.message ?? undefined,
    };
  }
  if (reason !== "exited") return null;
  switch (stop?.kind) {
    case "spot":
      return {
        kind: "info",
        message: `“${name}” stopped: Spot interruption`,
        detail:
          "AWS reclaimed the Spot capacity it ran on. Start the project again to get a new workspace.",
      };
    case "idle":
      return {
        kind: "info",
        message: `“${name}” stopped after being idle`,
        detail: "Its AWS workspace stopped itself to save cost. Start the project again to continue.",
      };
    case "lifetime":
      return {
        kind: "info",
        message: `“${name}” stopped: the environment's maximum workspace lifetime was reached`,
        detail:
          "Its AWS environment caps how long a workspace runs. With the WIP push on, unsaved work " +
          "was pushed (best effort) to a triple-c/wip/ branch. Start the project again to continue.",
      };
    default:
      return {
        kind: "error",
        message: `The container for “${name}” stopped`,
        detail:
          "It exited without Triple-C stopping it — open sessions were disconnected. " +
          "Start the project again to reconnect." +
          (stop?.message ? ` (${stop.message})` : ""),
      };
  }
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
      const { project, reason, stop } = event.payload;
      const state = useAppState.getState();
      state.updateProjectInList(project);
      const toast = exitToast(project.name, reason, stop);
      if (!toast) return;
      state.pushToast({ ...toast, dedupeKey: `container-exited-${project.id}` });
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
