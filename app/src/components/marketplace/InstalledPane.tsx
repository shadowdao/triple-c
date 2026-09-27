import { useState } from "react";
import type { MarketplaceApi } from "../../hooks/useMarketplace";
import { useAppState } from "../../store/appState";
import { KIND_LABELS } from "../../lib/marketplace";
import { applyMarketplaceNow } from "../../lib/tauri-commands";
import type { InstallScope, ItemUpdate, MarketplaceInstall, MarketplaceItemRef } from "../../lib/types";
import Button from "../ui/Button";
import UpdateDiffModal from "./UpdateDiffModal";

function errorText(e: unknown): string {
  return typeof e === "string" ? e : e instanceof Error ? e.message : String(e);
}

interface Pending {
  install: MarketplaceInstall;
  update: ItemUpdate;
  scope: InstallScope;
  scopeLabel: string;
  /** Hooks only: what the hook runs at `update.head`, captured with it. */
  hookCommands: string[] | undefined;
}

export default function InstalledPane({ mp }: { mp: MarketplaceApi }) {
  const appSettings = useAppState((s) => s.appSettings);
  const projects = useAppState((s) => s.projects);
  const pushToast = useAppState((s) => s.pushToast);
  const [pending, setPending] = useState<Pending | null>(null);
  const [applying, setApplying] = useState(false);

  const marketplaces = appSettings?.marketplaces ?? [];
  const known = new Set(marketplaces.map((m) => m.id));
  const nameOf = (id: string) => marketplaces.find((m) => m.id === id)?.name ?? id;
  const globalInstalls = appSettings?.global_marketplace_installs ?? [];

  /** The update for this very install: same item *and* pinned at the same
   *  commit (PR review #8) — updates are listed per (item, pinned commit). */
  const updateFor = (i: MarketplaceInstall) =>
    mp.updates.find(
      (u) =>
        u.item.marketplace_id === i.marketplace_id &&
        u.item.kind === i.kind &&
        u.item.key === i.key &&
        u.pinned === i.commit,
    );

  /** Hooks only (spec §3, preflight F8): the rendered commands at head, so the
   *  diff review shows what a hook will run after the update, not just the
   *  raw `hook.json` diff. */
  const hookCommandsFor = ({ item, head }: ItemUpdate): string[] | undefined => {
    if (item.kind !== "hook") return undefined;
    const snap = mp.snapshots.find((s) => s.marketplace_id === item.marketplace_id);
    // Only when the snapshot is at the head being reviewed; otherwise they
    // would describe a different version than the diff.
    if (snap?.head_commit !== head) return undefined;
    return snap.items.find((it) => it.kind === "hook" && it.key === item.key)?.hook_commands;
  };

  const removedSources = [
    ...new Set(
      [...globalInstalls, ...projects.flatMap((p) => p.marketplace_installs)]
        .map((i) => i.marketplace_id)
        .filter((id) => !known.has(id)),
    ),
  ];

  const applyNow = async () => {
    setApplying(true);
    try {
      const results = await applyMarketplaceNow(undefined);
      // F4 (preflight): the backend emits `marketplace-sync-finished` for
      // every project synced here, and `useMarketplaceSyncToasts` already
      // toasts any errors/skips from that event. This toast is only the
      // success/info summary — a second error toast here would double up.
      if (results.length === 0) {
        pushToast({ kind: "info", message: "No running projects — changes apply when a project starts." });
      } else {
        pushToast({
          kind: "success",
          message: `Marketplace applied to ${results.length} running project${results.length === 1 ? "" : "s"}. New Claude sessions will use it.`,
        });
      }
    } catch (e) {
      pushToast({ kind: "error", message: "Could not apply marketplace changes", detail: errorText(e) });
    } finally {
      setApplying(false);
    }
  };

  const row = (i: MarketplaceInstall, scope: InstallScope, scopeLabel: string, removeLabel: string) => {
    const upd = updateFor(i);
    const gone = !known.has(i.marketplace_id);
    // F7 (preflight): pass the bare item ref, not the MarketplaceInstall
    // itself — `commit` is not part of the ref the backend/store expect here.
    const ref: MarketplaceItemRef = { marketplace_id: i.marketplace_id, kind: i.kind, key: i.key };
    return (
      <li key={`${i.marketplace_id}/${i.kind}/${i.key}`} className="flex items-center justify-between gap-2 text-xs py-1">
        <div className="min-w-0">
          <span className="font-medium">{i.key}</span>
          <span className="ml-1 text-[var(--text-secondary)]">
            {KIND_LABELS[i.kind].replace(/s$/, "").toLowerCase()} · {nameOf(i.marketplace_id)} · {i.commit.slice(0, 8)}
          </span>
          {gone && <span className="ml-2 text-[var(--warning)]">Source removed</span>}
          {upd?.invalid_at_head && !gone && (
            // The update would be refused (re-review round 2), so it is
            // explained rather than offered.
            <span className="block text-[var(--warning)]">
              Update to {upd.head.slice(0, 8)} cannot be installed: {upd.invalid_at_head}
            </span>
          )}
        </div>
        <div className="flex gap-1 flex-shrink-0">
          {upd && !upd.invalid_at_head && !gone && (
            <Button
              size="sm"
              variant="secondary"
              aria-label={`Review update for ${i.key}`}
              onClick={() =>
                setPending({ install: i, update: upd, scope, scopeLabel, hookCommands: hookCommandsFor(upd) })
              }
            >
              Update available
            </Button>
          )}
          <Button size="sm" variant="ghost" aria-label={removeLabel} onClick={() => void mp.uninstall(ref, scope)}>
            Remove
          </Button>
        </div>
      </li>
    );
  };

  return (
    <div className="p-4 space-y-4 max-w-4xl">
      <div className="flex items-center justify-between gap-2">
        <p className="text-xs text-[var(--text-secondary)]">
          Installs are pinned to a commit. Containers pick up changes on their next start, or now for running ones.
          Changes apply to new Claude sessions.
        </p>
        <Button size="md" variant="primary" disabled={applying} onClick={() => void applyNow()}>
          {applying ? "Applying…" : "Apply now"}
        </Button>
      </div>

      {removedSources.length > 0 && (
        <div className="rounded-[var(--radius-control)] border border-[var(--warning)]/40 bg-[var(--warning-muted)] p-2 text-xs space-y-1">
          <p>
            Some installs come from marketplaces that were removed. They are removed from containers at their next
            sync.
          </p>
          <Button
            size="sm"
            variant="secondary"
            aria-label="Forget installs from removed marketplaces"
            onClick={() => removedSources.forEach((id) => void mp.forget(id))}
          >
            Forget
          </Button>
        </div>
      )}

      <section data-testid="installed-global">
        <h3 className="text-xs font-medium mb-1">All projects</h3>
        {globalInstalls.length === 0 ? (
          <p className="text-xs text-[var(--text-secondary)]">Nothing installed for all projects.</p>
        ) : (
          <ul>{globalInstalls.map((i) => row(i, { type: "global" }, "All projects", `Remove ${i.key} from all projects`))}</ul>
        )}
      </section>

      {projects.map((p) => (
        <section key={p.id} data-testid={`installed-project-${p.id}`}>
          <h3 className="text-xs font-medium mb-1">{p.name}</h3>
          {p.marketplace_installs.length === 0 ? (
            <p className="text-xs text-[var(--text-secondary)]">
              No project-only installs
              {p.marketplace_disabled.length > 0 ? ` · opted out of ${p.marketplace_disabled.length} global item(s)` : ""}.
            </p>
          ) : (
            <ul>
              {p.marketplace_installs.map((i) =>
                row(i, { type: "project", project_id: p.id }, p.name, `Remove ${i.key} from ${p.name}`),
              )}
            </ul>
          )}
        </section>
      ))}

      {pending && (
        <UpdateDiffModal
          item={pending.update.item}
          fromCommit={pending.install.commit}
          toCommit={pending.update.head}
          scopeLabel={pending.scopeLabel}
          hookCommands={pending.hookCommands}
          onClose={() => setPending(null)}
          // Pin exactly the head whose diff is on screen (final review I2).
          onAccept={() => mp.update(pending.update.item, pending.scope, pending.update.head)}
        />
      )}
    </div>
  );
}
