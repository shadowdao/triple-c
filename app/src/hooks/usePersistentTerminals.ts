import { useEffect } from "react";
import { useAppState } from "../store/appState";
import { isAwsProject } from "../lib/awsWorkspace";
import { listWorkspaceTerminals } from "../lib/tauri-commands";
import { reattachTab, terminalsToReopen } from "../lib/persistentTerminals";
import type { Project } from "../lib/types";

/** Connections already synced this run, `<project id>|<target id>`: an AWS
 *  project's `container_id` is its tunnel's endpoint, new on every connect. */
const synced = new Set<string>();

/** For tests. */
export function resetPersistentTerminalSync() {
  synced.clear();
}

function connectionKey(p: Project): string | null {
  if (!isAwsProject(p) || p.status !== "running" || !p.container_id) return null;
  return `${p.id}|${p.container_id}`;
}

/**
 * Reopen a project's tabs once per connection to its AWS workspace (app
 * start with the workspace still running, a Start that attached to a running
 * task, a re-attach): the terminals this app opened that still run there
 * come back as tabs, in the order they were opened, without stealing focus.
 * The backend waits for a re-attach in progress before listing.
 */
export async function syncProjectTerminals(project: Project): Promise<number> {
  const running = await listWorkspaceTerminals(project.id);
  const state = useAppState.getState();
  const open = new Set(state.sessions.map((s) => s.id));
  const reopen = terminalsToReopen(running, open);
  for (const t of reopen) {
    state.addSession(reattachTab(project.id, project.name, t), { activate: false });
  }
  if (reopen.length > 0) {
    state.pushToast({
      kind: "info",
      message: `Reopened ${reopen.length} terminal${reopen.length === 1 ? "" : "s"} still running in “${project.name}”`,
      dedupeKey: `terminals-reopened-${project.id}`,
    });
  }
  return reopen.length;
}

/** App-wide: run [`syncProjectTerminals`] for each new AWS connection. */
export function usePersistentTerminals() {
  const projects = useAppState((s) => s.projects);
  useEffect(() => {
    for (const p of projects) {
      const key = connectionKey(p);
      if (!key || synced.has(key)) continue;
      synced.add(key);
      syncProjectTerminals(p).catch((e) => {
        // Not connected yet, or the workspace did not answer: a later
        // connection (new container_id) tries again.
        synced.delete(key);
        console.warn(`Could not list the terminals of “${p.name}”:`, e);
      });
    }
  }, [projects]);
}
