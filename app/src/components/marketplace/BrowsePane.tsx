import { useMemo, useState } from "react";
import type { MarketplaceApi } from "../../hooks/useMarketplace";
import { useAppState } from "../../store/appState";
import { KIND_LABELS, KIND_ORDER, itemRefKey } from "../../lib/marketplace";
import { updateMarketplace } from "../../lib/tauri-commands";
import type { CatalogItem, ItemKind, Marketplace } from "../../lib/types";
import Button from "../ui/Button";
import Modal from "../ui/Modal";
import SegmentedControl from "../ui/SegmentedControl";
import { inputClass, selectClass } from "../ui/Field";
import AddMarketplaceModal from "./AddMarketplaceModal";
import ItemDetail from "./ItemDetail";

type KindFilter = ItemKind | "all";

const when = (iso: string | null) => (iso ? new Date(iso).toLocaleString() : "never");

function errorText(e: unknown): string {
  return typeof e === "string" ? e : e instanceof Error ? e.message : String(e);
}

export default function BrowsePane({ mp }: { mp: MarketplaceApi }) {
  const marketplaces = useAppState((s) => s.appSettings?.marketplaces ?? []);
  const accounts = useAppState((s) => s.appSettings?.marketplace_accounts ?? []);
  const globalInstalls = useAppState((s) => s.appSettings?.global_marketplace_installs ?? []);
  const projects = useAppState((s) => s.projects);
  const filterId = useAppState((s) => s.marketplaceFilterProjectId);
  const setFilterId = useAppState((s) => s.setMarketplaceFilterProjectId);
  const pushToast = useAppState((s) => s.pushToast);
  const [kind, setKind] = useState<KindFilter>("all");
  const [query, setQuery] = useState("");
  // The item is kept as it was read, with the head it was read at: an
  // install pins exactly what the detail pane shows (final review I2).
  const [selected, setSelected] = useState<{
    marketplaceId: string;
    item: CatalogItem;
    headCommit: string | null;
  } | null>(null);
  const [adding, setAdding] = useState(false);
  const [removing, setRemoving] = useState<Marketplace | null>(null);

  const rows = useMemo(() => {
    const q = query.trim().toLowerCase();
    return mp.snapshots.flatMap((snap) =>
      snap.items
        .filter((i) => kind === "all" || i.kind === kind)
        .filter((i) => q === "" || `${i.name} ${i.key} ${i.description}`.toLowerCase().includes(q))
        .sort((a, b) => KIND_ORDER.indexOf(a.kind) - KIND_ORDER.indexOf(b.kind) || a.name.localeCompare(b.name))
        .map((item) => ({ marketplaceId: snap.marketplace_id, item, headCommit: snap.head_commit })),
    );
  }, [mp.snapshots, kind, query]);

  const nameOf = (id: string) => marketplaces.find((m) => m.id === id)?.name ?? id;

  /** A refused change (e.g. an account for another host) is toasted; a saved
   *  one is fetched with the new account so its old fetch error goes away. */
  const changeAccount = async (m: Marketplace, accountId: string | null) => {
    try {
      await updateMarketplace({ ...m, account_id: accountId });
    } catch (e) {
      pushToast({ kind: "error", message: `Could not change the account for ${m.name}`, detail: errorText(e) });
      return;
    }
    try {
      await mp.reloadState();
    } catch (e) {
      console.error("Failed to reload after changing a marketplace account:", e);
    }
    await mp.refresh(m.id);
  };

  /** Global + every project's installs of this marketplace, for the removal warning. */
  const installCountFor = (marketplaceId: string) => {
    const global = globalInstalls.filter((i) => i.marketplace_id === marketplaceId).length;
    const perProject = projects.reduce(
      (sum, p) => sum + p.marketplace_installs.filter((i) => i.marketplace_id === marketplaceId).length,
      0,
    );
    return global + perProject;
  };

  return (
    <div className="flex h-full min-h-0">
      <aside className="w-64 flex-shrink-0 border-r border-[var(--border-color)] p-3 space-y-3 overflow-auto">
        <div className="flex items-center justify-between">
          <h2 className="text-xs font-medium">Marketplaces</h2>
          <Button size="sm" variant="secondary" onClick={() => setAdding(true)}>
            Add marketplace
          </Button>
        </div>
        {marketplaces.length === 0 && (
          <p className="text-xs text-[var(--text-secondary)]">No marketplaces yet. Add a git repository to browse its items.</p>
        )}
        {marketplaces.map((m) => {
          const snap = mp.snapshots.find((s) => s.marketplace_id === m.id);
          const refreshing = mp.refreshing.includes(m.id);
          return (
            <div key={m.id} className="space-y-1 text-xs">
              <div className="flex items-center justify-between gap-2">
                <span className="font-medium truncate" title={m.url}>
                  {m.name}
                </span>
                <div className="flex items-center gap-1 flex-shrink-0">
                  <Button
                    size="sm"
                    variant="ghost"
                    aria-label={`Refresh ${m.name}`}
                    disabled={refreshing}
                    onClick={() => void mp.refresh(m.id)}
                  >
                    {refreshing ? "…" : "↻"}
                  </Button>
                  <Button
                    size="sm"
                    variant="ghost"
                    aria-label={`Remove ${m.name}`}
                    onClick={() => setRemoving(m)}
                  >
                    Remove
                  </Button>
                </div>
              </div>
              {accounts.length > 0 ? (
                <label className="flex items-center gap-1 text-[var(--text-secondary)]">
                  <span>Account</span>
                  <select
                    aria-label={`Account for ${m.name}`}
                    value={m.account_id ?? ""}
                    onChange={(e) => void changeAccount(m, e.target.value === "" ? null : e.target.value)}
                    className={selectClass}
                  >
                    <option value="">None (public)</option>
                    {accounts.map((a) => (
                      <option key={a.id} value={a.id}>
                        {a.label}
                      </option>
                    ))}
                  </select>
                </label>
              ) : (
                <p className="text-[var(--text-secondary)]">No account (public repository)</p>
              )}
              <p className="text-[var(--text-secondary)]">Last fetched {when(snap?.fetched_at ?? null)}</p>
              {snap?.fetch_error && (
                <p className="text-[var(--error)] whitespace-pre-wrap leading-snug">{snap.fetch_error}</p>
              )}
            </div>
          );
        })}
        {projects.length > 0 && (
          <label className="block text-xs space-y-1">
            <span className="text-[var(--text-secondary)]">Show install state for</span>
            <select
              value={filterId ?? ""}
              onChange={(e) => setFilterId(e.target.value === "" ? null : e.target.value)}
              className={selectClass}
            >
              <option value="">All projects</option>
              {projects.map((p) => (
                <option key={p.id} value={p.id}>
                  {p.name}
                </option>
              ))}
            </select>
          </label>
        )}
      </aside>

      <section className="w-80 flex-shrink-0 border-r border-[var(--border-color)] p-3 space-y-2 overflow-auto">
        <SegmentedControl<KindFilter>
          label="Item kind"
          className="flex-wrap"
          value={kind}
          onChange={setKind}
          segments={[
            { value: "all", label: "All" },
            ...KIND_ORDER.map((k) => ({ value: k as KindFilter, label: KIND_LABELS[k] })),
          ]}
        />
        <input
          aria-label="Search items"
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          placeholder="Search"
          className={inputClass}
        />
        <ul className="space-y-1">
          {rows.map(({ marketplaceId, item, headCommit }) => {
            const key = itemRefKey({ marketplace_id: marketplaceId, kind: item.kind, key: item.key });
            const isSel =
              selected?.marketplaceId === marketplaceId &&
              selected.item.kind === item.kind &&
              selected.item.key === item.key;
            return (
              <li key={key}>
                <button
                  type="button"
                  onClick={() => setSelected({ marketplaceId, item, headCommit })}
                  className={`w-full text-left px-2 py-1.5 rounded-[var(--radius-control)] text-xs ${
                    isSel ? "bg-[var(--bg-tertiary)]" : "hover:bg-[var(--bg-tertiary)]"
                  }`}
                >
                  <span className="font-medium">{item.name}</span>
                  <span className="ml-1 text-[var(--text-secondary)]">{KIND_LABELS[item.kind].replace(/s$/, "").toLowerCase()}</span>
                  {item.invalid && <span className="ml-1 text-[var(--error)]">invalid</span>}
                  {mp.snapshots.length > 1 && (
                    <span className="block text-[var(--text-secondary)]">{nameOf(marketplaceId)}</span>
                  )}
                  {item.description && (
                    <span className="block text-[var(--text-secondary)] truncate">{item.description}</span>
                  )}
                </button>
              </li>
            );
          })}
          {rows.length === 0 && mp.snapshots.length > 0 && (
            <li className="text-xs text-[var(--text-secondary)]">No items match.</li>
          )}
        </ul>
      </section>

      <section className="flex-1 min-w-0 p-4 overflow-auto">
        {selected ? (
          <ItemDetail
            mp={mp}
            item={selected.item}
            marketplaceId={selected.marketplaceId}
            headCommit={selected.headCommit}
          />
        ) : (
          <p className="text-xs text-[var(--text-secondary)]">Select an item to see what it contains and install it.</p>
        )}
      </section>

      {adding && (
        <AddMarketplaceModal
          onClose={() => setAdding(false)}
          onAdded={() => {
            void mp.reloadState();
            void mp.load();
          }}
        />
      )}

      {removing && (
        <Modal
          title={`Remove marketplace “${removing.name}”?`}
          onClose={() => setRemoving(null)}
          footer={
            <>
              <Button size="md" variant="ghost" onClick={() => setRemoving(null)}>
                Cancel
              </Button>
              <Button
                size="md"
                variant="danger"
                onClick={() => {
                  const id = removing.id;
                  setRemoving(null);
                  void mp.remove(id);
                }}
              >
                Remove marketplace
              </Button>
            </>
          }
        >
          <p className="text-xs text-[var(--text-secondary)] leading-snug">
            {installCountFor(removing.id)} install{installCountFor(removing.id) === 1 ? "" : "s"} stay listed as
            “Source removed” and are removed from containers at their next sync. Use “Forget” on the Installed tab
            instead if you want to drop them immediately.
          </p>
        </Modal>
      )}
    </div>
  );
}
