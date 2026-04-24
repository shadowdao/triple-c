import { useCallback, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import * as commands from "../lib/tauri-commands";
import type { InstallOptions } from "../lib/types";

export function useInstallHelper() {
  const [options, setOptions] = useState<InstallOptions | null>(null);

  const loadOptions = useCallback(async () => {
    try {
      const opts = await commands.detectInstallOptions();
      setOptions(opts);
      return opts;
    } catch {
      setOptions(null);
      return null;
    }
  }, []);

  const runInstall = useCallback(
    async (onProgress?: (line: string) => void) => {
      const unlisten = onProgress
        ? await listen<string>("docker-install-progress", (e) => onProgress(e.payload))
        : null;
      try {
        await commands.runDockerInstall();
      } finally {
        unlisten?.();
      }
    },
    [],
  );

  return { options, loadOptions, runInstall };
}
