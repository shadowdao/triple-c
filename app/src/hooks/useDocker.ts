import { useCallback, useRef } from "react";
import { useShallow } from "zustand/react/shallow";
import { listen } from "@tauri-apps/api/event";
import { useAppState } from "../store/appState";
import * as commands from "../lib/tauri-commands";

export function useDocker() {
  const {
    dockerAvailable,
    setDockerAvailable,
    imageExists,
    setImageExists,
  } = useAppState(
    useShallow(s => ({
      dockerAvailable: s.dockerAvailable,
      setDockerAvailable: s.setDockerAvailable,
      imageExists: s.imageExists,
      setImageExists: s.setImageExists,
    }))
  );

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

  const pollingRef = useRef<ReturnType<typeof setInterval> | null>(null);

  const startDockerPolling = useCallback(() => {
    // Don't start if already polling
    if (pollingRef.current) return () => {};

    const interval = setInterval(async () => {
      try {
        const available = await commands.checkDocker();
        if (available) {
          clearInterval(interval);
          pollingRef.current = null;
          setDockerAvailable(true);
          // Also check image once Docker is available
          try {
            const exists = await commands.checkImageExists();
            setImageExists(exists);
          } catch {
            setImageExists(false);
          }
        }
      } catch {
        // Still not available, keep polling
      }
    }, 5000);

    pollingRef.current = interval;
    return () => {
      clearInterval(interval);
      pollingRef.current = null;
    };
  }, [setDockerAvailable, setImageExists]);

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
    startDockerPolling,
  };
}
