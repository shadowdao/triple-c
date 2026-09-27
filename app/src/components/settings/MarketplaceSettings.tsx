import { useEffect, useState } from "react";
import { useAppState } from "../../store/appState";
import { listMarketplaceUpdates } from "../../lib/tauri-commands";
import { applicableUpdates } from "../../lib/marketplace";
import Button from "../ui/Button";

const plural = (n: number, one: string, many: string) => `${n} ${n === 1 ? one : many}`;

export default function MarketplaceSettings() {
  const appSettings = useAppState((s) => s.appSettings);
  const openMarketplace = useAppState((s) => s.openMarketplace);
  const [updateCount, setUpdateCount] = useState<number | null>(null);

  useEffect(() => {
    let cancelled = false;
    listMarketplaceUpdates()
      .then((u) => {
        if (!cancelled) setUpdateCount(applicableUpdates(u).length);
      })
      .catch(() => {
        if (!cancelled) setUpdateCount(null);
      });
    return () => {
      cancelled = true;
    };
  }, [appSettings?.marketplaces.length]);

  const marketplaces = appSettings?.marketplaces.length ?? 0;
  const globalInstalls = appSettings?.global_marketplace_installs.length ?? 0;

  return (
    <div className="space-y-2">
      <p data-testid="marketplace-summary" className="text-xs text-[var(--text-secondary)] leading-snug">
        {plural(marketplaces, "marketplace", "marketplaces")} ·{" "}
        {globalInstalls} installed for all projects
        {updateCount !== null && updateCount > 0 && (
          <> · {plural(updateCount, "update available", "updates available")}</>
        )}
      </p>
      <p className="text-xs text-[var(--text-secondary)] leading-snug">
        Agents, skills, commands, hooks and plugins from git repositories, installed for all
        projects or per project. Changes apply to new Claude sessions.
      </p>
      <Button size="md" variant="secondary" onClick={() => openMarketplace()}>
        Open Marketplace
      </Button>
    </div>
  );
}
