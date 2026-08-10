import { useCallback, useRef, useState } from "react";
import type { Project } from "../lib/types";
import { useAppState } from "../store/appState";
import { useProjects } from "./useProjects";

export type SaveStatus = "idle" | "saving" | "saved" | "failed";

export interface SaveState {
  status: SaveStatus;
  error: string | null;
}

/**
 * Save-on-blur with a *visible* outcome. Previously every config write
 * swallowed its failure into `console.error`, which is silent data loss.
 */
export function useProjectSave(project: Project) {
  const { update } = useProjects();
  const pushToast = useAppState((s) => s.pushToast);
  const [state, setState] = useState<SaveState>({ status: "idle", error: null });
  const resetTimer = useRef<ReturnType<typeof setTimeout> | null>(null);

  const save = useCallback(
    async (patch: Partial<Project>) => {
      if (resetTimer.current) clearTimeout(resetTimer.current);
      setState({ status: "saving", error: null });
      try {
        await update({ ...project, ...patch });
        setState({ status: "saved", error: null });
        resetTimer.current = setTimeout(
          () => setState({ status: "idle", error: null }),
          2500,
        );
        return true;
      } catch (e) {
        const message = String(e);
        setState({ status: "failed", error: message });
        pushToast({
          kind: "error",
          message: `Could not save settings for “${project.name}”`,
          detail: message,
        });
        return false;
      }
    },
    [project, update, pushToast],
  );

  return { save, saveState: state };
}
