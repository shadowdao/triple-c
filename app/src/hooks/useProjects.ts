import { useCallback } from "react";
import { useAppState } from "../store/appState";
import * as commands from "../lib/tauri-commands";

export function useProjects() {
  const {
    projects,
    selectedProjectId,
    setProjects,
    setSelectedProject,
    updateProjectInList,
    removeProjectFromList,
  } = useAppState();

  const selectedProject = projects.find((p) => p.id === selectedProjectId) ?? null;

  const refresh = useCallback(async () => {
    const list = await commands.listProjects();
    setProjects(list);
  }, [setProjects]);

  const add = useCallback(
    async (name: string, path: string) => {
      const project = await commands.addProject(name, path);
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
      await commands.removeProject(id);
      removeProjectFromList(id);
    },
    [removeProjectFromList],
  );

  const start = useCallback(
    async (id: string) => {
      const updated = await commands.startProjectContainer(id);
      updateProjectInList(updated);
      return updated;
    },
    [updateProjectInList],
  );

  const stop = useCallback(
    async (id: string) => {
      await commands.stopProjectContainer(id);
      const list = await commands.listProjects();
      setProjects(list);
    },
    [setProjects],
  );

  const rebuild = useCallback(
    async (id: string) => {
      const updated = await commands.rebuildProjectContainer(id);
      updateProjectInList(updated);
      return updated;
    },
    [updateProjectInList],
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
