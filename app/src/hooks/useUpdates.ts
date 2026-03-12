import { useCallback, useRef } from "react";
import { useShallow } from "zustand/react/shallow";
import { useAppState } from "../store/appState";
import * as commands from "../lib/tauri-commands";

const CHECK_INTERVAL_MS = 24 * 60 * 60 * 1000; // 24 hours

export function useUpdates() {
  const {
    updateInfo,
    setUpdateInfo,
    imageUpdateInfo,
    setImageUpdateInfo,
    appVersion,
    setAppVersion,
    appSettings,
  } = useAppState(
    useShallow((s) => ({
      updateInfo: s.updateInfo,
      setUpdateInfo: s.setUpdateInfo,
      imageUpdateInfo: s.imageUpdateInfo,
      setImageUpdateInfo: s.setImageUpdateInfo,
      appVersion: s.appVersion,
      setAppVersion: s.setAppVersion,
      appSettings: s.appSettings,
    })),
  );

  const intervalRef = useRef<ReturnType<typeof setInterval> | null>(null);

  const loadVersion = useCallback(async () => {
    try {
      const version = await commands.getAppVersion();
      setAppVersion(version);
    } catch (e) {
      console.error("Failed to load app version:", e);
    }
  }, [setAppVersion]);

  const checkForUpdates = useCallback(async () => {
    try {
      const info = await commands.checkForUpdates();
      if (info) {
        // Respect dismissed version
        const dismissed = appSettings?.dismissed_update_version;
        if (dismissed && dismissed === info.version) {
          setUpdateInfo(null);
          return null;
        }
      }
      setUpdateInfo(info);
      return info;
    } catch (e) {
      console.error("Failed to check for updates:", e);
      return null;
    }
  }, [setUpdateInfo, appSettings?.dismissed_update_version]);

  const checkImageUpdate = useCallback(async () => {
    try {
      const info = await commands.checkImageUpdate();
      if (info) {
        // Respect dismissed image digest
        const dismissed = appSettings?.dismissed_image_digest;
        if (dismissed && dismissed === info.remote_digest) {
          setImageUpdateInfo(null);
          return null;
        }
      }
      setImageUpdateInfo(info);
      return info;
    } catch (e) {
      console.error("Failed to check for image updates:", e);
      return null;
    }
  }, [setImageUpdateInfo, appSettings?.dismissed_image_digest]);

  const startPeriodicCheck = useCallback(() => {
    if (intervalRef.current) return;
    intervalRef.current = setInterval(() => {
      if (appSettings?.auto_check_updates !== false) {
        checkForUpdates();
        checkImageUpdate();
      }
    }, CHECK_INTERVAL_MS);
    return () => {
      if (intervalRef.current) {
        clearInterval(intervalRef.current);
        intervalRef.current = null;
      }
    };
  }, [checkForUpdates, checkImageUpdate, appSettings?.auto_check_updates]);

  return {
    updateInfo,
    imageUpdateInfo,
    appVersion,
    loadVersion,
    checkForUpdates,
    checkImageUpdate,
    startPeriodicCheck,
  };
}
