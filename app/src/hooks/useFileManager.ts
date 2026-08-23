import { useState, useCallback } from "react";
import { save, open as openDialog } from "@tauri-apps/plugin-dialog";
import type { FileEntry } from "../lib/types";
import * as commands from "../lib/tauri-commands";

export function useFileManager(projectId: string) {
  const [currentPath, setCurrentPath] = useState("/workspace");
  const [entries, setEntries] = useState<FileEntry[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  /** Transient "uploading 3 files…" style note, shown beside the breadcrumb. */
  const [busy, setBusy] = useState<string | null>(null);

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

  /** Copy an entry out to a host path the user picks. */
  const downloadFile = useCallback(
    async (entry: FileEntry) => {
      try {
        const hostPath = await save({ defaultPath: entry.name });
        if (!hostPath) return;
        setError(null);
        await commands.downloadContainerFile(projectId, entry.path, hostPath);
      } catch (e) {
        setError(String(e));
      }
    },
    [projectId],
  );

  /**
   * Copy host files into the current directory. Shared by the Upload button and
   * the native drag-drop listener, so a dropped file and a picked one take the
   * same path — including the one refresh at the end rather than one per file.
   */
  const uploadPaths = useCallback(
    async (hostPaths: string[]) => {
      if (hostPaths.length === 0) return;
      setError(null);
      setBusy(`Uploading ${hostPaths.length} item${hostPaths.length > 1 ? "s" : ""}…`);
      const failures: string[] = [];
      try {
        for (const hostPath of hostPaths) {
          try {
            await commands.uploadFileToContainer(projectId, hostPath, currentPath);
          } catch (e) {
            failures.push(String(e));
          }
        }
      } finally {
        setBusy(null);
      }
      // Re-list first: `navigate` clears the error, so reporting before it
      // would wipe the very message the user needs.
      await navigate(currentPath);
      if (failures.length > 0) setError(failures.join(" · "));
    },
    [projectId, currentPath, navigate],
  );

  const uploadFile = useCallback(async () => {
    try {
      const selected = await openDialog({ multiple: true, directory: false });
      if (!selected) return;
      await uploadPaths(Array.isArray(selected) ? selected : [selected as string]);
    } catch (e) {
      setError(String(e));
    }
  }, [uploadPaths]);

  /**
   * Rename in place. `newName` is a bare name — Rust rejects anything with a
   * `/` in it, so this can never turn into a move. Resolves true on success so
   * the caller knows whether to leave edit mode.
   */
  const renameEntry = useCallback(
    async (entry: FileEntry, newName: string) => {
      const trimmed = newName.trim();
      if (!trimmed || trimmed === entry.name) return true;
      try {
        setError(null);
        await commands.renameContainerPath(projectId, entry.path, trimmed);
        await navigate(currentPath);
        return true;
      } catch (e) {
        setError(String(e));
        return false;
      }
    },
    [projectId, currentPath, navigate],
  );

  const createFolder = useCallback(
    async (name: string) => {
      const trimmed = name.trim();
      if (!trimmed) return true;
      try {
        setError(null);
        await commands.createContainerDirectory(projectId, currentPath, trimmed);
        await navigate(currentPath);
        return true;
      } catch (e) {
        setError(String(e));
        return false;
      }
    },
    [projectId, currentPath, navigate],
  );

  return {
    currentPath,
    entries,
    loading,
    error,
    busy,
    setError,
    navigate,
    goUp,
    refresh,
    downloadFile,
    uploadFile,
    uploadPaths,
    renameEntry,
    createFolder,
  };
}
