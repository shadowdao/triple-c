import type {
  ItemKind,
  ItemUpdate,
  MarketplaceInstall,
  MarketplaceItemRef,
  MarketplaceSnapshot,
  Project,
} from "./types";

/** How a project relates to one marketplace item. */
export type ProjectItemState =
  | "none"
  | "inherited"
  | "opted_out"
  | "project"
  | "project_pinned_differently";

export const KIND_ORDER: ItemKind[] = ["agent", "skill", "command", "hook", "plugin"];

export const KIND_LABELS: Record<ItemKind, string> = {
  agent: "Agents",
  skill: "Skills",
  command: "Commands",
  hook: "Hooks",
  plugin: "Plugins",
};

/** A marketplace is refreshed when its tab opens if the last fetch is older than this. */
export const STALE_AFTER_MS = 15 * 60 * 1000;

/** Updates that can actually be applied (not refused as invalid at head). */
export const applicableUpdates = (updates: ItemUpdate[]) => updates.filter((u) => u.invalid_at_head === null);

export const itemRefKey = (r: MarketplaceItemRef) => `${r.marketplace_id}/${r.kind}/${r.key}`;

/** Same shape as the item strings in a `SyncReport`. */
export const formatItemRef = (r: MarketplaceItemRef) => `${r.kind}:${r.key}`;

const sameItem = (a: MarketplaceItemRef, b: MarketplaceItemRef) =>
  a.marketplace_id === b.marketplace_id && a.kind === b.kind && a.key === b.key;

export function projectItemState(
  item: MarketplaceItemRef,
  globalInstalls: MarketplaceInstall[],
  project: Project,
): ProjectItemState {
  const own = project.marketplace_installs.find((i) => sameItem(i, item));
  const global = globalInstalls.find((i) => sameItem(i, item));
  if (own) {
    return global && global.commit !== own.commit ? "project_pinned_differently" : "project";
  }
  if (!global) return "none";
  return project.marketplace_disabled.some((d) => sameItem(d, item)) ? "opted_out" : "inherited";
}

/** Mirror of the backend's `effective_installs`, tagged with where each install comes from. */
export function effectiveInstalls(
  globalInstalls: MarketplaceInstall[],
  project: Project,
): (MarketplaceInstall & { source: "global" | "project" })[] {
  const byKey = new Map<string, MarketplaceInstall & { source: "global" | "project" }>();
  for (const g of globalInstalls) {
    if (project.marketplace_disabled.some((d) => sameItem(d, g))) continue;
    byKey.set(itemRefKey(g), { ...g, source: "global" });
  }
  for (const p of project.marketplace_installs) {
    byKey.set(itemRefKey(p), { ...p, source: "project" });
  }
  return [...byKey.entries()]
    .sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0))
    .map(([, v]) => v);
}

export function isStale(snapshot: MarketplaceSnapshot, now: number): boolean {
  if (!snapshot.fetched_at) return true;
  const at = Date.parse(snapshot.fetched_at);
  if (Number.isNaN(at)) return true;
  return now - at > STALE_AFTER_MS;
}
