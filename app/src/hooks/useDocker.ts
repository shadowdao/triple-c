import { useCallback } from "react";
import { listen } from "@tauri-apps/api/event";
import { useAppState } from "../store/appState";
import * as commands from "../lib/tauri-commands";

export function useDocker() {
  const {
    dockerAvailable,
    setDockerAvailable,
    imageExists,
    setImageExists,
  } = useAppState();

  const checkDocker = useCallback(async () => {
    try {
      const available = await commands.checkDocker();
      setDockerAvailable(available);
      return available;
    } catch {
      setDockerAvailable(false);
      return false;
    }
  }, [setDockerAvailable]);

  const checkImage = useCallback(async () => {
    try {
      const exists = await commands.checkImageExists();
      setImageExists(exists);
      return exists;
    } catch {
      setImageExists(false);
      return false;
    }
  }, [setImageExists]);

  const buildImage = useCallback(
    async (onProgress?: (msg: string) => void) => {
      const unlisten = onProgress
        ? await listen<string>("image-build-progress", (event) => {
            onProgress(event.payload);
          })
        : null;

      try {
        await commands.buildImage();
        setImageExists(true);
      } finally {
        unlisten?.();
      }
    },
    [setImageExists],
  );

  const pullImage = useCallback(
    async (imageName: string, onProgress?: (msg: string) => void) => {
      const unlisten = onProgress
        ? await listen<string>("image-pull-progress", (event) => {
            onProgress(event.payload);
          })
        : null;

      try {
        await commands.pullImage(imageName);
        setImageExists(true);
      } finally {
        unlisten?.();
      }
    },
    [setImageExists],
  );

  return {
    dockerAvailable,
    imageExists,
    checkDocker,
    checkImage,
    buildImage,
    pullImage,
  };
}
