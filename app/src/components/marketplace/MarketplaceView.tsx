import { useEffect, useRef, useState } from "react";
import { useMarketplace } from "../../hooks/useMarketplace";
import { applicableUpdates } from "../../lib/marketplace";
import BrowsePane from "./BrowsePane";
import InstalledPane from "./InstalledPane";
import AccountsPane from "./AccountsPane";

const SUB_TABS = [
  { id: "browse", label: "Browse" },
  { id: "installed", label: "Installed" },
  { id: "accounts", label: "Accounts" },
] as const;

export type MarketplaceSubTab = (typeof SUB_TABS)[number]["id"];

interface Props {
  active: boolean;
}

export default function MarketplaceView({ active }: Props) {
  const mp = useMarketplace();
  const [tab, setTab] = useState<MarketplaceSubTab>("browse");
  const { load } = mp;
  const wasActive = useRef(false);

  // Load (and refresh stale marketplaces) each time the tab comes to the front.
  useEffect(() => {
    if (active && !wasActive.current) void load({ refreshStale: true });
    wasActive.current = active;
  }, [active, load]);

  return (
    <div className={`w-full h-full flex flex-col min-h-0 ${active ? "" : "hidden"}`}>
      <div
        role="tablist"
        aria-label="Marketplace sections"
        className="flex gap-1 px-3 pt-3 border-b border-[var(--border-color)]"
      >
        {SUB_TABS.map((t) => (
          <button
            key={t.id}
            type="button"
            role="tab"
            aria-selected={tab === t.id}
            onClick={() => setTab(t.id)}
            className={`px-3 py-1.5 text-xs rounded-t-[var(--radius-control)] ${
              tab === t.id
                ? "bg-[var(--bg-primary)] text-[var(--text-primary)]"
                : "text-[var(--text-secondary)] hover:text-[var(--text-primary)]"
            }`}
          >
            {t.label}
            {t.id === "installed" && applicableUpdates(mp.updates).length > 0 && (
              <span className="ml-1.5 px-1 rounded-[4px] text-[10px] bg-[var(--accent-muted)] text-[var(--accent)]">
                {applicableUpdates(mp.updates).length}
              </span>
            )}
          </button>
        ))}
      </div>
      <div className="flex-1 min-h-0 overflow-auto">
        {tab === "browse" && <BrowsePane mp={mp} />}
        {tab === "installed" && <InstalledPane mp={mp} />}
        {tab === "accounts" && <AccountsPane mp={mp} />}
      </div>
    </div>
  );
}
