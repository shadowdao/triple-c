import { useEffect, useState } from "react";
import type { Project, WorkspaceStatus } from "../../../lib/types";
import { awsWorkspaceStatus } from "../../../lib/tauri-commands";

/** How often the card re-reads the status while it is on screen. */
export const STATUS_POLL_MS = 5000;

const REPO_STATE: Record<string, string> = {
  cloning: "cloning…",
  cloned: "cloned",
  failed: "failed",
  exists: "already there",
};

/**
 * An AWS workspace's background clone and setup script (phase 6, Q4/R5):
 * the start ends at "Connecting…" and this card is where their progress
 * shows, re-read every 5 s while the project is Running and the window is
 * visible. Each read is a background exec, so the
 * card never keeps an idle workspace up. Renders nothing for a local project,
 * a workspace that cannot be asked, or one with no repos and no setup script.
 */
export default function AwsWorkspaceStatusCard({ project }: { project: Project }) {
  const running = project.status === "running";
  const [status, setStatus] = useState<WorkspaceStatus | null>(null);

  useEffect(() => {
    setStatus(null);
    if (!running) return;
    let cancelled = false;
    let timer: ReturnType<typeof setTimeout> | null = null;
    const visible = () => document.visibilityState !== "hidden";
    const poll = async () => {
      timer = null;
      if (!visible()) return; // resumed by visibilitychange
      try {
        const s = await awsWorkspaceStatus(project.id);
        if (!cancelled) setStatus(s);
      } catch {
        // A failed read keeps the last status; the next poll tries again.
      }
      if (!cancelled) timer = setTimeout(poll, STATUS_POLL_MS);
    };
    const onVisibility = () => {
      if (visible() && timer === null && !cancelled) void poll();
    };
    void poll();
    document.addEventListener("visibilitychange", onVisibility);
    return () => {
      cancelled = true;
      if (timer !== null) clearTimeout(timer);
      document.removeEventListener("visibilitychange", onVisibility);
    };
  }, [project.id, running, project.container_id]);

  if (!running || !status) return null;
  const { clone, setup } = status;
  if (clone.done && clone.repos.length === 0 && setup.state === "none") return null;

  const failedRepos = clone.repos.filter((r) => r.state === "failed");
  const finished = clone.repos.filter((r) => r.state !== "cloning").length;
  const total = Math.max(project.aws?.repos.length ?? 0, clone.repos.length);
  const cloneLine = !clone.done
    ? total > 0
      ? `Still cloning repos (${finished} of ${total} done)`
      : "Still cloning repos"
    : failedRepos.length > 0
      ? `Clone failed for ${failedRepos.map((r) => r.folder).join(", ")}`
      : clone.repos.length > 0
        ? "Repos cloned"
        : null;
  const setupLine =
    setup.state === "running"
      ? clone.done
        ? "Setup script running…"
        : "Setup script waits for the clone"
      : setup.state === "ok"
        ? "Setup script finished"
        : setup.state === "failed"
          ? `Setup script failed${setup.code != null ? ` (exit ${setup.code})` : ""}`
          : null;
  const problem = failedRepos.length > 0 || setup.state === "failed";

  return (
    <section
      aria-label="Workspace start-up"
      className="p-3 border rounded-[var(--radius-panel)] bg-[var(--bg-secondary)] text-xs space-y-2"
      style={{ borderColor: problem ? "var(--warning)" : "var(--border-color)" }}
    >
      <h2 className="text-[11px] font-semibold uppercase tracking-wide text-[var(--text-secondary)]">
        Workspace start-up
      </h2>
      {cloneLine && (
        <p className="text-[var(--text-primary)]">
          {failedRepos.length > 0 && (
            <span aria-hidden="true" className="mr-1 text-[var(--warning)]">
              ▲
            </span>
          )}
          {cloneLine}
        </p>
      )}
      {clone.repos.length > 0 && (!clone.done || failedRepos.length > 0) && (
        <ul className="space-y-0.5 text-[var(--text-secondary)]">
          {clone.repos.map((r) => (
            <li key={r.folder}>
              <span className="font-mono text-[var(--text-primary)]">{r.folder}</span>{" "}
              {REPO_STATE[r.state] ?? r.state}
            </li>
          ))}
        </ul>
      )}
      {setupLine && (
        <p className="text-[var(--text-primary)]">
          {setup.state === "failed" && (
            <span aria-hidden="true" className="mr-1 text-[var(--warning)]">
              ▲
            </span>
          )}
          {setupLine}
        </p>
      )}
      {(setup.state === "running" || setup.state === "failed") && setup.tail.length > 0 && (
        <pre className="max-h-40 overflow-auto whitespace-pre-wrap break-all p-2 rounded-[var(--radius-control)] bg-[var(--bg-primary)] text-[var(--text-secondary)] font-mono text-[11px]">
          {(setup.state === "running" ? setup.tail.slice(-3) : setup.tail).join("\n")}
        </pre>
      )}
    </section>
  );
}
