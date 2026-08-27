import { useCallback } from "react";
import { useShallow } from "zustand/react/shallow";
import { useAppState } from "../store/appState";
import * as commands from "../lib/tauri-commands";
import type { ProjectPath, ProjectStatus } from "../lib/types";

export function useProjects() {
  const {
    projects,
    selectedProjectId,
    setProjects,
    setSelectedProject,
    updateProjectInList,
    removeProjectFromList,
  } = useAppState(
    useShallow(s => ({
      projects: s.projects,
      selectedProjectId: s.selectedProjectId,
      setProjects: s.setProjects,
      setSelectedProject: s.setSelectedProject,
      updateProjectInList: s.updateProjectInList,
      removeProjectFromList: s.removeProjectFromList,
    }))
  );

  const selectedProject = projects.find((p) => p.id === selectedProjectId) ?? null;

  const refresh = useCallback(async () => {
    const list = await commands.listProjects();
    setProjects(list);
  }, [setProjects]);

  const add = useCallback(
    async (name: string, paths: ProjectPath[]) => {
      const project = await commands.addProject(name, paths);
      // Refresh from backend to avoid stale closure issues
      const list = await commands.listProjects();
      setProjects(list);
      setSelectedProject(project.id);
      return project;
    },
    [setProjects, setSelectedProject],
  );

  const remove = useCallback(
    async (id: string) => {
      const report = await commands.removeProject(id);
      removeProjectFromList(id);
      return report;
    },
    [removeProjectFromList],
  );

  const setOptimisticStatus = useCallback(
    (id: string, status: "starting" | "stopping") => {
      const { projects } = useAppState.getState();
      const project = projects.find((p) => p.id === id);
      if (project) {
        updateProjectInList({ ...project, status });
      }
    },
    [updateProjectInList],
  );

  /**
   * Paint the optimistic status, run the command, and **put the status back if
   * the command never happened.**
   *
   * The optimistic write exists so a click moves the row immediately. It used
   * to be safe to leave in place on failure, because the only way these three
   * commands could fail was after the backend had already started changing
   * things — so a stale "starting" was at worst premature.
   *
   * That stopped being true when every lifecycle command started taking the
   * per-project lock and failing fast: a compaction, a reset or another start
   * holding the project now refuses `start`, `stop` **and** `rebuild` before
   * one byte of state changes. `stop` in particular could not fail this way at
   * all before — it took no exclusion. The optimistic paint then has nothing to
   * become, and `isTransitioning` disables both Start and Stop, so the row is
   * stuck: the only thing that clears it is `reconcileProjectStatuses`, which
   * runs once, from `App.tsx`, when Docker first appears. A restart.
   *
   * Re-reading the list is preferred over restoring what was on screen, because
   * a refusal is not the only way these throw — a start that dies half-way
   * really has changed the world, and `listProjects` is the thing that knows.
   * The captured status is only the fallback for when that call fails too:
   * leaving the row transitioning is the one outcome the user cannot get out
   * of, so it must not be what a second failure lands on.
   */
  const withOptimisticStatus = useCallback(
    async <T,>(
      id: string,
      status: "starting" | "stopping",
      run: () => Promise<T>,
    ): Promise<T> => {
      const previous: ProjectStatus | null =
        useAppState.getState().projects.find((p) => p.id === id)?.status ?? null;
      setOptimisticStatus(id, status);
      try {
        return await run();
      } catch (e) {
        try {
          setProjects(await commands.listProjects());
        } catch {
          const project = useAppState.getState().projects.find((p) => p.id === id);
          if (project && previous) updateProjectInList({ ...project, status: previous });
        }
        // Rethrown unchanged: `useProjectActions` is what turns this into a
        // toast, and it must still see the original failure.
        throw e;
      }
    },
    [setOptimisticStatus, setProjects, updateProjectInList],
  );

  const start = useCallback(
    (id: string) =>
      withOptimisticStatus(id, "starting", async () => {
        const updated = await commands.startProjectContainer(id);
        updateProjectInList(updated);
        return updated;
      }),
    [updateProjectInList, withOptimisticStatus],
  );

  const stop = useCallback(
    (id: string) =>
      withOptimisticStatus(id, "stopping", async () => {
        await commands.stopProjectContainer(id);
        const list = await commands.listProjects();
        setProjects(list);
      }),
    [setProjects, withOptimisticStatus],
  );

  const rebuild = useCallback(
    (id: string) =>
      withOptimisticStatus(id, "starting", async () => {
        const outcome = await commands.rebuildProjectContainer(id);
        updateProjectInList(outcome.project);
        return outcome;
      }),
    [updateProjectInList, withOptimisticStatus],
  );

  const update = useCallback(
    async (project: Parameters<typeof commands.updateProject>[0]) => {
      const updated = await commands.updateProject(project);
      updateProjectInList(updated);
      return updated;
    },
    [updateProjectInList],
  );

  return {
    projects,
    selectedProject,
    selectedProjectId,
    setSelectedProject,
    refresh,
    add,
    remove,
    start,
    stop,
    rebuild,
    update,
  };
}
