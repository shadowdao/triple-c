import type { ReattachOutcome, TerminalSession, WorkspaceTerminal } from "./types";

/**
 * Persistent AWS terminals (`host/persistent.rs`): every tab on an AWS
 * workspace whose image has tmux runs in its own tmux server, so it outlives
 * the app and a dropped connection. These are the frontend's rules for
 * getting back to one.
 */

/** How long a tab keeps trying after its connection dropped. */
export const RECONNECT_ATTEMPTS = 40;
export const RECONNECT_DELAY_MS = 3000;

export type ReattachResult = "reattached" | "ended" | "gave_up" | "cancelled";

/**
 * Attach a tab to its workspace session again, retrying while the workspace
 * cannot be reached (the tunnel is being re-opened). Ends as soon as the
 * workspace says the session is gone (`ended`), or when the tab or project
 * no longer wants it (`cancelled`: closed, stopped).
 */
export async function reattachWithRetry(opts: {
  reattach: () => Promise<ReattachOutcome>;
  stillWanted: () => boolean;
  onRetrying?: (error: unknown) => void;
  sleep?: (ms: number) => Promise<void>;
  attempts?: number;
  delayMs?: number;
}): Promise<ReattachResult> {
  const sleep = opts.sleep ?? ((ms: number) => new Promise<void>((r) => setTimeout(r, ms)));
  const attempts = opts.attempts ?? RECONNECT_ATTEMPTS;
  const delay = opts.delayMs ?? RECONNECT_DELAY_MS;
  for (let i = 0; i < attempts; i++) {
    if (!opts.stillWanted()) return "cancelled";
    try {
      const out = await opts.reattach();
      return out.outcome === "reattached" ? "reattached" : "ended";
    } catch (e) {
      opts.onRetrying?.(e);
    }
    if (i + 1 < attempts) await sleep(delay);
  }
  return opts.stillWanted() ? "gave_up" : "cancelled";
}

/** The tab label a workspace terminal gets when nothing else names it. */
export function terminalLabel(t: Pick<WorkspaceTerminal, "kind" | "name">): string {
  return t.name ?? (t.kind === "claude" ? "Claude Code" : "Shell");
}

/**
 * The tab for a workspace terminal: it attaches (and replays its history)
 * once its view is listening.
 */
export function reattachTab(
  projectId: string,
  projectName: string,
  t: WorkspaceTerminal,
): TerminalSession {
  return {
    id: t.session_id,
    projectId,
    projectName,
    sessionType: t.kind,
    sessionName: t.name,
    persistent: true,
    pendingReattach: true,
  };
}

/**
 * Which of a workspace's terminals to reopen as tabs after a connect: the
 * ones this app opened (`known`) that are not already open. Sessions from
 * elsewhere (another machine, the web terminal) are listed in Project Home →
 * Sessions instead, never opened unasked.
 */
export function terminalsToReopen(
  running: WorkspaceTerminal[],
  openIds: ReadonlySet<string>,
): WorkspaceTerminal[] {
  return running.filter((t) => t.known && !openIds.has(t.session_id));
}
