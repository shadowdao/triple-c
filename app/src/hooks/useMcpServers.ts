import { useCallback } from "react";
import { useShallow } from "zustand/react/shallow";
import { useAppState } from "../store/appState";
import * as commands from "../lib/tauri-commands";
import type { McpServer } from "../lib/types";

export function useMcpServers() {
  const {
    mcpServers,
    setMcpServers,
    updateMcpServerInList,
    removeMcpServerFromList,
  } = useAppState(
    useShallow(s => ({
      mcpServers: s.mcpServers,
      setMcpServers: s.setMcpServers,
      updateMcpServerInList: s.updateMcpServerInList,
      removeMcpServerFromList: s.removeMcpServerFromList,
    }))
  );

  const refresh = useCallback(async () => {
    const list = await commands.listMcpServers();
    setMcpServers(list);
  }, [setMcpServers]);

  const add = useCallback(
    async (name: string) => {
      const server = await commands.addMcpServer(name);
      const list = await commands.listMcpServers();
      setMcpServers(list);
      return server;
    },
    [setMcpServers],
  );

  const update = useCallback(
    async (server: McpServer) => {
      const updated = await commands.updateMcpServer(server);
      updateMcpServerInList(updated);
      return updated;
    },
    [updateMcpServerInList],
  );

  const remove = useCallback(
    async (id: string) => {
      await commands.removeMcpServer(id);
      removeMcpServerFromList(id);
    },
    [removeMcpServerFromList],
  );

  return { mcpServers, refresh, add, update, remove };
}
