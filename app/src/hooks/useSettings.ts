import { useCallback } from "react";
import { useAppState } from "../store/appState";
import * as commands from "../lib/tauri-commands";

export function useSettings() {
  const { hasKey, setHasKey } = useAppState();

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

  return {
    hasKey,
    checkApiKey,
    saveApiKey,
    removeApiKey,
  };
}
