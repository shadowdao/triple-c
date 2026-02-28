import { useCallback } from "react";
import { useShallow } from "zustand/react/shallow";
import { useAppState } from "../store/appState";
import * as commands from "../lib/tauri-commands";
import type { AppSettings } from "../lib/types";

export function useSettings() {
  const { hasKey, setHasKey, appSettings, setAppSettings } = useAppState(
    useShallow(s => ({
      hasKey: s.hasKey,
      setHasKey: s.setHasKey,
      appSettings: s.appSettings,
      setAppSettings: s.setAppSettings,
    }))
  );

  const checkApiKey = useCallback(async () => {
    try {
      const has = await commands.hasApiKey();
      setHasKey(has);
      return has;
    } catch {
      setHasKey(false);
      return false;
    }
  }, [setHasKey]);

  const saveApiKey = useCallback(
    async (key: string) => {
      await commands.setApiKey(key);
      setHasKey(true);
    },
    [setHasKey],
  );

  const removeApiKey = useCallback(async () => {
    await commands.deleteApiKey();
    setHasKey(false);
  }, [setHasKey]);

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
    hasKey,
    checkApiKey,
    saveApiKey,
    removeApiKey,
    appSettings,
    loadSettings,
    saveSettings,
  };
}
