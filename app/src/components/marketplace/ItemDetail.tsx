import type { CatalogItem } from "../../lib/types";
import type { MarketplaceApi } from "../../hooks/useMarketplace";
import { KIND_LABELS } from "../../lib/marketplace";
import StatusIndicator from "../ui/StatusIndicator";
import InstallControls from "./InstallControls";

interface Props {
  mp: MarketplaceApi;
  item: CatalogItem;
  marketplaceId: string;
  /** The marketplace head `item` was read at. */
  headCommit: string | null;
}

export default function ItemDetail({ mp, item, marketplaceId, headCommit }: Props) {
  return (
    <div className="space-y-3">
      <div>
        <p className="text-[10px] uppercase tracking-wide text-[var(--text-secondary)]">
          {KIND_LABELS[item.kind].replace(/s$/, "")} · <code className="font-mono">{item.path}</code>
        </p>
        <h3 className="text-sm font-medium text-[var(--text-primary)]">{item.name}</h3>
        {item.description && <p className="text-xs text-[var(--text-secondary)] leading-snug">{item.description}</p>}
      </div>
      {item.invalid && (
        <div className="rounded-[var(--radius-control)] border border-[var(--error)]/40 bg-[var(--error-muted)] p-2">
          <StatusIndicator tone="error" label="Cannot be installed" className="text-xs" />
          <p className="mt-1 text-xs text-[var(--text-secondary)]">{item.invalid}</p>
        </div>
      )}
      {item.kind === "hook" && item.hook_commands.length > 0 && (
        <div>
          <p className="text-xs font-medium mb-1">Commands this hook runs</p>
          <ul className="space-y-1">
            {item.hook_commands.map((c) => (
              <li key={c}>
                <code className="block font-mono text-xs break-all">{c}</code>
              </li>
            ))}
          </ul>
        </div>
      )}
      {item.preview && (
        <pre className="max-h-80 overflow-auto p-2 text-xs font-mono whitespace-pre-wrap rounded-[var(--radius-control)] bg-[var(--bg-primary)] border border-[var(--border-color)]">
          {item.preview}
        </pre>
      )}
      <div>
        <p className="text-xs font-medium mb-1">Install</p>
        <InstallControls mp={mp} item={item} marketplaceId={marketplaceId} headCommit={headCommit} />
        <p className="mt-2 text-[11px] text-[var(--text-secondary)]">
          Running containers pick changes up on their next start or with “Apply now” on the Installed tab. Changes
          apply to new Claude sessions.
        </p>
      </div>
    </div>
  );
}
