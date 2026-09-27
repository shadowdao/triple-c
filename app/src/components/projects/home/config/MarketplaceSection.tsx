import { useEffect, useState } from "react";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { ConfigGroup } from "../../../ui/Field";
import Toggle from "../../../ui/Toggle";
import Button from "../../../ui/Button";
import { useAppState } from "../../../../store/appState";
import { KIND_LABELS } from "../../../../lib/marketplace";
import { getMarketplaceSyncReport, setGlobalItemDisabled } from "../../../../lib/tauri-commands";
import type { MarketplaceItemRef, Project, SyncReport } from "../../../../lib/types";

interface Props {
  project: Project;
}

interface SyncFinishedEvent {
  project_id: string;
  report: SyncReport;
}

const kindWord = (k: MarketplaceItemRef["kind"]) => KIND_LABELS[k].replace(/s$/, "").toLowerCase();
const same = (a: MarketplaceItemRef, b: MarketplaceItemRef) =>
  a.marketplace_id === b.marketplace_id && a.kind === b.kind && a.key === b.key;

export default function MarketplaceSection({ project }: Props) {
  const appSettings = useAppState((s) => s.appSettings);
  const openMarketplace = useAppState((s) => s.openMarketplace);
  const updateProjectInList = useAppState((s) => s.updateProjectInList);
  const pushToast = useAppState((s) => s.pushToast);
  const [report, setReport] = useState<SyncReport | null>(null);
  const [busy, setBusy] = useState<string | null>(null);

  const globalInstalls = appSettings?.global_marketplace_installs ?? [];
  const nameOf = (id: string) => appSettings?.marketplaces.find((m) => m.id === id)?.name ?? "removed marketplace";

  useEffect(() => {
    let cancelled = false;

    const fetchReport = () => {
      getMarketplaceSyncReport(project.id)
        .then((r) => {
          if (!cancelled) setReport(r);
        })
        .catch(() => {
          if (!cancelled) setReport(null);
        });
    };

    fetchReport();

    // N7 (preflight): a sync also runs outside this component's own actions
    // (container start, "Apply now" from the Marketplace tab), so without
    // this the report shown here goes stale as soon as one finishes.
    let unlisten: UnlistenFn | null = null;
    listen<SyncFinishedEvent>("marketplace-sync-finished", (event) => {
      if (event.payload.project_id === project.id) fetchReport();
    })
      .then((fn) => {
        if (cancelled) fn();
        else unlisten = fn;
      })
      .catch(() => {
        // Not running inside Tauri (e.g. tests) — nothing to listen to.
      });

    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, [project.id, project.status]);

  const toggleGlobal = async (ref: MarketplaceItemRef, enabled: boolean) => {
    const id = `${ref.kind}-${ref.key}`;
    setBusy(id);
    try {
      updateProjectInList(await setGlobalItemDisabled(project.id, ref, !enabled));
    } catch (e) {
      pushToast({ kind: "error", message: `Could not change ${ref.key} for “${project.name}”`, detail: String(e) });
    } finally {
      setBusy(null);
    }
  };

  return (
    <ConfigGroup
      title="Marketplace"
      description="Items this project gets from marketplaces. Changes apply on the next container start or with Apply now, in new Claude sessions."
    >
      <div className="space-y-3">
        {globalInstalls.length > 0 && (
          <div>
            <p className="text-xs font-medium mb-1">From “All projects”</p>
            <ul className="space-y-1">
              {globalInstalls.map((g) => {
                const shadowed = project.marketplace_installs.some((p) => same(p, g));
                const enabled = !project.marketplace_disabled.some((d) => same(d, g));
                return (
                  <li
                    key={`${g.marketplace_id}/${g.kind}/${g.key}`}
                    data-testid={`mp-global-${g.kind}-${g.key}`}
                    className="flex items-center justify-between gap-2 text-xs"
                  >
                    <span className="min-w-0 truncate">
                      <span className="font-medium">{g.key}</span>{" "}
                      <span className="text-[var(--text-secondary)]">
                        {kindWord(g.kind)} · {nameOf(g.marketplace_id)}
                        {shadowed ? " · overridden by this project's own install" : ""}
                      </span>
                    </span>
                    <Toggle
                      label={`Use ${g.key} in ${project.name}`}
                      checked={enabled}
                      disabled={busy === `${g.kind}-${g.key}`}
                      onChange={(v) => void toggleGlobal({ marketplace_id: g.marketplace_id, kind: g.kind, key: g.key }, v)}
                    />
                  </li>
                );
              })}
            </ul>
          </div>
        )}
        {project.marketplace_installs.length > 0 && (
          <div>
            <p className="text-xs font-medium mb-1">This project only</p>
            <ul className="space-y-1">
              {project.marketplace_installs.map((i) => (
                <li
                  key={`${i.marketplace_id}/${i.kind}/${i.key}`}
                  data-testid={`mp-project-${i.kind}-${i.key}`}
                  className="text-xs"
                >
                  <span className="font-medium">{i.key}</span>{" "}
                  <span className="text-[var(--text-secondary)]">
                    {kindWord(i.kind)} · {nameOf(i.marketplace_id)} · This project only
                  </span>
                </li>
              ))}
            </ul>
          </div>
        )}
        {globalInstalls.length === 0 && project.marketplace_installs.length === 0 && (
          <p className="text-xs text-[var(--text-secondary)]">Nothing installed from a marketplace.</p>
        )}

        {report && (
          <div className="text-xs space-y-1">
            <p className="font-medium">
              Last sync {report.finished_at ? new Date(report.finished_at).toLocaleString() : ""}
            </p>
            <p className="text-[var(--text-secondary)]">
              {report.installed.length} installed · {report.updated.length} updated · {report.removed.length} removed
            </p>
            {report.skipped.map((s) => (
              <p key={s.item} className="text-[var(--warning)]">
                Skipped {s.item}: {s.reason}
              </p>
            ))}
            {report.errors.map((e) => (
              <p key={e} className="text-[var(--error)] whitespace-pre-wrap">
                {e}
              </p>
            ))}
          </div>
        )}

        <Button size="sm" variant="secondary" onClick={() => openMarketplace(project.id)}>
          Open in Marketplace
        </Button>
      </div>
    </ConfigGroup>
  );
}
