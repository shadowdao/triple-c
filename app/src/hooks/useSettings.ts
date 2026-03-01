import { useCallback } from "react";
import { useShallow } from "zustand/react/shallow";
import { useAppState } from "../store/appState";
import * as commands from "../lib/tauri-commands";
import type { AppSettings } from "../lib/types";

export function useSettings() {
  const { appSettings, setAppSettings } = useAppState(
    useShallow(s => ({
      appSettings: s.appSettings,
      setAppSettings: s.setAppSettings,
    }))
  );

  const loadSettings = useCallback(async () => {
    try {
      const settings = await commands.getSettings();
      setAppSettings(settings);
      return settings;
    } catch (e) {
      console.error("Failed to load settings:", e);
      return null;
    }
  }, [setAppSettings]);

  const saveSettings = useCallback(
    async (settings: AppSettings) => {
      const updated = await commands.updateSettings(settings);
      setAppSettings(updated);
      return updated;
    },
    [setAppSettings],
  );

  return {
    appSettings,
    loadSettings,
    saveSettings,
  };
}
