import { useState } from "react";
import type { MarketplaceApi } from "../../hooks/useMarketplace";
import { useAppState } from "../../store/appState";
import { removeMarketplaceAccount, testMarketplaceAccount } from "../../lib/tauri-commands";
import type { AccountMethod, MarketplaceAccount } from "../../lib/types";
import Button from "../ui/Button";
import AddAccountModal from "./AddAccountModal";

const METHOD_LABEL: Record<AccountMethod, string> = {
  gh_host: "GitHub — gh on this computer",
  gh_container: "GitHub — signed in via container",
  token: "Access token",
};

export default function AccountsPane(_props: { mp: MarketplaceApi }) {
  const appSettings = useAppState((s) => s.appSettings);
  const setAppSettings = useAppState((s) => s.setAppSettings);
  const pushToast = useAppState((s) => s.pushToast);
  const [adding, setAdding] = useState(false);
  const [testing, setTesting] = useState<string | null>(null);
  const [removing, setRemoving] = useState<string | null>(null);

  const accounts = appSettings?.marketplace_accounts ?? [];
  const marketplaces = appSettings?.marketplaces ?? [];
  const usedBy = (id: string) => marketplaces.filter((m) => m.account_id === id).map((m) => m.name);

  const test = async (a: MarketplaceAccount) => {
    setTesting(a.id);
    try {
      const login = await testMarketplaceAccount(a.id);
      pushToast({ kind: "success", message: `${a.label} works — signed in as ${login}` });
    } catch (e) {
      pushToast({ kind: "error", message: `${a.label} could not sign in`, detail: String(e) });
    } finally {
      setTesting(null);
    }
  };

  const remove = async (a: MarketplaceAccount) => {
    setRemoving(a.id);
    try {
      setAppSettings(await removeMarketplaceAccount(a.id));
    } catch (e) {
      pushToast({ kind: "error", message: `Could not remove ${a.label}`, detail: String(e) });
    } finally {
      setRemoving(null);
    }
  };

  return (
    <div className="p-4 space-y-3 max-w-3xl">
      <div className="flex items-center justify-between">
        <p className="text-xs text-[var(--text-secondary)]">
          Accounts are used to fetch private marketplaces. Tokens are kept in your OS keychain and never enter
          containers.
        </p>
        <Button size="md" variant="secondary" onClick={() => setAdding(true)}>
          Add account
        </Button>
      </div>
      {accounts.length === 0 && <p className="text-xs text-[var(--text-secondary)]">No accounts yet. Public repositories need none.</p>}
      <ul className="space-y-2">
        {accounts.map((a) => {
          const users = usedBy(a.id);
          const inUse = users.length > 0;
          return (
            <li
              key={a.id}
              className="flex items-center justify-between gap-2 p-2 rounded-[var(--radius-control)] border border-[var(--border-color)]"
            >
              <div className="min-w-0 text-xs">
                <p className="font-medium">{a.label}</p>
                <p className="text-[var(--text-secondary)]">
                  {METHOD_LABEL[a.method]} · {a.host}
                  {a.username ? ` · ${a.username}` : ""}
                </p>
                {inUse && <p className="text-[var(--text-secondary)]">Used by {users.join(", ")}</p>}
              </div>
              <div className="flex gap-1 flex-shrink-0">
                <Button
                  size="sm"
                  variant="ghost"
                  aria-label={`Test ${a.label}`}
                  disabled={testing === a.id}
                  onClick={() => void test(a)}
                >
                  {testing === a.id ? "Testing…" : "Test"}
                </Button>
                <Button
                  size="sm"
                  variant="ghost"
                  aria-label={`Remove ${a.label}`}
                  disabled={removing === a.id}
                  unavailable={inUse}
                  unavailableReason={`Used by ${users.join(", ")} — change or remove that marketplace first`}
                  onClick={() => void remove(a)}
                >
                  Remove
                </Button>
              </div>
            </li>
          );
        })}
      </ul>
      {adding && <AddAccountModal onClose={() => setAdding(false)} />}
    </div>
  );
}
