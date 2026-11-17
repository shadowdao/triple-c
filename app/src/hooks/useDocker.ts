import { useCallback, useRef } from "react";
import { useShallow } from "zustand/react/shallow";
import { listen } from "@tauri-apps/api/event";
import { useAppState } from "../store/appState";
import * as commands from "../lib/tauri-commands";

export function useDocker() {
  const {
    dockerAvailable,
    dockerStatus,
    setDockerStatus,
    setDockerAvailable,
    imageExists,
    setImageExists,
  } = useAppState(
    useShallow(s => ({
      dockerAvailable: s.dockerAvailable,
      dockerStatus: s.dockerStatus,
      setDockerStatus: s.setDockerStatus,
      setDockerAvailable: s.setDockerAvailable,
      imageExists: s.imageExists,
      setImageExists: s.setImageExists,
    }))
  );

  const checkDocker = useCallback(async () => {
    try {
      const status = await commands.getDockerStatus();
      setDockerStatus(status);
      return status.state === "running";
    } catch {
      setDockerAvailable(false);
      return false;
    }
  }, [setDockerStatus, setDockerAvailable]);

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

  /**
   * Poll until Docker appears, then stop. Every tick stores the latest
   * `dockerStatus`, so the Docker dialog follows "not running" → "no
   * permission" → running live and closes itself.
   *
   * `onAvailable` runs exactly once, after `dockerAvailable` is set and the
   * image has been re-checked. It exists because a session that started before
   * the daemon was up otherwise never does the "Docker is up" work — status
   * reconciliation, interrupted-migration recovery, loading the project list.
   */
  const startDockerPolling = useCallback((onAvailable?: () => void | Promise<void>) => {
    // Don't start if already polling
    if (pollingRef.current) return () => {};

    const interval = setInterval(async () => {
      try {
        const status = await commands.getDockerStatus();
        if (status.state === "running") {
          clearInterval(interval);
          pollingRef.current = null;
          setDockerStatus(status);
          // Also check image once Docker is available
          try {
            const exists = await commands.checkImageExists();
            setImageExists(exists);
          } catch {
            setImageExists(false);
          }
          try {
            await onAvailable?.();
          } catch (e) {
            console.error("Docker-available callback failed:", e);
          }
        } else {
          setDockerStatus(status);
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
  }, [setDockerStatus, setImageExists]);

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
    dockerStatus,
    imageExists,
    checkDocker,
    checkImage,
    buildImage,
    pullImage,
    startDockerPolling,
  };
}
