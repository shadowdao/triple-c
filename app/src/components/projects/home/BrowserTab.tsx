import { useCallback, useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import type {
  BrowserViewChangedEvent,
  BrowserViewStatus,
  Project,
} from "../../../lib/types";
import {
  getBrowserViewStatus,
  setBrowserViewEnabled,
} from "../../../lib/tauri-commands";
import { useAppState } from "../../../store/appState";
import Button from "../../ui/Button";
import StatusIndicator from "../../ui/StatusIndicator";

interface Props {
  project: Project;
  active: boolean;
}

const OFF: BrowserViewStatus = {
  enabled: false,
  state: "off",
  url: null,
  host_port: null,
  container_port: null,
  started_at: null,
  detection: null,
  message: null,
};

/**
 * Watch — and take over — the browser Claude is driving with Playwright inside
 * the container.
 *
 * The pane is an iframe onto Playwright's own live dashboard, which runs in the
 * container and is reached through a token-gated listener on the host's
 * loopback. Nothing starts until the user asks: this is remote control of a
 * browser in a privileged sandbox, so it is off by default and opted into per
 * project, exactly like the auth bridge.
 */
export default function BrowserTab({ project, active }: Props) {
  const [status, setStatus] = useState<BrowserViewStatus>(OFF);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  /** Bumped to force the iframe to reload without changing its src. */
  const [reloadKey, setReloadKey] = useState(0);
  const pushToast = useAppState((s) => s.pushToast);
  const running = project.status === "running";

  // The backend is the source of truth: it emits whenever a view starts or is
  // torn down (container stopped, project removed, viewer died).
  const projectId = project.id;
  const mounted = useRef(true);
  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
    };
  }, []);

  useEffect(() => {
    let dispose: (() => void) | undefined;
    listen<BrowserViewChangedEvent>("browser-view-changed", (event) => {
      if (event.payload.project_id === projectId && mounted.current) {
        setStatus(event.payload.status);
      }
    }).then((un) => {
      if (mounted.current) dispose = un;
      else un();
    });
    return () => dispose?.();
  }, [projectId]);

  useEffect(() => {
    if (!active || !running) return;
    getBrowserViewStatus(projectId)
      .then((s) => mounted.current && setStatus(s))
      .catch(() => {});
  }, [active, projectId, running]);

  const toggle = useCallback(
    async (next: boolean) => {
      setBusy(true);
      setError(null);
      try {
        const result = await setBrowserViewEnabled(projectId, next);
        if (mounted.current) setStatus(result);
      } catch (e) {
        const detail = String(e);
        if (mounted.current) setError(detail);
        pushToast({
          kind: "error",
          message: next ? "Could not start the browser view" : "Could not stop the browser view",
          detail,
        });
      } finally {
        if (mounted.current) setBusy(false);
      }
    },
    [projectId, pushToast],
  );

  // A stopped container can't be hosting a browser, so say that plainly rather
  // than offering a control that would only fail.
  if (!running) {
    return (
      <Explainer title="The container isn’t running.">
        Start the container, have Claude drive a browser with Playwright, then come
        back here to watch it.
      </Explainer>
    );
  }

  const live = status.state === "running" && status.url;

  return (
    <div className="flex flex-col h-full min-h-0">
      <div className="flex items-center gap-2 px-4 py-2 border-b border-[var(--border-color)] flex-shrink-0 flex-wrap">
        <StatusIndicator
          tone={
            busy
              ? "busy"
              : status.state === "running"
                ? "running"
                : status.state === "unavailable"
                  ? "error"
                  : "off"
          }
          label={
            busy
              ? "Starting"
              : status.state === "running"
                ? "Live"
                : status.state === "unavailable"
                  ? "Unavailable"
                  : "Off"
          }
        />
        {live && (
          <span className="text-xs text-[var(--text-secondary)] font-mono truncate">
            127.0.0.1:{status.host_port} → container :{status.container_port}
          </span>
        )}
        <div className="flex-1" />
        {live && (
          <Button size="md" onClick={() => setReloadKey((k) => k + 1)}>
            Reload
          </Button>
        )}
        <Button
          size="md"
          variant={live ? "secondary" : "primary"}
          disabled={busy}
          onClick={() => toggle(!status.enabled || status.state !== "running")}
        >
          {busy ? "Working…" : live ? "Stop" : "Start browser view"}
        </Button>
      </div>

      {live ? (
        <iframe
          key={reloadKey}
          // Loopback only, and the URL carries the one-time session token the
          // host-side gate checks before anything reaches the container.
          src={status.url ?? undefined}
          title={`Playwright browser view for ${project.name}`}
          className="flex-1 min-h-0 w-full border-0 bg-[var(--bg-primary)]"
        />
      ) : (
        <div className="flex-1 min-h-0 overflow-y-auto">
          {status.state === "unavailable" ? (
            <Unavailable status={status} />
          ) : error ? (
            <Explainer title="The browser view didn’t start." tone="error">
              <span className="font-mono text-xs break-words">{error}</span>
            </Explainer>
          ) : (
            <Explainer title="Nothing is being watched yet.">
              Start the view to run Playwright’s live dashboard inside this container
              and mirror it here. You’ll see any browser a script has published with{" "}
              <Code>await browser.bind(&apos;claude&apos;)</Code> — and{" "}
              <Code>@playwright/mcp</Code> publishes automatically, so nothing extra is
              needed if Claude is using that.
            </Explainer>
          )}
        </div>
      )}
    </div>
  );
}

/** The container can't serve a view — say exactly what is missing. */
function Unavailable({ status }: { status: BrowserViewStatus }) {
  const d = status.detection;
  return (
    <div className="p-4 max-w-[46rem] space-y-3">
      <h2 className="text-[13px] font-semibold text-[var(--text-primary)]">
        This container can’t serve a browser view yet
      </h2>
      <p className="text-[13px] text-[var(--text-secondary)] leading-relaxed">
        {status.message}
      </p>
      {d && (
        <dl className="text-xs grid grid-cols-[auto_1fr] gap-x-3 gap-y-1 pt-2 border-t border-[var(--border-color)]">
          <Detail label="Node.js" value={d.node_version} />
          <Detail label="Playwright" value={d.playwright_version} />
          <Detail label="browser.bind()" value={d.has_bind ? "available" : "not in this build"} />
          <Detail label="@playwright/cli" value={d.cli_version} />
          {d.searched.length > 0 && (
            <Detail label="Searched" value={d.searched.join(", ")} />
          )}
        </dl>
      )}
    </div>
  );
}

function Detail({ label, value }: { label: string; value: string | null }) {
  return (
    <>
      <dt className="text-[var(--text-secondary)]">{label}</dt>
      <dd className="font-mono text-[var(--text-primary)] break-all">
        {value ?? "not found"}
      </dd>
    </>
  );
}

function Explainer({
  title,
  tone = "normal",
  children,
}: {
  title: string;
  tone?: "normal" | "error";
  children: React.ReactNode;
}) {
  return (
    <div className="p-4 max-w-[46rem]">
      <h2
        className={`text-[13px] font-semibold ${
          tone === "error" ? "text-[var(--error)]" : "text-[var(--text-primary)]"
        }`}
      >
        {title}
      </h2>
      <p className="mt-1 text-[13px] text-[var(--text-secondary)] leading-relaxed">
        {children}
      </p>
    </div>
  );
}

function Code({ children }: { children: React.ReactNode }) {
  return (
    <code className="font-mono text-xs px-1 py-0.5 rounded-[var(--radius-control)] bg-[var(--bg-tertiary)] text-[var(--text-primary)]">
      {children}
    </code>
  );
}
