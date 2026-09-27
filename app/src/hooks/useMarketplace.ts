import { useCallback, useEffect, useState } from "react";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import * as commands from "../lib/tauri-commands";
import { useAppState } from "../store/appState";
import { isStale } from "../lib/marketplace";
import type {
  InstallScope,
  ItemUpdate,
  MarketplaceItemRef,
  MarketplaceSnapshot,
  SyncReport,
} from "../lib/types";

export interface MarketplaceApi {
  snapshots: MarketplaceSnapshot[];
  updates: ItemUpdate[];
  loading: boolean;
  /** Ids of marketplaces currently being fetched. */
  refreshing: string[];
  load: (opts?: { refreshStale?: boolean }) => Promise<void>;
  refresh: (marketplaceId?: string) => Promise<void>;
  /** Reload settings, projects and the update list after a mutation. */
  reloadState: () => Promise<void>;
  /** `commit`: the marketplace head the user reviewed (see `install_marketplace_item`). */
  install: (item: MarketplaceItemRef, scope: InstallScope, commit: string) => Promise<boolean>;
  uninstall: (item: MarketplaceItemRef, scope: InstallScope) => Promise<boolean>;
  setDisabled: (projectId: string, item: MarketplaceItemRef, disabled: boolean) => Promise<boolean>;
  /** `commit`: the head whose diff the user accepted. */
  update: (item: MarketplaceItemRef, scope: InstallScope, commit: string) => Promise<boolean>;
  forget: (marketplaceId: string) => Promise<boolean>;
  remove: (marketplaceId: string) => Promise<boolean>;
}

function errorText(e: unknown): string {
  return typeof e === "string" ? e : e instanceof Error ? e.message : String(e);
}

export function useMarketplace(): MarketplaceApi {
  const setAppSettings = useAppState((s) => s.setAppSettings);
  const setProjects = useAppState((s) => s.setProjects);
  const pushToast = useAppState((s) => s.pushToast);
  const [snapshots, setSnapshots] = useState<MarketplaceSnapshot[]>([]);
  const [updates, setUpdates] = useState<ItemUpdate[]>([]);
  const [loading, setLoading] = useState(false);
  const [refreshing, setRefreshing] = useState<string[]>([]);

  const merge = useCallback((fresh: MarketplaceSnapshot[]) => {
    setSnapshots((prev) => {
      const byId = new Map(prev.map((s) => [s.marketplace_id, s]));
      for (const s of fresh) byId.set(s.marketplace_id, s);
      return [...byId.values()];
    });
  }, []);

  const loadUpdates = useCallback(async () => {
    try {
      setUpdates(await commands.listMarketplaceUpdates());
    } catch (e) {
      console.error("Failed to list marketplace updates:", e);
    }
  }, []);

  const refresh = useCallback(
    async (marketplaceId?: string) => {
      const ids = marketplaceId ? [marketplaceId] : snapshots.map((s) => s.marketplace_id);
      setRefreshing((r) => [...new Set([...r, ...ids])]);
      try {
        merge(await commands.refreshMarketplaces(marketplaceId));
        await loadUpdates();
      } catch (e) {
        pushToast({ kind: "error", message: "Could not refresh the marketplace", detail: errorText(e) });
      } finally {
        setRefreshing((r) => r.filter((id) => !ids.includes(id)));
      }
    },
    [snapshots, merge, loadUpdates, pushToast],
  );

  const load = useCallback(
    async (opts: { refreshStale?: boolean } = {}) => {
      setLoading(true);
      try {
        const list = await commands.listMarketplaceSnapshots();
        setSnapshots(list);
        await loadUpdates();
        if (opts.refreshStale) {
          const now = Date.now();
          const stale = list.filter((s) => isStale(s, now)).map((s) => s.marketplace_id);
          if (stale.length > 0) {
            setRefreshing(stale);
            try {
              // One call per marketplace so one slow or failing repo does not hold up the rest.
              await Promise.all(
                stale.map(async (id) => {
                  try {
                    merge(await commands.refreshMarketplaces(id));
                  } finally {
                    setRefreshing((r) => r.filter((x) => x !== id));
                  }
                }),
              );
            } finally {
              await loadUpdates();
            }
          }
        }
      } catch (e) {
        pushToast({ kind: "error", message: "Could not load marketplaces", detail: errorText(e) });
      } finally {
        setLoading(false);
      }
    },
    [merge, loadUpdates, pushToast],
  );

  const reloadState = useCallback(async () => {
    const [settings, projects] = await Promise.all([commands.getSettings(), commands.listProjects()]);
    setAppSettings(settings);
    setProjects(projects);
    await loadUpdates();
  }, [setAppSettings, setProjects, loadUpdates]);

  /** Run a mutation; on failure toast it. Always resync local state afterwards. */
  const mutate = useCallback(
    async (label: string, run: () => Promise<unknown>): Promise<boolean> => {
      let ok = true;
      try {
        await run();
      } catch (e) {
        ok = false;
        pushToast({ kind: "error", message: label, detail: errorText(e) });
      }
      try {
        await reloadState();
      } catch (e) {
        console.error("Failed to reload after marketplace change:", e);
      }
      return ok;
    },
    [reloadState, pushToast],
  );

  return {
    snapshots,
    updates,
    loading,
    refreshing,
    load,
    refresh,
    reloadState,
    install: (item, scope, commit) =>
      mutate(`Could not install ${item.key}`, () => commands.installMarketplaceItem(item, scope, commit)),
    uninstall: (item, scope) =>
      mutate(`Could not remove ${item.key}`, () => commands.uninstallMarketplaceItem(item, scope)),
    setDisabled: (projectId, item, disabled) =>
      mutate(`Could not change ${item.key} for this project`, () =>
        commands.setGlobalItemDisabled(projectId, item, disabled),
      ),
    update: (item, scope, commit) =>
      mutate(`Could not update ${item.key}`, () => commands.updateMarketplaceItem(item, scope, commit)),
    forget: (marketplaceId) =>
      mutate("Could not forget those installs", () => commands.forgetMarketplaceInstalls(marketplaceId)),
    remove: async (marketplaceId) => {
      const ok = await mutate("Could not remove the marketplace", () =>
        commands.removeMarketplace(marketplaceId),
      );
      if (ok) setSnapshots((prev) => prev.filter((s) => s.marketplace_id !== marketplaceId));
      return ok;
    },
  };
}

interface SyncFinishedEvent {
  project_id: string;
  report: SyncReport;
}

/**
 * App-wide: toast when a marketplace sync (container start or "Apply now")
 * reports errors or skipped items. A clean sync is silent.
 */
export function useMarketplaceSyncToasts() {
  useEffect(() => {
    let cancelled = false;
    let unlisten: UnlistenFn | null = null;
    void listen<SyncFinishedEvent>("marketplace-sync-finished", (event) => {
      const { project_id, report } = event.payload;
      if (report.errors.length === 0 && report.skipped.length === 0) return;
      const state = useAppState.getState();
      const name = state.projects.find((p) => p.id === project_id)?.name ?? project_id;
      const lines = [
        ...report.errors,
        ...report.skipped.map((s) => `${s.item}: ${s.reason}`),
      ];
      state.pushToast({
        kind: report.errors.length > 0 ? "error" : "info",
        message: `Marketplace sync for “${name}” ${report.errors.length > 0 ? "had errors" : "skipped items"}`,
        detail: lines.join("\n"),
        dedupeKey: `marketplace-sync-${project_id}`,
      });
    }).then((fn) => {
      if (cancelled) fn();
      else unlisten = fn;
    });
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, []);
}
