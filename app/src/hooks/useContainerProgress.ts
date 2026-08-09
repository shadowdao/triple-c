import { useEffect } from "react";
import { listen } from "@tauri-apps/api/event";
import { useAppState } from "../store/appState";

/**
 * Single app-wide subscription to `container-progress`. Messages land in the
 * store keyed by project id so sidebar rows and Project Home can both show
 * inline progress — no blocking modal. The action that started the operation
 * clears the line when it settles (see `useProjectActions`).
 */
export function useContainerProgress() {
  useEffect(() => {
    const unlisten = listen<{ project_id: string; message: string }>(
      "container-progress",
      (event) => {
        useAppState
          .getState()
          .setContainerProgress(event.payload.project_id, event.payload.message);
      },
    );
    return () => {
      unlisten.then((f) => f()).catch(() => {});
    };
  }, []);
}
