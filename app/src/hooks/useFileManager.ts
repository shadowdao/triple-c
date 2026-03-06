import { useState, useCallback } from "react";
import { save, open as openDialog } from "@tauri-apps/plugin-dialog";
import type { FileEntry } from "../lib/types";
import * as commands from "../lib/tauri-commands";

export function useFileManager(projectId: string) {
  const [currentPath, setCurrentPath] = useState("/workspace");
  const [entries, setEntries] = useState<FileEntry[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const navigate = useCallback(
    async (path: string) => {
      setLoading(true);
      setError(null);
      try {
        const result = await commands.listContainerFiles(projectId, path);
        setEntries(result);
        setCurrentPath(path);
      } catch (e) {
        setError(String(e));
      } finally {
        setLoading(false);
      }
    },
    [projectId],
  );

  const goUp = useCallback(() => {
    if (currentPath === "/") return;
    const parent = currentPath.replace(/\/[^/]+$/, "") || "/";
    navigate(parent);
  }, [currentPath, navigate]);

  const refresh = useCallback(() => {
    navigate(currentPath);
  }, [currentPath, navigate]);

  const downloadFile = useCallback(
    async (entry: FileEntry) => {
      try {
        const hostPath = await save({ defaultPath: entry.name });
        if (!hostPath) return;
        await commands.downloadContainerFile(projectId, entry.path, hostPath);
      } catch (e) {
        setError(String(e));
      }
    },
    [projectId],
  );

  const uploadFile = useCallback(async () => {
    try {
      const selected = await openDialog({ multiple: false, directory: false });
      if (!selected) return;
      await commands.uploadFileToContainer(projectId, selected as string, currentPath);
      await navigate(currentPath);
    } catch (e) {
      setError(String(e));
    }
  }, [projectId, currentPath, navigate]);

  return {
    currentPath,
    entries,
    loading,
    error,
    navigate,
    goUp,
    refresh,
    downloadFile,
    uploadFile,
  };
}
