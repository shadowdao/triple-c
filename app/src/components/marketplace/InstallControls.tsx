import { useState } from "react";
import { useAppState } from "../../store/appState";
import { projectItemState, type ProjectItemState } from "../../lib/marketplace";
import type { MarketplaceApi } from "../../hooks/useMarketplace";
import type { CatalogItem, InstallScope, MarketplaceItemRef } from "../../lib/types";
import Toggle from "../ui/Toggle";
import HookConfirmModal from "./HookConfirmModal";
import PluginConfirmModal from "./PluginConfirmModal";

const STATE_LABEL: Record<ProjectItemState, string> = {
  none: "",
  inherited: "Inherited",
  opted_out: "Opted out",
  project: "This project",
  project_pinned_differently: "Pinned to a different commit",
};

interface Props {
  mp: MarketplaceApi;
  item: CatalogItem;
  marketplaceId: string;
  /**
   * The marketplace head `item` was read at. Installs pin exactly this commit;
   * the backend refuses if the marketplace has moved on since (final review I2).
   */
  headCommit: string | null;
}

/** A hook or plugin install waiting for confirmation, frozen at the moment it was asked for. */
interface PendingConfirm {
  scope: InstallScope;
  item: CatalogItem;
  commit: string;
}

export default function InstallControls({ mp, item, marketplaceId, headCommit }: Props) {
  const appSettings = useAppState((s) => s.appSettings);
  const projects = useAppState((s) => s.projects);
  const filterId = useAppState((s) => s.marketplaceFilterProjectId);
  const [pending, setPending] = useState<PendingConfirm | null>(null);
  const [busy, setBusy] = useState(false);

  const ref: MarketplaceItemRef = { marketplace_id: marketplaceId, kind: item.kind, key: item.key };
  const globalInstalls = appSettings?.global_marketplace_installs ?? [];
  const isGlobal = globalInstalls.some(
    (g) => g.marketplace_id === marketplaceId && g.kind === item.kind && g.key === item.key,
  );
  const disabled = item.invalid !== null || busy;
  // "" never matches a head, so the backend explains that a refresh is needed.
  const commit = headCommit ?? "";
  const shown = filterId ? projects.filter((p) => p.id === filterId) : projects;

  const run = async (fn: () => Promise<boolean>) => {
    setBusy(true);
    try {
      await fn();
    } finally {
      setBusy(false);
    }
  };

  /** Every install goes through here so a hook or plugin is always confirmed first. */
  const install = (scope: InstallScope) => {
    if (item.kind === "hook" || item.kind === "plugin") {
      setPending({ scope, item, commit });
      return;
    }
    void run(() => mp.install(ref, scope, commit));
  };

  const toggleProject = (projectId: string, state: ProjectItemState) => {
    const scope: InstallScope = { type: "project", project_id: projectId };
    switch (state) {
      case "none":
        install(scope);
        break;
      case "inherited":
        void run(() => mp.setDisabled(projectId, ref, true));
        break;
      case "opted_out":
        void run(() => mp.setDisabled(projectId, ref, false));
        break;
      case "project":
      case "project_pinned_differently":
        void run(() => mp.uninstall(ref, scope));
        break;
    }
  };

  return (
    <div className="space-y-2">
      <Toggle
        label="All projects"
        checked={isGlobal}
        disabled={disabled}
        onChange={(v) => (v ? install({ type: "global" }) : void run(() => mp.uninstall(ref, { type: "global" })))}
      />
      <ul className="space-y-1">
        {shown.map((p) => {
          const state = projectItemState(ref, globalInstalls, p);
          const checked = state === "inherited" || state === "project" || state === "project_pinned_differently";
          return (
            <li
              key={p.id}
              data-testid={`install-row-${p.id}`}
              className="flex items-center justify-between gap-2 text-xs"
            >
              <label className="flex items-center gap-2 min-w-0">
                <input
                  type="checkbox"
                  checked={checked}
                  disabled={disabled}
                  onChange={() => toggleProject(p.id, state)}
                />
                <span className="truncate">{p.name}</span>
              </label>
              {STATE_LABEL[state] && (
                <span className="text-[var(--text-secondary)] whitespace-nowrap">{STATE_LABEL[state]}</span>
              )}
            </li>
          );
        })}
      </ul>
      {projects.length === 0 && (
        <p className="text-xs text-[var(--text-secondary)]">No projects yet — “All projects” also covers projects added later.</p>
      )}
      {pending &&
        (() => {
          const confirm = () => {
            const { scope, commit: reviewed } = pending;
            setPending(null);
            void run(() => mp.install(ref, scope, reviewed));
          };
          const Confirm = pending.item.kind === "plugin" ? PluginConfirmModal : HookConfirmModal;
          return (
            <Confirm
              item={pending.item}
              commit={pending.commit}
              onCancel={() => setPending(null)}
              onConfirm={confirm}
            />
          );
        })()}
    </div>
  );
}
